//! Policy-aware coverage checks over document-local Dart evidence.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use globset::GlobSet;

use crate::check::{self, SymbolRef};
use crate::components::ComponentMap;
use crate::config::Config;
use crate::lang::{Target, Unit, registry};
use crate::scan::{AnalysisGap, TypeDef};
use crate::scip::dart::private::{Evidence, Symbol};

fn same_library(left: &Evidence, right: &Evidence) -> bool {
    // Unknown ownership cannot prove that a potential target is unrelated.
    left.library.is_none() || right.library.is_none() || left.library == right.library
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
    documents: &[Evidence],
    hidden: &[Evidence],
    target_only: &GlobSet,
) -> Result<Vec<AnalysisGap>> {
    if !config.scan_policy().uses_symbols
        && !config.components.values().any(|c| !c.defines.is_empty())
    {
        return Ok(Vec::new());
    }
    let all: Vec<&Evidence> = documents
        .iter()
        .chain(hidden)
        .filter(|d| registry::is(Path::new(&d.path), &registry::DART))
        .collect();
    let mut gaps = BTreeSet::new();
    for document in documents.iter().filter(|d| !target_only.is_match(&d.path)) {
        let from = map.component_of(&document.path);
        if from.is_none() && config.forbid_symbol.is_empty() {
            continue;
        }
        for symbol in &document.symbols {
            if symbol.definition {
                let Some(from) = from else {
                    continue;
                };
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
            let candidates: Vec<(&Evidence, &Symbol)> = all
                .iter()
                .filter(|d| same_library(document, d))
                .flat_map(|d| {
                    d.symbols
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
                    from: Unit::from(document.path.as_str()),
                    line: symbol.line,
                    specifier: target
                        .path
                        .strip_suffix(".dart")
                        .unwrap_or(&target.path)
                        .into(),
                    name: name.clone(),
                    method: None,
                    target: Some(Target::Internal(Unit::from(target.path.as_str()))),
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
                relevant |= !config.forbid_symbol.is_empty()
                    || from
                        .and_then(|from| config.rules.get(from))
                        .is_some_and(|r| !r.forbid_symbol.is_empty())
                    || all
                        .iter()
                        .filter(|d| same_library(document, d))
                        .filter_map(|d| map.component_of(&d.path))
                        .any(|to| {
                            Some(to) != from && config.components[to].public_symbols.is_some()
                        });
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
    use crate::scip::dart::private::{Inventory, occurrences};
    use ::scip::types::{
        Document as RawDocument, Occurrence, SymbolInformation, symbol_information::Kind,
    };
    use protobuf::EnumOrUnknown;
    use protobuf::Message;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[test]
    fn real_dart_index_keeps_private_type_evidence_only_where_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dart/basic");
        let bytes = include_bytes!("../../../tests/fixtures/dart/basic/evidence.scip");
        let index = ::scip::types::Index::parse_from_bytes(bytes).unwrap();
        let documents = crate::scip::parse(bytes, &root, Path::new("")).unwrap();
        let inventory = Inventory::read(&index, &root, Path::new(""), &documents);
        let found: Vec<_> = inventory
            .documents
            .values()
            .flat_map(|d| d.symbols.iter().map(|s| (d.path.as_str(), s)))
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
            let document = Evidence {
                path: "lib/source.dart".into(),
                symbols: occurrences(&raw, &lines),
                library: Some("lib/source.dart".into()),
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
        let mut source = Evidence {
            path: "lib/source.dart".into(),
            library: Some("lib/source.dart".into()),
            symbols: vec![Symbol {
                name: Some("_unknown".into()),
                line: 3,
                definition: false,
                same_file: false,
                is_type: None,
                owner: None,
                owner_unknown: false,
            }],
        };
        let target = Evidence {
            path: "lib/target.dart".into(),
            library: Some("lib/target.dart".into()),
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
        let global_config: Config = serde_norway::from_str(
            "version: 1\nforbid_symbol: [lib/target._unknown]\ncomponents:\n  source: {in: lib/source.dart}\n",
        )
        .unwrap();
        let global_map = ComponentMap::new(&global_config).unwrap();
        assert_eq!(
            coverage(
                &global_config,
                &global_map,
                &[source.clone()],
                &[],
                &GlobSet::empty(),
            )
            .unwrap()
            .len(),
            1,
        );
        let unowned_config: Config = serde_norway::from_str(
            "version: 1\nforbid_symbol: [lib/target._unknown]\ncomponents: {}\n",
        )
        .unwrap();
        let unowned_map = ComponentMap::new(&unowned_config).unwrap();
        assert_eq!(
            coverage(
                &unowned_config,
                &unowned_map,
                &[source.clone()],
                &[],
                &GlobSet::empty(),
            )
            .unwrap()
            .len(),
            1,
        );
        // Ambiguous part ownership cannot prove that the restricted target is unrelated.
        source.library = None;
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
            Evidence {
                path: "lib/consumer.dart".into(),
                library: Some("lib/api.dart".into()),
                symbols: vec![reference],
            },
            Evidence {
                path: "lib/provider.dart".into(),
                library: Some("lib/api.dart".into()),
                symbols: vec![definition],
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

    #[test]
    fn merged_private_evidence_rechecks_current_policy_and_target_only() {
        let strict: Config = serde_norway::from_str(
            "version: 1\ncomponents:\n  entities: {in: lib/entities, defines: ['*Entity']}\n  other: {in: lib/other}\n",
        ).unwrap();
        let map = ComponentMap::new(&strict).unwrap();
        let mut inventory = Inventory::default();
        for (at, name) in ["_FirstEntity", "_SecondEntity"].into_iter().enumerate() {
            let evidence = Evidence {
                path: "lib/other/wrong.dart".into(),
                library: Some("lib/other/wrong.dart".into()),
                symbols: vec![Symbol {
                    name: Some(name.into()),
                    line: at + 1,
                    definition: true,
                    same_file: true,
                    is_type: Some(true),
                    owner: None,
                    owner_unknown: false,
                }],
            };
            inventory.merge(Inventory {
                documents: BTreeMap::from([(evidence.path.clone(), evidence)]),
            });
        }
        let documents: Vec<_> = inventory.documents.into_values().collect();
        let gaps = coverage(&strict, &map, &documents, &[], &GlobSet::empty()).unwrap();
        assert_eq!(gaps.len(), 2);
        let relaxed: Config = serde_norway::from_str(
            "version: 1\ncomponents:\n  entities: {in: lib/entities}\n  other: {in: lib/other}\n",
        )
        .unwrap();
        assert!(
            coverage(
                &relaxed,
                &ComponentMap::new(&relaxed).unwrap(),
                &documents,
                &[],
                &GlobSet::empty()
            )
            .unwrap()
            .is_empty()
        );
        let target_only =
            crate::scan::glob_set(&["lib/other/wrong.dart".into()], "target_only").unwrap();
        assert!(
            coverage(&strict, &map, &documents, &[], &target_only)
                .unwrap()
                .is_empty()
        );
    }
}
