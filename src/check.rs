use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use pathfinding::NodeRefs;
use pathfinding::directed::bfs::{bfs, bfs_loop, bfs_reach};
use pathfinding::directed::strongly_connected_components::strongly_connected_components;
use serde::{Deserialize, Serialize};

use crate::components::ComponentMap;
use crate::config::{Config, Listed};
use crate::lang::Target;
use crate::scan::{Dep, slash};
use crate::scope::Scope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleId {
    /// An import between components that `may_depend_on` does not allow.
    Dependency,
    /// An import of a path outside the target component's public API.
    Privacy,
    /// A dependency cycle between components.
    Cycle,
    /// A component uses an external module its rule does not allow.
    External,
    /// A component exceeds a coupling limit.
    Metric,
    /// A file that belongs to no component, in strict mode.
    Orphan,
    /// An import between instances of an isolated component.
    Isolation,
    /// A directory of a component lacks an import or a file the rules require.
    Required,
    /// A file a component's rules forbid.
    File,
    /// A use of a name the rules forbid or the target does not make public.
    Symbol,
    /// An inline suppression that matches no violation.
    UnusedSuppression,
    /// An inline suppression without a reason.
    SuppressionReason,
    /// An `in` pattern of a component that matches no file of the project.
    Unmatched,
    /// An import of a file that `exclude` hides.
    Excluded,
    /// A chain of imports from a component to one its `must_not_reach` names.
    Reach,
    /// A file of a component that no chain of imports from an `entry` reaches.
    Unused,
    /// A type whose name a component's `defines` claims, defined elsewhere.
    Naming,
    /// A dependency between components that the `diagram` has no arrow for.
    Diagram,
    /// An external module the nearest manifest does not declare.
    Undeclared,
}

impl RuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleId::Dependency => "dependency",
            RuleId::Privacy => "privacy",
            RuleId::Cycle => "cycle",
            RuleId::External => "external",
            RuleId::Metric => "metric",
            RuleId::Orphan => "orphan",
            RuleId::Isolation => "isolation",
            RuleId::Required => "required",
            RuleId::File => "file",
            RuleId::Symbol => "symbol",
            RuleId::UnusedSuppression => "unused-suppression",
            RuleId::SuppressionReason => "suppression-reason",
            RuleId::Unmatched => "unmatched",
            RuleId::Excluded => "excluded",
            RuleId::Reach => "reach",
            RuleId::Unused => "unused",
            RuleId::Naming => "naming",
            RuleId::Diagram => "diagram",
            RuleId::Undeclared => "undeclared",
        }
    }

    /// Rules about the config rather than the code: no import or changed line
    /// stands behind them, so `--diff` and `--scope` keep them, and the
    /// baseline never accepts them.
    pub fn is_config_rule(self) -> bool {
        self == RuleId::Unmatched
    }

    /// Every rule, in the order of the declaration.
    pub const ALL: [RuleId; 19] = [
        RuleId::Dependency,
        RuleId::Privacy,
        RuleId::Cycle,
        RuleId::External,
        RuleId::Metric,
        RuleId::Orphan,
        RuleId::Isolation,
        RuleId::Required,
        RuleId::File,
        RuleId::Symbol,
        RuleId::UnusedSuppression,
        RuleId::SuppressionReason,
        RuleId::Unmatched,
        RuleId::Excluded,
        RuleId::Reach,
        RuleId::Unused,
        RuleId::Naming,
        RuleId::Diagram,
        RuleId::Undeclared,
    ];

    /// Rules that an inline comment can suppress.
    pub const SUPPRESSIBLE: [RuleId; 8] = [
        RuleId::Dependency,
        RuleId::Privacy,
        RuleId::External,
        RuleId::Isolation,
        RuleId::Symbol,
        RuleId::Excluded,
        RuleId::Naming,
        RuleId::Undeclared,
    ];

    pub fn suppressible(s: &str) -> Option<Self> {
        Self::SUPPRESSIBLE.into_iter().find(|r| r.as_str() == s)
    }
}

fn default_rule() -> RuleId {
    RuleId::Dependency
}

#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub rule: RuleId,
    pub file: String,
    pub line: usize,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
    pub from: String,
    pub to: String,
    pub target: String,
    pub specifier: String,
    /// Every import behind the violation; diff mode keeps it if any of them changed.
    #[serde(skip)]
    pub evidence: Vec<(String, usize)>,
}

/// Identity of a violation for the baseline: line numbers are left out so that
/// unrelated edits do not invalidate it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    #[serde(default = "default_rule")]
    pub rule: RuleId,
    pub file: String,
    pub target: String,
    pub from: String,
    pub to: String,
}

impl Violation {
    pub fn key(&self) -> Key {
        Key {
            rule: self.rule,
            file: self.file.clone(),
            target: self.target.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
        }
        .canonical()
    }
}

impl Key {
    /// The key without the file a violation happens to be reported at, where
    /// the rule is about something else: a cycle, a limit and a reach are
    /// about components, a requirement about a directory. Another file that sorts
    /// first then does not turn old debt into new.
    pub fn canonical(mut self) -> Self {
        match self.rule {
            RuleId::Cycle | RuleId::Metric | RuleId::Diagram => self.file.clear(),
            // Which file of the reached component the chain ends in changes as
            // freely as where it starts.
            RuleId::Reach => {
                self.file.clear();
                self.target.clear();
            }
            // The directory ends in `/`, so a canonical key stays as it is.
            RuleId::Required if !self.file.ends_with('/') => {
                self.file = self
                    .file
                    .rsplit_once('/')
                    .map_or_else(|| "./".to_string(), |(dir, _)| format!("{dir}/"));
            }
            _ => {}
        }
        self
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub files: usize,
    pub internal_deps: usize,
    pub external_deps: usize,
    pub unmapped_files: usize,
    pub suppressed: usize,
    /// Indexers that ran, and indexes taken from the cache instead.
    #[serde(skip_serializing_if = "is_zero")]
    pub indexed: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub from_cache: usize,
    /// File extensions whose standard library `$std` cannot tell, among the
    /// files of components whose rules name it: their imports were judged by
    /// name only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub std_unknown: Vec<String>,
    /// Extensions of checked component files whose types `defines` cannot
    /// see: they are read with tree-sitter, which reports no definitions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub naming_unseen: Vec<String>,
    /// Why `manifests` left some imports unchecked.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub manifests_partial: Vec<String>,
    /// Why `unused` took some files as used without judging them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unused_partial: Vec<String>,
    /// Source files of an indexed language that its indexer did not cover.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered: Vec<String>,
    /// `--component` and `--scope` of the run, as `component web`, `path src/api`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub limited_to: Vec<String>,
    /// In diff mode: `a -> b` for component dependencies that exist only
    /// because of the changed lines.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub new_dependencies: Vec<String>,
}

/// A name used through an import, with the import resolved.
#[derive(Debug, Clone)]
pub struct SymbolRef {
    pub file: std::path::PathBuf,
    pub line: usize,
    pub specifier: String,
    pub name: String,
    /// The method of `name` that is called, with what `forbid_symbol` names it
    /// through: `Shop::Db::Client` and `query`.
    pub method: Option<(String, String)>,
    /// None for a name of the project that no one file defines (a Ruby module
    /// reopened in each file under it): no component owns it.
    pub target: Option<Target>,
}

/// An import between two different components.
#[derive(Debug, Clone)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub file: String,
    pub line: usize,
}

/// Checks every internal dependency against the dependency and privacy rules.
/// Every violating import is returned, one per line, see [`dedupe`]. The edges
/// between components feed [`cycles`].
pub fn evaluate(
    config: &Config,
    map: &ComponentMap,
    scope: &Scope,
    deps: &[Dep],
) -> (Vec<Violation>, Vec<Edge>, Stats) {
    let mut stats = Stats::default();
    let mut found = Vec::new();
    let mut edges = Vec::new();
    let mut unmapped = BTreeSet::new();
    for dep in deps {
        let file = slash(&dep.file);
        // Violations and edges come from the whole project; the counters follow the scope.
        let counted = scope.contains(map, &file);
        let target = match &dep.target {
            Target::Internal(path) => slash(path),
            Target::External(module) => {
                stats.external_deps += usize::from(counted);
                if let Some(from) = map.component_of(&file)
                    && counted
                    && map.uses_std(from)
                    && !crate::stdlib::knows(&file)
                {
                    let ext = file.rsplit_once('.').map_or("", |(_, e)| e).to_string();
                    if !stats.std_unknown.contains(&ext) {
                        stats.std_unknown.push(ext);
                    }
                }
                if let Some(from) = map.component_of(&file)
                    && let Some(problem) = map.external_problem(from, &file, module)
                {
                    found.push(Violation {
                        rule: RuleId::External,
                        evidence: vec![(file.clone(), dep.line)],
                        file,
                        line: dep.line,
                        message: format!("{from} -> external {module} {}", problem.what),
                        because: problem
                            .because
                            .or_else(|| config.reason(from))
                            .map(str::to_string),
                        from: from.to_string(),
                        to: String::new(),
                        target: module.clone(),
                        specifier: dep.specifier.clone(),
                    });
                }
                continue;
            }
        };
        stats.internal_deps += usize::from(counted);
        let Some(from) = map.component_of(&file) else {
            if counted {
                unmapped.insert(file);
            }
            continue;
        };
        if map.is_excluded(&target) {
            found.push(Violation {
                rule: RuleId::Excluded,
                evidence: vec![(file.clone(), dep.line)],
                file,
                line: dep.line,
                message: format!(
                    "{from} imports {target}, which `exclude` hides, so no rule checks it; list it in `target_only` if it is generated, else do not import it"
                ),
                because: None,
                from: from.to_string(),
                to: String::new(),
                target,
                specifier: dep.specifier.clone(),
            });
            continue;
        }
        let Some(to) = map.component_of(&target) else {
            continue;
        };
        if from == to {
            if let (Some(here), Some(there)) = (map.instance_of(&file), map.instance_of(&target))
                && here != there
            {
                found.push(Violation {
                    rule: RuleId::Isolation,
                    evidence: vec![(file.clone(), dep.line)],
                    file,
                    line: dep.line,
                    message: format!(
                        "{here} -> {there}: instances of {from} must not import each other (imports {})",
                        dep.specifier
                    ),
                    because: config.components[from]
                        .because
                        .clone()
                        .or_else(|| config.reason(from).map(str::to_string)),
                    from: from.to_string(),
                    to: to.to_string(),
                    target,
                    specifier: dep.specifier.clone(),
                });
            } else if let Some(v) = slice_violation(config, map, from, to, &file, &target, dep) {
                found.push(v);
            }
            continue;
        }
        edges.push(Edge {
            from: from.to_string(),
            to: to.to_string(),
            file: file.clone(),
            line: dep.line,
        });
        let importers = config.components[to].allow_importers.as_deref();
        let (rule, message, because) = if !config.allows(from, to) {
            let how = if config.forbids(from, to) {
                "is forbidden"
            } else {
                "is not allowed"
            };
            let layer = config
                .layer_block(from, to)
                .filter(|_| !config.forbids(from, to))
                .map(|why| format!(": {why}"))
                .unwrap_or_default();
            (
                RuleId::Dependency,
                format!("{from} -> {to} {how}{layer} (imports {})", dep.specifier),
                config
                    .forbid_reason(from, to)
                    .or_else(|| config.reason(from)),
            )
        } else if importers.is_some_and(|list| !list.iter().any(|c| c == from)) {
            (
                RuleId::Dependency,
                format!(
                    "{from} -> {to}: {to} may be imported only from {} (imports {})",
                    importers.unwrap_or_default().join(", "),
                    dep.specifier
                ),
                config.components[to].because.as_deref(),
            )
        } else if !map.is_public(to, &target) {
            (
                RuleId::Privacy,
                format!(
                    "{from} -> {to}: {target} is not in the public API of {to} (imports {})",
                    dep.specifier
                ),
                config.components[to].because.as_deref(),
            )
        } else {
            if let Some(v) = slice_violation(config, map, from, to, &file, &target, dep) {
                found.push(v);
            }
            continue;
        };
        found.push(Violation {
            rule,
            evidence: vec![(file.clone(), dep.line)],
            file,
            line: dep.line,
            message,
            because: because.map(str::to_string),
            from: from.to_string(),
            to: to.to_string(),
            target,
            specifier: dep.specifier.clone(),
        });
    }
    stats.unmapped_files = unmapped.len();
    (found, edges, stats)
}

/// An import between two slices: both ends name the same placeholder in
/// their `in` patterns (`src/features/<feature>/ui`) and fill it differently.
fn slice_violation(
    config: &Config,
    map: &ComponentMap,
    from: &str,
    to: &str,
    file: &str,
    target: &str,
    dep: &Dep,
) -> Option<Violation> {
    let theirs = map.captures_of(target);
    let (name, here, there) = map.captures_of(file).into_iter().find_map(|(name, here)| {
        let (_, there) = theirs.iter().find(|(n, _)| *n == name)?;
        (here != *there).then(|| (name, here, there.clone()))
    })?;
    Some(Violation {
        rule: RuleId::Isolation,
        evidence: vec![(file.to_string(), dep.line)],
        file: file.to_string(),
        line: dep.line,
        message: format!("{from} -> {to}: <{name}> differs ({here} imports {there}: {target})"),
        because: config.components[to]
            .because
            .clone()
            .or_else(|| config.reason(from).map(str::to_string)),
        from: from.to_string(),
        to: to.to_string(),
        target: target.to_string(),
        specifier: dep.specifier.clone(),
    })
}

/// Component dependencies, as `a -> b`, whose every import sits on a line
/// that `changed` accepts: the edges that did not exist before the change.
pub fn new_dependencies(edges: &[Edge], changed: impl Fn(&str, usize) -> bool) -> Vec<String> {
    let mut all_changed: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for e in edges {
        let entry = all_changed.entry((&e.from, &e.to)).or_insert(true);
        *entry &= changed(&e.file, e.line);
    }
    all_changed
        .into_iter()
        .filter(|(_, new)| *new)
        .map(|((from, to), _)| format!("{from} -> {to}"))
        .collect()
}

/// Dependency cycles between the components covered by `no_cycles`: one
/// violation per strongly connected group, with the shortest cycle through it.
pub fn cycles(config: &Config, edges: &[Edge]) -> Vec<Violation> {
    let covered: Vec<&Edge> = edges
        .iter()
        .filter(|e| config.no_cycles.covers(&e.from) && config.no_cycles.covers(&e.to))
        .collect();
    let mut graph: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in &covered {
        graph.entry(&e.from).or_default().insert(&e.to);
        graph.entry(&e.to).or_default();
    }
    let nodes: Vec<&str> = graph.keys().copied().collect();
    let mut groups: Vec<BTreeSet<&str>> =
        strongly_connected_components(&nodes, |v| graph[v].iter().copied())
            .into_iter()
            .filter(|g| g.len() > 1)
            .map(|g| g.into_iter().collect())
            .collect();
    groups.sort();
    groups
        .into_iter()
        .map(|members| {
            let start = *members.first().expect("non-empty group");
            let cycle = shortest_cycle(&graph, &members, start);
            let first = covered
                .iter()
                .filter(|e| e.from == cycle[0] && e.to == cycle[1])
                .min_by(|x, y| (&x.file, x.line).cmp(&(&y.file, y.line)))
                .expect("cycle edge exists");
            let path = cycle.join(" -> ");
            let names: Vec<&str> = members.iter().copied().collect();
            let message = if members.len() == cycle.len() - 1 {
                format!("component dependency cycle: {path}")
            } else {
                format!(
                    "component dependency cycle among {} components: {} (shortest: {path})",
                    members.len(),
                    names.join(", ")
                )
            };
            let mut evidence: Vec<(String, usize)> = covered
                .iter()
                .filter(|e| members.contains(e.from.as_str()) && members.contains(e.to.as_str()))
                .map(|e| (e.file.clone(), e.line))
                .collect();
            evidence.sort();
            Violation {
                rule: RuleId::Cycle,
                file: first.file.clone(),
                line: first.line,
                message,
                because: None,
                // The key of a cycle is its members only, so moving the evidence does not churn the baseline.
                from: names.join(","),
                to: String::new(),
                target: String::new(),
                specifier: String::new(),
                evidence,
            }
        })
        .collect()
}

/// For each component and each one its `must_not_reach` names: the shortest
/// chain of imports between them through files of neither, reported at its
/// first import. Files of no component count as hops; an import straight
/// into the forbidden component is a `dependency` violation instead.
pub fn reaches(
    config: &Config,
    map: &ComponentMap,
    deps: &[Dep],
    files: &[String],
) -> Vec<Violation> {
    let pairs: Vec<(&str, &Listed)> = config
        .rules
        .iter()
        .flat_map(|(from, rule)| {
            rule.must_not_reach
                .iter()
                .map(move |to| (from.as_str(), to))
        })
        .collect();
    if pairs.is_empty() {
        return Vec::new();
    }
    let graph = file_graph(deps, files);
    let component: BTreeMap<&str, &str> = graph
        .iter()
        .flat_map(|(f, out)| std::iter::once(f.as_str()).chain(out.iter().map(|(t, _)| t.as_str())))
        .filter_map(|f| map.component_of(f).map(|c| (f, c)))
        .collect();
    let mut found = Vec::new();
    for (from, to) in pairs {
        let to_name = to.name();
        let of = |file: &str| component.get(file).copied();
        let starts = graph
            .keys()
            .map(String::as_str)
            .filter(|f| of(f) == Some(from));
        let is_hop = |file: &str| !matches!(of(file), Some(c) if c == from || c == to_name);
        let Some((hops, last)) =
            shortest_chain(&graph, starts, is_hop, |file| of(file) == Some(to_name))
        else {
            continue;
        };
        let (through, chain) = describe_chain(&hops, last, of);
        let (file, line) = hops[0].clone();
        found.push(Violation {
            rule: RuleId::Reach,
            file,
            line,
            message: format!("{from} reaches {to_name} through {through}: {chain}"),
            because: to
                .because()
                .or_else(|| config.reason(from))
                .map(str::to_string),
            from: from.to_string(),
            to: to_name.to_string(),
            target: last.to_string(),
            specifier: String::new(),
            evidence: hops,
        });
    }
    found
}

/// What a file of no component is called in reports.
pub const NO_COMPONENT: &str = "no component";

/// A chain from [`shortest_chain`] for a reader: the components of its hops,
/// `util, no component`, and the chain itself, `a.kt:2 -> b.kt:3 -> c.kt`.
pub fn describe_chain<'a>(
    hops: &[(String, usize)],
    last: &str,
    of: impl Fn(&str) -> Option<&'a str>,
) -> (String, String) {
    let through: BTreeSet<&str> = hops[1..]
        .iter()
        .map(|(f, _)| of(f).unwrap_or(NO_COMPONENT))
        .collect();
    let chain: Vec<String> = hops
        .iter()
        .map(|(f, l)| format!("{f}:{l}"))
        .chain(std::iter::once(last.to_string()))
        .collect();
    (
        through.into_iter().collect::<Vec<_>>().join(", "),
        chain.join(" -> "),
    )
}

/// Every file -> the files it imports, with the line of the import. An
/// import of a package directory is one of each file directly in it.
pub fn file_graph(deps: &[Dep], files: &[String]) -> BTreeMap<String, BTreeSet<(String, usize)>> {
    let mut in_dir: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for file in files {
        let dir = file.rsplit_once('/').map_or("", |(dir, _)| dir);
        in_dir.entry(dir).or_default().push(file);
    }
    let known: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let mut graph: BTreeMap<String, BTreeSet<(String, usize)>> = BTreeMap::new();
    for dep in deps {
        let Target::Internal(path) = &dep.target else {
            continue;
        };
        let to = slash(path);
        let targets = if known.contains(to.as_str()) {
            vec![to.as_str()]
        } else {
            in_dir.get(to.as_str()).cloned().unwrap_or_default()
        };
        let out = graph.entry(slash(&dep.file)).or_default();
        for target in targets {
            out.insert((target.to_string(), dep.line));
        }
    }
    graph
}

/// Every file a chain of `successors` reaches from any of `roots`, the roots
/// included.
pub fn reach<'a, I: IntoIterator<Item = &'a str>>(
    roots: impl IntoIterator<Item = &'a str>,
    mut successors: impl FnMut(&'a str) -> I,
) -> BTreeSet<&'a str> {
    // `None` stands before every root, so one search starts from all of them.
    let roots: Vec<Option<&str>> = roots.into_iter().map(Some).collect();
    bfs_reach(None, |node: &Option<&'a str>| match node {
        None => roots.clone(),
        Some(file) => successors(file).into_iter().map(Some).collect(),
    })
    .flatten()
    .collect()
}

/// The shortest chain from any of `starts` through files `is_hop` accepts to
/// a file `is_end` accepts: each hop with the line of its import, and the
/// file it ends in. Every start is searched at once, so no start's longer
/// chain hides another's shorter one.
pub fn shortest_chain<'a>(
    graph: &'a BTreeMap<String, BTreeSet<(String, usize)>>,
    starts: impl Iterator<Item = &'a str>,
    is_hop: impl Fn(&str) -> bool,
    is_end: impl Fn(&str) -> bool,
) -> Option<(Vec<(String, usize)>, &'a str)> {
    /// A file the chain passes through, or the one it ends in.
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum Step<'a> {
        Hop(&'a str),
        End(&'a str),
    }
    let starts: BTreeSet<&str> = starts.collect();
    let first: Vec<Step> = starts.iter().map(|file| Step::Hop(file)).collect();
    let path = bfs(
        first.iter().collect::<NodeRefs<_>>(),
        |step| {
            let Step::Hop(file) = *step else {
                return Vec::new();
            };
            graph
                .get(file)
                .into_iter()
                .flatten()
                .filter_map(|(next, _)| {
                    let next = next.as_str();
                    // From a start, an import of the end is direct, not a chain.
                    if is_end(next) && !starts.contains(file) {
                        Some(Step::End(next))
                    } else {
                        is_hop(next).then_some(Step::Hop(next))
                    }
                })
                .collect()
        },
        |step| matches!(step, Step::End(_)),
    )?;
    let files: Vec<&str> = path
        .iter()
        .map(|step| match *step {
            Step::Hop(file) | Step::End(file) => file,
        })
        .collect();
    let hops = files
        .windows(2)
        .map(|pair| {
            let line = graph[pair[0]]
                .iter()
                .find(|(to, _)| to == pair[1])
                .map(|(_, line)| *line)
                .expect("a step of the chain is an import");
            (pair[0].to_string(), line)
        })
        .collect();
    Some((hops, *files.last().expect("a chain ends in a file")))
}

/// Shortest path from `start` back to itself inside `members`, breadth-first:
/// `[start, …, start]`.
fn shortest_cycle<'a>(
    graph: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    members: &BTreeSet<&'a str>,
    start: &'a str,
) -> Vec<&'a str> {
    bfs_loop(&start, |v| {
        graph[v].iter().copied().filter(|w| members.contains(w))
    })
    .expect("a strongly connected group has a cycle through every member")
}

/// Coupling limits: `max_fan_out` counts the components a component depends
/// on, `max_instability` is `out / (in + out)` over the same component edges.
pub fn metrics(config: &Config, edges: &[Edge]) -> Vec<Violation> {
    let mut out: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut incoming: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in edges {
        out.entry(&e.from).or_default().insert(&e.to);
        incoming.entry(&e.to).or_default().insert(&e.from);
    }
    let mut found = Vec::new();
    for (name, rule) in &config.rules {
        let Some(targets) = out.get(name.as_str()) else {
            continue;
        };
        let fan_out = targets.len();
        let fan_in = incoming.get(name.as_str()).map_or(0, BTreeSet::len);
        let instability = fan_out as f64 / (fan_in + fan_out) as f64;
        let mut evidence: Vec<(String, usize)> = edges
            .iter()
            .filter(|e| e.from == *name)
            .map(|e| (e.file.clone(), e.line))
            .collect();
        evidence.sort();
        let (file, line) = evidence[0].clone();
        let list = targets.iter().copied().collect::<Vec<_>>().join(", ");
        let mut over = |limit: &str, message: String| {
            found.push(Violation {
                rule: RuleId::Metric,
                file: file.clone(),
                line,
                message,
                because: rule.because.clone(),
                from: name.clone(),
                to: String::new(),
                target: limit.to_string(),
                specifier: String::new(),
                evidence: evidence.clone(),
            });
        };
        if let Some(max) = rule.max_fan_out
            && fan_out > max
        {
            over(
                "max_fan_out",
                format!(
                    "{name} depends on {fan_out} components, more than max_fan_out {max}: {list}"
                ),
            );
        }
        if let Some(max) = rule.max_instability
            && instability > max
        {
            over(
                "max_instability",
                format!(
                    "{name} has instability {instability:.2} ({fan_out} out, {fan_in} in), above max_instability {max}"
                ),
            );
        }
    }
    found
}

/// What a directory of a component imports and holds.
#[derive(Default)]
struct Unit {
    /// Checked files, sorted; the first one carries the violations.
    files: BTreeSet<String>,
    components: BTreeSet<String>,
    external: BTreeSet<String>,
}

/// `must_depend_on`, `must_use_external`, `requires_file` and `requires_directive`: every directory
/// that holds checked files of a component must import what the rule lists and
/// contain the files it names. One violation per directory and missing item.
pub fn required(
    root: &Path,
    config: &Config,
    map: &ComponentMap,
    deps: &[Dep],
    paths: &[String],
    all_files: &[String],
) -> Vec<Violation> {
    let parent = |file: &str| file.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();
    let wanted = |from: &str| {
        config.rules.get(from).is_some_and(|r| {
            !(r.must_depend_on.is_empty()
                && r.must_use_external.is_empty()
                && r.requires_file.is_empty()
                && r.requires_directive.is_empty())
        })
    };
    let mut units: BTreeMap<(String, String), Unit> = BTreeMap::new();
    for file in paths {
        if let Some(component) = map.component_of(file).filter(|c| wanted(c)) {
            units
                .entry((component.to_string(), parent(file)))
                .or_default()
                .files
                .insert(file.clone());
        }
    }
    for dep in deps {
        let file = slash(&dep.file);
        let Some(component) = map.component_of(&file) else {
            continue;
        };
        let Some(unit) = units.get_mut(&(component.to_string(), parent(&file))) else {
            continue;
        };
        match &dep.target {
            Target::Internal(target) => {
                if let Some(to) = map.component_of(&slash(target)) {
                    unit.components.insert(to.to_string());
                }
            }
            Target::External(module) => {
                unit.external.insert(module.clone());
            }
        }
    }
    let mut dir_files: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for file in all_files {
        let (dir, name) = file.rsplit_once('/').unwrap_or(("", file));
        dir_files.entry(dir).or_default().push(name);
    }
    let mut found = Vec::new();
    for ((component, dir), unit) in &units {
        let rule = &config.rules[component];
        let first = unit.files.first().expect("a unit has files").clone();
        let shown = if dir.is_empty() { "." } else { dir };
        let mut missing = |target: String, message: String, wanted: &Listed| {
            found.push(Violation {
                rule: RuleId::Required,
                file: first.clone(),
                line: 1,
                message,
                because: wanted
                    .because()
                    .or(rule.because.as_deref())
                    .map(str::to_string),
                from: component.clone(),
                to: String::new(),
                target,
                specifier: String::new(),
                evidence: vec![(first.clone(), 1)],
            });
        };
        for wanted in &rule.must_depend_on {
            if !unit.components.contains(wanted.name()) {
                missing(
                    format!("import {}", wanted.name()),
                    format!(
                        "{shown} ({component}) must import {}, and no file in it does",
                        wanted.name()
                    ),
                    wanted,
                );
            }
        }
        for wanted in &rule.must_use_external {
            let used = ComponentMap::external_matches(wanted.name(), unit.external.iter());
            if !used {
                missing(
                    format!("external {}", wanted.name()),
                    format!(
                        "{shown} ({component}) must use the external module {}, and no file in it does",
                        wanted.name()
                    ),
                    wanted,
                );
            }
        }
        for wanted in &rule.requires_file {
            let names = dir_files.get(dir.as_str()).map_or(&[][..], Vec::as_slice);
            let present = globset::Glob::new(wanted.name())
                .map(|g| g.compile_matcher())
                .is_ok_and(|m| names.iter().any(|n| m.is_match(n)));
            if !present {
                missing(
                    format!("file {}", wanted.name()),
                    format!(
                        "{shown} ({component}) must contain a file {}",
                        wanted.name()
                    ),
                    wanted,
                );
            }
        }
        for wanted in &rule.requires_directive {
            let Ok(pattern) = regex::Regex::new(wanted.name()) else {
                continue;
            };
            let present = unit.files.iter().any(|file| {
                std::fs::read_to_string(root.join(file)).is_ok_and(|text| pattern.is_match(&text))
            });
            if !present {
                missing(
                    format!("directive {}", wanted.name()),
                    format!(
                        "{shown} ({component}) must contain a comment or text matching `{}`, and no file in it does",
                        wanted.name()
                    ),
                    wanted,
                );
            }
        }
    }
    found
}

/// `forbid_symbol` and `public_symbols`: names used through imports.
pub fn symbols(config: &Config, map: &ComponentMap, uses: &[SymbolRef]) -> Vec<Violation> {
    let mut found = Vec::new();
    for used in uses {
        let file = slash(&used.file);
        let Some(from) = map.component_of(&file) else {
            continue;
        };
        let mut report = |message: String, target: String, to: &str, because: Option<&str>| {
            found.push(Violation {
                rule: RuleId::Symbol,
                evidence: vec![(file.clone(), used.line)],
                file: file.clone(),
                line: used.line,
                message,
                because: because.map(str::to_string),
                from: from.to_string(),
                to: to.to_string(),
                target,
                specifier: used.specifier.clone(),
            });
        };
        if let Some(rule) = config.rules.get(from) {
            for listed in &rule.forbid_symbol {
                let pattern = listed.name();
                if names_symbol(pattern, &used.specifier, &used.name)
                    || used
                        .method
                        .as_ref()
                        .is_some_and(|(owner, method)| names_symbol(pattern, owner, method))
                {
                    report(
                        format!("{from} must not use {}", listed.name()),
                        listed.name().to_string(),
                        "",
                        listed.because().or(rule.because.as_deref()),
                    );
                }
            }
        }
        if let Some(Target::Internal(path)) = &used.target
            && let Some(to) = map.component_of(&slash(path))
            && to != from
            && let Some(public) = &config.components[to].public_symbols
            && !public.contains(&used.name)
        {
            report(
                format!(
                    "{from} -> {to}: {} is not in the public symbols of {to} ({})",
                    used.name,
                    public.join(", ")
                ),
                format!("{}.{}", used.specifier, used.name),
                to,
                config.components[to].because.as_deref(),
            );
        }
    }
    found
}

/// Whether `pattern` (`pkg.Name` or `import/path.Name`) names the symbol
/// `name` used through the import `specifier`.
fn names_symbol(pattern: &str, specifier: &str, name: &str) -> bool {
    let Some((qualifier, wanted)) = pattern.rsplit_once('.') else {
        return false;
    };
    wanted == name
        && (qualifier == specifier
            || specifier
                .strip_suffix(qualifier)
                .is_some_and(|rest| rest.ends_with('/')))
}

/// `forbid_file`: files of a component whose name or path matches a forbidden glob.
pub fn forbidden_files(config: &Config, map: &ComponentMap, files: &[String]) -> Vec<Violation> {
    let mut found = Vec::new();
    for (component, rule) in &config.rules {
        if rule.forbid_file.is_empty() {
            continue;
        }
        let matchers: Vec<(&Listed, globset::GlobMatcher)> = rule
            .forbid_file
            .iter()
            .filter_map(|l| Some((l, globset::Glob::new(l.name()).ok()?.compile_matcher())))
            .collect();
        for file in files {
            if map.component_of(file) != Some(component.as_str()) {
                continue;
            }
            let name = file.rsplit_once('/').map_or(file.as_str(), |(_, n)| n);
            let Some((listed, _)) = matchers
                .iter()
                .find(|(_, m)| m.is_match(name) || m.is_match(file))
            else {
                continue;
            };
            found.push(Violation {
                rule: RuleId::File,
                file: file.clone(),
                line: 1,
                message: format!(
                    "{component} must not contain {file} (forbid_file {})",
                    listed.name()
                ),
                because: listed
                    .because()
                    .or(rule.because.as_deref())
                    .map(str::to_string),
                from: component.clone(),
                to: String::new(),
                target: file.clone(),
                specifier: String::new(),
                evidence: vec![(file.clone(), 1)],
            });
        }
    }
    found
}

/// In strict mode, every checked file must belong to a component.
pub fn orphans(config: &Config, map: &ComponentMap, files: &[String]) -> Vec<Violation> {
    if !config.strict {
        return Vec::new();
    }
    files
        .iter()
        .filter(|f| map.component_of(f).is_none() && !map.is_unowned(f))
        .map(|file| Violation {
            rule: RuleId::Orphan,
            file: file.clone(),
            line: 1,
            message: "file belongs to no component (strict mode)".to_string(),
            because: None,
            from: String::new(),
            to: String::new(),
            target: file.clone(),
            specifier: String::new(),
            evidence: vec![(file.clone(), 1)],
        })
        .collect()
}

/// One violation per `in` pattern that matches no file in `files`, at its
/// line in the config text, or line 1 when it comes from `extends`. The
/// `tests` component of `tests: auto` and components marked `optional` may
/// match nothing.
pub fn unmatched(
    config: &Config,
    map: &ComponentMap,
    files: &[String],
    config_file: &str,
    config_text: &str,
) -> Vec<Violation> {
    map.unmatched_patterns(files)
        .into_iter()
        .filter(|(component, _)| !config.components[*component].optional)
        .map(|(component, pattern)| Violation {
            rule: RuleId::Unmatched,
            file: config_file.to_string(),
            line: config_line(config_text, component, pattern),
            message: format!(
                "component `{component}` matches no file (in: {pattern}); fix the pattern, or set `optional: true` if the component may be empty (files that `exclude` or .gitignore hide do not count)"
            ),
            because: None,
            from: component.to_string(),
            to: String::new(),
            target: pattern.to_string(),
            specifier: String::new(),
            evidence: Vec::new(),
        })
        .chain(
            crate::unused::unmatched_entries(config, files)
                .unwrap_or_default()
                .into_iter()
                .map(|pattern| Violation {
                    rule: RuleId::Unmatched,
                    file: config_file.to_string(),
                    line: entry_line(config_text, pattern),
                    message: format!(
                        "`entry` pattern `{pattern}` matches no file, so everything it would reach looks unused; fix the pattern or drop it"
                    ),
                    because: None,
                    from: String::new(),
                    to: String::new(),
                    target: pattern.to_string(),
                    specifier: String::new(),
                    evidence: Vec::new(),
                }),
        )
        .collect()
}

/// The line, counted from 1, of `pattern` under `entry:`, else 1.
fn entry_line(text: &str, pattern: &str) -> usize {
    let lines: Vec<&str> = text.lines().map(without_comment).collect();
    lines
        .iter()
        .position(|l| l.trim_start().starts_with("entry:"))
        .and_then(|start| {
            lines[start..]
                .iter()
                .position(|l| holds(l, pattern))
                .map(|i| start + i + 1)
        })
        .unwrap_or(1)
}

/// The line, counted from 1, where `component` lists `pattern` under
/// `components:` in the config text; 1 when the text does not declare it
/// (it comes from `extends`).
fn config_line(text: &str, component: &str, pattern: &str) -> usize {
    let lines: Vec<&str> = text.lines().map(without_comment).collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let Some(section) = lines.iter().position(|l| l.trim_end() == "components:") else {
        return 1;
    };
    let body: Vec<(usize, &str)> = lines
        .iter()
        .copied()
        .enumerate()
        .skip(section + 1)
        .filter(|(_, l)| !l.trim().is_empty())
        .take_while(|(_, l)| indent(l) > 0)
        .collect();
    let Some(level) = body.first().map(|(_, l)| indent(l)) else {
        return 1;
    };
    let names = |l: &str| {
        l.split_once(':')
            .is_some_and(|(key, _)| key.trim().trim_matches(['"', '\'']) == component)
    };
    let Some(at) = body
        .iter()
        .position(|(_, l)| indent(l) == level && names(l))
    else {
        return 1;
    };
    body[at..]
        .iter()
        .take_while(|(i, l)| *i == body[at].0 || indent(l) > level)
        .find(|(_, l)| holds(l, pattern))
        .map_or(1, |(i, _)| i + 1)
}

/// The line without a `#` comment.
fn without_comment(line: &str) -> &str {
    line.match_indices('#')
        .find(|(i, _)| *i == 0 || line[..*i].ends_with(char::is_whitespace))
        .map_or(line, |(i, _)| &line[..i])
}

/// Whether `line` holds `pattern` as a whole, not as part of a longer path.
fn holds(line: &str, pattern: &str) -> bool {
    let in_path = |c: char| c.is_alphanumeric() || "/_-.*".contains(c);
    line.match_indices(pattern).any(|(i, _)| {
        let rest = &line[i + pattern.len()..];
        // The config may write `src/a/` for `src/a`.
        let rest = rest
            .strip_prefix('/')
            .filter(|r| !r.starts_with(in_path))
            .unwrap_or(rest);
        !line[..i].ends_with(in_path) && !rest.starts_with(in_path)
    })
}

/// Every type of a component whose name the `defines` of other components
/// claim, and not its own: `OrderRepository` in `web` when `repository`
/// defines `*Repository`.
pub fn naming(
    config: &Config,
    map: &ComponentMap,
    types: &[crate::scan::TypeDef],
) -> Result<Vec<Violation>, anyhow::Error> {
    let mut claims = Vec::new();
    for (name, component) in config
        .components
        .iter()
        .filter(|(_, c)| !c.defines.is_empty())
    {
        for pattern in &component.defines {
            let glob = globset::Glob::new(pattern)
                .map_err(|e| {
                    anyhow::anyhow!("bad pattern `{pattern}` in components.{name}.defines: {e}")
                })?
                .compile_matcher();
            claims.push((name.as_str(), pattern.as_str(), glob));
        }
    }
    let mut found = Vec::new();
    for def in types {
        let Some(home) = map.component_of(&def.file) else {
            continue;
        };
        let owners: Vec<(&str, &str)> = claims
            .iter()
            .filter(|(_, _, glob)| glob.is_match(&def.name))
            .map(|(c, p, _)| (*c, *p))
            .collect();
        if owners.is_empty() || owners.iter().any(|(c, _)| *c == home) {
            continue;
        }
        let (owner, pattern) = owners[0];
        let places: Vec<&str> = owners.iter().map(|(c, _)| *c).collect();
        found.push(Violation {
            rule: RuleId::Naming,
            file: def.file.clone(),
            line: def.line,
            message: format!(
                "{} is defined in {home}, but types named like {pattern} belong in {}",
                def.name,
                places.join(" or ")
            ),
            because: config.components[owner].because.clone(),
            from: home.to_string(),
            to: owner.to_string(),
            target: def.name.clone(),
            specifier: String::new(),
            evidence: vec![(def.file.clone(), def.line)],
        });
    }
    Ok(found)
}

/// One violation per use: the same key at the same line once, every other line
/// kept, sorted by key and line. The baseline still matches by key alone.
pub fn dedupe(violations: Vec<Violation>) -> Vec<Violation> {
    let mut unique: BTreeMap<(Key, usize), Violation> = BTreeMap::new();
    for v in violations {
        unique.entry((v.key(), v.line)).or_insert(v);
    }
    unique.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canonical_key_stays_canonical() {
        let key = |rule, file: &str| Key {
            rule,
            file: file.into(),
            target: String::new(),
            from: String::new(),
            to: String::new(),
        };
        for (rule, file, want) in [
            (RuleId::Required, "src/a/M.kt", "src/a/"),
            (RuleId::Required, "M.kt", "./"),
            (RuleId::Cycle, "src/a/M.kt", ""),
            (RuleId::Reach, "src/a/M.kt", ""),
            (RuleId::Dependency, "src/a/M.kt", "src/a/M.kt"),
        ] {
            let once = key(rule, file).canonical();
            assert_eq!(once.file, want);
            assert_eq!(once.clone().canonical(), once);
        }
    }

    #[test]
    fn config_line_finds_the_pattern_in_the_declaration_of_its_component() {
        let text = r#"version: 1
rules:
  b: { may_depend_on: [a] }
components:
  a:
    in:
      - src/ab
      - src/a   # was src/x
  "c": { in: src/c }
  d: { in: [src/d/] }
  b:
    # old home was src/old
    in: src/old
exclude: ["src/gone/**"]
"#;
        assert_eq!(config_line(text, "b", "src/old"), 13);
        assert_eq!(config_line(text, "d", "src/d"), 10);
        assert_eq!(config_line(text, "a", "src/ab"), 7);
        assert_eq!(config_line(text, "a", "src/a"), 8);
        assert_eq!(config_line(text, "c", "src/c"), 9);
        // A component that comes from `extends` is not declared here.
        assert_eq!(config_line(text, "legacy", "src/gone"), 1);
    }

    #[test]
    fn a_type_belongs_in_one_of_the_components_that_claim_its_name() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  web: { in: web }
  repo: { in: repo, defines: ['*Repository'] }
  cache: { in: cache, defines: ['*Repository', '*Cache'] }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let def = |file: &str, name: &str| crate::scan::TypeDef {
            file: file.into(),
            line: 3,
            name: name.into(),
        };
        let found = naming(
            &config,
            &map,
            &[
                def("web/a.go", "UserRepository"),
                def("repo/r.go", "OrderRepository"),
                def("cache/c.go", "OrderRepository"),
                def("web/b.go", "Page"),
                def("tools/t.go", "ToolRepository"),
            ],
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "UserRepository is defined in web, but types named like *Repository belong in cache or repo"
        );
        assert_eq!((found[0].file.as_str(), found[0].line), ("web/a.go", 3));
    }

    #[test]
    fn dedupe_keeps_every_line_and_drops_repeats() {
        let at = |line: usize| Violation {
            rule: RuleId::Symbol,
            file: "f".into(),
            line,
            message: String::new(),
            because: None,
            from: "a".into(),
            to: String::new(),
            target: "db.Client".into(),
            specifier: String::new(),
            evidence: Vec::new(),
        };
        let lines: Vec<usize> = dedupe(vec![at(9), at(3), at(9)])
            .iter()
            .map(|v| v.line)
            .collect();
        assert_eq!(lines, [3, 9]);
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            file: format!("{from}/x"),
            line: 1,
        }
    }

    fn config(no_cycles: &str) -> Config {
        serde_norway::from_str(&format!(
            "version: 1
components:
  a: {{ in: a }}
  b: {{ in: b }}
  c: {{ in: c }}
  d: {{ in: d }}
no_cycles: {no_cycles}
"
        ))
        .unwrap()
    }

    #[test]
    fn reports_each_cycle_group_once_with_shortest_path() {
        let edges = [
            edge("a", "b"),
            edge("b", "c"),
            edge("c", "a"),
            edge("b", "a"),
            edge("c", "d"),
        ];
        let found = cycles(&config("true"), &edges);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "component dependency cycle among 3 components: a, b, c (shortest: a -> b -> a)"
        );
        assert_eq!(found[0].from, "a,b,c");
        assert_eq!(found[0].evidence.len(), 4);
    }

    #[test]
    fn reach_reports_the_shortest_chain_from_any_file() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  a: { in: a }
  c: { in: c }
rules:
  a: { must_not_reach: [c] }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let dep = |from: &str, to: &str, line| Dep {
            file: from.into(),
            line,
            specifier: String::new(),
            target: Target::Internal(to.into()),
        };
        // `a/A1.kt` sorts first but its chain is the longer one.
        let deps = [
            dep("a/A1.kt", "x/L1.kt", 1),
            dep("x/L1.kt", "x/L2.kt", 2),
            dep("x/L2.kt", "c/C.kt", 3),
            dep("a/A2.kt", "x/S.kt", 4),
            dep("x/S.kt", "c/C.kt", 5),
        ];
        let files: Vec<String> = [
            "a/A1.kt", "a/A2.kt", "x/L1.kt", "x/L2.kt", "x/S.kt", "c/C.kt",
        ]
        .map(String::from)
        .to_vec();
        let found = reaches(&config, &map, &deps, &files);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "a reaches c through no component: a/A2.kt:4 -> x/S.kt:5 -> c/C.kt"
        );
        assert_eq!(found[0].target, "c/C.kt");
    }

    fn limits(rule: &str) -> Config {
        serde_norway::from_str(&format!(
            "version: 1
components:
  a: {{ in: a }}
  b: {{ in: b }}
  c: {{ in: c }}
rules:
  a: {{ {rule} }}
"
        ))
        .unwrap()
    }

    #[test]
    fn metrics_report_fan_out_and_instability() {
        let edges = [edge("a", "b"), edge("a", "c"), edge("b", "a")];
        // a: out 2, in 1 -> instability 0.67
        let found = metrics(&limits("max_fan_out: 1, max_instability: 0.5"), &edges);
        let targets: Vec<_> = found.iter().map(|v| v.target.as_str()).collect();
        assert_eq!(targets, ["max_fan_out", "max_instability"]);
        assert!(found[0].message.contains("depends on 2 components"));
        assert!(found[1].message.contains("instability 0.67"));
        assert!(metrics(&limits("max_fan_out: 2, max_instability: 0.7"), &edges).is_empty());
    }

    #[test]
    fn a_dependency_is_new_only_when_all_its_imports_changed() {
        let edges = [
            Edge {
                line: 1,
                ..edge("a", "b")
            },
            Edge {
                line: 2,
                ..edge("a", "b")
            },
            Edge {
                line: 3,
                ..edge("a", "c")
            },
        ];
        // Line 1 of a/x is old, so a -> b existed before; only a -> c is new.
        let got = new_dependencies(&edges, |_, line| line != 1);
        assert_eq!(got, ["a -> c"]);
    }

    #[test]
    fn orphans_only_in_strict_mode() {
        let yaml = |strict: bool| -> Config {
            serde_norway::from_str(&format!(
                "version: 1\ncomponents:\n  a: {{ in: a }}\nstrict: {strict}\n"
            ))
            .unwrap()
        };
        let files = ["a/x.go".to_string(), "b/y.go".to_string()];
        let strict = yaml(true);
        let map = ComponentMap::new(&strict).unwrap();
        let found = orphans(&strict, &map, &files);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, "b/y.go");
        assert!(orphans(&yaml(false), &map, &files).is_empty());
    }

    #[test]
    fn of_cycles_as_short_the_first_in_name_order_is_shown() {
        let edges = [
            edge("a", "c"),
            edge("c", "a"),
            edge("a", "b"),
            edge("b", "a"),
        ];
        let found = cycles(&config("true"), &edges);
        assert_eq!(
            found[0].message,
            "component dependency cycle among 3 components: a, b, c (shortest: a -> b -> a)"
        );
    }

    #[test]
    fn simple_cycle_and_scoping() {
        let edges = [edge("a", "b"), edge("b", "c"), edge("c", "a")];
        let found = cycles(&config("true"), &edges);
        assert_eq!(
            found[0].message,
            "component dependency cycle: a -> b -> c -> a"
        );
        assert!(cycles(&config("[a, b]"), &edges).is_empty());
        assert!(cycles(&config("false"), &edges).is_empty());
    }
}
