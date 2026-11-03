//! Local SCIP IDs are document-scoped. Keep private-symbol evidence without
//! inventing cross-file identities, and disclose only gaps relevant to rules.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ::scip::types::{Document as RawDocument, Occurrence, symbol_information::Kind};
use anyhow::Result;
use globset::GlobSet;

use crate::check::{self, SymbolRef};
use crate::components::ComponentMap;
use crate::config::Config;
use crate::lang::Target;
use crate::scan::{AnalysisGap, TypeDef};
use crate::scip::{Document, occurrence_text};

#[derive(Debug, Clone)]
pub(crate) struct Symbol {
    name: Option<String>,
    line: usize,
    definition: bool,
    same_file: bool,
    /// None means the index omitted classification: never assume it is harmless.
    is_type: Option<bool>,
    owner: Option<String>,
    owner_unknown: bool,
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

fn same_library(left: &Document, right: &Document) -> bool {
    // Unknown ownership cannot prove that a potential target is unrelated.
    left.dart_library.is_none()
        || right.dart_library.is_none()
        || left.dart_library == right.dart_library
}

fn gap(file: &str, symbol: &Symbol, rule: &str) -> AnalysisGap {
    AnalysisGap {
        file: file.into(),
        line: Some(symbol.line),
        reason: format!(
            "Dart private declarations use local SCIP symbols; {rule} cannot fully verify {}",
            symbol
                .name
                .as_deref()
                .unwrap_or("an occurrence whose source range is unavailable")
        ),
    }
}

pub(crate) fn coverage(
    config: &Config,
    map: &ComponentMap,
    documents: &[Document],
    hidden: &[Document],
    target_only: &GlobSet,
) -> Result<Vec<AnalysisGap>> {
    if !config.uses_symbols() && !config.components.values().any(|c| !c.defines.is_empty()) {
        return Ok(Vec::new());
    }
    let all: Vec<&Document> = documents
        .iter()
        .chain(hidden)
        .filter(|d| crate::relpath::extension(&d.path) == "dart")
        .collect();
    let mut gaps = BTreeSet::new();
    for document in documents.iter().filter(|d| !target_only.is_match(&d.path)) {
        let Some(from) = map.component_of(&document.path) else {
            continue;
        };
        for symbol in &document.dart_private {
            if symbol.definition {
                if symbol.is_type == Some(false) {
                    continue;
                }
                let relevant = match &symbol.name {
                    Some(name) => !check::naming(
                        config,
                        map,
                        &[TypeDef {
                            file: document.path.clone(),
                            line: symbol.line,
                            name: name.clone(),
                        }],
                    )?
                    .is_empty(),
                    None => config
                        .components
                        .iter()
                        .any(|(name, c)| name != from && !c.defines.is_empty()),
                };
                if relevant {
                    gaps.insert(gap(
                        &document.path,
                        symbol,
                        "the configured defines naming claims",
                    ));
                }
                continue;
            }
            // The existing symbol rules judge uses from another file, not a
            // file's own declarations, parameters or local variables.
            if symbol.same_file {
                continue;
            }
            let candidates: Vec<(&Document, &Symbol)> = all
                .iter()
                .filter(|d| same_library(document, d))
                .flat_map(|d| {
                    d.dart_private
                        .iter()
                        .filter(|s| s.definition)
                        .filter(|s| {
                            symbol.name.is_none() || s.name.is_none() || symbol.name == s.name
                        })
                        .map(|s| (*d, s))
                })
                .collect();
            let mut relevant = false;
            for (target, definition) in &candidates {
                let Some(name) = definition.owner.as_ref().or(definition.name.as_ref()) else {
                    continue;
                };
                // Candidate identities are used only to ask whether a rule
                // could apply. They never become resolved symbols or violations.
                let potential = SymbolRef {
                    file: PathBuf::from(&document.path),
                    line: symbol.line,
                    specifier: target
                        .path
                        .strip_suffix(".dart")
                        .unwrap_or(&target.path)
                        .into(),
                    name: name.clone(),
                    method: None,
                    target: Some(Target::Internal(PathBuf::from(&target.path))),
                };
                relevant |= !check::symbols(config, map, &[potential]).is_empty();
            }
            if candidates.is_empty()
                || candidates
                    .iter()
                    .any(|(_, s)| s.name.is_none() || s.owner_unknown)
            {
                // Missing definitions or source ranges are not proof of full
                // coverage. Retain a warning under potentially applicable rules.
                relevant |= config
                    .rules
                    .get(from)
                    .is_some_and(|r| !r.forbid_symbol.is_empty())
                    || all
                        .iter()
                        .filter(|d| same_library(document, d))
                        .filter_map(|d| map.component_of(&d.path))
                        .any(|to| to != from && config.components[to].public_symbols.is_some());
            }
            if relevant {
                gaps.insert(gap(
                    &document.path,
                    symbol,
                    "the configured forbid_symbol/public_symbols rules",
                ));
            }
        }
    }
    Ok(gaps.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::scip::types::SymbolInformation;
    use protobuf::EnumOrUnknown;
    use std::path::Path;

    #[test]
    fn real_dart_index_keeps_private_type_evidence_only_where_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dart-basic");
        let documents = crate::scip::parse(
            include_bytes!("../../../tests/fixtures/dart-basic/evidence.scip"),
            &root,
            Path::new(""),
        )
        .unwrap();
        let found: Vec<_> = documents
            .iter()
            .flat_map(|d| d.dart_private.iter().map(|s| (d.path.as_str(), s)))
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "lib/other.dart");
        assert_eq!(found[0].1.name.as_deref(), Some("_Private"));
        assert_eq!(found[0].1.is_type, Some(true));
        assert_eq!(found[0].1.line, 5);
    }

    #[test]
    fn missing_source_ranges_and_symbol_kinds_do_not_certify_naming_coverage() {
        let config: Config = serde_norway::from_str("version: 1\ncomponents:\n  owner: {in: lib/owner, defines: ['*Entity']}\n  source: {in: lib/source.dart}\n").unwrap();
        let map = ComponentMap::new(&config).unwrap();
        for (kind, lines) in [
            (Kind::Class, Vec::new()),
            (
                Kind::UnspecifiedKind,
                vec!["class _HiddenEntity {}".to_string()],
            ),
        ] {
            let raw = RawDocument {
                symbols: vec![SymbolInformation {
                    symbol: "local 0".into(),
                    kind: EnumOrUnknown::new(kind),
                    ..Default::default()
                }],
                occurrences: vec![Occurrence {
                    symbol: "local 0".into(),
                    symbol_roles: 1,
                    range: vec![0, 6, 19],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let document = Document {
                path: "lib/source.dart".into(),
                dart_private: occurrences(&raw, &lines),
                dart_library: Some("lib/source.dart".into()),
                ..Default::default()
            };
            let gaps = coverage(&config, &map, &[document], &[], &GlobSet::empty()).unwrap();
            assert_eq!(gaps.len(), 1, "{gaps:?}");
            assert!(gaps[0].reason.contains("defines"));
        }
    }

    #[test]
    fn unknown_private_targets_stay_partial_only_under_potentially_applicable_rules() {
        let config: Config = serde_norway::from_str("version: 1\ncomponents:\n  target: {in: lib/target.dart, public_symbols: []}\n  source: {in: lib/source.dart}\n").unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let mut source = Document {
            path: "lib/source.dart".into(),
            dart_library: Some("lib/source.dart".into()),
            dart_private: vec![Symbol {
                name: Some("_unknown".into()),
                line: 3,
                definition: false,
                same_file: false,
                is_type: None,
                owner: None,
                owner_unknown: false,
            }],
            ..Default::default()
        };
        let target = Document {
            path: "lib/target.dart".into(),
            dart_library: Some("lib/target.dart".into()),
            ..Default::default()
        };
        assert!(
            coverage(
                &config,
                &map,
                &[source.clone(), target.clone()],
                &[],
                &GlobSet::empty()
            )
            .unwrap()
            .is_empty()
        );
        // Ambiguous part ownership cannot prove that the restricted target is unrelated.
        source.dart_library = None;
        assert_eq!(
            coverage(&config, &map, &[source, target], &[], &GlobSet::empty())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn missing_member_owner_does_not_certify_public_symbol_coverage() {
        let config: Config = serde_norway::from_str("version: 1\ncomponents:\n  provider: {in: lib/provider.dart, public_symbols: [_method]}\n  consumer: {in: lib/consumer.dart}\n").unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let reference = Symbol {
            name: Some("_method".into()),
            line: 2,
            definition: false,
            same_file: false,
            is_type: Some(false),
            owner: None,
            owner_unknown: false,
        };
        let definition = Symbol {
            definition: true,
            same_file: true,
            owner_unknown: true,
            ..reference.clone()
        };
        let documents = [
            Document {
                path: "lib/consumer.dart".into(),
                dart_library: Some("lib/api.dart".into()),
                dart_private: vec![reference],
                ..Default::default()
            },
            Document {
                path: "lib/provider.dart".into(),
                dart_library: Some("lib/api.dart".into()),
                dart_private: vec![definition],
                ..Default::default()
            },
        ];
        // Whitelisting _method cannot prove that its missing containing class
        // would also be allowed, which is how class-member uses are checked.
        assert_eq!(
            coverage(&config, &map, &documents, &[], &GlobSet::empty())
                .unwrap()
                .len(),
            1
        );
    }
}
