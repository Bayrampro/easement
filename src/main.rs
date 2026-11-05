mod baseline;
mod check;
mod components;
mod config;
mod decision;
mod diagram;
mod diff;
mod doctor;
mod explain;
mod flow;
mod graph;
mod impact;
mod indexer;
mod init;
mod lang;
mod manifests;
mod packages;
mod patch;
mod relpath;
mod report;
mod scan;
mod scip;
mod scope;
mod stdlib;
mod suggest;
mod suppress;
mod unused;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{ColorChoice, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use baseline::{Baseline, Verdict};
use components::ComponentMap;
use config::Config;

/// Architecture quality gate: fails when code dependencies cross component
/// boundaries that easement.yml does not allow.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// When to colour the output. `auto` colours a terminal and honours
    /// NO_COLOR and CLICOLOR_FORCE.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,
    /// How many languages to index at once: a number, or `cpus` for one per
    /// processor. Indexers can take gigabytes each, so the default is one.
    #[arg(long, global = true, value_name = "N|cpus", default_value = "1")]
    jobs: indexer::Jobs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the project. Exit code 0: passed, 1: violations, 2: error.
    Check {
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Ignore the baseline and report every violation.
        #[arg(long)]
        no_baseline: bool,
        /// Judge only imports on lines added since the merge-base with this
        /// revision (e.g. `origin/main`), including uncommitted changes.
        #[arg(long, value_name = "BASE")]
        diff: Option<String>,
        /// Check again whenever a file changes, re-reading only what changed.
        #[arg(long)]
        watch: bool,
        /// Report only on the files of this component; repeat for several.
        #[arg(long, value_name = "NAME")]
        component: Vec<String>,
        /// Report only on files under this path (a glob, relative to the config); repeat for several.
        #[arg(long, value_name = "PATH")]
        scope: Vec<String>,
        /// Run the indexers even when an index of the unchanged project is cached.
        #[arg(long, conflicts_with = "cache_only")]
        no_cache: bool,
        /// Run no indexer: read every index from the cache, and exit 3 when one is not
        /// cached. For CI, to set up an indexer's toolchain only when it has to run.
        #[arg(long)]
        cache_only: bool,
    },
    /// Record current violations as legalized debt in the baseline file. Once
    /// the file exists, it only shrinks: new violations are listed, not written.
    Baseline {
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
        /// Write new violations too, legalizing new debt. A human decision.
        #[arg(long)]
        accept_new: bool,
    },
    /// Write a starting easement.yml from the layout and imports of the project.
    Init {
        /// Overwrite an existing easement.yml.
        #[arg(long)]
        force: bool,
    },
    /// Show which component a file belongs to and what that component may do.
    Explain {
        /// File or directory, relative to the working directory or to the config.
        path: PathBuf,
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Draw the components and the dependencies between them.
    Graph {
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, value_enum, default_value_t = graph::GraphFormat::Mermaid)]
        format: graph::GraphFormat,
        /// Draw only this component and the ones it shares an arrow with.
        #[arg(long)]
        focus: Option<String>,
    },
    /// List every file that depends on the given files, directly or through
    /// others: what a change can break, and which tests to run.
    Impact {
        /// Files or directories, relative to the working directory or to the config.
        paths: Vec<PathBuf>,
        /// Take the files changed since the merge-base with this branch too.
        #[arg(long, value_name = "BASE")]
        diff: Option<String>,
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Show why one component depends on another: whether the config allows it,
    /// and the imports behind it, or the shortest chain of imports.
    Why {
        from: String,
        to: String,
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Say what the current dependency graph suggests for the config: allowances
    /// nothing uses, imports nothing allows, instances that could be isolated.
    Suggest {
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
        /// Print a patch for the config instead: it drops the `may_depend_on` entries no
        /// import uses (never one next to files no indexer covered), and leaves
        /// comments and reasons alone. Apply it with `git apply` or `patch -p1`.
        #[arg(long)]
        patch: bool,
        /// Exit 1 when `may_depend_on` has entries no import uses (the ones `--patch` would
        /// drop), so that CI keeps the allowances from piling up. Entries next to files no
        /// indexer covered are only reported, never fail it.
        #[arg(long, conflicts_with = "patch")]
        check: bool,
    },
    /// Check that the indexers the project needs are installed and can run.
    Doctor {
        #[arg(long, short, default_value = config::DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Print the JSON Schema of easement.yml.
    Schema,
    /// Print a shell completion script, e.g. `easement completions zsh > _easement`.
    Completions { shell: Shell },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
    /// SARIF 2.1.0, for GitHub Code Scanning.
    Sarif,
    /// GitHub Actions annotations.
    Github,
    /// JUnit XML.
    Junit,
}

struct Project {
    root: PathBuf,
    config: Config,
    /// The config file, resolved.
    config_path: PathBuf,
}

impl Project {
    fn open(config_path: &Path) -> Result<Self> {
        let config = Config::load(config_path)?;
        let config_path = dunce::canonicalize(config_path)
            .with_context(|| format!("cannot resolve {}", config_path.display()))?;
        let root = config_path
            .parent()
            .context("config has no parent directory")?
            .to_path_buf();
        Ok(Self {
            root,
            config,
            config_path,
        })
    }

    fn baseline_path(&self) -> PathBuf {
        let rel = self
            .config
            .baseline
            .clone()
            .unwrap_or_else(|| config::DEFAULT_BASELINE.into());
        self.root.join(rel)
    }

    fn analyze(&self, cache: &mut scan::Cache, scope: &scope::Scope) -> Result<Analysis> {
        let scan = scan::scan(&self.root, &self.config, cache)?;
        let map = ComponentMap::new(&self.config)?;
        let (mut violations, edges, mut stats) =
            check::evaluate(&self.config, &map, scope, &scan.deps);
        violations.extend(check::symbols(&self.config, &map, &scan.symbols));
        violations.extend(check::naming(&self.config, &map, &scan.types)?);
        let undeclared =
            manifests::undeclared(&self.config, &map, &self.root, &scan.deps, &scan.all_files);
        violations.extend(undeclared.violations);
        let (mut violations, suppressed) = suppress::apply(violations, &scan.suppressions);
        violations.extend(check::unmatched(
            &self.config,
            &map,
            &scan.all_files,
            &scan::slash(self.config_path.strip_prefix(&self.root)?),
            &std::fs::read_to_string(&self.config_path).unwrap_or_default(),
        ));
        violations.extend(check::cycles(&self.config, &edges));
        violations.extend(diagram::check(&self.config, &self.root, &edges)?);
        violations.extend(check::reaches(
            &self.config,
            &map,
            &scan.deps,
            &scan.all_files,
            scan.flow.as_ref(),
        ));
        violations.extend(check::metrics(&self.config, &edges));
        violations.extend(check::orphans(&self.config, &map, &scan.paths));
        violations.extend(check::forbidden_files(&self.config, &map, &scan.all_files));
        violations.extend(check::required(
            &self.root,
            &self.config,
            &map,
            &scan.deps,
            &scan.paths,
            &scan.all_files,
        ));
        let unused = unused::unused(&unused::Input {
            config: &self.config,
            map: &map,
            root: &self.root,
            deps: &scan.deps,
            unread_deps: &scan.unread_deps,
            unread: &scan.unread,
            paths: &scan.paths,
            all_files: &scan.all_files,
            uncovered: &scan.uncovered,
        })?;
        violations.extend(unused.violations);
        if self
            .config
            .components
            .values()
            .any(|c| !c.defines.is_empty())
        {
            let unseen: std::collections::BTreeSet<String> = scan
                .paths
                .iter()
                .filter(|f| map.component_of(f).is_some())
                // Types come from an index; an adapter reads imports alone.
                .filter(|f| !indexer::INDEXERS.iter().any(|l| l.covers(f)))
                .map(|f| relpath::extension(f))
                .filter(|ext| !ext.is_empty())
                .map(str::to_string)
                .collect();
            stats.naming_unseen = unseen.into_iter().collect();
        }
        stats.unused_partial = unused.reasons;
        stats.manifests_partial = undeclared.reasons;
        if self.config.packages {
            stats.packages_partial = scan.packages_problems.clone();
        }
        if self.config.tests.is_some() {
            stats.parts_partial = parts_reasons(&scan.parts_doubts);
        }
        stats
            .parts_partial
            .extend(parts_unmatched(&self.config, &scan.deps));
        stats.symbols_partial = check::symbols_partial(&self.config, &map, &scan.unnamed_uses);
        // Only a whole index can tell a name that matches nothing: files it did not
        // cover, names it left unnamed and languages without an index may hold it.
        let indexed_whole = scan.uncovered.is_empty()
            && scan.unnamed_uses.is_empty()
            && scan.known_symbols.1
            && scan
                .paths
                .iter()
                .all(|f| indexer::INDEXERS.iter().any(|l| l.covers(f)));
        stats.symbols_unmatched =
            check::symbols_unmatched(&self.config, &scan.known_symbols.0, indexed_whole);
        stats.files = scan
            .paths
            .iter()
            .filter(|f| scope.contains(&map, f))
            .count();
        stats.suppressed = suppressed;
        (stats.indexed, stats.from_cache) = scan.indexed;
        stats.uncovered = scan
            .uncovered
            .iter()
            .filter(|f| scope.contains(&map, f) && !map.is_uncovered_ok(f))
            .cloned()
            .collect();
        Ok(Analysis {
            unindexed: scan.uncovered,
            violations,
            all_files: scan.all_files,
            edges,
            deps: scan.deps,
            files: scan.paths,
            stats,
            map,
        })
    }

    /// Everything that changes the verdict: the files of the project, the
    /// config and the baseline, as a hash of their names, sizes and times.
    fn fingerprint(&self, config_path: &Path) -> u64 {
        let mut hasher = DefaultHasher::new();
        let mut files: Vec<PathBuf> = scan::walker(&self.root)
            .build()
            .flatten()
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .map(|e| e.into_path())
            .collect();
        files.push(config_path.to_path_buf());
        files.push(self.baseline_path());
        files.sort();
        files.dedup();
        for file in files {
            if let Ok(meta) = std::fs::metadata(&file) {
                (&file, meta.len(), meta.modified().ok()).hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

struct Analysis {
    violations: Vec<check::Violation>,
    /// Every source file no indexer read, `uncovered_ok` or not: the report
    /// leaves the accepted ones out, but nobody saw their imports, so what
    /// `suggest`, `impact`, `why` and `baseline` say about them stays careful.
    unindexed: Vec<String>,
    /// Every file that is not excluded, source or not.
    all_files: Vec<String>,
    edges: Vec<check::Edge>,
    deps: Vec<scan::Dep>,
    files: Vec<String>,
    stats: check::Stats,
    map: ComponentMap,
}

struct CheckOptions {
    config: PathBuf,
    format: Format,
    no_baseline: bool,
    diff: Option<String>,
    components: Vec<String>,
    paths: Vec<String>,
    index_cache: indexer::CacheUse,
    jobs: indexer::Jobs,
}

fn check(options: &CheckOptions, cache: &mut scan::Cache) -> Result<bool> {
    let project = Project::open(&options.config)?;
    let scope = scope::Scope::new(&project.config, &options.components, &options.paths)?;
    let Analysis {
        violations,
        mut stats,
        map,
        edges,
        ..
    } = project.analyze(cache, &scope)?;
    stats.limited_to = options
        .components
        .iter()
        .map(|c| format!("component {c}"))
        .chain(options.paths.iter().map(|p| format!("path {p}")))
        .collect();
    let current = violations.iter().map(check::Violation::key).collect();
    // Cycles and limits come from the whole project, so a violation belongs to
    // the scope when any file behind it does.
    let violations: Vec<_> = violations
        .into_iter()
        .filter(|v| {
            v.rule.is_config_rule() || v.evidence.iter().any(|(f, _)| scope.contains(&map, f))
        })
        .collect();
    let changes = options
        .diff
        .as_ref()
        .map(|base| diff::Changes::collect(&project.root, base))
        .transpose()?;
    if let Some(changes) = &changes {
        stats.new_dependencies = check::new_dependencies(&edges, |f, l| changes.contains(f, l));
    }
    let judged = match &changes {
        Some(changes) => violations
            .into_iter()
            .filter(|v| {
                v.rule.is_config_rule() || v.evidence.iter().any(|(f, l)| changes.contains(f, *l))
            })
            .collect(),
        None => violations,
    };
    let baseline = if options.no_baseline {
        None
    } else {
        Baseline::load(&project.baseline_path())?
    };
    let (warnings, judged): (Vec<_>, Vec<_>) = check::dedupe(judged)
        .into_iter()
        .partition(|v| project.config.warns(v.rule));
    let mut verdict = Verdict::judge(judged, warnings, &current, baseline.as_ref());
    verdict.owners = project.config.owners();
    verdict.stale.retain(|k| scope.contains(&map, &k.file));
    verdict.sort();
    let changes = changes.as_ref();
    match options.format {
        Format::Text => anstream::print!("{}", report::colored(&verdict, &stats, changes)),
        Format::Json => println!("{}", report::json(&verdict, &stats, changes)),
        Format::Sarif => println!("{}", report::sarif(&verdict)),
        Format::Github => print!("{}", report::github(&verdict, &stats, changes)),
        Format::Junit => print!("{}", report::junit(&verdict)),
    }
    Ok(verdict.passed())
}

/// Runs the check, then again after every change of the project. Never returns
/// on its own; errors (a broken config mid-edit) are shown and waited out.
fn watch(options: &CheckOptions) -> Result<bool> {
    let mut cache = scan::Cache::new(options.index_cache, options.jobs);
    let mut last = None;
    loop {
        let stamp = Project::open(&options.config)
            .ok()
            .map(|p| p.fingerprint(&options.config));
        if last != Some(stamp) {
            last = Some(stamp);
            if std::io::stdout().is_terminal() {
                print!("\x1b[2J\x1b[H");
            }
            if let Err(err) = check(options, &mut cache) {
                eprintln!("easement: error: {err:#}");
            }
            println!("easement: watching for changes, Ctrl-C to stop");
            std::io::stdout().flush()?;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn run(cli: Cli) -> Result<bool> {
    let jobs = cli.jobs;
    let fresh = || scan::Cache::new(indexer::CacheUse::default(), jobs);
    match cli.command {
        Command::Check {
            config,
            format,
            no_baseline,
            diff,
            watch: watching,
            component,
            scope,
            no_cache,
            cache_only,
        } => {
            let index_cache = if no_cache {
                indexer::CacheUse::Off
            } else if cache_only {
                indexer::CacheUse::Only
            } else {
                indexer::CacheUse::On
            };
            let options = CheckOptions {
                config,
                format,
                no_baseline,
                diff,
                components: component,
                paths: scope,
                index_cache,
                jobs,
            };
            if watching {
                watch(&options)
            } else {
                check(
                    &options,
                    &mut scan::Cache::new(options.index_cache, options.jobs),
                )
            }
        }
        Command::Baseline { config, accept_new } => {
            let project = Project::open(&config)?;
            let analysis = project.analyze(&mut fresh(), &scope::Scope::default())?;
            let (config_rules, violations): (Vec<_>, Vec<_>) = check::dedupe(analysis.violations)
                .into_iter()
                .partition(|v| v.rule.is_config_rule());
            // Warnings never fail the check, so there is nothing to legalize.
            let violations: Vec<_> = violations
                .into_iter()
                .filter(|v| !project.config.warns(v.rule))
                .collect();
            let path = project.baseline_path();
            let old = Baseline::load(&path)?;
            let (violations, refused) = match &old {
                Some(old) => old.admit(violations, accept_new),
                None => (violations, Vec::new()),
            };
            let mut baseline = Baseline::from_violations(&violations);
            // A file no indexer read may still hold its debt: keep its entries.
            let unread: Vec<_> = old
                .iter()
                .flat_map(|old| &old.violations)
                .filter(|k| analysis.unindexed.contains(&k.file))
                .filter(|k| !baseline.violations.contains(k))
                .cloned()
                .collect();
            baseline.violations.extend(unread.iter().cloned());
            baseline.save(&path)?;
            println!(
                "easement: {} violation(s) written to {}",
                violations.len(),
                path.display()
            );
            if let Some(note) = report::cache_note(&analysis.stats) {
                println!("easement: {note}");
            }
            if !unread.is_empty() {
                println!(
                    "easement: {} entr(ies) kept for files no indexer covered, so their debt is not known to be paid; `easement check` lists those files",
                    unread.len()
                );
            }
            for v in &refused {
                println!("{}:{}: {} [{}]", v.file, v.line, v.message, v.rule.as_str());
                if let Some(because) = &v.because {
                    println!("  because: {because}");
                }
            }
            if !refused.is_empty() {
                println!(
                    "easement: {} new violation(s) not written: the baseline only shrinks. Fix them, or legalize them with `easement baseline --accept-new` if a human agrees",
                    refused.len()
                );
            }
            if !config_rules.is_empty() {
                println!(
                    "easement: {} violation(s) of the config left out, the baseline never accepts them; `easement check` lists them",
                    config_rules.len()
                );
            }
            Ok(refused.is_empty())
        }
        Command::Impact {
            paths,
            diff,
            config,
            format,
        } => {
            let project = Project::open(&config)?;
            if paths.is_empty() && diff.is_none() {
                anyhow::bail!("name the changed files or directories, or pass `--diff <base>`");
            }
            let analysis = project.analyze(&mut fresh(), &scope::Scope::default())?;
            let named: Vec<String> = paths
                .iter()
                .map(|p| explain::root_relative(&project.root, p))
                .collect();
            let mut changed = impact::files_under(&named, &analysis.all_files)?;
            let mut deleted = Vec::new();
            if let Some(base) = &diff {
                let changes = diff::Changes::collect(&project.root, base)?;
                let known: std::collections::HashSet<&str> =
                    analysis.all_files.iter().map(String::as_str).collect();
                changed.extend(
                    changes
                        .files()
                        .filter(|f| known.contains(f))
                        .map(str::to_string),
                );
                deleted = changes.deleted;
            }
            let mut result = impact::impact(
                &analysis.map,
                &analysis.deps,
                &analysis.all_files,
                changed,
                deleted,
                &analysis.unindexed,
            );
            result.cached = report::cache_note(&analysis.stats);
            match format {
                OutputFormat::Text => print!("{}", impact::render(&result, &analysis.map)),
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
            }
            Ok(true)
        }
        Command::Why { from, to, config } => {
            let project = Project::open(&config)?;
            let analysis = project.analyze(&mut fresh(), &scope::Scope::default())?;
            let graph = impact::Graph {
                map: &analysis.map,
                deps: &analysis.deps,
                files: &analysis.all_files,
                violations: &analysis.violations,
                uncovered: &analysis.unindexed,
            };
            print!("{}", impact::why(&project.config, &graph, &from, &to)?);
            if let Some(note) = report::cache_note(&analysis.stats) {
                println!("Cached: {note}");
            }
            Ok(true)
        }
        Command::Init { force } => {
            let root = std::env::current_dir()?;
            println!("{}", init::run(&root, force, jobs)?);
            Ok(true)
        }
        Command::Explain { path, config } => {
            let project = Project::open(&config)?;
            let map = ComponentMap::new(&project.config)?;
            let rel = explain::root_relative(&project.root, &path);
            print!("{}", explain::explain(&project.config, &map, &rel)?);
            Ok(true)
        }
        Command::Graph {
            config,
            format,
            focus,
        } => {
            let project = Project::open(&config)?;
            if let Some(name) = &focus {
                project.config.require_component(name)?;
            }
            let analysis = project.analyze(&mut fresh(), &scope::Scope::default())?;
            // On stderr: stdout is the diagram itself.
            for reason in report::confidence_reasons(&analysis.stats, None) {
                eprintln!("easement: partial: {reason}");
            }
            if let Some(note) = report::cache_note(&analysis.stats) {
                eprintln!("easement: {note}");
            }
            print!(
                "{}",
                graph::render(
                    format,
                    &project.config,
                    &analysis.edges,
                    &analysis.violations,
                    focus.as_deref(),
                )
            );
            Ok(true)
        }
        Command::Suggest {
            config,
            patch,
            check,
        } => {
            let project = Project::open(&config)?;
            let analysis = project.analyze(&mut fresh(), &scope::Scope::default())?;
            if patch {
                let text = std::fs::read_to_string(&config)
                    .with_context(|| format!("cannot read {}", config.display()))?;
                let gone = suggest::removable(
                    &project.config,
                    &analysis.map,
                    &analysis.edges,
                    &analysis.unindexed,
                );
                let (new, skipped) = patch::remove_allowances(&text, &gone);
                for note in skipped
                    .into_iter()
                    .chain(report::cache_note(&analysis.stats))
                {
                    eprintln!("easement: {note}");
                }
                let name = config.to_string_lossy().replace('\\', "/");
                print!("{}", patch::unified_diff(&name, &text, &new));
                return Ok(true);
            }
            print!(
                "{}",
                suggest::suggest(
                    &project.config,
                    &analysis.map,
                    &analysis.edges,
                    &analysis.deps,
                    &analysis.files,
                    &analysis.unindexed,
                    &diagram::unused_arrows(&project.config, &project.root, &analysis.edges)?,
                )
            );
            if let Some(note) = report::cache_note(&analysis.stats) {
                println!("easement: {note}");
            }
            let unused = suggest::removable(
                &project.config,
                &analysis.map,
                &analysis.edges,
                &analysis.unindexed,
            );
            Ok(!check || unused.is_empty())
        }
        Command::Doctor { config, format } => {
            let project = Project::open(&config)?;
            let files = scan::project_files(&project.root, &project.config)?;
            let diagnoses = indexer::diagnose(&project.root, &files);
            print!(
                "{}",
                match format {
                    OutputFormat::Text => doctor::text(&diagnoses),
                    OutputFormat::Json => doctor::json(&diagnoses),
                }
            );
            Ok(doctor::ok(&diagnoses))
        }
        Command::Schema => {
            println!("{}", schema_text());
            Ok(true)
        }
        Command::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "easement",
                &mut std::io::stdout(),
            );
            Ok(true)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    anstream::ColorChoice::write_global(match cli.color {
        ColorChoice::Auto => anstream::ColorChoice::Auto,
        ColorChoice::Always => anstream::ColorChoice::Always,
        ColorChoice::Never => anstream::ColorChoice::Never,
    });
    match run(cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            let red = anstyle::AnsiColor::Red.on_default().bold();
            anstream::eprintln!("easement: {red}error{red:#}: {err:#}");
            if err.downcast_ref::<indexer::NotCached>().is_some() {
                ExitCode::from(3)
            } else {
                ExitCode::from(2)
            }
        }
    }
}

/// The JSON Schema of easement.yml as `easement schema` prints it and the
/// repository keeps it in `easement.schema.json`.
fn schema_text() -> String {
    serde_json::to_string_pretty(&config::schema()).expect("a schema is JSON")
}

/// How many files a line about test code that was not told apart names.
const PARTS_SHOWN: usize = 3;

/// A component whose `part` no file of the scan has owns nothing, and no rule on it judges
/// anything: a typo, or `packages` left off.
fn parts_unmatched(config: &config::Config, deps: &[scan::Dep]) -> Vec<String> {
    let found: std::collections::BTreeSet<&str> = deps
        .iter()
        .flat_map(|dep| {
            let target = match &dep.target {
                lang::Target::Internal(unit) => unit.part_name(),
                lang::Target::External(_) => None,
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
            (!found.contains(part)).then(|| {
                format!(
                    "component `{name}`: `part: {part}` matches no part of the files read (found: {list}), so it owns nothing and no rule on it judges anything; parts come from the manifests `packages` reads (`feature/<name>`, `dev-dependencies`, `build-dependencies`) and from `#[cfg(test)]` code of Rust (`cfg(test)`); `optional: true` accepts it"
                )
            })
        })
        .collect()
}

/// One line for each kind of doubt about test code, with the files it holds.
fn parts_reasons(doubts: &[(String, lang::parts::Doubt)]) -> Vec<String> {
    let mut by_kind: std::collections::BTreeMap<lang::parts::Doubt, Vec<&str>> =
        std::collections::BTreeMap::new();
    for (file, doubt) in doubts {
        by_kind.entry(*doubt).or_default().push(file);
    }
    by_kind
        .into_iter()
        .map(|(doubt, mut files)| {
            files.sort_unstable();
            // A file with several modules of one doubt is named once.
            files.dedup();
            let mut named = files
                .iter()
                .take(PARTS_SHOWN)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            if files.len() > PARTS_SHOWN {
                named.push_str(&format!(" and {} more", files.len() - PARTS_SHOWN));
            }
            doubt.reason(&named)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// What each part of easement makes of a file by its name: pinned while
    /// the knowledge of languages moves into one registry.
    #[test]
    fn languages_are_told_apart_as_before() {
        let files = [
            "a.go", "a.py", "a.pyi", "a.ts", "a.d.ts", "a.tsx", "a.mts", "a.cts", "a.js", "a.jsx",
            "a.mjs", "a.cjs", "A.java", "A.kt", "a.kts", "A.cs", "a.php", "a.rb", "a.rs",
            "a.swift", "Makefile", "v1.2/x", "a.json",
        ];
        let mut table = String::new();
        for file in files {
            let covered: Vec<&str> = crate::indexer::INDEXERS
                .iter()
                .filter(|l| l.covers(file))
                .map(|l| l.name())
                .collect();
            let adapters: Vec<&str> = crate::lang::adapters(std::path::Path::new("."))
                .unwrap()
                .iter()
                .filter(|a| a.handles(std::path::Path::new(file)))
                .map(|a| a.name())
                .collect();
            table.push_str(&format!(
                "{file}: indexer {covered:?}, adapter {adapters:?}, std {}, unused {:?}, manifest {:?}\n",
                crate::stdlib::knows(file),
                crate::unused::family(file),
                crate::lang::registry::of(file).and_then(|l| l.manifest),
            ));
        }
        let mut is_std = String::new();
        for (file, module) in [
            ("a.go", "net/http"),
            ("a.go", "github.com/x/y"),
            ("a.go", "C"),
            ("a.py", "os.path"),
            ("a.pyi", "os"),
            ("a.py", "requests"),
            ("A.java", "java.util.List"),
            ("A.kt", "kotlin.collections.List"),
            ("a.kts", "java.io.File"),
            ("a.rs", "std::fmt"),
            ("a.rs", "serde::de"),
            ("A.cs", "System.IO"),
            ("a.php", "DateTime"),
            ("a.php", "Symfony\\Component"),
            ("a.swift", "Foundation"),
            ("a.ts", "fs"),
            ("a.rb", "json"),
        ] {
            is_std.push_str(&format!(
                "{file} {module}: {}\n",
                crate::stdlib::is_std(file, module)
            ));
        }
        insta::assert_snapshot!("languages_by_file", table);
        insta::assert_snapshot!("std_by_language", is_std);
        insta::assert_snapshot!(
            "test_patterns",
            crate::lang::registry::test_patterns().join("\n")
        );
    }

    /// Editors read `easement.schema.json` from the repository, so it must be
    /// the schema the config types give.
    #[test]
    fn the_schema_file_is_the_generated_schema() {
        let file = include_str!("../easement.schema.json");
        assert!(
            file.trim_end() == super::schema_text(),
            "easement.schema.json is out of date: run `cargo run -q -- schema > easement.schema.json`"
        );
    }
}
