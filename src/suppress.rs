use crate::check::{RuleId, Violation};

const NEXT_LINE: &str = "easement:disable-next-line";
const SAME_LINE: &str = "easement:disable-line";

/// An inline exception, written in a comment of any language:
///
/// ```text
/// // easement:disable-next-line dependency -- legacy admin export, remove with #123
/// import { exportAll } from "../admin/export";
/// import { x } from "../admin/x"; // easement:disable-line privacy -- generated client
/// ```
#[derive(Debug, Clone)]
pub struct Suppression {
    pub file: String,
    /// Line of the comment.
    pub line: usize,
    /// Line whose violations it suppresses.
    pub target_line: usize,
    pub rules: Vec<String>,
    pub reason: Option<String>,
}

/// What opens a comment in the languages easement reads.
const OPENERS: [&str; 4] = ["//", "/*", "#", "<!--"];

/// A suppression is a comment that begins with the directive. Text in a string
/// literal or later in a comment (an example in a doc comment) is not one.
pub fn parse(file: &str, source: &str) -> Vec<Suppression> {
    let mut found = Vec::new();
    for (i, text) in source.lines().enumerate() {
        let line = i + 1;
        let Some(comment) = directive(text) else {
            continue;
        };
        let (rest, target_line) = if let Some(rest) = comment.strip_prefix(NEXT_LINE) {
            (rest, line + 1)
        } else if let Some(rest) = comment.strip_prefix(SAME_LINE) {
            (rest, line)
        } else {
            continue;
        };
        // Drop a closing block comment: `/* easement:disable-line dependency -- why */`.
        let rest = rest
            .trim()
            .trim_end_matches("*/")
            .trim_end_matches("-->")
            .trim();
        let (rules, reason) = match rest.split_once("--") {
            Some((rules, reason)) => (rules, Some(reason.trim())),
            None => (rest, None),
        };
        found.push(Suppression {
            file: file.to_string(),
            line,
            target_line,
            rules: rules
                .split([',', ' '])
                .filter(|r| !r.is_empty())
                .map(str::to_string)
                .collect(),
            reason: reason.filter(|r| !r.is_empty()).map(str::to_string),
        });
    }
    found
}

/// The text of the first comment on the line, from its first word, when it
/// starts with `easement:`. A line inside a block comment (` * …`) is a comment.
fn directive(text: &str) -> Option<&str> {
    if let Some(rest) = text.trim_start().strip_prefix('*')
        && let Some(found) = word(rest)
    {
        return Some(found);
    }
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        let rest = &text[at..];
        // A quote that closes on this line opens a string; one that does not
        // (a Rust lifetime, an apostrophe) is a plain character.
        if matches!(c, '"' | '\'' | '`')
            && let Some(end) = closing(&rest[1..], c)
        {
            at += 1 + end + 1;
            continue;
        }
        if let Some(opener) = OPENERS.iter().find(|o| rest.starts_with(**o)) {
            return word(&rest[opener.len()..]);
        }
        at += c.len_utf8();
    }
    None
}

/// The comment after its opener, when its first word is a directive.
fn word(rest: &str) -> Option<&str> {
    let rest = rest.trim_start_matches(['/', '*', '!']).trim_start();
    rest.starts_with("easement:").then_some(rest)
}

/// The byte offset of the unescaped `quote` that closes a string.
fn closing(text: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            _ if c == quote => return Some(at),
            _ => {}
        }
    }
    None
}

/// Removes suppressed violations. Suppressions without a reason, naming an
/// unknown rule or matching nothing become violations themselves, so an
/// exception is always explained and never outlives the code it excused.
pub fn apply(violations: Vec<Violation>, suppressions: &[Suppression]) -> (Vec<Violation>, usize) {
    let mut used = vec![false; suppressions.len()];
    let mut kept = Vec::new();
    let mut suppressed = 0;
    for v in violations {
        let hit = suppressions.iter().position(|s| {
            s.reason.is_some()
                && s.file == v.file
                && s.target_line == v.line
                && s.rules.iter().any(|r| r == v.rule.as_str())
        });
        match hit {
            Some(i) => {
                used[i] = true;
                suppressed += 1;
            }
            None => kept.push(v),
        }
    }
    for (s, used) in suppressions.iter().zip(used) {
        let problem = if s.reason.is_none() {
            Some((
                RuleId::SuppressionReason,
                format!(
                    "suppression of {} needs a reason after `--`",
                    s.rules.join(",")
                ),
            ))
        } else if let Some(bad) = s.rules.iter().find(|r| RuleId::suppressible(r).is_none()) {
            Some((
                RuleId::UnusedSuppression,
                format!(
                    "`{bad}` is not a rule that can be suppressed (use {})",
                    RuleId::SUPPRESSIBLE.map(RuleId::as_str).join(", ")
                ),
            ))
        } else if !used {
            Some((
                RuleId::UnusedSuppression,
                format!(
                    "suppression of {} matches no violation on line {}; remove it",
                    s.rules.join(","),
                    s.target_line
                ),
            ))
        } else {
            None
        };
        if let Some((rule, message)) = problem {
            kept.push(Violation {
                rule,
                file: s.file.clone(),
                line: s.line,
                message,
                because: None,
                from: String::new(),
                to: String::new(),
                // The comment line is the identity: there is no import to key it by.
                target: format!("line {}", s.line),
                specifier: String::new(),
                evidence: vec![(s.file.clone(), s.line)],
            });
        }
    }
    (kept, suppressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comment_styles() {
        // One literal per line: a directive inside a string is not a suppression.
        let src = concat!(
            "// easement:disable-next-line dependency -- legacy export\n",
            "import x from './x'; // easement:disable-line dependency, privacy -- generated\n",
            "# easement:disable-next-line privacy\n",
            "/* easement:disable-line privacy -- why */\n",
        );
        let got = parse("a.ts", src);
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].target_line, 2);
        assert_eq!(got[0].reason.as_deref(), Some("legacy export"));
        assert_eq!(got[1].target_line, 2);
        assert_eq!(got[1].rules, ["dependency", "privacy"]);
        assert_eq!(got[2].reason, None);
        assert_eq!(got[3].rules, ["privacy"]);
        assert_eq!(got[3].reason.as_deref(), Some("why"));
    }

    #[test]
    fn a_directive_opens_the_first_comment_of_its_line() {
        let found = |line: &str| parse("a", line).len();
        for line in [
            r#"let url = "http://x/#a"; // easement:disable-line privacy -- why"#,
            "fn f<'a>(x: &'a str) {} // easement:disable-line dependency -- why",
            "x = 'a#b'  # easement:disable-line dependency -- why",
            " * easement:disable-next-line dependency -- in a doc block",
            "<!-- easement:disable-line dependency -- why -->",
            "//easement:disable-line dependency -- no space",
        ] {
            assert_eq!(found(line), 1, "{line}");
        }
        for line in [
            r#"const NEXT: &str = "easement:disable-next-line";"#,
            r#"let s = "// easement:disable-next-line dependency -- in a string";"#,
            "/// // easement:disable-next-line dependency -- an example in a doc comment",
            "// see easement:disable-line for the syntax",
            "x = 1 # note // easement:disable-line dependency -- not first",
        ] {
            assert_eq!(found(line), 0, "{line}");
        }
    }
}
