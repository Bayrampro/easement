use anstyle::{AnsiColor, Style};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;

use crate::baseline::Verdict;
use crate::check::{Key, RuleId, Stats, Violation};
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
    confidence: &'static str,
    confidence_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<DiffInfo<'a>>,
    stats: &'a Stats,
}

#[derive(Serialize)]
struct DiffInfo<'a> {
    base: &'a str,
    merge_base: &'a str,
}

/// Why a verdict is only as good as the code the check could read.
fn confidence_reasons(stats: &Stats, changes: Option<&Changes>) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(changes) = changes {
        reasons.push(format!(
            "only violations on lines changed since {} were judged, the rest of the project was not",
            changes.base
        ));
    }
    if !stats.limited_to.is_empty() {
        reasons.push(format!(
            "the report is limited to {}, the rest of the project was not judged",
            stats.limited_to.join(", ")
        ));
    }
    if !stats.std_unknown.is_empty() {
        reasons.push(format!(
            "`$std` cannot tell the standard library of .{} files, so their standard imports count as other modules; name those modules instead",
            stats.std_unknown.join(", .")
        ));
    }
    if !stats.uncovered.is_empty() {
        reasons.push(format!(
            "{} source files were not covered by an indexer, so imports in or of them were not checked",
            stats.uncovered.len()
        ));
    }
    reasons
}

pub fn json(verdict: &Verdict, stats: &Stats, changes: Option<&Changes>) -> String {
    let reasons = confidence_reasons(stats, changes);
    let report = JsonReport {
        passed: verdict.passed(),
        violations: &verdict.new,
        warnings: &verdict.warnings,
        owners: &verdict.owners,
        stale: &verdict.stale,
        legalized: verdict.legalized,
        confidence: if reasons.is_empty() {
            "full"
        } else {
            "partial"
        },
        confidence_reasons: reasons,
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
pub fn text(verdict: &Verdict, stats: &Stats, changes: Option<&Changes>) -> String {
    render(verdict, stats, changes, false)
}

/// The text report with ANSI styles; the caller strips them where the output
/// is not a terminal.
pub fn colored(verdict: &Verdict, stats: &Stats, changes: Option<&Changes>) -> String {
    render(verdict, stats, changes, true)
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

fn render(verdict: &Verdict, stats: &Stats, changes: Option<&Changes>, color: bool) -> String {
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
            verdict.owners.get(&v.from).map(String::as_str),
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
                verdict.owners.get(&v.from).map(String::as_str),
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
    if !stats.uncovered.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&paint(
            color,
            YELLOW,
            "Not covered by an indexer (imports in or of these files were not checked; add them to a project the indexer reads, `exclude` them, or accept them in `uncovered_ok`):",
        ));
        out.push('\n');
        for file in stats.uncovered.iter().take(UNCOVERED_SHOWN) {
            out.push_str(&format!("  {file}\n"));
        }
        if stats.uncovered.len() > UNCOVERED_SHOWN {
            out.push_str(&format!(
                "  … and {} more (all of them in `--format json`, `stats.uncovered`)\n",
                stats.uncovered.len() - UNCOVERED_SHOWN
            ));
        }
    }
    if !stats.std_unknown.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&paint(
            color,
            YELLOW,
            &format!(
                "`$std` cannot tell the standard library of .{} files: their standard imports count as other modules; name those modules instead",
                stats.std_unknown.join(", .")
            ),
        ));
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
pub fn github(verdict: &Verdict, stats: &Stats) -> String {
    let mut out = String::new();
    for (level, v) in verdict.reported() {
        let mut text = match &v.because {
            Some(because) => format!("{} (because: {because})", v.message),
            None => v.message.clone(),
        };
        if let Some(owner) = verdict.owners.get(&v.from) {
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
    out.push_str(
        &text(verdict, stats, None)
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
        let out = github(&verdict(), &Stats::default());
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
}
