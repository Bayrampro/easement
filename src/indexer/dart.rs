//! Dart prerequisites, configured inputs and SCIP postprocessing.
use super::*;
use crate::scan::AnalysisGap;

pub(crate) mod directives;
mod inputs;
mod private;
use directives::Kind;

use crate::scip::dart::{Inputs, Package};

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
    let path = dir
        .ancestors()
        .map(|p| p.join(".dart_tool/package_config.json"))
        .find(|p| p.is_file())
        .context(
            "missing .dart_tool/package_config.json (including ancestor workspace resolution); \
             run dart pub get or flutter pub get",
        )?;
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

pub(super) fn resolved_inputs(context: &InputContext<'_>) -> Result<Vec<Input>> {
    let cwd = context.root.join(context.dir);
    let resolution = resolution(&cwd)?;
    let sdk = sdk(&cwd)?;
    inputs::collect(context.binary, &sdk, &cwd, &resolution)
}

pub(super) fn reader() -> Box<dyn super::Reader> {
    Box::<DartReader>::default()
}

#[derive(Default)]
struct DartReader {
    inventory: scip::dart::private::Inventory,
}

impl super::Reader for DartReader {
    fn read(&mut self, bytes: &[u8], root: &Path, base: &Path) -> Result<ReadFacts> {
        let cwd = root.join(base);
        let resolution = resolution(&cwd)?;
        let sdk = sdk(&cwd)?;
        let sdk_modules = sdk_modules(&sdk, &resolution)?;
        let parsed = scip::dart::parse(
            bytes,
            root,
            base,
            &Inputs {
                packages: &resolution.packages,
                sdk_modules: &sdk_modules,
            },
        )?;
        if parsed.documents.is_empty() {
            bail!(
                "scip_dart indexed no files in {}; check analyzer exclusions and package resolution",
                cwd.display()
            );
        }
        self.inventory.merge(parsed.private);
        Ok(ReadFacts {
            documents: parsed.documents,
            gaps: parsed.gaps,
        })
    }

    fn validate(&mut self, root: &Path, cwd: &Path) -> Result<Vec<AnalysisGap>> {
        analyze(root, cwd, &sdk(cwd)?)
    }

    fn finish(
        self: Box<Self>,
        _language: &Indexer,
        mut documents: Documents,
        mut gaps: Vec<AnalysisGap>,
        context: &RunContext<'_>,
    ) -> Result<Indexed> {
        let RunContext {
            root,
            files,
            excluded,
        } = context;
        let mut inventory = self.inventory;
        let all: BTreeSet<&str> = files.iter().chain(*excluded).map(String::as_str).collect();
        let indexed: BTreeSet<String> = documents.0.iter().map(|d| d.path.clone()).collect();
        gaps.extend(directive_gaps(root, &mut documents.0, &mut inventory)?);
        gaps.extend(scip::dart::complete(root, &mut documents.0, &all, excluded));
        gaps.sort();
        gaps.dedup();
        let uncovered = files
            .iter()
            .filter(|file| {
                registry::is(Path::new(file), &registry::DART) && !indexed.contains(file.as_str())
            })
            .cloned()
            .collect();
        inventory.ensure(&documents.0);
        let (sources, hidden): (Vec<_>, Vec<_>) = inventory
            .documents
            .into_values()
            .filter(|d| all.contains(d.path.as_str()))
            .partition(|d| files.contains(&d.path));
        let coverage: CoverageCheck = Box::new(move |config, map| {
            let target_only = crate::scan::glob_set(&config.target_only, "target_only")?;
            private::coverage(config, map, &sources, &hidden, &target_only)
        });
        Ok(Indexed {
            coverage_checks: vec![coverage],
            domains: Vec::new(),
            documents,
            analysis_partial: gaps,
            uncovered,
            ran: 0,
            cached: 0,
        })
    }
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
        gaps.push(AnalysisGap {
            file,
            line: Some(fields[4].parse().context("invalid Dart diagnostic line")?),
            reason: format!(
                "Dart analyzer {}: {}; repair source or dependency preparation and run with --no-cache",
                fields[2], fields[7]
            ),
        });
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

fn directive_gaps(
    root: &Path,
    documents: &mut [Document],
    inventory: &mut scip::dart::private::Inventory,
) -> Result<Vec<AnalysisGap>> {
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
                gaps.push(gap(
                    &document.path,
                    None,
                    "Dart directive inventory could not read this source; \
                     repair syntax and run with --no-cache",
                ));
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
                gaps.push(gap(
                    &document.path,
                    Some(directive.line),
                    "conditional Dart directive: SCIP does not cover every branch; \
                     verify other platform branches separately",
                ));
            }
            if directive.uri.is_some() && reference.is_none() {
                gaps.push(gap(
                    &document.path,
                    Some(directive.line),
                    "Dart directive has no matching SCIP reference; \
                     repair resolution or generated input and run with --no-cache",
                ));
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
        let library = if directives
            .iter()
            .any(|d| matches!(d.kind, Kind::PartOf | Kind::NamedPartOf))
        {
            let owners: BTreeSet<&String> =
                owners.get(&document.path).into_iter().flatten().collect();
            (owners.len() == 1).then(|| (*owners.first().expect("one owner")).clone())
        } else {
            Some(document.path.clone())
        };
        if let Some(evidence) = inventory.documents.get_mut(&document.path) {
            evidence.library = library;
        }
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
                });
            } else {
                gaps.push(gap(
                    &document.path,
                    Some(directive.line),
                    "named part of has no unique indexed owning library; \
                     its dependency was not verified",
                ));
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
