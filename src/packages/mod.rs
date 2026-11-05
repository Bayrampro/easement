//! The `packages` key: a manifest is a checked file, and each dependency it
//! declares is an edge from it, like an import of a source file. A
//! dev-dependency is test code, so `tests: auto` takes it.

mod cargo;
mod go;
mod npm;

use std::path::{Path, PathBuf};

use crate::check::Unjudged;
use crate::lang::parts::Part;
use crate::lang::registry::{self, Manifest};
use crate::lang::{Target, Unit};
use crate::scan::Dep;

/// Where a declared dependency comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A package of the project, by the path of its directory as written.
    Path(String),
    /// `workspace = true`: what the workspace manifest declares under the name.
    Workspace,
    /// A package by its name: a package of the project that goes by it, else a
    /// third-party one.
    Package,
    /// A registry or a repository: a third-party package.
    Registry,
    /// The manifest itself: an item of a feature that names another feature of it.
    Own,
}

/// One dependency a manifest declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// The line of the manifest that declares it, 1-based.
    pub line: usize,
    /// The name it is declared under.
    pub name: String,
    /// The package it is, which differs from the name for a renamed one.
    pub package: String,
    pub source: Source,
    /// The table it is in, when that is not the plain dependencies.
    pub part: Option<Part>,
    /// The `cfg` of the `[target.<cfg>.dependencies]` table it is in, when it is in one.
    pub target: Option<String>,
    /// The feature of the package it reaches, when it is not the package itself.
    pub feature: Option<String>,
    /// Whether the table declares it `optional = true`: a feature turns it on.
    pub optional: bool,
}

impl Declared {
    /// A dependency of the plain tables, with none of the features of Cargo.
    pub fn new(line: usize, name: &str, package: &str, source: Source) -> Self {
        Self {
            line,
            name: name.to_string(),
            package: package.to_string(),
            source,
            part: None,
            target: None,
            feature: None,
            optional: false,
        }
    }
}

/// The dependencies a manifest of `kind` declares, with the line of each.
/// `None` for an unreadable or unsupported manifest, not a manifest declaring nothing.
pub fn declared(kind: Manifest, text: &str) -> Option<Vec<Declared>> {
    match kind {
        Manifest::Cargo => cargo::declared(text),
        Manifest::Npm => npm::declared(text),
        Manifest::Go => go::declared(text),
        Manifest::Pub => None,
    }
}

/// The dependencies the manifests among `files` declare, as edges.
pub fn edges(root: &Path, files: &[String], read_as_source: impl Fn(&str) -> bool) -> Edges {
    let text_of = |file: &str| std::fs::read_to_string(root.join(file)).ok();
    let mut found = Edges::default();
    let names = Names::of(files, &text_of);
    found.problems.extend(names.duplicates.iter().cloned());
    for file in files {
        let Some(kind) = registry::manifest_of(file) else {
            continue;
        };
        if !read_as_source(file) {
            continue;
        }
        found.manifests.push(file.clone());
        if kind == Manifest::Pub {
            found.problems.push(
                Unjudged::new(format!(
                    "{file}: Pub dependency-declaration edges with packages: true are not yet supported; \
                     Dart source dependency checks and manifests: true remain available"
                ))
                .in_files([file.clone()]),
            );
            continue;
        }
        let Some(text) = text_of(file) else {
            found.problems.push(
                Unjudged::new(format!(
                    "{file}: cannot be read, so what it declares is not checked"
                ))
                .in_files([file.clone()]),
            );
            continue;
        };
        let Some(list) = declared(kind, &text) else {
            found.problems.push(
                Unjudged::new(format!(
                    "{file}: is not valid {}, so what it declares is not checked",
                    kind.file_name()
                ))
                .in_files([file.clone()]),
            );
            continue;
        };
        for declared in list {
            let (resolved, problem) = resolve(file, &declared, files, &text_of, &names);
            found.problems.extend(problem);
            let from = match &declared.part {
                Some(part) => Unit::in_part(Path::new(file), part.clone()),
                None => Unit::from(file.as_str()),
            };
            let (specifier, target) = match (resolved, &declared.feature) {
                (Resolved::Internal(path), feature) => {
                    let unit = match feature.as_deref().filter(|f| *f != "default") {
                        Some(feature) => Unit::in_part(
                            Path::new(&path),
                            Part {
                                name: format!("feature/{feature}"),
                                kind: None,
                            },
                        ),
                        None => Unit::from(path.as_str()),
                    };
                    // A package that names itself (`path = "."`) is no dependency.
                    if unit == from {
                        continue;
                    }
                    (path, Target::Internal(unit))
                }
                // A feature of a third-party package is the package's own business.
                (Resolved::External(_), Some(_)) => continue,
                (Resolved::External(module), None) => (module.clone(), Target::External(module)),
            };
            found.deps.push(Dep {
                from,
                file: PathBuf::from(file),
                line: declared.line,
                specifier,
                target,
            });
        }
    }
    found
}

/// What the manifests of a project declare.
#[derive(Debug, Default)]
pub struct Edges {
    /// The manifests that were read, which are checked files.
    pub manifests: Vec<String>,
    pub deps: Vec<Dep>,
    /// Why a manifest, or a dependency of one, was not judged as it declares.
    pub problems: Vec<Unjudged>,
}

/// What a declared dependency is, once resolved against the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The manifest of a package of the project, relative to the root.
    Internal(String),
    /// A third-party package, in the spelling of an import.
    External(String),
}

/// The packages of the project by the name they are known by, for the kinds of manifest
/// that name them: `package.json` by `name`, `go.mod` by `module`.
#[derive(Debug, Default)]
pub struct Names {
    by_name: std::collections::BTreeMap<(Manifest, String), String>,
    /// Names two manifests of a kind declare.
    pub duplicates: Vec<Unjudged>,
}

impl Names {
    pub fn of(files: &[String], read: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut names = Self::default();
        let mut sorted: Vec<&String> = files.iter().collect();
        sorted.sort();
        for file in sorted {
            // Tools skip these directories, and so does the name of a package.
            if file
                .split('/')
                .any(|dir| dir == "vendor" || dir == "testdata")
            {
                continue;
            }
            let Some(kind) = registry::manifest_of(file).filter(|k| *k != Manifest::Cargo) else {
                continue;
            };
            let Some(text) = read(file) else {
                continue;
            };
            let name = match kind {
                Manifest::Npm => npm::name(&text),
                Manifest::Go => go::name(&text),
                Manifest::Cargo | Manifest::Pub => None,
            };
            let Some(name) = name else {
                continue;
            };
            match names.by_name.get(&(kind, name.clone())) {
                Some(first) => names.duplicates.push(
                    Unjudged::new(format!(
                        "two `{}` go by `{name}`, {first} and {file}: a dependency on it is taken for the first",
                        kind.file_name()
                    ))
                    .in_files([first.clone(), file.clone()]),
                ),
                None => {
                    names.by_name.insert((kind, name), file.clone());
                }
            }
        }
        names
    }

    fn get(&self, kind: Manifest, name: &str) -> Option<&String> {
        self.by_name.get(&(kind, name.to_string()))
    }
}

/// Resolves what the manifest at `manifest` declares as `declared`. `files` are
/// the project's files; `read` gives the text of one of them.
pub fn resolve(
    manifest: &str,
    declared: &Declared,
    files: &[String],
    read: &dyn Fn(&str) -> Option<String>,
    names: &Names,
) -> (Resolved, Option<Unjudged>) {
    let Some(kind) = registry::manifest_of(manifest) else {
        return (Resolved::External(declared.package.clone()), None);
    };
    let internal = |dir: &str, path: &str| {
        let target = join(dir, path).map(|dir| manifest_in(kind, &dir))?;
        files.contains(&target).then_some(target)
    };
    let dir = parent(manifest);
    let mut package = declared.package.clone();
    let mut problem = None;
    let found = match &declared.source {
        Source::Path(path) => internal(dir, path),
        Source::Package => names
            .get(kind, &declared.package)
            .filter(|file| file.as_str() != manifest)
            .cloned(),
        Source::Workspace => match cargo::workspace_dependency(dir, &declared.name, read) {
            Ok(Some(entry)) => {
                if let Some(real) = entry.package {
                    package = real;
                }
                entry.path.and_then(|path| internal(&entry.dir, &path))
            }
            Ok(None) => None,
            Err(workspace) => {
                problem = Some(
                    Unjudged::new(format!(
                        "{manifest}: `{}` is `workspace = true`, but {workspace} cannot be read, so it is taken for an external crate",
                        declared.name
                    ))
                    .in_files([manifest.to_string()]),
                );
                None
            }
        },
        Source::Own => Some(manifest.to_string()),
        Source::Registry => None,
    };
    let resolved = found.map_or_else(
        || {
            Resolved::External(match kind {
                // Rust spells a crate with `_` in an import.
                Manifest::Cargo => package.replace('-', "_"),
                Manifest::Npm | Manifest::Go | Manifest::Pub => package,
            })
        },
        Resolved::Internal,
    );
    (resolved, problem)
}

/// The directory a manifest is in, `""` for the root.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The manifest of `kind` of the package in `dir`.
pub(super) fn manifest_in(kind: Manifest, dir: &str) -> String {
    let name = kind.file_name();
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `path` written in the manifest of `dir`, relative to the root; `None` if it leaves the project.
fn join(dir: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::parts::Kind;

    fn found(text: &str) -> Vec<(usize, String, String, Source, Option<String>)> {
        declared(Manifest::Cargo, text)
            .unwrap_or_default()
            .into_iter()
            .map(|d| {
                (
                    d.line,
                    d.name,
                    d.package,
                    d.source,
                    d.part.map(|part| part.name),
                )
            })
            .collect()
    }

    #[test]
    fn pub_manifests_disclose_unsupported_package_edges() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("pubspec.yaml"),
            "name: app\ndependencies:\n  http: ^1.0.0\n",
        )
        .unwrap();
        let found = edges(directory.path(), &["pubspec.yaml".into()], |_| true);
        assert!(found.deps.is_empty());
        assert_eq!(found.manifests, ["pubspec.yaml"]);
        assert_eq!(found.problems.len(), 1);
        assert!(found.problems[0].reason.contains("not yet supported"));
    }

    #[test]
    fn a_cargo_manifest_declares_a_dependency_on_the_line_of_its_name() {
        let text = concat!(
            "[package]\n",                                            // 1
            "name = \"chunk-store\"\n",                               // 2
            "\n",                                                     // 3
            "[dependencies]\n",                                       // 4
            "storage-api = { workspace = true }\n",                   // 5
            "serde_json = \"1\"\n",                                   // 6
            "local = { path = \"../local\" }\n",                      // 7
            "json = { package = \"serde_json\", version = \"1\" }\n", // 8
            "\n",                                                     // 9
            "[dev-dependencies]\n",                                   // 10
            "testkit = { workspace = true }\n",                       // 11
            "\n",                                                     // 12
            "[build-dependencies]\n",                                 // 13
            "cc = \"1\"\n",                                           // 14
            "\n",                                                     // 15
            "[target.'cfg(unix)'.dependencies]\n",                    // 16
            "libc = \"0.2\"\n",                                       // 17
            "\n",                                                     // 18
            "[dependencies.regex]\n",                                 // 19
            "version = \"1\"\n",                                      // 20
        );
        let s = |x: &str| x.to_string();
        assert_eq!(
            found(text),
            [
                (
                    5,
                    s("storage-api"),
                    s("storage-api"),
                    Source::Workspace,
                    None
                ),
                (6, s("serde_json"), s("serde_json"), Source::Registry, None),
                (7, s("local"), s("local"), Source::Path(s("../local")), None),
                (8, s("json"), s("serde_json"), Source::Registry, None),
                (
                    11,
                    s("testkit"),
                    s("testkit"),
                    Source::Workspace,
                    Some(s("dev-dependencies"))
                ),
                (
                    14,
                    s("cc"),
                    s("cc"),
                    Source::Registry,
                    Some(s("build-dependencies"))
                ),
                (17, s("libc"), s("libc"), Source::Registry, None),
                (19, s("regex"), s("regex"), Source::Registry, None),
            ]
        );
        let kinds: Vec<_> = declared(Manifest::Cargo, text)
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.part.and_then(|part| part.kind))
            .collect();
        assert_eq!(kinds[4], Some(Kind::Test));
        assert_eq!(kinds[5], None);
    }

    #[test]
    fn a_manifest_that_is_no_toml_is_unread_not_empty() {
        assert_eq!(declared(Manifest::Cargo, "[dependencies\nx = "), None);
        assert_eq!(
            declared(Manifest::Cargo, "[package]\nname = \"a\"\n"),
            Some(Vec::new())
        );
    }

    #[test]
    fn the_same_name_in_two_tables_is_two_dependencies() {
        let text =
            "[dependencies]\nlibc = \"0.2\"\n\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n";
        let lines: Vec<_> = found(text).iter().map(|d| d.0).collect();
        assert_eq!(lines, [2, 5]);
    }

    #[test]
    fn the_lines_of_a_multi_line_table_are_no_dependencies() {
        let text = "[dependencies]\nfoo = {\n    version = \"1\",\n    path = \"../foo\",\n}\nversion = \"2\"\n";
        let names: Vec<_> = found(text).iter().map(|d| (d.0, d.1.clone())).collect();
        assert_eq!(names, [(2, "foo".to_string()), (6, "version".to_string())]);
    }

    fn dep(name: &str, source: Source) -> Declared {
        Declared {
            line: 1,
            name: name.to_string(),
            package: name.to_string(),
            source,
            part: None,
            target: None,
            feature: None,
            optional: false,
        }
    }

    fn project() -> (Vec<String>, impl Fn(&str) -> Option<String>) {
        let manifests: Vec<(&str, &str)> = vec![
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nstorage-api = { path = \"crates/storage-api\" }\nserde = \"1\"\n",
            ),
            (
                "crates/storage-api/Cargo.toml",
                "[package]\nname = \"storage-api\"\n",
            ),
            (
                "crates/chunk-store/Cargo.toml",
                "[package]\nname = \"chunk-store\"\n",
            ),
            ("crates/local/Cargo.toml", "[package]\nname = \"local\"\n"),
        ];
        let files = manifests.iter().map(|(p, _)| p.to_string()).collect();
        let text = move |path: &str| {
            manifests
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, t)| t.to_string())
        };
        (files, text)
    }

    #[test]
    fn a_path_dependency_is_the_manifest_of_a_package_of_the_project() {
        let (files, read) = project();
        let got = resolve(
            "crates/chunk-store/Cargo.toml",
            &dep("local", Source::Path("../local".into())),
            &files,
            &read,
            &Names::default(),
        )
        .0;
        assert_eq!(got, Resolved::Internal("crates/local/Cargo.toml".into()));
        // A path with no manifest of the project there names nothing of it.
        let missing = resolve(
            "crates/chunk-store/Cargo.toml",
            &dep("gone", Source::Path("../gone".into())),
            &files,
            &read,
            &Names::default(),
        )
        .0;
        assert_eq!(missing, Resolved::External("gone".into()));
    }

    #[test]
    fn a_workspace_dependency_is_what_the_workspace_manifest_declares() {
        let (files, read) = project();
        let from = "crates/chunk-store/Cargo.toml";
        assert_eq!(
            resolve(
                from,
                &dep("storage-api", Source::Workspace),
                &files,
                &read,
                &Names::default()
            )
            .0,
            Resolved::Internal("crates/storage-api/Cargo.toml".into())
        );
        assert_eq!(
            resolve(
                from,
                &dep("serde", Source::Workspace),
                &files,
                &read,
                &Names::default()
            )
            .0,
            Resolved::External("serde".into())
        );
    }

    #[test]
    fn a_renamed_workspace_dependency_is_spelled_as_its_package() {
        let manifests = [(
            "Cargo.toml",
            "[workspace]\n\n[workspace.dependencies]\nhttp = { package = \"real-http\", version = \"1\" }\n",
        )];
        let read = |path: &str| {
            manifests
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, t)| t.to_string())
        };
        let files = vec!["Cargo.toml".to_string(), "crates/a/Cargo.toml".to_string()];
        let (got, problem) = resolve(
            "crates/a/Cargo.toml",
            &dep("http", Source::Workspace),
            &files,
            &read,
            &Names::default(),
        );
        assert_eq!(got, Resolved::External("real_http".into()));
        assert_eq!(problem, None);
    }

    #[test]
    fn a_workspace_manifest_that_cannot_be_read_is_said() {
        let read = |path: &str| (path == "Cargo.toml").then(|| "[workspace\n".to_string());
        let files = vec!["Cargo.toml".to_string(), "crates/a/Cargo.toml".to_string()];
        let (got, problem) = resolve(
            "crates/a/Cargo.toml",
            &dep("api", Source::Workspace),
            &files,
            &read,
            &Names::default(),
        );
        assert_eq!(got, Resolved::External("api".into()));
        let problem = problem.unwrap();
        assert!(problem.reason.contains("Cargo.toml"), "{problem:?}");
    }

    #[test]
    fn a_third_party_package_is_spelled_as_an_import_spells_it() {
        let (files, read) = project();
        let mut renamed = dep("json", Source::Registry);
        renamed.package = "serde-json".into();
        assert_eq!(
            resolve(
                "crates/local/Cargo.toml",
                &renamed,
                &files,
                &read,
                &Names::default()
            )
            .0,
            Resolved::External("serde_json".into())
        );
    }

    /// (line, name, the part it leaves, the feature it reaches, where it comes from)
    type Row = (usize, String, Option<String>, Option<String>, Source);

    fn edges_of(text: &str) -> Vec<Row> {
        declared(Manifest::Cargo, text)
            .unwrap()
            .into_iter()
            .map(|d| (d.line, d.name, d.part.map(|p| p.name), d.feature, d.source))
            .collect()
    }

    #[test]
    fn features_declare_what_they_turn_on_and_an_optional_dependency_belongs_to_it() {
        let text = concat!(
            "[features]\n",                                                 // 1
            "default = [\"fake\"]\n",                                       // 2
            "fake = [\"storage-api/fake\", \"dep:serde\", \"other\"]\n",    // 3
            "other = [\"tls?/rustls\"]\n",                                  // 4
            "\n",                                                           // 5
            "[dependencies]\n",                                             // 6
            "storage-api = { workspace = true, features = [\"extra\"] }\n", // 7
            "serde = { version = \"1\", optional = true }\n",               // 8
            "tls = { version = \"1\", optional = true }\n",                 // 9
            "gzip = { version = \"1\", optional = true }\n",                // 10
        );
        let s = |x: &str| Some(x.to_string());
        let n = |x: &str| x.to_string();
        assert_eq!(
            edges_of(text),
            [
                // `default` is the package itself.
                (2, n("fake"), None, s("fake"), Source::Own),
                (
                    3,
                    n("storage-api"),
                    s("feature/fake"),
                    None,
                    Source::Workspace
                ),
                (
                    3,
                    n("storage-api"),
                    s("feature/fake"),
                    s("fake"),
                    Source::Workspace
                ),
                (3, n("serde"), s("feature/fake"), None, Source::Registry),
                (3, n("other"), s("feature/fake"), s("other"), Source::Own),
                // A weak item reaches the feature of `tls`, not `tls`.
                (
                    4,
                    n("tls"),
                    s("feature/other"),
                    s("rustls"),
                    Source::Registry
                ),
                (7, n("storage-api"), None, None, Source::Workspace),
                (7, n("storage-api"), None, s("extra"), Source::Workspace),
                // `serde` is named by `dep:serde`, so it has no feature of its own;
                // `tls` and `gzip` have the implicit one.
                (9, n("tls"), s("feature/tls"), None, Source::Registry),
                (10, n("gzip"), s("feature/gzip"), None, Source::Registry),
            ]
        );
    }

    /// (line, the part it leaves, what it reaches and the part of it) for each edge.
    fn edge_list(files: &[(&str, &str)]) -> Vec<(usize, Option<String>, String, Option<String>)> {
        let dir = tempfile::TempDir::new().unwrap();
        let mut names = Vec::new();
        for (path, text) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
            names.push(path.to_string());
        }
        let found = edges(dir.path(), &names, |_| true);
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        found
            .deps
            .into_iter()
            .filter(|dep| dep.file.to_str() == Some("crates/telegram/Cargo.toml"))
            .map(|dep| {
                let (key, part) = match &dep.target {
                    Target::Internal(unit) => (unit.key(), unit.part_name().map(str::to_string)),
                    Target::External(module) => (module.clone(), None),
                };
                (
                    dep.line,
                    dep.from.part_name().map(str::to_string),
                    key,
                    part,
                )
            })
            .collect()
    }

    #[test]
    fn a_feature_item_reaches_the_feature_of_another_manifest_of_the_project() {
        let s = |x: &str| Some(x.to_string());
        let n = |x: &str| x.to_string();
        let got = edge_list(&[
            (
                "Cargo.toml",
                "[workspace]\n\n[workspace.dependencies]\nstorage-api = { path = \"crates/storage-api\" }\n",
            ),
            (
                "crates/storage-api/Cargo.toml",
                "[package]\nname = \"storage-api\"\n",
            ),
            (
                "crates/telegram/Cargo.toml",
                concat!(
                    "[features]\n",                                      // 1
                    "default = [\"fake\"]\n",                            // 2
                    "fake = [\"storage-api/fake\", \"serde/derive\"]\n", // 3
                    "\n",                                                // 4
                    "[dependencies]\n",                                  // 5
                    "storage-api = { workspace = true }\n",              // 6
                    "serde = \"1\"\n",                                   // 7
                ),
            ),
        ]);
        let kit = n("crates/storage-api/Cargo.toml");
        let own = n("crates/telegram/Cargo.toml");
        assert_eq!(
            got,
            [
                // `default` is the package itself, and `fake` a feature of the manifest.
                (2, None, own, s("feature/fake")),
                (3, s("feature/fake"), kit.clone(), None),
                (3, s("feature/fake"), kit.clone(), s("feature/fake")),
                // An external package has no part of the project to reach.
                (3, s("feature/fake"), n("serde"), None),
                (6, None, kit, None),
                (7, None, n("serde"), None),
            ]
        );
    }

    #[test]
    fn default_as_a_target_is_the_package_itself() {
        let own = "crates/telegram/Cargo.toml".to_string();
        let got = edge_list(&[(
            "crates/telegram/Cargo.toml",
            "[features]\ndefault = []\nfull = [\"default\"]\n",
        )]);
        assert_eq!(got, [(3, Some("feature/full".to_string()), own, None)]);
    }

    #[test]
    fn a_literal_string_is_an_item_too() {
        let rows = edges_of(
            "[features]\nfake = ['dep:serde']\n\n[dependencies]\nserde = { version = \"1\", optional = true }\n",
        );
        assert_eq!(
            rows,
            [(
                2,
                "serde".to_string(),
                Some("feature/fake".to_string()),
                None,
                Source::Registry
            )]
        );
    }

    #[test]
    fn an_item_takes_what_the_dependencies_declare_not_the_dev_ones() {
        let text = concat!(
            "[dev-dependencies]\n",
            "x = { path = \"../x\" }\n",
            "\n",
            "[dependencies]\n",
            "x = \"1\"\n",
            "\n",
            "[features]\n",
            "f = [\"x/g\"]\n",
        );
        let items: Vec<_> = edges_of(text).into_iter().filter(|r| r.0 == 8).collect();
        let f = Some("feature/f".to_string());
        assert_eq!(
            items,
            [
                (8, "x".to_string(), f.clone(), None, Source::Registry),
                (
                    8,
                    "x".to_string(),
                    f,
                    Some("g".to_string()),
                    Source::Registry
                ),
            ]
        );
    }

    #[test]
    fn an_item_that_names_no_dependency_is_no_edge() {
        let rows = edges_of("[features]\nf = [\"typo/g\", \"dep:typo\"]\n");
        assert!(rows.is_empty(), "{rows:?}");
    }

    /// Resolves each dependency a manifest of a project declares.
    fn resolved(
        kind: Manifest,
        manifest: &str,
        project: &[(&str, &str)],
    ) -> Vec<(String, Resolved)> {
        let files: Vec<String> = project.iter().map(|(p, _)| p.to_string()).collect();
        let read = |path: &str| {
            project
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, t)| t.to_string())
        };
        let names = Names::of(&files, &read);
        let text = read(manifest).unwrap();
        declared(kind, &text)
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d.name.clone(),
                    resolve(manifest, d, &files, &read, &names).0,
                )
            })
            .collect()
    }

    #[test]
    fn an_npm_dependency_is_the_package_json_that_goes_by_its_name_or_its_path() {
        let got = resolved(
            Manifest::Npm,
            "packages/web/package.json",
            &[
                ("packages/api/package.json", "{\"name\": \"@acme/api\"}"),
                ("packages/local/package.json", "{\"name\": \"local\"}"),
                (
                    "packages/web/package.json",
                    "{\"name\": \"web\", \"dependencies\": {\"@acme/api\": \"workspace:*\", \"lodash\": \"^4\", \"loc\": \"file:../local\"}}",
                ),
            ],
        );
        let internal = |p: &str| Resolved::Internal(p.to_string());
        assert_eq!(
            got,
            [
                (
                    "@acme/api".to_string(),
                    internal("packages/api/package.json")
                ),
                (
                    "lodash".to_string(),
                    Resolved::External("lodash".to_string())
                ),
                ("loc".to_string(), internal("packages/local/package.json")),
            ]
        );
    }

    #[test]
    fn a_go_module_is_the_go_mod_that_declares_its_path_or_replaces_it() {
        let got = resolved(
            Manifest::Go,
            "app/go.mod",
            &[
                (
                    "app/go.mod",
                    "module example.com/app\n\nrequire (\n\texample.com/lib v1.0.0\n\tgithub.com/lib/pq v1.10.0\n\texample.com/moved v1.0.0\n)\n\nreplace example.com/moved => ../moved\n",
                ),
                ("lib/go.mod", "module example.com/lib\n"),
                ("moved/go.mod", "module example.com/moved\n"),
            ],
        );
        let internal = |p: &str| Resolved::Internal(p.to_string());
        assert_eq!(
            got,
            [
                ("example.com/lib".to_string(), internal("lib/go.mod")),
                (
                    "github.com/lib/pq".to_string(),
                    Resolved::External("github.com/lib/pq".to_string())
                ),
                ("example.com/moved".to_string(), internal("moved/go.mod")),
            ]
        );
    }

    #[test]
    fn two_manifests_that_go_by_one_name_are_said() {
        let project = [
            ("a/package.json", "{\"name\": \"dup\"}"),
            ("b/package.json", "{\"name\": \"dup\"}"),
        ];
        let files: Vec<String> = project.iter().map(|(p, _)| p.to_string()).collect();
        let read = |path: &str| {
            project
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, t)| t.to_string())
        };
        let names = Names::of(&files, &read);
        assert_eq!(names.duplicates.len(), 1);
        assert!(names.duplicates[0].reason.contains("dup"));
        assert_eq!(
            names.duplicates[0].files,
            ["a/package.json", "b/package.json"]
        );
    }

    #[test]
    fn a_vendored_or_test_data_manifest_does_not_count_as_a_package_of_the_project() {
        let project = [
            ("a/go.mod", "module example.com/a\n"),
            ("vendor/x/go.mod", "module example.com/a\n"),
            ("pkg/testdata/go.mod", "module example.com/a\n"),
        ];
        let files: Vec<String> = project.iter().map(|(p, _)| p.to_string()).collect();
        let read = |path: &str| {
            project
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, t)| t.to_string())
        };
        let names = Names::of(&files, &read);
        assert!(names.duplicates.is_empty(), "{:?}", names.duplicates);
    }
}
