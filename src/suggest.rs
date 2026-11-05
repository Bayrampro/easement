use std::collections::{BTreeMap, BTreeSet};

use crate::check::Edge;
use crate::components::ComponentMap;
use crate::config::Config;
use crate::decision::{self, Refusal};
use crate::lang::Target;
use crate::scan::Dep;

/// What the current dependency graph says about the config: allowances nothing
/// uses, imports nothing allows, and components that look isolated already.
pub fn suggest(
    config: &Config,
    map: &ComponentMap,
    edges: &[Edge],
    deps: &[Dep],
    files: &[String],
    uncovered: &[String],
    stale_arrows: &[(String, String)],
) -> String {
    let Counts { unindexed, used } = counts(map, edges, uncovered);

    let mut out = String::new();

    let allowances = unused_allowances(config, &used, &unindexed);
    section(
        &mut out,
        "Allowed in may_depend_on but never imported (remove to tighten the rules):",
        allowances
            .removable
            .iter()
            .map(|(from, to)| format!("{from} -> {to}"))
            .collect(),
    );
    section(
        &mut out,
        "Not imported among the indexed files, but some files were not indexed, so the import may be there (do not remove without checking):",
        allowances.unreliable,
    );

    // A forbidden import cannot be allowed: the config would contradict itself,
    // or route around a closed or sibling layer.
    let mut missing: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut forbidden: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for ((from, to), count) in &used {
        let list = match decision::pair(config, map, from, to).grant {
            Ok(_) => continue,
            Err(Refusal::NotGranted { layers: None }) => &mut missing,
            Err(_) => &mut forbidden,
        };
        list.entry(from)
            .or_default()
            .push(format!("{to} ({count} import(s))"));
    }
    let lines = |list: BTreeMap<&str, Vec<String>>| {
        list.into_iter()
            .map(|(from, targets)| format!("{from}: {}", targets.join(", ")))
            .collect()
    };
    section(
        &mut out,
        "Imported but not allowed (add to may_depend_on, or fix the imports):",
        lines(missing),
    );
    section(
        &mut out,
        "Imported but forbidden by the config (fix the imports):",
        lines(forbidden),
    );

    section(
        &mut out,
        "Arrows of the diagram that no import stands for (drop them, or the dependency is still to come):",
        stale_arrows
            .iter()
            .map(|(from, to)| {
                // An import of it may sit in a file no indexer read.
                let unread: usize = [from, to]
                    .iter()
                    .filter_map(|c| unindexed.get(c.as_str()))
                    .sum();
                if unread == 0 {
                    format!("{from} -> {to}")
                } else {
                    format!("{from} -> {to} (unreliable: {unread} file(s) not indexed)")
                }
            })
            .collect(),
    );

    section(
        &mut out,
        "Instances that never import each other (candidates for `isolated: true`):",
        isolation_candidates(config, map, deps, files, &unindexed),
    );

    if out.is_empty() {
        out.push_str("Nothing to suggest: the config matches the dependency graph.\n");
    }
    out
}

/// `may_depend_on` entries that no import uses.
struct Allowances {
    /// Safe to remove: both components were read in full.
    removable: Vec<(String, String)>,
    /// Not imported among the indexed files, but files of a side were not indexed.
    unreliable: Vec<String>,
}

fn unused_allowances(
    config: &Config,
    used: &BTreeMap<(&str, &str), usize>,
    unindexed: &BTreeMap<&str, usize>,
) -> Allowances {
    let mut found = Allowances {
        removable: Vec::new(),
        unreliable: Vec::new(),
    };
    for (from, rule) in &config.rules {
        for to in rule.may_depend_on.iter().map(|l| l.name()) {
            if used.contains_key(&(from.as_str(), to)) {
                continue;
            }
            let gaps: Vec<String> = [from.as_str(), to]
                .into_iter()
                .filter_map(|c| Some(format!("{} file(s) of {c}", unindexed.get(c)?)))
                .collect();
            if gaps.is_empty() {
                found.removable.push((from.clone(), to.to_string()));
            } else {
                found
                    .unreliable
                    .push(format!("{from} -> {to} (not indexed: {})", gaps.join(", ")));
            }
        }
    }
    found
}

/// The `may_depend_on` entries that can go: no import uses them, and every file of both
/// components was read.
pub fn removable(
    config: &Config,
    map: &ComponentMap,
    edges: &[Edge],
    uncovered: &[String],
) -> Vec<(String, String)> {
    let Counts { unindexed, used } = counts(map, edges, uncovered);
    unused_allowances(config, &used, &unindexed).removable
}

/// What `suggest` reads off the graph: the files of each component that no indexer
/// read, and the imports between each pair of components.
struct Counts<'a> {
    /// Files no indexer saw have no edges: a missing import next to them proves nothing.
    unindexed: BTreeMap<&'a str, usize>,
    used: BTreeMap<(&'a str, &'a str), usize>,
}

fn counts<'a>(map: &'a ComponentMap, edges: &'a [Edge], uncovered: &[String]) -> Counts<'a> {
    let mut unindexed: BTreeMap<&str, usize> = BTreeMap::new();
    for file in uncovered {
        if let Some(component) = map.component_of(file) {
            *unindexed.entry(component).or_default() += 1;
        }
    }
    let mut used: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for e in edges {
        *used.entry((&e.from, &e.to)).or_default() += 1;
    }
    Counts { unindexed, used }
}

fn section(out: &mut String, title: &str, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(title);
    out.push('\n');
    for item in items {
        out.push_str(&format!("  {item}\n"));
    }
}

/// Components whose `in` pattern has a `*` (one directory per instance), that
/// are not isolated yet, have at least two instances and no import between them.
fn isolation_candidates(
    config: &Config,
    map: &ComponentMap,
    deps: &[Dep],
    files: &[String],
    unindexed: &BTreeMap<&str, usize>,
) -> Vec<String> {
    // (component, pattern) -> instances seen, and whether instances import each other
    let mut groups: BTreeMap<(String, String), (BTreeSet<String>, bool)> = BTreeMap::new();
    let instance = |file: &str, pattern: &str| {
        let n = pattern.split('/').count();
        file.split('/').take(n).collect::<Vec<_>>().join("/")
    };
    let locate = |file: &str| {
        let (component, pattern) = map.component_and_pattern(file)?;
        // `**` spans directory levels, so it cannot tell instances apart.
        if config.components[component].isolated || !pattern.contains('*') || pattern.contains("**")
        {
            return None;
        }
        Some((
            (component.to_string(), pattern.to_string()),
            instance(file, pattern),
        ))
    };
    for file in files {
        if let Some((key, here)) = locate(file) {
            groups.entry(key).or_default().0.insert(here);
        }
    }
    for dep in deps {
        let Target::Internal(target) = &dep.target else {
            continue;
        };
        let (file, target) = (dep.from.key(), target.key());
        let Some((key, here)) = locate(&file) else {
            continue;
        };
        let entry = groups.entry(key.clone()).or_default();
        entry.0.insert(here.clone());
        if let Some((there_key, there)) = locate(&target)
            && there_key == key
            && there != here
        {
            entry.1 = true;
        }
    }
    groups
        .into_iter()
        .filter(|(_, (instances, crossing))| instances.len() > 1 && !crossing)
        .map(|((component, pattern), (instances, _))| {
            let names: Vec<_> = instances
                .iter()
                .map(|i| i.rsplit('/').next().unwrap_or(i).to_string())
                .collect();
            let note = match unindexed.get(component.as_str()) {
                Some(n) => format!(" [unreliable: {n} file(s) not indexed]"),
                None => String::new(),
            };
            format!("{component} (`{pattern}`): {}{note}", names.join(", "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn dep(file: &str, target: &str) -> Dep {
        Dep {
            from: file.into(),
            file: PathBuf::from(file),
            line: 1,
            specifier: String::new(),
            target: Target::Internal(target.into()),
        }
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            file: format!("{from}/x"),
            line: 1,
        }
    }

    #[test]
    fn suggests_removals_additions_and_isolation() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  web: { in: src/web }
  core: { in: src/core }
  data: { in: src/data }
  pages: { in: 'src/pages/*' }
rules:
  web: { may_depend_on: [core, data] }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let edges = [edge("web", "core"), edge("core", "data")];
        let deps = [
            dep("src/pages/a/x.ts", "src/core/c.ts"),
            dep("src/pages/b/y.ts", "src/pages/b/z.ts"),
        ];
        let files = [
            "src/pages/a/x.ts".to_string(),
            "src/pages/b/y.ts".to_string(),
            "src/pages/b/z.ts".to_string(),
            "src/pages/c/leaf.ts".to_string(),
        ];
        let out = suggest(&config, &map, &edges, &deps, &files, &[], &[]);
        assert!(out.contains("  web -> data\n"), "{out}");
        assert!(!out.contains("web -> core"), "{out}");
        assert!(out.contains("  core: data (1 import(s))\n"), "{out}");
        assert!(out.contains("  pages (`src/pages/*`): a, b, c\n"), "{out}");

        let crossing = [dep("src/pages/a/x.ts", "src/pages/b/y.ts")];
        assert!(!suggest(&config, &map, &edges, &crossing, &files, &[], &[]).contains("pages"));
    }

    #[test]
    fn does_not_trust_a_missing_import_next_to_files_nothing_indexed() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  web: { in: src/web }
  core: { in: src/core }
  generated: { in: src/generated }
rules:
  web: { may_depend_on: [core, generated] }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let edges = [edge("web", "core")];
        let uncovered = ["src/generated/index.ts".to_string()];
        let out = suggest(&config, &map, &edges, &[], &[], &uncovered, &[]);
        assert!(
            out.contains("  web -> generated (not indexed: 1 file(s) of generated)\n"),
            "{out}"
        );
        // It is not offered as a plain removal.
        assert!(!out.contains("  web -> generated\n"), "{out}");
        let clean = suggest(&config, &map, &edges, &[], &[], &[], &[]);
        assert!(clean.contains("  web -> generated\n"), "{clean}");
    }

    #[test]
    fn a_double_star_pattern_is_never_an_isolation_candidate() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  routes: { in: 'src/routes/**' }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let files = [
            "src/routes/a/x.ts".to_string(),
            "src/routes/b/y.ts".to_string(),
        ];
        let out = suggest(&config, &map, &[], &[], &files, &[], &[]);
        assert!(!out.contains("isolated"), "{out}");
    }

    #[test]
    fn isolation_candidates_with_unindexed_files_are_marked() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  pages: { in: 'src/pages/*' }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let files = [
            "src/pages/a/x.ts".to_string(),
            "src/pages/b/y.ts".to_string(),
        ];
        let uncovered = ["src/pages/c/z.ts".to_string()];
        let out = suggest(&config, &map, &[], &[], &files, &uncovered, &[]);
        assert!(out.contains("[unreliable: 1 file(s) not indexed]"), "{out}");
    }
}
