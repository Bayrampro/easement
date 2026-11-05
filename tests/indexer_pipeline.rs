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
