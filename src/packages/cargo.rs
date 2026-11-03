//! `Cargo.toml`: the dependency tables, the features, and the workspace.

use std::sync::LazyLock;

use regex::Regex;

use super::{Declared, Source, manifest_in, parent};
use crate::lang::parts::{Kind, Part};
use crate::lang::registry::Manifest;

/// What a workspace manifest declares under a name.
pub(super) struct WorkspaceEntry {
    /// The directory of the workspace manifest.
    pub(super) dir: String,
    pub(super) path: Option<String>,
    pub(super) package: Option<String>,
}

/// What the closest workspace manifest above `dir` declares under `name`. `Err` names a
/// manifest that holds `[workspace]` text it cannot read as TOML.
pub(super) fn workspace_dependency(
    dir: &str,
    name: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<WorkspaceEntry>, String> {
    let mut here = Some(dir);
    while let Some(current) = here {
        let manifest = manifest_in(Manifest::Cargo, current);
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

pub(super) fn declared(text: &str) -> Option<Vec<Declared>> {
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
