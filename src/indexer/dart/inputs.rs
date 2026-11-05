//! Enumerate resolved Dart inputs; hashing and cache I/O belong to the shared engine.
use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Launcher {
    dart: PathBuf,
    config: PathBuf,
    source: PathBuf,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(super) fn collect(
    binary: &Path,
    sdk: &Path,
    dir: &Path,
    resolution: &Resolution,
) -> Result<Vec<Input>> {
    let wrapper =
        std::fs::read_to_string(binary).context("Dart cache needs a verifiable launcher")?;
    let header = wrapper.lines().nth(1).context("unknown Dart launcher")?;
    let launcher: Launcher = serde_json::from_str(
        header
            .strip_prefix("# easement-scip-dart-v1 ")
            .context("unknown Dart launcher; use the pinned installation for cache support")?,
    )?;
    let expected = format!(
        "#!/bin/sh\n{header}\nexec {} {} {} \"$@\"\n",
        quote(&launcher.dart.to_string_lossy()),
        quote(&format!("--packages={}", launcher.config.display())),
        quote(&launcher.source.to_string_lossy())
    );
    if wrapper != expected {
        bail!("unrecognized Dart launcher implementation");
    }
    let mut inputs = vec![
        Input::Tree(dir.to_path_buf()),
        Input::File(binary.to_path_buf()),
        Input::File(launcher.source.clone()),
        Input::Value(
            "extraction profile".into(),
            b"v1: scip_dart --output <temporary> .; \
              dart --suppress-analytics analyze --format=machine ."
                .to_vec(),
        ),
    ];
    for sdk in [
        sdk,
        launcher
            .dart
            .parent()
            .and_then(Path::parent)
            .context("launcher has no SDK")?,
    ] {
        inputs.push(Input::Tree(sdk.join("bin")));
        inputs.push(Input::Tree(sdk.join("lib")));
        inputs.push(Input::File(sdk.join("version")));
    }
    package_inputs(&launcher.config, &mut inputs)?;
    package_inputs(&resolution.path, &mut inputs)?;
    for package in resolution.packages.values() {
        inputs.push(Input::Tree(package.root.clone()));
        options(&package.root, resolution, &mut inputs)?;
    }
    for parent in dir.ancestors() {
        for name in ["pubspec.yaml", "pubspec.lock", "pubspec_overrides.yaml"] {
            inputs.push(Input::File(parent.join(name)));
        }
    }
    options(dir, resolution, &mut inputs)?;
    Ok(inputs)
}

fn package_inputs(config: &Path, inputs: &mut Vec<Input>) -> Result<()> {
    inputs.push(Input::File(config.to_path_buf()));
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(config)?)?;
    for package in json["packages"]
        .as_array()
        .context("launcher has no resolved packages")?
    {
        inputs.push(Input::Tree(local_uri(
            config.parent().context("resolution has no parent")?,
            package["rootUri"]
                .as_str()
                .context("package has no rootUri")?,
        )?));
    }
    Ok(())
}

fn options(dir: &Path, resolution: &Resolution, inputs: &mut Vec<Input>) -> Result<()> {
    for parent in dir.ancestors() {
        for name in ["analysis_options.yaml", ".analysis_options"] {
            let path = parent.join(name);
            inputs.push(Input::File(path.clone()));
            if path.exists() {
                included(&path, resolution, inputs, &mut BTreeSet::new())?;
            }
        }
    }
    Ok(())
}

fn included(
    path: &Path,
    resolution: &Resolution,
    inputs: &mut Vec<Input>,
    active: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let path = dunce::canonicalize(path)?;
    if !active.insert(path.clone()) {
        bail!("cyclic analyzer configuration; cache bypassed");
    }
    inputs.push(Input::File(path.clone()));
    let yaml: serde_norway::Value = serde_norway::from_slice(&std::fs::read(&path)?)?;
    if !yaml["plugins"].is_null() || !yaml["analyzer"]["plugins"].is_null() {
        bail!("analyzer plugin inputs cannot be fingerprinted completely; cache bypassed");
    }
    let includes: Vec<&str> = match &yaml["include"] {
        serde_norway::Value::Null => Vec::new(),
        serde_norway::Value::String(s) => vec![s],
        serde_norway::Value::Sequence(items) => items
            .iter()
            .map(|v| v.as_str().context("invalid analyzer include"))
            .collect::<Result<_>>()?,
        _ => bail!("invalid analyzer include"),
    };
    for include in includes {
        let next = if let Some(uri) = include.strip_prefix("package:") {
            let (name, relative) = uri.split_once('/').context("invalid package include")?;
            let package = resolution
                .packages
                .get(name)
                .context("unresolved analyzer include")?;
            local_uri(&package.root.join(&package.uri_path), relative)?
        } else {
            local_uri(path.parent().context("options have no parent")?, include)?
        };
        included(&next, resolution, inputs, active)?;
    }
    active.remove(&path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolved_inputs_track_sdk_indexer_hidden_inputs_and_local_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let sdk = tmp.path().join("sdk");
        let runner = tmp.path().join("runner");
        let dependency = tmp.path().join("local");
        for path in [
            root.join(".dart_tool"),
            sdk.join("bin"),
            sdk.join("lib"),
            runner.clone(),
            dependency.clone(),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        for (path, text) in [
            (sdk.join("bin/dart"), "sdk"),
            (sdk.join("version"), "3.13.7"),
            (sdk.join("lib/libraries.json"), "{}"),
            (runner.join("indexer.dart"), "indexer"),
            (dependency.join("dep.dart"), "class Dep {}"),
            (root.join("app.dart"), "void main() {}"),
            (tmp.path().join("outside.yaml"), "analyzer: {}"),
            (
                root.join("analysis_options.yaml"),
                "include: ../outside.yaml",
            ),
        ] {
            std::fs::write(path, text).unwrap();
        }
        let config = root.join(".dart_tool/package_config.json");
        let json = serde_json::json!({
            "configVersion": 2,
            "packages": [{"name": "runner", "rootUri": runner.to_str().unwrap()}]
        });
        std::fs::write(&config, json.to_string()).unwrap();
        let binary = tmp.path().join("scip_dart");
        let dart = sdk.join("bin/dart");
        let source = runner.join("indexer.dart");
        let header = format!(
            "# easement-scip-dart-v1 {}",
            serde_json::json!({"dart": dart, "config": config, "source": source})
        );
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\n{header}\nexec {} {} {} \"$@\"\n",
                quote(dart.to_str().unwrap()),
                quote(&format!("--packages={}", config.display())),
                quote(source.to_str().unwrap())
            ),
        )
        .unwrap();
        let resolution = Resolution {
            path: config.clone(),
            packages: BTreeMap::from([(
                "local".into(),
                Package {
                    root: dependency.clone(),
                    uri_path: String::new(),
                },
            )]),
        };
        let key = || {
            let inputs = collect(&binary, &sdk, &root, &resolution).unwrap();
            let mut hash = StableHasher::default();
            super::super::super::cache::hash_inputs(inputs, &root, &mut hash).unwrap();
            hash.finish()
        };
        let mut previous = key();
        assert_eq!(previous, key());
        for path in [
            root.join("app.dart"),
            root.join("pubspec.lock"),
            config,
            sdk.join("bin/dart"),
            source,
            dependency.join("dep.dart"),
            tmp.path().join("outside.yaml"),
        ] {
            let mut bytes = std::fs::read(&path).unwrap_or_default();
            bytes.extend_from_slice(b"\n");
            std::fs::write(path, bytes).unwrap();
            let next = key();
            assert_ne!(previous, next);
            previous = next;
        }
        std::fs::write(
            root.join("analysis_options.yaml"),
            "plugins: {custom: {path: ../plugin}}\n",
        )
        .unwrap();
        assert!(collect(&binary, &sdk, &root, &resolution).is_err());
    }
}
