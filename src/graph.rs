use std::collections::BTreeMap;

use crate::check::{Edge, RuleId, Violation};
use crate::config::Config;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum GraphFormat {
    Mermaid,
    Dot,
}

/// One arrow of the component graph: how many imports it stands for, and
/// whether any of them breaks a rule.
struct Arrow {
    imports: usize,
    violating: bool,
}

fn arrows(edges: &[Edge], violations: &[Violation]) -> BTreeMap<(String, String), Arrow> {
    let mut arrows: BTreeMap<(String, String), Arrow> = BTreeMap::new();
    for e in edges {
        arrows
            .entry((e.from.clone(), e.to.clone()))
            .or_insert(Arrow {
                imports: 0,
                violating: false,
            })
            .imports += 1;
    }
    for v in violations {
        if matches!(v.rule, RuleId::Dependency | RuleId::Privacy)
            && let Some(arrow) = arrows.get_mut(&(v.from.clone(), v.to.clone()))
        {
            arrow.violating = true;
        }
    }
    arrows
}

/// The component graph with its violations in red, ready for a README or a pull request.
/// With `focus`, only that component and the ones it shares an arrow with.
pub fn render(
    format: GraphFormat,
    config: &Config,
    edges: &[Edge],
    violations: &[Violation],
    focus: Option<&str>,
) -> String {
    let mut arrows = arrows(edges, violations);
    if let Some(focus) = focus {
        arrows.retain(|(from, to), _| from == focus || to == focus);
    }
    let nodes: Vec<&str> = config
        .components
        .keys()
        .map(String::as_str)
        .filter(|name| {
            focus.is_none_or(|focus| {
                *name == focus || arrows.keys().any(|(f, t)| f == name || t == name)
            })
        })
        .collect();
    match format {
        GraphFormat::Mermaid => mermaid(&nodes, &arrows),
        GraphFormat::Dot => dot(&nodes, &arrows),
    }
}

fn mermaid(nodes: &[&str], arrows: &BTreeMap<(String, String), Arrow>) -> String {
    let ids: BTreeMap<&str, String> = nodes
        .iter()
        .enumerate()
        .map(|(i, name)| (*name, format!("c{i}")))
        .collect();
    let mut out = String::from("graph LR\n");
    for (name, id) in &ids {
        out.push_str(&format!("  {id}[\"{}\"]\n", name.replace('"', "#quot;")));
    }
    let mut red = Vec::new();
    for (i, ((from, to), arrow)) in arrows.iter().enumerate() {
        let (from, to) = (&ids[from.as_str()], &ids[to.as_str()]);
        if arrow.violating {
            out.push_str(&format!("  {from} -.->|{} ✗| {to}\n", arrow.imports));
            red.push(i.to_string());
        } else {
            out.push_str(&format!("  {from} -->|{}| {to}\n", arrow.imports));
        }
    }
    if !red.is_empty() {
        out.push_str(&format!(
            "  linkStyle {} stroke:#d00,stroke-width:2px,color:#d00\n",
            red.join(",")
        ));
    }
    out
}

fn dot(nodes: &[&str], arrows: &BTreeMap<(String, String), Arrow>) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut out = String::from("digraph easement {\n  rankdir=LR;\n  node [shape=box];\n");
    for name in nodes {
        out.push_str(&format!("  {};\n", quote(name)));
    }
    for ((from, to), arrow) in arrows {
        let style = if arrow.violating {
            ", color=red, fontcolor=red, style=dashed"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {} -> {} [label=\"{}\"{style}];\n",
            quote(from),
            quote(to),
            arrow.imports
        ));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_keeps_a_component_and_its_neighbours() {
        let (config, edges, violations) = fixture();
        let dot = render(GraphFormat::Dot, &config, &edges, &violations, Some("b"));
        assert!(dot.contains("\"a\" -> \"b\""));
        assert!(!dot.contains("lone"));
        let alone = render(GraphFormat::Dot, &config, &edges, &violations, Some("lone"));
        assert!(alone.contains("\"lone\";") && !alone.contains("->"));
    }

    fn fixture() -> (Config, Vec<Edge>, Vec<Violation>) {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  a: { in: a }
  b: { in: b }
  lone: { in: lone }
",
        )
        .unwrap();
        let edge = |from: &str, to: &str, line| Edge {
            from: from.into(),
            to: to.into(),
            file: format!("{from}/x"),
            line,
        };
        let edges = vec![edge("a", "b", 1), edge("a", "b", 2), edge("b", "a", 1)];
        let violation = Violation {
            rule: RuleId::Dependency,
            file: "b/x".into(),
            line: 1,
            message: String::new(),
            because: None,
            from: "b".into(),
            to: "a".into(),
            target: String::new(),
            specifier: String::new(),
            evidence: Vec::new(),
        };
        (config, edges, vec![violation])
    }

    #[test]
    fn mermaid_marks_violations() {
        let (config, edges, violations) = fixture();
        let out = render(GraphFormat::Mermaid, &config, &edges, &violations, None);
        assert_eq!(
            out,
            "graph LR\n  c0[\"a\"]\n  c1[\"b\"]\n  c2[\"lone\"]\n  c0 -->|2| c1\n  c1 -.->|1 ✗| c0\n  linkStyle 1 stroke:#d00,stroke-width:2px,color:#d00\n"
        );
    }

    #[test]
    fn dot_marks_violations() {
        let (config, edges, violations) = fixture();
        let out = render(GraphFormat::Dot, &config, &edges, &violations, None);
        assert!(out.contains("\"a\" -> \"b\" [label=\"2\"];"));
        assert!(
            out.contains("\"b\" -> \"a\" [label=\"1\", color=red, fontcolor=red, style=dashed];")
        );
        assert!(out.contains("  \"lone\";"));
    }
}
