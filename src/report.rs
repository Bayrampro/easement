use anstyle::{AnsiColor, Style};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;

use crate::baseline::Verdict;
use crate::check::{Key, RuleId, Stats, Unjudged, Violation};
use crate::components::AcceptedGap;
use crate::diff::Changes;

#[derive(Serialize)]
struct JsonReport<'a> {
    passed: bool,
    violations: &'a [Violation],
    /// Violations of the rules in `warn`, which do not fail the check.
    warnings: &'a [Violation],
    /// Who to ask about a violation, by the component in its `from`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    owners: &'a BTreeMap<String, String>,
    stale: &'a [Key],
    legalized: usize,
    /// `full`, or `partial` when `passed` rests on code the check could not read.
    confidence: Confidence,
    confidence_reasons: Vec<String>,
    #[serde(flatten)]
    coverage: &'a Coverage,
    /// Set when indexes came from the cache: what their key leaves out and how
    /// to index again.
    #[serde(skip_serializing_if = "Option::is_none")]
    cached: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<DiffInfo<'a>>,
    stats: &'a Stats,
}

#[derive(Serialize)]
struct DiffInfo<'a> {
    base: &'a str,
    merge_base: &'a str,
}

/// How much of the project an answer covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Full,
    /// It rests on code no indexer read, or judges only a part by design.
    Partial,
}

/// What an answer leaves out: the part it was asked to judge, the code it could
/// not read or judge, and the gaps the policy accepts. A cache hit is none of them.
#[derive(Debug, Default, Serialize)]
pub struct Coverage {
    /// What the run was asked to judge, when not the whole project.
    pub narrowed: Vec<Narrowing>,
    /// Code or facts the run could not read or judge: a pass says nothing about them.
    pub evidence_gaps: Vec<EvidenceGap>,
    /// Files no indexer read that `uncovered_ok` accepts, by the entry that accepts them.
    pub accepted_gaps: Vec<AcceptedGap>,
}

/// A part of the project or of a change that a narrowed verdict judges alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Narrowing {
    pub kind: NarrowingKind,
    /// The base of `--diff`, the component of `--component`, the path of `--scope`.
    pub limited_to: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NarrowingKind {
    Diff,
    Component,
    Scope,
}

/// Code or facts that a check could not read or judge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceGap {
    pub kind: GapKind,
    /// The files the gap is in, when they are known one by one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// The extensions of the files the gap is in, when it holds for a whole language.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// How many files, when they are known one by one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    pub message: String,
    /// How to close the gap.
    pub close: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// Source files or facts an indexer did not fully cover.
    Uncovered,
    /// Files whose types `defines` cannot see.
    NamingUnseen,
    /// Files whose standard library `$std` cannot tell.
    StdUnknown,
    /// Imports that `manifests` could not judge.
    Manifests,
    /// Test code that could not be told from the rest of its file.
    Parts,
    /// Manifests, or dependencies of one, that `packages` could not judge.
    Packages,
    /// Uses of names that `forbid_symbol` could not judge.
    Symbols,
    /// Files that `unused` took as used without judging them.
    Unused,
    /// Files a change deletes: what imported them is not in the code any more.
    Deleted,
}

/// The parts that `--diff`, `--component` and `--scope` narrow a check to.
pub fn narrowed(
    changes: Option<&Changes>,
    components: &[String],
    paths: &[String],
) -> Vec<Narrowing> {
    let item = |kind, limited_to: &String| Narrowing {
        kind,
        limited_to: limited_to.clone(),
    };
    changes
        .map(|c| item(NarrowingKind::Diff, &c.base))
        .into_iter()
        .chain(components.iter().map(|c| item(NarrowingKind::Component, c)))
        .chain(paths.iter().map(|p| item(NarrowingKind::Scope, p)))
        .collect()
}

/// The narrowing by `--component` and `--scope`, as `component web` and
/// `path src/api`: what `stats.limited_to` holds.
pub fn limited_to(narrowed: &[Narrowing]) -> Vec<String> {
    narrowed
        .iter()
        .filter_map(|n| match n.kind {
            NarrowingKind::Diff => None,
            NarrowingKind::Component => Some(format!("component {}", n.limited_to)),
            NarrowingKind::Scope => Some(format!("path {}", n.limited_to)),
        })
        .collect()
}

impl EvidenceGap {
    /// The gap of what a rule could not judge, with how to close it.
    pub fn of(kind: GapKind, unjudged: &Unjudged, close: &str) -> Self {
        EvidenceGap {
            kind,
            files: unjudged.files.clone(),
            extensions: unjudged.extensions.clone(),
            count: (!unjudged.files.is_empty()).then_some(unjudged.files.len()),
            message: unjudged.reason.clone(),
            close: close.to_string(),
        }
    }
}

pub const CLOSE_UNCOVERED: &str =
    "add them to a project the indexer reads, `exclude` them, or accept them in `uncovered_ok`";

/// Code a check could not read or judge, in the order the reasons were always given.
pub fn evidence_gaps(stats: &Stats) -> Vec<EvidenceGap> {
    let mut gaps = analysis_gaps(stats);
    let mut add = |kind, unjudged: &[Unjudged], close: &str| {
        gaps.extend(unjudged.iter().map(|u| EvidenceGap::of(kind, u, close)));
    };
    add(
        GapKind::Unused,
        &stats.unused_partial,
        "the files it took as used may be dead: review them by hand, or have an indexer read them",
    );
    add(
        GapKind::Manifests,
        &stats.manifests_partial,
        "fix or add the manifest the message names, or review those imports by hand",
    );
    add(
        GapKind::Parts,
        &stats.parts_partial,
        "fix what the message names, or review that code by hand",
    );
    add(
        GapKind::Packages,
        &stats.packages_partial,
        "fix the manifest the message names, or review those dependencies by hand",
    );
    add(
        GapKind::Symbols,
        &stats.symbols_partial,
        "do what the message says, or review those uses by hand",
    );
    if !stats.naming_unseen.is_empty() {
        add(
            GapKind::NamingUnseen,
            &[Unjudged::new(format!(
                "`defines` cannot see the types of .{} files, which are read without an index, so their names were not checked",
                stats.naming_unseen.join(", .")
            ))
            .in_extensions(stats.naming_unseen.iter().cloned())],
            "no indexer reads these files, so review the names of their types by hand",
        );
    }
    if !stats.std_unknown.is_empty() {
        add(
            GapKind::StdUnknown,
            &[Unjudged::new(format!(
                "`$std` cannot tell the standard library of .{} files, so their standard imports count as other modules; name those modules instead",
                stats.std_unknown.join(", .")
            ))
            .in_extensions(stats.std_unknown.iter().cloned())],
            "name the standard modules the rule means instead of `$std`",
        );
    }
    if !stats.uncovered.is_empty() {
        add(
            GapKind::Uncovered,
            &[Unjudged::new(format!(
                "{} source files were not covered by an indexer, so imports in or of them were not checked",
                stats.uncovered.len()
            ))
            .in_files(stats.uncovered.iter().cloned())],
            CLOSE_UNCOVERED,
        );
    }
    gaps
}

impl Coverage {
    /// What a check over `stats` leaves out, when it was narrowed to `narrowed`.
    pub fn of_check(
        stats: &Stats,
        narrowed: Vec<Narrowing>,
        accepted_gaps: Vec<AcceptedGap>,
    ) -> Self {
        Coverage {
            narrowed,
            evidence_gaps: evidence_gaps(stats),
            accepted_gaps,
        }
    }

    /// `partial` when the answer is narrowed or has an evidence gap; an
    /// accepted gap alone leaves it `full`.
    pub fn confidence(&self) -> Confidence {
        if self.narrowed.is_empty() && self.evidence_gaps.is_empty() {
            Confidence::Full
        } else {
            Confidence::Partial
        }
    }

    /// Why the answer may be short, as one sentence each: the narrowing, then the gaps.
    pub fn reasons(&self) -> Vec<String> {
        let mut reasons = self.narrowed_reasons();
        reasons.extend(self.evidence_gaps.iter().map(|g| g.message.clone()));
        reasons
    }

    /// One sentence for `--diff`, and one for `--component` and `--scope` together.
    fn narrowed_reasons(&self) -> Vec<String> {
        let mut reasons: Vec<String> = self
            .narrowed
            .iter()
            .filter(|n| n.kind == NarrowingKind::Diff)
            .map(|n| {
                format!(
                    "only violations on lines changed since {} were judged, the rest of the project was not",
                    n.limited_to
                )
            })
            .collect();
        let limited = limited_to(&self.narrowed);
        if !limited.is_empty() {
            reasons.push(format!(
                "the report is limited to {}, the rest of the project was not judged",
                limited.join(", ")
            ));
        }
        reasons
    }
}

/// Missing facts inside documents an indexer did read.
pub fn analysis_gaps(stats: &Stats) -> Vec<EvidenceGap> {
    stats
        .analysis_partial
        .iter()
        .zip(analysis_reasons(stats))
        .map(|(gap, reason)| {
            EvidenceGap::of(
                GapKind::Uncovered,
                &Unjudged::new(reason).in_files((!gap.file.is_empty()).then(|| gap.file.clone())),
                "fix the inputs the message names, or review those dependencies by hand; \
                 recheck with `--no-cache`",
            )
        })
        .collect()
}

/// Gaps inside indexed documents, with their source location.
pub fn analysis_reasons(stats: &Stats) -> Vec<String> {
    stats
        .analysis_partial
        .iter()
        .map(|gap| {
            format!(
                "{}{}: {}",
                gap.file,
                gap.line.map(|line| format!(":{line}")).unwrap_or_default(),
                gap.reason
            )
        })
        .collect()
}

/// What a verdict over cached indexes rests on. Not a reason for `partial`: the
/// key holds configured inputs; state outside them still needs re-verification.
pub fn cache_note(stats: &Stats) -> Option<String> {
    (stats.from_cache > 0).then(|| {
        format!(
            "{} index(es) came from the cache, keyed on the configured source, dependency, \
             tool and environment inputs; state outside those inputs is not verified, \
             so run with `--no-cache` after changing it",
            stats.from_cache
        )
    })
}

/// What a run of the check says: its verdict, what it saw, what it leaves out
/// and, in diff mode, the change it judged.
#[derive(Clone, Copy)]
pub struct Report<'a> {
    pub verdict: &'a Verdict,
    pub stats: &'a Stats,
    pub coverage: &'a Coverage,
    pub changes: Option<&'a Changes>,
    /// `--require-evidence`: an evidence gap fails a run without violations.
    pub require_evidence: bool,
}

/// How many uncovered files the text report names before it counts the rest.
const UNCOVERED_SHOWN: usize = 20;

const BOLD: Style = Style::new().bold();
const DIM: Style = Style::new().dimmed();
const RED: Style = AnsiColor::Red.on_default().bold();
const GREEN: Style = AnsiColor::Green.on_default().bold();
const YELLOW: Style = AnsiColor::Yellow.on_default();

/// `text` wrapped in the escape codes of `style`, or as it is when `color` is off.
fn paint(color: bool, style: Style, text: &str) -> String {
    if color {
        format!("{style}{text}{style:#}")
    } else {
        text.to_string()
    }
}

/// The first files of a list, one per line under `indent`, and how many more.
fn file_lines(files: &[String], indent: &str) -> String {
    let mut out = String::new();
    for file in files.iter().take(UNCOVERED_SHOWN) {
        out.push_str(&format!("{indent}{file}\n"));
    }
    if files.len() > UNCOVERED_SHOWN {
        out.push_str(&format!(
            "{indent}… and {} more (all of them in `--format json`)\n",
            files.len() - UNCOVERED_SHOWN
        ));
    }
    out
}

/// The evidence gaps, each with how to close it and its files, then the
/// accepted gaps, each with its reason and files.
/// Under `--require-evidence` the gaps fail the run, and their heading says so.
fn gap_lines(coverage: &Coverage, required: bool, color: bool) -> String {
    let mut out = String::new();
    if !coverage.evidence_gaps.is_empty() {
        out.push_str(&if required {
            paint(
                color,
                RED,
                "Evidence required (`--require-evidence`), but the check could not read or judge this code:",
            )
        } else {
            paint(
                color,
                YELLOW,
                "Evidence gaps (a pass says nothing about this code):",
            )
        });
        out.push('\n');
        for gap in &coverage.evidence_gaps {
            out.push_str(&format!("  {}\n", paint(color, YELLOW, &gap.message)));
            out.push_str(&paint(color, DIM, &format!("    to close: {}", gap.close)));
            out.push('\n');
            out.push_str(&file_lines(&gap.files, "    "));
        }
    }
    if !coverage.accepted_gaps.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(
            "Accepted gaps (`uncovered_ok`: no indexer read them, so their imports were not checked):\n",
        );
        for gap in &coverage.accepted_gaps {
            out.push_str(&format!("  {}", gap.uncovered_ok.join(", ")));
            if let Some(because) = &gap.because {
                out.push_str(&paint(color, DIM, &format!(" because: {because}")));
            }
            out.push('\n');
            out.push_str(&file_lines(&gap.files, "    "));
        }
    }
    out
}

/// `file:line: message [rule]`, and the reason under it when there is one.
fn violation_lines(v: &Violation, owner: Option<&str>, tag: Style, color: bool) -> String {
    let mut out = format!(
        "{}: {} {}\n",
        paint(color, BOLD, &format!("{}:{}", v.file, v.line)),
        v.message,
        paint(color, tag, &format!("[{}]", v.rule.as_str()))
    );
    if let Some(because) = &v.because {
        out.push_str(&paint(color, DIM, &format!("  because: {because}")));
        out.push('\n');
    }
    if let Some(owner) = owner {
        out.push_str(&paint(color, DIM, &format!("  owner: {owner}")));
        out.push('\n');
    }
    out
}

impl Report<'_> {
    /// Whether `--require-evidence` fails the run: it has an evidence gap.
    /// Violations and stale entries fail it first.
    pub fn evidence_required(self) -> bool {
        self.require_evidence && !self.coverage.evidence_gaps.is_empty()
    }

    /// Whether the run passes: no violations, no stale entries, and the
    /// evidence `--require-evidence` asks for.
    pub fn passed(self) -> bool {
        self.verdict.passed() && !self.evidence_required()
    }

    /// The report as data.
    pub fn json(self) -> String {
        let Report {
            verdict,
            stats,
            coverage,
            changes,
            require_evidence: _,
        } = self;
        let report = JsonReport {
            passed: self.passed(),
            violations: &verdict.new,
            warnings: &verdict.warnings,
            owners: &verdict.owners,
            stale: &verdict.stale,
            legalized: verdict.legalized,
            confidence: coverage.confidence(),
            confidence_reasons: coverage.reasons(),
            coverage,
            cached: cache_note(stats),
            diff: changes.map(|c| DiffInfo {
                base: &c.base,
                merge_base: &c.merge_base,
            }),
            stats,
        };
        serde_json::to_string_pretty(&report).expect("report serializes")
    }

    /// The plain text report.
    pub fn text(self) -> String {
        self.render(false)
    }

    /// The text report with ANSI styles; the caller strips them where the output
    /// is not a terminal.
    pub fn colored(self) -> String {
        self.render(true)
    }

    fn render(self, color: bool) -> String {
        let Report {
            verdict,
            stats,
            coverage,
            changes,
            require_evidence: _,
        } = self;
        let mut out = String::new();
        if let Some(c) = changes {
            let short = &c.merge_base[..c.merge_base.len().min(12)];
            out.push_str(&format!(
                "Checking lines changed since {} (merge-base {short})\n\n",
                c.base
            ));
        }
        for v in &verdict.new {
            out.push_str(&violation_lines(
                v,
                verdict.owner_of(v).as_deref(),
                RED,
                color,
            ));
        }
        if !verdict.warnings.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&paint(
                color,
                YELLOW,
                "Warnings (rules in `warn`; do not add more):",
            ));
            out.push('\n');
            for v in &verdict.warnings {
                out.push_str(&violation_lines(
                    v,
                    verdict.owner_of(v).as_deref(),
                    YELLOW,
                    color,
                ));
            }
        }
        if !verdict.stale.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&paint(
                color,
                YELLOW,
                "Stale baseline entries (fixed, run `easement baseline` to shrink it):",
            ));
            out.push('\n');
            for k in &verdict.stale {
                let what = stale_description(k);
                out.push_str(&format!("  [{}] {what}\n", k.rule.as_str()));
            }
        }
        if !stats.new_dependencies.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("New dependencies between components in this change:\n");
            for d in &stats.new_dependencies {
                out.push_str(&format!("  {d}\n"));
            }
        }
        if !stats.symbols_unmatched.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            for note in &stats.symbols_unmatched {
                out.push_str(&paint(color, YELLOW, note));
                out.push('\n');
            }
        }
        let gaps = gap_lines(coverage, self.evidence_required(), color);
        if !gaps.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&gaps);
        }
        if let Some(note) = cache_note(stats) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&note);
            out.push('\n');
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let status = if self.passed() {
            paint(color, GREEN, "passed")
        } else {
            paint(color, RED, "failed")
        };
        let warnings = if verdict.warnings.is_empty() {
            String::new()
        } else {
            format!(", {} warning(s)", verdict.warnings.len())
        };
        out.push_str(&format!(
        "easement {status}: {} violation(s), {} stale, {} legalized{warnings}; {} files, {} internal deps",
        verdict.new.len(),
        verdict.stale.len(),
        verdict.legalized,
        stats.files,
        stats.internal_deps,
    ));
        if !stats.limited_to.is_empty() {
            out.push_str(&format!(", limited to {}", stats.limited_to.join(", ")));
        }
        if stats.suppressed > 0 {
            out.push_str(&format!(", {} suppressed", stats.suppressed));
        }
        if !stats.uncovered.is_empty() {
            out.push_str(&format!(
                ", {} files not covered by an indexer",
                stats.uncovered.len()
            ));
        }
        if stats.unmapped_files > 0 {
            out.push_str(&format!(
                ", {} files outside components",
                stats.unmapped_files
            ));
        }
        out.push('\n');
        out
    }
}

fn stale_description(k: &Key) -> String {
    match k.rule {
        RuleId::Cycle => format!("cycle among {}", k.from),
        RuleId::Metric => format!("{} exceeds {}", k.from, k.target),
        RuleId::Reach => format!("{} reaches {}", k.from, k.to),
        RuleId::Diagram => format!("{} -> {} is now in the diagram or gone", k.from, k.to),
        RuleId::External => format!("{}: {} -> external {}", k.file, k.from, k.target),
        RuleId::Dependency | RuleId::Privacy => {
            format!("{}: {} -> {} ({})", k.file, k.from, k.to, k.target)
        }
        _ => format!("{}: {}", k.file, k.target),
    }
}

fn stale_message(k: &Key) -> String {
    format!(
        "stale baseline entry, fixed debt: {} (run `easement baseline` to shrink it)",
        stale_description(k)
    )
}

/// SARIF 2.1.0, the format GitHub Code Scanning ingests.
pub fn sarif(verdict: &Verdict) -> String {
    let mut ids: Vec<&str> = verdict
        .new
        .iter()
        .chain(&verdict.warnings)
        .map(|v| v.rule.as_str())
        .collect();
    if !verdict.stale.is_empty() {
        ids.push(STALE);
    }
    ids.sort_unstable();
    ids.dedup();
    let rules: Vec<_> = ids
        .iter()
        .map(|id| json!({ "id": id, "shortDescription": { "text": format!("easement {id}") } }))
        .collect();
    let mut results: Vec<_> = verdict
        .reported()
        .map(|(level, v)| {
            let text = match &v.because {
                Some(because) => format!("{} (because: {because})", v.message),
                None => v.message.clone(),
            };
            json!({
                "ruleId": v.rule.as_str(),
                "level": level,
                "message": { "text": text },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": v.file },
                        "region": { "startLine": v.line },
                    },
                }],
            })
        })
        .collect();
    results.extend(verdict.stale.iter().map(|k| {
        json!({
            "ruleId": STALE,
            "level": "error",
            "message": { "text": stale_message(k) },
        })
    }));
    let report = json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "easement",
                "version": env!("CARGO_PKG_VERSION"),
                "informationUri": env!("CARGO_PKG_HOMEPAGE"),
                "rules": rules,
            } },
            "results": results,
        }],
    });
    serde_json::to_string_pretty(&report).expect("report serializes")
}

fn escape_command(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn escape_property(text: &str) -> String {
    escape_command(text).replace(':', "%3A").replace(',', "%2C")
}

impl Report<'_> {
    /// GitHub Actions workflow commands: annotations on the changed files.
    pub fn github(self) -> String {
        let Report {
            verdict,
            stats,
            coverage,
            changes: _,
            require_evidence: _,
        } = self;
        let mut out = String::new();
        for (level, v) in verdict.reported() {
            let mut text = match &v.because {
                Some(because) => format!("{} (because: {because})", v.message),
                None => v.message.clone(),
            };
            if let Some(owner) = verdict.owner_of(v) {
                text.push_str(&format!(" (owner: {owner})"));
            }
            out.push_str(&format!(
                "::{level} file={},line={},title={}::{}\n",
                escape_property(&v.file),
                v.line,
                escape_property(&format!("easement {}", v.rule.as_str())),
                escape_command(&text)
            ));
        }
        for k in &verdict.stale {
            out.push_str(&format!(
                "::error title={}::{}\n",
                escape_property(&format!("easement {STALE}")),
                escape_command(&stale_message(k))
            ));
        }
        if !stats.new_dependencies.is_empty() {
            out.push_str(&format!(
                "::notice title={}::{}\n",
                escape_property("easement graph"),
                escape_command(&format!(
                    "New component dependencies in this change: {}",
                    stats.new_dependencies.join(", ")
                ))
            ));
        }
        let narrowed = coverage
            .narrowed_reasons()
            .into_iter()
            .map(|reason| ("notice", reason));
        let gap_level = if self.evidence_required() {
            "error"
        } else {
            "warning"
        };
        let gaps = coverage
            .evidence_gaps
            .iter()
            .map(|gap| (gap_level, gap.message.clone()));
        for (level, reason) in narrowed.chain(gaps) {
            out.push_str(&format!(
                "::{level} title={}::{}\n",
                escape_property("easement partial"),
                escape_command(&reason)
            ));
        }
        if let Some(note) = cache_note(stats) {
            out.push_str(&format!(
                "::notice title={}::{}\n",
                escape_property("easement cache"),
                escape_command(&note)
            ));
        }
        out.push_str(
            &self
                .text()
                .lines()
                .last()
                .map_or_else(String::new, |l| format!("{l}\n")),
        );
        out
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// JUnit XML: one failed test case per violation, so CI shows them as test failures.
pub fn junit(verdict: &Verdict) -> String {
    let failures = verdict.new.len() + verdict.stale.len();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<testsuite name=\"easement\" tests=\"{}\" failures=\"{failures}\">\n",
        failures.max(1)
    ));
    if failures == 0 {
        out.push_str("  <testcase classname=\"easement\" name=\"architecture\"/>\n");
    }
    for v in &verdict.new {
        let name = format!("{}:{}", v.file, v.line);
        let mut body = v.message.clone();
        if let Some(because) = &v.because {
            body.push_str(&format!("\nbecause: {because}"));
        }
        out.push_str(&format!(
            "  <testcase classname=\"easement.{}\" name=\"{}\"><failure message=\"{}\" type=\"{}\">{}</failure></testcase>\n",
            v.rule.as_str(),
            escape_xml(&name),
            escape_xml(&v.message),
            v.rule.as_str(),
            escape_xml(&body)
        ));
    }
    for k in &verdict.stale {
        let message = stale_message(k);
        out.push_str(&format!(
            "  <testcase classname=\"easement.{STALE}\" name=\"{}\"><failure message=\"{}\" type=\"{STALE}\">{}</failure></testcase>\n",
            escape_xml(&stale_description(k)),
            escape_xml(&message),
            escape_xml(&message)
        ));
    }
    out.push_str("</testsuite>\n");
    out
}

const STALE: &str = "stale-baseline";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::Verdict;

    fn verdict() -> Verdict {
        Verdict {
            new: vec![Violation {
                rule: RuleId::Dependency,
                file: "src/a.ts".into(),
                line: 3,
                message: "a -> b is not allowed <x> 100%".into(),
                because: Some("layers".into()),
                from: "a".into(),
                to: "b".into(),
                target: "src/b.ts".into(),
                specifier: "../b".into(),
                evidence: Vec::new(),
            }],
            warnings: vec![],
            owners: BTreeMap::new(),
            stale: vec![Key {
                rule: RuleId::Metric,
                file: "f".into(),
                target: "max_fan_out".into(),
                from: "core".into(),
                to: String::new(),
            }],
            legalized: 0,
        }
    }

    #[test]
    fn sarif_has_located_results() {
        let report: serde_json::Value = serde_json::from_str(&sarif(&verdict())).unwrap();
        assert_eq!(report["version"], "2.1.0");
        let results = report["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["ruleId"], "dependency");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            3
        );
        assert_eq!(results[1]["ruleId"], "stale-baseline");
        assert_eq!(
            report["runs"][0]["tool"]["driver"]["rules"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn github_annotations_are_escaped() {
        let out = Report {
            verdict: &verdict(),
            stats: &Stats::default(),
            coverage: &Coverage::default(),
            changes: None,
            require_evidence: false,
        }
        .github();
        assert!(out.starts_with(
            "::error file=src/a.ts,line=3,title=easement dependency::a -> b is not allowed <x> 100%25 (because: layers)\n"
        ));
        assert!(out.contains("::error title=easement stale-baseline::"));
        assert!(out.contains("easement failed"));
    }

    #[test]
    fn junit_counts_failures_and_escapes() {
        let xml = junit(&verdict());
        assert!(xml.contains("tests=\"2\" failures=\"2\""));
        assert!(xml.contains("&lt;x&gt;"));
        let clean = junit(&Verdict {
            new: vec![],
            warnings: vec![],
            owners: BTreeMap::new(),
            stale: vec![],
            legalized: 0,
        });
        assert!(clean.contains("tests=\"1\" failures=\"0\""));
        assert!(clean.contains("name=\"architecture\"/>"));
    }

    #[test]
    fn a_cache_hit_is_named_with_how_to_index_again() {
        let stats = Stats {
            indexed: 1,
            from_cache: 2,
            ..Stats::default()
        };
        let report: serde_json::Value = serde_json::from_str(
            &Report {
                verdict: &verdict(),
                stats: &stats,
                coverage: &Coverage::default(),
                changes: None,
                require_evidence: false,
            }
            .json(),
        )
        .unwrap();
        let cached = report["cached"].as_str().unwrap();
        assert!(
            cached.contains("2 index(es)") && cached.contains("--no-cache"),
            "{cached}"
        );
        let out = Report {
            verdict: &verdict(),
            stats: &stats,
            coverage: &Coverage::default(),
            changes: None,
            require_evidence: false,
        }
        .text();
        assert!(out.contains(cached), "{out}");

        let fresh: serde_json::Value = serde_json::from_str(
            &Report {
                verdict: &verdict(),
                stats: &Stats::default(),
                coverage: &Coverage::default(),
                changes: None,
                require_evidence: false,
            }
            .json(),
        )
        .unwrap();
        assert!(fresh.get("cached").is_none());
        assert!(
            !Report {
                verdict: &verdict(),
                stats: &Stats::default(),
                coverage: &Coverage::default(),
                changes: None,
                require_evidence: false,
            }
            .text()
            .contains("--no-cache")
        );
    }

    #[test]
    fn an_accepted_gap_leaves_the_verdict_full_and_is_still_reported() {
        let accepted = vec![AcceptedGap {
            uncovered_ok: vec!["gen/**".into(), "tools/**".into()],
            because: Some("generated".into()),
            files: vec!["gen/b.go".into(), "tools/a.go".into()],
        }];
        let coverage = Coverage {
            accepted_gaps: accepted,
            ..Coverage::default()
        };
        assert_eq!(coverage.confidence(), Confidence::Full);
        assert!(coverage.reasons().is_empty());
        let out = Report {
            verdict: &verdict(),
            stats: &Stats::default(),
            coverage: &coverage,
            changes: None,
            require_evidence: false,
        }
        .text();
        assert!(
            out.contains("  gen/**, tools/** because: generated\n    gen/b.go\n    tools/a.go\n"),
            "{out}"
        );
    }

    #[test]
    fn narrowing_by_component_and_scope_is_one_reason_after_the_diff() {
        let coverage = Coverage {
            narrowed: vec![
                Narrowing {
                    kind: NarrowingKind::Diff,
                    limited_to: "main".into(),
                },
                Narrowing {
                    kind: NarrowingKind::Component,
                    limited_to: "db".into(),
                },
                Narrowing {
                    kind: NarrowingKind::Scope,
                    limited_to: "internal".into(),
                },
            ],
            ..Coverage::default()
        };
        assert_eq!(coverage.confidence(), Confidence::Partial);
        assert_eq!(
            coverage.reasons(),
            [
                "only violations on lines changed since main were judged, the rest of the project was not",
                "the report is limited to component db, path internal, the rest of the project was not judged",
            ]
        );
    }

    #[test]
    fn require_evidence_turns_the_gaps_of_a_clean_run_into_errors() {
        let clean = Verdict {
            new: vec![],
            warnings: vec![],
            owners: BTreeMap::new(),
            stale: vec![],
            legalized: 0,
        };
        let stats = Stats {
            uncovered: vec!["tools/gen.go".into()],
            ..Stats::default()
        };
        let coverage = Coverage::of_check(&stats, vec![], vec![]);
        let report = |require_evidence| Report {
            verdict: &clean,
            stats: &stats,
            coverage: &coverage,
            changes: None,
            require_evidence,
        };
        assert!(report(false).passed());
        let github = report(false).github();
        assert!(
            github.contains("::warning title=easement partial::"),
            "{github}"
        );
        assert!(github.contains("easement passed"), "{github}");

        assert!(!report(true).passed());
        let github = report(true).github();
        assert!(
            github.contains("::error title=easement partial::"),
            "{github}"
        );
        assert!(github.contains("easement failed"), "{github}");
        // Nothing to require of a run that reads everything.
        let whole = Coverage::default();
        assert!(
            Report {
                coverage: &whole,
                ..report(true)
            }
            .passed()
        );
    }

    #[test]
    fn github_says_why_a_verdict_is_partial_and_when_it_was_cached() {
        let stats = Stats {
            from_cache: 1,
            uncovered: vec!["tools/gen.go".into()],
            ..Stats::default()
        };
        let out = Report {
            verdict: &verdict(),
            stats: &stats,
            coverage: &Coverage::of_check(&stats, vec![], vec![]),
            changes: None,
            require_evidence: false,
        }
        .github();
        assert!(
            out.contains(
                "::warning title=easement partial::1 source files were not covered by an indexer"
            ),
            "{out}"
        );
        assert!(out.contains("::notice title=easement cache::1 index(es) came from the cache"));
        assert!(out.contains("--no-cache"));
        assert!(
            out.ends_with("1 files not covered by an indexer\n"),
            "{out}"
        );
        assert!(
            !Report {
                verdict: &verdict(),
                stats: &Stats::default(),
                coverage: &Coverage::default(),
                changes: None,
                require_evidence: false,
            }
            .github()
            .contains("::warning")
        );

        // A run narrowed by design is a notice: a warning on every pull request
        // would teach to skip the ones about code nobody read.
        let narrowed = Coverage {
            narrowed: super::narrowed(None, &["web".into()], &[]),
            ..Coverage::default()
        };
        let out = Report {
            verdict: &verdict(),
            stats: &Stats::default(),
            coverage: &narrowed,
            changes: None,
            require_evidence: false,
        }
        .github();
        assert!(
            out.contains("::notice title=easement partial::the report is limited to component web"),
            "{out}"
        );
        assert!(!out.contains("::warning"), "{out}");
    }

    #[test]
    fn gaps_inside_indexed_documents_count_as_required_evidence() {
        let stats = Stats {
            analysis_partial: vec![crate::scan::AnalysisGap {
                file: "src/generated.rs".into(),
                line: Some(7),
                reason: "the indexer omitted a reference".into(),
            }],
            ..Stats::default()
        };
        let coverage = Coverage::of_check(&stats, vec![], vec![]);
        assert_eq!(coverage.confidence(), Confidence::Partial);
        assert_eq!(coverage.evidence_gaps.len(), 1);
        let gap = &coverage.evidence_gaps[0];
        assert_eq!(gap.kind, GapKind::Uncovered);
        assert_eq!(gap.files, ["src/generated.rs"]);
        assert_eq!(gap.count, Some(1));
        assert_eq!(
            gap.message,
            "src/generated.rs:7: the indexer omitted a reference"
        );
        let clean = Verdict {
            new: vec![],
            warnings: vec![],
            owners: BTreeMap::new(),
            stale: vec![],
            legalized: 0,
        };
        let report = Report {
            verdict: &clean,
            stats: &stats,
            coverage: &coverage,
            changes: None,
            require_evidence: true,
        };
        assert!(!report.passed());
        assert!(report.github().contains("::error title=easement partial::"));
    }
}
