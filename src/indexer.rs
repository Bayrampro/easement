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

use crate::scip::{self, Document, Documents};

/// A language whose dependencies come from a SCIP index.
pub struct Language {
    pub name: &'static str,
    binary: &'static str,
    /// Files that mark a directory the indexer runs in.
    markers: &'static [&'static str],
    /// Run only in the outermost marked directories: a Cargo workspace holds its members.
    outermost: bool,
    /// Run at the project root when no marker is found (Python projects often have none).
    fallback_root: bool,
    /// Extensions of the source files the indexer covers.
    extensions: &'static [&'static str],
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
    /// Which project files under the index directory the cache key reads (the `/`-separated
    /// path): what the indexer could see. Another language's sources, docs and assets are left
    /// out, so a change to them reuses the index. When in doubt, a file is in.
    inputs: fn(&str) -> bool,
    /// Which inherited environment variables change what the indexer writes
    /// (`GOFLAGS`, `PYTHONPATH`); their values are in the cache key.
    env: fn(&str) -> bool,
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

fn nothing_needed(_: &Path, _: &Path, _: &str) -> Result<()> {
    Ok(())
}

/// Every file: for an indexer whose build reads files of its own choosing (Maven, Gradle,
/// Cargo, MSBuild, Bundler, Composer, Python's environment).
fn every_file(_: &str) -> bool {
    true
}

/// The file name, the last part of a `/`-separated path.
fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// scip-go reads the Go sources and what `go` resolves them with: the module and workspace
/// files and a vendored dependency list.
fn go_inputs(path: &str) -> bool {
    let name = file_name(path);
    name.ends_with(".go")
        || matches!(
            name,
            "go.mod" | "go.sum" | "go.work" | "go.work.sum" | "modules.txt"
        )
}

const TYPESCRIPT_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// scip-typescript reads what the TypeScript compiler does: the sources and declarations, every
/// JSON file (a `tsconfig.json` and what it extends, a `package.json`, a JSON module an import
/// names) and the lock files that decide which dependency is installed.
fn typescript_inputs(path: &str) -> bool {
    let name = file_name(path);
    let source = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| TYPESCRIPT_EXTENSIONS.contains(&ext));
    source
        || name.ends_with(".json")
        || name.ends_with(".json5")
        || matches!(
            name,
            "pnpm-lock.yaml" | "pnpm-workspace.yaml" | "yarn.lock" | "bun.lock" | "bun.lockb"
        )
}

pub const LANGUAGES: &[Language] = &[
    Language {
        name: "go",
        binary: "scip-go",
        markers: &["go.mod"],
        outermost: false,
        fallback_root: false,
        extensions: &["go"],
        args: |out| vec!["--output".into(), out.display().to_string()],
        install: "mise use -g go:github.com/scip-code/scip-go/cmd/scip-go",
        mise: Some("go:github.com/scip-code/scip-go/cmd/scip-go"),
        skip_dirs: &[],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: go_inputs,
        env: go_env,
    },
    Language {
        name: "typescript",
        binary: "scip-typescript",
        markers: &["tsconfig.json"],
        outermost: false,
        fallback_root: false,
        extensions: TYPESCRIPT_EXTENSIONS,
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
        inputs: typescript_inputs,
        env: |_| false,
    },
    Language {
        name: "python",
        binary: "scip-python",
        markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        outermost: false,
        fallback_root: true,
        extensions: &["py", "pyi"],
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
        inputs: every_file,
        env: |name| starts_with_any(name, &["PYTHON", "VIRTUAL_ENV", "CONDA_PREFIX"]),
    },
    Language {
        name: "php",
        binary: "scip-php",
        markers: &["composer.json"],
        outermost: false,
        fallback_root: false,
        extensions: &["php"],
        args: |_| Vec::new(),
        install: "composer global require davidrjenni/scip-php",
        mise: None,
        skip_dirs: &["vendor"],
        writes_in_cwd: Some("index.scip"),
        needs: require_composer,
        inputs: every_file,
        env: |_| false,
    },
    Language {
        name: "java",
        binary: "scip-java",
        markers: &["pom.xml", "build.gradle", "build.gradle.kts"],
        outermost: true,
        fallback_root: false,
        extensions: &["java"],
        args: |out| vec!["index".into(), "--output".into(), out.display().to_string()],
        install: "mise use -g github:sourcegraph/scip-java",
        mise: Some("github:sourcegraph/scip-java"),
        skip_dirs: &["target", "build"],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: every_file,
        env: |name| starts_with_any(name, &["JAVA_", "JDK_JAVA_OPTIONS", "MAVEN_"]),
    },
    Language {
        name: "csharp",
        binary: "scip-dotnet",
        markers: &["*.csproj", "*.sln", "*.slnx"],
        outermost: true,
        fallback_root: false,
        extensions: &["cs"],
        args: |out| vec!["index".into(), "--output".into(), out.display().to_string()],
        install: "dotnet tool install --global scip-dotnet",
        mise: None,
        skip_dirs: &["bin", "obj"],
        writes_in_cwd: None,
        needs: nothing_needed,
        inputs: every_file,
        env: |name| starts_with_any(name, &["DOTNET_", "MSBUILD", "NUGET_"]),
    },
    Language {
        name: "ruby",
        binary: "scip-ruby",
        markers: &["Gemfile", "*.gemspec"],
        outermost: true,
        fallback_root: false,
        extensions: &["rb"],
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
        inputs: every_file,
        env: |name| starts_with_any(name, &["BUNDLE_", "GEM_"]),
    },
    Language {
        name: "rust",
        binary: "rust-analyzer",
        markers: &["Cargo.toml"],
        outermost: true,
        fallback_root: false,
        extensions: &["rs"],
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
        inputs: every_file,
        env: |name| starts_with_any(name, &["RUSTFLAGS", "RUSTC", "CARGO_"]),
    },
];

impl Language {
    pub fn covers(&self, file: &str) -> bool {
        file.rsplit_once('.')
            .is_some_and(|(_, ext)| self.extensions.contains(&ext))
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

    fn roots(&self, files: &[String]) -> Vec<String> {
        let mut dirs: Vec<String> = files
            .iter()
            .filter_map(|f| {
                let (dir, name) = f.rsplit_once('/').unwrap_or(("", f));
                self.has_marker(name).then(|| dir.to_string())
            })
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

/// What the indexers found, and the source files they did not cover.
pub struct Indexed {
    pub documents: Documents,
    /// Indexers that ran, and indexes read from the cache instead.
    pub ran: usize,
    pub cached: usize,
    /// Source files of an indexed language that no index mentions.
    pub uncovered: Vec<String>,
}

/// Indexes every language that has source files among `files` (project files,
/// `/`-separated, relative to `root`).
/// What an indexer is run with besides its arguments: the build tags of Go.
fn environment(language: &Language, go_tags: &[String]) -> Vec<(&'static str, String)> {
    if language.name != "go" || go_tags.is_empty() {
        return Vec::new();
    }
    let mut flags = std::env::var("GOFLAGS").unwrap_or_default();
    if !flags.is_empty() {
        flags.push(' ');
    }
    flags.push_str(&format!("-tags={}", go_tags.join(",")));
    vec![("GOFLAGS", flags)]
}

/// `excluded` are files that `exclude` hides: they are not indexed for
/// themselves, but the index resolves imports of them, so they are part of
/// the cache key.
pub fn run(
    root: &Path,
    files: &[String],
    excluded: &[String],
    cache: CacheUse,
    jobs: Jobs,
    go_tags: &[String],
) -> Result<Indexed> {
    let (mut ran, mut cached) = (0, 0);
    let keyed: Vec<String> = files.iter().chain(excluded).cloned().collect();
    let mut wanted: Vec<&String> = Vec::new();
    let mut groups = Vec::new();
    for language in LANGUAGES {
        wanted.extend(files.iter().filter(|f| language.covers(f)));
        if !files.iter().any(|f| language.covers(f)) {
            continue;
        }
        // Source files with no `go.mod` or `tsconfig.json` above them are not
        // indexed; they are reported as uncovered, or can be `exclude`d.
        let roots = language.roots(files);
        if !roots.is_empty() {
            groups.push((language, roots));
        }
    }
    let results = in_parallel(&groups, jobs.get(), |(language, roots)| {
        roots
            .iter()
            .map(|dir| index_dir(language, root, dir, &keyed, cache, go_tags))
            .collect::<Result<Vec<_>>>()
    })?;
    let mut parts = Vec::new();
    for (documents, from_cache) in results.into_iter().flatten() {
        parts.push(documents);
        if from_cache {
            cached += 1;
        } else {
            ran += 1;
        }
    }
    let documents = if parts.is_empty() {
        Documents::default()
    } else {
        Documents::merged(parts)?
    };
    let indexed: BTreeSet<&str> = documents.0.iter().map(|d| d.path.as_str()).collect();
    let uncovered = wanted
        .into_iter()
        .filter(|f| !indexed.contains(f.as_str()))
        .cloned()
        .collect();
    Ok(Indexed {
        documents,
        ran,
        cached,
        uncovered,
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

/// Everything that decides what an indexer would write: the indexer itself, its
/// environment ([`key_env`]) and
/// the name and content of every project file under `dir` that the indexer reads
/// (`Language::inputs`, all but build output), plus the files that sit directly in the directories above `dir`
/// (`tsconfig.base.json`, `go.work`, a root `package.json`), which the project
/// files extend. Nothing about the checkout (its path, the modification times)
/// is in the key, so a worktree of the same code reuses the index. Dependencies
/// outside the project are not covered; `--no-cache` is for when they change
/// without a lock file.
fn fingerprint(
    language: &Language,
    binary: &Path,
    root: &Path,
    dir: &str,
    files: &[String],
    env: &[(String, String)],
) -> u64 {
    let mut hasher = StableHasher::default();
    language.name.hash(&mut hasher);
    env.hash(&mut hasher);
    (language.args)(Path::new("")).hash(&mut hasher);
    indexer_identity(binary).hash(&mut hasher);
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let above = |f: &str| {
        let parent = f.rsplit_once('/').map_or("", |(parent, _)| parent);
        parent.len() < dir.len() && (parent.is_empty() || dir.starts_with(&format!("{parent}/")))
    };
    let mut seen: Vec<(&str, Option<u64>)> = files
        .iter()
        .filter(|f| {
            (f.starts_with(&prefix) && !language.in_skip_dir(f) && (language.inputs)(f)) || above(f)
        })
        .map(|f| {
            // An unreadable file is a different state from every readable one.
            let content = std::fs::read(root.join(f))
                .ok()
                .map(|bytes| content_hash(&bytes));
            (f.as_str(), content)
        })
        .collect();
    seen.sort();
    seen.hash(&mut hasher);
    hasher.finish()
}

/// The environment an indexer runs with, as far as it changes the index: the
/// inherited variables that `Language::env` names, with what easement sets for
/// the run over them.
fn key_env(
    language: &Language,
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
    static CONTENT: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Option<u64>>>,
    > = std::sync::OnceLock::new();
    let real = std::fs::canonicalize(binary).ok();
    let path = real.clone().unwrap_or_else(|| binary.to_path_buf());
    let hash = |path: &Path| std::fs::read(path).ok().map(|bytes| content_hash(&bytes));
    let content = CONTENT
        .get_or_init(Default::default)
        .lock()
        .map(|mut seen| *seen.entry(path.clone()).or_insert_with(|| hash(&path)))
        .unwrap_or_else(|_| hash(&path));
    let modified = if content.is_some() {
        None
    } else {
        std::fs::metadata(binary).and_then(|m| m.modified()).ok()
    };
    (real, content, modified)
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
fn cache_file(language: &Language, dir: &str, key: u64) -> Option<(PathBuf, String)> {
    let mut hasher = StableHasher::default();
    (language.name, dir).hash(&mut hasher);
    let prefix = format!("{:016x}-", hasher.finish());
    Some((
        cache_dir()?.join(format!("{prefix}{key:016x}.scip")),
        prefix,
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
    LANGUAGES
        .iter()
        .filter(|language| files.iter().any(|f| language.covers(f)))
        .map(|language| {
            let binary = which::which(language.binary).ok();
            let version = binary.as_deref().and_then(version_of);
            let problems = language
                .roots(files)
                .iter()
                .filter_map(|dir| {
                    let shown = if dir.is_empty() { "." } else { dir };
                    (language.needs)(root, &root.join(dir), shown)
                        .err()
                        .map(|e| e.to_string())
                })
                .collect();
            Diagnosis {
                language: language.name,
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
fn version_of(binary: &Path) -> Option<String> {
    let output = Command::new(binary).arg("--version").output().ok()?;
    [output.stdout, output.stderr].iter().find_map(|bytes| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    })
}

fn index_dir(
    language: &Language,
    root: &Path,
    dir: &str,
    files: &[String],
    cache: CacheUse,
    go_tags: &[String],
) -> Result<(Vec<Document>, bool)> {
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
            language.name,
            language.install
        )
    })?;
    let key = fingerprint(
        language,
        &binary,
        root,
        dir,
        files,
        &key_env(language, std::env::vars_os(), &env),
    );
    let cached = (cache != CacheUse::Off)
        .then(|| cache_file(language, dir, key))
        .flatten();
    if let Some((path, _)) = &cached
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(documents) = scip::parse(&bytes, Path::new(dir))
        && !documents.is_empty()
    {
        // Recency decides what pruning keeps: best effort, a failure only ages the entry.
        if let Ok(file) = std::fs::File::options().write(true).open(path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        return Ok((documents, true));
    }
    if cache == CacheUse::Only {
        return Err(NotCached {
            language: language.name,
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
    let output = Command::new(binary)
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
    let documents = scip::parse(&bytes, Path::new(dir))
        .with_context(|| format!("cannot read the index of {} in {shown}", language.binary))?;
    if documents.is_empty() {
        bail!(
            "{} indexed no files in {shown}; check what its project file includes",
            language.binary
        );
    }
    if let Some((path, prefix)) = cached
        && let Some(parent) = path.parent()
    {
        // Best effort: a cache that cannot be written only costs the next run its speed.
        let _ = std::fs::create_dir_all(parent);
        // Written to a temporary file first, so that a run in another checkout never
        // reads half an index: a cut at a message boundary would parse as fewer files.
        let _ = tempfile::NamedTempFile::new_in(parent).and_then(|mut file| {
            std::io::Write::write_all(&mut file, &bytes)?;
            file.persist(&path).map_err(|e| e.error)
        });
        prune(parent, &prefix);
    }
    Ok((documents, false))
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
    fn the_cache_key_reads_the_environment_the_indexer_inherits() {
        let go = &LANGUAGES[0];
        let ts = &LANGUAGES[1];
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
    fn cache_keys_hash_with_blake3_which_no_build_of_easement_changes() {
        // The BLAKE3 test vector: the hash of `abc` begins 64 37 b3 ac 38 46 51 33.
        assert_eq!(content_hash(b"abc"), 0x3351_4638_acb3_3764);
        let mut hasher = StableHasher::default();
        hasher.write(b"abc");
        assert_eq!(hasher.finish(), 0x3351_4638_acb3_3764);
    }

    #[test]
    fn languages_cover_their_sources_and_roots() {
        let ts = &LANGUAGES[1];
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
        assert_eq!(LANGUAGES[0].roots(&files), ["", "svc"]);
        assert_eq!(ts.roots(&files), ["web"]);
        // A workspace root holds its members; Python falls back to the root.
        assert_eq!(LANGUAGES[7].roots(&files), [""]);
        assert_eq!(LANGUAGES[2].roots(&files), [""]);
        let nested: Vec<String> = ["a/Cargo.toml", "b/Cargo.toml", "b/c/Cargo.toml"]
            .map(String::from)
            .to_vec();
        assert_eq!(LANGUAGES[7].roots(&nested), ["a", "b"]);
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
