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
        Source::Own => Some(manifest.to_string()),
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
    // `[features]`: the items of each feature, with their lines.
    let mut items: Vec<(usize, String, String)> = Vec::new();
    let mut in_features = false;
    let mut feature: Option<String> = None;
    // How many `{` and `[` of a value are still open: its lines are no keys.
    let mut open = 0_i32;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if open > 0 {
            if in_features && let Some(feature) = &feature {
                items.extend(strings(trimmed).map(|item| (line_number, feature.clone(), item)));
            }
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
            in_features = target.is_none() && rest == ["features"];
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
        } else if in_features && let Some(caps) = KEY.captures(line) {
            let name = caps[1].trim_matches(['"', '\'']).to_string();
            let value = trimmed.split_once('=').map_or("", |(_, value)| value);
            items.extend(strings(value).map(|item| (line_number, name.clone(), item)));
            open = brackets(value);
            feature = Some(name);
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
    Some(with_features(found, &items))
}

/// The strings of the value of a TOML line, without its comment.
fn strings(text: &str) -> impl Iterator<Item = String> + '_ {
    static STRING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""([^"]*)"|'([^']*)'"#).expect("valid regex"));
    let code = text.split('#').next().unwrap_or_default();
    STRING
        .captures_iter(code)
        .map(|caps| {
            caps.get(1)
                .or_else(|| caps.get(2))
                .map_or("", |m| m.as_str())
                .to_string()
        })
        .collect::<Vec<_>>()
        .into_iter()
}

/// What a feature item turns on.
enum Item<'a> {
    /// `dep:x`: the dependency.
    Dependency(&'a str),
    /// `x/f`, or `x?/f` when weak: the dependency, and its feature.
    Reaches {
        name: &'a str,
        feature: &'a str,
        weak: bool,
    },
    /// `other`: a feature of the manifest.
    Feature(&'a str),
}

fn item(text: &str) -> Item<'_> {
    if let Some(name) = text.strip_prefix("dep:") {
        return Item::Dependency(name);
    }
    match text.split_once('/') {
        Some((name, feature)) => match name.strip_suffix('?') {
            Some(name) => Item::Reaches {
                name,
                feature,
                weak: true,
            },
            None => Item::Reaches {
                name,
                feature,
                weak: false,
            },
        },
        None => Item::Feature(text),
    }
}

/// The part a feature is, `default` being the package itself.
fn feature_part(feature: &str) -> Option<Part> {
    (feature != "default").then(|| Part {
        name: format!("feature/{feature}"),
        kind: None,
    })
}

/// The dependencies of the tables, put where the features say they belong: an optional
/// one is a dependency of what turns it on, and of its implicit feature when nothing
/// names it with `dep:`; and the items of the features, which are edges of their own.
fn with_features(declared: Vec<Declared>, items: &[(usize, String, String)]) -> Vec<Declared> {
    let named = |name: &str| {
        items
            .iter()
            .any(|(_, _, text)| matches!(item(text), Item::Dependency(n) if n == name))
    };
    // The features whose items reach a dependency.
    let owners = |name: &str| {
        let mut owners: Vec<&str> = Vec::new();
        for (_, feature, text) in items {
            let hit = match item(text) {
                Item::Dependency(n) => n == name,
                Item::Reaches { name: n, weak, .. } => n == name && !weak,
                Item::Feature(_) => false,
            };
            if hit && !owners.contains(&feature.as_str()) {
                owners.push(feature);
            }
        }
        owners
    };
    let mut out: Vec<Declared> = Vec::new();
    for d in &declared {
        if !d.optional {
            out.push(d.clone());
        } else if !named(&d.name) {
            // The implicit feature `name` turns it on.
            out.push(Declared {
                part: feature_part(&d.name),
                ..d.clone()
            });
        } else if d.feature.is_some() {
            for owner in owners(&d.name) {
                out.push(Declared {
                    part: feature_part(owner),
                    ..d.clone()
                });
            }
        }
    }
    // What an item names is a dependency of the crate, never a dev-dependency.
    let table_dependency = |name: &str| {
        declared.iter().find(|d| {
            d.name == name
                && d.feature.is_none()
                && !d
                    .part
                    .as_ref()
                    .is_some_and(|part| part.kind == Some(Kind::Test))
        })
    };
    for (line, feature, text) in items {
        let from = feature_part(feature);
        let base = |name: &str| {
            table_dependency(name).map(|d| Declared {
                line: *line,
                part: None,
                feature: None,
                optional: false,
                ..d.clone()
            })
        };
        let mut edge = |name: &str, feature: Option<&str>| {
            // A name no table declares is a mistake Cargo reports; it is no edge.
            if let Some(base) = base(name) {
                out.push(Declared {
                    part: from.clone(),
                    feature: feature.map(str::to_string),
                    ..base
                });
            }
        };
        match item(text) {
            Item::Dependency(name) => edge(name, None),
            Item::Reaches {
                name,
                feature,
                weak,
            } => {
                if !weak {
                    edge(name, None);
                }
                edge(name, Some(feature));
            }
            Item::Feature(name) => out.push(Declared {
                line: *line,
                name: name.to_string(),
                package: name.to_string(),
                source: Source::Own,
                part: from.clone(),
                target: None,
                feature: Some(name.to_string()),
                optional: false,
            }),
        }
    }
    out.sort_by_key(|d| d.line);
    out
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
fn cargo_declared(manifest: &toml::Table, table: &Table, name: &str, line: usize) -> Vec<Declared> {
    let section = match &table.target {
        Some(cfg) => manifest
            .get("target")
            .and_then(|target| target.get(cfg))
            .and_then(|target| target.get(table.kind)),
        None => manifest.get(table.kind),
    };
    let Some(spec) = section.and_then(|section| section.get(name)) else {
        return Vec::new();
    };
    let package = spec
        .get("package")
        .and_then(toml::Value::as_str)
        .unwrap_or(name)
        .to_string();
    let optional = spec.get("optional").and_then(toml::Value::as_bool) == Some(true);
    let source = if spec.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
        Source::Workspace
    } else if let Some(path) = spec.get("path").and_then(toml::Value::as_str) {
        Source::Path(path.to_string())
    } else {
        Source::Registry
    };
    let own = Declared {
        line,
        name: name.to_string(),
        package,
        source,
        part: part_of(table.kind),
        target: table.target.clone(),
        feature: None,
        optional,
    };
    // `features = ["f"]` also reaches the feature `f` of the package.
    let features: Vec<Declared> = spec
        .get("features")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(|f| Declared {
            feature: Some(f.to_string()),
            ..own.clone()
        })
        .collect();
    std::iter::once(own).chain(features).collect()
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
}
