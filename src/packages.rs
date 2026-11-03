//! The `packages` key: a manifest is a checked file, and each dependency it
//! declares is an edge from it, like an import of a source file. A
//! dev-dependency is test code, so `tests: auto` takes it.

use std::path::{Path, PathBuf};

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
}

/// The dependencies a manifest of `kind` declares, with the line of each.
/// A kind of manifest whose reader is not written yet declares none.
pub fn declared(kind: Manifest, text: &str) -> Vec<Declared> {
    match kind {
        Manifest::Cargo => cargo(text),
        Manifest::Npm | Manifest::Go => Vec::new(),
    }
}

/// The manifests among `files` that are read, and the dependencies they declare as edges.
/// A manifest of a kind whose reader is not written yet is left out.
pub fn edges(
    root: &Path,
    files: &[String],
    read_as_source: impl Fn(&str) -> bool,
) -> (Vec<String>, Vec<Dep>) {
    let text_of = |file: &str| std::fs::read_to_string(root.join(file)).ok();
    let mut manifests = Vec::new();
    let mut deps = Vec::new();
    for file in files {
        let Some(kind) = registry::manifest_of(file).filter(|kind| *kind == Manifest::Cargo) else {
            continue;
        };
        if !read_as_source(file) {
            continue;
        }
        let Some(text) = text_of(file) else {
            continue;
        };
        manifests.push(file.clone());
        for declared in declared(kind, &text) {
            let (specifier, target) = match resolve(file, &declared, files, &text_of) {
                Resolved::Internal(path) => {
                    (path.clone(), Target::Internal(Unit::from(path.as_str())))
                }
                Resolved::External(module) => (module.clone(), Target::External(module)),
            };
            deps.push(Dep {
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
    (manifests, deps)
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
) -> Resolved {
    let external = || Resolved::External(declared.package.replace('-', "_"));
    let internal = |dir: &str, path: &str| {
        let target = join(dir, path).map(|dir| manifest_in(&dir))?;
        files.contains(&target).then_some(target)
    };
    let dir = parent(manifest);
    let found = match &declared.source {
        Source::Path(path) => internal(dir, path),
        Source::Workspace => workspace_path(dir, &declared.name, read)
            .and_then(|(root, path)| internal(&root, &path)),
        Source::Registry => None,
    };
    found.map_or_else(external, Resolved::Internal)
}

/// The directory a manifest is in, `""` for the root.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The manifest of the package in `dir`.
fn manifest_in(dir: &str) -> String {
    if dir.is_empty() {
        "Cargo.toml".to_string()
    } else {
        format!("{dir}/Cargo.toml")
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

/// What the closest workspace manifest above `dir` declares under `name` as a
/// path: the directory of that manifest and the path.
fn workspace_path(
    dir: &str,
    name: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    let mut here = Some(dir);
    while let Some(current) = here {
        if let Some(text) = read(&manifest_in(current))
            && let Ok(table) = toml::from_str::<toml::Table>(&text)
            && let Some(workspace) = table.get("workspace")
        {
            let path = workspace
                .get("dependencies")?
                .get(name)?
                .get("path")?
                .as_str()?;
            return Some((current.to_string(), path.to_string()));
        }
        here = (!current.is_empty()).then(|| parent(current));
    }
    None
}

const CARGO_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// A table of dependencies of `Cargo.toml`: which one, and under which `cfg` of
/// `[target.<cfg>.…]` when one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Table<'a> {
    kind: &'a str,
    target: Option<String>,
}

fn cargo(text: &str) -> Vec<Declared> {
    let Ok(manifest) = toml::from_str::<toml::Table>(text) else {
        return Vec::new();
    };
    let key = Regex::new(r#"^\s*("[^"]+"|'[^']+'|[A-Za-z0-9_-]+)\s*[.=]"#).expect("valid regex");
    let mut found = Vec::new();
    let mut table: Option<Table> = None;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
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
            && let Some(caps) = key.captures(line)
        {
            let name = caps[1].trim_matches(['"', '\'']);
            if !found
                .iter()
                .any(|d: &Declared| d.name == name && d.part == part_of(here.kind))
            {
                found.extend(cargo_declared(&manifest, here, name, line_number));
            }
        }
    }
    found
}

/// The dependency `name` of `table`, from the parsed manifest, on `line`.
fn cargo_declared(
    manifest: &toml::Table,
    table: &Table,
    name: &str,
    line: usize,
) -> Option<Declared> {
    let tables = match &table.target {
        Some(cfg) => manifest.get("target")?.get(cfg)?,
        None => &toml::Value::Table(manifest.clone()),
    };
    let spec = tables.get(table.kind)?.get(name)?;
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
            .into_iter()
            .map(|d| d.part.and_then(|part| part.kind))
            .collect();
        assert_eq!(kinds[4], Some(Kind::Test));
        assert_eq!(kinds[5], None);
    }

    #[test]
    fn a_manifest_that_is_no_toml_declares_nothing() {
        assert!(found("[dependencies\nx = ").is_empty());
    }

    fn dep(name: &str, source: Source) -> Declared {
        Declared {
            line: 1,
            name: name.to_string(),
            package: name.to_string(),
            source,
            part: None,
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
        );
        assert_eq!(got, Resolved::Internal("crates/local/Cargo.toml".into()));
        // A path with no manifest of the project there names nothing of it.
        let missing = resolve(
            "crates/chunk-store/Cargo.toml",
            &dep("gone", Source::Path("../gone".into())),
            &files,
            &read,
        );
        assert_eq!(missing, Resolved::External("gone".into()));
    }

    #[test]
    fn a_workspace_dependency_is_what_the_workspace_manifest_declares() {
        let (files, read) = project();
        let from = "crates/chunk-store/Cargo.toml";
        assert_eq!(
            resolve(from, &dep("storage-api", Source::Workspace), &files, &read),
            Resolved::Internal("crates/storage-api/Cargo.toml".into())
        );
        assert_eq!(
            resolve(from, &dep("serde", Source::Workspace), &files, &read),
            Resolved::External("serde".into())
        );
    }

    #[test]
    fn a_third_party_package_is_spelled_as_an_import_spells_it() {
        let (files, read) = project();
        let mut renamed = dep("json", Source::Registry);
        renamed.package = "serde-json".into();
        assert_eq!(
            resolve("crates/local/Cargo.toml", &renamed, &files, &read),
            Resolved::External("serde_json".into())
        );
    }
}
