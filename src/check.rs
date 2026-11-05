use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use pathfinding::NodeRefs;
use pathfinding::directed::bfs::{bfs, bfs_loop, bfs_reach};
use pathfinding::directed::strongly_connected_components::strongly_connected_components;
use serde::{Deserialize, Serialize};

use crate::components::ComponentMap;
use crate::config::{Config, Listed};
use crate::decision::{self, Clause, Decision, Refusal};
use crate::flow::{self, Flow};
use crate::lang::{Target, registry};
use crate::policy::Policy;
use crate::relpath;
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

/// Code a rule could not read or judge: why, in one sentence, and the files or
/// the extensions of the files it holds for, where they are known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unjudged {
    pub reason: String,
    pub files: Vec<String>,
    pub extensions: Vec<String>,
}

impl Unjudged {
    pub fn new(reason: String) -> Self {
        Self {
            reason,
            ..Self::default()
        }
    }

    /// The files it holds for, sorted, each once.
    pub fn in_files<S: Into<String>>(mut self, files: impl IntoIterator<Item = S>) -> Self {
        self.files = files.into_iter().map(Into::into).collect();
        self.files.sort();
        self.files.dedup();
        self
    }

    /// The extensions of the files it holds for, sorted, each once.
    pub fn in_extensions<S: Into<String>>(
        mut self,
        extensions: impl IntoIterator<Item = S>,
    ) -> Self {
        self.extensions = extensions.into_iter().map(Into::into).collect();
        self.extensions.sort();
        self.extensions.dedup();
        self
    }
}

/// The reasons alone: what `stats` has always said of code a rule could not judge.
fn reasons<S: serde::Serializer>(unjudged: &[Unjudged], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(unjudged.iter().map(|u| &u.reason))
}

#[derive(Debug, Default, Serialize)]
pub struct Stats {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub analysis_partial: Vec<crate::scan::AnalysisGap>,
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
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "reasons")]
    pub manifests_partial: Vec<Unjudged>,
    /// Test code that `tests: auto` could not take out of the file it sits in.
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "reasons")]
    pub parts_partial: Vec<Unjudged>,
    /// Manifests, or dependencies of one, that `packages` could not judge as declared.
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "reasons")]
    pub packages_partial: Vec<Unjudged>,
    /// Why `forbid_symbol` left some uses of names unchecked.
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "reasons")]
    pub symbols_partial: Vec<Unjudged>,
    /// `forbid_symbol` names that match no symbol of the index, though one of the
    /// same name sits deeper under the qualifier: the rule can never fire.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols_unmatched: Vec<String>,
    /// Why `unused` took some files as used without judging them.
    #[serde(skip_serializing_if = "Vec::is_empty", serialize_with = "reasons")]
    pub unused_partial: Vec<Unjudged>,
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
    /// The node the use is in: the file, or a part of it that only a test build has.
    pub from: crate::lang::Unit,
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
        let external = matches!(dep.target, Target::External(_));
        if external {
            stats.external_deps += usize::from(counted);
        } else {
            stats.internal_deps += usize::from(counted);
        }
        let Some(decision) = decision::decide(config, map, dep) else {
            if counted && !external {
                unmapped.insert(file);
            }
            continue;
        };
        if external && counted && map.uses_std(decision.from) && !crate::stdlib::knows(&file) {
            let ext = relpath::extension(&file).to_string();
            if !stats.std_unknown.contains(&ext) {
                stats.std_unknown.push(ext);
            }
        }
        if let Some(to) = decision.to.filter(|to| *to != decision.from) {
            edges.push(Edge {
                from: decision.from.to_string(),
                to: to.to_string(),
                file: file.clone(),
                line: dep.line,
            });
        }
        found.extend(violation(&decision, dep));
    }
    stats.unmapped_files = unmapped.len();
    (found, edges, stats)
}

/// The violation a rejected dependency decision raises at the import behind it.
fn violation(decision: &Decision, dep: &Dep) -> Option<Violation> {
    let Decision {
        from,
        to,
        target,
        grant: Err(clause),
        because,
    } = decision
    else {
        return None;
    };
    let file = slash(&dep.file);
    let to = to.unwrap_or_default();
    let spec = &dep.specifier;
    let message = match clause {
        Clause::External(what) => format!("{from} -> external {target} {what}"),
        Clause::ExcludedTarget => format!(
            "{from} imports {target}, which `exclude` hides, so no rule checks it; list it in `target_only` if it is generated, in a component or `unowned` if it is part of the project, else do not import it"
        ),
        Clause::OtherInstance { here, there } => format!(
            "{here} -> {there}: instances of {from} must not import each other (imports {spec})"
        ),
        Clause::SliceMismatch { name, here, there } => {
            format!("{from} -> {to}: <{name}> differs ({here} imports {there}: {target})")
        }
        Clause::Refused(Refusal::Prohibited { .. }) => {
            format!("{from} -> {to} is forbidden (imports {spec})")
        }
        Clause::Refused(Refusal::NotGranted { layers }) => {
            let layers = layers
                .as_ref()
                .map(|why| format!(": {why}"))
                .unwrap_or_default();
            format!("{from} -> {to} is not allowed{layers} (imports {spec})")
        }
        Clause::ImporterNotListed(importers) => format!(
            "{from} -> {to}: {to} may be imported only from {} (imports {spec})",
            importers.join(", ")
        ),
        Clause::OutsideSurface => {
            format!("{from} -> {to}: {target} is not in the public API of {to} (imports {spec})")
        }
    };
    Some(Violation {
        rule: clause.rule(),
        evidence: vec![(file.clone(), dep.line)],
        file,
        line: dep.line,
        message,
        because: because.map(str::to_string),
        from: from.to_string(),
        to: to.to_string(),
        target: target.clone(),
        specifier: spec.clone(),
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
    flow: Option<&Flow>,
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
    let graph = match flow {
        Some(flow) => symbol_graph(deps, files, flow),
        None => file_graph(deps, files),
    };
    // A node is a file, or with `reach: symbols` a method of one.
    let component: BTreeMap<&str, &str> = graph
        .iter()
        .flat_map(|(f, out)| std::iter::once(f.as_str()).chain(out.iter().map(|(t, _)| t.as_str())))
        .filter_map(|f| map.component_of(flow::file_of(f)).map(|c| (f, c)))
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
        let hops: Vec<(String, usize)> = hops
            .into_iter()
            .map(|(node, line)| (flow::file_of(&node).to_string(), line))
            .collect();
        let last = flow::file_of(last);
        let (through, chain) = describe_chain(&hops, last, |file| map.component_of(file));
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

/// The files of a project by directory, the set of them, and the groups of
/// them that are built together (see [`registry::built_together`]).
struct Files<'a> {
    in_dir: BTreeMap<&'a str, Vec<&'a str>>,
    known: BTreeSet<&'a str>,
    together: BTreeMap<&'a str, std::rc::Rc<Vec<&'a str>>>,
}

impl<'a> Files<'a> {
    fn new(files: &'a [String]) -> Self {
        let mut in_dir: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for file in files {
            in_dir.entry(relpath::parent(file)).or_default().push(file);
        }
        let known: BTreeSet<&str> = files.iter().map(String::as_str).collect();
        let mut together = BTreeMap::new();
        for group in registry::built_together(&known.iter().copied().collect::<Vec<_>>()) {
            let group = std::rc::Rc::new(group);
            for file in group.iter() {
                together.insert(*file, group.clone());
            }
        }
        Self {
            in_dir,
            known,
            together,
        }
    }

    /// The files an import of `path` names: the file with those built together
    /// with it (the other files of its Go package), or one of each file directly
    /// in a package directory.
    fn targets(&self, path: &str) -> Vec<&'a str> {
        match self.known.get(path) {
            Some(file) => match self.together.get(file) {
                Some(group) => group.to_vec(),
                None => vec![*file],
            },
            None => self.in_dir.get(path).cloned().unwrap_or_default(),
        }
    }
}

/// Every file -> the files it imports, with the line of the import. An
/// import of a package directory is one of each file directly in it.
pub fn file_graph(deps: &[Dep], files: &[String]) -> BTreeMap<String, BTreeSet<(String, usize)>> {
    let files = Files::new(files);
    let mut graph: BTreeMap<String, BTreeSet<(String, usize)>> = BTreeMap::new();
    for dep in deps {
        let Target::Internal(unit) = &dep.target else {
            continue;
        };
        let out = graph.entry(dep.from.key()).or_default();
        for target in files.targets(&unit.key()) {
            out.insert((target.to_string(), dep.line));
        }
    }
    graph
}

/// The graph of `reach: symbols`: every node (see [`flow::Node::key`]) -> the nodes
/// its references name, with the line. An import that no reference of its file
/// accounts for reaches everything in what it names, from the definition that holds
/// its line.
fn symbol_graph(
    deps: &[Dep],
    files: &[String],
    flow: &Flow,
) -> BTreeMap<String, BTreeSet<(String, usize)>> {
    let files = Files::new(files);
    let mut graph: BTreeMap<String, BTreeSet<(String, usize)>> = BTreeMap::new();
    let mut covered: BTreeSet<(&str, usize, &str)> = BTreeSet::new();
    for edge in &flow.edges {
        let (from, to) = (edge.from.file.as_str(), edge.to.file.as_str());
        if !files.known.contains(from) || !files.known.contains(to) {
            continue;
        }
        covered.insert((from, edge.line, to));
        graph
            .entry(edge.from.key())
            .or_default()
            .insert((edge.to.key(), edge.line));
    }
    for dep in deps {
        let Target::Internal(unit) = &dep.target else {
            continue;
        };
        let file = dep.from.key();
        // Each file the import names that no reference at its line reaches is reached
        // whole, as the other files of a Go package are: they run on import too.
        let targets: Vec<&str> = files
            .targets(&unit.key())
            .into_iter()
            .filter(|target| !covered.contains(&(file.as_str(), dep.line, *target)))
            .collect();
        for from in flow.owners(&file, dep.line) {
            let out = graph.entry(from.key()).or_default();
            for target in &targets {
                for node in flow.nodes_of(target) {
                    out.insert((node.key(), dep.line));
                }
            }
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
    let parent = |file: &str| relpath::parent(file).to_string();
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
        let file = dep.from.key();
        let Some(component) = map.component_of_unit(&dep.from) else {
            continue;
        };
        let Some(unit) = units.get_mut(&(component.to_string(), parent(&file))) else {
            continue;
        };
        match &dep.target {
            Target::Internal(target) => {
                if let Some(to) = map.component_of_unit(target) {
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
        dir_files
            .entry(relpath::parent(file))
            .or_default()
            .push(relpath::file_name(file));
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
        let owner = map.component_of_unit(&used.from);
        // A file that belongs to no component is judged by the top-level list alone.
        let from = owner.unwrap_or_default();
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
        let own = owner
            .and_then(|component| config.rules.get(component))
            .into_iter()
            .flat_map(|rule| {
                rule.forbid_symbol
                    .iter()
                    .map(move |listed| (listed, rule.because.as_deref()))
            });
        let global = config.forbid_symbol.iter().map(|listed| (listed, None));
        let mut seen = std::collections::BTreeSet::new();
        for (listed, fallback) in global.chain(own) {
            let pattern = listed.name();
            if names_symbol(pattern, &used.specifier, &used.name)
                || used
                    .method
                    .as_ref()
                    .is_some_and(|(owner, method)| names_symbol(pattern, owner, method))
            {
                // A name in both lists is one violation.
                if !seen.insert(pattern) {
                    continue;
                }
                let who = match owner {
                    Some(component) => component.to_string(),
                    None => "unowned files".to_string(),
                };
                report(
                    format!("{who} must not use {pattern}"),
                    pattern.to_string(),
                    "",
                    listed.because().or(fallback),
                );
            }
        }
        if owner.is_some()
            && let Some(Target::Internal(unit)) = &used.target
            && let Some(to) = map.component_of_unit(unit)
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

/// Why `forbid_symbol` left uses of names unchecked: the files in `unnamed`
/// use names of an npm package that the indexer left unnamed, because a
/// `package.json` with no `name` inside it (`{"type": "module"}`) hides it.
/// Only files that a `forbid_symbol` list covers count: all of them for the top-level
/// list, else those of a component with its own.
pub fn symbols_partial(config: &Config, map: &ComponentMap, unnamed: &[String]) -> Vec<Unjudged> {
    let unnamed: Vec<&str> = unnamed
        .iter()
        .map(String::as_str)
        .filter(|file| forbids_symbols(config, map, file))
        .collect();
    if unnamed.is_empty() {
        return Vec::new();
    }
    const SHOWN: usize = 3;
    let mut files = unnamed[..unnamed.len().min(SHOWN)].join(", ");
    if unnamed.len() > SHOWN {
        files.push_str(&format!(" and {} more files", unnamed.len() - SHOWN));
    }
    vec![Unjudged::new(format!(
        "`forbid_symbol` cannot tell the npm package of the names used in {files}: a `package.json` without `name` inside the package hides it from the indexer, so those uses were not checked; scip-typescript 0.6.1 or newer names such a package: update the indexer and run again"
    )).in_files(unnamed)]
}

/// Whether a `forbid_symbol` list covers `file`: the top-level one, or that of
/// its component.
fn forbids_symbols(config: &Config, map: &ComponentMap, file: &str) -> bool {
    !config.forbid_symbol.is_empty()
        || map
            .component_of(file)
            .and_then(|component| config.rules.get(component))
            .is_some_and(|rule| !rule.forbid_symbol.is_empty())
}

/// The files among `files` that a symbol rule applies to although the registry
/// says their language has no verified uses of names: the rule passes there
/// without judging anything. One per rule and language.
///
/// `forbid_symbol` applies to every file when the top-level list is set, else to
/// the files of a component with its own; `public_symbols` to the files of a
/// component that import another component that has it.
pub fn symbols_unverified(
    config: &Config,
    map: &ComponentMap,
    files: &[&str],
    deps: &[Dep],
) -> Vec<Unjudged> {
    // The language of a file whose uses of names easement does not read.
    let unverified = |file: &str| registry::of(file).filter(|language| !language.symbols);
    let mut found: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for file in files.iter().copied() {
        if let Some(language) = unverified(file)
            && forbids_symbols(config, map, file)
        {
            found
                .entry(("forbid_symbol", language.name))
                .or_default()
                .push(file);
        }
    }
    let files: BTreeSet<&str> = files.iter().copied().collect();
    for dep in deps {
        let file = slash(&dep.file);
        let Some(&file) = files.get(file.as_str()) else {
            continue;
        };
        let Some(language) = unverified(file) else {
            continue;
        };
        let Some(from) = map.component_of_unit(&dep.from) else {
            continue;
        };
        let Target::Internal(unit) = &dep.target else {
            continue;
        };
        if let Some(to) = map.component_of_unit(unit)
            && to != from
            && config.components[to].public_symbols.is_some()
        {
            found
                .entry(("public_symbols", language.name))
                .or_default()
                .push(file);
        }
    }
    found
        .into_iter()
        .map(|((rule, language), mut files)| {
            files.sort_unstable();
            files.dedup();
            Unjudged::new(format!(
                "`{rule}` cannot judge the names used in {} {language} files: easement does not read uses of names from {language}, so those uses were not checked",
                files.len()
            ))
            .in_files(files)
        })
        .collect()
}

/// The `forbid_symbol` names that no symbol of the index matches, though a symbol
/// of the same name sits in a namespace under the qualifier (`I18n.locale` is
/// defined as `I18n::Base.locale`): such a rule cannot fire. A name the index has
/// never seen stays silent, since banning what the code does not use yet is what
/// the rule is for. `known` is what the index knows under the names that
/// `forbid_symbol` lists. `known_for` supplies only complete relevant domains;
/// an indeterminate query returns `None` and produces no absence claim.
pub fn symbols_unmatched(
    config: &Config,
    known_for: impl Fn(Option<&str>) -> Option<Vec<(String, String)>>,
) -> Vec<String> {
    let mut found = Vec::new();
    let lists = std::iter::once(("the top level", &config.forbid_symbol)).chain(
        config
            .rules
            .iter()
            .map(|(component, rule)| (component.as_str(), &rule.forbid_symbol)),
    );
    for (component, list) in lists {
        if list.is_empty() {
            continue;
        }
        let Some(known) = known_for((component != "the top level").then_some(component)) else {
            continue;
        };
        for listed in list {
            let pattern = listed.name();
            let Some((qualifier, wanted)) = pattern.rsplit_once('.') else {
                continue;
            };
            if known
                .iter()
                .any(|(specifier, name)| names_symbol(pattern, specifier, name))
            {
                continue;
            }
            let nested = known
                .iter()
                .map(|(specifier, _)| specifier)
                .find(|specifier| {
                    known.contains(&((*specifier).clone(), wanted.to_string()))
                        && specifier.strip_prefix(qualifier).is_some_and(|rest| {
                            rest.starts_with("::") || rest.starts_with('.') || rest.starts_with('/')
                        })
                });
            if let Some(specifier) = nested {
                found.push(format!(
                    "`forbid_symbol` `{pattern}` of {component} matches no symbol of the index, so it can never fire; did you mean `{specifier}.{wanted}`?"
                ));
            }
        }
    }
    found.sort();
    found.dedup();
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
            let name = relpath::file_name(file);
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
/// line in the configuration the policy was loaded from, or line 1 when a
/// file it `extends` defines the component. The
/// `tests` component of `tests: auto` and components marked `optional` may
/// match nothing.
pub fn unmatched(
    policy: &Policy,
    map: &ComponentMap,
    files: &[String],
    deps: &[Dep],
    config_file: &str,
) -> Vec<Violation> {
    let config = &policy.config;
    let config_text = policy.config_input().content.as_str();
    map.unmatched_patterns(files)
        .into_iter()
        .filter(|(component, _)| !config.components[*component].optional)
        .map(|(component, pattern)| Violation {
            rule: RuleId::Unmatched,
            file: config_file.to_string(),
            line: own_line(policy, component, pattern),
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
        .chain(unmatched_parts(policy, deps, config_file))
        .collect()
}

/// A `part` that names no part of the files read, unless its component is
/// `optional`: it owns nothing, so no rule on it judges anything. A typo, or
/// `packages` left off.
fn unmatched_parts(policy: &Policy, deps: &[Dep], config_file: &str) -> Vec<Violation> {
    let config = &policy.config;
    let found: BTreeSet<&str> = deps
        .iter()
        .flat_map(|dep| {
            let target = match &dep.target {
                Target::Internal(unit) => unit.part_name(),
                Target::External(_) => None,
            };
            [dep.from.part_name(), target]
        })
        .flatten()
        .collect();
    let list = if found.is_empty() {
        "none".to_string()
    } else {
        found.iter().copied().collect::<Vec<_>>().join(", ")
    };
    config
        .components
        .iter()
        .filter(|(_, component)| !component.optional)
        .filter_map(|(name, component)| {
            let part = component.part.as_deref()?;
            if found.contains(part) {
                return None;
            }
            let guess = crate::config::closest(part, found.iter().copied())
                .map(|p| format!("did you mean `{p}`? "))
                .unwrap_or_default();
            Some(Violation {
                rule: RuleId::Unmatched,
                file: config_file.to_string(),
                line: own_line(policy, name, part),
                message: format!(
                    "component `{name}`: `part: {part}` matches no part of the files read (found: {list}), so it owns nothing and no rule on it judges anything; {guess}parts come from the manifests `packages` reads (`feature/<name>`, `dev-dependencies`, `build-dependencies`) and from `#[cfg(test)]` code of Rust (`cfg(test)`); fix the part, or set `optional: true` if it may own nothing"
                ),
                because: None,
                from: name.clone(),
                to: String::new(),
                target: part.to_string(),
                specifier: String::new(),
                evidence: Vec::new(),
            })
        })
        .collect()
}

/// The line, counted from 1, where `component` lists `pattern` in the
/// configuration the policy was loaded from; 1 when a file it extends
/// defines the component.
fn own_line(policy: &Policy, component: &str, pattern: &str) -> usize {
    if !policy
        .defined_by("components", component)
        .is_some_and(|by| by.kind == crate::policy::InputKind::Config)
    {
        return 1;
    }
    config_line(&policy.config_input().content, component, pattern)
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

    fn forbidding(names: &[&str]) -> Config {
        let list: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
        serde_norway::from_str(&format!(
            "version: 1
components:
  web: {{ in: web }}
rules:
  web: {{ forbid_symbol: [{}] }}
",
            list.join(", ")
        ))
        .unwrap()
    }

    fn known(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(q, n)| (q.to_string(), n.to_string()))
            .collect()
    }

    #[test]
    fn a_name_defined_only_deeper_under_its_qualifier_is_flagged_with_the_candidate() {
        let config = forbidding(&["I18n.locale"]);
        let notes = symbols_unmatched(&config, |_| Some(known(&[("I18n::Base", "locale")])));
        assert_eq!(
            notes,
            [
                "`forbid_symbol` `I18n.locale` of web matches no symbol of the index, so it can never fire; did you mean `I18n::Base.locale`?"
            ]
        );
    }

    #[test]
    fn a_name_that_matches_or_that_nothing_defines_stays_silent() {
        let known = known(&[("I18n::Base", "locale"), ("log", "Println")]);
        for pattern in [
            // Matches exactly.
            "I18n::Base.locale",
            // Banned before anyone uses it, with another package of the same name around.
            "fmt.Println",
            // Not a nested namespace of the qualifier.
            "Time.locale",
            // Not defined at all.
            "I18n.deploy",
        ] {
            let config = forbidding(&[pattern]);
            assert!(
                symbols_unmatched(&config, |_| Some(known.clone())).is_empty(),
                "{pattern}"
            );
        }
    }

    #[test]
    fn unjudged_npm_names_say_which_scip_typescript_names_them() {
        let config = forbidding(&["i18next.i18next"]);
        let map = ComponentMap::new(&config).unwrap();
        let notes = symbols_partial(&config, &map, &["web/a.ts".to_string()]);
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].reason.contains("scip-typescript 0.6.1 or newer")
                && notes[0].reason.contains("web/a.ts"),
            "{notes:?}"
        );
        assert_eq!(notes[0].files, ["web/a.ts"]);
    }

    #[test]
    fn public_symbols_over_files_without_verified_uses_is_a_gap() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  core: { in: core, public_symbols: [Order] }
  web: { in: web }
  api: { in: api }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let dep = |from: &str, to: &str| Dep {
            from: from.into(),
            file: from.into(),
            line: 1,
            specifier: String::new(),
            target: Target::Internal(to.into()),
        };
        let deps = [
            dep("web/A.kt", "core/Order.kt"),
            dep("web/B.kt", "web/A.kt"),
            dep("api/c.go", "core/Order.kt"),
            dep("core/Repo.kt", "core/Order.kt"),
        ];
        let files = ["web/A.kt", "web/B.kt", "api/c.go", "core/Repo.kt"];
        let notes = symbols_unverified(&config, &map, &files, &deps);
        // Only a kotlin file that imports `core` from another component is unjudged:
        // Go uses are read, and `public_symbols` binds no file of its own component.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].files, ["web/A.kt"]);
        assert!(
            notes[0].reason.contains("`public_symbols`") && notes[0].reason.contains("kotlin"),
            "{notes:?}"
        );
        // Without a symbol rule nothing is unjudged.
        let plain: Config = serde_norway::from_str(
            "version: 1\ncomponents:\n  core: { in: core }\n  web: { in: web }\n",
        )
        .unwrap();
        let map = ComponentMap::new(&plain).unwrap();
        assert!(symbols_unverified(&plain, &map, &files, &deps).is_empty());
    }

    #[test]
    fn nothing_is_said_when_the_index_is_not_whole() {
        let config = forbidding(&["I18n.locale"]);
        assert!(symbols_unmatched(&config, |_| None).is_empty());
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
            from: from.into(),
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
        let found = reaches(&config, &map, &deps, &files, None);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "a reaches c through no component: a/A2.kt:4 -> x/S.kt:5 -> c/C.kt"
        );
        assert_eq!(found[0].target, "c/C.kt");
    }

    /// `a/A.rb` calls `host` (line 2); `x/G.rb` defines `host` (lines 3-5) and `fetch` (7-9),
    /// and `fetch` calls `get` of `c/C.rb`. `host_calls` lists what `host` references.
    fn symbol_world(host_calls: &[&str]) -> (Config, ComponentMap, Vec<Dep>, Vec<String>, Flow) {
        use crate::scip::{Document, Reference, Scope};
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
            from: from.into(),
            file: from.into(),
            line,
            specifier: String::new(),
            target: Target::Internal(to.into()),
        };
        let reference = |line, symbol: &str| Reference {
            line,
            symbol: symbol.to_string(),
            spelled: None,
        };
        let scope = |symbol: &str, first, last| Scope {
            symbol: symbol.to_string(),
            first,
            last,
        };
        let host: Vec<Reference> = host_calls.iter().map(|s| reference(4, s)).collect();
        let documents = vec![
            Document {
                path: "a/A.rb".into(),
                references: vec![reference(2, "host().")],
                ..Document::default()
            },
            Document {
                path: "x/G.rb".into(),
                definitions: vec!["host().".into(), "fetch().".into()],
                scopes: vec![scope("host().", 3, 5), scope("fetch().", 7, 9)],
                references: host.into_iter().chain([reference(8, "get().")]).collect(),
                ..Document::default()
            },
            Document {
                path: "c/C.rb".into(),
                definitions: vec!["get().".into()],
                scopes: vec![scope("get().", 1, 1)],
                ..Document::default()
            },
        ];
        let deps = vec![dep("a/A.rb", "x/G.rb", 2), dep("x/G.rb", "c/C.rb", 8)];
        let files = ["a/A.rb", "x/G.rb", "c/C.rb"].map(String::from).to_vec();
        let flow = crate::flow::build(&documents);
        (config, map, deps, files, flow)
    }

    #[test]
    fn reach_by_symbols_passes_a_method_that_reaches_nothing_forbidden() {
        let (config, map, deps, files, flow) = symbol_world(&[]);
        // By files, calling `host` reaches what `fetch` uses.
        assert_eq!(reaches(&config, &map, &deps, &files, None).len(), 1);
        assert!(reaches(&config, &map, &deps, &files, Some(&flow)).is_empty());
    }

    #[test]
    fn reach_by_symbols_keeps_a_chain_through_the_called_method() {
        // `host` calls `get` itself (line 4), so the chain stays, at the call of `host`.
        let (config, map, deps, files, flow) = symbol_world(&["get()."]);
        let found = reaches(&config, &map, &deps, &files, Some(&flow));
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "a reaches c through no component: a/A.rb:2 -> x/G.rb:4 -> c/C.rb"
        );
    }

    #[test]
    fn reach_by_symbols_reaches_the_files_of_a_package_that_no_reference_names() {
        use crate::scip::{Document, Reference, Scope};
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
            from: from.into(),
            file: from.into(),
            line,
            specifier: String::new(),
            target: Target::Internal(to.into()),
        };
        // `a/A.go` imports the package `x` at line 3 and calls `One` of `x/one.go`; `x/two.go`
        // holds code that runs on import and uses `c`.
        let documents = vec![
            Document {
                path: "a/A.go".into(),
                references: vec![Reference {
                    line: 3,
                    symbol: "One().".into(),
                    spelled: None,
                }],
                ..Document::default()
            },
            Document {
                path: "x/one.go".into(),
                definitions: vec!["One().".into()],
                scopes: vec![Scope {
                    symbol: "One().".into(),
                    first: 1,
                    last: 2,
                }],
                ..Document::default()
            },
            Document {
                path: "x/two.go".into(),
                ..Document::default()
            },
        ];
        let deps = [dep("a/A.go", "x", 3), dep("x/two.go", "c/C.go", 5)];
        let files = ["a/A.go", "x/one.go", "x/two.go", "c/C.go"]
            .map(String::from)
            .to_vec();
        let flow = crate::flow::build(&documents);
        let found = reaches(&config, &map, &deps, &files, Some(&flow));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].message,
            "a reaches c through no component: a/A.go:3 -> x/two.go:5 -> c/C.go"
        );
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

    #[test]
    fn a_symbol_used_in_test_code_is_judged_as_tests_not_as_the_file() {
        use crate::lang::parts::{Kind, Part};
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  store: { in: src/store.rs }
tests: auto
rules:
  store: { forbid_symbol: [testkit.Clock] }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let used = |from: crate::lang::Unit| SymbolRef {
            file: "src/store.rs".into(),
            from,
            line: 3,
            specifier: "testkit".into(),
            name: "Clock".into(),
            method: None,
            target: Some(Target::External("testkit".into())),
        };
        let test_code = crate::lang::Unit::in_part(
            std::path::Path::new("src/store.rs"),
            Part {
                name: "cfg(test)".into(),
                kind: Some(Kind::Test),
            },
        );
        assert_eq!(
            symbols(
                &config,
                &map,
                &[used(crate::lang::Unit::from("src/store.rs"))]
            )
            .len(),
            1
        );
        assert!(symbols(&config, &map, &[used(test_code)]).is_empty());
    }
}
