//! The `undeclared` rule: an external module a file imports has to be a
//! dependency of the nearest manifest above it (`package.json`, `go.mod`,
//! `Cargo.toml`), or the build works only by accident.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::check::{RuleId, Violation};
use crate::components::ComponentMap;
use crate::config::Config;
use crate::lang::Target;
use crate::scan::Dep;

/// The kinds of manifests, by the extensions of the files they govern.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Npm,
    Go,
    Cargo,
}

impl Kind {
    fn of(file: &str) -> Option<Self> {
        match file.rsplit_once('.').map_or("", |(_, e)| e) {
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Kind::Npm),
            "go" => Some(Kind::Go),
            "rs" => Some(Kind::Cargo),
            _ => None,
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Kind::Npm => "package.json",
            Kind::Go => "go.mod",
            Kind::Cargo => "Cargo.toml",
        }
    }

    /// The package an external module belongs to: `lodash` for
    /// `lodash/fp`, `@scope/pkg` for `@scope/pkg/x`, the crate for a Rust
    /// path. Go modules are matched by prefix instead, so the path stays.
    fn package(self, module: &str) -> String {
        match self {
            Kind::Npm => {
                let mut parts = module.split('/');
                let first = parts.next().unwrap_or_default();
                match parts.next() {
                    Some(second) if first.starts_with('@') => format!("{first}/{second}"),
                    _ => first.to_string(),
                }
            }
            Kind::Go => module.to_string(),
            Kind::Cargo => module
                .split("::")
                .next()
                .unwrap_or(module)
                .replace('-', "_"),
        }
    }
}

/// What a manifest declares: the names of its dependencies, and its own
/// name, which a file of the package may import too.
fn declared(kind: Kind, text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    match kind {
        Kind::Npm => {
            let json: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
            if let Some(name) = json["name"].as_str() {
                names.insert(name.to_string());
            }
            for section in [
                "dependencies",
                "devDependencies",
                "peerDependencies",
                "optionalDependencies",
            ] {
                for name in json[section].as_object().into_iter().flat_map(|m| m.keys()) {
                    names.insert(name.clone());
                    // `@types/node` declares the types of `node`, `@types/a__b` of `@a/b`.
                    if let Some(typed) = name.strip_prefix("@types/") {
                        names.insert(match typed.split_once("__") {
                            Some((scope, pkg)) => format!("@{scope}/{pkg}"),
                            None => typed.to_string(),
                        });
                    }
                }
            }
        }
        Kind::Go => {
            let mut in_block = false;
            for line in text
                .lines()
                .map(|l| l.split("//").next().unwrap_or_default().trim())
            {
                if let Some(module) = line.strip_prefix("module ") {
                    names.insert(module.trim().trim_matches('"').to_string());
                } else if line == "require (" {
                    in_block = true;
                } else if in_block && line == ")" {
                    in_block = false;
                } else if let Some(required) =
                    line.strip_prefix("require ").or(in_block.then_some(line))
                    && let Some(path) = required.split_whitespace().next()
                {
                    names.insert(path.trim_matches('"').to_string());
                }
            }
        }
        Kind::Cargo => {
            let table: toml::Table = toml::from_str(text).unwrap_or_default();
            if let Some(name) = table
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            {
                names.insert(name.replace('-', "_"));
            }
            let mut sections: Vec<&toml::Value> = Vec::new();
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                sections.extend(table.get(key));
                sections.extend(table.get("workspace").and_then(|w| w.get(key)));
            }
            for target in table
                .get("target")
                .and_then(|t| t.as_table())
                .into_iter()
                .flat_map(|t| t.values())
            {
                for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    sections.extend(target.get(key));
                }
            }
            for section in sections {
                for name in section.as_table().into_iter().flat_map(|t| t.keys()) {
                    names.insert(name.replace('-', "_"));
                }
            }
        }
    }
    names
}

/// Whether a manifest that declares `names` covers the package of `module`.
fn covers(kind: Kind, names: &BTreeSet<String>, package: &str) -> bool {
    match kind {
        Kind::Go => names
            .iter()
            .any(|m| package == m || package.starts_with(&format!("{m}/"))),
        Kind::Npm | Kind::Cargo => names.contains(package),
    }
}

pub struct Undeclared {
    pub violations: Vec<Violation>,
    pub reasons: Vec<String>,
}

/// With `manifests`, every import of an external module that the nearest
/// manifest above the importing file does not declare.
pub fn undeclared(
    config: &Config,
    map: &ComponentMap,
    root: &Path,
    deps: &[Dep],
    all_files: &[String],
) -> Undeclared {
    if !config.manifests {
        return Undeclared {
            violations: Vec::new(),
            reasons: Vec::new(),
        };
    }
    let known: BTreeSet<&str> = all_files.iter().map(String::as_str).collect();
    let mut cache: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut violations = Vec::new();
    let mut unjudged = BTreeSet::new();
    let mut orphans = BTreeSet::new();
    for dep in deps {
        let Target::External(module) = &dep.target else {
            continue;
        };
        let file = crate::scan::slash(&dep.file);
        let Some(kind) = Kind::of(&file) else {
            unjudged.insert(file.rsplit_once('.').map_or("", |(_, e)| e).to_string());
            continue;
        };
        if crate::stdlib::is_std(&file, module) {
            continue;
        }
        let Some(manifest) = nearest(&file, kind.file_name(), &known) else {
            orphans.insert(kind.file_name());
            continue;
        };
        let names = cache.entry(manifest.clone()).or_insert_with(|| {
            declared(
                kind,
                &std::fs::read_to_string(root.join(&manifest)).unwrap_or_default(),
            )
        });
        let package = kind.package(module);
        if covers(kind, names, &package) {
            continue;
        }
        violations.push(Violation {
            rule: RuleId::Undeclared,
            evidence: vec![(file.clone(), dep.line)],
            line: dep.line,
            message: format!(
                "{package} is not declared in {manifest}; add it to the dependencies, or drop the import"
            ),
            because: None,
            from: map.component_of(&file).unwrap_or_default().to_string(),
            to: String::new(),
            target: package,
            specifier: dep.specifier.clone(),
            file,
        });
    }
    let mut reasons = Vec::new();
    if !unjudged.is_empty() {
        let exts: Vec<String> = unjudged.into_iter().collect();
        reasons.push(format!(
            "`manifests` cannot tell the package of a module in .{} files, so their imports were not checked",
            exts.join(", .")
        ));
    }
    if !orphans.is_empty() {
        let names: Vec<&str> = orphans.into_iter().collect();
        reasons.push(format!(
            "some files have no {} above them, so their imports were not checked",
            names.join(" or ")
        ));
    }
    Undeclared {
        violations,
        reasons,
    }
}

/// The nearest file named `name` in the directory of `file` or above it.
fn nearest(file: &str, name: &str, known: &BTreeSet<&str>) -> Option<String> {
    let mut dir = file.rsplit_once('/').map_or("", |(d, _)| d);
    loop {
        let candidate = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        };
        if known.contains(candidate.as_str()) {
            return Some(candidate);
        }
        if dir.is_empty() {
            return None;
        }
        dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_package_of_a_module() {
        assert_eq!(Kind::Npm.package("lodash/fp"), "lodash");
        assert_eq!(
            Kind::Npm.package("@mantine/core/styles.css"),
            "@mantine/core"
        );
        assert_eq!(Kind::Cargo.package("serde_json::Value"), "serde_json");
    }

    #[test]
    fn reads_what_each_manifest_declares() {
        let npm = declared(
            Kind::Npm,
            r#"{"name": "app", "dependencies": {"react": "1"}, "devDependencies": {"@types/node": "1", "@types/babel__core": "1"}}"#,
        );
        for name in ["app", "react", "node", "@babel/core"] {
            assert!(npm.contains(name), "{name}");
        }
        let go = declared(
            Kind::Go,
            "module ex.com/m\n\ngo 1.22\n\nrequire github.com/lib/pq v1.10.0\n\nrequire (\n\tgolang.org/x/sync v0.1.0 // indirect\n\tgithub.com/a/b v1.0.0\n)\n",
        );
        assert!(covers(Kind::Go, &go, "github.com/lib/pq"));
        assert!(covers(Kind::Go, &go, "golang.org/x/sync/errgroup"));
        assert!(covers(Kind::Go, &go, "ex.com/m/internal/x"));
        assert!(!covers(Kind::Go, &go, "github.com/a/bc"));
        let cargo = declared(
            Kind::Cargo,
            "[package]\nname = \"my-app\"\n\n[dependencies]\nserde = \"1\"\nserde-json = { version = \"1\" }\n\n[dev-dependencies.insta]\nversion = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n",
        );
        for name in ["my_app", "serde", "serde_json", "insta", "libc"] {
            assert!(cargo.contains(name), "{name}");
        }
    }

    #[test]
    fn finds_the_nearest_manifest() {
        let known: BTreeSet<&str> = ["package.json", "web/package.json"].into();
        assert_eq!(
            nearest("web/src/a.ts", "package.json", &known).as_deref(),
            Some("web/package.json")
        );
        assert_eq!(
            nearest("api/a.ts", "package.json", &known).as_deref(),
            Some("package.json")
        );
        assert_eq!(nearest("a.go", "go.mod", &known), None);
    }
}
