//! Dart-only orchestration and coverage. SCIP owns semantic resolution.
use super::*;
use crate::scan::AnalysisGap;
use serde::{Deserialize, Serialize};

mod cache;
pub(crate) mod directives;
pub(crate) mod private;
pub(crate) mod semantics;
use directives::Kind;

#[derive(Debug)]
struct Package {
    root: PathBuf,
    uri_path: String,
}
struct Resolution {
    path: PathBuf,
    packages: BTreeMap<String, Package>,
}

pub(crate) fn names_package(source: &str, package: &str) -> bool {
    directives::read(source).is_ok_and(|directives| {
        directives.iter().any(|d| {
            matches!(d.kind, Kind::Import | Kind::Export)
                && d.uri
                    .as_deref()
                    .is_some_and(|uri| registry::Manifest::Pub.package(uri) == package)
        })
    })
}

fn roots(root: &Path, files: &[String]) -> Result<Vec<PathBuf>> {
    let mut roots = BTreeSet::new();
    for file in files
        .iter()
        .filter(|f| registry::is(Path::new(f), &registry::DART))
    {
        let absolute = root.join(file);
        let owner = absolute
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(root))
            .find(|p| p.join("pubspec.yaml").is_file());
        roots.insert(owner.with_context(|| format!("{file} has no owning pubspec.yaml inside the analysis root; run from the package or workspace root"))?.to_path_buf());
    }
    Ok(roots.into_iter().collect())
}

/// Pub resolution uses file URIs, with relative URIs resolved against its file.
fn local_uri(base: &Path, uri: &str) -> Result<PathBuf> {
    let uri = if let Some(file) = uri.strip_prefix("file:") {
        if let Some(authority) = file.strip_prefix("//") {
            if authority.starts_with("localhost/") {
                &authority["localhost".len()..]
            } else if authority.starts_with('/') {
                authority
            } else {
                bail!("unsupported remote file URI in Pub resolution: {uri}");
            }
        } else {
            file
        }
    } else {
        uri
    };
    if uri.contains(['?', '#']) || (uri.contains(':') && !Path::new(uri).is_absolute()) {
        bail!("unsupported non-file Pub resolution URI: {uri}");
    }
    let bytes = uri.as_bytes();
    let mut decoded = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = uri
                .get(at + 1..at + 3)
                .context("invalid percent escape in Pub URI")?;
            decoded.push(u8::from_str_radix(hex, 16).context("invalid percent escape in Pub URI")?);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    let path = PathBuf::from(String::from_utf8(decoded).context("Pub URI is not UTF-8")?);
    let path = base.join(path);
    if path.exists() {
        return dunce::canonicalize(path).context("unreadable Pub resolution path");
    }
    // Source-free workspace containers legitimately have no packageUri directory.
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if component == std::path::Component::ParentDir {
            normalized.pop();
        } else if component != std::path::Component::CurDir {
            normalized.push(component);
        }
    }
    Ok(normalized)
}

fn resolution(dir: &Path) -> Result<Resolution> {
    let path = dir.ancestors().map(|p| p.join(".dart_tool/package_config.json")).find(|p| p.is_file()).context("missing .dart_tool/package_config.json (including ancestor workspace resolution); run dart pub get or flutter pub get")?;
    let text = std::fs::read(&path).context("cannot read Dart package configuration")?;
    let json: serde_json::Value = serde_json::from_slice(&text)
        .context("invalid Dart package configuration; run dart pub get or flutter pub get")?;
    if json["configVersion"] != 2 {
        bail!(
            "unsupported Dart package configuration version; run dart pub get or flutter pub get"
        );
    }
    let mut packages = BTreeMap::new();
    for entry in json["packages"]
        .as_array()
        .context("Dart package configuration has no packages")?
    {
        let name = entry["name"]
            .as_str()
            .context("Dart package configuration has an unnamed package")?;
        let root = local_uri(
            path.parent()
                .context("package configuration has no directory")?,
            entry["rootUri"]
                .as_str()
                .context("Dart package has no rootUri")?,
        )?;
        if !root.is_dir() {
            bail!(
                "Pub resolution points to missing package {name}; run dart pub get or flutter pub get"
            );
        }
        let uri_root = local_uri(&root, entry["packageUri"].as_str().unwrap_or(""))?;
        let uri_path = crate::scan::slash(
            uri_root
                .strip_prefix(&root)
                .context("Dart packageUri is outside its package root")?,
        );
        if packages
            .insert(name.to_string(), Package { root, uri_path })
            .is_some()
        {
            bail!("duplicate package {name} in Dart resolution");
        }
    }
    let manifest: serde_norway::Value =
        serde_norway::from_slice(&std::fs::read(dir.join("pubspec.yaml"))?)
            .context("invalid pubspec.yaml")?;
    let own = manifest["name"]
        .as_str()
        .context("pubspec.yaml has no package name")?;
    if !packages.get(own).is_some_and(|p| p.root == dir) {
        bail!(
            "Dart resolution does not map {own} to its owning package; run dart pub get or flutter pub get"
        );
    }
    Ok(Resolution { path, packages })
}

fn sdk(cwd: &Path) -> Result<PathBuf> {
    let binary = which::which("dart")
        .context("Dart SDK is missing; install Dart or Flutter and put dart on PATH")?;
    // Dart can be a mise shim. Reuse the existing resolver without asking
    // rustup, which does not manage Dart executables.
    let resolvers: Vec<_> = multiplexers()
        .into_iter()
        .filter(|m| m.path.file_stem().is_some_and(|s| s == "mise"))
        .collect();
    let Runs::Binary((Some(dart), _, _)) = what_runs(&binary, "dart", cwd, &resolvers) else {
        bail!(
            "cannot resolve the Dart SDK shim; install or activate the selected Dart/Flutter toolchain"
        );
    };
    let bin = dart.parent().context("dart has no directory")?;
    for candidate in [
        bin.parent().unwrap_or(bin).to_path_buf(),
        bin.join("cache/dart-sdk"),
    ] {
        if candidate.join("version").is_file() && candidate.join("lib/libraries.json").is_file() {
            let version = std::fs::read_to_string(candidate.join("version"))?;
            let mut numbers = version.trim().split('.');
            if numbers.next() != Some("3")
                || numbers
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .is_none_or(|v| v < 12)
            {
                bail!(
                    "scip_dart requires Dart >=3.12.0 <4.0.0; install the pinned Dart/Flutter SDK"
                );
            }
            return Ok(candidate);
        }
    }
    bail!(
        "cannot identify the Dart SDK behind {}; put a Dart SDK bin directory on PATH",
        dart.display()
    )
}

fn sdk_modules(sdk: &Path, resolution: &Resolution) -> Result<BTreeMap<String, BTreeSet<PathBuf>>> {
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sdk.join("lib/libraries.json"))?)?;
    let mut modules: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    for platform in json
        .as_object()
        .context("invalid Dart SDK library catalog")?
        .values()
    {
        for (name, library) in platform["libraries"].as_object().into_iter().flatten() {
            if let Some(uri) = library["uri"].as_str() {
                modules
                    .entry(format!("dart:{name}"))
                    .or_default()
                    .insert(sdk.join("lib").join(uri));
            }
        }
    }
    // Flutter's SDK library catalog is supplied by its resolved sky_engine package.
    if let Some(package) = resolution.packages.get("sky_engine") {
        let base = package.root.join(&package.uri_path);
        let path = base.join("_embedder.yaml");
        if path.is_file() {
            let yaml: serde_norway::Value = serde_norway::from_slice(&std::fs::read(path)?)?;
            for (name, uri) in yaml["embedded_libs"].as_mapping().into_iter().flatten() {
                modules
                    .entry(name.as_str().context("invalid SDK library name")?.into())
                    .or_default()
                    .insert(base.join(uri.as_str().context("invalid SDK library URI")?));
            }
        }
    }
    Ok(modules)
}

pub(super) fn needs(_: &Path, dir: &Path, _: &str) -> Result<()> {
    sdk(dir)?;
    resolution(dir)?;
    Ok(())
}

pub(super) fn diagnosis(root: &Path, files: &[String], language: &Indexer) -> Diagnosis {
    let binary = which::which(language.binary).ok();
    let mut problems = roots(root, files)
        .map(|dirs| {
            let mut problems = Vec::new();
            for dir in dirs {
                if let Err(e) = needs(root, &dir, "") {
                    problems.push(format!("{}: {e:#}", dir.display()));
                }
            }
            problems
        })
        .unwrap_or_else(|e| vec![e.to_string()]);
    let probe = binary
        .as_ref()
        .map(|path| Command::new(path).arg("--version").output());
    let version = match probe {
        Some(Ok(output)) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(str::to_string),
        Some(Ok(output)) => {
            problems.push(format!(
                "scip_dart --version failed ({}); reinstall the pinned indexer: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .next()
                    .unwrap_or("no diagnostic")
            ));
            None
        }
        Some(Err(error)) => {
            problems.push(format!("cannot execute scip_dart: {error}"));
            None
        }
        None => None,
    };
    Diagnosis {
        language: language.name(),
        binary: language.binary,
        found: binary.is_some(),
        version,
        path: binary.map(|p| p.display().to_string()),
        install: language.install,
        mise: language.mise,
        problems,
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    version: u32,
    index: Vec<u8>,
    gaps: Vec<AnalysisGap>,
}

pub(super) fn run(
    root: &Path,
    files: &[String],
    excluded: &[String],
    mode: CacheUse,
) -> Result<Option<Indexed>> {
    if !files
        .iter()
        .any(|f| registry::is(Path::new(f), &registry::DART))
    {
        return Ok(None);
    }
    let dirs = roots(root, files)?;
    let binary = which::which("scip_dart").context("scip_dart is missing; run `dart pub global activate scip_dart 1.8.1` or install the pinned launcher from an easement source checkout (see README), then run easement doctor")?;
    let sdk = sdk(root)?;
    let mut parts = Vec::new();
    let (mut ran, mut cached) = (0, 0);
    let mut gaps = Vec::new();
    let all: BTreeSet<&str> = files.iter().chain(excluded).map(String::as_str).collect();
    for dir in dirs {
        let resolution = resolution(&dir).with_context(|| format!("Dart package {} is not prepared; run dart pub get or flutter pub get in that directory", dir.display()))?;
        let base = dir.strip_prefix(root)?;
        let key = if mode == CacheUse::Off {
            None
        } else {
            cache::fingerprint(&binary, &sdk, &dir, &resolution).ok()
        };
        let path = key.as_ref().and_then(|key| {
            cache_dir().map(|p| {
                p.join(format!(
                    "dart-v1-{:016x}-{key}.json",
                    content_hash(dir.as_os_str().as_encoded_bytes())
                ))
            })
        });
        let entry = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Entry>(&b).ok())
            .filter(|e| e.version == 1);
        let (documents, entry) = if let Some(entry) = entry
            && let Ok(documents) = scip::parse(&entry.index, root, base)
            && !documents.is_empty()
        {
            cached += 1;
            (documents, entry)
        } else {
            if mode == CacheUse::Only {
                return Err(NotCached {
                    language: "dart",
                    dir: crate::scan::slash(base),
                }
                .into());
            }
            let temp = tempfile::tempdir()?;
            let output = Command::new(&binary)
                .args([
                    std::ffi::OsStr::new("--output"),
                    temp.path().join("index.scip").as_os_str(),
                    std::ffi::OsStr::new("."),
                ])
                .current_dir(&dir)
                .output()
                .context("cannot run scip_dart")?;
            if !output.status.success() {
                bail!(
                    "scip_dart failed in {} ({}):\n{}{}",
                    dir.display(),
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let index = std::fs::read(temp.path().join("index.scip"))
                .context("scip_dart wrote no index")?;
            let documents = scip::parse(&index, root, base)?;
            if documents.is_empty() {
                bail!(
                    "scip_dart indexed no files in {}; check analyzer exclusions and package resolution",
                    dir.display()
                );
            }
            let gaps = analyze(root, &dir, &sdk)?;
            let entry = Entry {
                version: 1,
                index,
                gaps,
            };
            // Do not publish a cache entry if input changed while tools ran.
            if let Some(path) = path
                && key == cache::fingerprint(&binary, &sdk, &dir, &resolution).ok()
            {
                cache::write(&path, &entry);
            }
            ran += 1;
            (documents, entry)
        };
        gaps.extend(entry.gaps);
        let mut documents = documents;
        let sdk_modules = sdk_modules(&sdk, &resolution)?;
        for document in &mut documents {
            for reference in &mut document.references {
                if !scip::dart_symbol_supported(&reference.symbol) {
                    gaps.push(gap(&document.path, Some(reference.line), "unsupported Dart SCIP symbol; its dependency or symbol use was not verified"));
                    continue;
                }
                let package = reference.symbol.split(' ').nth(2).unwrap_or_default();
                reference.dart_module = scip::dart_module(
                    &reference.symbol,
                    resolution
                        .packages
                        .get(package)
                        .map(|p| p.uri_path.as_str()),
                );
                reference.dart_internal_package = resolution
                    .packages
                    .get(package)
                    .is_some_and(|p| p.root.starts_with(root));
                if let Some(namespace) = scip::dart_file(&reference.symbol) {
                    if package.starts_with("dart:") {
                        let suffix = namespace
                            .strip_prefix(package)
                            .unwrap_or_default()
                            .trim_start_matches('/');
                        let exists = sdk_modules.get(package).is_some_and(|paths| {
                            paths.iter().any(|path| {
                                if suffix.is_empty() {
                                    path.is_file()
                                } else {
                                    path.parent()
                                        .is_some_and(|base| base.join(suffix).is_file())
                                }
                            })
                        });
                        if !exists {
                            gaps.push(gap(&document.path, Some(reference.line), "Dart SCIP references a missing SDK library target; repair SDK preparation and run with --no-cache"));
                        }
                    } else if let Some(package) = resolution.packages.get(package) {
                        let target = package.root.join(namespace);
                        if !target.is_file() {
                            gaps.push(gap(&document.path, Some(reference.line), "Dart SCIP references a missing package file; repair imports or generated input and run with --no-cache"));
                        } else if let Ok(relative) = target.strip_prefix(root) {
                            let relative = crate::scan::slash(relative);
                            reference.dart_target_excluded = excluded.contains(&relative);
                            reference.dart_target = Some(relative);
                        }
                    }
                }
                if reference.symbol.starts_with("scip-dart ")
                    && reference.dart_module.is_none()
                    && !reference.dart_internal_package
                {
                    gaps.push(gap(&document.path, Some(reference.line), "Dart reference cannot be mapped through packageUri; its external module was not fully verified"));
                }
            }
        }
        parts.push(documents);
    }
    let mut documents = Documents::merged(parts)?;
    gaps.extend(directive_gaps(root, &mut documents.0)?);
    gaps.sort();
    gaps.dedup();
    let indexed: BTreeSet<&str> = documents.0.iter().map(|d| d.path.as_str()).collect();
    let uncovered = all
        .into_iter()
        .filter(|f| {
            registry::is(Path::new(f), &registry::DART)
                && files.iter().any(|s| s == f)
                && !indexed.contains(f)
        })
        .map(str::to_string)
        .collect();
    Ok(Some(Indexed {
        documents,
        ran,
        cached,
        uncovered,
        analysis_partial: gaps,
    }))
}

fn analyze(root: &Path, dir: &Path, sdk: &Path) -> Result<Vec<AnalysisGap>> {
    let output =
        Command::new(
            sdk.join("bin")
                .join(if cfg!(windows) { "dart.exe" } else { "dart" }),
        )
        .args(["--suppress-analytics", "analyze", "--format=machine", "."])
        .current_dir(dir)
        .output()
        .context("cannot run dart analyze")?;
    let mut gaps = Vec::new();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut records = 0;
    for line in text.lines().filter(|s| !s.trim().is_empty()) {
        let fields = machine_fields(line);
        if fields.len() != 8 || !matches!(fields[0].as_str(), "ERROR" | "WARNING" | "INFO") {
            bail!("unrecognized dart analyze machine output: {line}");
        }
        records += 1;
        if fields[0] != "ERROR" {
            continue;
        }
        let file = Path::new(&fields[3])
            .strip_prefix(root)
            .map(crate::scan::slash)
            .unwrap_or_else(|_| {
                crate::scan::slash(dir.strip_prefix(root).unwrap_or(Path::new("pubspec.yaml")))
            });
        gaps.push(AnalysisGap { file, line: Some(fields[4].parse().context("invalid Dart diagnostic line")?), reason: format!("Dart analyzer {}: {}; repair source or dependency preparation and run with --no-cache", fields[2], fields[7]) });
    }
    if !output.status.success() && (records == 0 || !matches!(output.status.code(), Some(1..=3))) {
        bail!("dart analyze failed ({}): {text}", output.status);
    }
    Ok(gaps)
}

fn machine_fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|c| matches!(c, '|' | '\\')) {
            fields
                .last_mut()
                .expect("one field")
                .push(chars.next().expect("peeked"));
        } else if c == '|' {
            fields.push(String::new());
        } else {
            fields.last_mut().expect("one field").push(c);
        }
    }
    fields
}

fn directive_gaps(root: &Path, documents: &mut [Document]) -> Result<Vec<AnalysisGap>> {
    let defined: BTreeMap<String, String> = documents
        .iter()
        .flat_map(|d| {
            d.definitions
                .iter()
                .filter(|s| s.starts_with("scip-dart ") && s.ends_with('/'))
                .map(|s| (s.clone(), d.path.clone()))
        })
        .collect();
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut inventories = Vec::new();
    let mut gaps = Vec::new();
    for document in documents.iter() {
        if !registry::is(Path::new(&document.path), &registry::DART) {
            inventories.push(Vec::new());
            continue;
        }
        let source = std::fs::read_to_string(root.join(&document.path))
            .with_context(|| format!("cannot inventory Dart directives in {}", document.path))?;
        let directives = match directives::read(&source) {
            Ok(d) => d,
            Err(()) => {
                gaps.push(gap(&document.path, None, "Dart directive inventory could not read this source; repair syntax and run with --no-cache"));
                Vec::new()
            }
        };
        for directive in &directives {
            let reference = document.references.iter().find(|r| {
                r.line == directive.line && r.spelled == directive.uri && r.symbol.ends_with('/')
            });
            if directive.kind == Kind::Part
                && let Some(reference) = reference
                && let Some(part) = defined.get(&reference.symbol)
            {
                owners
                    .entry(part.clone())
                    .or_default()
                    .push(document.path.clone());
            }
            if directive.conditional {
                gaps.push(gap(&document.path, Some(directive.line), "conditional Dart directive: SCIP does not cover every branch; verify other platform branches separately"));
            }
            if directive.uri.is_some() && reference.is_none() {
                gaps.push(gap(&document.path, Some(directive.line), "Dart directive has no matching SCIP reference; repair resolution or generated input and run with --no-cache"));
            }
            if directive.uri.is_none() && directive.kind != Kind::NamedPartOf {
                gaps.push(gap(
                    &document.path,
                    Some(directive.line),
                    "unsupported Dart directive URI; its dependency was not verified",
                ));
            }
        }
        inventories.push(directives);
    }
    for (document, directives) in documents.iter_mut().zip(inventories) {
        document.dart_library = if directives
            .iter()
            .any(|d| matches!(d.kind, Kind::PartOf | Kind::NamedPartOf))
        {
            let owners: BTreeSet<&String> =
                owners.get(&document.path).into_iter().flatten().collect();
            (owners.len() == 1).then(|| (*owners.first().expect("one owner")).clone())
        } else {
            Some(document.path.clone())
        };
        for directive in directives.iter().filter(|d| d.kind == Kind::NamedPartOf) {
            let owners: BTreeSet<&String> =
                owners.get(&document.path).into_iter().flatten().collect();
            if owners.len() == 1 {
                let owner = owners.first().expect("one owner");
                let symbol = defined
                    .iter()
                    .find(|(_, path)| path == owner)
                    .map(|(s, _)| s.clone())
                    .expect("indexed owner");
                document.references.push(scip::Reference {
                    line: directive.line,
                    symbol,
                    spelled: None,
                    dart_module: None,
                    dart_internal_package: true,
                    dart_target: None,
                    dart_target_excluded: false,
                });
            } else {
                gaps.push(gap(&document.path, Some(directive.line), "named part of has no unique indexed owning library; its dependency was not verified"));
            }
        }
    }
    Ok(gaps)
}

fn gap(file: &str, line: Option<usize>, reason: &str) -> AnalysisGap {
    AnalysisGap {
        file: file.into(),
        line,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn machine_output_unescapes_pipes_without_corrupting_windows_paths() {
        assert_eq!(
            machine_fields(r"ERROR|TYPE|CODE|C:\work\a.dart|1|2|3|bad \| name")[7],
            "bad | name"
        );
    }
}
