//! Normalize scip-dart identities before the shared facts pass.
use super::*;
use crate::scan::AnalysisGap;

pub(crate) mod private;

const UNMAPPED: &str = concat!(
    "Dart reference cannot be mapped through packageUri; ",
    "its external module was not fully verified"
);

fn gap(file: &str, line: usize, reason: &str) -> AnalysisGap {
    AnalysisGap {
        file: file.into(),
        line: Some(line),
        reason: reason.into(),
    }
}

#[derive(Debug)]
pub(crate) struct Package {
    pub root: PathBuf,
    pub uri_path: String,
}

pub(crate) struct Inputs<'a> {
    pub packages: &'a BTreeMap<String, Package>,
    pub sdk_modules: &'a BTreeMap<String, BTreeSet<PathBuf>>,
}

pub(crate) struct Read {
    pub documents: Vec<Document>,
    pub gaps: Vec<AnalysisGap>,
    pub private: private::Inventory,
}

/// Repair only the released indexer's version and SDK descriptor spelling.
pub(super) fn syntax_symbol(symbol: &str) -> String {
    let parts: Vec<&str> = symbol.splitn(5, ' ').collect();
    let ["scip-dart", manager, package, version, descriptors] = parts.as_slice() else {
        return symbol.into();
    };
    let version = if *version == "null" { "." } else { version };
    let descriptors = if package.starts_with("dart:") {
        descriptors
            .strip_prefix(package)
            .map(|tail| format!("{}{tail}", quote(package)))
    } else {
        None
    };
    format!(
        "scip-dart {manager} {package} {version} {}",
        descriptors.as_deref().unwrap_or(parts[4])
    )
}

fn quote(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        && !path.contains('\\')
        && !path.contains(':')
}

fn package_identity(path: &Path) -> String {
    let mut identity = String::from("@");
    for byte in crate::scan::slash(path).bytes() {
        use std::fmt::Write;
        write!(identity, "{byte:02x}").expect("writing to a string");
    }
    identity
}

fn internal(symbol: &str) -> Option<String> {
    let parsed = split(symbol)?;
    (parsed.scheme == "scip-dart" && parsed.package.starts_with('@')).then(|| parsed.namespace("/"))
}

fn symbol(symbol: &str, root: &Path, inputs: &Inputs) -> std::result::Result<String, &'static str> {
    let repaired = syntax_symbol(symbol);
    let parsed = split(&repaired)
        .ok_or("unsupported Dart SCIP symbol; its dependency or symbol use was not verified")?;
    let namespace = parsed.namespace("/").replace('\\', "/");
    if matches!(parsed.referent(), Referent::Member(name) if name.is_empty()) {
        return Err("unsupported Dart SCIP symbol; its dependency or symbol use was not verified");
    }
    if parsed.package.starts_with("dart:") {
        let suffix = namespace
            .strip_prefix(parsed.package)
            .filter(|tail| tail.is_empty() || tail.starts_with('/'))
            .ok_or("unsupported Dart SDK namespace")?
            .trim_start_matches('/');
        if !suffix.is_empty() && !relative(suffix) {
            return Err("unsupported Dart SDK namespace");
        }
        if matches!(parsed.referent(), Referent::Module) {
            let exists = inputs.sdk_modules.get(parsed.package).is_some_and(|paths| {
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
                return Err(
                    "Dart SCIP references a missing SDK library target; repair SDK preparation and run with --no-cache",
                );
            }
        }
        return Ok(repaired);
    }
    // Canonical internal symbols name a physical package and a project-relative file.
    // The marker cannot collide with a valid Pub package name.
    if parsed.package.starts_with('@') {
        if relative(&namespace)
            && inputs.packages.values().any(|package| {
                package
                    .root
                    .strip_prefix(root)
                    .is_ok_and(|base| package_identity(base) == parsed.package)
                    && root.join(&namespace).starts_with(&package.root)
            })
        {
            return Ok(repaired);
        }
        return Err("Dart symbol has an unknown internal package identity");
    }
    let package = inputs.packages.get(parsed.package).ok_or(UNMAPPED)?;
    let prefix = format!("package:{}/", parsed.package);
    let raw_path = if let Some(path) = namespace.strip_prefix(&prefix) {
        // A canonical URI namespace is stable under repeated normalization.
        if package.uri_path.is_empty() {
            path.into()
        } else {
            format!("{}/{path}", package.uri_path)
        }
    } else {
        namespace.clone()
    };
    if !relative(&raw_path) {
        return Err("unsupported Dart package file identity");
    }
    let target = package.root.join(&raw_path);
    if !target.is_file() {
        return Err(
            "Dart SCIP references a missing package file; repair imports or generated input and run with --no-cache",
        );
    }
    let (owner, version, namespace) = if let Ok(base) = package.root.strip_prefix(root) {
        (
            package_identity(base),
            ".",
            crate::scan::slash(target.strip_prefix(root).expect("internal package")),
        )
    } else {
        let path = if package.uri_path.is_empty() {
            raw_path.as_str()
        } else {
            raw_path
                .strip_prefix(&format!("{}/", package.uri_path))
                .ok_or(UNMAPPED)?
        };
        (
            parsed.package.into(),
            repaired.split(' ').nth(3).expect("parsed header"),
            format!("{prefix}{path}"),
        )
    };
    let text = repaired.splitn(5, ' ').nth(4).expect("parsed descriptors");
    let end = scan(text)
        .expect("parsed descriptors")
        .iter()
        .take_while(|(_, kind, _)| *kind == Kind::Namespace)
        .last()
        .map_or(0, |(_, _, end)| *end);
    let tail: String = text.chars().skip(end).collect();
    Ok(format!(
        "scip-dart pub {owner} {version} {}/{tail}",
        quote(&namespace)
    ))
}

pub(crate) fn parse(bytes: &[u8], root: &Path, base: &Path, inputs: &Inputs) -> Result<Read> {
    let index = Index::parse_from_bytes(bytes).context("not a SCIP index")?;
    let mut gaps = Vec::new();
    let documents = super::parse_index(&index, root, base, |file, occurrence| {
        match symbol(&occurrence.symbol, root, inputs) {
            Ok(symbol) => Some(symbol),
            Err(reason) => {
                gaps.push(gap(file, line_of(occurrence) + 1, reason));
                None
            }
        }
    })?;
    let private = private::Inventory::read(&index, root, base, &documents);
    Ok(Read {
        documents,
        gaps,
        private,
    })
}

/// Resolve only module identities for known files. A module alias says nothing
/// about definitions or outgoing imports, and must never count as indexed source.
pub(crate) fn complete(
    root: &Path,
    documents: &mut Vec<Document>,
    known: &BTreeSet<&str>,
    excluded: &[String],
) -> Vec<AnalysisGap> {
    let mut definitions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for document in documents.iter() {
        for symbol in &document.definitions {
            definitions
                .entry(symbol.clone())
                .or_default()
                .insert(document.path.clone());
        }
    }
    let mut aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut gaps = Vec::new();
    for document in documents.iter_mut() {
        document.references.retain(|reference| {
            let Some(target) = internal(&reference.symbol) else {
                return true;
            };
            let identity = class_symbol(&reference.symbol);
            if let Some(paths) = definitions.get(&identity) {
                if paths.len() == 1 {
                    return true;
                }
                gaps.push(gap(
                    &document.path,
                    reference.line,
                    concat!(
                        "internal Dart symbol has ambiguous indexed definitions; ",
                        "repair resolution and run with --no-cache"
                    ),
                ));
                return false;
            }
            let module = split(&reference.symbol)
                .is_some_and(|parsed| matches!(parsed.referent(), Referent::Module));
            if module
                && relative(&target)
                && known.contains(target.as_str())
                && root.join(&target).is_file()
            {
                aliases
                    .entry(target.clone())
                    .or_default()
                    .insert(reference.symbol.clone());
                if !excluded.contains(&target) {
                    gaps.push(gap(
                        &document.path,
                        reference.line,
                        concat!(
                            "Dart target has no indexed definition; ",
                            "imports in or of that source were not fully verified"
                        ),
                    ));
                }
                return true;
            }
            let reason = if module {
                concat!(
                    "an internal Dart reference has no indexed definition; ",
                    "repair resolution or generated sources and run with --no-cache"
                )
            } else {
                concat!(
                    "an internal Dart symbol has no indexed definition; ",
                    "repair resolution or generated sources and run with --no-cache"
                )
            };
            gaps.push(gap(&document.path, reference.line, reason));
            false
        });
    }
    for (path, symbols) in aliases {
        if let Some(document) = documents.iter_mut().find(|d| d.path == path) {
            document.definitions.extend(symbols);
        } else {
            documents.push(Document {
                path,
                definitions: symbols.into_iter().collect(),
                ..Default::default()
            });
        }
    }
    gaps
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use ::scip::types::Document as RawDocument;

    fn occurrence(symbol: &str, definition: bool, line: i32) -> Occurrence {
        Occurrence {
            symbol: symbol.into(),
            symbol_roles: i32::from(definition),
            range: vec![line, 0, 0],
            ..Default::default()
        }
    }

    fn bytes(occurrences: Vec<Occurrence>) -> Vec<u8> {
        Index {
            documents: vec![RawDocument {
                language: "Dart".into(),
                relative_path: "lib/app.dart".into(),
                occurrences,
                ..Default::default()
            }],
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap()
    }

    fn file(root: &Path, name: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "// fixture\n").unwrap();
    }

    // Real SCIP artifacts remain testable without an installed SDK or Pub cache.
    pub(in crate::scip) fn fixture(bytes: &[u8], root: &Path) -> Vec<Document> {
        let tmp = tempfile::tempdir().unwrap();
        let raw = Index::parse_from_bytes(bytes).unwrap();
        let manifest: serde_norway::Value =
            serde_norway::from_slice(&std::fs::read(root.join("pubspec.yaml")).unwrap()).unwrap();
        let own = manifest["name"].as_str().unwrap();
        let mut packages = BTreeMap::from([(
            own.into(),
            Package {
                root: root.to_path_buf(),
                uri_path: "lib".into(),
            },
        )]);
        let mut sdk_modules: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
        for occurrence in raw.documents.iter().flat_map(|d| &d.occurrences) {
            let normalized = syntax_symbol(&occurrence.symbol);
            let Some(parsed) = split(&normalized) else {
                continue;
            };
            if parsed.scheme != "scip-dart" {
                continue;
            }
            if parsed.package.starts_with("dart:") {
                let path = tmp
                    .path()
                    .join(parsed.package.replace(':', "_"))
                    .join("library.dart");
                file(
                    tmp.path(),
                    path.strip_prefix(tmp.path()).unwrap().to_str().unwrap(),
                );
                let namespace = parsed.namespace("/");
                let suffix = namespace
                    .strip_prefix(parsed.package)
                    .unwrap_or_default()
                    .trim_start_matches('/');
                if !suffix.is_empty() {
                    file(path.parent().unwrap(), suffix);
                }
                sdk_modules
                    .entry(parsed.package.into())
                    .or_default()
                    .insert(path);
            } else if parsed.package != own {
                let package_root = tmp.path().join(parsed.package);
                file(&package_root, &parsed.namespace("/"));
                packages.entry(parsed.package.into()).or_insert(Package {
                    root: package_root,
                    uri_path: "lib".into(),
                });
            }
        }
        let read = parse(
            bytes,
            root,
            Path::new(""),
            &Inputs {
                packages: &packages,
                sdk_modules: &sdk_modules,
            },
        )
        .unwrap();
        assert!(read.gaps.is_empty(), "{:?}", read.gaps);
        read.documents
    }

    #[test]
    fn external_package_uri_is_resolved_before_shared_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let dependency = tmp.path().join("dependency");
        file(&root, "lib/app.dart");
        for prefix in ["lib", "src/public", ""] {
            let namespace = if prefix.is_empty() {
                "types.dart".into()
            } else {
                format!("{prefix}/types.dart")
            };
            file(&dependency, &namespace);
            let packages = BTreeMap::from([(
                "dep".into(),
                Package {
                    root: dependency.clone(),
                    uri_path: prefix.into(),
                },
            )]);
            let inputs = Inputs {
                packages: &packages,
                sdk_modules: &BTreeMap::new(),
            };
            let raw = bytes(vec![
                occurrence(&format!("scip-dart pub dep 1.0.0 `{namespace}`/"), false, 0),
                occurrence(
                    &format!("scip-dart pub dep 1.0.0 `{namespace}`/Value#"),
                    false,
                    1,
                ),
            ]);
            let read = parse(&raw, &root, Path::new(""), &inputs).unwrap();
            assert!(read.gaps.is_empty());
            for reference in &read.documents[0].references {
                assert_eq!(
                    symbol(&reference.symbol, &root, &inputs).unwrap(),
                    reference.symbol
                );
            }
            let facts = facts(&read.documents, &[], true);
            assert_eq!(
                facts.deps[0].target,
                Target::External("package:dep/types.dart".into())
            );
            assert_eq!(facts.symbols[0].specifier, "package:dep/types.dart");
        }
    }

    #[test]
    fn physical_package_identity_separates_equal_pub_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let raw = bytes(vec![occurrence(
            "scip-dart pub same null lib/`app.dart`/",
            true,
            0,
        )]);
        let mut identities = Vec::new();
        for base in ["packages/one", "packages/two"] {
            let package_root = root.join(base);
            file(&package_root, "lib/app.dart");
            let packages = BTreeMap::from([(
                "same".into(),
                Package {
                    root: package_root,
                    uri_path: "lib".into(),
                },
            )]);
            let read = parse(
                &raw,
                root,
                Path::new(base),
                &Inputs {
                    packages: &packages,
                    sdk_modules: &BTreeMap::new(),
                },
            )
            .unwrap();
            assert!(read.gaps.is_empty());
            identities.push(read.documents[0].definitions[0].clone());
        }
        assert_ne!(identities[0], identities[1]);
    }

    #[test]
    fn canonical_symbols_are_idempotent_and_keep_scope_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        file(root, "lib/app.dart");
        let packages = BTreeMap::from([(
            "app".into(),
            Package {
                root: root.to_path_buf(),
                uri_path: "lib".into(),
            },
        )]);
        let inputs = Inputs {
            packages: &packages,
            sdk_modules: &BTreeMap::new(),
        };
        let mut method = occurrence(
            "scip-dart pub app null lib/`app.dart`/Thing#`run``now`(a).",
            true,
            1,
        );
        method.enclosing_range = vec![1, 0, 3, 0];
        let first = parse(&bytes(vec![method]), root, Path::new(""), &inputs).unwrap();
        let symbol = first.documents[0].definitions[0].clone();
        let mut normalized = occurrence(&symbol, true, 1);
        normalized.enclosing_range = vec![1, 0, 3, 0];
        let second = parse(&bytes(vec![normalized]), root, Path::new(""), &inputs).unwrap();
        assert!(second.gaps.is_empty());
        assert_eq!(
            first.documents[0].definitions,
            second.documents[0].definitions
        );
        assert_eq!(first.documents[0].scopes, second.documents[0].scopes);
        assert_eq!(first.documents[0].scopes[0].symbol, symbol);
    }

    #[test]
    fn a_module_alias_preserves_edges_without_inventing_type_definitions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        file(root, "lib/app.dart");
        file(root, "lib/domain.dart");
        let packages = BTreeMap::from([(
            "app".into(),
            Package {
                root: root.to_path_buf(),
                uri_path: "lib".into(),
            },
        )]);
        let read = parse(
            &bytes(vec![
                occurrence("scip-dart pub app null lib/`app.dart`/", true, 0),
                occurrence("scip-dart pub app . lib/`domain.dart`/", false, 1),
                occurrence("scip-dart pub app . lib/`domain.dart`/Domain#", false, 2),
            ]),
            root,
            Path::new(""),
            &Inputs {
                packages: &packages,
                sdk_modules: &BTreeMap::new(),
            },
        )
        .unwrap();
        assert_eq!(
            read.documents.len(),
            1,
            "raw coverage must exclude alias targets"
        );
        let mut documents = read.documents;
        let gaps = complete(
            root,
            &mut documents,
            &BTreeSet::from(["lib/app.dart", "lib/domain.dart"]),
            &[],
        );
        assert!(
            gaps.iter()
                .any(|gap| gap.file == "lib/app.dart" && gap.line == Some(2))
        );
        assert!(
            gaps.iter()
                .any(|gap| gap.file == "lib/app.dart" && gap.line == Some(3))
        );
        let target = documents
            .iter()
            .find(|d| d.path == "lib/domain.dart")
            .unwrap();
        assert_eq!(target.definitions.len(), 1);
        assert!(target.types.is_empty() && target.scopes.is_empty());
        assert!(
            target.references.is_empty(),
            "an alias cannot invent outgoing imports"
        );
        let facts = facts(&documents, &[], true);
        assert_eq!(
            facts.deps[0].target,
            Target::Internal(Unit::from("lib/domain.dart"))
        );
        assert!(facts.symbols.is_empty());
    }

    #[test]
    fn invalid_or_unmapped_symbols_cannot_become_external_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "lib/app.dart");
        let read = parse(
            &bytes(vec![
                occurrence("scip-dart pub absent . lib/`unknown.dart`/", false, 0),
                occurrence("scip-dart pub broken . `unfinished", false, 1),
            ]),
            tmp.path(),
            Path::new(""),
            &Inputs {
                packages: &BTreeMap::new(),
                sdk_modules: &BTreeMap::new(),
            },
        )
        .unwrap();
        assert_eq!(read.gaps.len(), 2);
        assert!(read.documents[0].references.is_empty());
        assert!(facts(&read.documents, &[], true).deps.is_empty());
    }

    #[test]
    fn sdk_namespace_repairs_are_scoped_and_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "sdk/async.dart");
        let modules = BTreeMap::from([(
            "dart:async".into(),
            BTreeSet::from([tmp.path().join("sdk/async.dart")]),
        )]);
        let inputs = Inputs {
            packages: &BTreeMap::new(),
            sdk_modules: &modules,
        };
        let raw = "scip-dart pub dart:async null dart:async/Future#";
        let normalized = symbol(raw, tmp.path(), &inputs).unwrap();
        assert_eq!(
            normalized,
            "scip-dart pub dart:async . `dart:async`/Future#"
        );
        assert_eq!(
            symbol(&normalized, tmp.path(), &inputs).unwrap(),
            normalized
        );
        assert!(
            symbol(
                "scip-dart pub dart:missing . dart:missing/",
                tmp.path(),
                &inputs
            )
            .is_err()
        );
        assert!(
            symbol(
                "scip-dart pub dart:async . dart:async/../../bad/",
                tmp.path(),
                &inputs
            )
            .is_err()
        );
        let foreign = "scip-python pip dart:async null dart:async/Future#";
        assert_eq!(syntax_symbol(foreign), foreign);
    }

    #[test]
    fn other_scip_schemes_and_non_dart_documents_are_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "lib/app.dart");
        let inputs = Inputs {
            packages: &BTreeMap::new(),
            sdk_modules: &BTreeMap::new(),
        };
        for (language, raw_symbol) in [
            ("Dart", "scip-python pip example null foo/"),
            ("Rust", "scip-dart pub example null lib/`app.dart`/"),
        ] {
            let mut index =
                Index::parse_from_bytes(&bytes(vec![occurrence(raw_symbol, false, 0)])).unwrap();
            index.documents[0].language = language.into();
            let read = parse(
                &index.write_to_bytes().unwrap(),
                tmp.path(),
                Path::new(""),
                &inputs,
            )
            .unwrap();
            assert!(read.gaps.is_empty());
            assert_eq!(read.documents[0].references[0].symbol, raw_symbol);
        }
    }

    #[test]
    fn excluded_module_aliases_resolve_but_do_not_add_source_diagnostics() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        file(root, "lib/app.dart");
        file(root, "lib/domain.dart");
        let packages = BTreeMap::from([(
            "app".into(),
            Package {
                root: root.to_path_buf(),
                uri_path: "lib".into(),
            },
        )]);
        let mut read = parse(
            &bytes(vec![occurrence(
                "scip-dart pub app . lib/`domain.dart`/",
                false,
                0,
            )]),
            root,
            Path::new(""),
            &Inputs {
                packages: &packages,
                sdk_modules: &BTreeMap::new(),
            },
        )
        .unwrap();
        let known = BTreeSet::from(["lib/app.dart", "lib/domain.dart"]);
        let gaps = complete(
            root,
            &mut read.documents,
            &known,
            &["lib/domain.dart".into()],
        );
        assert!(gaps.is_empty());
        let (visible, hidden): (Vec<_>, Vec<_>) = read
            .documents
            .into_iter()
            .partition(|d| d.path == "lib/app.dart");
        assert!(hidden[0].references.is_empty());
        assert_eq!(
            facts(&visible, &hidden, false).deps[0].target,
            Target::Internal(Unit::from("lib/domain.dart"))
        );
    }

    #[test]
    fn merged_package_indexes_resolve_normalized_definitions_and_references() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for base in ["packages/app", "packages/domain"] {
            file(&root.join(base), "lib/app.dart");
        }
        let packages = BTreeMap::from([
            (
                "app".into(),
                Package {
                    root: root.join("packages/app"),
                    uri_path: "lib".into(),
                },
            ),
            (
                "domain".into(),
                Package {
                    root: root.join("packages/domain"),
                    uri_path: "lib".into(),
                },
            ),
        ]);
        let inputs = Inputs {
            packages: &packages,
            sdk_modules: &BTreeMap::new(),
        };
        let app = parse(
            &bytes(vec![
                occurrence("scip-dart pub app null lib/`app.dart`/", true, 0),
                occurrence("scip-dart pub domain . lib/`app.dart`/", false, 1),
                occurrence("scip-dart pub domain . lib/`app.dart`/Domain#", false, 2),
            ]),
            root,
            Path::new("packages/app"),
            &inputs,
        )
        .unwrap();
        let domain = parse(
            &bytes(vec![
                occurrence("scip-dart pub domain null lib/`app.dart`/", true, 0),
                occurrence("scip-dart pub domain . lib/`app.dart`/Domain#", true, 1),
            ]),
            root,
            Path::new("packages/domain"),
            &inputs,
        )
        .unwrap();
        let mut merged = Documents::merged(vec![app.documents, domain.documents]).unwrap();
        let gaps = complete(
            root,
            &mut merged.0,
            &BTreeSet::from(["packages/app/lib/app.dart", "packages/domain/lib/app.dart"]),
            &[],
        );
        assert!(gaps.is_empty(), "{gaps:?}");
        let facts = facts(&merged.0, &[], true);
        assert_eq!(
            facts.deps[0].target,
            Target::Internal(Unit::from("packages/domain/lib/app.dart"))
        );
        assert_eq!(facts.symbols[0].specifier, "packages/domain/lib/app");
    }

    #[test]
    fn missing_package_targets_and_invalid_uri_mappings_are_disclosed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let dependency = tmp.path().join("dependency");
        file(&root, "lib/app.dart");
        file(&dependency, "lib/present.dart");
        let packages = BTreeMap::from([(
            "dep".into(),
            Package {
                root: dependency,
                uri_path: "src".into(),
            },
        )]);
        let read = parse(
            &bytes(vec![
                occurrence("scip-dart pub dep . lib/`missing.dart`/", false, 0),
                occurrence("scip-dart pub dep . lib/`present.dart`/", false, 1),
                occurrence("scip-dart pub dep . `../outside.dart`/", false, 2),
            ]),
            &root,
            Path::new(""),
            &Inputs {
                packages: &packages,
                sdk_modules: &BTreeMap::new(),
            },
        )
        .unwrap();
        assert_eq!(read.gaps.len(), 3);
        assert!(read.gaps[0].reason.contains("missing package file"));
        assert!(read.gaps[1].reason.contains("packageUri"));
        assert!(read.documents[0].references.is_empty());
    }

    #[test]
    fn canonical_file_descriptors_escape_backticks_and_normalize_separators() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "lib/a`b.dart");
        let packages = BTreeMap::from([(
            "app".into(),
            Package {
                root: tmp.path().to_path_buf(),
                uri_path: "lib".into(),
            },
        )]);
        let inputs = Inputs {
            packages: &packages,
            sdk_modules: &BTreeMap::new(),
        };
        let raw = "scip-dart pub app null `lib\\a``b.dart`/Value#";
        let canonical = symbol(raw, tmp.path(), &inputs).unwrap();
        assert_eq!(split(&canonical).unwrap().namespace("/"), "lib/a`b.dart");
        assert_eq!(symbol(&canonical, tmp.path(), &inputs).unwrap(), canonical);
    }

    #[test]
    fn ambiguous_internal_definitions_are_disclosed_without_choosing_a_target() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "lib/app.dart");
        file(tmp.path(), "lib/domain.dart");
        let packages = BTreeMap::from([(
            "app".into(),
            Package {
                root: tmp.path().to_path_buf(),
                uri_path: "lib".into(),
            },
        )]);
        let mut read = parse(
            &bytes(vec![occurrence(
                "scip-dart pub app . lib/`domain.dart`/Domain#",
                false,
                0,
            )]),
            tmp.path(),
            Path::new(""),
            &Inputs {
                packages: &packages,
                sdk_modules: &BTreeMap::new(),
            },
        )
        .unwrap();
        let identity = read.documents[0].references[0].symbol.clone();
        for path in ["lib/one.dart", "lib/two.dart"] {
            read.documents.push(Document {
                path: path.into(),
                definitions: vec![identity.clone()],
                ..Default::default()
            });
        }
        let gaps = complete(
            tmp.path(),
            &mut read.documents,
            &BTreeSet::from(["lib/app.dart", "lib/domain.dart"]),
            &[],
        );
        assert_eq!(gaps.len(), 1);
        assert!(gaps[0].reason.contains("ambiguous indexed definitions"));
        assert!(read.documents[0].references.is_empty());
        assert!(facts(&read.documents, &[], true).symbols.is_empty());
    }
}
