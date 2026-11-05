//! Document-local Dart evidence, kept outside shared SCIP documents.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::lang::registry;
use crate::scip::{Document, occurrence_text};
use ::scip::types::{Document as RawDocument, Index, Occurrence, symbol_information::Kind};

#[derive(Debug, Clone, Default)]
pub(crate) struct Evidence {
    pub path: String,
    pub symbols: Vec<Symbol>,
    /// An owner is present only when directive coverage established it uniquely.
    pub library: Option<String>,
}

#[derive(Default)]
pub(crate) struct Inventory {
    pub documents: BTreeMap<String, Evidence>,
}

impl Inventory {
    pub fn read(index: &Index, root: &Path, base: &Path, documents: &[Document]) -> Self {
        let paths: BTreeSet<&str> = documents.iter().map(|d| d.path.as_str()).collect();
        let prefix = crate::scan::slash(base);
        let mut inventory = Self::default();
        for raw in &index.documents {
            let relative = raw.relative_path.replace('\\', "/");
            let path = if prefix.is_empty() {
                relative
            } else {
                format!("{prefix}/{relative}")
            };
            if !paths.contains(path.as_str())
                || raw.language != "Dart"
                || !registry::is(Path::new(&path), &registry::DART)
                || !raw
                    .occurrences
                    .iter()
                    .any(|o| o.symbol.starts_with("scip-dart "))
            {
                continue;
            }
            let lines = std::fs::read_to_string(root.join(&path))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            inventory
                .documents
                .entry(path.clone())
                .or_insert_with(|| Evidence {
                    path,
                    ..Default::default()
                })
                .symbols
                .extend(occurrences(raw, &lines));
        }
        inventory.ensure(documents);
        inventory
    }

    pub fn merge(&mut self, other: Self) {
        for (path, evidence) in other.documents {
            match self.documents.get_mut(&path) {
                Some(seen) => {
                    seen.symbols.extend(evidence.symbols);
                    if seen.library != evidence.library {
                        seen.library = None;
                    }
                }
                None => {
                    self.documents.insert(path, evidence);
                }
            }
        }
    }

    /// Aliases can participate as possible targets, without supplying evidence.
    pub fn ensure(&mut self, documents: &[Document]) {
        for document in documents
            .iter()
            .filter(|d| registry::is(Path::new(&d.path), &registry::DART))
        {
            self.documents
                .entry(document.path.clone())
                .or_insert_with(|| Evidence {
                    path: document.path.clone(),
                    ..Default::default()
                });
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Symbol {
    pub(crate) name: Option<String>,
    pub(crate) line: usize,
    pub(crate) definition: bool,
    pub(crate) same_file: bool,
    /// None means the index omitted classification: never assume it is harmless.
    pub(crate) is_type: Option<bool>,
    pub(crate) owner: Option<String>,
    pub(crate) owner_unknown: bool,
}

fn is_type(kind: Kind) -> Option<bool> {
    match kind {
        Kind::UnspecifiedKind => None,
        Kind::Class
        | Kind::Enum
        | Kind::Interface
        | Kind::Mixin
        | Kind::TypeAlias
        | Kind::Extension
        | Kind::Struct
        | Kind::Trait
        | Kind::Type => Some(true),
        _ => Some(false),
    }
}

fn range(range: &[i32]) -> Option<((i32, i32), (i32, i32))> {
    match range {
        [line, start, end] => Some(((*line, *start), (*line, *end))),
        [line, start, end_line, end] => Some(((*line, *start), (*end_line, *end))),
        _ => None,
    }
}

pub(crate) fn occurrences(document: &RawDocument, lines: &[String]) -> Vec<Symbol> {
    let kinds: BTreeMap<&str, Kind> = document
        .symbols
        .iter()
        .map(|s| (s.symbol.as_str(), s.kind.enum_value_or_default()))
        .collect();
    let definitions: BTreeSet<&str> = document
        .occurrences
        .iter()
        .filter(|o| o.symbol_roles & 1 != 0)
        .map(|o| o.symbol.as_str())
        .collect();
    let encoding = document.position_encoding.enum_value_or_default();
    let text = |o: &Occurrence| occurrence_text(lines, o, encoding).map(str::to_string);
    document
        .occurrences
        .iter()
        .filter(|o| o.symbol.starts_with("local "))
        .filter_map(|o| {
            let name = text(o);
            if name
                .as_ref()
                .is_some_and(|n| !n.starts_with('_') || n == "_")
            {
                return None;
            }
            let enclosing = |outer: &&Occurrence| {
                outer.symbol_roles & 1 != 0
                    && outer.symbol != o.symbol
                    && range(&o.range).is_some_and(|(position, _)| {
                        range(&outer.enclosing_range)
                            .is_some_and(|(start, end)| start <= position && position < end)
                    })
            };
            let mut classification = kinds.get(o.symbol.as_str()).copied().and_then(is_type);
            if classification.is_none()
                && document.occurrences.iter().filter(enclosing).any(|outer| {
                    kinds.get(outer.symbol.as_str()).copied().and_then(is_type) == Some(false)
                })
            {
                // The pinned indexer leaves local-variable kinds unspecified.
                // A definition inside a known callable/initializer is not a
                // library-private type merely because its name starts with _.
                classification = Some(false);
            }
            let owner = range(&o.range).and_then(|_| {
                document
                    .occurrences
                    .iter()
                    .filter(enclosing)
                    .filter(|outer| {
                        kinds.get(outer.symbol.as_str()).copied().and_then(is_type) == Some(true)
                    })
                    .find_map(text)
            });
            let owner_unknown = owner.is_none()
                && kinds.get(o.symbol.as_str()).is_some_and(|kind| {
                    matches!(
                        kind,
                        Kind::Method | Kind::Field | Kind::Constructor | Kind::Property
                    )
                });
            Some(Symbol {
                name,
                line: crate::scip::line_of(o) + 1,
                definition: o.symbol_roles & 1 != 0,
                same_file: definitions.contains(o.symbol.as_str()),
                is_type: classification,
                owner,
                owner_unknown,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::Message;

    #[test]
    fn private_occurrence_inventory_is_guarded_by_dart_path_language_and_scheme() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("lib")).unwrap();
        for (language, path, scheme, expected) in [
            ("Dart", "lib/file.dart", "scip-dart", 1),
            ("Rust", "lib/file.dart", "scip-dart", 0),
            ("Dart", "lib/file.rs", "scip-dart", 0),
            ("Dart", "lib/file.dart", "scip-python", 0),
        ] {
            std::fs::write(root.path().join(path), "class _Hidden {}\n").unwrap();
            let raw = ::scip::types::Document {
                language: language.into(),
                relative_path: path.into(),
                symbols: vec![::scip::types::SymbolInformation {
                    symbol: "local 0".into(),
                    kind: ::scip::types::symbol_information::Kind::Class.into(),
                    ..Default::default()
                }],
                occurrences: vec![
                    Occurrence {
                        symbol: format!("{scheme} pub example . lib/`file.dart`/"),
                        symbol_roles: 1,
                        range: vec![0, 0, 0],
                        ..Default::default()
                    },
                    Occurrence {
                        symbol: "local 0".into(),
                        symbol_roles: 1,
                        range: vec![0, 6, 13],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let index = Index {
                documents: vec![raw],
                ..Default::default()
            };
            let documents =
                crate::scip::parse(&index.write_to_bytes().unwrap(), root.path(), Path::new(""))
                    .unwrap();
            let inventory = crate::scip::dart::private::Inventory::read(
                &index,
                root.path(),
                Path::new(""),
                &documents,
            );
            assert_eq!(
                inventory
                    .documents
                    .values()
                    .map(|d| d.symbols.len())
                    .sum::<usize>(),
                expected,
                "{language} {path} {scheme}"
            );
        }
    }

    #[test]
    fn merging_local_evidence_preserves_symbols_and_unknown_library_ownership() {
        let raw = RawDocument {
            symbols: vec![::scip::types::SymbolInformation {
                symbol: "local 0".into(),
                kind: Kind::Class.into(),
                ..Default::default()
            }],
            occurrences: vec![Occurrence {
                symbol: "local 0".into(),
                symbol_roles: 1,
                range: vec![0, 6, 8],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut inventory = Inventory::default();
        for (name, library) in [("_A", "lib/one.dart"), ("_B", "lib/two.dart")] {
            let path = "lib/part.dart".to_string();
            let evidence = Evidence {
                path: path.clone(),
                library: Some(library.into()),
                symbols: occurrences(&raw, &[format!("class {name} {{}}")]),
            };
            inventory.merge(Inventory {
                documents: BTreeMap::from([(path, evidence)]),
            });
        }
        let evidence = &inventory.documents["lib/part.dart"];
        assert_eq!(
            evidence
                .symbols
                .iter()
                .map(|s| s.name.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("_A"), Some("_B")]
        );
        assert!(evidence.library.is_none());
    }

    #[test]
    fn module_aliases_do_not_supply_private_evidence_or_library_ownership() {
        let mut inventory = Inventory::default();
        inventory.ensure(&[Document {
            path: "lib/alias.dart".into(),
            definitions: vec!["scip-dart pub @ . `lib/alias.dart`/".into()],
            ..Default::default()
        }]);
        let alias = &inventory.documents["lib/alias.dart"];
        assert!(alias.symbols.is_empty());
        assert!(alias.library.is_none());
    }
}
