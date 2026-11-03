//! Conservative keys include resolved inputs outside the ordinary source walk.
use super::*;

#[derive(Deserialize)]
struct Launcher {
    dart: PathBuf,
    config: PathBuf,
    source: PathBuf,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn field(hash: &mut blake3::Hasher, value: &[u8]) {
    hash.update(&(value.len() as u64).to_le_bytes());
    hash.update(value);
}

pub(super) fn fingerprint(
    binary: &Path,
    sdk: &Path,
    dir: &Path,
    resolution: &Resolution,
) -> Result<String> {
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
    let mut hash = blake3::Hasher::new();
    hash.update(b"easement-dart-cache-v1");
    field(
        &mut hash,
        b"scip_dart --output <temporary> .; dart --suppress-analytics analyze --format=machine .",
    );
    field(&mut hash, wrapper.as_bytes());
    let mut environment: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| key != "EASEMENT_CACHE_DIR")
        .collect();
    environment.sort();
    for (key, value) in environment {
        field(&mut hash, format!("{key:?}").as_bytes());
        field(&mut hash, format!("{value:?}").as_bytes());
    }
    let mut seen = BTreeSet::new();
    tree(dir, &mut seen, &mut hash)?;
    for sdk in [
        sdk,
        launcher
            .dart
            .parent()
            .and_then(Path::parent)
            .context("launcher has no SDK")?,
    ] {
        for name in ["bin", "lib", "version"] {
            tree(&sdk.join(name), &mut seen, &mut hash)?;
        }
    }
    tree(&launcher.source, &mut seen, &mut hash)?;
    inputs(&launcher.config, &mut seen, &mut hash)?;
    inputs(&resolution.path, &mut seen, &mut hash)?;
    for package in resolution.packages.values() {
        tree(&package.root, &mut seen, &mut hash)?;
        options(&package.root, resolution, &mut seen, &mut hash)?;
    }
    for parent in dir.ancestors() {
        for name in ["pubspec.yaml", "pubspec.lock", "pubspec_overrides.yaml"] {
            let path = parent.join(name);
            field(&mut hash, path.to_string_lossy().as_bytes());
            if path.exists() {
                tree(&path, &mut seen, &mut hash)?;
            }
        }
    }
    options(dir, resolution, &mut seen, &mut hash)?;
    Ok(hash.finalize().to_hex().to_string())
}

fn tree(path: &Path, seen: &mut BTreeSet<PathBuf>, hash: &mut blake3::Hasher) -> Result<()> {
    field(hash, path.as_os_str().as_encoded_bytes());
    let canonical = dunce::canonicalize(path)?;
    field(hash, canonical.as_os_str().as_encoded_bytes());
    if !seen.insert(canonical.clone()) {
        hash.update(b"alias");
        return Ok(());
    }
    if canonical.is_dir() {
        let mut children = std::fs::read_dir(&canonical)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        hash.update(b"directory");
        hash.update(&(children.len() as u64).to_le_bytes());
        for entry in children {
            if entry.file_name() == ".git" {
                continue;
            }
            tree(&entry.path(), seen, hash)?;
        }
    } else {
        if !std::fs::metadata(&canonical)?.is_file() {
            bail!("non-file Dart input cannot be fingerprinted safely");
        }
        use std::io::Read;
        let mut file = std::fs::File::open(canonical)?;
        let mut buffer = [0; 65536];
        let mut content = blake3::Hasher::new();
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            content.update(&buffer[..read]);
        }
        hash.update(b"file");
        hash.update(content.finalize().as_bytes());
    }
    Ok(())
}

fn inputs(config: &Path, seen: &mut BTreeSet<PathBuf>, hash: &mut blake3::Hasher) -> Result<()> {
    tree(config, seen, hash)?;
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(config)?)?;
    for package in json["packages"]
        .as_array()
        .context("launcher has no resolved packages")?
    {
        let path = local_uri(
            config.parent().context("resolution has no parent")?,
            package["rootUri"]
                .as_str()
                .context("package has no rootUri")?,
        )?;
        tree(&path, seen, hash)?;
    }
    Ok(())
}

fn options(
    dir: &Path,
    resolution: &Resolution,
    seen: &mut BTreeSet<PathBuf>,
    hash: &mut blake3::Hasher,
) -> Result<()> {
    for parent in dir.ancestors() {
        for name in ["analysis_options.yaml", ".analysis_options"] {
            let path = parent.join(name);
            field(hash, path.to_string_lossy().as_bytes());
            if path.exists() {
                included(&path, resolution, seen, &mut BTreeSet::new(), hash)?;
            }
        }
    }
    Ok(())
}

fn included(
    path: &Path,
    resolution: &Resolution,
    seen: &mut BTreeSet<PathBuf>,
    active: &mut BTreeSet<PathBuf>,
    hash: &mut blake3::Hasher,
) -> Result<()> {
    let path = dunce::canonicalize(path)?;
    if !active.insert(path.clone()) {
        bail!("cyclic analyzer configuration; cache bypassed");
    }
    tree(&path, seen, hash)?;
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
        included(&next, resolution, seen, active, hash)?;
    }
    active.remove(&path);
    Ok(())
}

pub(super) fn write(path: &Path, entry: &Entry) {
    let Some(parent) = path.parent() else {
        return;
    };
    let result = || -> Result<()> {
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(file.as_file_mut(), entry)?;
        file.persist(path)?;
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && let Some((prefix, _)) = name.rsplit_once('-')
        {
            super::super::prune(parent, &format!("{prefix}-"));
        }
        Ok(())
    };
    let _ = result();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dart_fingerprint_tracks_sdk_indexer_hidden_inputs_and_local_dependencies() {
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
        let json = serde_json::json!({"configVersion": 2, "packages": [{"name": "runner", "rootUri": runner.to_str().unwrap()}]});
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
        let key = || fingerprint(&binary, &sdk, &root, &resolution).unwrap();
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
        assert!(fingerprint(&binary, &sdk, &root, &resolution).is_err());
    }
}
