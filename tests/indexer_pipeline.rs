//! Instrumented tools exercise the shared pipeline independently of installed SDKs.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use protobuf::Message;
use scip::types::{Document, Index, Occurrence, SymbolInformation, symbol_information::Kind};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Tools {
    temporary: TempDir,
    root: PathBuf,
    policy: PathBuf,
    sdk: PathBuf,
    bin: PathBuf,
    runner: PathBuf,
    state: PathBuf,
    cache: PathBuf,
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
}

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Tools {
    fn new() -> Self {
        let temporary = TempDir::new().unwrap();
        let base = fs::canonicalize(temporary.path()).unwrap();
        let tools = Self {
            root: base.join("project"),
            policy: base.join("project/easement.yml"),
            sdk: base.join("sdk"),
            bin: base.join("bin"),
            runner: base.join("runner"),
            state: base.join("state"),
            cache: base.join("cache"),
            temporary,
        };
        for dir in [
            "project/lib",
            "project/.dart_tool",
            "sdk/bin",
            "sdk/lib",
            "bin",
            "runner",
            "state",
            "dependency",
        ] {
            fs::create_dir_all(base.join(dir)).unwrap();
        }
        fs::write(tools.root.join("pubspec.yaml"), "name: app\n").unwrap();
        fs::write(
            tools.root.join("lib/app.dart"),
            "class Visible {}\nclass _HiddenEntity {}\n",
        )
        .unwrap();
        fs::write(&tools.policy, "version: 1\ncomponents:\n  app: {in: lib}\n").unwrap();
        fs::write(
            tools.root.join("private.yml"),
            concat!(
                "version: 1\ncomponents:\n  app: {in: lib}\n",
                "  entities: {in: entities, optional: true, defines: ['*Entity']}\n",
            ),
        )
        .unwrap();
        fs::write(tools.sdk.join("version"), "3.13.7").unwrap();
        fs::write(tools.sdk.join("lib/libraries.json"), "{}").unwrap();
        fs::write(
            tools.runner.join("source.dart"),
            "// instrumented indexer\n",
        )
        .unwrap();
        fs::write(
            tools.runner.join("package_config.json"),
            json!({"packages": [{"name": "tool", "rootUri": tools.runner}]}).to_string(),
        )
        .unwrap();
        fs::write(
            tools.root.join(".dart_tool/package_config.json"),
            json!({"configVersion": 2, "packages": [
                {"name": "app", "rootUri": tools.root, "packageUri": "lib/"},
                {"name": "dependency", "rootUri": base.join("dependency"), "packageUri": ""}
            ]})
            .to_string(),
        )
        .unwrap();
        let module = "scip-dart pub app null lib/`app.dart`/";
        let mut index = Index::new();
        index.documents.push(Document {
            language: "Dart".into(),
            relative_path: "lib/app.dart".into(),
            occurrences: vec![
                Occurrence {
                    symbol: module.into(),
                    symbol_roles: 1,
                    range: vec![0, 0, 0],
                    ..Default::default()
                },
                Occurrence {
                    symbol: "local 1".into(),
                    symbol_roles: 1,
                    range: vec![1, 6, 19],
                    ..Default::default()
                },
            ],
            symbols: vec![SymbolInformation {
                symbol: "local 1".into(),
                kind: Kind::Class.into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        fs::write(
            tools.runner.join("dart.scip"),
            index.write_to_bytes().unwrap(),
        )
        .unwrap();
        let script = r#"#!/bin/sh
set -eu
if [ "${1-}" != "--suppress-analytics" ]; then
    shift 2
    if [ "${1-}" = "--version" ]; then
        if [ -f @STATE@/fail-version ]; then echo 'broken indexer' >&2; exit 1; fi
        echo 'scip_dart instrumented'; exit 0
    fi
    echo index >> @STATE@/calls
    touch @STATE@/dart-started
    if [ -f @STATE@/parallel ]; then
        count=0
        while [ ! -f @STATE@/go-started ]; do
            count=$((count + 1)); [ "$count" -lt 500 ] || exit 9; sleep 0.01
        done
        touch @STATE@/overlap
    fi
    [ "$1" = '--output' ]
    cp @RUNNER@/dart.scip "$2"
else
    echo analyze >> @STATE@/calls
    if [ -f @STATE@/mutate ]; then echo '// changed during extraction' >> @ROOT@/lib/app.dart; fi
    if [ -f @STATE@/error ]; then
        echo "ERROR|COMPILE_TIME_ERROR|TEST_ERROR|$PWD/lib/app.dart|1|1|1|instrumented analyzer gap"
        exit 3
    fi
fi
"#;
        executable(&tools.sdk.join("bin/dart"), &tools.expand(script));
        let dart = tools.sdk.join("bin/dart");
        let config = tools.runner.join("package_config.json");
        let source = tools.runner.join("source.dart");
        let header = json!({"dart": dart, "config": config, "source": source});
        executable(
            &tools.bin.join("scip_dart"),
            &format!(
                "#!/bin/sh\n# easement-scip-dart-v1 {header}\nexec {} {} {} \"$@\"\n",
                quote(&dart),
                quote(Path::new(&format!("--packages={}", config.display()))),
                quote(&source)
            ),
        );
        tools
    }

    fn expand(&self, text: &str) -> String {
        text.replace("@STATE@", &quote(&self.state))
            .replace("@RUNNER@", &quote(&self.runner))
            .replace("@ROOT@", &quote(&self.root))
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut paths = vec![self.bin.clone(), self.sdk.join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut command = Command::cargo_bin("easement").unwrap();
        command
            .current_dir(&self.root)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("EASEMENT_CACHE_DIR", &self.cache)
            .arg(subcommand)
            .args(["--config", self.policy.to_str().unwrap()]);
        command
    }

    fn check(&self, options: &[&str]) -> Value {
        let output = self
            .command("check")
            .args(["--format", "json"])
            .args(options)
            .assert()
            .success();
        serde_json::from_slice(&output.get_output().stdout).unwrap()
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.state.join("calls")).unwrap_or_default()
    }
}

#[test]
fn cache_hit_runs_neither_tool_and_recomputes_private_policy_from_raw_scip() {
    let mut tools = Tools::new();
    fs::write(tools.state.join("error"), "").unwrap();
    tools
        .command("check")
        .args(["--cache-only"])
        .assert()
        .code(3);
    let cold = tools.check(&[]);
    assert_eq!(cold["stats"]["indexed"], 1, "{cold}");
    assert_eq!(
        cold["stats"]["analysis_partial"].as_array().unwrap().len(),
        1
    );
    let warm = tools.check(&["--cache-only"]);
    assert_eq!(warm["stats"]["from_cache"], 1, "{warm}");
    assert_eq!(
        cold["stats"]["analysis_partial"],
        warm["stats"]["analysis_partial"]
    );
    assert_eq!(tools.calls(), "index\nanalyze\n");
    tools.policy = tools.root.join("private.yml");
    let policy = tools.check(&["--cache-only"]);
    let gaps = policy["stats"]["analysis_partial"].as_array().unwrap();
    assert_eq!(gaps.len(), 2, "{policy}");
    assert!(
        gaps.iter()
            .any(|gap| gap["reason"].as_str().unwrap().contains("defines")),
        "{policy}"
    );
    assert_eq!(tools.calls(), "index\nanalyze\n");
    tools.policy = tools.root.join("easement.yml");
    let ordinary = tools.check(&["--cache-only"]);
    assert_eq!(
        ordinary["stats"]["analysis_partial"],
        cold["stats"]["analysis_partial"]
    );
    assert_eq!(tools.calls(), "index\nanalyze\n");
    tools.policy = tools.root.join("private.yml");
    tools.check(&["--no-cache"]);
    assert_eq!(tools.calls(), "index\nanalyze\nindex\nanalyze\n");
}

#[test]
fn changed_inputs_during_extraction_are_not_published() {
    let tools = Tools::new();
    fs::write(tools.state.join("mutate"), "").unwrap();
    tools.check(&[]);
    tools
        .command("check")
        .args(["--cache-only"])
        .assert()
        .code(3);
    fs::remove_file(tools.state.join("mutate")).unwrap();
    tools.check(&[]);
    tools.check(&["--cache-only"]);
    assert_eq!(tools.calls(), "index\nanalyze\nindex\nanalyze\n");
}

#[test]
fn dart_shares_the_language_worker_pool_and_jobs_do_not_change_reports() {
    let tools = Tools::new();
    fs::create_dir(tools.root.join("backend")).unwrap();
    fs::write(
        tools.root.join("backend/go.mod"),
        "module example.com/backend\ngo 1.22\n",
    )
    .unwrap();
    fs::write(tools.root.join("backend/backend.go"), "package backend\n").unwrap();
    fs::write(
        &tools.policy,
        "version: 1\ncomponents:\n  app: {in: lib}\n  backend: {in: backend}\n",
    )
    .unwrap();
    let mut index = Index::new();
    index.documents.push(Document {
        language: "Go".into(),
        relative_path: "backend.go".into(),
        ..Default::default()
    });
    fs::write(
        tools.runner.join("go.scip"),
        index.write_to_bytes().unwrap(),
    )
    .unwrap();
    executable(
        &tools.bin.join("scip-go"),
        &tools.expand(
            r#"#!/bin/sh
set -eu
touch @STATE@/go-started
if [ -f @STATE@/parallel ]; then
    count=0
    while [ ! -f @STATE@/dart-started ]; do
        count=$((count + 1)); [ "$count" -lt 500 ] || exit 9; sleep 0.01
    done
fi
while [ "$1" != '--output' ]; do shift; done
cp @RUNNER@/go.scip "$2"
"#,
        ),
    );
    fs::write(tools.state.join("parallel"), "").unwrap();
    let parallel = tools.check(&["--no-cache", "--jobs", "2"]);
    assert!(tools.state.join("overlap").is_file());
    fs::remove_file(tools.state.join("parallel")).unwrap();
    let serial = tools.check(&["--no-cache", "--jobs", "1"]);
    assert_eq!(parallel, serial);
    assert_eq!(parallel["stats"]["indexed"], 2, "{parallel}");
}

#[test]
fn configured_inputs_and_environment_invalidate_the_shared_entry() {
    let tools = Tools::new();
    fs::write(
        tools.root.join("analysis_options.yaml"),
        "include: ../included.yaml\n",
    )
    .unwrap();
    let include = tools.temporary.path().join("included.yaml");
    fs::write(&include, "analyzer: {}\n").unwrap();
    tools.check(&[]);
    tools.check(&["--cache-only"]);
    for file in [
        tools.root.join(".dart_tool/package_config.json"),
        tools.temporary.path().join("dependency/value.dart"),
        tools.sdk.join("version"),
        tools.runner.join("source.dart"),
        tools.runner.join("package_config.json"),
        include,
    ] {
        let mut text = fs::read_to_string(&file).unwrap_or_default();
        text.push('\n');
        fs::write(file, text).unwrap();
        tools
            .command("check")
            .args(["--cache-only"])
            .assert()
            .code(3);
        tools.check(&[]);
        tools.check(&["--cache-only"]);
    }
    tools
        .command("check")
        .env("DART_TEST_CONTRACT", "changed")
        .args(["--cache-only"])
        .assert()
        .code(3);
    // Unknown launchers and plugins bypass caching but still perform analysis.
    let binary = tools.bin.join("scip_dart");
    let original = fs::read_to_string(&binary).unwrap();
    fs::write(&binary, format!("{original}\n")).unwrap();
    tools
        .command("check")
        .args(["--cache-only"])
        .assert()
        .code(3);
    tools.check(&[]);
    tools
        .command("check")
        .args(["--cache-only"])
        .assert()
        .code(3);
    fs::write(&binary, original).unwrap();
    for options in [
        "analyzer: {plugins: [custom]}\n",
        "include: analysis_options.yaml\n",
        "include: missing.yaml\n",
    ] {
        fs::write(tools.root.join("analysis_options.yaml"), options).unwrap();
        tools
            .command("check")
            .args(["--cache-only"])
            .assert()
            .code(3);
        tools.check(&[]);
        tools
            .command("check")
            .args(["--cache-only"])
            .assert()
            .code(3);
    }
}

#[test]
fn doctor_uses_common_roots_prerequisites_and_strict_version_probes() {
    let tools = Tools::new();
    tools.command("doctor").assert().success();
    fs::write(tools.state.join("fail-version"), "").unwrap();
    tools
        .command("doctor")
        .assert()
        .code(1)
        .stdout(contains("--version failed"));
    fs::remove_file(tools.state.join("fail-version")).unwrap();
    let config = tools.root.join(".dart_tool/package_config.json");
    fs::write(&config, "{").unwrap();
    tools
        .command("doctor")
        .assert()
        .code(1)
        .stdout(contains("invalid Dart package configuration"));
    fs::remove_file(config).unwrap();
    tools
        .command("doctor")
        .assert()
        .code(1)
        .stdout(contains("package_config.json"));
    fs::remove_file(tools.sdk.join("lib/libraries.json")).unwrap();
    tools
        .command("doctor")
        .assert()
        .code(1)
        .stdout(contains("cannot identify the Dart SDK"));
    tools
        .command("doctor")
        .env("PATH", &tools.bin)
        .assert()
        .code(1)
        .stdout(contains("Dart SDK is missing"));
    fs::remove_file(tools.bin.join("scip_dart")).unwrap();
    tools
        .command("doctor")
        .env("PATH", &tools.bin)
        .assert()
        .code(1)
        .stdout(contains("not found"));
}

impl Tools {
    fn go_package(&self) {
        fs::create_dir_all(self.root.join("backend")).unwrap();
        fs::write(
            self.root.join("backend/go.mod"),
            "module example.com/backend\ngo 1.22\n",
        )
        .unwrap();
        fs::write(
            self.root.join("backend/backend.go"),
            "package backend\ntype Thing struct {}\n",
        )
        .unwrap();
        fs::write(self.root.join("backend/other.go"), "package backend\n").unwrap();
        let mut index = Index::new();
        index.documents.push(Document {
            language: "Go".into(),
            relative_path: "backend.go".into(),
            occurrences: vec![Occurrence {
                symbol: "scip-go gomod example.com/backend . `example.com/backend/nested`/Thing#"
                    .into(),
                symbol_roles: 1,
                range: vec![1, 5, 10],
                ..Default::default()
            }],
            ..Default::default()
        });
        index.documents.push(Document {
            language: "Go".into(),
            relative_path: "other.go".into(),
            ..Default::default()
        });
        fs::write(self.runner.join("go.scip"), index.write_to_bytes().unwrap()).unwrap();
        executable(
            &self.bin.join("scip-go"),
            &self.expand(
                r#"#!/bin/sh
set -eu
while [ "$1" != '--output' ]; do shift; done
cp @RUNNER@/go.scip "$2"
"#,
            ),
        );
    }

    fn baseline(&self, entries: Value) {
        fs::write(
            self.root.join("easement.baseline.yml"),
            serde_norway::to_string(&json!({"violations": entries})).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn a_dart_gap_keeps_only_related_baseline_debt_and_allows_independent_stale() {
    let tools = Tools::new();
    tools.go_package();
    fs::write(
        &tools.policy,
        "version: 1\ncomponents:\n  app: {in: lib}\n  backend: {in: backend}\n",
    )
    .unwrap();
    fs::write(tools.state.join("error"), "").unwrap();
    let entries = json!([
        {"rule": "dependency", "file": "lib/app.dart", "target": "lib/old.dart", "from": "app", "to": "old"},
        {"rule": "dependency", "file": "backend/backend.go", "target": "backend/old.go", "from": "backend", "to": "old"}
    ]);
    tools.baseline(entries);
    let result = tools
        .command("check")
        .args(["--format", "json", "--no-cache"])
        .assert()
        .code(1);
    let report: Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["stale"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["stale"][0]["file"], "backend/backend.go");
    let selected = tools
        .command("check")
        .args(["--scope", "backend", "--format", "json"])
        .assert()
        .code(1);
    let selected: Value = serde_json::from_slice(&selected.get_output().stdout).unwrap();
    assert!(
        selected["stats"].get("analysis_partial").is_none(),
        "{selected}"
    );
    assert_eq!(selected["stale"].as_array().unwrap().len(), 1);
    tools.check(&["--scope", "lib/app.dart"]);
    for arguments in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Coverage Test",
            "-c",
            "user.email=coverage@example.invalid",
            "commit",
            "-q",
            "-m",
            "test: coverage base",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(arguments)
                .current_dir(&tools.root)
                .status()
                .unwrap()
                .success()
        );
    }
    let diff = tools
        .command("check")
        .args(["--diff", "HEAD", "--format", "json"])
        .assert()
        .code(1);
    let diff: Value = serde_json::from_slice(&diff.get_output().stdout).unwrap();
    assert_eq!(diff["stale"], report["stale"]);
    tools.command("baseline").assert().success();
    let saved: Value = serde_norway::from_str(
        &fs::read_to_string(tools.root.join("easement.baseline.yml")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["violations"].as_array().unwrap().len(), 1, "{saved}");
    assert_eq!(saved["violations"][0]["file"], "lib/app.dart");
}

#[test]
fn component_symbol_diagnostics_survive_an_unrelated_language_gap() {
    let tools = Tools::new();
    tools.go_package();
    fs::write(&tools.policy, concat!(
        "version: 1\ncomponents:\n  app: {in: lib}\n  backend: {in: backend}\n",
        "forbid_symbol: [example.com/backend.Thing]\nrules:\n  backend: {forbid_symbol: [example.com/backend.Thing]}\n"
    )).unwrap();
    fs::write(tools.state.join("error"), "").unwrap();
    let report = tools.check(&["--no-cache"]);
    let notes = report["stats"]["symbols_unmatched"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{report}");
    assert!(notes[0].as_str().unwrap().contains("of backend"));
    assert!(
        notes[0]
            .as_str()
            .unwrap()
            .contains("example.com/backend/nested.Thing")
    );
}

#[test]
fn package_diagnostics_have_the_same_local_coverage_on_fresh_and_cached_reads() {
    let tools = Tools::new();
    tools.go_package();
    fs::write(
        &tools.policy,
        "version: 1\ncomponents:\n  app: {in: lib}\n  backend: {in: backend}\n",
    )
    .unwrap();
    let dart = tools.sdk.join("bin/dart");
    let script = fs::read_to_string(&dart)
        .unwrap()
        .replace("$PWD/lib/app.dart", "/outside/project/generated.dart");
    executable(&dart, &script);
    fs::write(tools.state.join("error"), "").unwrap();
    tools.baseline(json!([
        {"rule": "external", "file": "lib/app.dart", "target": "package:old", "from": "app", "to": ""},
        {"rule": "external", "file": "backend/backend.go", "target": "old", "from": "backend", "to": ""}
    ]));
    let read = |cached: bool| {
        let mut command = tools.command("check");
        command.args(["--format", "json"]);
        if cached {
            command.arg("--cache-only");
        }
        let output = command.assert().code(1);
        serde_json::from_slice::<Value>(&output.get_output().stdout).unwrap()
    };
    let cold = read(false);
    let calls = tools.calls();
    let warm = read(true);
    assert_eq!(tools.calls(), calls);
    assert_eq!(cold["stale"], warm["stale"]);
    assert_eq!(warm["stale"].as_array().unwrap().len(), 1, "{warm}");
    assert_eq!(warm["stale"][0]["file"], "backend/backend.go");
    assert_eq!(
        cold["stats"]["analysis_partial"],
        warm["stats"]["analysis_partial"]
    );
}

#[test]
fn independent_files_in_one_dart_package_can_retire_their_own_debt() {
    let tools = Tools::new();
    fs::write(tools.root.join("lib/clean.dart"), "class Clean {}\n").unwrap();
    let path = tools.runner.join("dart.scip");
    let mut index = Index::parse_from_bytes(&fs::read(&path).unwrap()).unwrap();
    index.documents.push(Document {
        language: "Dart".into(),
        relative_path: "lib/clean.dart".into(),
        ..Default::default()
    });
    fs::write(path, index.write_to_bytes().unwrap()).unwrap();
    fs::write(tools.state.join("error"), "").unwrap();
    tools.baseline(json!([
        {"rule": "external", "file": "lib/app.dart", "target": "package:old", "from": "app", "to": ""},
        {"rule": "external", "file": "lib/clean.dart", "target": "package:old", "from": "app", "to": ""}
    ]));
    let result = tools
        .command("check")
        .args(["--format", "json", "--no-cache"])
        .assert()
        .code(1);
    let report: Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["stale"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["stale"][0]["file"], "lib/clean.dart");
}

#[test]
fn suggestions_and_graph_answers_do_not_certify_absence_through_a_gap() {
    let tools = Tools::new();
    tools.go_package();
    fs::write(
        &tools.policy,
        concat!(
            "version: 1\ncomponents:\n  app: {in: lib}\n  backend: {in: backend/backend.go}\n",
            "  other: {in: backend/other.go}\n",
            "rules:\n  app:\n    may_depend_on: [backend]\n  backend:\n    may_depend_on: [other]\n"
        ),
    )
    .unwrap();
    fs::write(tools.state.join("error"), "").unwrap();
    tools
        .command("suggest")
        .arg("--patch")
        .assert()
        .success()
        .stdout(contains("-    may_depend_on: [other]"))
        .stdout(predicates::prelude::PredicateBooleanExt::not(contains(
            "-    may_depend_on: [backend]",
        )));
    tools
        .command("why")
        .args(["app", "backend"])
        .assert()
        .success()
        .stdout(contains("no dependency path was established"))
        .stdout(contains("partial"));
    tools
        .command("impact")
        .arg("lib/app.dart")
        .assert()
        .success()
        .stdout(contains("No dependents were established"))
        .stdout(contains("Partial:"));
}

#[test]
fn missing_generated_diagnostics_stay_local_and_target_only_errors_are_not_source_gaps() {
    let tools = Tools::new();
    let dart = tools.sdk.join("bin/dart");
    let original = fs::read_to_string(&dart).unwrap();
    executable(
        &dart,
        &original.replace("$PWD/lib/app.dart", "$PWD/lib/missing.g.dart"),
    );
    fs::write(tools.state.join("error"), "").unwrap();
    tools.baseline(json!([
        {"rule": "external", "file": "lib/missing.g.dart", "target": "package:old", "from": "app", "to": ""},
        {"rule": "external", "file": "lib/app.dart", "target": "package:old", "from": "app", "to": ""}
    ]));
    let result = tools
        .command("check")
        .args(["--format", "json", "--no-cache"])
        .assert()
        .code(1);
    let report: Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(
        report["stats"]["analysis_partial"][0]["file"],
        "lib/missing.g.dart"
    );
    assert_eq!(report["stale"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["stale"][0]["file"], "lib/app.dart");
    executable(&dart, &original);
    fs::remove_file(tools.root.join("easement.baseline.yml")).unwrap();
    fs::write(
        &tools.policy,
        "version: 1\ncomponents:\n  app: {in: lib}\ntarget_only: [lib/app.dart]\n",
    )
    .unwrap();
    let report = tools.check(&["--no-cache"]);
    assert!(
        report["stats"].get("analysis_partial").is_none(),
        "{report}"
    );
}

#[test]
fn incomplete_middle_files_protect_reach_debt_in_both_reach_modes_and_narrow_scopes() {
    let tools = Tools::new();
    for (file, source) in [
        ("app.dart", "import 'middle.dart';\nclass Visible {}\n"),
        ("middle.dart", "class Middle {}\n"),
        ("end.dart", "class End {}\n"),
    ] {
        fs::write(tools.root.join("lib").join(file), source).unwrap();
    }
    let mut index = Index::new();
    for file in ["app.dart", "middle.dart", "end.dart"] {
        let mut occurrences = vec![Occurrence {
            symbol: format!("scip-dart pub app null lib/`{file}`/"),
            symbol_roles: 1,
            range: vec![0, 0, 0],
            ..Default::default()
        }];
        if file == "app.dart" {
            occurrences.push(Occurrence {
                symbol: "scip-dart pub app null lib/`middle.dart`/".into(),
                range: vec![0, 7, 20],
                ..Default::default()
            });
        }
        index.documents.push(Document {
            language: "Dart".into(),
            relative_path: format!("lib/{file}"),
            occurrences,
            ..Default::default()
        });
    }
    fs::write(
        tools.runner.join("dart.scip"),
        index.write_to_bytes().unwrap(),
    )
    .unwrap();
    let dart = tools.sdk.join("bin/dart");
    executable(
        &dart,
        &fs::read_to_string(&dart)
            .unwrap()
            .replace("$PWD/lib/app.dart", "$PWD/lib/middle.dart"),
    );
    for mode in ["files", "symbols"] {
        fs::write(
            &tools.policy,
            format!(
                concat!(
                    "version: 1\nreach: {}\ncomponents:\n  app: {{in: lib/app.dart}}\n",
                    "  middle: {{in: lib/middle.dart}}\n  end: {{in: lib/end.dart}}\n",
                    "rules:\n  app: {{may_depend_on: [middle], must_not_reach: [end]}}\n"
                ),
                mode
            ),
        )
        .unwrap();
        tools.baseline(
            json!([{ "rule": "reach", "file": "", "target": "", "from": "app", "to": "end" }]),
        );
        fs::write(tools.state.join("error"), "").unwrap();
        let report = tools.check(&[]);
        let gaps = report["stats"]["analysis_partial"].as_array().unwrap();
        assert_eq!(gaps.len(), 1, "{report}");
        assert_eq!(gaps[0]["file"], "lib/middle.dart");
        let scoped = tools.check(&["--cache-only", "--scope", "lib/app.dart"]);
        assert!(scoped["stale"].as_array().unwrap().is_empty(), "{scoped}");
        assert!(
            scoped["stats"].get("analysis_partial").is_none(),
            "{scoped}"
        );
        fs::remove_file(tools.state.join("error")).unwrap();
        let complete = tools
            .command("check")
            .args(["--no-cache", "--format", "json"])
            .assert()
            .code(1);
        let complete: Value = serde_json::from_slice(&complete.get_output().stdout).unwrap();
        assert_eq!(complete["stale"].as_array().unwrap().len(), 1, "{complete}");
    }
}
