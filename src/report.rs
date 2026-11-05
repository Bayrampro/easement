use anstyle::{AnsiColor, Style};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;

use crate::baseline::Verdict;
use crate::check::{Key, RuleId, Stats, Violation};
use crate::components::ComponentMap;
use crate::config::Config;
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
    /// Source files of an indexed language that no indexer covered.
    Uncovered,
    /// Files whose types `defines` cannot see.
    NamingUnseen,
    /// Files whose standard library `$std` cannot tell.
    StdUnknown,
    /// Imports that `manifests` could not judge.
    Manifests,
    /// Test code that could not be told from the rest of its file, or a `part` that owns nothing.
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

/// The files no indexer read that one `uncovered_ok` entry accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptedGap {
    /// The globs of the entry.
    pub uncovered_ok: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
    pub files: Vec<String>,
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

/// The `uncovered_ok` entries that accept some of the `unread` files, each with
/// the files it accepts; an entry that accepts none of them is no gap.
pub fn accepted_gaps<'a>(
    config: &Config,
    map: &ComponentMap,
    unread: impl IntoIterator<Item = &'a String>,
) -> Vec<AcceptedGap> {
    let mut by_entry: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for file in unread {
        if let Some(entry) = map.uncovered_ok_entry(file) {
            by_entry.entry(entry).or_default().push(file.clone());
        }
    }
    by_entry
        .into_iter()
        .map(|(entry, mut files)| {
            files.sort();
            let accepted = &config.uncovered_ok[entry];
            AcceptedGap {
                uncovered_ok: accepted
                    .patterns()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                because: accepted.because().map(str::to_string),
                files,
            }
        })
        .collect()
}

impl EvidenceGap {
    /// A gap known only by what its message says.
    fn by_message(kind: GapKind, message: &str, close: &str) -> Self {
        EvidenceGap {
            kind,
            files: Vec::new(),
            extensions: Vec::new(),
            count: None,
            message: message.to_string(),
            close: close.to_string(),
        }
    }

    /// A gap in the files named.
    pub fn of_files(kind: GapKind, files: &[String], message: String, close: &str) -> Self {
        EvidenceGap {
            files: files.to_vec(),
            count: Some(files.len()),
            ..Self::by_message(kind, &message, close)
        }
    }

    /// A gap in every file of the extensions named.
    fn of_extensions(kind: GapKind, extensions: &[String], message: String, close: &str) -> Self {
        EvidenceGap {
            extensions: extensions.to_vec(),
            ..Self::by_message(kind, &message, close)
        }
    }
}

pub const CLOSE_UNCOVERED: &str =
    "add them to a project the indexer reads, `exclude` them, or accept them in `uncovered_ok`";

/// Code a check could not read or judge, in the order the reasons were always given.
pub fn evidence_gaps(stats: &Stats) -> Vec<EvidenceGap> {
    let mut gaps = Vec::new();
    let mut prose = |kind, reasons: &[String], close: &str| {
        gaps.extend(
            reasons
                .iter()
                .map(|r| EvidenceGap::by_message(kind, r, close)),
        );
    };
    prose(
        GapKind::Unused,
        &stats.unused_partial,
        "the files it took as used may be dead: review them by hand, or have an indexer read them",
    );
    prose(
        GapKind::Manifests,
        &stats.manifests_partial,
        "fix or add the manifest the message names, or review those imports by hand",
    );
    prose(
        GapKind::Parts,
        &stats.parts_partial,
        "fix what the message names, or review that code by hand",
    );
    prose(
        GapKind::Packages,
        &stats.packages_partial,
        "fix the manifest the message names, or review those dependencies by hand",
    );
    prose(
        GapKind::Symbols,
        &stats.symbols_partial,
        "do what the message says, or review those uses by hand",
    );
    if !stats.naming_unseen.is_empty() {
        gaps.push(EvidenceGap::of_extensions(
            GapKind::NamingUnseen,
            &stats.naming_unseen,
            format!(
                "`defines` cannot see the types of .{} files, which are read without an index, so their names were not checked",
                stats.naming_unseen.join(", .")
            ),
            "no indexer reads these files, so review the names of their types by hand",
        ));
    }
    if !stats.std_unknown.is_empty() {
        gaps.push(EvidenceGap::of_extensions(
            GapKind::StdUnknown,
            &stats.std_unknown,
            format!(
                "`$std` cannot tell the standard library of .{} files, so their standard imports count as other modules; name those modules instead",
                stats.std_unknown.join(", .")
            ),
            "name the standard modules the rule means instead of `$std`",
        ));
    }
    if !stats.uncovered.is_empty() {
        gaps.push(EvidenceGap::of_files(
            GapKind::Uncovered,
            &stats.uncovered,
            format!(
                "{} source files were not covered by an indexer, so imports in or of them were not checked",
                stats.uncovered.len()
            ),
            CLOSE_UNCOVERED,
        ));
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

/// What a verdict over cached indexes rests on. Not a reason for `partial`: the
/// key holds what decides an index, but not dependencies outside the project.
pub fn cache_note(stats: &Stats) -> Option<String> {
    (stats.from_cache > 0).then(|| {
        format!(
            "{} index(es) came from the cache, keyed on the project files, the indexer and its environment but not on dependencies outside the project; if those changed without a lock file, run with `--no-cache` to index again",
            stats.from_cache
        )
    })
}

pub fn json(
    verdict: &Verdict,
    stats: &Stats,
    coverage: &Coverage,
    changes: Option<&Changes>,
) -> String {
    let report = JsonReport {
        passed: verdict.passed(),
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

/// The plain text report.
pub fn text(
    verdict: &Verdict,
    stats: &Stats,
    coverage: &Coverage,
    changes: Option<&Changes>,
) -> String {
    render(verdict, stats, coverage, changes, false)
}

/// The text report with ANSI styles; the caller strips them where the output
/// is not a terminal.
pub fn colored(
    verdict: &Verdict,
    stats: &Stats,
    coverage: &Coverage,
    changes: Option<&Changes>,
) -> String {
    render(verdict, stats, coverage, changes, true)
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
fn gap_lines(coverage: &Coverage, color: bool) -> String {
    let mut out = String::new();
    if !coverage.evidence_gaps.is_empty() {
        out.push_str(&paint(
            color,
            YELLOW,
            "Evidence gaps (a pass says nothing about this code):",
        ));
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

fn render(
    verdict: &Verdict,
    stats: &Stats,
    coverage: &Coverage,
    changes: Option<&Changes>,
    color: bool,
) -> String {
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
    let gaps = gap_lines(coverage, color);
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
    let status = if verdict.passed() {
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

/// GitHub Actions workflow commands: annotations on the changed files.
pub fn github(
    verdict: &Verdict,
    stats: &Stats,
    coverage: &Coverage,
    changes: Option<&Changes>,
) -> String {
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
    let gaps = coverage
        .evidence_gaps
        .iter()
        .map(|gap| ("warning", gap.message.clone()));
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
        &text(verdict, stats, coverage, changes)
            .lines()
            .last()
            .map_or_else(String::new, |l| format!("{l}\n")),
    );
    out
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
        let out = github(&verdict(), &Stats::default(), &Coverage::default(), None);
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
        let report: serde_json::Value =
            serde_json::from_str(&json(&verdict(), &stats, &Coverage::default(), None)).unwrap();
        let cached = report["cached"].as_str().unwrap();
        assert!(
            cached.contains("2 index(es)") && cached.contains("--no-cache"),
            "{cached}"
        );
        let out = text(&verdict(), &stats, &Coverage::default(), None);
        assert!(out.contains(cached), "{out}");

        let fresh: serde_json::Value = serde_json::from_str(&json(
            &verdict(),
            &Stats::default(),
            &Coverage::default(),
            None,
        ))
        .unwrap();
        assert!(fresh.get("cached").is_none());
        assert!(
            !text(&verdict(), &Stats::default(), &Coverage::default(), None).contains("--no-cache")
        );
    }

    #[test]
    fn an_accepted_gap_leaves_the_verdict_full_and_is_still_reported() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  app: { in: app }
uncovered_ok:
  - { name: [gen/**, tools/**], because: generated }
  - nothing/**
  - tools/a.go
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let unread: Vec<String> = ["tools/a.go", "gen/b.go", "app/c.go"]
            .map(String::from)
            .into();
        // A file two entries accept belongs to the first; an entry that
        // accepts none of the files is no gap.
        let accepted = accepted_gaps(&config, &map, &unread);
        assert_eq!(
            accepted,
            [AcceptedGap {
                uncovered_ok: vec!["gen/**".into(), "tools/**".into()],
                because: Some("generated".into()),
                files: vec!["gen/b.go".into(), "tools/a.go".into()],
            }]
        );
        let coverage = Coverage {
            accepted_gaps: accepted,
            ..Coverage::default()
        };
        assert_eq!(coverage.confidence(), Confidence::Full);
        assert!(coverage.reasons().is_empty());
        let out = text(&verdict(), &Stats::default(), &coverage, None);
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
    fn github_says_why_a_verdict_is_partial_and_when_it_was_cached() {
        let stats = Stats {
            from_cache: 1,
            uncovered: vec!["tools/gen.go".into()],
            ..Stats::default()
        };
        let out = github(
            &verdict(),
            &stats,
            &Coverage::of_check(&stats, vec![], vec![]),
            None,
        );
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
            !github(&verdict(), &Stats::default(), &Coverage::default(), None)
                .contains("::warning")
        );

        // A run narrowed by design is a notice: a warning on every pull request
        // would teach to skip the ones about code nobody read.
        let narrowed = Coverage {
            narrowed: super::narrowed(None, &["web".into()], &[]),
            ..Coverage::default()
        };
        let out = github(&verdict(), &Stats::default(), &narrowed, None);
        assert!(
            out.contains("::notice title=easement partial::the report is limited to component web"),
            "{out}"
        );
        assert!(!out.contains("::warning"), "{out}");
    }
}
