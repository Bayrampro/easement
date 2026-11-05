use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::check::{NO_COMPONENT, Violation, describe_chain, file_graph, reach, shortest_chain};
use crate::components::ComponentMap;
use crate::config::Config;
use crate::decision::{self, Refusal};
use crate::report::Confidence;
use crate::scan::Dep;

/// What depends on a set of files, directly or through other files.
#[derive(Serialize)]
pub struct Impact {
    pub changed: Vec<String>,
    /// Files the change deletes: what imported them is not in the code any more.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
    pub dependents: Vec<Dependent>,
    /// `full`, or `partial` when dependents may be missing.
    pub confidence: Confidence,
    pub confidence_reasons: Vec<String>,
    /// Set when the indexes came from the cache: what their key leaves out and
    /// how to index again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached: Option<String>,
}

#[derive(Serialize)]
pub struct Dependent {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
}

/// The files of the project that `paths` name, a file or everything under a directory.
pub fn files_under(paths: &[String], files: &[String]) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for path in paths {
        let path = path.trim_end_matches('/');
        let before = found.len();
        found.extend(
            files
                .iter()
                .filter(|f| path.is_empty() || *f == path || f.starts_with(&format!("{path}/")))
                .cloned(),
        );
        if found.len() == before {
            bail!("`{path}` matches no file of the project (or `exclude` hides it)");
        }
    }
    Ok(found)
}

/// Why an answer may be short: source files no indexer read, whose imports are unknown.
fn uncovered_reason(uncovered: &[String]) -> Option<String> {
    (!uncovered.is_empty()).then(|| {
        format!(
            "{} source files were not covered by an indexer, so imports in them are missing; `easement check` lists them",
            uncovered.len()
        )
    })
}

/// Every file that imports one of `changed`, directly or through other files.
pub fn impact(
    map: &ComponentMap,
    deps: &[Dep],
    files: &[String],
    changed: BTreeSet<String>,
    deleted: Vec<String>,
    uncovered: &[String],
) -> Impact {
    let mut importers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (from, out) in file_graph(deps, files) {
        for (to, _) in out {
            importers.entry(to).or_default().insert(from.clone());
        }
    }
    let reached = reach(changed.iter().map(String::as_str), |file| {
        importers
            .get(file)
            .into_iter()
            .flatten()
            .map(String::as_str)
    });
    let dependents = reached
        .into_iter()
        .filter(|file| !changed.contains(*file))
        .map(|file| Dependent {
            file: file.to_string(),
            component: map.component_of(file).map(str::to_string),
        })
        .collect();
    let mut confidence_reasons: Vec<String> = uncovered_reason(uncovered).into_iter().collect();
    if !deleted.is_empty() {
        confidence_reasons.push(format!(
            "{} deleted file(s): what imported them is not in the code any more; `easement check` and the build show the broken imports",
            deleted.len()
        ));
    }
    Impact {
        changed: changed.into_iter().collect(),
        deleted,
        dependents,
        confidence: Confidence::of(&confidence_reasons),
        confidence_reasons,
        cached: None,
    }
}

/// The text form of [`impact`]: the changed files, then their dependents by component.
pub fn render(impact: &Impact, map: &ComponentMap) -> String {
    let components = |files: &mut dyn Iterator<Item = &str>| -> String {
        let names: BTreeSet<&str> = files
            .map(|f| map.component_of(f).unwrap_or(NO_COMPONENT))
            .collect();
        if names.is_empty() {
            String::new()
        } else {
            format!(" in {}", names.into_iter().collect::<Vec<_>>().join(", "))
        }
    };
    let mut out = format!(
        "Changed: {} file(s){}\n",
        impact.changed.len(),
        components(&mut impact.changed.iter().map(String::as_str))
    );
    if !impact.deleted.is_empty() {
        out.push_str(&format!("Deleted: {}\n", impact.deleted.join(", ")));
    }
    if impact.dependents.is_empty() {
        out.push_str("Nothing depends on them.\n");
    } else {
        out.push_str(&format!(
            "Depend on them: {} file(s){}\n",
            impact.dependents.len(),
            components(&mut impact.dependents.iter().map(|d| d.file.as_str()))
        ));
        let mut by_component: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for d in &impact.dependents {
            by_component
                .entry(d.component.as_deref().unwrap_or(NO_COMPONENT))
                .or_default()
                .push(&d.file);
        }
        for (component, files) in by_component {
            out.push_str(&format!("  {component}: {}\n", files.join(", ")));
        }
    }
    for reason in &impact.confidence_reasons {
        out.push_str(&format!("Partial: {reason}\n"));
    }
    if let Some(note) = &impact.cached {
        out.push_str(&format!("Cached: {note}\n"));
    }
    out
}

/// What `why` reads from one run of the check.
pub struct Graph<'a> {
    pub map: &'a ComponentMap,
    pub deps: &'a [Dep],
    pub files: &'a [String],
    pub violations: &'a [Violation],
    pub uncovered: &'a [String],
}

/// Why `from` depends on `to`: what the config says, then each import behind
/// it with the violation it raises, if any, or the shortest chain of imports
/// when there is no direct one.
pub fn why(config: &Config, graph: &Graph, from: &str, to: &str) -> Result<String> {
    for name in [from, to] {
        config.require_component(name)?;
    }
    let verdict = decision::pair(config, graph.map, from, to);
    let said = match &verdict.grant {
        Ok(grant) => format!("is allowed ({})", grant.as_str()),
        Err(Refusal::Prohibited { reach: true }) => "is forbidden (must_not_reach)".to_string(),
        Err(Refusal::Prohibited { reach: false }) => {
            "is forbidden (must_not_depend_on)".to_string()
        }
        Err(Refusal::NotGranted { layers: Some(why) }) => format!("is not allowed: {why}"),
        Err(Refusal::NotGranted { layers: None }) => "is not allowed".to_string(),
    };
    let because = verdict
        .because
        .map(|b| format!(": {b}"))
        .unwrap_or_default();
    let mut out = format!("{from} -> {to} {said}{because}\n");
    if let Some(importers) = verdict.importers {
        out.push_str(&format!(
            "  but {to} may be imported only from {}\n",
            importers.join(", ")
        ));
    }
    // What a grant leaves to the path of each import.
    if !verdict.rejected() {
        for condition in verdict.conditions(to) {
            out.push_str(&format!("  but {condition}\n"));
        }
    }
    for name in [from, to] {
        if let Some(owner) = &config.components[name].owner {
            out.push_str(&format!("  owner of {name}: {owner}\n"));
        }
    }
    let of = |file: &str| graph.map.component_of(file);
    let files = file_graph(graph.deps, graph.files);
    // One line per import, however many files of a package it reaches.
    let mut direct: BTreeMap<(&str, usize), Vec<&str>> = BTreeMap::new();
    for (file, out) in files.iter().filter(|(f, _)| of(f) == Some(from)) {
        for (target, line) in out.iter().filter(|(t, _)| of(t) == Some(to)) {
            direct
                .entry((file.as_str(), *line))
                .or_default()
                .push(target);
        }
    }
    if direct.is_empty() {
        let starts = files
            .keys()
            .map(String::as_str)
            .filter(|f| of(f) == Some(from));
        let is_hop = |f: &str| !matches!(of(f), Some(c) if c == from || c == to);
        match shortest_chain(&files, starts, is_hop, |f| of(f) == Some(to)) {
            Some((hops, last)) => {
                let (through, chain) = describe_chain(&hops, last, of);
                out.push_str(&format!(
                    "{from} imports nothing of {to} directly; it reaches {to} through {through}:\n  {chain}\n"
                ));
            }
            None => out.push_str(&format!("{from} does not depend on {to}\n")),
        }
    } else {
        out.push_str(&format!("{} import(s):\n", direct.len()));
        for ((file, line), targets) in direct {
            out.push_str(&format!("  {file}:{line} imports {}", targets.join(", ")));
            let raised: BTreeSet<&str> = graph
                .violations
                .iter()
                .filter(|v| v.file == file && v.line == line && v.from == from)
                .map(|v| v.rule.as_str())
                .collect();
            if !raised.is_empty() {
                let rules: Vec<&str> = raised.into_iter().collect();
                out.push_str(&format!(" [violates: {}]", rules.join(", ")));
            }
            out.push('\n');
        }
    }
    if let Some(reason) = uncovered_reason(graph.uncovered) {
        out.push_str(&format!("Partial: {reason}\n"));
    }
    Ok(out)
}
