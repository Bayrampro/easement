use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

/// Writes a stub package for each import specifier into `node_modules`, so the
/// TypeScript indexer sees the module as installed: `lodash/fp` becomes
/// `node_modules/lodash/package.json` and `node_modules/lodash/fp.d.ts`.
fn stub_packages(root: &Path, specifiers: &[&str]) {
    for specifier in specifiers {
        let mut parts = specifier.split('/');
        let first = parts.next().unwrap();
        let (name, rest): (String, Vec<&str>) = if first.starts_with('@') {
            (
                format!("{first}/{}", parts.next().unwrap()),
                parts.collect(),
            )
        } else {
            (first.to_string(), parts.collect())
        };
        let dir = root.join("node_modules").join(&name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0","types":"index.d.ts"}}"#),
        )
        .unwrap();
        let stub = "declare const stub: any;\nexport default stub;\n";
        fs::write(dir.join("index.d.ts"), stub).unwrap();
        if !rest.is_empty() {
            let file = dir.join(format!("{}.d.ts", rest.join("/")));
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, stub).unwrap();
        }
    }
}

/// A scratch copy of a fixture project, so tests can write baselines and edit code.
/// A `modules.txt` in the fixture lists third-party imports to stub as installed.
fn fixture(name: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    copy_dir(&src, tmp.path());
    let root = tmp.path().to_path_buf();
    if let Ok(list) = fs::read_to_string(root.join("modules.txt")) {
        stub_packages(&root, &list.lines().collect::<Vec<_>>());
        fs::remove_file(root.join("modules.txt")).unwrap();
    }
    (tmp, root)
}

/// Where the tests keep indexes, so they do not touch the cache of whoever runs them.
fn test_cache() -> &'static Path {
    static DIR: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| TempDir::new().unwrap()).path()
}

fn easement(root: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    easement_cached(root, args, test_cache())
}

fn easement_cached(root: &Path, args: &[&str], cache: &Path) -> assert_cmd::assert::Assert {
    Command::cargo_bin("easement")
        .unwrap()
        .current_dir(root)
        .env("EASEMENT_CACHE_DIR", cache)
        .args(args)
        .assert()
}

#[test]
fn go_reports_layer_violation() {
    let (_tmp, root) = fixture("go");
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "internal/handlers/books.go:6: handler -> repository is not allowed",
        ))
        .stdout(contains("1 violation(s)"));
}

#[test]
fn ts_resolves_relative_alias_require_and_js_extension() {
    let (_tmp, root) = fixture("ts");
    let out = easement(&root, &["check", "--format", "json"])
        .code(1)
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let pairs: Vec<(String, String, String)> = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["file"].as_str().unwrap().into(),
                v["to"].as_str().unwrap().into(),
                v["target"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            (
                "src/app/checkout.ts".into(),
                "infra".into(),
                "src/infra/db.ts".into()
            ),
            (
                "src/ui/legacy.js".into(),
                "infra".into(),
                "src/infra/db.ts".into()
            ),
        ]
    );
    assert_eq!(report["passed"], false);
    assert_eq!(report["stats"]["internal_deps"], 6);
}

#[test]
fn baseline_is_a_ratchet() {
    let (_tmp, root) = fixture("ts");

    easement(&root, &["baseline"])
        .success()
        .stdout(contains("2 violation(s)"));
    easement(&root, &["check"])
        .success()
        .stdout(contains("2 legalized"));

    // Paying off debt without shrinking the baseline fails the gate.
    let page = root.join("src/ui/legacy.js");
    let fixed = fs::read_to_string(&page)
        .unwrap()
        .replace("const db = require(\"../infra/db\");", "const db = 1;");
    fs::write(&page, fixed).unwrap();
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("Stale baseline entries"));

    easement(&root, &["baseline"])
        .success()
        .stdout(contains("1 violation(s)"));
    easement(&root, &["check"]).success();

    // New debt is never accepted silently.
    fs::write(
        root.join("src/domain/leak.ts"),
        "import { checkout } from \"../app/checkout\";\n",
    )
    .unwrap();
    easement(&root, &["check"]).code(1).stdout(contains(
        "src/domain/leak.ts:1: domain -> app is not allowed",
    ));

    easement(&root, &["check", "--no-baseline"])
        .code(1)
        .stdout(contains("2 violation(s)"));
}

#[test]
fn exclude_skips_files() {
    let (_tmp, root) = fixture("go");
    let config = fs::read_to_string(root.join("easement.yml")).unwrap();
    fs::write(
        root.join("easement.yml"),
        format!("{config}\nexclude: [internal/handlers]\n"),
    )
    .unwrap();
    // The violations of the handlers are gone with their files, and the
    // component left without a file is reported instead.
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("[dependency]").not())
        .stdout(contains("component `handler` matches no file"));
}

#[test]
fn config_errors_exit_with_2() {
    let (_tmp, root) = fixture("go");
    fs::write(
        root.join("easement.yml"),
        "version: 1\ncomponents:\n  a: { in: a }\nrules:\n  a: { may_depend_on: [ghost] }\n",
    )
    .unwrap();
    easement(&root, &["check"])
        .code(2)
        .stderr(contains("unknown component `ghost`"));
    easement(&root, &["check", "-c", "missing.yml"]).code(2);
}

fn violations(root: &Path) -> (Vec<(String, String)>, serde_json::Value) {
    let assert = easement(root, &["check", "--format", "json"]);
    let output = assert.get_output();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "no report ({err}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let pairs = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["file"].as_str().unwrap().to_string(),
                v["target"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    (pairs, report["stats"].clone())
}

#[test]
fn python_resolves_absolute_relative_and_from_imports() {
    let (_tmp, root) = fixture("python");
    let (pairs, stats) = violations(&root);
    assert_eq!(
        pairs,
        vec![
            (
                "src/shop/api/handlers.py".to_string(),
                "src/shop/infra/db.py".to_string()
            ),
            (
                "src/shop/api/views.py".to_string(),
                "src/shop/infra/__init__.py".to_string()
            ),
        ]
    );
    // handlers: shop.domain, shop.domain.order, shop.infra.db, .views; db: ..domain.order;
    // views: shop.infra
    assert_eq!(stats["internal_deps"], 6);
    // dataclasses
    assert_eq!(stats["external_deps"], 1);
}

#[test]
fn java_resolves_by_package_declaration() {
    let (_tmp, root) = fixture("java");
    let (pairs, stats) = violations(&root);
    assert_eq!(
        pairs,
        vec![(
            "src/main/java/com/acme/shop/web/OrderController.java".to_string(),
            "src/main/java/com/acme/shop/infra/OrderRepository.java".to_string()
        )]
    );
    // repository -> Order, controller -> Order (static), controller -> OrderRepository (infra.*)
    assert_eq!(stats["internal_deps"], 3);
}

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn diff_mode_judges_only_changed_lines() {
    let (_tmp, root) = fixture("ts");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    git(&root, &["checkout", "-q", "-b", "feature"]);

    // Legacy violations exist on main, but this change touches nothing.
    easement(&root, &["check"]).code(1);
    easement(&root, &["check", "--diff", "main"])
        .success()
        .stdout(contains("Checking lines changed since main"));

    // A committed new violation in an existing file.
    let checkout = root.join("src/app/checkout.ts");
    let text = fs::read_to_string(&checkout).unwrap();
    fs::write(
        &checkout,
        format!("import * as ui from \"../ui/page\";\n{text}"),
    )
    .unwrap();
    git(&root, &["commit", "-q", "-am", "leak"]);
    // An uncommitted, untracked file with another one.
    fs::write(
        root.join("src/domain/leak.ts"),
        "import { save } from \"../infra/db\";\n",
    )
    .unwrap();

    let out = easement(&root, &["check", "--diff", "main", "--format", "json"])
        .code(1)
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    // A verdict over the changed lines says nothing about the rest of the project.
    assert_eq!(report["confidence"], "partial");
    assert!(
        report["confidence_reasons"][0]
            .as_str()
            .unwrap()
            .contains("changed since main")
    );
    let found: Vec<(String, u64)> = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["file"].as_str().unwrap().into(),
                v["line"].as_u64().unwrap(),
            )
        })
        .collect();
    // checkout.ts -> infra moved to line 4 but is untouched, so it is not reported.
    assert_eq!(
        found,
        vec![
            ("src/app/checkout.ts".to_string(), 1),
            ("src/domain/leak.ts".to_string(), 1),
        ]
    );
    assert_eq!(report["diff"]["base"], "main");
    let new: Vec<&str> = report["stats"]["new_dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert_eq!(new, ["app -> ui", "domain -> infra"]);
    easement(&root, &["check", "--diff", "main"])
        .code(1)
        .stdout(contains(
            "New dependencies between components in this change:\n  app -> ui\n  domain -> infra\n",
        ));
    easement(&root, &["check", "--diff", "main", "--format", "github"])
        .code(1)
        .stdout(contains(
            "::notice title=easement graph::New component dependencies in this change: app -> ui, domain -> infra",
        ));

    easement(&root, &["check", "--diff", "no-such-branch"])
        .code(2)
        .stderr(contains("merge-base"));
}

#[test]
fn public_api_cycles_reasons_and_suppressions() {
    let (_tmp, root) = fixture("rules");
    let out = easement(&root, &["check", "--format", "json"])
        .code(1)
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let found: Vec<(String, u64, String)> = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["file"].as_str().unwrap().to_string(),
                v["line"].as_u64().unwrap(),
                v["rule"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected = [
        ("src/billing/internal/charge.ts", 1, "cycle"),
        ("src/orders/service.ts", 2, "privacy"),
        ("src/ui/page.ts", 4, "dependency"),
        ("src/ui/page.ts", 4, "suppression-reason"),
        ("src/ui/page.ts", 5, "unused-suppression"),
    ];
    assert_eq!(
        found,
        expected
            .iter()
            .map(|(f, l, r)| (f.to_string(), *l, r.to_string()))
            .collect::<Vec<_>>()
    );
    assert_eq!(report["stats"]["suppressed"], 1);
    let violations = report["violations"].as_array().unwrap();
    assert_eq!(
        violations[0]["message"],
        "component dependency cycle: billing -> orders -> billing"
    );
    assert_eq!(
        violations[1]["because"],
        "billing internals change without notice"
    );
    assert_eq!(violations[2]["because"], "the UI talks to orders only");

    // Text output carries the rule id and the reason.
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "src/orders/service.ts:2: orders -> billing: src/billing/internal/charge.ts is not in the public API of billing",
        ))
        .stdout(contains("  because: billing internals change without notice"));

    // Cycles and the other rules are baselined like dependency violations.
    easement(&root, &["baseline"])
        .success()
        .stdout(contains("5 violation(s)"));
    easement(&root, &["check"]).success();
}

#[test]
fn layers_forbidden_externals_metrics_and_strict() {
    let (_tmp, root) = fixture("policy");
    let out = easement(&root, &["check", "--format", "json"])
        .code(1)
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let found: Vec<(String, String, String)> = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["rule"].as_str().unwrap().into(),
                v["file"].as_str().unwrap().into(),
                v["target"].as_str().unwrap().into(),
            )
        })
        .collect();
    let has = |rule: &str, file: &str, target: &str| {
        assert!(
            found.contains(&(rule.into(), file.into(), target.into())),
            "missing {rule} {file} {target} in {found:?}"
        );
    };
    has("dependency", "src/web/page.ts", "src/data/db.ts");
    has("dependency", "src/core/run.ts", "src/web/page.ts");
    has("external", "src/web/page.ts", "moment/locale/ru");
    has("external", "src/core/run.ts", "axios");
    has("metric", "src/core/run.ts", "max_fan_out");
    has("orphan", "src/stray.ts", "src/stray.ts");
    assert_eq!(found.len(), 6, "{found:?}");
    // Layers allow web -> core, core -> data, and common allows shared.
    assert!(!found.iter().any(|(_, _, t)| t == "src/core/run.ts"));
    assert!(!found.iter().any(|(_, _, t)| t == "src/shared/fmt.ts"));
    assert!(!found.iter().any(|(_, _, t)| t == "lodash/fp"));
}

#[test]
fn external_violations_can_be_suppressed() {
    let (_tmp, root) = fixture("policy");
    let page = root.join("src/web/page.ts");
    let text = fs::read_to_string(&page).unwrap();
    fs::write(
        &page,
        text.replace(
            "import moment",
            "// easement:disable-next-line external -- legacy locale\nimport moment",
        ),
    )
    .unwrap();
    easement(&root, &["check"])
        .code(1)
        .stdout(predicates::str::contains("moment").not())
        .stdout(contains("1 suppressed"));
}

fn stdout_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

/// A scratch project made of the given files.
const TSCONFIG: &str = r#"{"compilerOptions":{"target":"es2022","module":"esnext","moduleResolution":"bundler","allowJs":true,"noEmit":true,"skipLibCheck":true},"include":["**/*"],"exclude":["node_modules"]}"#;

/// A scratch project from the given files; the TypeScript indexer needs a
/// `tsconfig.json`, so one is added when there are TypeScript or JavaScript files.
fn project(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let has_ts = files
        .iter()
        .any(|(p, _)| [".ts", ".tsx", ".js"].iter().any(|e| p.ends_with(e)));
    if has_ts && !files.iter().any(|(p, _)| *p == "tsconfig.json") {
        fs::write(tmp.path().join("tsconfig.json"), TSCONFIG).unwrap();
    }
    for (path, text) in files {
        let path = tmp.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let root = tmp.path().to_path_buf();
    (tmp, root)
}

#[test]
fn ci_formats() {
    let (_tmp, root) = fixture("policy");

    let github = stdout_of(easement(&root, &["check", "--format", "github"]).code(1));
    assert!(
        github.contains(
            "::error file=src/web/page.ts,line=2,title=easement dependency::web -> data is forbidden"
        ),
        "{github}"
    );

    let sarif: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["check", "--format", "sarif"]).code(1),
    ))
    .unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    assert!(results.iter().any(|r| {
        r["ruleId"] == "orphan"
            && r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"] == "src/stray.ts"
    }));

    let junit = stdout_of(easement(&root, &["check", "--format", "junit"]).code(1));
    assert!(junit.contains("<testsuite name=\"easement\" tests=\"6\" failures=\"6\">"));
    assert!(junit.contains("type=\"external\""));
}

#[test]
fn init_writes_a_config_that_passes() {
    let (_tmp, root) = project(&[
        (
            "src/web/page.ts",
            "import { run } from '../core/run';\nexport const p = run;\n",
        ),
        (
            "src/core/run.ts",
            "import { q } from '../data/q';\nexport const run = q;\n",
        ),
        ("src/data/q.ts", "export const q = 1;\n"),
        ("tests/t.ts", "export {};\n"),
    ]);
    easement(&root, &["init"])
        .success()
        .stdout(contains("3 components and 2 allowed dependencies"));
    let config = fs::read_to_string(root.join("easement.yml")).unwrap();
    assert!(config.contains("  web: { in: src/web }"), "{config}");
    assert!(
        config.contains("  web: { may_depend_on: [core] }"),
        "{config}"
    );
    assert!(
        config.contains("  core: { may_depend_on: [data] }"),
        "{config}"
    );
    assert!(!config.contains("tests"), "{config}");
    easement(&root, &["check"])
        .success()
        .stdout(contains("passed"));
    // Turning the generated allowance around is a violation.
    fs::write(
        root.join("src/data/q.ts"),
        "import { run } from '../core/run';\nexport const q = run;\n",
    )
    .unwrap();
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("data -> core is not allowed"));
    easement(&root, &["init"])
        .code(2)
        .stderr(contains("already exists"));
    easement(&root, &["init", "--force"]).success();
}

#[test]
fn explain_graph_and_schema() {
    let (_tmp, root) = fixture("policy");

    let explained = stdout_of(easement(&root, &["explain", "src/web/page.ts"]).success());
    assert!(
        explained.contains("component: web (matches `src/web`)"),
        "{explained}"
    );
    assert!(explained.contains("may depend on: core (layers), shared (common)"));
    assert!(explained.contains("forbidden: data"));
    easement(&root, &["explain", "src/stray.ts"])
        .code(2)
        .stderr(contains("belongs to no component"));

    let mermaid = stdout_of(easement(&root, &["graph"]).success());
    assert!(mermaid.starts_with("graph LR\n"), "{mermaid}");
    assert!(
        mermaid.contains("-.->"),
        "the forbidden web -> data edge is dashed: {mermaid}"
    );
    assert!(mermaid.contains("linkStyle"));
    let dot = stdout_of(easement(&root, &["graph", "--format", "dot"]).success());
    assert!(dot.starts_with("digraph easement {"), "{dot}");
    assert!(dot.contains("color=red"));

    let schema: serde_json::Value =
        serde_json::from_str(&stdout_of(easement(&root, &["schema"]).success())).unwrap();
    assert_eq!(schema["type"], "object");
}

/// The keys serde reports as the valid ones, read from its error for an unknown key.
fn config_keys(yaml: &str) -> Vec<String> {
    let (_tmp, root) = project(&[("easement.yml", yaml)]);
    let stderr = easement(&root, &["check"])
        .code(2)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(stderr).unwrap();
    let list = stderr
        .split("expected one of ")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap();
    let mut keys: Vec<String> = list
        .split('`')
        .skip(1)
        .step_by(2)
        .map(String::from)
        .collect();
    keys.sort();
    keys
}

#[test]
fn schema_lists_exactly_the_config_keys() {
    let (_tmp, root) = fixture("policy");
    let schema: serde_json::Value =
        serde_json::from_str(&stdout_of(easement(&root, &["schema"]).success())).unwrap();
    let keys = |value: &serde_json::Value| -> Vec<String> {
        let mut keys: Vec<String> = value["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };
    // `extends` is merged away before the config is read, so serde never lists it.
    let mut top = keys(&schema);
    top.retain(|k| k != "extends");
    assert_eq!(
        top,
        config_keys("version: 1\nbogus: 1\ncomponents:\n  a: { in: a }\n")
    );
    assert_eq!(
        keys(&schema["definitions"]["component"]),
        config_keys("version: 1\ncomponents:\n  a: { in: a, bogus: 1 }\n")
    );
    assert_eq!(
        keys(&schema["definitions"]["rule"]),
        config_keys("version: 1\ncomponents:\n  a: { in: a }\nrules:\n  a: { bogus: 1 }\n")
    );
}

#[test]
fn extends_shares_a_config_between_packages() {
    let (_tmp, root) = project(&[
        (
            "shared/easement.base.yml",
            "version: 1\ncomponents:\n  ui: { in: src/ui }\n  db: { in: src/db }\nrules:\n  ui: { because: the UI never touches the database }\n",
        ),
        ("easement.yml", "extends: shared/easement.base.yml\n"),
        ("src/ui/a.ts", "import '../db/q';\n"),
        ("src/db/q.ts", "export {};\n"),
    ]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("ui -> db is not allowed"))
        .stdout(contains("because: the UI never touches the database"));
}

#[test]
fn watch_checks_again_after_a_change() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  a: { in: src/a }\n  b: { in: src/b }\n",
        ),
        ("src/a/x.ts", "export {};\n"),
        ("src/b/y.ts", "export {};\n"),
    ]);
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("easement"))
        .current_dir(&root)
        .args(["check", "--watch"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let wait_for = |text: &str| -> bool {
        loop {
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(line) if line.contains(text) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    };
    let passed = wait_for("easement passed");
    if passed {
        fs::write(root.join("src/a/x.ts"), "import '../b/y';\n").unwrap();
    }
    let failed = passed && wait_for("a -> b is not allowed");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(passed, "the first check did not pass");
    assert!(failed, "the change was not picked up");
}

/// scip-ruby has no Linux arm64 release (sourcegraph/scip-ruby#228), so the Ruby tests
/// skip themselves there. Anywhere else a missing indexer is still an error.
fn no_scip_ruby_here() -> bool {
    cfg!(all(target_os = "linux", target_arch = "aarch64")) && which::which("scip-ruby").is_err()
}

/// Every layered fixture has one forbidden import, web -> infra, and allowed ones
/// that must be seen: the count of internal dependencies proves the adapter resolved them.
fn layered(name: &str, file: &str, target: &str, internal_deps: u64) {
    if name == "ruby" && no_scip_ruby_here() {
        return;
    }
    let (_tmp, root) = fixture(name);
    if name == "rust" {
        // Not stored in the fixture: tools that walk the repository would take it for a crate.
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
    }
    if name == "php" {
        // scip-php reads the autoloader, so the (empty) dependencies have to be installed.
        // `which` finds composer.bat on Windows, which `Command` alone does not.
        let composer = which::which("composer").expect("composer is needed for the PHP indexer");
        let status = std::process::Command::new(composer)
            .args(["install", "--no-interaction", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("composer runs");
        assert!(status.success(), "composer install");
    }
    let (found, stats) = violations(&root);
    assert_eq!(
        found,
        vec![(file.to_string(), target.to_string())],
        "{name}"
    );
    assert_eq!(stats["internal_deps"], internal_deps, "{name}");
}

#[test]
fn rust_resolves_the_module_tree() {
    // `use super::*` in the inline test module adds nothing; `use std::fmt` is external.
    layered("rust", "src/web/mod.rs", "src/infra/repo.rs", 8);
}

#[test]
fn csharp_resolves_namespaces() {
    layered(
        "csharp",
        "Web/OrderController.cs",
        "Infra/OrderRepository.cs",
        3,
    );
}

#[test]
fn ruby_resolves_constants() {
    layered(
        "ruby",
        "lib/shop/web/order_controller.rb",
        "lib/shop/infra/order_repository.rb",
        3,
    );
}

#[test]
fn kotlin_resolves_packages_and_declarations() {
    layered(
        "kotlin",
        "src/main/kotlin/com/acme/shop/web/OrderController.kt",
        "src/main/kotlin/com/acme/shop/infra/OrderRepository.kt",
        3,
    );
}

#[test]
fn php_resolves_psr4_names() {
    layered(
        "php",
        "src/Web/OrderController.php",
        "src/Infra/OrderRepository.php",
        3,
    );
}

#[test]
fn swift_resolves_package_modules() {
    layered(
        "swift",
        "Sources/Web/OrderController.swift",
        "Sources/Infra",
        3,
    );
}

fn reports(root: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let mut all = vec!["check", "--format", "json"];
    all.extend_from_slice(args);
    let out = easement(root, &all).get_output().stdout.clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    report["violations"].as_array().unwrap().clone()
}

fn find<'a>(found: &'a [serde_json::Value], file: &str, rule: &str) -> &'a serde_json::Value {
    found
        .iter()
        .find(|v| v["file"] == file && v["rule"] == rule)
        .unwrap_or_else(|| panic!("no {rule} in {file}: {found:?}"))
}

#[test]
fn instances_importers_priority_reasons_and_unowned() {
    let (_tmp, root) = fixture("structure");
    let found = reports(&root, &[]);
    assert_eq!(found.len(), 7, "{found:?}");

    // Instances of `pages` must not import each other.
    let isolation = find(&found, "src/pages/contacts/index.ts", "isolation");
    assert!(
        isolation["message"]
            .as_str()
            .unwrap()
            .contains("src/pages/contacts -> src/pages/segments")
    );
    assert_eq!(isolation["because"], "pages ship as separate bundles");

    // `ee` is imported only from wiring, even though `common` lets everyone in.
    let importers = find(&found, "src/pages/segments/list.ts", "dependency");
    assert!(
        importers["message"]
            .as_str()
            .unwrap()
            .contains("may be imported only from wiring")
    );
    assert_eq!(importers["because"], "ee is licensed separately");

    // The priority of `tests` beats the longer pattern of `billing`.
    let test = find(&found, "src/billing/feature/area/ui.test.ts", "dependency");
    assert_eq!(test["from"], "tests");

    // A reason on the listed name beats the reason of the rule.
    assert_eq!(
        find(&found, "src/core/util.ts", "dependency")["because"],
        "core never knows enterprise code"
    );
    let externals: Vec<_> = found.iter().filter(|v| v["rule"] == "external").collect();
    assert_eq!(externals.len(), 2);
    let because =
        |target: &str| externals.iter().find(|v| v["target"] == target).unwrap()["because"].clone();
    assert_eq!(because("moment"), "moment is deprecated, use date-fns");
    assert_eq!(because("lodash/fp"), "core stays dependency-free");

    // Only the file outside every component and outside `unowned` is an orphan.
    let orphans: Vec<_> = found.iter().filter(|v| v["rule"] == "orphan").collect();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0]["file"], "src/stray.ts");
}

#[test]
fn isolation_can_be_suppressed() {
    let (_tmp, root) = fixture("structure");
    let file = root.join("src/pages/contacts/index.ts");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        format!("// easement:disable-next-line isolation -- shared list\n{text}"),
    )
    .unwrap();
    let found = reports(&root, &[]);
    assert!(!found.iter().any(|v| v["rule"] == "isolation"), "{found:?}");
}

#[test]
fn component_and_scope_narrow_the_report() {
    let (_tmp, root) = fixture("structure");

    let core = reports(&root, &["--component", "core"]);
    assert_eq!(core.len(), 3, "{core:?}");
    assert!(core.iter().all(|v| v["file"] == "src/core/util.ts"));

    let pages = reports(&root, &["--scope", "src/pages"]);
    let files: Vec<_> = pages.iter().map(|v| v["file"].as_str().unwrap()).collect();
    assert_eq!(
        files,
        ["src/pages/contacts/index.ts", "src/pages/segments/list.ts"]
    );

    // Both restrictions must hold.
    assert!(reports(&root, &["--component", "core", "--scope", "src/pages"]).is_empty());

    // The counters follow the scope, the cycles and limits still see everything.
    let out = easement(
        &root,
        &["check", "--format", "json", "--component", "pages"],
    )
    .get_output()
    .stdout
    .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(report["stats"]["files"], 2);
    assert_eq!(report["stats"]["internal_deps"], 3);

    easement(&root, &["check", "--component", "ghost"])
        .code(2)
        .stderr(contains("unknown component `ghost`"));
}

#[test]
fn scoped_runs_do_not_report_stale_entries_of_other_files() {
    let (_tmp, root) = fixture("structure");
    easement(&root, &["baseline"]).success();
    // Fix a violation inside `pages`: its baseline entry is stale there, not in `core`.
    fs::write(
        root.join("src/pages/segments/list.ts"),
        "export const list = 1;\n",
    )
    .unwrap();
    easement(&root, &["check", "--component", "core"])
        .success()
        .stdout(contains("0 stale"));
    easement(&root, &["check", "--component", "pages"])
        .code(1)
        .stdout(contains("1 stale"));
}

#[test]
fn required_imports_modules_and_files_are_checked_per_directory() {
    let (_tmp, root) = fixture("required");
    let found = reports(&root, &[]);
    let got: Vec<(String, String)> = found
        .iter()
        .map(|v| {
            assert_eq!(v["rule"], "required");
            (
                v["file"].as_str().unwrap().into(),
                v["target"].as_str().unwrap().into(),
            )
        })
        .collect();
    let has = |file: &str, target: &str| {
        assert!(
            got.contains(&(file.into(), target.into())),
            "{file} {target}: {got:?}"
        );
    };
    // `books` has everything; `users` imports shared from one file but misses the rest.
    has("src/api/users/resources/b.ts", "external goverter");
    has("src/api/users/resources/b.ts", "file README.md");
    has("src/api/orders/resources/d.ts", "import shared");
    has("src/api/orders/resources/d.ts", "external goverter");
    has("src/api/orders/resources/d.ts", "file README.md");
    assert_eq!(got.len(), 5, "{got:?}");
    assert_eq!(
        found[0]["because"], "mappers are generated with goverter",
        "{found:?}"
    );

    // Adding what is missing clears the violations of that directory.
    let users = root.join("src/api/users/resources");
    fs::write(users.join("README.md"), "# users\n").unwrap();
    fs::write(
        users.join("b.ts"),
        "import goverter from \"goverter\";\nexport const b = goverter;\n",
    )
    .unwrap();
    let found = reports(&root, &[]);
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(
        found
            .iter()
            .all(|v| v["file"] == "src/api/orders/resources/d.ts")
    );
}

#[test]
fn forbidden_files_and_required_directives_are_reported() {
    let (_tmp, root) = fixture("files");
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "src/ui/a.css:1: ui must not contain src/ui/a.css (forbid_file *.css)",
        ))
        .stdout(contains("because: styles live in the design system"))
        .stdout(contains(
            "src/conv/bad/m.ts:1: src/conv/bad (conv) must contain a comment or text matching `goverter:converter`",
        ))
        .stdout(contains("src/conv/ok").not())
        .stdout(contains("2 violation(s)"));
}

#[test]
fn tests_auto_gives_test_files_their_own_allow_list() {
    let (_tmp, root) = fixture("testsauto");
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "src/app/x.test.ts:3: tests -> db is not allowed (imports src/db/d.ts)",
        ))
        .stdout(contains(
            "because: tests reach the database only through testhelper",
        ))
        .stdout(contains("1 violation(s)"));
    easement(&root, &["explain", "src/app/x.test.ts"])
        .success()
        .stdout(contains("component: tests"));
}

#[test]
fn symbols_are_checked_through_imports() {
    let (_tmp, root) = fixture("symbols");
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "internal/handlers/books.go:9: handler must not use db.Client [symbol]",
        ))
        .stdout(contains("because: use db.Scoped so every query is tenant-bound"))
        .stdout(contains(
            "internal/handlers/books.go:12: handler -> db: Raw is not in the public symbols of db (Scoped, Tx)",
        ))
        .stdout(contains("because: handlers get a scoped client, never the raw one"))
        .stdout(contains(
            "internal/handlers/books.go:9: handler -> db: Client is not in the public symbols of db",
        ))
        // Scoped and Tx are public; the Raw on line 10 is suppressed.
        .stdout(contains("books.go:10").not())
        .stdout(contains("3 violation(s)"));
}

#[test]
fn forbid_symbol_catches_every_way_to_use_a_type() {
    let (_tmp, root) = fixture("symbol_uses");
    // One file per way of using db.Client, and a file that uses it twice: every use is reported.
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "internal/handlers/field.go:6: handler must not use db.Client",
        ))
        .stdout(contains(
            "internal/handlers/param.go:5: handler must not use db.Client",
        ))
        .stdout(contains(
            "internal/handlers/local.go:6: handler must not use db.Client",
        ))
        // A method call on a value of the type, without the type being spelled out.
        .stdout(contains(
            "internal/handlers/chained.go:6: handler must not use db.Client",
        ))
        .stdout(contains(
            "internal/handlers/held.go:7: handler must not use db.Client",
        ))
        .stdout(contains(
            "internal/handlers/twice.go:6: handler must not use db.Client",
        ))
        .stdout(contains(
            "internal/handlers/twice.go:7: handler must not use db.Client",
        ))
        .stdout(contains("allowed.go").not())
        .stdout(contains("7 violation(s)"));
}

#[test]
fn ruby_symbols_are_constants_and_methods() {
    if no_scip_ruby_here() {
        return;
    }
    let (_tmp, root) = fixture("ruby_symbols");
    easement(&root, &["check"])
        .code(1)
        // A gem's constant, which Sorbet knows from the RBI in sorbet/rbi.
        .stdout(contains(
            "lib/shop/mutators/publish.rb:8: mutators must not use ActiveRecord.Rollback [symbol]",
        ))
        .stdout(contains(
            "because: a mutator returns its failure, it does not roll back",
        ))
        // One method of a class, called on the class.
        .stdout(contains(
            "lib/shop/web/controller.rb:18: web must not use Shop::Db::Client.connect [symbol]",
        ))
        // The class in a signature, a method called on a value of it, and a class method.
        .stdout(contains(
            "lib/shop/web/controller.rb:11: web -> db: Client is not in the public symbols of db (Scoped)",
        ))
        .stdout(contains(
            "lib/shop/web/controller.rb:13: web -> db: Client is not in the public symbols of db (Scoped)",
        ))
        .stdout(contains(
            "lib/shop/web/controller.rb:18: web -> db: Client is not in the public symbols of db (Scoped)",
        ))
        // Scoped is public, Shop::Db is only the namespace, and web may raise a rollback.
        .stdout(contains("controller.rb:17").not())
        .stdout(contains("controller.rb:19").not())
        .stdout(contains("5 violation(s)"));
}

#[test]
fn uncovered_files_are_listed_and_the_verdict_says_it_is_partial() {
    let (_tmp, root) = fixture("uncovered");
    easement(&root, &["check"])
        .success()
        .stdout(contains("Not covered by an indexer"))
        .stdout(contains("  tools/gen.go"))
        .stdout(contains("  tools/other.go"))
        .stdout(contains("2 files not covered by an indexer"));
    let report: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["check", "--format", "json"]).success(),
    ))
    .unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["confidence"], "partial");
    assert_eq!(
        report["confidence_reasons"][0],
        "2 source files were not covered by an indexer, so imports in or of them were not checked"
    );
}

#[test]
fn uncovered_ok_accepts_known_gaps() {
    let (_tmp, root) = fixture("uncovered");
    let config = fs::read_to_string(root.join("easement.yml")).unwrap();
    fs::write(
        root.join("easement.yml"),
        format!(
            "{config}uncovered_ok:\n  - {{ name: 'tools/**', because: build helpers with no go.mod on purpose }}\n"
        ),
    )
    .unwrap();
    easement(&root, &["check"])
        .success()
        .stdout(contains("Not covered").not())
        .stdout(contains("not covered by an indexer").not());
    let report: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["check", "--format", "json"]).success(),
    ))
    .unwrap();
    assert_eq!(report["confidence"], "full");
    assert!(report["confidence_reasons"].as_array().unwrap().is_empty());
}

#[test]
fn uncovered_ok_takes_a_list_of_globs_under_one_reason() {
    let (_tmp, root) = fixture("uncovered");
    let config = fs::read_to_string(root.join("easement.yml")).unwrap();
    fs::write(
        root.join("easement.yml"),
        format!(
            "{config}uncovered_ok:\n  - {{ name: ['tools/gen.go', 'tools/other.go'], because: build helpers }}\n"
        ),
    )
    .unwrap();
    easement(&root, &["check"])
        .success()
        .stdout(contains("not covered by an indexer").not());
    // One glob of the list is not enough for the other file.
    fs::write(
        root.join("easement.yml"),
        format!("{config}uncovered_ok:\n  - {{ name: ['tools/gen.go'] }}\n"),
    )
    .unwrap();
    easement(&root, &["check"])
        .success()
        .stdout(contains("  tools/other.go"))
        .stdout(contains("  tools/gen.go").not());
}

#[test]
fn doctor_reports_the_indexer_of_each_language() {
    let (_tmp, root) = fixture("uncovered");
    easement(&root, &["doctor"])
        .success()
        .stdout(contains("go: scip-go"))
        .stdout(contains("ok"));
    let report: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["doctor", "--format", "json"]).success(),
    ))
    .unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["languages"][0]["language"], "go");
    assert_eq!(report["languages"][0]["binary"], "scip-go");
    assert_eq!(report["languages"][0]["found"], true);
}

#[test]
fn doctor_names_a_missing_indexer_and_how_to_install_it() {
    let (_tmp, root) = fixture("uncovered");
    let run = |args: &[&str]| {
        Command::cargo_bin("easement")
            .unwrap()
            .current_dir(&root)
            .env("PATH", "")
            .args(args)
            .assert()
    };
    run(&["doctor"])
        .code(1)
        .stdout(contains("go: scip-go not found"))
        .stdout(contains(
            "mise use -g go:github.com/scip-code/scip-go/cmd/scip-go",
        ))
        .stdout(contains("[tools]"))
        .stdout(contains(
            "\"go:github.com/scip-code/scip-go/cmd/scip-go\" = \"latest\"",
        ));
    let report: serde_json::Value =
        serde_json::from_str(&stdout_of(run(&["doctor", "--format", "json"]).code(1))).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["languages"][0]["found"], false);
}

#[test]
fn doctor_says_when_dependencies_are_not_installed() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  app: { in: src }\n",
        ),
        (
            "package.json",
            r#"{ "dependencies": { "left-pad": "1.0.0" } }"#,
        ),
        ("src/a.ts", "export const a = 1;\n"),
    ]);
    easement(&root, &["doctor"])
        .code(1)
        .stdout(contains("dependencies are not installed"));
}

#[test]
fn go_tags_bring_files_behind_a_build_constraint_into_the_index() {
    let (_tmp, root) = fixture("gotags");
    // The race file is not built without its tag: no indexer reads it.
    easement(&root, &["check"])
        .success()
        .stdout(contains("  pkg/race.go"))
        .stdout(contains("1 files not covered by an indexer"));
    let config = fs::read_to_string(root.join("easement.yml")).unwrap();
    fs::write(
        root.join("easement.yml"),
        format!("{config}go:\n  tags: [race]\n"),
    )
    .unwrap();
    // Same files, other tags: the cached index of the first run must not be used.
    easement(&root, &["check"])
        .success()
        .stdout(contains("not covered").not());
}

#[test]
fn a_narrowed_run_says_it_judged_only_a_part() {
    let (_tmp, root) = fixture("symbol_uses");
    let json = |args: &[&str]| -> serde_json::Value {
        let mut all = vec!["check", "--format", "json"];
        all.extend_from_slice(args);
        let out = easement(&root, &all).get_output().stdout.clone();
        serde_json::from_slice(&out).unwrap()
    };
    let whole = json(&[]);
    assert_eq!(whole["confidence"], "full");
    let scoped = json(&["--scope", "internal/db"]);
    assert_eq!(scoped["confidence"], "partial");
    assert_eq!(
        scoped["confidence_reasons"][0],
        "the report is limited to path internal/db, the rest of the project was not judged"
    );
    easement(&root, &["check", "--scope", "internal/db"])
        .success()
        .stdout(contains("limited to path internal/db"));
    let by_component = json(&["--component", "db", "--scope", "internal"]);
    assert_eq!(
        by_component["confidence_reasons"][0],
        "the report is limited to component db, path internal, the rest of the project was not judged"
    );
}

#[test]
fn suggest_patch_drops_unused_allowances_and_keeps_the_rest() {
    let config = "version: 1
# who may use whom
components:
  app: { in: src/app }
  lib: { in: src/lib }
  old: { in: src/old }

rules:
  app:
    may_depend_on:
      - lib # the only one used
      - { name: old, because: was needed once }
";
    let (_tmp, root) = project(&[
        ("easement.yml", config),
        (
            "src/app/a.ts",
            "import { l } from \"../lib/l\";\nexport const a = l;\n",
        ),
        ("src/lib/l.ts", "export const l = 1;\n"),
        ("src/old/o.ts", "export const o = 1;\n"),
    ]);
    let diff = stdout_of(easement(&root, &["suggest", "--patch"]).success());
    assert!(
        diff.contains("--- a/easement.yml\n+++ b/easement.yml\n"),
        "{diff}"
    );
    assert!(
        diff.contains("-      - { name: old, because: was needed once }"),
        "{diff}"
    );
    assert!(!diff.contains("-      - lib"), "{diff}");

    fs::write(root.join("fix.patch"), &diff).unwrap();
    Command::new("git")
        .args(["apply", "fix.patch"])
        .current_dir(&root)
        .assert()
        .success();
    let fixed = fs::read_to_string(root.join("easement.yml")).unwrap();
    assert!(fixed.contains("# who may use whom") && fixed.contains("- lib # the only one used"));
    assert!(!fixed.contains("old,"), "{fixed}");
    easement(&root, &["suggest"])
        .success()
        .stdout(contains("never imported").not());
    // Nothing left to remove: an empty patch.
    assert_eq!(
        stdout_of(easement(&root, &["suggest", "--patch"]).success()),
        ""
    );
}

#[test]
fn suggest_check_fails_on_unused_allowances_only() {
    let config = |allow: &str| {
        format!(
            "version: 1
components:
  app: {{ in: src/app }}
  lib: {{ in: src/lib }}
  old: {{ in: src/old }}

rules:
  app:
    may_depend_on: [{allow}]
"
        )
    };
    let files = |config: String| {
        project(&[
            ("easement.yml", &config),
            (
                "src/app/a.ts",
                "import { l } from \"../lib/l\";\nexport const a = l;\n",
            ),
            ("src/lib/l.ts", "export const l = 1;\n"),
            ("src/old/o.ts", "export const o = 1;\n"),
        ])
    };
    let (_tmp, root) = files(config("lib, old"));
    easement(&root, &["suggest", "--check"])
        .code(1)
        .stdout(contains("app -> old"));
    // Without --check the same report is only informative.
    easement(&root, &["suggest"]).success();
    easement(&root, &["suggest", "--check", "--patch"]).code(2);
    let (_tmp, root) = files(config("lib"));
    easement(&root, &["suggest", "--check"]).success();
}

#[test]
fn suggest_reports_what_the_graph_says_about_the_config() {
    let (_tmp, root) = fixture("ts");
    easement(&root, &["suggest"])
        .success()
        .stdout(contains("Imported but not allowed"))
        .stdout(contains("app: infra (1 import(s))"));
}

#[test]
fn completions_cover_every_shell_and_subcommand() {
    let tmp = TempDir::new().unwrap();
    for (shell, marker) in [
        ("bash", "_easement()"),
        ("zsh", "#compdef easement"),
        ("fish", "complete -c easement"),
        ("powershell", "Register-ArgumentCompleter"),
        ("elvish", "edit:completion:arg-completer"),
    ] {
        let script = stdout_of(easement(tmp.path(), &["completions", shell]).success());
        assert!(script.contains(marker), "{shell}: {script}");
        assert!(script.contains("baseline"), "{shell} lacks subcommands");
    }
    easement(tmp.path(), &["completions", "tcsh"])
        .code(2)
        .stderr(contains("invalid value"));
}

#[test]
fn color_follows_the_flag_and_the_environment() {
    let (_tmp, root) = fixture("java");
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut cmd = Command::cargo_bin("easement").unwrap();
        cmd.current_dir(&root).args(args);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        stdout_of(cmd.assert().code(1))
    };
    // Piped output carries no escape codes by default.
    assert!(!run(&["check"], &[]).contains('\x1b'));
    // `--color always` forces them; `NO_COLOR` and `--color never` remove them.
    assert!(run(&["check", "--color", "always"], &[]).contains('\x1b'));
    assert!(!run(&["check", "--color", "never"], &[("CLICOLOR_FORCE", "1")]).contains('\x1b'));
    assert!(!run(&["check"], &[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")]).contains('\x1b'));
    // The structured formats are never coloured.
    assert!(!run(&["check", "--format", "json", "--color", "always"], &[]).contains('\x1b'));
}

#[test]
fn readme_mentions_every_config_key_of_the_schema() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../easement.schema.json")).unwrap();
    let readme = include_str!("../README.md");
    let mut missing = Vec::new();
    let mut keys = |properties: &serde_json::Value| {
        for key in properties["properties"].as_object().unwrap().keys() {
            if !readme.contains(&format!("{key}:")) && !readme.contains(&format!("`{key}`")) {
                missing.push(key.clone());
            }
        }
    };
    keys(&schema);
    keys(&schema["definitions"]["component"]);
    keys(&schema["definitions"]["rule"]);
    assert!(missing.is_empty(), "README never mentions: {missing:?}");
}

/// What a run says about the code, without how the indexes were obtained.
fn verdict(root: &Path, cache: &Path) -> (serde_json::Value, serde_json::Value) {
    let out = easement_cached(root, &["check", "--format", "json"], cache);
    let report: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("a JSON report");
    (report["violations"].clone(), report["stats"].clone())
}

#[test]
fn another_checkout_of_the_same_code_reuses_the_indexes() {
    let cache = TempDir::new().unwrap();
    let checkout = |name: &str| {
        let (tmp, root) = fixture(name);
        if name == "rust" {
            // Not stored in the fixture, see `layered`.
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .unwrap();
        }
        (tmp, root)
    };
    for name in ["go", "ts", "python", "rust", "java"] {
        let (_first_tmp, first) = checkout(name);
        let (violations, stats) = verdict(&first, cache.path());
        assert!(
            stats["indexed"].as_u64().unwrap_or(0) >= 1,
            "{name}: {stats}"
        );
        // A copy is another path with other modification times.
        let (_second_tmp, second) = checkout(name);
        let (again, cached) = verdict(&second, cache.path());
        assert!(cached.get("indexed").is_none(), "{name}: {cached}");
        assert_eq!(cached["from_cache"], stats["indexed"], "{name}");
        assert_eq!(again, violations, "{name}");
        assert_eq!(cached["internal_deps"], stats["internal_deps"], "{name}");
    }
}

#[test]
fn a_touched_file_hits_the_cache_and_a_changed_file_above_the_project_misses_it() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  pkg: { in: pkg }\n",
        ),
        ("base.json", "{\"compilerOptions\": {}}\n"),
        (
            "pkg/tsconfig.json",
            "{\"extends\": \"../base.json\", \"include\": [\"src\"]}\n",
        ),
        ("pkg/src/a.ts", "export const a = 1;\n"),
    ]);
    let cache = TempDir::new().unwrap();
    let first = verdict(&root, cache.path()).1["indexed"].clone();
    assert!(first.as_u64().unwrap_or(0) >= 1, "{first}");
    // The same bytes with a new modification time: the content is what counts.
    let file = root.join("pkg/src/a.ts");
    fs::write(&file, fs::read(&file).unwrap()).unwrap();
    assert_eq!(verdict(&root, cache.path()).1["from_cache"], first);
    // The project's `tsconfig.json` extends a file that sits above it.
    fs::write(
        root.join("base.json"),
        "{\"compilerOptions\": {\"strict\": true}}\n",
    )
    .unwrap();
    let after = verdict(&root, cache.path()).1;
    assert!(after["indexed"].as_u64().unwrap_or(0) >= 1, "{after}");
}

#[test]
fn target_only_files_are_imported_but_never_checked_as_sources() {
    let config = |key: &str| {
        format!(
            "version: 1
components:
  app: {{ in: src/app }}
  generated: {{ in: src/generated }}
rules:
  app: {{ may_depend_on: [] }}
  generated: {{ may_depend_on: [] }}
{key}: ['**/*.gen.ts']
"
        )
    };
    let files = |config: String| {
        project(&[
            ("easement.yml", &config),
            ("tsconfig.json", "{\"include\": [\"src\"]}\n"),
            (
                "src/app/a.ts",
                "import { api } from \"../generated/api.gen\";\nexport const a = api;\n",
            ),
            (
                "src/generated/api.gen.ts",
                "import { a } from \"../app/a\";\nexport const api = 1;\nexport const b = a;\n",
            ),
        ])
    };
    let pairs = |root: &Path| -> Vec<(String, String)> {
        let out = easement(root, &["check", "--format", "json"]);
        let report: serde_json::Value =
            serde_json::from_slice(&out.get_output().stdout).expect("a JSON report");
        report["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["file"].as_str().unwrap().to_string(),
                    v["to"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    // The import of the generated file is seen and judged; its own import of the app is not.
    let (_tmp, root) = files(config("target_only"));
    assert_eq!(
        pairs(&root),
        [("src/app/a.ts".to_string(), "generated".to_string())]
    );
    // `exclude` hides the file altogether: importing it fails as `excluded`,
    // and the component that owned it matches no file.
    let (_tmp, root) = files(config("exclude"));
    assert_eq!(
        pairs(&root),
        [
            ("easement.yml".to_string(), String::new()),
            ("src/app/a.ts".to_string(), String::new())
        ]
    );
}

#[test]
fn a_change_to_one_language_reuses_the_index_of_the_other() {
    // Go and TypeScript share the project root, as in a repository with a Go backend and a
    // frontend: each index is keyed by the files its indexer reads.
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  api: { in: api }\n  web: { in: web }\n",
        ),
        ("go.mod", "module example.com/shop\n\ngo 1.22\n"),
        ("api/api.go", "package api\n\nconst Name = \"shop\"\n"),
        ("web/a.ts", "export const a = 1;\n"),
        ("README.md", "# shop\n"),
    ]);
    let cache = TempDir::new().unwrap();
    let stats = || verdict(&root, cache.path()).1;
    assert_eq!(stats()["indexed"], 2);
    let edit = |path: &str| {
        let file = root.join(path);
        let text = fs::read_to_string(&file).unwrap();
        fs::write(&file, format!("{text}\n// changed\n")).unwrap();
    };
    // Neither indexer reads the README.
    edit("README.md");
    let after = stats();
    assert!(after.get("indexed").is_none(), "{after}");
    assert_eq!(after["from_cache"], 2);
    edit("web/a.ts");
    let after = stats();
    assert_eq!(
        (after["indexed"].clone(), after["from_cache"].clone()),
        (1.into(), 1.into())
    );
    edit("api/api.go");
    let after = stats();
    assert_eq!(
        (after["indexed"].clone(), after["from_cache"].clone()),
        (1.into(), 1.into())
    );
}

#[test]
fn cache_only_runs_no_indexer_and_exits_3_on_a_miss() {
    let (_tmp, root) = fixture("go");
    let cache = TempDir::new().unwrap();
    // Nothing cached yet: no indexer runs, and the miss has its own exit code.
    easement_cached(&root, &["check", "--cache-only"], cache.path())
        .code(3)
        .stderr(contains("not cached"));
    // The fixture has violations: exit 1 with the index read from the cache.
    easement_cached(&root, &["check"], cache.path()).code(1);
    let out = easement_cached(
        &root,
        &["check", "--cache-only", "--format", "json"],
        cache.path(),
    )
    .code(1);
    let report: serde_json::Value = serde_json::from_str(&stdout_of(out)).unwrap();
    assert_eq!(report["stats"]["from_cache"], 1);
    easement_cached(
        &root,
        &["check", "--cache-only", "--no-cache"],
        cache.path(),
    )
    .code(2);
}

#[test]
fn indexes_are_cached_until_the_project_changes() {
    let (_tmp, root) = fixture("go");
    let cache = TempDir::new().unwrap();
    let stats = |extra: &[&str]| -> serde_json::Value {
        let mut args = vec!["check", "--format", "json"];
        args.extend_from_slice(extra);
        let out = easement_cached(&root, &args, cache.path()).code(1);
        let report: serde_json::Value = serde_json::from_str(&stdout_of(out)).unwrap();
        report["stats"].clone()
    };
    let first = stats(&[]);
    assert_eq!(first["indexed"], 1);
    assert!(first.get("from_cache").is_none());

    // Nothing changed: the index comes from the cache, and the verdict is the same.
    let second = stats(&[]);
    assert!(second.get("indexed").is_none());
    assert_eq!(second["from_cache"], 1);
    assert_eq!(second["internal_deps"], first["internal_deps"]);

    // --no-cache runs the indexer again.
    assert_eq!(stats(&["--no-cache"])["indexed"], 1);

    // A changed file misses the cache.
    let file = root.join("internal/models/book.go");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("{text}\n// changed\n")).unwrap();
    assert_eq!(stats(&[])["indexed"], 1);
    assert_eq!(stats(&[])["from_cache"], 1);
}

/// A project whose `ghost` component points at a directory that is gone.
fn ghost_project() -> (TempDir, PathBuf) {
    project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  app: { in: src/app }\n  ghost: { in: src/renamed_away }\nrules:\n  app: { may_depend_on: [ghost] }\n",
        ),
        ("src/app/Main.kt", "package app\n"),
    ])
}

#[test]
fn a_component_that_matches_no_file_fails() {
    let (_tmp, root) = ghost_project();
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "easement.yml:4: component `ghost` matches no file (in: src/renamed_away)",
        ))
        .stdout(contains("[unmatched]"));
}

#[test]
fn an_optional_component_may_match_no_file() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ntests: auto\ncomponents:\n  app: { in: src/app }\n  plugins: { in: src/plugins, optional: true }\nrules:\n  app: { may_depend_on: [plugins] }\n",
        ),
        ("src/app/Main.kt", "package app\n"),
    ]);
    // Neither `plugins` nor the `tests` of `tests: auto` has a file.
    easement(&root, &["check"]).success();
}

#[test]
fn an_unmatched_component_is_reported_in_json() {
    let (_tmp, root) = ghost_project();
    let report: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["check", "--format", "json"]).code(1),
    ))
    .unwrap();
    let v = &report["violations"][0];
    assert_eq!(v["rule"], "unmatched");
    assert_eq!(v["file"], "easement.yml");
    assert_eq!(v["from"], "ghost");
    assert_eq!(v["target"], "src/renamed_away");
}

#[test]
fn diff_and_scope_still_report_an_unmatched_component() {
    let (_tmp, root) = ghost_project();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    // The pull request touches neither the config nor a line with an import.
    fs::write(root.join("src/app/Other.kt"), "package app\n").unwrap();
    easement(&root, &["check", "--diff", "main"])
        .code(1)
        .stdout(contains("component `ghost` matches no file"));
    easement(&root, &["check", "--component", "app"])
        .code(1)
        .stdout(contains("component `ghost` matches no file"));
}

#[test]
fn the_baseline_never_accepts_an_unmatched_component() {
    let (_tmp, root) = ghost_project();
    easement(&root, &["baseline"])
        .success()
        .stdout(contains("0 violation(s) written"))
        .stdout(contains("1 violation(s) of the config left out"));
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("component `ghost` matches no file"));
}

/// Files that .gitignore hides are not read and no import of them is seen,
/// so a component of nothing else checks nothing.
#[test]
fn a_component_of_ignored_files_matches_no_file() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  app: { in: src/app }\n  gen: { in: src/gen }\nrules:\n  app: { may_depend_on: [gen] }\n",
        ),
        (".gitignore", "src/gen/\n"),
        ("src/gen/api.ts", "export const api = 1;\n"),
        (
            "src/app/a.ts",
            "import { api } from \"../gen/api\";\nexport const a = api;\n",
        ),
    ]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("component `gen` matches no file"))
        .stdout(contains(".gitignore hide do not count"));
}

#[test]
fn the_baseline_only_shrinks_unless_new_debt_is_accepted() {
    let (_tmp, root) = fixture("ts");
    easement(&root, &["baseline"])
        .success()
        .stdout(contains("2 violation(s)"));

    // Pay off one debt and add a new one.
    let page = root.join("src/ui/legacy.js");
    let fixed = fs::read_to_string(&page)
        .unwrap()
        .replace("const db = require(\"../infra/db\");", "const db = 1;");
    fs::write(&page, fixed).unwrap();
    fs::write(
        root.join("src/domain/leak.ts"),
        "import { checkout } from \"../app/checkout\";\n",
    )
    .unwrap();

    // The paid debt leaves the baseline; the new one is not written.
    easement(&root, &["baseline"])
        .code(1)
        .stdout(contains("1 violation(s) written"))
        .stdout(contains(
            "src/domain/leak.ts:1: domain -> app is not allowed",
        ))
        .stdout(contains("1 new violation(s) not written"))
        .stdout(contains("--accept-new"));
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("src/domain/leak.ts:1"))
        .stdout(contains("Stale baseline entries").not());

    // Legalizing new debt takes the flag.
    easement(&root, &["baseline", "--accept-new"])
        .success()
        .stdout(contains("2 violation(s) written"));
    easement(&root, &["check"]).success();
}

#[test]
fn the_baseline_keeps_the_debt_of_files_no_indexer_read() {
    let (_tmp, root) = fixture("ts");
    easement(&root, &["baseline"]).success();
    // The indexer stops covering the UI, so its debt is not seen, not paid.
    let tsconfig = fs::read_to_string(root.join("tsconfig.json")).unwrap();
    fs::write(
        root.join("tsconfig.json"),
        tsconfig.replacen('{', "{\n  \"exclude\": [\"src/ui\"],", 1),
    )
    .unwrap();
    easement(&root, &["baseline"])
        .success()
        .stdout(contains("kept for files no indexer covered"));
    let baseline = fs::read_to_string(root.join("easement.baseline.yml")).unwrap();
    assert!(baseline.contains("src/ui/legacy.js"), "{baseline}");
}

#[test]
fn a_new_file_on_a_baselined_cycle_or_limit_is_not_new_debt() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\nno_cycles: true\ncomponents:\n  a: { in: src/a }\n  b: { in: src/b }\nrules:\n  a: { may_depend_on: [b], max_fan_out: 0 }\n  b: { may_depend_on: [a] }\n",
        ),
        ("src/a/M.kt", "package a\nimport b.N\nclass M\n"),
        ("src/b/N.kt", "package b\nimport a.M\nclass N\n"),
    ]);
    easement(&root, &["baseline"])
        .success()
        .stdout(contains("2 violation(s)"));
    // A file that sorts first now carries the same edge.
    fs::write(root.join("src/a/A.kt"), "package a\nimport b.N\nclass A\n").unwrap();
    easement(&root, &["check"])
        .success()
        .stdout(contains("2 legalized"));
}

#[test]
fn a_new_file_in_a_directory_with_baselined_requirement_is_not_new_debt() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  a: { in: src/a }\nrules:\n  a: { requires_file: [README.md] }\n",
        ),
        ("src/a/M.kt", "package a\nclass M\n"),
    ]);
    easement(&root, &["baseline"])
        .success()
        .stdout(contains("1 violation(s)"));
    fs::write(root.join("src/a/A.kt"), "package a\nclass A\n").unwrap();
    easement(&root, &["check"])
        .success()
        .stdout(contains("1 legalized"));
}

#[test]
fn an_import_of_an_excluded_file_fails() {
    let config = "version: 1\ncomponents:\n  app: { in: src/app }\nrules:\n  app: { may_depend_on: [] }\nexclude: [src/legacy]\n";
    // Resolved by an indexer.
    let (_tmp, root) = project(&[
        ("easement.yml", config),
        ("src/legacy/api.ts", "export const api = 1;\n"),
        (
            "src/app/a.ts",
            "import { api } from \"../legacy/api\";\nexport const a = api;\n",
        ),
    ]);
    easement(&root, &["check"]).code(1).stdout(contains(
        "src/app/a.ts:1: app imports src/legacy/api.ts, which `exclude` hides, so no rule checks it",
    ));
    // Resolved by a tree-sitter adapter.
    let (_tmp, root) = project(&[
        ("easement.yml", config),
        ("src/legacy/Api.kt", "package legacy\nclass Api\n"),
        ("src/app/A.kt", "package app\nimport legacy.Api\nclass A\n"),
    ]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("src/app/A.kt:2: app imports src/legacy/Api.kt"))
        .stdout(contains("[excluded]"))
        .stdout(contains("[dependency]").not());
    // A deliberate exception is suppressed like any import.
    fs::write(
        root.join("src/app/A.kt"),
        "package app\n// easement:disable-next-line excluded -- legacy client, remove with #1\nimport legacy.Api\nclass A\n",
    )
    .unwrap();
    easement(&root, &["check"]).success();
}

#[test]
fn an_excluded_file_is_part_of_the_index_cache_key() {
    let config = "version: 1\ncomponents:\n  app: { in: src/app }\nrules:\n  app: { may_depend_on: [] }\nexclude: [src/gen]\n";
    let (_tmp, root) = project(&[
        ("easement.yml", config),
        ("src/gen/keep.ts", "export {};\n"),
        (
            "src/app/a.ts",
            "import { g } from \"../gen/g\";\nexport const a = g;\n",
        ),
    ]);
    let cache = TempDir::new().unwrap();
    easement_cached(&root, &["check"], cache.path()).success();
    // Code generation writes the file the app imports.
    fs::write(root.join("src/gen/g.ts"), "export const g = 1;\n").unwrap();
    easement_cached(&root, &["check"], cache.path())
        .code(1)
        .stdout(contains("[excluded]"));
}

#[test]
fn excluded_wildcard_imports_and_ignored_files() {
    // A Kotlin wildcard import resolves to the directory.
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  app: { in: src/app }\nexclude: ['src/legacy/**']\n",
        ),
        ("src/legacy/Api.kt", "package legacy\nclass Api\n"),
        ("src/app/A.kt", "package app\nimport legacy.*\nclass A\n"),
    ]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("src/app/A.kt:2: app imports src/legacy"));
    // A file that .gitignore hides is not read, in any language.
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  app: { in: src/app }\nexclude: [src/gen]\n",
        ),
        (".gitignore", "src/gen/\n"),
        ("src/gen/g.ts", "export const g = 1;\n"),
        (
            "src/app/a.ts",
            "import { g } from \"../gen/g\";\nexport const a = g;\n",
        ),
    ]);
    easement(&root, &["check"]).success();
}

/// `domain` must not reach `http`, even through `util`, though every hop on
/// its own is allowed.
fn reach_project(extra: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let mut files = vec![
        (
            "easement.yml",
            "version: 1\ncomponents:\n  domain: { in: src/domain }\n  util: { in: src/util }\n  http: { in: src/http }\nrules:\n  domain:\n    may_depend_on: [util]\n    must_not_reach: [{ name: http, because: the domain knows no transport }]\n  util: { may_depend_on: [http] }\n",
        ),
        (
            "src/domain/Order.kt",
            "package domain\nimport util.Retry\nclass Order\n",
        ),
        (
            "src/util/Retry.kt",
            "package util\nimport http.Client\nclass Retry\n",
        ),
        ("src/http/Client.kt", "package http\nclass Client\n"),
    ];
    files.extend_from_slice(extra);
    project(&files)
}

#[test]
fn a_component_must_not_reach_another_through_a_chain() {
    let (_tmp, root) = reach_project(&[]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "src/domain/Order.kt:2: domain reaches http through util: src/domain/Order.kt:2 -> src/util/Retry.kt:2 -> src/http/Client.kt [reach]",
        ))
        .stdout(contains("because: the domain knows no transport"));
}

#[test]
fn reach_follows_files_of_no_component_and_leaves_direct_imports_to_dependency() {
    // Through a file that belongs to no component.
    let (_tmp, root) = reach_project(&[
        (
            "src/domain/Order.kt",
            "package domain\nimport misc.Glue\nclass Order\n",
        ),
        (
            "src/misc/Glue.kt",
            "package misc\nimport http.Client\nclass Glue\n",
        ),
    ]);
    easement(&root, &["check"]).code(1).stdout(contains(
        "domain reaches http through no component: src/domain/Order.kt:2 -> src/misc/Glue.kt:2 -> src/http/Client.kt [reach]",
    ));
    // Through an import of a whole package, which resolves to its directory.
    let (_tmp, root) = reach_project(&[(
        "src/domain/Order.kt",
        "package domain\nimport util.*\nclass Order\n",
    )]);
    easement(&root, &["check"]).code(1).stdout(contains(
        "domain reaches http through util: src/domain/Order.kt:2 -> src/util/Retry.kt:2 -> src/http/Client.kt [reach]",
    ));
    // Straight into it: forbidden, and not a chain.
    let (_tmp, root) = reach_project(&[(
        "src/domain/Order.kt",
        "package domain\nimport http.Client\nclass Order\n",
    )]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains("domain -> http is forbidden"))
        .stdout(contains("[reach]").not());
}

#[test]
fn a_baselined_reach_stays_legal_and_diff_reports_a_new_one() {
    let (_tmp, root) = reach_project(&[]);
    easement(&root, &["baseline"]).success();
    // Another file on the same chain is not new debt.
    fs::write(
        root.join("src/domain/Account.kt"),
        "package domain\nimport util.Retry\nclass Account\n",
    )
    .unwrap();
    easement(&root, &["check"])
        .success()
        .stdout(contains("1 legalized"));

    // A pull request that adds the last hop is judged in diff mode.
    let (_tmp, root) = reach_project(&[("src/util/Retry.kt", "package util\nclass Retry\n")]);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    fs::write(
        root.join("src/util/Retry.kt"),
        "package util\nimport http.Client\nclass Retry\n",
    )
    .unwrap();
    easement(&root, &["check", "--diff", "main"])
        .code(1)
        .stdout(contains("domain reaches http through util"));
}

#[test]
fn reach_crosses_go_packages() {
    let (_tmp, root) = project(&[
        ("go.mod", "module example.com/app\n\ngo 1.22\n"),
        (
            "easement.yml",
            "version: 1\ncomponents:\n  domain: { in: internal/domain }\n  util: { in: internal/util }\n  http: { in: internal/http }\nrules:\n  domain: { may_depend_on: [util], must_not_reach: [http] }\n  util: { may_depend_on: [http] }\n",
        ),
        (
            "internal/domain/order.go",
            "package domain\n\nimport \"example.com/app/internal/util\"\n\nvar Order = util.Retry\n",
        ),
        (
            "internal/util/retry.go",
            "package util\n\nimport \"example.com/app/internal/http\"\n\nvar Retry = http.Client\n",
        ),
        (
            "internal/http/client.go",
            "package http\n\nvar Client = 1\n",
        ),
    ]);
    easement(&root, &["check"]).code(1).stdout(contains(
        "domain reaches http through util: internal/domain/order.go:3 -> internal/util/retry.go:3 -> internal/http/client.go [reach]",
    ));
}

#[test]
fn suggest_never_offers_to_allow_a_forbidden_import() {
    let (_tmp, root) = reach_project(&[(
        "src/domain/Order.kt",
        "package domain\nimport http.Client\nclass Order\n",
    )]);
    easement(&root, &["suggest"])
        .success()
        .stdout(contains(
            "Imported but forbidden by must_not_depend_on or must_not_reach (fix the imports):\n  domain: http (1 import(s))",
        ))
        .stdout(contains("add to may_depend_on").not());
}

/// Vertical slices: a feature's UI may use its own data and any feature's API.
fn slices_project(extra: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let mut files = vec![
        (
            "easement.yml",
            "version: 1\ncomponents:\n  ui: { in: 'src/features/<feature>/ui' }\n  data: { in: 'src/features/<feature>/data' }\n  api: { in: 'src/features/*/api' }\n  app: { in: src/app }\nrules:\n  ui: { may_depend_on: [data, api] }\n  app: { may_depend_on: [ui] }\n",
        ),
        (
            "src/features/orders/ui/View.kt",
            "package features.orders.ui\nimport features.orders.data.Repo\nimport features.billing.api.Api\nclass View\n",
        ),
        (
            "src/features/orders/data/Repo.kt",
            "package features.orders.data\nclass Repo\n",
        ),
        (
            "src/features/billing/api/Api.kt",
            "package features.billing.api\nclass Api\n",
        ),
        (
            "src/features/billing/data/Ledger.kt",
            "package features.billing.data\nclass Ledger\n",
        ),
        (
            "src/app/Main.kt",
            "package app\nimport features.orders.ui.View\nclass Main\n",
        ),
    ];
    files.extend_from_slice(extra);
    project(&files)
}

#[test]
fn a_named_placeholder_keeps_dependencies_within_one_slice() {
    let (_tmp, root) = slices_project(&[]);
    easement(&root, &["check"]).success();

    let (_tmp, root) = slices_project(&[(
        "src/features/billing/ui/Pay.kt",
        "package features.billing.ui\nimport features.orders.data.Repo\nimport features.orders.ui.View\nclass Pay\n",
    )]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "src/features/billing/ui/Pay.kt:2: ui -> data: <feature> differs (billing imports orders: src/features/orders/data/Repo.kt) [isolation]",
        ))
        .stdout(contains(
            "src/features/billing/ui/Pay.kt:3: ui -> ui: <feature> differs (billing imports orders: src/features/orders/ui/View.kt) [isolation]",
        ));
}

/// `reach_project` with the rule `reach` reported as a warning.
fn warned_reach_project() -> (TempDir, PathBuf) {
    let (tmp, root) = reach_project(&[]);
    warn_on_reach(&root);
    (tmp, root)
}

fn warn_on_reach(root: &Path) {
    let config = fs::read_to_string(root.join("easement.yml")).unwrap();
    fs::write(
        root.join("easement.yml"),
        format!("{config}warn: [reach]\n"),
    )
    .unwrap();
}

#[test]
fn a_rule_listed_in_warn_reports_without_failing() {
    let (_tmp, root) = warned_reach_project();
    easement(&root, &["check"])
        .success()
        .stdout(contains("Warnings (rules in `warn`; do not add more):"))
        .stdout(contains("domain reaches http through util"))
        .stdout(contains(
            "easement passed: 0 violation(s), 0 stale, 0 legalized, 1 warning(s)",
        ));
    let report: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["check", "--format", "json"]).success(),
    ))
    .unwrap();
    assert_eq!(report["violations"].as_array().unwrap().len(), 0);
    assert_eq!(report["warnings"][0]["rule"], "reach");
    easement(&root, &["check", "--format", "github"])
        .success()
        .stdout(contains(
            "::warning file=src/domain/Order.kt,line=2,title=easement reach::",
        ));
    let sarif = stdout_of(easement(&root, &["check", "--format", "sarif"]).success());
    assert!(sarif.contains("\"level\": \"warning\""), "{sarif}");
    // The baseline records only what fails.
    easement(&root, &["baseline"])
        .success()
        .stdout(contains("0 violation(s) written"));
}

#[test]
fn moving_a_baselined_rule_into_warn_leaves_no_stale_entries() {
    let (_tmp, root) = reach_project(&[]);
    easement(&root, &["baseline"]).success();
    warn_on_reach(&root);
    easement(&root, &["check"])
        .success()
        .stdout(contains("0 stale"));
}

#[test]
fn warn_takes_only_rules_that_can_be_tolerated() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  a: { in: a }\nwarn: [bogus]\n",
        ),
        ("a/A.kt", "package a\n"),
    ]);
    easement(&root, &["check"])
        .code(2)
        .stderr(contains("unknown rule `bogus` in warn"))
        .stderr(contains("dependency"))
        .stderr(contains("unmatched").not());
    fs::write(
        root.join("easement.yml"),
        "version: 1\ncomponents:\n  a: { in: a }\nwarn: [unmatched]\n",
    )
    .unwrap();
    easement(&root, &["check"])
        .code(2)
        .stderr(contains("`unmatched` is about the config"));
}

#[test]
fn warnings_are_sorted_and_warn_is_inherited_through_extends() {
    let (_tmp, root) = project(&[
        (
            "base.yml",
            "version: 1\ncomponents:\n  a: { in: src/a }\n  b: { in: src/b }\nwarn: [dependency]\n",
        ),
        ("easement.yml", "extends: base.yml\n"),
        ("src/a/Z.kt", "package a\nimport b.B\nclass Z\n"),
        (
            "src/a/A.kt",
            "package a\n\nimport b.B\nimport b.C\nclass A\n",
        ),
        ("src/b/B.kt", "package b\nclass B\n"),
        ("src/b/C.kt", "package b\nclass C\n"),
    ]);
    let out = stdout_of(easement(&root, &["check"]).success());
    let a3 = out.find("src/a/A.kt:3").unwrap();
    let a4 = out.find("src/a/A.kt:4").unwrap();
    let z2 = out.find("src/a/Z.kt:2").unwrap();
    assert!(a3 < a4 && a4 < z2, "{out}");
    // A `warn` of its own replaces the inherited one.
    fs::write(root.join("easement.yml"), "extends: base.yml\nwarn: []\n").unwrap();
    easement(&root, &["check"]).code(1);
}

#[test]
fn impact_lists_what_depends_on_a_change() {
    let (_tmp, root) = reach_project(&[]);
    easement(&root, &["impact", "src/http/Client.kt"])
        .success()
        .stdout(contains("Changed: 1 file(s) in http"))
        .stdout(contains("Depend on them: 2 file(s) in domain, util"))
        .stdout(contains("  domain: src/domain/Order.kt"))
        .stdout(contains("  util: src/util/Retry.kt"));
    let report: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["impact", "src/util", "--format", "json"]).success(),
    ))
    .unwrap();
    assert_eq!(report["changed"], serde_json::json!(["src/util/Retry.kt"]));
    assert_eq!(report["dependents"][0]["file"], "src/domain/Order.kt");
    assert_eq!(report["dependents"][0]["component"], "domain");
    assert_eq!(report["confidence"], "full");

    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    fs::write(
        root.join("src/util/Retry.kt"),
        "package util\nimport http.Client\nclass Retry2\n",
    )
    .unwrap();
    easement(&root, &["impact", "--diff", "main"])
        .success()
        .stdout(contains("Changed: 1 file(s) in util"))
        .stdout(contains("  domain: src/domain/Order.kt"));
    easement(&root, &["impact"])
        .code(2)
        .stderr(contains("--diff"));
}

#[test]
fn why_shows_the_imports_or_the_chain_between_two_components() {
    let (_tmp, root) = reach_project(&[]);
    easement(&root, &["why", "util", "http"])
        .success()
        .stdout(contains("util -> http is allowed (may_depend_on)"))
        .stdout(contains(
            "1 import(s):\n  src/util/Retry.kt:2 imports src/http/Client.kt\n",
        ));
    easement(&root, &["why", "domain", "http"])
        .success()
        .stdout(contains(
            "domain -> http is forbidden (must_not_reach): the domain knows no transport",
        ))
        .stdout(contains(
            "domain imports nothing of http directly; it reaches http through util:",
        ))
        .stdout(contains(
            "  src/domain/Order.kt:2 -> src/util/Retry.kt:2 -> src/http/Client.kt",
        ));
    easement(&root, &["why", "http", "domain"])
        .success()
        .stdout(contains("http does not depend on domain"));
    easement(&root, &["why", "http", "ghost"])
        .code(2)
        .stderr(contains("unknown component `ghost`"));
}

#[test]
fn graph_focus_keeps_a_component_and_its_neighbours() {
    let (_tmp, root) = slices_project(&[]);
    let out = stdout_of(easement(&root, &["graph", "--focus", "data"]).success());
    assert!(out.contains("\"ui\""), "{out}");
    assert!(out.contains("\"data\""), "{out}");
    assert!(!out.contains("\"app\""), "{out}");
    easement(&root, &["graph", "--focus", "ghost"])
        .code(2)
        .stderr(contains("unknown component `ghost`"));
}

#[test]
fn impact_and_why_say_when_an_answer_may_be_short() {
    let (_tmp, root) = fixture("uncovered");
    fs::write(
        root.join("easement.yml"),
        "version: 1\ncomponents:\n  app: { in: app }\n  tools: { in: tools }\n",
    )
    .unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout_of(
        easement(&root, &["impact", "app", "--format", "json"]).success(),
    ))
    .unwrap();
    assert_eq!(report["confidence"], "partial");
    easement(&root, &["why", "tools", "app"])
        .success()
        .stdout(contains("tools does not depend on app"))
        .stdout(contains("source files were not covered by an indexer"));

    // A deleted file: what imported it is gone from the code.
    let (_tmp, root) = reach_project(&[]);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    fs::remove_file(root.join("src/http/Client.kt")).unwrap();
    easement(&root, &["impact", "--diff", "main"])
        .success()
        .stdout(contains("Changed: 0 file(s)\n"))
        .stdout(contains("Deleted: src/http/Client.kt"))
        .stdout(contains("Partial: 1 deleted file(s)"));
}

#[test]
fn why_reports_what_check_would_say_about_each_import() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  a: { in: src/a }\n  b: { in: src/b, allow_importers: [c] }\n  c: { in: src/c }\nrules:\n  a: { may_depend_on: [b] }\n",
        ),
        ("src/a/A.kt", "package a\nimport b.*\nclass A\n"),
        ("src/b/B.kt", "package b\nclass B\n"),
        ("src/b/C.kt", "package b\nclass C\n"),
        ("src/c/C.kt", "package c\nclass C\n"),
    ]);
    easement(&root, &["why", "a", "b"])
        .success()
        .stdout(contains(
            "a -> b is allowed (may_depend_on)\n  but b may be imported only from c",
        ))
        .stdout(contains(
            "1 import(s):\n  src/a/A.kt:2 imports src/b/B.kt, src/b/C.kt [violates: dependency]",
        ));
    easement(&root, &["why", "a", "a"])
        .success()
        .stdout(contains("a -> a is allowed (same component)"));
}

#[test]
fn std_stands_for_the_standard_library_of_the_language() {
    let (_tmp, root) = project(&[
        (
            "easement.yml",
            "version: 1\ncomponents:\n  core: { in: src/core }\n  ui: { in: src/ui }\nrules:\n  core: { may_use_external: [$std] }\n  ui: { forbid_external: [{ name: $std, because: the UI goes through core }] }\n",
        ),
        (
            "src/core/A.kt",
            "package core\nimport java.util.UUID\nimport kotlin.math.max\nimport okhttp3.Client\nclass A\n",
        ),
        (
            "src/ui/B.kt",
            "package ui\nimport java.time.Instant\nclass B\n",
        ),
    ]);
    easement(&root, &["check"])
        .code(1)
        .stdout(contains(
            "src/core/A.kt:4: core -> external okhttp3.Client is not in may_use_external",
        ))
        .stdout(contains("src/core/A.kt:2").not())
        .stdout(contains("src/core/A.kt:3").not())
        .stdout(contains(
            "src/ui/B.kt:2: ui -> external java.time.Instant is forbidden",
        ))
        .stdout(contains("because: the UI goes through core"));
}
