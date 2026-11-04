//! Opt-in semantic companion execution and independent conservative caching.
use super::*;
use crate::dart::facts::Facts;
use std::io::Write;
use std::process::Stdio;

pub(crate) struct Extracted {
    pub facts: Vec<Facts>,
    pub ran: usize,
    pub cached: usize,
}

pub(crate) fn run(
    root: &Path,
    files: &[String],
    enabled: &[String],
    mode: CacheUse,
) -> Result<Extracted> {
    let mut result = Extracted {
        facts: Vec::new(),
        ran: 0,
        cached: 0,
    };
    if enabled.is_empty() {
        return Ok(result);
    }
    let binary = which::which("easement_dart_semantics").context("easement_dart_semantics is missing; run `mise run install-dart-semantics` in an easement source checkout and put target/dart-indexer on PATH (see README)")?;
    let version = Command::new(&binary).arg("--version").output().context("cannot run easement_dart_semantics --version; reinstall with mise run install-dart-semantics")?;
    if !version.status.success()
        || String::from_utf8_lossy(&version.stdout).trim()
            != "easement_dart_semantics 1.1.0 facts=2 analyzer=14.4.0"
    {
        bail!(
            "incompatible Dart semantic companion; run mise run install-dart-semantics (requires facts=2, companion 1.1.0, analyzer 14.4.0)"
        );
    }
    let sdk = sdk(root)?;
    let enabled_roots = roots(root, enabled)?;
    for dir in enabled_roots {
        let resolution = resolution(&dir)?;
        let package_files: Vec<_> = files
            .iter()
            .filter(|file| {
                registry::is(Path::new(file), &registry::DART)
                    && roots(root, &[(*file).clone()]).is_ok_and(|roots| roots.contains(&dir))
            })
            .cloned()
            .collect();
        // Catalog local resolved dependencies for stable explicit selectors and
        // path defaults, even when their declarations are not exposed by an API.
        let catalog: Vec<_> = files
            .iter()
            .filter(|file| {
                registry::is(Path::new(file), &registry::DART)
                    && resolution.packages.values().any(|package| {
                        package.root.starts_with(root) && root.join(file).starts_with(&package.root)
                    })
            })
            .cloned()
            .collect();
        let fingerprint = || -> Result<String> {
            let mut hash = blake3::Hasher::new();
            hash.update(b"easement-dart-semantics-facts-v2");
            hash.update(cache::semantic_fingerprint(&binary, &sdk, &dir, &resolution)?.as_bytes());
            hash.update(&serde_json::to_vec(&(root, &package_files, &catalog))?);
            Ok(hash.finalize().to_hex().to_string())
        };
        let key = (mode != CacheUse::Off)
            .then(&fingerprint)
            .transpose()
            .ok()
            .flatten();
        let path = key.as_ref().and_then(|key| {
            cache_dir().map(|p| {
                p.join(format!(
                    "dart-semantics-v2-{:016x}-{key}.json",
                    content_hash(dir.as_os_str().as_encoded_bytes())
                ))
            })
        });
        let cached = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Facts>(&b).ok())
            .filter(compatible);
        let mut facts = if let Some(facts) = cached {
            result.cached += 1;
            facts
        } else {
            if mode == CacheUse::Only {
                return Err(NotCached {
                    language: "dart semantic",
                    dir: crate::scan::slash(dir.strip_prefix(root)?),
                }
                .into());
            }
            let request = serde_json::json!({"root": root, "package": dir, "sdk": sdk, "files": package_files, "catalog": catalog});
            let mut child = Command::new(&binary)
                .current_dir(&dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("cannot run Dart semantic companion")?;
            child
                .stdin
                .take()
                .context("companion stdin unavailable")?
                .write_all(&serde_json::to_vec(&request)?)?;
            let output = child.wait_with_output()?;
            if !output.status.success() {
                bail!(
                    "Dart semantic companion failed in {}: {}",
                    dir.display(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let facts: Facts = serde_json::from_slice(&output.stdout).context(
                "malformed Dart semantic facts; reinstall with mise run install-dart-semantics",
            )?;
            if !compatible(&facts) {
                bail!(
                    "incompatible Dart semantic fact format or extractor identity; reinstall with mise run install-dart-semantics"
                );
            }
            validate(&facts)?;
            if let Some(path) = path
                && key == fingerprint().ok()
            {
                cache::write(&path, &facts);
            }
            result.ran += 1;
            facts
        };
        validate(&facts)?;
        for file in &package_files {
            if !facts.files.contains(file) {
                facts.gaps.push(crate::dart::facts::Gap { file: file.clone(), line: 1, reason: "Dart semantic companion did not analyze this requested file; repair analyzer exclusions or package resolution".into() });
            }
        }
        result.facts.push(facts);
    }
    Ok(result)
}

fn compatible(facts: &Facts) -> bool {
    facts.format_version == 2
        && facts.extractor_version == "1.1.0"
        && facts.analyzer_version == "14.4.0"
        && facts.sdk_version.starts_with("3.")
}

fn validate(facts: &Facts) -> Result<()> {
    fn location(at: &crate::dart::facts::Location) -> Result<()> {
        let path = Path::new(&at.file);
        if path.is_absolute()
            || at.file.contains('\\')
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            || at.line == 0
            || at.column == 0
        {
            bail!("invalid project-relative source location in Dart semantic facts");
        }
        Ok(())
    }
    fn ty(value: &crate::dart::facts::Type) -> Result<()> {
        if let Some(at) = &value.origin {
            location(at)?;
        }
        for child in &value.children {
            ty(child)?;
        }
        Ok(())
    }
    for file in &facts.files {
        location(&crate::dart::facts::Location {
            file: file.clone(),
            line: 1,
            column: 1,
        })?;
    }
    for class in &facts.ui_classes {
        location(&class.location)?;
    }
    for callable in &facts.callables {
        location(&callable.location)?;
        ty(&callable.return_type)?;
        for returned in &callable.returns {
            location(&returned.location)?;
            location(&returned.origin)?;
            ty(&returned.ty)?;
        }
    }
    for declaration in &facts.declarations {
        location(&declaration.location)?;
        if declaration.location.file.is_empty() {
            bail!("Dart semantic declaration has no project location");
        }
        for used in declaration.api.iter().chain(&declaration.uses) {
            location(&used.location)?;
            location(&used.origin)?;
            ty(&used.ty)?;
        }
        for supertype in &declaration.supertypes {
            ty(supertype)?;
        }
    }
    for gap in &facts.gaps {
        location(&crate::dart::facts::Location {
            file: gap.file.clone(),
            line: gap.line,
            column: 1,
        })?;
    }
    Ok(())
}

pub(crate) fn diagnosis() -> Diagnosis {
    let path = which::which("easement_dart_semantics").ok();
    let output = path
        .as_ref()
        .and_then(|p| Command::new(p).arg("--version").output().ok());
    let version = output
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let mut problems = Vec::new();
    if path.is_some()
        && !output.as_ref().is_some_and(|o| {
            o.status.success()
                && version.as_deref()
                    == Some("easement_dart_semantics 1.1.0 facts=2 analyzer=14.4.0")
        })
    {
        problems.push("incompatible or unexecutable Dart semantic companion; reinstall with mise run install-dart-semantics".into());
    }
    Diagnosis {
        language: "dart semantics",
        binary: "easement_dart_semantics",
        found: path.is_some(),
        path: path.map(|p| p.display().to_string()),
        version,
        install: "mise run install-dart-semantics",
        mise: None,
        problems,
    }
}
