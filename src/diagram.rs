//! The `diagram` rule: a Mermaid graph in the documentation is the picture
//! of the architecture, and the dependencies between components have to be
//! its arrows.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::check::{Edge, RuleId, Violation};
use crate::config::Config;
use crate::policy::Policy;

/// The arrows of the Mermaid graphs in `text` (a Markdown file with
/// ```` ```mermaid ```` blocks, or a bare `.mmd` file), between the nodes
/// that name components by id or by label.
pub fn arrows(text: &str, components: &BTreeSet<&str>) -> Result<Arrows> {
    let blocks = mermaid_blocks(text);
    if blocks.is_empty() {
        bail!("no Mermaid `graph` or `flowchart` in it");
    }
    // Labels by block: an id may name different things in two graphs.
    let mut drawn = BTreeSet::new();
    for block in &blocks {
        let mut labels: BTreeMap<String, String> = BTreeMap::new();
        let mut raw = Vec::new();
        for statement in block.lines().flat_map(|l| l.split(';')).map(str::trim) {
            let keyword = statement.split_whitespace().next().unwrap_or_default();
            if statement.is_empty() || statement.starts_with("%%") || KEYWORDS.contains(&keyword) {
                continue;
            }
            let (groups, links) = parse_statement(statement);
            for (id, label) in groups.iter().flatten() {
                if let Some(label) = label {
                    labels.insert(id.clone(), label.clone());
                }
            }
            for (i, link) in links.iter().enumerate() {
                for (a, _) in &groups[i] {
                    for (b, _) in &groups[i + 1] {
                        match link {
                            Link::Forward => raw.push((a.clone(), b.clone())),
                            Link::Both => {
                                raw.push((a.clone(), b.clone()));
                                raw.push((b.clone(), a.clone()));
                            }
                            Link::Plain => {}
                        }
                    }
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
        drawn.extend(
            raw.into_iter()
                .filter_map(|(a, b)| Some((name(&a)?, name(&b)?))),
        );
    }
    Ok(drawn)
}

/// Arrows between components, from and to.
pub type Arrows = BTreeSet<(String, String)>;

/// Statements of a graph that declare no nodes or arrows.
const KEYWORDS: &[&str] = &[
    "graph",
    "flowchart",
    "subgraph",
    "end",
    "direction",
    "classDef",
    "class",
    "style",
    "linkStyle",
    "click",
    "accTitle",
    "accDescr",
];

/// How two node groups are linked: an arrow, an arrow both ways
/// (`<-->`, `o--o`), or a line with no direction (`---`, `-.-`, `~~~`).
#[derive(Debug, PartialEq)]
enum Link {
    Forward,
    Both,
    Plain,
}

/// A node as written: its id and the label it carries, if any.
type Node = (String, Option<String>);

/// A statement split at its links: the node groups (`a & b`) and the links
/// between them, one fewer.
fn parse_statement(text: &str) -> (Vec<Vec<Node>>, Vec<Link>) {
    let chars: Vec<char> = text.chars().collect();
    let mut groups = Vec::new();
    let mut links = Vec::new();
    let mut last = 0;
    let mut i = 0;
    while i < chars.len() {
        match link_at(&chars, i) {
            Some((link, after)) => {
                groups.push(group(&chars[last..i].iter().collect::<String>()));
                links.push(link);
                last = after;
                i = after;
            }
            None => i += 1,
        }
    }
    groups.push(group(&chars[last..].iter().collect::<String>()));
    // A statement with no link declares nodes only.
    if groups.iter().any(Vec::is_empty) && !links.is_empty() {
        return (Vec::new(), Vec::new());
    }
    (groups, links)
}

fn is_line(c: char) -> bool {
    matches!(c, '-' | '=' | '.' | '~')
}

/// The link that starts at `i`, if one does, and where it ends (after its
/// `|label|`): `-->`, `---`, `-.->`, `==>`, `--o`, `--x`, `<-->`, `~~~`,
/// and the ones that carry their text inside, `-- text -->`.
fn link_at(chars: &[char], i: usize) -> Option<(Link, usize)> {
    let at_word = i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '@';
    let mut j = i;
    let head = match chars[i] {
        '<' => true,
        'o' | 'x' if at_word && chars.get(i + 1).is_some_and(|c| is_line(*c)) => true,
        _ => false,
    };
    if head {
        j += 1;
    }
    let run_start = j;
    while j < chars.len() && is_line(chars[j]) {
        j += 1;
    }
    let run: String = chars[run_start..j].iter().collect();
    let dashes = run.chars().filter(|c| matches!(c, '-' | '=' | '~')).count();
    if dashes < 2 && !(run.contains('.') && dashes >= 1) {
        return None;
    }
    // Inside a node id, a single run like `web-app` never gets here, but
    // `a--b` with no spaces is still a line.
    let mut tail = chars
        .get(j)
        .filter(|c| matches!(c, '>' | 'o' | 'x'))
        .filter(|c| {
            **c == '>'
                || chars
                    .get(j + 1)
                    .is_none_or(|n| n.is_whitespace() || *n == '|')
        })
        .is_some();
    if tail {
        j += 1;
    } else if matches!(run.as_str(), "--" | "==" | "-.")
        && chars.get(j).is_some_and(|c| c.is_whitespace())
    {
        // `-- text -->`: the text runs to the link that closes it.
        let mut k = j;
        while k < chars.len() {
            if is_line(chars[k]) {
                let close_start = k;
                while k < chars.len() && is_line(chars[k]) {
                    k += 1;
                }
                if k - close_start >= 2
                    && chars.get(k).is_some_and(|c| matches!(c, '>' | 'o' | 'x'))
                {
                    j = k + 1;
                    tail = true;
                    break;
                }
            } else {
                k += 1;
            }
        }
    }
    if run.contains('~') {
        tail = false;
    }
    // An arrow's label: `-->|uses|`.
    let mut end = j;
    while end < chars.len() && chars[end].is_whitespace() {
        end += 1;
    }
    if chars.get(end) == Some(&'|') {
        end += 1;
        while end < chars.len() && chars[end] != '|' {
            end += 1;
        }
        j = (end + 1).min(chars.len());
    }
    let link = match (head, tail) {
        (true, true) => Link::Both,
        (false, true) => Link::Forward,
        _ => Link::Plain,
    };
    Some((link, j))
}

/// The nodes of a group, `a & b[Label]`, each as id and label.
fn group(text: &str) -> Vec<Node> {
    text.split('&')
        .map(|n| node(n.trim()))
        .filter(|(id, _)| !id.is_empty())
        .collect()
}

/// The bodies of the Mermaid graphs: the fenced ```` ```mermaid ```` blocks of
/// a Markdown text, or the whole text when it is a graph itself.
fn mermaid_blocks(text: &str) -> Vec<String> {
    let is_graph = |body: &str| {
        let mut lines = body.lines().map(str::trim).filter(|l| !l.is_empty());
        let mut first = lines.next();
        // Frontmatter (`---` … `---`) and `%%` directives come first.
        if first == Some("---") {
            first = lines.by_ref().skip_while(|l| *l != "---").nth(1);
        }
        while first.is_some_and(|l| l.starts_with("%%")) {
            first = lines.next();
        }
        first.is_some_and(|l| {
            let word = l
                .split(|c: char| c.is_whitespace() || c == ';')
                .next()
                .unwrap_or_default();
            word == "graph" || word == "flowchart"
        })
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

/// A node as written: its id and, if any, the label in brackets:
/// `web[Web UI]`, `db[(Postgres)]`, `api:::hot`, `e1@{ shape: rect }`.
fn node(text: &str) -> Node {
    let text = text.trim();
    // An edge id written before the link: `a e1@--> b`.
    let text = match text.rsplit_once(char::is_whitespace) {
        Some((before, id)) if id.ends_with('@') => before.trim(),
        _ => text,
    };
    let text = text.split(":::").next().unwrap_or(text);
    let text = text.split("@{").next().unwrap_or(text).trim();
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
/// has no arrow for, reported once per pair at its first import. The diagram
/// is the one the policy was loaded with.
pub fn check(policy: &Policy, edges: &[Edge]) -> Result<Vec<Violation>> {
    let config = &policy.config;
    let Some((path, drawn)) = drawn_arrows(policy)? else {
        return Ok(Vec::new());
    };
    let mut missing: BTreeMap<(&str, &str), Vec<&Edge>> = BTreeMap::new();
    for edge in edges.iter().filter(|e| drawn_needed(config, e)) {
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

/// The diagram the policy was loaded with, as the config names it, and its
/// arrows between components; `None` without `diagram`.
fn drawn_arrows(policy: &Policy) -> Result<Option<(&Path, Arrows)>> {
    let (Some(path), Some(diagram)) = (&policy.config.diagram, policy.diagram()?) else {
        return Ok(None);
    };
    let components: BTreeSet<&str> = policy
        .config
        .components
        .keys()
        .map(String::as_str)
        .collect();
    let drawn = arrows(&diagram.content, &components)
        .with_context(|| format!("diagram {}", path.display()))?;
    Ok(Some((path, drawn)))
}

/// Whether a dependency has to be drawn: not the imports of the `tests`
/// that `tests: auto` makes, and not those of `common` components, which
/// every component may use.
fn drawn_needed(config: &Config, edge: &Edge) -> bool {
    let tests = config.tests.is_some() && edge.from == crate::config::TESTS;
    !tests && !config.common.contains(&edge.to)
}

/// The arrows of the diagram that no import stands for: pictures of
/// dependencies the code does not have.
pub fn unused_arrows(policy: &Policy, edges: &[Edge]) -> Result<Vec<(String, String)>> {
    let Some((_, drawn)) = drawn_arrows(policy)? else {
        return Ok(Vec::new());
    };
    let real: BTreeSet<(&str, &str)> = edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    Ok(drawn
        .into_iter()
        .filter(|(a, b)| !real.contains(&(a.as_str(), b.as_str())))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(text: &str) -> BTreeSet<(String, String)> {
        let components: BTreeSet<&str> = [
            "web",
            "service",
            "repository",
            "db",
            "graphql",
            "styles",
            "jobs",
        ]
        .into();
        arrows(text, &components).unwrap()
    }

    fn pairs(list: &[(&str, &str)]) -> BTreeSet<(String, String)> {
        list.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn reads_the_arrows_between_components() {
        let text = "# Architecture\n\n```mermaid\nflowchart LR\n  %% a comment\n  ui[web] --> service -.->|uses| repository\n  repository ==> pg[(db)]\n  service --> Users\n  subgraph core [Core]\n    service\n  end\n```\n\nText.\n";
        assert_eq!(
            drawn(text),
            pairs(&[
                ("web", "service"),
                ("service", "repository"),
                ("repository", "db")
            ])
        );
        assert!(arrows("no graph here", &BTreeSet::new()).is_err());
    }

    #[test]
    fn reads_every_kind_of_link() {
        for (line, want) in [
            ("graphql --> service", vec![("graphql", "service")]),
            ("styles --> web", vec![("styles", "web")]),
            (
                "web --> service & jobs",
                vec![("web", "service"), ("web", "jobs")],
            ),
            (
                "web & jobs --> service",
                vec![("web", "service"), ("jobs", "service")],
            ),
            ("web -- calls --> service", vec![("web", "service")]),
            ("web -. maybe .-> service", vec![("web", "service")]),
            ("web == hard ==> service", vec![("web", "service")]),
            ("web --o service", vec![("web", "service")]),
            ("web --x service", vec![("web", "service")]),
            (
                "web <--> service",
                vec![("web", "service"), ("service", "web")],
            ),
            ("web e1@--> service", vec![("web", "service")]),
            ("web:::hot --> service", vec![("web", "service")]),
            (
                "web --> service; service --> repository",
                vec![("web", "service"), ("service", "repository")],
            ),
            ("web --- service", vec![]),
            ("web -.- service", vec![]),
            ("web ~~~ service", vec![]),
        ] {
            assert_eq!(
                drawn(&format!("graph TD\n  {line}\n")),
                pairs(&want),
                "{line}"
            );
        }
    }

    #[test]
    fn a_graph_may_open_with_frontmatter() {
        let text = "```mermaid\n---\ntitle: Shop\n---\nflowchart LR\n  web --> service\n```\n";
        assert_eq!(drawn(text), pairs(&[("web", "service")]));
    }
}
