//! The `undeclared` rule: an external module a file imports has to be a
//! dependency of the nearest manifest above it (`package.json`, `go.mod`,
//! `Cargo.toml`, `pubspec.yaml`), or the build works only by accident.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;

use crate::check::{RuleId, Unjudged, Violation};
use crate::components::ComponentMap;
use crate::config::Config;
use crate::lang::Target;
use crate::lang::registry::{self, Manifest};
use crate::relpath;
use crate::scan::Dep;

impl Manifest {
    /// The package an external module belongs to: `lodash` for
    /// `lodash/fp`, `@scope/pkg` for `@scope/pkg/x`, the crate for a Rust
    /// path (already written with `_`). Go modules are matched by prefix
    /// instead, so the path stays.
    pub(crate) fn package(self, module: &str) -> String {
        match self {
            Manifest::Npm => {
                let mut parts = module.split('/');
                let first = parts.next().unwrap_or_default();
                match parts.next() {
                    Some(second) if first.starts_with('@') => format!("{first}/{second}"),
                    _ => first.to_string(),
                }
            }
            Manifest::Go => module.to_string(),
            Manifest::Cargo => module.split("::").next().unwrap_or(module).to_string(),
            Manifest::Pub => module
                .strip_prefix("package:")
                .unwrap_or(module)
                .split('/')
                .next()
                .unwrap_or(module)
                .to_string(),
        }
    }
}

const NPM_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// What a manifest declares: the names of its dependencies, and its own
/// name, which a file of the package may import too. `None` for a manifest
/// that cannot be read, and for a `package.json` that only marks its
/// directory (`{"type": "module"}`), which is no package of its own.
fn declared(kind: Manifest, text: &str) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    match kind {
        Manifest::Pub => {
            let yaml: serde_norway::Value = serde_norway::from_str(text).ok()?;
            let mapping = yaml.as_mapping()?;
            if let Some(name) = mapping.get("name").and_then(|v| v.as_str()) {
                names.insert(name.to_string());
            }
            for section in ["dependencies", "dev_dependencies"] {
                if let Some(value) = mapping.get(section).filter(|v| !v.is_null()) {
                    for name in value.as_mapping()?.keys() {
                        names.insert(name.as_str()?.to_string());
                    }
                }
            }
        }
        Manifest::Npm => {
            let json: serde_json::Value = serde_json::from_str(text).ok()?;
            if json["name"].is_null() && NPM_SECTIONS.iter().all(|s| json[s].is_null()) {
                return None;
            }
            if let Some(name) = json["name"].as_str() {
                names.insert(name.to_string());
            }
            for section in NPM_SECTIONS {
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
        Manifest::Go => {
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
        Manifest::Cargo => {
            let table: toml::Table = toml::from_str(text).ok()?;
            if let Some(name) = table
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            {
                names.insert(name.replace('-', "_"));
            }
            let keys = ["dependencies", "dev-dependencies", "build-dependencies"];
            let mut sections: Vec<&toml::Value> = Vec::new();
            for key in keys {
                sections.extend(table.get(key));
                sections.extend(table.get("workspace").and_then(|w| w.get(key)));
            }
            for target in table
                .get("target")
                .and_then(|t| t.as_table())
                .into_iter()
                .flat_map(|t| t.values())
            {
                for key in keys {
                    sections.extend(target.get(key));
                }
            }
            for section in sections {
                for (name, spec) in section.as_table().into_iter().flatten() {
                    names.insert(name.replace('-', "_"));
                    // `json = { package = "serde_json" }`: the index names the real crate.
                    if let Some(real) = spec.get("package").and_then(|p| p.as_str()) {
                        names.insert(real.replace('-', "_"));
                    }
                }
            }
        }
    }
    Some(names)
}

/// Whether a manifest that declares `names` covers `package`.
fn covers(kind: Manifest, names: &BTreeSet<String>, package: &str) -> bool {
    match kind {
        Manifest::Go => names
            .iter()
            .any(|m| package == m || package.starts_with(&format!("{m}/"))),
        Manifest::Npm | Manifest::Cargo | Manifest::Pub => names.contains(package),
    }
}

/// Whether the source of a file names `package` in an import. The index
/// names the package that defines a symbol, which differs from the one the
/// code imports for a re-export (`serde` re-exports `serde_core`), a global
/// type (`NodeJS.Timeout`) or a Node builtin; only the imported one has to
/// be declared.
fn names_in_source(kind: Manifest, source: &str, package: &str) -> bool {
    match kind {
        Manifest::Pub => crate::indexer::dart::names_package(source, package),
        Manifest::Npm => {
            let quoted = Regex::new(
                r#"(?:\bfrom|\bimport|\brequire\s*\(|\bimport\s*\()\s*["']([^"']+)["']"#,
            )
            .expect("valid regex");
            quoted
                .captures_iter(source)
                .map(|c| c[1].to_string())
                // A path names a file, not a package.
                .filter(|specifier| !specifier.starts_with(['.', '/']))
                .any(|specifier| Manifest::Npm.package(&specifier) == package)
        }
        Manifest::Cargo => {
            let name = regex::escape(package);
            Regex::new(&format!(
                r"(?:^|[^\w:]){name}::|\bextern\s+crate\s+{name}\b"
            ))
            .expect("valid regex")
            .is_match(source)
        }
        // A Go import is the package path itself.
        Manifest::Go => true,
    }
}

pub struct Undeclared {
    pub violations: Vec<Violation>,
    pub reasons: Vec<Unjudged>,
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
    // Manifest -> what it declares; `None` when it declares no package.
    let mut manifests: BTreeMap<String, Option<BTreeSet<String>>> = BTreeMap::new();
    let mut unreadable: BTreeSet<String> = BTreeSet::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut violations = Vec::new();
    let mut unjudged = BTreeSet::new();
    let mut orphans = BTreeSet::new();
    // The files no manifest is above.
    let mut unowned = BTreeSet::new();
    for dep in deps {
        let Target::External(module) = &dep.target else {
            continue;
        };
        let file = crate::scan::slash(&dep.file);
        // What a manifest declares is `packages`' to judge, not an import to look up.
        if registry::manifest_of(&file).is_some() {
            continue;
        }
        let Some(kind) = registry::of(&file).and_then(|language| language.manifest) else {
            unjudged.insert(relpath::extension(&file).to_string());
            continue;
        };
        if crate::stdlib::is_std(&file, module) {
            continue;
        }
        let Some(manifest) =
            package_manifest(&file, kind, root, &known, &mut manifests, &mut unreadable)
        else {
            orphans.insert(kind.file_name());
            unowned.insert(file.clone());
            continue;
        };
        let Some(Some(names)) = manifests.get(&manifest) else {
            continue; // unreadable: reported once below
        };
        let package = kind.package(module);
        if covers(kind, names, &package) {
            continue;
        }
        let source = sources
            .entry(file.clone())
            .or_insert_with(|| std::fs::read_to_string(root.join(&file)).unwrap_or_default());
        if !names_in_source(kind, source, &package) {
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
        reasons.push(
            Unjudged::new(format!(
                "`manifests` cannot tell the package of a module in .{} files, so their imports were not checked",
                exts.join(", .")
            ))
            .in_extensions(exts),
        );
    }
    if !unreadable.is_empty() {
        let list: Vec<String> = unreadable.into_iter().collect();
        reasons.push(
            Unjudged::new(format!(
                "{} could not be read, so the imports of the files under it were not checked",
                list.join(", ")
            ))
            .in_files(list),
        );
    }
    if !orphans.is_empty() {
        let names: Vec<&str> = orphans.into_iter().collect();
        reasons.push(
            Unjudged::new(format!(
                "some files have no {} above them, so their imports were not checked",
                names.join(" or ")
            ))
            .in_files(unowned),
        );
    }
    Undeclared {
        violations,
        reasons,
    }
}

/// The manifest that governs `file`: the nearest one above it that declares
/// a package, considering only `known` project files and passing over a
/// `package.json` that only marks its directory.
/// An unreadable one stops the search and is recorded in `unreadable`.
fn package_manifest(
    file: &str,
    kind: Manifest,
    root: &Path,
    known: &BTreeSet<&str>,
    manifests: &mut BTreeMap<String, Option<BTreeSet<String>>>,
    unreadable: &mut BTreeSet<String>,
) -> Option<String> {
    let mut below = file.to_string();
    loop {
        let manifest = nearest(&below, kind.file_name(), known)?;
        let declares = manifests
            .entry(manifest.clone())
            .or_insert_with(|| {
                let text = match std::fs::read_to_string(root.join(&manifest)) {
                    Ok(text) => text,
                    Err(_) => {
                        unreadable.insert(manifest.clone());
                        return None;
                    }
                };
                let names = declared(kind, &text);
                let marker = kind == Manifest::Npm
                    && serde_json::from_str::<serde_json::Value>(&text).is_ok();
                if names.is_none() && !marker {
                    unreadable.insert(manifest.clone());
                }
                names
            })
            .is_some();
        if declares || unreadable.contains(&manifest) {
            return Some(manifest);
        }
        // A marker: look above its directory.
        let dir = manifest.rsplit_once('/').map(|(d, _)| d)?;
        below = dir.to_string();
    }
}

/// The nearest file named `name` in the directory of `file` or above it.
fn nearest(file: &str, name: &str, known: &BTreeSet<&str>) -> Option<String> {
    let mut dir = relpath::parent(file);
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
        dir = relpath::parent(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dart_manifest_counts_pub_declarations_not_resolution_overrides() {
        let yaml = "name: example\ndependencies:\n  hosted: ^1.0.0\n  hosted_map: {hosted: https://pub.dev, version: ^1.0.0}\n  from_git: {git: https://example.com/package.git}\n  local: {path: ../local}\n  flutter: {sdk: flutter}\ndev_dependencies:\n  testing: any\ndependency_overrides:\n  override_only: {path: ../override}\n";
        let names = declared(Manifest::Pub, yaml).unwrap();
        for name in [
            "example",
            "hosted",
            "hosted_map",
            "from_git",
            "local",
            "flutter",
            "testing",
        ] {
            assert!(names.contains(name));
        }
        assert!(!names.contains("override_only"));
        assert_eq!(
            Manifest::Pub.package("package:hosted/src/api.dart"),
            "hosted"
        );
        assert!(declared(Manifest::Pub, "dependencies: [bad]").is_none());
        assert!(declared(Manifest::Pub, "name: example\ndependencies:\n").is_some());
        assert!(names_in_source(
            Manifest::Pub,
            "export r'package:hosted/api.dart';",
            "hosted"
        ));
        assert!(!names_in_source(
            Manifest::Pub,
            "// import 'package:hosted/api.dart';",
            "hosted"
        ));
    }

    #[test]
    fn names_the_package_of_a_module() {
        assert_eq!(Manifest::Npm.package("lodash/fp"), "lodash");
        assert_eq!(
            Manifest::Npm.package("@mantine/core/styles.css"),
            "@mantine/core"
        );
        assert_eq!(Manifest::Cargo.package("serde_json::Value"), "serde_json");
    }

    #[test]
    fn reads_what_each_manifest_declares() {
        let npm = declared(
            Manifest::Npm,
            r#"{"name": "app", "dependencies": {"react": "1"}, "devDependencies": {"@types/node": "1", "@types/babel__core": "1"}}"#,
        )
        .unwrap();
        for name in ["app", "react", "node", "@babel/core"] {
            assert!(npm.contains(name), "{name}");
        }
        let go = declared(
            Manifest::Go,
            "module ex.com/m\n\ngo 1.22\n\nrequire github.com/lib/pq v1.10.0\n\nrequire (\n\tgolang.org/x/sync v0.1.0 // indirect\n\tgithub.com/a/b v1.0.0\n)\n",
        )
        .unwrap();
        assert!(covers(Manifest::Go, &go, "github.com/lib/pq"));
        assert!(covers(Manifest::Go, &go, "golang.org/x/sync/errgroup"));
        assert!(covers(Manifest::Go, &go, "ex.com/m/internal/x"));
        assert!(!covers(Manifest::Go, &go, "github.com/a/bc"));
        let cargo = declared(
            Manifest::Cargo,
            "[package]\nname = \"my-app\"\n\n[dependencies]\nserde = \"1\"\nserde-json = { version = \"1\" }\njson5 = { package = \"json-five\", version = \"1\" }\n\n[dev-dependencies.insta]\nversion = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n",
        )
        .unwrap();
        for name in [
            "my_app",
            "serde",
            "serde_json",
            "insta",
            "libc",
            "json5",
            "json_five",
        ] {
            assert!(cargo.contains(name), "{name}");
        }
    }

    #[test]
    fn a_marker_or_a_broken_manifest_declares_nothing() {
        assert!(declared(Manifest::Npm, r#"{"type": "module"}"#).is_none());
        assert!(declared(Manifest::Npm, "{ not json").is_none());
        assert!(declared(Manifest::Cargo, "[dependencies\n").is_none());
    }

    #[test]
    fn only_a_package_the_source_imports_has_to_be_declared() {
        let ts =
            "import { a } from \"pkg/sub\";\nlet t: NodeJS.Timeout;\nimport fs from \"node:fs\";\n";
        assert!(names_in_source(Manifest::Npm, ts, "pkg"));
        assert!(!names_in_source(Manifest::Npm, ts, "node"));
        assert!(!names_in_source(Manifest::Npm, ts, "b"));
        // A path names a file of the project, not the package `.` or ``.
        let relative = "import { h } from \"./helper\";\nimport x from \"/abs/x\";\n";
        assert!(!names_in_source(Manifest::Npm, relative, "."));
        assert!(!names_in_source(Manifest::Npm, relative, ""));
        let rs = "use serde::de::Error;\nextern crate libc;\nfn f() { other::x(); }\n";
        assert!(names_in_source(Manifest::Cargo, rs, "serde"));
        assert!(names_in_source(Manifest::Cargo, rs, "libc"));
        assert!(!names_in_source(Manifest::Cargo, rs, "serde_core"));
        assert!(!names_in_source(
            Manifest::Cargo,
            "use my::serde::x;",
            "serde"
        ));
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

    #[test]
    fn a_marker_package_json_is_passed_over() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src/esm")).unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name": "app"}"#).unwrap();
        std::fs::write(
            dir.path().join("src/esm/package.json"),
            r#"{"type": "module"}"#,
        )
        .unwrap();
        let known: BTreeSet<&str> = ["package.json", "src/esm/package.json"].into();
        let (mut manifests, mut unreadable) = (BTreeMap::new(), BTreeSet::new());
        let found = package_manifest(
            "src/esm/a.ts",
            Manifest::Npm,
            dir.path(),
            &known,
            &mut manifests,
            &mut unreadable,
        );
        assert_eq!(found.as_deref(), Some("package.json"));
        assert!(unreadable.is_empty());
    }
    fn manifest_samples() -> [(Manifest, &'static str); 4] {
        [
            (Manifest::Npm, r#"{"name":"owner"}"#),
            (Manifest::Go, "module example.com/owner\n"),
            (Manifest::Cargo, "[package]\nname = 'owner'\n"),
            (Manifest::Pub, "name: owner\n"),
        ]
    }

    #[test]
    fn every_manifest_kind_uses_the_nearest_known_ancestor() {
        for (kind, text) in manifest_samples() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("project");
            std::fs::create_dir_all(root.join("web/src")).unwrap();
            std::fs::create_dir_all(root.join("neighbor")).unwrap();
            let name = kind.file_name();
            let nested = format!("web/{name}");
            let neighbor = format!("neighbor/{name}");
            for path in [name, nested.as_str(), neighbor.as_str()] {
                std::fs::write(root.join(path), text).unwrap();
            }
            std::fs::write(tmp.path().join(name), text).unwrap();
            for (source, known, expected) in [
                (
                    "web/src/file",
                    BTreeSet::from([name, nested.as_str(), neighbor.as_str()]),
                    Some(nested.as_str()),
                ),
                (
                    "api/file",
                    BTreeSet::from([name, nested.as_str(), neighbor.as_str()]),
                    Some(name),
                ),
                ("file", BTreeSet::from([name]), Some(name)),
                ("api/file", BTreeSet::from([neighbor.as_str()]), None),
                ("web/src/file", BTreeSet::new(), None),
            ] {
                let (mut manifests, mut unreadable) = (BTreeMap::new(), BTreeSet::new());
                let found =
                    package_manifest(source, kind, &root, &known, &mut manifests, &mut unreadable);
                assert_eq!(found.as_deref(), expected, "{kind:?}: {source}");
                assert!(unreadable.is_empty());
            }
            // Even when only the manifest outside the analysis root remains,
            // lookup cannot discover it as a fallback.
            for path in [name, nested.as_str(), neighbor.as_str()] {
                std::fs::remove_file(root.join(path)).unwrap();
            }
            let (mut manifests, mut unreadable) = (BTreeMap::new(), BTreeSet::new());
            assert!(
                package_manifest(
                    "web/src/file",
                    kind,
                    &root,
                    &BTreeSet::new(),
                    &mut manifests,
                    &mut unreadable
                )
                .is_none()
            );
            assert!(unreadable.is_empty());
        }
    }

    #[test]
    fn manifests_outside_known_never_shadow_a_known_parent_or_supply_a_fallback() {
        for (kind, text) in manifest_samples() {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir(tmp.path().join("nested")).unwrap();
            let name = kind.file_name();
            let child = format!("nested/{name}");
            std::fs::write(tmp.path().join(name), text).unwrap();
            std::fs::write(tmp.path().join(&child), text).unwrap();
            for known in [BTreeSet::from([name]), BTreeSet::new()] {
                let (mut manifests, mut unreadable) = (BTreeMap::new(), BTreeSet::new());
                let found = package_manifest(
                    "nested/file",
                    kind,
                    tmp.path(),
                    &known,
                    &mut manifests,
                    &mut unreadable,
                );
                assert_eq!(
                    found.as_deref(),
                    known.contains(name).then_some(name),
                    "{kind:?}"
                );
                assert!(!manifests.contains_key(&child));
                assert!(unreadable.is_empty());
            }
        }
    }

    #[test]
    fn unreadable_known_manifests_block_parent_fallback_for_every_kind() {
        for (kind, text) in manifest_samples() {
            for state in ["missing", "directory", "invalid UTF-8"] {
                let tmp = tempfile::tempdir().unwrap();
                std::fs::create_dir(tmp.path().join("nested")).unwrap();
                let name = kind.file_name();
                let child = format!("nested/{name}");
                std::fs::write(tmp.path().join(name), text).unwrap();
                let child_path = tmp.path().join(&child);
                match state {
                    "directory" => std::fs::create_dir(&child_path).unwrap(),
                    "invalid UTF-8" => std::fs::write(&child_path, [0xff]).unwrap(),
                    _ => {}
                }
                let known = BTreeSet::from([name, child.as_str()]);
                let (mut manifests, mut unreadable) = (BTreeMap::new(), BTreeSet::new());
                for _ in 0..2 {
                    let found = package_manifest(
                        "nested/file",
                        kind,
                        tmp.path(),
                        &known,
                        &mut manifests,
                        &mut unreadable,
                    );
                    assert_eq!(found.as_deref(), Some(child.as_str()), "{kind:?}: {state}");
                    assert!(unreadable.contains(&child), "{kind:?}: {state}");
                    assert_eq!(manifests.get(&child), Some(&None));
                    assert!(!manifests.contains_key(name));
                }
            }
        }
    }

    #[test]
    fn broken_manifests_stop_the_search_instead_of_becoming_markers() {
        for (kind, valid, broken) in [
            (Manifest::Npm, r#"{"name":"owner"}"#, "{"),
            (
                Manifest::Cargo,
                "[package]\nname = 'owner'\n",
                "[dependencies\n",
            ),
            (Manifest::Pub, "name: owner\n", "dependencies: [bad]\n"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir(tmp.path().join("nested")).unwrap();
            let name = kind.file_name();
            let child = format!("nested/{name}");
            std::fs::write(tmp.path().join(name), valid).unwrap();
            std::fs::write(tmp.path().join(&child), broken).unwrap();
            let known = BTreeSet::from([name, child.as_str()]);
            let (mut manifests, mut unreadable) = (BTreeMap::new(), BTreeSet::new());
            let found = package_manifest(
                "nested/file",
                kind,
                tmp.path(),
                &known,
                &mut manifests,
                &mut unreadable,
            );
            assert_eq!(found.as_deref(), Some(child.as_str()));
            assert_eq!(unreadable, BTreeSet::from([child]));
            assert!(!manifests.contains_key(name));
        }
    }

    #[test]
    fn pub_workspace_declarations_are_not_merged_into_the_member_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("packages/member/lib")).unwrap();
        std::fs::write(
            tmp.path().join("pubspec.yaml"),
            concat!(
                "name: workspace\nworkspace: [packages/member]\n",
                "dependencies: {root_only: any}\n",
            ),
        )
        .unwrap();
        let child = "packages/member/pubspec.yaml";
        std::fs::write(
            tmp.path().join(child),
            concat!(
                "name: member\nresolution: workspace\n",
                "dependencies: {member_only: any}\n",
            ),
        )
        .unwrap();
        let known = BTreeSet::from(["pubspec.yaml", child]);
        let (mut manifests, mut unreadable) = (BTreeMap::new(), BTreeSet::new());
        let found = package_manifest(
            "packages/member/lib/file.dart",
            Manifest::Pub,
            tmp.path(),
            &known,
            &mut manifests,
            &mut unreadable,
        );
        assert_eq!(found.as_deref(), Some(child));
        let names = manifests.get(child).unwrap().as_ref().unwrap();
        assert!(names.contains("member_only"));
        assert!(!names.contains("root_only"));
        assert!(!manifests.contains_key("pubspec.yaml"));
        assert!(unreadable.is_empty());
    }

    #[test]
    fn failed_manifest_reads_report_partial_instead_of_undeclared_or_orphaned_imports() {
        let config: Config =
            serde_norway::from_str("version: 1\nmanifests: true\ncomponents:\n  app: {in: src}\n")
                .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        for (kind, source, module) in [
            (Manifest::Npm, "src/file.ts", "dep/sub"),
            (Manifest::Go, "src/file.go", "example.com/dep"),
            (Manifest::Cargo, "src/file.rs", "dep::Thing"),
            (Manifest::Pub, "src/file.dart", "package:dep/api.dart"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let files = [kind.file_name().to_string(), source.to_string()];
            let deps = [Dep {
                file: source.into(),
                from: crate::lang::Unit::from(source),
                line: 1,
                specifier: module.into(),
                target: Target::External(module.into()),
            }];
            let result = undeclared(&config, &map, tmp.path(), &deps, &files);
            assert!(result.violations.is_empty(), "{kind:?}");
            assert_eq!(result.reasons.len(), 1, "{kind:?}: {:?}", result.reasons);
            assert!(
                result.reasons[0].reason.contains("could not be read"),
                "{kind:?}: {:?}",
                result.reasons
            );
            assert!(result.reasons[0].reason.contains(kind.file_name()));
        }
    }
}
