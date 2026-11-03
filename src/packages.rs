//! The `packages` key: a manifest is a checked file, and each dependency it
//! declares is an edge from it, like an import of a source file. A
//! dev-dependency is test code, so `tests: auto` takes it.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::lang::parts::{Kind, Part};
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
    /// A registry or a repository: a third-party package.
    Registry,
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
}

/// The dependencies a manifest of `kind` declares, with the line of each.
/// `None` for a manifest that cannot be read, which is not one that declares nothing.
/// A kind of manifest whose reader is not written yet declares none.
pub fn declared(kind: Manifest, text: &str) -> Option<Vec<Declared>> {
    match kind {
        Manifest::Cargo => cargo(text),
        Manifest::Npm | Manifest::Go => Some(Vec::new()),
    }
}

/// The dependencies the manifests among `files` declare, as edges.
/// A manifest of a kind whose reader is not written yet is left out.
pub fn edges(root: &Path, files: &[String], read_as_source: impl Fn(&str) -> bool) -> Edges {
    let text_of = |file: &str| std::fs::read_to_string(root.join(file)).ok();
    let mut found = Edges::default();
    for file in files {
        let Some(kind) = registry::manifest_of(file).filter(|kind| *kind == Manifest::Cargo) else {
            continue;
        };
        if !read_as_source(file) {
            continue;
        }
        found.manifests.push(file.clone());
        let Some(text) = text_of(file) else {
            found.problems.push(format!(
                "{file}: cannot be read, so what it declares is not checked"
            ));
            continue;
        };
        let Some(list) = declared(kind, &text) else {
            found.problems.push(format!(
                "{file}: is not valid {}, so what it declares is not checked",
                kind.file_name()
            ));
            continue;
        };
        for declared in list {
            let (resolved, problem) = resolve(file, &declared, files, &text_of);
            found.problems.extend(problem);
            let (specifier, target) = match resolved {
                // A package that names itself (`path = "."`) is no dependency.
                Resolved::Internal(path) if path == *file => continue,
                Resolved::Internal(path) => {
                    (path.clone(), Target::Internal(Unit::from(path.as_str())))
                }
                Resolved::External(module) => (module.clone(), Target::External(module)),
            };
            found.deps.push(Dep {
                from: match declared.part {
                    Some(part) => Unit::in_part(Path::new(file), part),
                    None => Unit::from(file.as_str()),
                },
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
    pub problems: Vec<String>,
}

/// What a declared dependency is, once resolved against the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The manifest of a package of the project, relative to the root.
    Internal(String),
    /// A third-party package, in the spelling of an import.
    External(String),
}

/// Resolves what the manifest at `manifest` declares as `declared`. `files` are
/// the project's files; `read` gives the text of one of them.
pub fn resolve(
    manifest: &str,
    declared: &Declared,
    files: &[String],
    read: &dyn Fn(&str) -> Option<String>,
) -> (Resolved, Option<String>) {
    let internal = |dir: &str, path: &str| {
        let target = join(dir, path).map(|dir| manifest_in(&dir))?;
        files.contains(&target).then_some(target)
    };
    let dir = parent(manifest);
    let mut package = declared.package.clone();
    let mut problem = None;
    let found = match &declared.source {
        Source::Path(path) => internal(dir, path),
        Source::Workspace => match workspace_dependency(dir, &declared.name, read) {
            Ok(Some(entry)) => {
                if let Some(real) = entry.package {
                    package = real;
                }
                entry.path.and_then(|path| internal(&entry.dir, &path))
            }
            Ok(None) => None,
            Err(workspace) => {
                problem = Some(format!(
                    "{manifest}: `{}` is `workspace = true`, but {workspace} cannot be read, so it is taken for an external crate",
                    declared.name
                ));
                None
            }
        },
        Source::Registry => None,
    };
    let resolved = found.map_or_else(
        || Resolved::External(package.replace('-', "_")),
        Resolved::Internal,
    );
    (resolved, problem)
}

/// What a workspace manifest declares under a name.
struct WorkspaceEntry {
    /// The directory of the workspace manifest.
    dir: String,
    path: Option<String>,
    package: Option<String>,
}

/// The directory a manifest is in, `""` for the root.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The manifest of the package in `dir`.
fn manifest_in(dir: &str) -> String {
    let name = Manifest::Cargo.file_name();
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

/// What the closest workspace manifest above `dir` declares under `name`. `Err` names a
/// manifest that holds `[workspace]` text it cannot read as TOML.
fn workspace_dependency(
    dir: &str,
    name: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<WorkspaceEntry>, String> {
    let mut here = Some(dir);
    while let Some(current) = here {
        let manifest = manifest_in(current);
        if let Some(text) = read(&manifest) {
            match toml::from_str::<toml::Table>(&text) {
                Ok(table) => {
                    if let Some(workspace) = table.get("workspace") {
                        let spec = workspace.get("dependencies").and_then(|d| d.get(name));
                        let text_of = |key: &str| {
                            spec.and_then(|s| s.get(key))
                                .and_then(toml::Value::as_str)
                                .map(str::to_string)
                        };
                        return Ok(spec.map(|_| WorkspaceEntry {
                            dir: current.to_string(),
                            path: text_of("path"),
                            package: text_of("package"),
                        }));
                    }
                }
                Err(_) if text.contains("[workspace") => return Err(manifest),
                Err(_) => {}
            }
        }
        here = (!current.is_empty()).then(|| parent(current));
    }
    Ok(None)
}

const CARGO_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// A table of dependencies of `Cargo.toml`: which one, and under which `cfg` of
/// `[target.<cfg>.…]` when one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Table<'a> {
    kind: &'a str,
    target: Option<String>,
}

fn cargo(text: &str) -> Option<Vec<Declared>> {
    static KEY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"^\s*("[^"]+"|'[^']+'|[A-Za-z0-9_-]+)\s*[.=]"#).expect("valid regex")
    });
    let manifest = toml::from_str::<toml::Table>(text).ok()?;
    let mut found: Vec<Declared> = Vec::new();
    let mut table: Option<Table> = None;
    // How many `{` and `[` of a value are still open: its lines are no keys.
    let mut open = 0_i32;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if open > 0 {
            open += brackets(trimmed);
            continue;
        }
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            let header = trimmed
                .split_once(']')
                .map_or(trimmed, |(inside, _)| inside)
                .trim_start_matches('[');
            let path = key_path(header);
            let (target, rest) = match path.as_slice() {
                [first, cfg, rest @ ..] if first == "target" => (Some(cfg.clone()), rest),
                rest => (None, rest),
            };
            table = None;
            if let Some(kind) = rest
                .first()
                .and_then(|name| CARGO_TABLES.iter().find(|kind| *kind == name))
            {
                let here = Table { kind, target };
                match rest {
                    // `[dependencies.regex]`: the table is the dependency.
                    [_, name] => found.extend(cargo_declared(&manifest, &here, name, line_number)),
                    [_] => table = Some(here),
                    _ => {}
                }
            }
        } else if let Some(here) = &table
            && let Some(caps) = KEY.captures(line)
        {
            let name = caps[1].trim_matches(['"', '\'']);
            if !found
                .iter()
                .any(|d| d.name == name && d.part == part_of(here.kind) && d.target == here.target)
            {
                found.extend(cargo_declared(&manifest, here, name, line_number));
            }
            open = brackets(trimmed.split_once('=').map_or("", |(_, value)| value));
        }
    }
    Some(found)
}

/// `{` and `[` opened by `text` minus the ones it closes, outside strings.
fn brackets(text: &str) -> i32 {
    let mut quote: Option<char> = None;
    let mut open = 0;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => break,
            (None, '{' | '[') => open += 1,
            (None, '}' | ']') => open -= 1,
            _ => {}
        }
    }
    open
}

/// The dependency `name` of `table`, from the parsed manifest, on `line`.
fn cargo_declared(
    manifest: &toml::Table,
    table: &Table,
    name: &str,
    line: usize,
) -> Option<Declared> {
    let section = match &table.target {
        Some(cfg) => manifest.get("target")?.get(cfg)?.get(table.kind)?,
        None => manifest.get(table.kind)?,
    };
    let spec = section.get(name)?;
    let package = spec
        .get("package")
        .and_then(toml::Value::as_str)
        .unwrap_or(name)
        .to_string();
    let source = if spec.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
        Source::Workspace
    } else if let Some(path) = spec.get("path").and_then(toml::Value::as_str) {
        Source::Path(path.to_string())
    } else {
        Source::Registry
    };
    Some(Declared {
        line,
        name: name.to_string(),
        package,
        source,
        part: part_of(table.kind),
        target: table.target.clone(),
    })
}

/// The part of a manifest a table of dependencies is: none for the plain one.
fn part_of(kind: &str) -> Option<Part> {
    match kind {
        "dev-dependencies" => Some(Part {
            name: kind.to_string(),
            kind: Some(Kind::Test),
        }),
        "build-dependencies" => Some(Part {
            name: kind.to_string(),
            kind: None,
        }),
        _ => None,
    }
}

/// The keys of a dotted TOML key: `target.'cfg(unix)'.dependencies`.
fn key_path(header: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in header.chars() {
        match (quote, c) {
            (None, '.') => keys.push(std::mem::take(&mut current)),
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (_, c) => current.push(c),
        }
    }
    keys.push(current);
    keys.into_iter().map(|k| k.trim().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .0;
        assert_eq!(got, Resolved::Internal("crates/local/Cargo.toml".into()));
        // A path with no manifest of the project there names nothing of it.
        let missing = resolve(
            "crates/chunk-store/Cargo.toml",
            &dep("gone", Source::Path("../gone".into())),
            &files,
            &read,
        )
        .0;
        assert_eq!(missing, Resolved::External("gone".into()));
    }

    #[test]
    fn a_workspace_dependency_is_what_the_workspace_manifest_declares() {
        let (files, read) = project();
        let from = "crates/chunk-store/Cargo.toml";
        assert_eq!(
            resolve(from, &dep("storage-api", Source::Workspace), &files, &read).0,
            Resolved::Internal("crates/storage-api/Cargo.toml".into())
        );
        assert_eq!(
            resolve(from, &dep("serde", Source::Workspace), &files, &read).0,
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
        );
        assert_eq!(got, Resolved::External("api".into()));
        assert!(problem.unwrap().contains("Cargo.toml"));
    }

    #[test]
    fn a_third_party_package_is_spelled_as_an_import_spells_it() {
        let (files, read) = project();
        let mut renamed = dep("json", Source::Registry);
        renamed.package = "serde-json".into();
        assert_eq!(
            resolve("crates/local/Cargo.toml", &renamed, &files, &read).0,
            Resolved::External("serde_json".into())
        );
    }
}
