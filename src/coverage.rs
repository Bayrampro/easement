//! Which absence claims the available extraction facts can establish.
use std::collections::{BTreeMap, BTreeSet};

use crate::check::{Key, RuleId};
use crate::components::ComponentMap;
use crate::lang::{Target, registry};
use crate::scan::{AnalysisGap, Dep};
use crate::scope::Scope;

/// Runtime provenance, rebuilt from the current inputs even on a cache hit.
#[derive(Default)]
pub struct Domain {
    pub root: String,
    pub language: &'static str,
    pub files: BTreeSet<String>,
    pub gaps: Vec<AnalysisGap>,
}

/// Symbol names and missing classification, without losing their language domain.
#[derive(Default)]
pub struct Symbols {
    pub known: BTreeMap<String, BTreeSet<(String, String)>>,
    pub incomplete: BTreeSet<String>,
}

impl Symbols {
    pub fn insert(&mut self, domain: &str, qualifier: String, name: String) {
        self.known
            .entry(domain.into())
            .or_default()
            .insert((qualifier, name));
    }
}

pub struct Input<'a> {
    pub files: &'a [String],
    pub uncovered: &'a [String],
    pub gaps: &'a [AnalysisGap],
    pub domains: &'a [Domain],
    pub unnamed: &'a [String],
}

pub struct Coverage {
    files: BTreeSet<String>,
    uncovered: BTreeSet<String>,
    partial: BTreeMap<AnalysisGap, BTreeSet<String>>,
    uncertain: BTreeSet<String>,
    symbol_incomplete: BTreeSet<String>,
}

fn language(file: &str) -> Option<&'static str> {
    registry::of(file).map(|language| language.name)
}

impl Coverage {
    pub fn new(input: Input<'_>) -> Self {
        let files: BTreeSet<String> = input.files.iter().cloned().collect();
        let uncovered: BTreeSet<String> = input.uncovered.iter().cloned().collect();
        let mut partial = BTreeMap::new();
        for gap in input.gaps {
            let origins: Vec<_> = input
                .domains
                .iter()
                .filter(|d| d.gaps.contains(gap))
                .collect();
            let package = origins.iter().any(|domain| domain.root == gap.file);
            let affected = if language(&gap.file).is_some() && !package {
                // Keep absent generated paths as well as files present in the walk.
                BTreeSet::from([gap.file.clone()])
            } else {
                if origins.is_empty() {
                    // A producer without provenance cannot establish a narrower claim.
                    files.clone()
                } else {
                    origins
                        .into_iter()
                        .flat_map(|d| &d.files)
                        .filter(|f| files.contains(*f))
                        .cloned()
                        .collect()
                }
            };
            partial.insert(gap.clone(), affected);
        }
        let uncertain: BTreeSet<String> = uncovered
            .iter()
            .cloned()
            .chain(partial.values().flatten().cloned())
            .collect();
        let mut symbol_incomplete: BTreeSet<String> = uncertain
            .iter()
            .chain(input.unnamed)
            .filter_map(|f| language(f).map(str::to_string))
            .collect();
        // Only languages with verified symbol uses can establish their absence.
        for file in &files {
            if !registry::of(file).is_some_and(|language| language.symbols) {
                symbol_incomplete.insert(language(file).unwrap_or("").into());
            }
        }
        for domain in input.domains {
            if domain.files.iter().any(|f| uncertain.contains(f)) {
                symbol_incomplete.insert(domain.language.into());
            }
        }
        Self {
            files,
            uncovered,
            partial,
            uncertain,
            symbol_incomplete,
        }
    }

    pub fn uncertain(&self) -> Vec<String> {
        self.uncertain.iter().cloned().collect()
    }

    pub fn gap_in_scope(&self, gap: &AnalysisGap, scope: &Scope, map: &ComponentMap) -> bool {
        self.partial
            .get(gap)
            .is_some_and(|files| files.iter().any(|file| scope.contains_coverage(map, file)))
    }

    pub fn uncovered(&self) -> Vec<String> {
        self.uncovered.iter().cloned().collect()
    }

    /// A component can use symbols defined outside itself, so all domains of its
    /// source languages participate. Global queries require every project domain.
    pub fn symbols_for(
        &self,
        component: Option<&str>,
        map: &ComponentMap,
        symbols: &Symbols,
    ) -> Option<Vec<(String, String)>> {
        let domains: BTreeSet<String> = self
            .files
            .iter()
            .filter(|file| {
                component.is_none_or(|c| map.possible_components_of_file(file).contains(c))
            })
            .filter_map(|file| language(file).map(str::to_string))
            .collect();
        if domains.is_empty()
            || symbols.incomplete.contains("")
            || domains
                .iter()
                .any(|d| self.symbol_incomplete.contains(d) || symbols.incomplete.contains(d))
        {
            return None;
        }
        Some(
            domains
                .iter()
                .filter_map(|d| symbols.known.get(d))
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        )
    }

    fn footprint(&self, key: &Key, map: &ComponentMap, deps: &[Dep]) -> BTreeSet<String> {
        let all: BTreeSet<String> = self.files.iter().chain(&self.uncertain).cloned().collect();
        let component_files = |names: &BTreeSet<&str>| {
            all.iter()
                .filter(|f| {
                    map.possible_components_of_file(f)
                        .iter()
                        .any(|c| names.contains(c))
                })
                .cloned()
                .collect()
        };
        match key.rule {
            RuleId::Cycle => component_files(&key.from.split(',').collect()),
            RuleId::Metric | RuleId::Diagram => {
                component_files(&BTreeSet::from([key.from.as_str()]))
            }
            RuleId::Required => {
                let directory = key.file.trim_end_matches('/').trim_start_matches("./");
                all.iter()
                    .filter(|f| crate::relpath::parent(f) == directory)
                    .cloned()
                    .collect()
            }
            RuleId::Reach => {
                // File reach deliberately overapproximates symbol reach. Missing
                // symbol edges must never make a debt look demonstrably absent.
                let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                for dep in deps {
                    let Target::Internal(target) = &dep.target else {
                        continue;
                    };
                    let target = crate::scan::slash(target.path());
                    let source = crate::scan::slash(&dep.file);
                    graph.entry(source).or_default().extend(
                        all.iter()
                            .filter(|file| {
                                file.as_str() == target || crate::relpath::parent(file) == target
                            })
                            .cloned(),
                    );
                }
                let mut visited: BTreeSet<&str> = all
                    .iter()
                    .filter(|file| {
                        map.possible_components_of_file(file)
                            .contains(key.from.as_str())
                    })
                    .map(String::as_str)
                    .collect();
                let mut pending: Vec<&str> = visited.iter().copied().collect();
                while let Some(file) = pending.pop() {
                    if map.possible_components_of_file(file) == BTreeSet::from([key.to.as_str()]) {
                        continue;
                    }
                    for next in graph.get(file).into_iter().flatten() {
                        if visited.insert(next.as_str()) {
                            pending.push(next.as_str());
                        }
                    }
                }
                visited.into_iter().map(str::to_string).collect()
            }
            _ => BTreeSet::from([key.file.clone()]),
        }
    }

    pub fn can_retire(&self, key: &Key, map: &ComponentMap, deps: &[Dep]) -> bool {
        self.footprint(key, map, deps).is_disjoint(&self.uncertain)
    }

    pub fn key_in_scope(&self, key: &Key, map: &ComponentMap, deps: &[Dep], scope: &Scope) -> bool {
        let files = self.footprint(key, map, deps);
        // A removed component has no current files; a whole-project run still
        // retires its old keys, whereas a narrowed report makes no such claim.
        if files.is_empty() {
            return scope.is_whole();
        }
        let owners: Option<BTreeSet<&str>> = match key.rule {
            RuleId::Cycle => Some(key.from.split(',').collect()),
            RuleId::Metric | RuleId::Diagram | RuleId::Required => {
                Some(BTreeSet::from([key.from.as_str()]))
            }
            _ => None,
        };
        files.iter().any(|file| {
            let possible = map.possible_components_of_file(file);
            if let Some(owners) = &owners {
                possible
                    .intersection(owners)
                    .any(|owner| scope.contains_owner(file, Some(owner)))
            } else if key.rule == RuleId::Reach {
                scope.contains(map, file)
                    || [key.from.as_str(), key.to.as_str()].iter().any(|owner| {
                        possible.contains(owner) && scope.contains_owner(file, Some(owner))
                    })
            } else if possible.contains(key.from.as_str()) {
                scope.contains_owner(file, Some(key.from.as_str()))
            } else {
                scope.contains(map, file)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::lang::Unit;

    fn config() -> Config {
        serde_norway::from_str(concat!(
            "version: 1\ncomponents:\n  a: {in: a}\n  b: {in: b}\n  c: {in: c}\n",
            "  unrelated: {in: unrelated}\n",
        ))
        .unwrap()
    }

    fn fixture(gaps: &[&str]) -> Coverage {
        let files =
            ["a/file.go", "b/file.go", "c/file.go", "unrelated/file.dart"].map(String::from);
        let gaps: Vec<_> = gaps
            .iter()
            .map(|file| AnalysisGap {
                file: (*file).into(),
                line: Some(1),
                reason: "incomplete facts".into(),
            })
            .collect();
        Coverage::new(Input {
            files: &files,
            uncovered: &[],
            gaps: &gaps,
            domains: &[],
            unnamed: &[],
        })
    }

    fn key(rule: RuleId, from: &str, to: &str, file: &str) -> Key {
        Key {
            rule,
            from: from.into(),
            to: to.into(),
            file: file.into(),
            target: String::new(),
        }
        .canonical()
    }

    fn dependency(from: &str, to: &str) -> Dep {
        Dep {
            file: from.into(),
            from: Unit::from(from),
            target: Target::Internal(Unit::from(to)),
            line: 1,
            specifier: to.into(),
        }
    }

    #[test]
    fn local_debt_and_directory_requirements_follow_the_affected_files() {
        let map = ComponentMap::new(&config()).unwrap();
        let coverage = fixture(&["a/file.go", "a/missing.g.go"]);
        assert!(!coverage.can_retire(&key(RuleId::Dependency, "a", "b", "a/file.go"), &map, &[]));
        assert!(!coverage.can_retire(&key(RuleId::Symbol, "a", "b", "a/missing.g.go"), &map, &[]));
        assert!(coverage.can_retire(&key(RuleId::Dependency, "b", "a", "b/file.go"), &map, &[]));
        assert!(!coverage.can_retire(&key(RuleId::Required, "a", "", "a/file.go"), &map, &[]));
        assert!(coverage.can_retire(&key(RuleId::Required, "b", "", "b/file.go"), &map, &[]));
    }

    #[test]
    fn canonical_graph_keys_are_protected_only_by_their_components() {
        let map = ComponentMap::new(&config()).unwrap();
        let coverage = fixture(&["a/file.go"]);
        for rule in [RuleId::Cycle, RuleId::Metric, RuleId::Diagram] {
            let affected = key(
                rule,
                if rule == RuleId::Cycle { "a,b" } else { "a" },
                "b",
                "",
            );
            let independent = key(
                rule,
                if rule == RuleId::Cycle { "b,c" } else { "b" },
                "c",
                "",
            );
            assert!(!coverage.can_retire(&affected, &map, &[]), "{rule:?}");
            assert!(coverage.can_retire(&independent, &map, &[]), "{rule:?}");
            let scope = Scope::new(&config(), &["a".into()], &[]).unwrap();
            assert!(coverage.key_in_scope(&affected, &map, &[], &scope));
            assert!(!coverage.key_in_scope(&independent, &map, &[], &scope));
        }
    }

    #[test]
    fn reach_protects_an_incomplete_hop_even_without_a_known_path_to_the_target() {
        let map = ComponentMap::new(&config()).unwrap();
        let reach = key(RuleId::Reach, "a", "c", "");
        let deps = [dependency("a/file.go", "b/file.go")];
        assert!(!fixture(&["b/file.go"]).can_retire(&reach, &map, &deps));
        assert!(fixture(&["unrelated/file.dart"]).can_retire(&reach, &map, &deps));
        assert!(fixture(&["b/file.go"]).can_retire(&reach, &map, &[]));
        let scope = Scope::new(&config(), &[], &["b".into()]).unwrap();
        assert!(fixture(&[]).key_in_scope(&reach, &map, &deps, &scope));
    }

    #[test]
    fn package_gaps_are_expanded_only_to_the_producing_domain() {
        let files = ["a/file.go", "unrelated/file.dart"].map(String::from);
        let gap = AnalysisGap {
            file: String::new(),
            line: Some(1),
            reason: "external diagnostic".into(),
        };
        let domains = [Domain {
            root: String::new(),
            language: "dart",
            files: BTreeSet::from([files[1].clone()]),
            gaps: vec![gap.clone()],
        }];
        let coverage = Coverage::new(Input {
            files: &files,
            uncovered: &[],
            gaps: std::slice::from_ref(&gap),
            domains: &domains,
            unnamed: &[],
        });
        assert_eq!(coverage.uncertain(), [files[1].clone()]);
        let scope = Scope::new(&config(), &["a".into()], &[]).unwrap();
        assert!(!coverage.gap_in_scope(&gap, &scope, &ComponentMap::new(&config()).unwrap()));
    }

    #[test]
    fn symbols_require_all_relevant_language_domains_but_not_unrelated_ones() {
        let map = ComponentMap::new(&config()).unwrap();
        let coverage = fixture(&["unrelated/file.dart"]);
        let mut symbols = Symbols::default();
        symbols.insert("go", "example.com/nested".into(), "Thing".into());
        symbols.insert("dart", "lib/nested".into(), "Thing".into());
        assert_eq!(
            coverage.symbols_for(Some("a"), &map, &symbols).unwrap(),
            [("example.com/nested".into(), "Thing".into())]
        );
        assert!(coverage.symbols_for(None, &map, &symbols).is_none());
        symbols.incomplete.insert("go".into());
        assert!(coverage.symbols_for(Some("a"), &map, &symbols).is_none());
    }

    #[test]
    fn uncovered_remains_distinct_from_partial_documents() {
        let files = ["a/file.go", "b/file.go"].map(String::from);
        let gaps = [AnalysisGap {
            file: files[1].clone(),
            line: None,
            reason: "gap".into(),
        }];
        let coverage = Coverage::new(Input {
            files: &files,
            uncovered: &files[..1],
            gaps: &gaps,
            domains: &[],
            unnamed: &[],
        });
        assert_eq!(coverage.uncovered(), files[..1]);
        assert_eq!(coverage.uncertain(), files);
    }
    #[test]
    fn gaps_in_files_with_parts_also_protect_the_parts_components() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("easement.yml");
        std::fs::write(
            &path,
            "version: 1\ncomponents:\n  app: {in: src}\n  provider: {in: provider}\ntests: auto\n",
        )
        .unwrap();
        let config = crate::policy::Policy::load(&path, None).unwrap().config;
        let map = ComponentMap::new(&config).unwrap();
        let files = ["src/file.rs".to_string(), "provider/file.rs".to_string()];
        let gaps = [AnalysisGap {
            file: files[0].clone(),
            line: None,
            reason: "missing parts".into(),
        }];
        let coverage = Coverage::new(Input {
            files: &files,
            uncovered: &[],
            gaps: &gaps,
            domains: &[],
            unnamed: &[],
        });
        let cycle = key(RuleId::Cycle, "provider,tests", "", "");
        assert!(!coverage.can_retire(&cycle, &map, &[]));
        let scope = Scope::new(&config, &["tests".into()], &[]).unwrap();
        assert!(coverage.key_in_scope(&cycle, &map, &[], &scope));
        assert!(coverage.gap_in_scope(&gaps[0], &scope, &map));
        let ordinary_cycle = key(RuleId::Cycle, "app,provider", "", "");
        assert!(!coverage.key_in_scope(&ordinary_cycle, &map, &[], &scope));
        let ordinary_file = key(RuleId::Dependency, "app", "provider", "src/file.rs");
        assert!(!coverage.key_in_scope(&ordinary_file, &map, &[], &scope));
    }
    #[test]
    fn package_directories_with_source_extensions_are_not_mistaken_for_files() {
        let files = [
            "package.dart/lib/main.dart".to_string(),
            "backend/main.go".to_string(),
        ];
        let gap = AnalysisGap {
            file: "package.dart".into(),
            line: Some(1),
            reason: "package diagnostic".into(),
        };
        let domains = [Domain {
            root: "package.dart".into(),
            language: "dart",
            files: BTreeSet::from([files[0].clone()]),
            gaps: vec![gap.clone()],
        }];
        let coverage = Coverage::new(Input {
            files: &files,
            uncovered: &[],
            gaps: std::slice::from_ref(&gap),
            domains: &domains,
            unnamed: &[],
        });
        assert_eq!(coverage.uncertain(), files[..1]);
    }
    #[test]
    fn symbol_absence_requires_verified_uses_from_the_registry() {
        let map = ComponentMap::new(&config()).unwrap();
        let files = ["a/Example.java".into()];
        let coverage = Coverage::new(Input {
            files: &files,
            uncovered: &[],
            gaps: &[],
            domains: &[],
            unnamed: &[],
        });
        let mut symbols = Symbols::default();
        symbols.insert("java", "example.nested".into(), "Thing".into());
        assert!(coverage.symbols_for(Some("a"), &map, &symbols).is_none());
    }
}
