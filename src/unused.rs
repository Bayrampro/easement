//! The `unused` rule: files of components that no chain of imports from an
//! entry point reaches. An agent acting on it deletes the file, so every
//! doubt counts a file as reached.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use regex::Regex;

use crate::check::{RuleId, Violation, file_graph};
use crate::components::ComponentMap;
use crate::config::{Config, TEST_PATTERNS, TESTS};
use crate::scan::{Dep, glob_set};

/// The languages the rule judges, each checked against its indexer (C# and
/// PHP by the CI tests): the index reports a use of another file wherever
/// it happens. Ruby loads by convention, and Kotlin and Swift are read by
/// imports alone.
const FAMILIES: &[(&str, &[&str])] = &[
    ("go", &["go"]),
    (
        "js",
        &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
    ),
    ("py", &["py"]),
    ("rs", &["rs"]),
    ("java", &["java"]),
    ("cs", &["cs"]),
    ("php", &["php"]),
];

/// Files a toolchain or a convention runs without an import.
const CONVENTIONAL: &[&str] = &[
    "**/*.d.ts",
    "**/package-info.java",
    "**/module-info.java",
    "**/conftest.py",
    "**/__main__.py",
    "**/setup.py",
    "**/build.rs",
    "**/src/main.rs",
    "**/src/lib.rs",
    "**/src/bin/**",
    "**/examples/**",
    "**/benches/**",
    "**/tests/**",
];

/// What the rule reads from one scan.
pub struct Input<'a> {
    pub config: &'a Config,
    pub map: &'a ComponentMap,
    pub root: &'a Path,
    /// Imports between checked files, from the index or an adapter.
    pub deps: &'a [Dep],
    /// Imports the checks do not judge but that reach a file all the same:
    /// those in `target_only` and excluded files.
    pub unread_deps: &'a [Dep],
    /// Files the checks do not read: `target_only` and excluded ones.
    pub unread: &'a [String],
    pub paths: &'a [String],
    pub all_files: &'a [String],
    pub uncovered: &'a [String],
}

pub struct Unused {
    pub violations: Vec<Violation>,
    /// Why some files were taken as used without being judged.
    pub reasons: Vec<String>,
}

fn ext(file: &str) -> &str {
    file.rsplit_once('.').map_or("", |(_, e)| e)
}

fn family(file: &str) -> Option<&'static str> {
    let ext = ext(file);
    FAMILIES
        .iter()
        .find(|(_, exts)| exts.contains(&ext))
        .map(|(name, _)| *name)
}

/// The directory of a `/`-separated path, `""` at the root.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

pub fn unused(input: &Input) -> Result<Unused> {
    let config = input.config;
    if config.entry.is_empty() {
        return Ok(Unused {
            violations: Vec::new(),
            reasons: Vec::new(),
        });
    }
    let owned = |patterns: &[&str]| patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    let entry = glob_set(&config.entry, "entry")?;
    let tests = glob_set(&owned(TEST_PATTERNS), "tests")?;
    let conventional = glob_set(&owned(CONVENTIONAL), "conventional entries")?;
    let packaged = package_entries(input.root, input.all_files);

    // A family is judged only where the graph sees all of it: not when an
    // indexer left some of its files out, and not Java beside Kotlin, which
    // uses Java classes of its package without an import.
    let mut skipped: BTreeMap<&str, String> = BTreeMap::new();
    for file in input.uncovered {
        if let Some(f) = family(file) {
            skipped.insert(f, "some of its files were not covered by an indexer".into());
        }
    }
    if input.paths.iter().any(|f| matches!(ext(f), "kt" | "kts")) {
        skipped.insert(
            "java",
            "Kotlin beside it uses Java classes without an import".into(),
        );
    }
    let mut unjudged: BTreeSet<&str> = BTreeSet::new();
    let mut roots: Vec<&str> = input.unread.iter().map(String::as_str).collect();
    let mut candidates = Vec::new();
    for file in input.paths {
        let judged = family(file).filter(|f| !skipped.contains_key(f));
        if judged.is_none() && family(file).is_none() {
            unjudged.insert(ext(file));
        }
        let root = entry.is_match(file)
            || tests.is_match(file)
            || conventional.is_match(file)
            || packaged.contains(file.as_str())
            || go_main(input.root, file)
            || input.map.component_of(file).is_none_or(|c| c == TESTS)
            || judged.is_none();
        if root {
            roots.push(file);
        } else {
            candidates.push(file);
        }
    }

    let mut graph = file_graph(input.deps, input.all_files);
    for (from, out) in file_graph(input.unread_deps, input.all_files) {
        graph.entry(from).or_default().extend(out);
    }
    for (from, to) in side_effect_imports(input.root, input.paths, input.all_files) {
        graph.entry(from).or_default().insert((to, 0));
    }
    let reached = reached_from(roots, &graph, input.paths);

    let violations = candidates
        .into_iter()
        .filter(|f| !reached.contains(f.as_str()))
        .map(|file| Violation {
            rule: RuleId::Unused,
            file: file.clone(),
            line: 1,
            message: "not reachable from any `entry`; if a framework, DI, reflection or a build tool loads it, add it to `entry`".to_string(),
            because: None,
            from: input.map.component_of(file).unwrap_or_default().to_string(),
            to: String::new(),
            target: String::new(),
            specifier: String::new(),
            evidence: vec![(file.clone(), 1)],
        })
        .collect();
    let mut reasons: Vec<String> = skipped
        .into_iter()
        .map(|(f, why)| format!("`unused` did not judge the {f} files: {why}"))
        .collect();
    if !unjudged.is_empty() {
        let exts: Vec<&str> = unjudged.into_iter().collect();
        reasons.push(format!(
            "`unused` cannot judge .{} files, whose import graph misses uses, so it took them as used",
            exts.join(", .")
        ));
    }
    Ok(Unused {
        violations,
        reasons,
    })
}

/// Every file a chain of imports from `roots` reaches. Go compiles a package
/// whole, so reaching one of its files reaches all; Python runs the
/// `__init__.py` of every package above a module.
fn reached_from<'a>(
    roots: Vec<&'a str>,
    graph: &'a BTreeMap<String, BTreeSet<(String, usize)>>,
    paths: &'a [String],
) -> BTreeSet<&'a str> {
    let mut in_dir: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for file in paths {
        in_dir.entry(parent(file)).or_default().push(file);
    }
    let mut packages: BTreeSet<&str> = BTreeSet::new();
    let mut reached: BTreeSet<&'a str> = BTreeSet::new();
    let mut queue: VecDeque<&'a str> = VecDeque::new();
    let visit = |file: &'a str, reached: &mut BTreeSet<&'a str>, queue: &mut VecDeque<&'a str>| {
        if reached.insert(file) {
            queue.push_back(file);
        }
    };
    for root in roots {
        visit(root, &mut reached, &mut queue);
    }
    while let Some(file) = queue.pop_front() {
        for (next, _) in graph.get(file).into_iter().flatten() {
            visit(next, &mut reached, &mut queue);
        }
        let dir = parent(file);
        match ext(file) {
            "go" if packages.insert(dir) => {
                for sibling in in_dir.get(dir).into_iter().flatten() {
                    if sibling.ends_with(".go") {
                        visit(sibling, &mut reached, &mut queue);
                    }
                }
            }
            "py" => {
                let mut dir = dir;
                loop {
                    let init = if dir.is_empty() {
                        "__init__.py".to_string()
                    } else {
                        format!("{dir}/__init__.py")
                    };
                    if let Some(found) = in_dir
                        .get(dir)
                        .and_then(|fs| fs.iter().find(|f| **f == init))
                    {
                        visit(found, &mut reached, &mut queue);
                    }
                    if dir.is_empty() {
                        break;
                    }
                    dir = parent(dir);
                }
            }
            _ => {}
        }
    }
    reached
}

/// Whether a Go file is in `package main`: a program of its own.
fn go_main(root: &Path, file: &str) -> bool {
    file.ends_with(".go")
        && std::fs::read_to_string(root.join(file)).is_ok_and(|text| {
            text.lines()
                .map(str::trim)
                .find(|l| l.starts_with("package "))
                .is_some_and(|l| l == "package main")
        })
}

/// The file a relative JavaScript or TypeScript specifier names, as the
/// bundlers resolve it: the path itself, with an extension, or its index.
fn resolve_js(from_dir: &str, spec: &str, files: &BTreeSet<&str>) -> Option<String> {
    let mut parts: Vec<&str> = if from_dir.is_empty() {
        Vec::new()
    } else {
        from_dir.split('/').collect()
    };
    for segment in spec.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    let base = parts.join("/");
    let exts = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "d.ts"];
    std::iter::once(base.clone())
        .chain(exts.iter().map(|e| format!("{base}.{e}")))
        .chain(exts.iter().map(|e| format!("{base}/index.{e}")))
        .find(|candidate| files.contains(candidate.as_str()))
}

/// Imports for their side effect (`import "./polyfill"`), `require` and
/// literal `import()` of relative paths in JavaScript and TypeScript, which
/// an index may not report as a use.
fn side_effect_imports(
    root: &Path,
    paths: &[String],
    all_files: &[String],
) -> Vec<(String, String)> {
    let pattern = Regex::new(
        r#"(?m)(?:^\s*import\s+|\brequire\s*\(\s*|\bimport\s*\(\s*)["'](\.{1,2}/[^"']+)["']"#,
    )
    .expect("valid regex");
    let files: BTreeSet<&str> = all_files.iter().map(String::as_str).collect();
    let mut found = Vec::new();
    for file in paths.iter().filter(|f| family(f) == Some("js")) {
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        for capture in pattern.captures_iter(&text) {
            if let Some(target) = resolve_js(parent(file), &capture[1], &files) {
                found.push((file.clone(), target));
            }
        }
    }
    found
}

/// The files the `package.json` files name as what a package runs or
/// exports: `main`, `module`, `types`, `bin` and `exports`.
fn package_entries(root: &Path, all_files: &[String]) -> BTreeSet<String> {
    let files: BTreeSet<&str> = all_files.iter().map(String::as_str).collect();
    let mut entries = BTreeSet::new();
    for manifest in all_files
        .iter()
        .filter(|f| *f == "package.json" || f.ends_with("/package.json"))
    {
        let Ok(text) = std::fs::read_to_string(root.join(manifest)) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let mut named = Vec::new();
        for key in [
            "main", "module", "types", "typings", "bin", "exports", "browser",
        ] {
            collect_strings(&json[key], &mut named);
        }
        for path in named {
            let spec = if path.starts_with('.') {
                path
            } else {
                format!("./{path}")
            };
            if let Some(file) = resolve_js(parent(manifest), &spec, &files) {
                entries.insert(file);
            }
        }
    }
    entries
}

fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

/// Patterns of `entry` that match no file of the project, with the files
/// excluded ones count too: a stale entry makes everything below it unused.
pub fn unmatched_entries<'a>(config: &'a Config, files: &[String]) -> Result<Vec<&'a str>> {
    let mut stale = Vec::new();
    for pattern in &config.entry {
        let set = glob_set(std::slice::from_ref(pattern), "entry")?;
        if !files.iter().any(|f| set.is_match(f)) {
            stale.push(pattern.as_str());
        }
    }
    Ok(stale)
}

/// For the tests: a `Target` that names a project file.
#[cfg(test)]
fn internal(path: &str) -> crate::lang::Target {
    crate::lang::Target::Internal(path.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_js_specifiers_like_a_bundler() {
        let files: BTreeSet<&str> = ["src/a.ts", "src/lib/index.ts", "src/p.d.ts"].into();
        assert_eq!(
            resolve_js("src/x", "../a", &files).as_deref(),
            Some("src/a.ts")
        );
        assert_eq!(
            resolve_js("src", "./lib", &files).as_deref(),
            Some("src/lib/index.ts")
        );
        assert_eq!(
            resolve_js("src", "./p", &files).as_deref(),
            Some("src/p.d.ts")
        );
        assert_eq!(resolve_js("", "../../a", &files), None);
    }

    #[test]
    fn go_packages_and_python_packages_come_whole() {
        let paths: Vec<String> = [
            "app/a.go",
            "app/b.go",
            "pkg/__init__.py",
            "pkg/sub/m.py",
            "pkg/sub/__init__.py",
        ]
        .map(String::from)
        .to_vec();
        let graph = BTreeMap::new();
        let reached = reached_from(vec!["app/a.go", "pkg/sub/m.py"], &graph, &paths);
        assert!(reached.contains("app/b.go"));
        assert!(reached.contains("pkg/sub/__init__.py"));
        assert!(reached.contains("pkg/__init__.py"));
        let deps = [Dep {
            file: "x.go".into(),
            line: 1,
            specifier: String::new(),
            target: internal("app/a.go"),
        }];
        assert_eq!(file_graph(&deps, &paths).len(), 1);
    }

    #[test]
    fn finds_side_effect_imports_and_package_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let write = |path: &str, text: &str| {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        };
        write(
            "src/main.ts",
            "import \"./polyfill\";\nconst x = require('./legacy');\n",
        );
        write("src/polyfill.ts", "");
        write("src/legacy.js", "");
        write(
            "package.json",
            r#"{"bin": {"tool": "bin/tool.js"}, "exports": {".": "./src/main.ts"}}"#,
        );
        write("bin/tool.js", "");
        let files: Vec<String> = [
            "src/main.ts",
            "src/polyfill.ts",
            "src/legacy.js",
            "package.json",
            "bin/tool.js",
        ]
        .map(String::from)
        .to_vec();
        let found = side_effect_imports(dir.path(), &files, &files);
        assert_eq!(
            found,
            [
                ("src/main.ts".to_string(), "src/polyfill.ts".to_string()),
                ("src/main.ts".to_string(), "src/legacy.js".to_string()),
            ]
        );
        let entries = package_entries(dir.path(), &files);
        assert!(entries.contains("bin/tool.js") && entries.contains("src/main.ts"));
    }
}
