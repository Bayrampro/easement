use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

use crate::check::SymbolRef;
use crate::config::ScanPolicy;
use crate::indexer;
use crate::lang::{self, Import, Target, Unit, registry};
use crate::scip;
use crate::suppress::{self, Suppression};

/// A resolved dependency of one project file.
#[derive(Debug, Clone)]
pub struct Dep {
    /// Where it is written: diff mode, suppressions, the baseline and the
    /// report read the file and the line.
    pub file: PathBuf,
    pub line: usize,
    pub specifier: String,
    pub target: Target,
    /// The node it leaves: the file itself.
    pub from: Unit,
}

/// What was read from a file, valid while its size and modification time stay the same.
struct Parsed {
    modified: Option<SystemTime>,
    len: u64,
    imports: Vec<Import>,
    suppressions: Vec<Suppression>,
}

/// Parsed files kept between scans, so watch mode re-reads only what changed.
/// Resolution is never cached: it depends on the other files.
#[derive(Default)]
pub struct Cache {
    parsed: HashMap<PathBuf, Parsed>,
    /// Whether the indexers' output is read from and kept in the index cache.
    pub index_cache: indexer::CacheUse,
    /// How many languages are indexed at once.
    pub jobs: indexer::Jobs,
}

impl Cache {
    /// A cache that keeps the indexers' output between runs as `index_cache` says,
    /// and indexes `jobs` languages at once.
    pub fn new(index_cache: indexer::CacheUse, jobs: indexer::Jobs) -> Self {
        Self {
            index_cache,
            jobs,
            ..Self::default()
        }
    }
}

/// A type a file defines: a class, a struct, an interface, a trait.
#[derive(Debug, Clone)]
pub struct TypeDef {
    pub file: String,
    pub line: usize,
    pub name: String,
}

pub struct Scan {
    pub coverage_checks: Vec<indexer::CoverageCheck>,
    pub domains: Vec<crate::coverage::Domain>,
    pub checked_files: Vec<String>,
    pub analysis_partial: Vec<AnalysisGap>,
    /// Every file that is not excluded, source or not, relative to the root with `/` separators.
    pub all_files: Vec<String>,
    /// Every checked file, relative to the root with `/` separators.
    pub paths: Vec<String>,
    pub deps: Vec<Dep>,
    /// Imports in files the checks do not read (`target_only`, excluded):
    /// not judged, but they still reach a file.
    pub unread_deps: Vec<Dep>,
    /// The `target_only` and excluded files the walk saw.
    pub unread: Vec<String>,
    pub symbols: Vec<SymbolRef>,
    /// The types checked files define, from the index.
    pub types: Vec<TypeDef>,
    pub suppressions: Vec<Suppression>,
    /// Source files of an indexed language that the indexer did not cover,
    /// for example files outside every `tsconfig.json`.
    pub uncovered: Vec<String>,
    /// Checked files that use a name of a package the indexer left unnamed,
    /// which no rule on names judged.
    pub unnamed_uses: Vec<String>,
    /// Files whose test code a finder of parts could not tell from the file's.
    pub parts_doubts: Vec<(String, lang::parts::Doubt)>,
    /// Manifests, or dependencies of one, that `packages` could not judge as declared.
    pub packages_problems: Vec<crate::check::Unjudged>,
    /// Watched symbol names and missing classification, by language domain.
    pub known_symbols: crate::coverage::Symbols,
    /// Indexers that ran, and indexes taken from the cache.
    pub indexed: (usize, usize),
    /// With `reach: symbols`, the references of the index by the definition that holds
    /// them.
    pub flow: Option<crate::flow::Flow>,
}

/// Analysis limitations within files that may still appear in an index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct AnalysisGap {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub reason: String,
}

/// Project walker: honours .gitignore (even outside a git checkout) and
/// always skips `node_modules`.
pub fn walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .require_git(false)
        .filter_entry(|e| e.file_name() != "node_modules");
    builder
}

/// The project's files (`/`-separated, relative to `root`) without the excluded ones.
pub fn project_files(root: &Path, policy: &ScanPolicy) -> Result<Vec<String>> {
    let exclude = glob_set(&policy.exclude, "exclude")?;
    let mut files = Vec::new();
    for entry in walker(root).build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Some(rel) = lang::relative(root, entry.path())
            && !exclude.is_match(&rel)
        {
            files.push(slash(&rel));
        }
    }
    Ok(files)
}

/// Reads the project as `policy`, the scan-relevant part of the policy, asks.
pub fn scan(root: &Path, policy: &ScanPolicy, cache: &mut Cache) -> Result<Scan> {
    let exclude = glob_set(&policy.exclude, "exclude")?;
    let target_only = glob_set(&policy.target_only, "target_only")?;
    let adapters = lang::adapters(root)?;
    let mut paths = Vec::new();
    let mut all_files = Vec::new();
    // Excluded files the walk sees (not the ones .gitignore hides).
    let mut excluded = HashSet::new();
    let mut deps = Vec::new();
    let mut suppressions = Vec::new();
    let mut seen = HashSet::new();
    for entry in walker(root).build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Some(rel) = lang::relative(root, path) else {
            continue;
        };
        if exclude.is_match(&rel) {
            excluded.insert(slash(&rel));
            continue;
        }
        all_files.push(slash(&rel));
        if indexer::INDEXERS.iter().any(|l| l.covers(&slash(&rel))) {
            continue; // read from the index below
        }
        if target_only.is_match(&rel) {
            continue; // a target of imports, never read as a source
        }
        let Some(adapter) = adapters.iter().find(|a| a.handles(path)) else {
            continue;
        };
        let meta = entry.metadata()?;
        let modified = meta.modified().ok();
        let fresh = cache
            .parsed
            .get(&rel)
            .is_some_and(|p| p.len == meta.len() && p.modified == modified && modified.is_some());
        if !fresh {
            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue, // binary or unreadable
            };
            let parsed = Parsed {
                modified,
                len: meta.len(),
                imports: adapter.imports(path, &source).with_context(|| {
                    format!("{} adapter failed on {}", adapter.name(), rel.display())
                })?,
                suppressions: suppress::parse(&slash(&rel), &source),
            };
            cache.parsed.insert(rel.clone(), parsed);
        }
        let parsed = &cache.parsed[&rel];
        seen.insert(rel.clone());
        paths.push(slash(&rel));
        suppressions.extend(parsed.suppressions.iter().cloned());
        for import in &parsed.imports {
            deps.push(Dep {
                from: Unit::from(rel.as_path()),
                file: rel.clone(),
                line: import.line,
                specifier: import.specifier.clone(),
                target: adapter.resolve(path, &import.specifier),
            });
        }
    }
    cache.parsed.retain(|path, _| seen.contains(path));

    let mut hidden: Vec<String> = excluded.iter().cloned().collect();
    hidden.sort();
    let indexed = indexer::run(
        root,
        &all_files,
        &hidden,
        cache.index_cache,
        cache.jobs,
        policy,
    )?;
    let project: HashSet<&str> = all_files.iter().map(String::as_str).collect();
    // Excluded files are never read as sources, but what they define stays
    // resolvable, so an import of one is seen and fails as `excluded`.
    let (mut documents, hidden): (Vec<scip::Document>, Vec<scip::Document>) = indexed
        .documents
        .0
        .into_iter()
        .filter(|d| project.contains(d.path.as_str()) || excluded.contains(&d.path))
        .partition(|d| project.contains(d.path.as_str()));
    let mut parts_doubts: Vec<(String, lang::parts::Doubt)> = Vec::new();
    // (the file that declares them, the files a test-only module may be in)
    let mut declared_files: Vec<(String, Vec<String>)> = Vec::new();
    // The other modules each file declares, test code too if the file turns out to be.
    let mut modules: HashMap<String, Vec<lang::parts::Module>> = HashMap::new();
    for document in &mut documents {
        let Some(find) = lang::registry::of(&document.path).and_then(|language| language.parts)
        else {
            continue;
        };
        match std::fs::read_to_string(root.join(&document.path)) {
            Ok(source) => {
                let found = find(&document.path, &source);
                document.parts = found.spans;
                parts_doubts.extend(found.doubts.into_iter().map(|d| (document.path.clone(), d)));
                declared_files.extend(found.files.into_iter().map(|f| (document.path.clone(), f)));
                modules.insert(document.path.clone(), found.modules);
            }
            Err(_) => parts_doubts.push((document.path.clone(), lang::parts::Doubt::Unreadable)),
        }
    }
    // A module that only a test build has is test code as a whole, in its own file, and
    // so is every module that file declares, all the way down.
    let mut pending = declared_files;
    let mut test_files: HashSet<String> = HashSet::new();
    while let Some((declaring, candidates)) = pending.pop() {
        let Some(file) = candidates
            .into_iter()
            .find(|c| project.contains(c.as_str()))
        else {
            parts_doubts.push((declaring, lang::parts::Doubt::ModuleInAnotherFile));
            continue;
        };
        if !test_files.insert(file.clone()) {
            continue;
        }
        if let Some(document) = documents.iter_mut().find(|d| d.path == file) {
            document.parts = vec![lang::parts::Span::test(1..=usize::MAX)];
        }
        for module in modules.remove(&file).unwrap_or_default() {
            match module {
                lang::parts::Module::Files(candidates) => pending.push((file.clone(), candidates)),
                lang::parts::Module::Unplaced(doubt) => parts_doubts.push((file.clone(), doubt)),
            }
        }
    }
    for document in documents
        .iter()
        .filter(|d| !target_only.is_match(d.path.as_str()))
    {
        paths.push(document.path.clone());
        if let Ok(source) = std::fs::read_to_string(root.join(&document.path)) {
            suppressions.extend(suppress::parse(&document.path, &source));
        }
    }
    let types: Vec<TypeDef> = documents
        .iter()
        .filter(|d| !target_only.is_match(d.path.as_str()))
        .flat_map(|d| {
            d.types.iter().map(|(name, line)| TypeDef {
                file: d.path.clone(),
                line: *line,
                name: name.clone(),
            })
        })
        .collect();
    let flow =
        (policy.reach == crate::config::Reach::Symbols).then(|| crate::flow::build(&documents));
    let facts = scip::facts_watching(
        &documents,
        &hidden,
        policy.uses_symbols,
        &policy.watched_symbols,
    );
    let is_source = |file: &Path| {
        let file = slash(file);
        project.contains(file.as_str()) && !target_only.is_match(&file)
    };
    let (sources, unread_deps): (Vec<Dep>, Vec<Dep>) =
        facts.deps.into_iter().partition(|dep| is_source(&dep.file));
    deps.extend(sources);
    let mut packages_problems = Vec::new();
    if policy.packages {
        let edges = crate::packages::edges(root, &all_files, |file| !target_only.is_match(file));
        paths.extend(edges.manifests);
        deps.extend(edges.deps);
        packages_problems = edges.problems;
    }
    let mut unread_deps = unread_deps;
    unread_deps.extend(scip::facts(&hidden, &documents, false).deps);
    let mut unread: Vec<String> = excluded.iter().cloned().collect();
    unread.extend(
        all_files
            .iter()
            .filter(|f| target_only.is_match(f.as_str()))
            .cloned(),
    );
    unread.sort();
    let known_symbols = facts.known;
    let unnamed_uses = facts
        .unnamed
        .iter()
        .filter(|file| is_source(Path::new(file.as_str())))
        .cloned()
        .collect();
    let symbols = facts
        .symbols
        .into_iter()
        .filter(|symbol| is_source(&symbol.file))
        // Uses of names count only for a language whose uses are verified; a rule
        // over the others is an evidence gap (`check::symbols_unverified`).
        .filter(|symbol| {
            symbol
                .file
                .to_str()
                .and_then(lang::registry::of)
                .is_some_and(|language| language.symbols)
        })
        // A use of a name an excluded file defines adds nothing: its import
        // already fails as `excluded`.
        .filter(|symbol| {
            !matches!(&symbol.target, Some(Target::Internal(unit)) if exclude.is_match(unit.path()))
        })
        .collect();
    let checked_files = all_files
        .iter()
        .filter(|f| registry::of(f).is_some() && !target_only.is_match(f.as_str()))
        .cloned()
        .collect();
    Ok(Scan {
        coverage_checks: indexed.coverage_checks,
        domains: indexed.domains,
        checked_files,
        analysis_partial: indexed
            .analysis_partial
            .into_iter()
            .chain(facts.analysis_partial)
            .filter(|gap| {
                registry::of(&gap.file).is_none()
                    || (!exclude.is_match(&gap.file) && !target_only.is_match(&gap.file))
            })
            .collect(),
        all_files,
        paths,
        deps,
        unread_deps,
        unread,
        types,
        symbols,
        suppressions,
        uncovered: indexed.uncovered,
        unnamed_uses,
        parts_doubts,
        packages_problems,
        known_symbols,
        indexed: (indexed.ran, indexed.cached),
        flow,
    })
}

/// Matches each pattern and everything below it.
pub fn glob_set(patterns: &[String], key: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let p = p.trim_end_matches('/');
        builder.add(Glob::new(p).with_context(|| format!("bad {key} pattern `{p}`"))?);
        builder.add(Glob::new(&format!("{p}/**"))?);
    }
    Ok(builder.build()?)
}

/// `/`-separated string form of a relative path, used for matching and output.
pub fn slash(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
