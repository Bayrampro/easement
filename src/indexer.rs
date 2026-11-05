//! Runs the SCIP indexers of the project's languages and reads what they write.
//!
//! easement does not resolve imports itself: an indexer that knows the language's
//! build system (`go.mod`, `tsconfig.json`) does. A missing indexer, a failed run
//! or missing dependencies is an error, never a quiet gap in the check.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::lang::registry::{self, Language};
use crate::relpath;
use crate::scip::{self, Document, Documents};

mod cache;
pub(crate) mod dart;

/// The SCIP indexer of a language: how to run it and what its index depends on.
pub struct Indexer {
    /// The language whose files it reads.
    pub language: &'static Language,
    binary: &'static str,
    /// Files that mark a directory the indexer runs in.
    markers: &'static [&'static str],
    /// Run only in the outermost marked directories: a Cargo workspace holds its members.
    outermost: bool,
    root_policy: RootPolicy,
    reader: fn() -> Box<dyn Reader>,
    strict_version: bool,
    /// Run at the project root when no marker is found (Python projects often have none).
    fallback_root: bool,
    /// Arguments that write the index to the given file.
    args: fn(&Path) -> Vec<String>,
    install: &'static str,
    /// The mise tool that installs the indexer, for the `[tools]` of a `mise.toml`.
    mise: Option<&'static str>,
    /// Directories whose files belong to dependencies, not to the project.
    skip_dirs: &'static [&'static str],
    /// For an indexer that has no output option: the file it writes in its working directory.
    writes_in_cwd: Option<&'static str>,
    /// What has to be in place before the indexer runs: (project root, directory, shown name).
    needs: fn(&Path, &Path, &str) -> Result<()>,
    /// Enumerate files, trees and named values that determine extraction.
    /// An error bypasses caching while allowing an ordinary run.
    inputs: fn(&InputContext<'_>) -> Result<Vec<Input>>,
    /// Project-relative files used by the default collector. Each language keeps
    /// its existing source/build filters; ancestor files are included as well.
    project_files: fn(&str) -> bool,
    /// Which inherited environment variables change what the indexer writes
    /// (`GOFLAGS`, `PYTHONPATH`); their values are in the cache key.
    env: fn(&str) -> bool,
}

#[derive(Clone, Copy)]
enum RootPolicy {
    Markers,
    SourceOwners,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Input {
    File(PathBuf),
    Tree(PathBuf),
    Value(String, Vec<u8>),
}

struct InputContext<'a> {
    language: &'a Indexer,
    binary: &'a Path,
    root: &'a Path,
    dir: &'a str,
    files: &'a [String],
}

struct RunContext<'a> {
    root: &'a Path,
    files: &'a [String],
    excluded: &'a [String],
}

struct ReadFacts {
    documents: Vec<Document>,
    gaps: Vec<crate::scan::AnalysisGap>,
}

trait Reader: Send {
    fn read(&mut self, bytes: &[u8], root: &Path, base: &Path) -> Result<ReadFacts>;

    fn validate(&mut self, _root: &Path, _cwd: &Path) -> Result<Vec<crate::scan::AnalysisGap>> {
        Ok(Vec::new())
    }

    fn finish(
        self: Box<Self>,
        language: &Indexer,
        documents: Documents,
        gaps: Vec<crate::scan::AnalysisGap>,
        context: &RunContext<'_>,
    ) -> Result<Indexed> {
        let covered: BTreeSet<&str> = documents.0.iter().map(|d| d.path.as_str()).collect();
        let uncovered = context
            .files
            .iter()
            .filter(|file| language.covers(file) && !covered.contains(file.as_str()))
            .cloned()
            .collect();
        Ok(Indexed {
            coverage_checks: Vec::new(),
            domains: Vec::new(),
            documents,
            analysis_partial: gaps,
            uncovered,
            ran: 0,
            cached: 0,
        })
    }
}

struct ScipReader;

impl Reader for ScipReader {
    fn read(&mut self, bytes: &[u8], root: &Path, base: &Path) -> Result<ReadFacts> {
        Ok(ReadFacts {
            documents: scip::parse(bytes, root, base)?,
            gaps: Vec::new(),
        })
    }
}

fn ordinary_reader() -> Box<dyn Reader> {
    Box::new(ScipReader)
}

fn project_inputs(context: &InputContext<'_>) -> Result<Vec<Input>> {
    let prefix = if context.dir.is_empty() {
        String::new()
    } else {
        format!("{}/", context.dir)
    };
    let above = |file: &str| {
        let parent = relpath::parent(file);
        parent.len() < context.dir.len()
            && (parent.is_empty() || context.dir.starts_with(&format!("{parent}/")))
    };
    Ok(context
        .files
        .iter()
        .filter(|file| {
            (file.starts_with(&prefix)
                && !context.language.in_skip_dir(file)
                && (context.language.project_files)(file))
                || above(file)
        })
        .map(|file| Input::File(context.root.join(file)))
        .collect())
}

/// Go's own variables (`GOFLAGS`, `GOOS`, `GO111MODULE`) have no `_` after `GO`,
/// unlike `GOOGLE_APPLICATION_CREDENTIALS`, which a CI job may point at another
/// file every run; cgo's are `CGO_*`.
fn go_env(name: &str) -> bool {
    name.strip_prefix("GO").is_some_and(|rest| {
        rest.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    }) || name.starts_with("CGO_")
}

fn starts_with_any(name: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| name.starts_with(prefix))
}

/// pyright asks the `python3` on the path for its search paths, and scip-python
/// runs `pip` for the installed packages: `PYTHONPATH` changes what an import
/// resolves to. `PYTHONHASHSEED` is left out: tox sets a new one every run. So
/// is `VIRTUAL_ENV`, which alone does not change the index and names the
/// checkout when the environment lives in it, as uv and Poetry put it.
fn python_env(name: &str) -> bool {
    (name.starts_with("PYTHON") && name != "PYTHONHASHSEED") || name.starts_with("PIP_")
}

/// scip-java runs Maven or Gradle with the JDK of `JAVA_HOME`: `MAVEN_ARGS`
/// can skip the build, `ORG_GRADLE_PROJECT_*` sets project properties. Not
/// `JAVA_*` as a prefix: CI runners name every JDK they carry
/// (`JAVA_HOME_17_X64`), used or not.
fn java_env(name: &str) -> bool {
    matches!(
        name,
        "JAVA_HOME"
            | "JAVA_OPTS"
            | "JAVA_TOOL_OPTIONS"
            | "JDK_JAVA_OPTIONS"
            | "_JAVA_OPTIONS"
            | "GRADLE_OPTS"
            | "GRADLE_USER_HOME"
    ) || starts_with_any(name, &["MAVEN_", "ORG_GRADLE_PROJECT_"])
}

/// MSBuild reads every environment variable as a property, by a name of any
/// case (`DefineConstants=X` turns on `#if X`). Keying all of them would miss on
/// every CI run, so the key holds the SDK's own and the properties that decide
/// what is compiled; a project that reads another variable needs `--no-cache`
/// after changing it.
fn csharp_env(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    starts_with_any(&name, &["DOTNET_", "MSBUILD", "NUGET_"])
        || matches!(
            name.as_str(),
            "DEFINECONSTANTS"
                | "CONFIGURATION"
                | "PLATFORM"
                | "TARGETFRAMEWORK"
                | "TARGETFRAMEWORKS"
                | "RUNTIMEIDENTIFIER"
                | "RUNTIMEIDENTIFIERS"
                | "LANGVERSION"
        )
}

/// rust-analyzer runs Cargo and rustc of the toolchain rustup picks: flags,
/// `--cfg`, the toolchain and the standard library's sources change the index.
/// Not the variables Cargo sets for what it runs (`CARGO_PKG_NAME`,
/// `CARGO_MANIFEST_DIR`), which name the checkout and so would keep a
/// worktree from reusing the index.
fn rust_env(name: &str) -> bool {
    const SET_BY_CARGO: &[&str] = &[
        "CARGO_MANIFEST_",
        "CARGO_PKG_",
        "CARGO_CRATE_NAME",
        "CARGO_BIN_",
        "CARGO_PRIMARY_PACKAGE",
        "CARGO_TARGET_TMPDIR",
        "CARGO_MAKEFLAGS",
        "CARGO_CFG_",
        "CARGO_FEATURE_",
    ];
    starts_with_any(name, &["RUSTFLAGS", "RUSTC", "RUSTDOC"])
        || matches!(
            name,
            "CARGO" | "RUSTUP_TOOLCHAIN" | "RUSTUP_HOME" | "RUST_SRC_PATH"
        )
        || (name.starts_with("CARGO_") && !starts_with_any(name, SET_BY_CARGO))
}

fn nothing_needed(_: &Path, _: &Path, _: &str) -> Result<()> {
    Ok(())
}

/// Every file: for an indexer whose build reads files of its own choosing (Maven, Gradle,
/// Cargo, MSBuild, Bundler, Composer, Python's environment).
fn every_file(_: &str) -> bool {
    true
}

/// scip-go reads the Go sources and what `go` resolves them with: the module and workspace
/// files and a vendored dependency list.
fn go_inputs(path: &str) -> bool {
    let name = relpath::file_name(path);
    name.ends_with(".go")
        || matches!(
            name,
            "go.mod" | "go.sum" | "go.work" | "go.work.sum" | "modules.txt"
        )
}

/// scip-typescript reads what the TypeScript compiler does: the sources and declarations, every
/// JSON file (a `tsconfig.json` and what it extends, a `package.json`, a JSON module an import
/// names) and the lock files that decide which dependency is installed.
fn typescript_inputs(path: &str) -> bool {
    let name = relpath::file_name(path);
    let source = registry::TYPESCRIPT
        .extensions
        .contains(&relpath::extension(name));
    source
        || name.ends_with(".json")
        || name.ends_with(".json5")
        || matches!(
            name,
            "pnpm-lock.yaml" | "pnpm-workspace.yaml" | "yarn.lock" | "bun.lock" | "bun.lockb"
        )
}

pub static INDEXERS: &[Indexer] = &[
    Indexer {
        language: &registry::GO,
        binary: "scip-go",
        markers: &["go.mod"],
        outermost: false,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: false,
        args: |out| vec!["--output".into(), out.display().to_string()],
        install: "mise use -g go:github.com/scip-code/scip-go/cmd/scip-go",
        mise: Some("go:github.com/scip-code/scip-go/cmd/scip-go"),
        skip_dirs: &[],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: project_inputs,
        project_files: go_inputs,
        env: go_env,
    },
    Indexer {
        language: &registry::TYPESCRIPT,
        binary: "scip-typescript",
        markers: &["tsconfig.json"],
        outermost: false,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: false,
        args: |out| {
            vec![
                "index".into(),
                "--no-progress-bar".into(),
                "--output".into(),
                out.display().to_string(),
            ]
        },
        install: "mise use -g npm:@sourcegraph/scip-typescript",
        mise: Some("npm:@sourcegraph/scip-typescript"),
        skip_dirs: &[],
        writes_in_cwd: None,
        needs: require_installed,
        inputs: project_inputs,
        project_files: typescript_inputs,
        env: |_| false,
    },
    Indexer {
        language: &registry::PYTHON,
        binary: "scip-python",
        markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        outermost: false,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: true,
        args: |out| {
            vec![
                "index".into(),
                ".".into(),
                "--quiet".into(),
                "--project-name".into(),
                "project".into(),
                "--project-version".into(),
                "0".into(),
                "--output".into(),
                out.display().to_string(),
            ]
        },
        install: "mise use -g npm:@sourcegraph/scip-python",
        mise: Some("npm:@sourcegraph/scip-python"),
        skip_dirs: &[],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: project_inputs,
        project_files: every_file,
        env: python_env,
    },
    Indexer {
        language: &registry::PHP,
        binary: "scip-php",
        markers: &["composer.json"],
        outermost: false,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: false,
        args: |_| Vec::new(),
        install: "composer global require davidrjenni/scip-php",
        mise: None,
        skip_dirs: &["vendor"],
        writes_in_cwd: Some("index.scip"),
        needs: require_composer,
        inputs: project_inputs,
        project_files: every_file,
        env: |_| false,
    },
    Indexer {
        language: &registry::JAVA,
        binary: "scip-java",
        markers: &["pom.xml", "build.gradle", "build.gradle.kts"],
        outermost: true,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: false,
        args: |out| vec!["index".into(), "--output".into(), out.display().to_string()],
        install: "mise use -g github:sourcegraph/scip-java",
        mise: Some("github:sourcegraph/scip-java"),
        skip_dirs: &["target", "build"],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: project_inputs,
        project_files: every_file,
        env: java_env,
    },
    Indexer {
        language: &registry::CSHARP,
        binary: "scip-dotnet",
        markers: &["*.csproj", "*.sln", "*.slnx"],
        outermost: true,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: false,
        args: |out| vec!["index".into(), "--output".into(), out.display().to_string()],
        install: "dotnet tool install --global scip-dotnet",
        mise: None,
        skip_dirs: &["bin", "obj"],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: project_inputs,
        project_files: every_file,
        env: csharp_env,
    },
    Indexer {
        language: &registry::RUBY,
        binary: "scip-ruby",
        markers: &["Gemfile", "*.gemspec"],
        outermost: true,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: false,
        args: |out| {
            vec![
                "--gem-metadata".into(),
                "project@0".into(),
                "--index-file".into(),
                out.display().to_string(),
                ".".into(),
            ]
        },
        install: "mise use -g github:sourcegraph/scip-ruby",
        mise: Some("github:sourcegraph/scip-ruby"),
        skip_dirs: &["vendor"],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: project_inputs,
        project_files: every_file,
        // scip-ruby reads no variable that changes the index; Bundler's are read
        // by the gem's wrapper, which easement does not run.
        env: |_| false,
    },
    Indexer {
        language: &registry::RUST,
        binary: "rust-analyzer",
        markers: &["Cargo.toml"],
        outermost: true,
        root_policy: RootPolicy::Markers,
        reader: ordinary_reader,
        strict_version: false,
        fallback_root: false,
        args: |out| {
            vec![
                "scip".into(),
                ".".into(),
                "--output".into(),
                out.display().to_string(),
            ]
        },
        install: "rustup component add rust-analyzer",
        mise: None,
        skip_dirs: &[],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: project_inputs,
        project_files: every_file,
        env: rust_env,
    },
    Indexer {
        language: &registry::DART,
        binary: "scip_dart",
        markers: &["pubspec.yaml"],
        outermost: false,
        root_policy: RootPolicy::SourceOwners,
        reader: dart::reader,
        strict_version: true,
        fallback_root: false,
        args: |out| vec!["--output".into(), out.display().to_string(), ".".into()],
        install: "dart pub global activate scip_dart 1.8.1",
        mise: None,
        skip_dirs: &[".dart_tool"],
        writes_in_cwd: None,
        needs: dart::needs,
        inputs: dart::resolved_inputs,
        project_files: every_file,
        env: |name| name != "EASEMENT_CACHE_DIR",
    },
];

impl Indexer {
    pub fn name(&self) -> &'static str {
        self.language.name
    }

    pub fn covers(&self, file: &str) -> bool {
        self.language.extensions.contains(&relpath::extension(file))
            && !file.ends_with(".d.ts")
            && !self.in_skip_dir(file)
    }

    /// A marker is a file name, or `*.ext` for any file with that extension.
    fn has_marker(&self, name: &str) -> bool {
        self.markers
            .iter()
            .any(|marker| match marker.strip_prefix('*') {
                Some(suffix) => name.ends_with(suffix),
                None => *marker == name,
            })
    }

    /// Whether a file is in a directory of build output or dependencies.
    fn in_skip_dir(&self, file: &str) -> bool {
        file.split('/').any(|part| self.skip_dirs.contains(&part))
    }

    fn index_roots(&self, root: &Path, files: &[String]) -> Result<Vec<String>> {
        if matches!(self.root_policy, RootPolicy::Markers) {
            return Ok(self.roots(files));
        }
        let marker_in = |dir: &Path| {
            let relative = crate::scan::slash(dir.strip_prefix(root).expect("project ancestor"));
            files.iter().any(|file| {
                relpath::parent(file) == relative && self.has_marker(relpath::file_name(file))
            }) || self
                .markers
                .iter()
                .any(|marker| !marker.contains('*') && dir.join(marker).is_file())
                || (self.markers.iter().any(|marker| marker.contains('*'))
                    && std::fs::read_dir(dir)
                        .ok()
                        .into_iter()
                        .flatten()
                        .flatten()
                        .any(|entry| {
                            entry.file_type().is_ok_and(|kind| kind.is_file())
                                && entry
                                    .file_name()
                                    .to_str()
                                    .is_some_and(|name| self.has_marker(name))
                        }))
        };
        let mut roots = BTreeSet::new();
        for file in files.iter().filter(|file| self.covers(file)) {
            let absolute = root.join(file);
            let owner = absolute
                .ancestors()
                .skip(1)
                .take_while(|dir| dir.starts_with(root))
                .find(|dir| marker_in(dir))
                .with_context(|| {
                    format!(
                        "{file} has no owning {} inside the analysis root; \
                         run from the package or workspace root",
                        self.markers.join(" or ")
                    )
                })?;
            roots.insert(crate::scan::slash(owner.strip_prefix(root)?));
        }
        Ok(roots.into_iter().collect())
    }

    fn roots(&self, files: &[String]) -> Vec<String> {
        let mut dirs: Vec<String> = files
            .iter()
            .filter(|f| self.has_marker(relpath::file_name(f)))
            .map(|f| relpath::parent(f).to_string())
            .collect();
        dirs.sort();
        dirs.dedup();
        if self.outermost {
            let all = dirs.clone();
            dirs.retain(|d| {
                !all.iter()
                    .any(|p| p != d && (p.is_empty() || d.starts_with(&format!("{p}/"))))
            });
        }
        if dirs.is_empty() && self.fallback_root {
            dirs.push(String::new());
        }
        dirs
    }
}

/// `--cache-only` met an index that is not cached: the indexer would have to run.
#[derive(Debug)]
pub struct NotCached {
    pub language: &'static str,
    pub dir: String,
}

impl std::fmt::Display for NotCached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = if self.dir.is_empty() { "." } else { &self.dir };
        write!(
            f,
            "the {} index of {shown} is not cached, and --cache-only runs no indexer",
            self.language
        )
    }
}

impl std::error::Error for NotCached {}

/// How indexes are obtained: run the indexer, or read a cached index first, or only read one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CacheUse {
    Off,
    #[default]
    On,
    Only,
}

/// Coverage checks over extracted evidence, evaluated against the current policy.
/// The checks retain facts, never policy decisions, including on a cache hit.
pub type CoverageCheck = Box<
    dyn Fn(
            &crate::config::Config,
            &crate::components::ComponentMap,
        ) -> Result<Vec<crate::scan::AnalysisGap>>
        + Send
        + Sync,
>;

/// What the indexers found, and the source files they did not cover.
pub struct Indexed {
    pub coverage_checks: Vec<CoverageCheck>,
    /// Extraction scopes reconstructed for both fresh and cached indexes.
    pub domains: Vec<crate::coverage::Domain>,
    pub analysis_partial: Vec<crate::scan::AnalysisGap>,
    pub documents: Documents,
    /// Indexers that ran, and indexes read from the cache instead.
    pub ran: usize,
    pub cached: usize,
    /// Source files of an indexed language that no index mentions.
    pub uncovered: Vec<String>,
}

/// Go build tags are passed through the indexer's inherited environment.
fn environment(language: &Indexer, go_tags: &[String]) -> Vec<(&'static str, String)> {
    if language.name() != "go" || go_tags.is_empty() {
        return Vec::new();
    }
    let mut flags = std::env::var("GOFLAGS").unwrap_or_default();
    if !flags.is_empty() {
        flags.push(' ');
    }
    flags.push_str(&format!("-tags={}", go_tags.join(",")));
    vec![("GOFLAGS", flags)]
}

/// Indexes every language that has source files among `files` (project files,
/// `/`-separated, relative to `root`). The scan policy supplies extraction settings;
/// coverage checks judge the retained evidence against the loaded policy later.
///
/// `excluded` are files that `exclude` hides: they are not indexed for
/// themselves, but the index resolves imports of them, so they are part of
/// the cache key.
pub fn run(
    root: &Path,
    files: &[String],
    excluded: &[String],
    cache: CacheUse,
    jobs: Jobs,
    policy: &crate::config::ScanPolicy,
) -> Result<Indexed> {
    let context = RunContext {
        root,
        files,
        excluded,
    };
    let keyed: Vec<String> = files.iter().chain(excluded).cloned().collect();
    let mut groups = Vec::new();
    let mut wanted = Vec::new();
    for language in INDEXERS {
        wanted.extend(files.iter().filter(|file| language.covers(file)).cloned());
        if !files.iter().any(|file| language.covers(file)) {
            continue;
        }
        let roots = language.index_roots(root, files)?;
        if !roots.is_empty() {
            groups.push((language, roots));
        }
    }
    let results = in_parallel(
        &groups,
        jobs.get(),
        |(language, roots)| -> Result<Indexed> {
            let mut reader = (language.reader)();
            let mut parts = Vec::new();
            let mut gaps = Vec::new();
            let mut domains = Vec::new();
            let (mut ran, mut cached) = (0, 0);
            for dir in roots {
                let (read, hit) = index_dir(
                    language,
                    reader.as_mut(),
                    root,
                    dir,
                    &keyed,
                    cache,
                    &policy.go_tags,
                )?;
                let files = keyed
                    .iter()
                    .filter(|file| {
                        language.covers(file)
                            && (dir.is_empty() || file.starts_with(&format!("{dir}/")))
                            && (!matches!(language.root_policy, RootPolicy::SourceOwners)
                                || !roots.iter().any(|nested| {
                                    nested.len() > dir.len()
                                        && file.starts_with(&format!("{nested}/"))
                                }))
                    })
                    .cloned()
                    .collect();
                domains.push(crate::coverage::Domain {
                    root: dir.clone(),
                    language: language.name(),
                    files,
                    gaps: read.gaps.clone(),
                });
                parts.push(read.documents);
                gaps.extend(read.gaps);
                if hit {
                    cached += 1;
                } else {
                    ran += 1;
                }
            }
            let mut indexed = reader.finish(language, Documents::merged(parts)?, gaps, &context)?;
            indexed.domains = domains;
            indexed.ran = ran;
            indexed.cached = cached;
            Ok(indexed)
        },
    )?;
    let mut parts = Vec::new();
    let mut analysis_partial = Vec::new();
    let mut coverage_checks = Vec::new();
    let mut domains = Vec::new();
    let mut uncovered = BTreeSet::new();
    let (mut ran, mut cached) = (0, 0);
    for indexed in results {
        parts.push(indexed.documents.0);
        coverage_checks.extend(indexed.coverage_checks);
        domains.extend(indexed.domains);
        analysis_partial.extend(indexed.analysis_partial);
        uncovered.extend(indexed.uncovered);
        ran += indexed.ran;
        cached += indexed.cached;
    }
    let documents = if parts.is_empty() {
        Documents::default()
    } else {
        Documents::merged(parts)?
    };
    let covered: BTreeSet<&str> = documents.0.iter().map(|d| d.path.as_str()).collect();
    uncovered.extend(
        wanted
            .into_iter()
            .filter(|file| !covered.contains(file.as_str())),
    );
    Ok(Indexed {
        coverage_checks,
        domains,
        documents,
        analysis_partial,
        uncovered: uncovered.into_iter().collect(),
        ran,
        cached,
    })
}

/// How many languages are indexed at once (`--jobs`): one by default, since
/// scip-java and rust-analyzer can each take gigabytes. The directories of one
/// language are always indexed one after another: they share its build state
/// (Maven's repository, the lock of cargo's `target`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jobs(std::num::NonZero<usize>);

impl Jobs {
    pub fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs(std::num::NonZero::<usize>::MIN)
    }
}

impl std::str::FromStr for Jobs {
    type Err = String;

    /// A number of languages, or `cpus` for as many as the machine has processors.
    fn from_str(text: &str) -> Result<Self, String> {
        if text == "cpus" {
            return Ok(Jobs(
                std::thread::available_parallelism().unwrap_or(std::num::NonZero::<usize>::MIN),
            ));
        }
        text.parse()
            .map(Jobs)
            .map_err(|_| format!("`{text}` is neither a number of 1 or more nor `cpus`"))
    }
}

/// Runs `job` on every group, `at_most` of them at once, and returns the results in
/// the order of `groups`, or the failure that a run in order would meet first. After
/// a failure, the groups behind it do not start; the ones before it still finish,
/// since one of them may fail too and come first.
fn in_parallel<T: Sync, R: Send, E: Send>(
    groups: &[T],
    at_most: usize,
    job: impl Fn(&T) -> Result<R, E> + Sync,
) -> Result<Vec<R>, E> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let failed_at = AtomicUsize::new(usize::MAX);
    let slots: Vec<std::sync::Mutex<Option<Result<R, E>>>> =
        groups.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..at_most.min(groups.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(group) = groups.get(i) else { break };
                    if i > failed_at.load(Ordering::SeqCst) {
                        continue;
                    }
                    let result = job(group);
                    if result.is_err() {
                        failed_at.fetch_min(i, Ordering::SeqCst);
                    }
                    // No job runs while a slot is locked, so a lock is never poisoned.
                    *slots[i].lock().expect("a slot is never poisoned") = Some(result);
                }
            });
        }
    });
    let mut results = Vec::with_capacity(groups.len());
    for slot in slots {
        // A group is skipped only behind a failure, which this loop returns first.
        match slot.into_inner().expect("a slot is never poisoned") {
            Some(Ok(result)) => results.push(result),
            Some(Err(failure)) => return Err(failure),
            None => unreachable!("a group was skipped, but no group before it failed"),
        }
    }
    Ok(results)
}

/// Where indexes are kept between runs: `EASEMENT_CACHE_DIR`, else the user's cache directory.
fn cache_dir() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = var("EASEMENT_CACHE_DIR");
    if base.is_some() {
        return base;
    }
    let user = var("XDG_CACHE_HOME").or_else(|| {
        if cfg!(windows) {
            var("LOCALAPPDATA")
        } else if cfg!(target_os = "macos") {
            var("HOME").map(|home| home.join("Library/Caches"))
        } else {
            var("HOME").map(|home| home.join(".cache"))
        }
    })?;
    Some(user.join("easement"))
}

/// Hash the indexer, configured inputs, commands and inherited environment.
/// Unverifiable inputs disable caching; they never become a reusable weak key.
fn fingerprint(
    language: &Indexer,
    binary: &Path,
    root: &Path,
    dir: &str,
    files: &[String],
    env: &[(String, String)],
) -> Result<u64> {
    let mut hash = StableHasher::default();
    "easement-index-v2".hash(&mut hash);
    language.name().hash(&mut hash);
    env.hash(&mut hash);
    (language.args)(Path::new("")).hash(&mut hash);
    let identity = what_runs(binary, language.binary, &root.join(dir), &multiplexers());
    if !matches!(identity, Runs::Binary((_, Some(_), _))) {
        bail!("indexer implementation cannot be verified for caching");
    }
    identity.hash(&mut hash);
    let mut inputs = (language.inputs)(&InputContext {
        language,
        binary,
        root,
        dir,
        files,
    })?;
    // Read content again even if the implementation identity was memoized. A
    // replacement preserving size and mtime must not reuse an extraction.
    inputs.push(Input::File(binary.to_path_buf()));
    if let Runs::Binary((Some(real), _, _)) = identity {
        inputs.push(Input::File(real));
    }
    cache::hash_inputs(inputs, root, &mut hash)?;
    Ok(hash.finish())
}

/// The environment an indexer runs with, as far as it changes the index: the
/// inherited variables that `Language::env` names, with what easement sets for
/// the run over them.
fn key_env(
    language: &Indexer,
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    set: &[(&str, String)],
) -> Vec<(String, String)> {
    let mut env: BTreeMap<String, String> = inherited
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .filter(|(k, _)| (language.env)(k))
        .collect();
    env.extend(set.iter().map(|(k, v)| (k.to_string(), v.clone())));
    env.into_iter().collect()
}

/// What tells one build of an indexer from another: where it really is (symbolic links
/// resolved, so a version manager's install directory names the version) and a hash of its
/// content. Not when it was installed: a CI runner that restores the toolchain from a cache gets
/// the same files with other modification times, and must still find the indexes. The indexer is
/// not run for a version: some (scip-php) take any argument as a request to index. A binary
/// that cannot be read is told apart by its modification time, which only costs reuse.
fn indexer_identity(
    binary: &Path,
) -> (Option<PathBuf>, Option<u64>, Option<std::time::SystemTime>) {
    type Seen =
        std::collections::HashMap<(PathBuf, Option<std::time::SystemTime>, u64), Option<u64>>;
    static CONTENT: std::sync::OnceLock<std::sync::Mutex<Seen>> = std::sync::OnceLock::new();
    let real = std::fs::canonicalize(binary).ok();
    let path = real.clone().unwrap_or_else(|| binary.to_path_buf());
    let meta = std::fs::metadata(&path).ok();
    let modified = meta.as_ref().and_then(|m| m.modified().ok());
    // Hashed once per process while the file stays as it is; an indexer upgraded in
    // place during `--watch` has another time or size, and is hashed again.
    let seen = (path.clone(), modified, meta.map_or(0, |m| m.len()));
    let hash = |path: &Path| std::fs::read(path).ok().map(|bytes| content_hash(&bytes));
    let content = CONTENT
        .get_or_init(Default::default)
        .lock()
        .map(|mut known| *known.entry(seen).or_insert_with(|| hash(&path)))
        .unwrap_or_else(|_| hash(&path));
    (
        real,
        content,
        if content.is_some() { None } else { modified },
    )
}

/// What the key holds of an indexer: the binary it is, or, for a multiplexer's
/// proxy, the binary the multiplexer runs.
#[derive(Debug, PartialEq, Hash)]
enum Runs {
    Binary((Option<PathBuf>, Option<u64>, Option<std::time::SystemTime>)),
    /// A proxy whose multiplexer could not tell what it runs here: never the
    /// proxy's own identity, which every toolchain shares.
    Unresolved(Option<PathBuf>),
}

/// A program that runs another one by the name it is called with. rustup's
/// proxies (`~/.cargo/bin/rust-analyzer`) and mise's shims are one file for
/// every toolchain, so what tells two indexers apart is the binary each runs
/// from the index directory, where `rust-toolchain.toml` or `mise.toml`
/// choose it.
struct Multiplexer {
    path: PathBuf,
    resolve: Box<Resolve>,
}

/// The binary a multiplexer runs for `name` in a directory, `None` when it cannot tell.
type Resolve = dyn Fn(&str, &Path) -> Option<PathBuf>;

/// The multiplexers on the path: rustup and mise, both of which answer `which <name>`.
fn multiplexers() -> Vec<Multiplexer> {
    ["rustup", "mise"]
        .into_iter()
        .filter_map(|tool| {
            let path = which::which(tool).ok()?;
            let program = path.clone();
            let resolve = move |name: &str, cwd: &Path| {
                let output = Command::new(&program)
                    .args(["which", name])
                    .current_dir(cwd)
                    // A toolchain that is not installed is an answer, not a download.
                    .env("RUSTUP_AUTO_INSTALL", "0")
                    .output()
                    .ok()?;
                let found = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
                (output.status.success() && found.is_file()).then_some(found)
            };
            Some(Multiplexer {
                path,
                resolve: Box::new(resolve),
            })
        })
        .collect()
}

/// What runs when `binary` (installed as `name`) runs in `cwd`. A file with the
/// content of a multiplexer is its proxy, whether a symbolic link, a hard link
/// or a copy.
fn what_runs(binary: &Path, name: &str, cwd: &Path, multiplexers: &[Multiplexer]) -> Runs {
    let own = indexer_identity(binary);
    let size = |path: &Path| std::fs::metadata(path).map(|m| m.len()).ok();
    let proxy = multiplexers.iter().find(|m| {
        own.1.is_some() && size(&m.path) == size(binary) && indexer_identity(&m.path).1 == own.1
    });
    match proxy {
        None => Runs::Binary(own),
        Some(multiplexer) => match (multiplexer.resolve)(name, cwd) {
            Some(real) => Runs::Binary(indexer_identity(&real)),
            None => Runs::Unresolved(own.0),
        },
    }
}

/// A hasher for the keys of the index cache, which outlives the build of easement that
/// wrote it: `DefaultHasher` may change its algorithm in any Rust release, BLAKE3 does not.
/// The bytes still come from std's `Hash`, which Rust does not promise to keep either; a
/// change there only makes the cache miss once.
#[derive(Default)]
struct StableHasher(blake3::Hasher);

impl Hasher for StableHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(&self) -> u64 {
        first_word(self.0.finalize())
    }
}

/// The BLAKE3 hash of a file's content, cut to 64 bits.
fn content_hash(bytes: &[u8]) -> u64 {
    first_word(blake3::hash(bytes))
}

fn first_word(hash: blake3::Hash) -> u64 {
    let mut word = [0; 8];
    word.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_le_bytes(word)
}

/// The file the index of `dir` is kept in; the name carries the fingerprint, so a
/// changed project misses it, and `prefix` finds the entries of the same directory
/// and indexer, which are pruned to the most recently used few.
fn cache_file(language: &Indexer, dir: &str, key: u64) -> Option<(PathBuf, String)> {
    let mut hasher = StableHasher::default();
    (language.name(), dir).hash(&mut hasher);
    let prefix = format!("{:016x}-", hasher.finish());
    Some((
        cache_dir()?.join(format!("v2-{prefix}{key:016x}.idx")),
        format!("v2-{prefix}"),
    ))
}

/// Whether the indexer of one language can run in a project.
#[derive(Debug, serde::Serialize)]
pub struct Diagnosis {
    pub language: &'static str,
    pub binary: &'static str,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub install: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mise: Option<&'static str>,
    /// What is missing around the indexer: dependencies that are not installed.
    pub problems: Vec<String>,
}

impl Diagnosis {
    pub fn ok(&self) -> bool {
        self.found && self.problems.is_empty()
    }
}

/// For every language with source files among `files`: is its indexer on the
/// `PATH`, which version, and is what it needs in place.
pub fn diagnose(root: &Path, files: &[String]) -> Vec<Diagnosis> {
    INDEXERS
        .iter()
        .filter(|language| files.iter().any(|f| language.covers(f)))
        .map(|language| {
            let binary = which::which(language.binary).ok();
            let mut problems = Vec::new();
            match language.index_roots(root, files) {
                Ok(roots) => {
                    for dir in roots {
                        let shown = if dir.is_empty() { "." } else { &dir };
                        if let Err(error) = (language.needs)(root, &root.join(&dir), shown) {
                            problems.push(format!("{error:#}"));
                        }
                    }
                }
                Err(error) => problems.push(format!("{error:#}")),
            }
            let version = binary.as_deref().and_then(|path| {
                match version_of(path, language.strict_version) {
                    Ok(version) => version,
                    Err(error) => {
                        problems.push(format!("{error:#}"));
                        None
                    }
                }
            });
            Diagnosis {
                language: language.name(),
                binary: language.binary,
                found: binary.is_some(),
                path: binary.map(|p| p.display().to_string()),
                version,
                install: language.install,
                mise: language.mise,
                problems,
            }
        })
        .collect()
}

/// The first line a tool prints for `--version`.
fn version_of(binary: &Path, strict: bool) -> Result<Option<String>> {
    let output = match Command::new(binary).arg("--version").output() {
        Ok(output) => output,
        Err(error) if strict => {
            return Err(error).with_context(|| format!("cannot execute {}", binary.display()));
        }
        Err(_) => return Ok(None),
    };
    if strict && !output.status.success() {
        bail!(
            "{} --version failed ({}); reinstall the indexer: {}",
            binary.file_name().unwrap_or_default().to_string_lossy(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("no diagnostic")
        );
    }
    Ok([output.stdout, output.stderr].iter().find_map(|bytes| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    }))
}

fn index_dir(
    language: &Indexer,
    reader: &mut dyn Reader,
    root: &Path,
    dir: &str,
    files: &[String],
    cache: CacheUse,
    go_tags: &[String],
) -> Result<(ReadFacts, bool)> {
    let env = environment(language, go_tags);
    let cwd = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    let shown = if dir.is_empty() { "." } else { dir };
    // `which` finds `scip-typescript.cmd` on Windows, which `Command` alone does not.
    let binary = which::which(language.binary).map_err(|_| {
        anyhow::anyhow!(
            "`{}` is not installed, and easement needs it for {} files; install it with `{}`",
            language.binary,
            language.name(),
            language.install
        )
    })?;
    let key = if cache == CacheUse::Off {
        None
    } else {
        fingerprint(
            language,
            &binary,
            root,
            dir,
            files,
            &key_env(language, std::env::vars_os(), &env),
        )
        .ok()
    };
    let cached = key.and_then(|key| cache_file(language, dir, key));
    if let Some((path, _)) = &cached
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(artifact) = cache::decode(&bytes)
        && let Ok(mut read) = reader.read(artifact.index, root, Path::new(dir))
        && !read.documents.is_empty()
    {
        if let Ok(file) = std::fs::File::options().write(true).open(path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        read.gaps.extend(artifact.gaps);
        return Ok((read, true));
    }
    if cache == CacheUse::Only {
        return Err(NotCached {
            language: language.name(),
            dir: dir.to_string(),
        }
        .into());
    }
    // Checked only when the indexer runs: a cached index was written with them in place, and
    // what they are is in its key (a lock file).
    (language.needs)(root, &cwd, shown)?;
    let out = tempfile::tempdir().context("cannot create a temporary directory")?;
    let file = match language.writes_in_cwd {
        Some(name) => {
            let written = cwd.join(name);
            if written.exists() {
                bail!(
                    "{} would overwrite {}, which is in the way; move it",
                    language.binary,
                    written.display()
                );
            }
            written
        }
        None => out.path().join("index.scip"),
    };
    let output = Command::new(&binary)
        .args((language.args)(&file))
        .envs(env.iter().map(|(k, v)| (*k, v)))
        .current_dir(&cwd)
        .output()
        .with_context(|| format!("cannot run {}", language.binary))?;
    if !output.status.success() {
        // Some indexers report on stdout (Maven), others on stderr.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        // The first lines name the error, the last ones the place; keep both.
        let excerpt = if lines.len() > 14 {
            [&lines[..5], &["    …"][..], &lines[lines.len() - 8..]].concat()
        } else {
            lines
        };
        let hint = if text.contains("LAUNCHER_ARGS[@]: unbound variable") {
            "\nscip-java's wrapper needs bash 4.4 or newer; the bash of macOS is 3.2, put a newer one first on the PATH (`brew install bash`)"
        } else {
            ""
        };
        bail!(
            "{} failed in {shown} ({}); if that directory is not part of the project, add it to `exclude`:\n{}{hint}",
            language.binary,
            output.status,
            excerpt.join("\n")
        );
    }
    let bytes = std::fs::read(&file)
        .with_context(|| format!("{} wrote no index in {shown}", language.binary));
    if language.writes_in_cwd.is_some() {
        let _ = std::fs::remove_file(&file);
    }
    let bytes = bytes?;
    let mut read = reader
        .read(&bytes, root, Path::new(dir))
        .with_context(|| format!("cannot read the index of {} in {shown}", language.binary))?;
    if read.documents.is_empty() {
        bail!(
            "{} indexed no files in {shown}; check what its project file includes",
            language.binary
        );
    }
    let validation = reader.validate(root, &cwd)?;
    if let Some((path, prefix)) = cached
        && key
            == fingerprint(
                language,
                &binary,
                root,
                dir,
                files,
                &key_env(language, std::env::vars_os(), &env),
            )
            .ok()
    {
        cache::write(&path, &prefix, &bytes, &validation);
    }
    read.gaps.extend(validation);
    Ok((read, false))
}

/// Entries of one directory and indexer that are kept: checkouts of the same
/// project share them, so a new entry must not remove those of the others.
const KEPT_PER_DIR: usize = 32;

/// Removes all but the `KEPT_PER_DIR` most recently used entries with `prefix`.
fn prune(parent: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut mine: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    mine.sort_by(|a, b| b.cmp(a));
    for (_, path) in mine.into_iter().skip(KEPT_PER_DIR) {
        let _ = std::fs::remove_file(path);
    }
}

/// `scip-php` reads the dependencies' code from `vendor`, through the autoloader.
fn require_composer(_root: &Path, dir: &Path, shown: &str) -> Result<()> {
    if !dir.join("composer.lock").is_file() || !dir.join("vendor/autoload.php").is_file() {
        bail!(
            "dependencies are not installed in {shown}: run `composer install` (scip-php needs composer.lock and vendor/)"
        );
    }
    Ok(())
}

/// Without `node_modules` the TypeScript indexer cannot see third-party
/// modules, so external dependencies would silently disappear from the check.
fn require_installed(root: &Path, dir: &Path, shown: &str) -> Result<()> {
    let manifest = dir.join("package.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return Ok(());
    };
    let declares = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .is_some_and(|json| {
            ["dependencies", "devDependencies"].iter().any(|key| {
                json.get(key)
                    .and_then(|v| v.as_object())
                    .is_some_and(|o| !o.is_empty())
            })
        });
    if !declares {
        return Ok(());
    }
    let mut at: Option<PathBuf> = Some(dir.to_path_buf());
    while let Some(current) = at {
        if current.join("node_modules").is_dir() {
            return Ok(());
        }
        at = (current != root)
            .then(|| current.parent().map(Path::to_path_buf))
            .flatten();
    }
    bail!(
        "dependencies are not installed in {shown}: run `npm install` (or pnpm, yarn) first; `easement doctor` lists everything else that is missing"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn jobs_are_a_number_or_the_cpus() {
        assert_eq!(Jobs::default().get(), 1);
        assert_eq!("3".parse::<Jobs>().unwrap().get(), 3);
        let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        assert_eq!("cpus".parse::<Jobs>().unwrap().get(), cpus);
        assert!("0".parse::<Jobs>().is_err());
        assert!("many".parse::<Jobs>().is_err());
    }

    #[test]
    fn groups_run_at_most_so_many_at_once_and_answer_in_their_order() {
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let groups = [30, 0, 0, 0, 0];
        let results = in_parallel(&groups, 2, |&ms| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            // Wait for a second group to run, however late its thread starts; a pool
            // that runs one group at a time gives up here and fails the test.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while most.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            // The first group finishes last.
            std::thread::sleep(Duration::from_millis(ms));
            running.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, ()>(ms)
        });
        assert_eq!(results, Ok(groups.to_vec()));
        assert_eq!(most.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_failure_reported_is_the_first_in_order() {
        // The second group fails first, but a run in order meets the first one's failure.
        let groups = [(30, Err("first")), (0, Err("second")), (0, Ok(()))];
        let results = in_parallel(&groups, 3, |(ms, result)| {
            std::thread::sleep(Duration::from_millis(*ms));
            *result
        });
        assert_eq!(results, Err("first"));
    }

    #[test]
    fn after_a_failure_the_groups_behind_it_do_not_start() {
        let started = AtomicUsize::new(0);
        // The first group still runs to its end: it comes before the failure.
        let groups = [(30, Ok(())), (0, Err("second")), (0, Ok(())), (0, Ok(()))];
        let results = in_parallel(&groups, 2, |(ms, result)| {
            started.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(*ms));
            *result
        });
        assert_eq!(results, Err("second"));
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn each_indexer_keys_the_variables_that_change_its_index() {
        let keys = |language: &str, name: &str| {
            let indexer = INDEXERS.iter().find(|i| i.name() == language).unwrap();
            (indexer.env)(name)
        };
        for (language, name, keyed) in [
            ("go", "GOFLAGS", true),
            ("go", "GOOS", true),
            ("go", "CGO_ENABLED", true),
            ("go", "GOOGLE_APPLICATION_CREDENTIALS", false),
            ("go", "GOROOT_1_22_X64", false),
            ("typescript", "NODE_PATH", false),
            ("python", "PYTHONPATH", true),
            ("python", "PIP_USER", true),
            ("python", "VIRTUAL_ENV", false),
            ("python", "PYTHONHASHSEED", false),
            ("php", "COMPOSER_VENDOR_DIR", false),
            ("java", "JAVA_HOME", true),
            ("java", "MAVEN_ARGS", true),
            ("java", "ORG_GRADLE_PROJECT_flavor", true),
            ("java", "GRADLE_USER_HOME", true),
            ("java", "JAVA_HOME_17_X64", false),
            ("csharp", "DefineConstants", true),
            ("csharp", "MSBuildSDKsPath", true),
            ("csharp", "DOTNET_ROOT", true),
            ("csharp", "GITHUB_RUN_ID", false),
            ("ruby", "BUNDLE_GEMFILE", false),
            ("rust", "RUSTFLAGS", true),
            ("rust", "CARGO_ENCODED_RUSTFLAGS", true),
            ("rust", "CARGO_BUILD_RUSTFLAGS", true),
            ("rust", "RUSTUP_TOOLCHAIN", true),
            ("rust", "RUST_SRC_PATH", true),
            ("rust", "CARGO", true),
            ("rust", "CARGO_PKG_NAME", false),
            ("rust", "CARGO_MANIFEST_DIR", false),
        ] {
            assert_eq!(keys(language, name), keyed, "{language} {name}");
        }
    }

    #[test]
    fn the_cache_key_reads_the_environment_the_indexer_inherits() {
        let go = &INDEXERS[0];
        let ts = &INDEXERS[1];
        let inherited = || {
            [
                ("GOFLAGS", "-tags=race"),
                ("HOME", "/home/a"),
                ("CGO_ENABLED", "0"),
                // Not Go's: a CI job may point it at another file every run.
                ("GOOGLE_APPLICATION_CREDENTIALS", "/tmp/gha-creds-1.json"),
            ]
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        };
        assert_eq!(
            key_env(go, inherited(), &[]),
            [("CGO_ENABLED", "0"), ("GOFLAGS", "-tags=race")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        // What easement sets for the run wins over what it inherits.
        assert_eq!(
            key_env(go, inherited(), &[("GOFLAGS", "-tags=x".to_string())])[1],
            ("GOFLAGS".to_string(), "-tags=x".to_string())
        );
        assert!(key_env(ts, inherited(), &[]).is_empty());
    }

    #[test]
    fn a_proxy_is_known_by_the_binary_its_multiplexer_runs() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        };
        let mux = write("mux", "the multiplexer");
        // A proxy is the multiplexer's file under the indexer's name (a copy on Windows).
        let proxy = write("scip-x", "the multiplexer");
        let stable = write("scip-x-stable", "build 1");
        let nightly = write("scip-x-nightly", "build 2");
        let runs = |real: Option<PathBuf>| {
            let multiplexers = [Multiplexer {
                path: mux.clone(),
                resolve: Box::new(move |name, _| {
                    assert_eq!(name, "scip-x");
                    real.clone()
                }),
            }];
            what_runs(&proxy, "scip-x", dir.path(), &multiplexers)
        };
        let on_stable = runs(Some(stable.clone()));
        assert_eq!(on_stable, Runs::Binary(indexer_identity(&stable)));
        assert_ne!(on_stable, runs(Some(nightly)));
        // Unresolved is neither a toolchain's binary nor the proxy shared by all.
        let unresolved = runs(None);
        assert!(matches!(unresolved, Runs::Unresolved(_)));
        assert_ne!(unresolved, Runs::Binary(indexer_identity(&proxy)));
        // A binary of its own is itself.
        assert_eq!(
            what_runs(&stable, "scip-x", dir.path(), &[]),
            Runs::Binary(indexer_identity(&stable))
        );
    }

    #[test]
    fn an_indexer_replaced_in_place_has_another_identity() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("scip-x");
        std::fs::write(&binary, b"version 1").unwrap();
        let before = indexer_identity(&binary);
        assert_eq!(before, indexer_identity(&binary));
        std::fs::write(&binary, b"version 2, longer").unwrap();
        assert_ne!(before.1, indexer_identity(&binary).1);
    }

    #[test]
    fn cache_keys_hash_with_blake3_which_no_build_of_easement_changes() {
        // The BLAKE3 test vector: the hash of `abc` begins 64 37 b3 ac 38 46 51 33.
        assert_eq!(content_hash(b"abc"), 0x3351_4638_acb3_3764);
        let mut hasher = StableHasher::default();
        hasher.write(b"abc");
        assert_eq!(hasher.finish(), 0x3351_4638_acb3_3764);
    }

    #[test]
    fn languages_cover_their_sources_and_roots() {
        let ts = &INDEXERS[1];
        assert!(ts.covers("src/a.tsx") && ts.covers("a.mjs"));
        assert!(!ts.covers("src/a.d.ts") && !ts.covers("main.go"));
        let files: Vec<String> = [
            "go.mod",
            "svc/go.mod",
            "web/tsconfig.json",
            "a.go",
            "Cargo.toml",
            "crates/x/Cargo.toml",
            "tools/Cargo.toml",
            "app.py",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(INDEXERS[0].roots(&files), ["", "svc"]);
        assert_eq!(ts.roots(&files), ["web"]);
        // A workspace root holds its members; Python falls back to the root.
        assert_eq!(INDEXERS[7].roots(&files), [""]);
        assert_eq!(INDEXERS[2].roots(&files), [""]);
        let nested: Vec<String> = ["a/Cargo.toml", "b/Cargo.toml", "b/c/Cargo.toml"]
            .map(String::from)
            .to_vec();
        assert_eq!(INDEXERS[7].roots(&nested), ["a", "b"]);
    }

    #[test]
    fn source_owners_skip_workspace_containers_and_empty_packages() {
        let dart = INDEXERS.iter().find(|i| i.name() == "dart").unwrap();
        let files = [
            "pubspec.yaml",
            "packages/app/pubspec.yaml",
            "packages/app/lib/app.dart",
            "packages/empty/pubspec.yaml",
        ]
        .map(String::from);
        assert_eq!(
            dart.index_roots(tempfile::tempdir().unwrap().path(), &files)
                .unwrap(),
            ["packages/app"]
        );
    }

    #[test]
    fn source_owners_use_the_nearest_marker_and_never_escape_the_analysis_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("package/nested/lib")).unwrap();
        std::fs::write(temporary.path().join("pubspec.yaml"), "name: outside").unwrap();
        let dart = INDEXERS.iter().find(|i| i.name() == "dart").unwrap();
        let files = ["package/nested/lib/file.dart".to_string()];
        assert!(dart.index_roots(&root, &files).is_err());
        // Markers hidden from the source walk still determine ownership.
        std::fs::write(root.join("package/pubspec.yaml"), "name: package").unwrap();
        assert_eq!(dart.index_roots(&root, &files).unwrap(), ["package"]);
        std::fs::write(root.join("package/nested/pubspec.yaml"), "name: nested").unwrap();
        assert_eq!(dart.index_roots(&root, &files).unwrap(), ["package/nested"]);
        assert!(dart.index_roots(&root, &[]).unwrap().is_empty());
    }

    #[test]
    fn the_cache_key_of_go_and_typescript_reads_only_what_their_indexer_does() {
        for path in [
            "a.go",
            "svc/x_test.go",
            "go.mod",
            "go.sum",
            "go.work",
            "vendor/modules.txt",
        ] {
            assert!(go_inputs(path), "{path}");
        }
        for path in [
            "web/a.tsx",
            "README.md",
            "locales/en.json",
            "pnpm-lock.yaml",
            "x.goal",
        ] {
            assert!(!go_inputs(path), "{path}");
        }
        for path in [
            "src/a.tsx",
            "types/x.d.ts",
            "lib/a.mjs",
            "tsconfig.json",
            "tsconfig.app.json",
            "package.json",
            "locales/en.json",
            "pnpm-lock.yaml",
            "yarn.lock",
        ] {
            assert!(typescript_inputs(path), "{path}");
        }
        for path in ["main.go", "go.mod", "README.md", "style.css", "logo.svg"] {
            assert!(!typescript_inputs(path), "{path}");
        }
    }

    #[test]
    fn an_indexer_is_told_apart_by_its_content_not_by_when_it_was_installed() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("scip-go");
        std::fs::write(&first, "build 1").unwrap();
        let identity = indexer_identity(&first);
        assert!(identity.1.is_some() && identity.2.is_none(), "{identity:?}");
        // The same build restored from a cache: another modification time, the same identity.
        let file = std::fs::File::options().write(true).open(&first).unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        assert_eq!(indexer_identity(&first), identity);
        // Another build is another indexer.
        let second = dir.path().join("other");
        std::fs::write(&second, "build 2").unwrap();
        assert_ne!(indexer_identity(&second).1, identity.1);
    }

    #[test]
    fn missing_node_modules_is_an_error_only_when_dependencies_are_declared() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        require_installed(root, root, ".").unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"x"}"#).unwrap();
        require_installed(root, root, ".").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"dependencies":{"lodash":"1"}}"#,
        )
        .unwrap();
        let err = require_installed(root, root, ".").unwrap_err();
        assert!(err.to_string().contains("not installed"));
        assert!(err.to_string().contains("easement doctor"));
        std::fs::create_dir(root.join("node_modules")).unwrap();
        require_installed(root, root, ".").unwrap();
    }

    #[test]
    fn pruning_keeps_the_most_recently_used_entries_of_one_directory_only() {
        let dir = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now();
        let write = |name: String, age: u64| {
            let file = std::fs::File::create(dir.path().join(name)).unwrap();
            file.set_modified(now - std::time::Duration::from_secs(age))
                .unwrap();
        };
        for i in 0..KEPT_PER_DIR + 3 {
            write(format!("aaaa-{i:016x}.scip"), 100 + i as u64);
        }
        write("bbbb-0.scip".into(), 10_000);
        prune(dir.path(), "aaaa-");
        let left = |prefix: &str| {
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                .count()
        };
        assert_eq!(left("aaaa-"), KEPT_PER_DIR);
        assert_eq!(left("bbbb-"), 1);
        // The newest ones stay, the oldest go.
        assert!(dir.path().join(format!("aaaa-{:016x}.scip", 0)).exists());
        assert!(
            !dir.path()
                .join(format!("aaaa-{:016x}.scip", KEPT_PER_DIR + 2))
                .exists()
        );
    }
}
