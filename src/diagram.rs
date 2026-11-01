//! The `diagram` rule: a Mermaid graph in the documentation is the picture
//! of the architecture, and the dependencies between components have to be
//! its arrows.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::check::{Edge, RuleId, Violation};
use crate::config::Config;

/// The arrows of the Mermaid graphs in `text` (a Markdown file with
/// ```` ```mermaid ```` blocks, or a bare `.mmd` file), between the nodes
/// that name components by id or by label.
pub fn arrows(text: &str, components: &BTreeSet<&str>) -> Result<BTreeSet<(String, String)>> {
    let blocks = mermaid_blocks(text);
    if blocks.is_empty() {
        bail!("no Mermaid `graph` or `flowchart` in it");
    }
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut raw = Vec::new();
    for block in &blocks {
        for line in block.lines().map(str::trim) {
            if line.is_empty()
                || line.starts_with("%%")
                || line.starts_with("graph")
                || line.starts_with("flowchart")
                || line.starts_with("subgraph")
                || line == "end"
                || line.starts_with("classDef")
                || line.starts_with("class ")
                || line.starts_with("style")
                || line.starts_with("linkStyle")
            {
                continue;
            }
            let nodes: Vec<(String, Option<String>)> =
                split_chain(line).iter().map(|n| node(n)).collect();
            for (id, label) in &nodes {
                if let Some(label) = label {
                    labels.insert(id.clone(), label.clone());
                }
            }
            for pair in nodes.windows(2) {
                raw.push((pair[0].0.clone(), pair[1].0.clone()));
            }
        }
    }
    let name = |id: &str| -> Option<String> {
        if components.contains(id) {
            return Some(id.to_string());
        }
        labels
            .get(id)
            .filter(|l| components.contains(l.as_str()))
            .cloned()
    };
    Ok(raw
        .into_iter()
        .filter_map(|(a, b)| Some((name(&a)?, name(&b)?)))
        .collect())
}

/// The bodies of the Mermaid graphs: the fenced ```` ```mermaid ```` blocks of
/// a Markdown text, or the whole text when it is a graph itself.
fn mermaid_blocks(text: &str) -> Vec<String> {
    let is_graph = |body: &str| {
        body.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("%%"))
            .is_some_and(|l| l.starts_with("graph") || l.starts_with("flowchart"))
    };
    if is_graph(text) {
        return vec![text.to_string()];
    }
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let fence = line.trim_start();
        match &mut current {
            None if fence.starts_with("```mermaid") => current = Some(String::new()),
            Some(body) if fence.starts_with("```") => {
                if is_graph(body) {
                    blocks.push(std::mem::take(body));
                }
                current = None;
            }
            Some(body) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {}
        }
    }
    blocks
}

/// The nodes of a line, split at its arrows: `a --> b -.->|x| c` gives
/// `a`, `b`, `c`. A line with no arrow is one node (a declaration).
fn split_chain(line: &str) -> Vec<String> {
    let line = line.trim_end_matches(';');
    let mut parts = Vec::new();
    let mut rest = line;
    while let Some((at, len)) = find_arrow(rest) {
        parts.push(rest[..at].trim().to_string());
        rest = &rest[at + len..];
        // An arrow's label: `-->|uses|`.
        if let Some(after) = rest.trim_start().strip_prefix('|') {
            rest = after.split_once('|').map_or("", |(_, r)| r);
        }
    }
    parts.push(rest.trim().to_string());
    parts.retain(|p| !p.is_empty());
    parts
}

/// The first arrow in `text`: its start and length. Arrows are runs of
/// `-`, `.` and `=` ending in `>`, such as `-->`, `-.->` and `==>`.
fn find_arrow(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'-' | b'=') {
            let start = i;
            while i < bytes.len() && matches!(bytes[i], b'-' | b'.' | b'=') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'>' && i - start >= 2 {
                return Some((start, i + 1 - start));
            }
        } else {
            i += 1;
        }
    }
    None
}

/// A node as written: its id and, if any, the label in brackets:
/// `web[Web UI]`, `db[(Postgres)]`, `api`.
fn node(text: &str) -> (String, Option<String>) {
    let text = text.trim();
    match text.find(['[', '(', '{', '>']) {
        Some(at) => {
            let id = text[..at].trim().to_string();
            let label = text[at..]
                .trim_matches(|c| {
                    matches!(
                        c,
                        '[' | ']' | '(' | ')' | '{' | '}' | '>' | '"' | '/' | '\\'
                    )
                })
                .trim()
                .to_string();
            (id, Some(label))
        }
        None => (text.to_string(), None),
    }
}

/// With `diagram`, every dependency between components that the diagram
/// has no arrow for, reported once per pair at its first import.
pub fn check(config: &Config, root: &Path, edges: &[Edge]) -> Result<Vec<Violation>> {
    let Some(path) = &config.diagram else {
        return Ok(Vec::new());
    };
    let text = std::fs::read_to_string(root.join(path))
        .with_context(|| format!("cannot read the diagram {}", path.display()))?;
    let components: BTreeSet<&str> = config.components.keys().map(String::as_str).collect();
    let drawn =
        arrows(&text, &components).with_context(|| format!("diagram {}", path.display()))?;
    let mut missing: BTreeMap<(&str, &str), Vec<&Edge>> = BTreeMap::new();
    for edge in edges {
        if !drawn.contains(&(edge.from.clone(), edge.to.clone())) {
            missing
                .entry((&edge.from, &edge.to))
                .or_default()
                .push(edge);
        }
    }
    Ok(missing
        .into_iter()
        .map(|((from, to), mut imports)| {
            imports.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
            let first = imports[0];
            Violation {
                rule: RuleId::Diagram,
                file: first.file.clone(),
                line: first.line,
                message: format!(
                    "{from} -> {to} is not in the diagram {} ({} import(s)); draw the arrow, or drop the dependency",
                    path.display(),
                    imports.len()
                ),
                because: None,
                from: from.to_string(),
                to: to.to_string(),
                target: String::new(),
                specifier: String::new(),
                evidence: imports.iter().map(|e| (e.file.clone(), e.line)).collect(),
            }
        })
        .collect())
}

/// The arrows of the diagram that no import stands for: pictures of
/// dependencies the code does not have.
pub fn unused_arrows(
    config: &Config,
    root: &Path,
    edges: &[Edge],
) -> Result<Vec<(String, String)>> {
    let Some(path) = &config.diagram else {
        return Ok(Vec::new());
    };
    let text = std::fs::read_to_string(root.join(path))
        .with_context(|| format!("cannot read the diagram {}", path.display()))?;
    let components: BTreeSet<&str> = config.components.keys().map(String::as_str).collect();
    let real: BTreeSet<(&str, &str)> = edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    Ok(arrows(&text, &components)?
        .into_iter()
        .filter(|(a, b)| !real.contains(&(a.as_str(), b.as_str())))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_arrows_between_components() {
        let components: BTreeSet<&str> = ["web", "service", "repository", "db"].into();
        let text = "# Architecture\n\n```mermaid\nflowchart LR\n  %% a comment\n  ui[web] --> service -.->|uses| repository\n  repository ==> pg[(db)]\n  service --> Users\n  subgraph core\n    service\n  end\n```\n\nText.\n";
        let got = arrows(text, &components).unwrap();
        let want: BTreeSet<(String, String)> = [
            ("web", "service"),
            ("service", "repository"),
            ("repository", "db"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        assert_eq!(got, want);
        assert!(arrows("no graph here", &components).is_err());
        assert_eq!(
            arrows("graph TD\n  web --> service\n", &components)
                .unwrap()
                .len(),
            1
        );
    }
}
