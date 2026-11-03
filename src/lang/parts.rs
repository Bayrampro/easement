//! The parts of a file that a dependency can belong to instead of the file:
//! code a language only builds for its tests, for example.

use std::ops::RangeInclusive;

use regex::Regex;

/// What the core's own rules read of a part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Code that only a test build has: `tests: auto` takes it.
    Test,
}

/// A part of a file, as the language that found it names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Part {
    pub name: String,
    pub kind: Option<Kind>,
}

/// The lines of a file (1-based, both ends included) that a part holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub lines: RangeInclusive<usize>,
    pub part: Part,
}

/// The `#[cfg(test)]` items of a Rust source: the attribute, then the item up
/// to its closing brace (or its `;`). `mod tests;` is no part: its code is in
/// another file.
pub fn rust(source: &str) -> Found {
    let masked = mask_rust(source.as_bytes());
    let Ok(text) = std::str::from_utf8(&masked) else {
        return Found {
            spans: Vec::new(),
            doubts: vec![Doubt::Unreadable],
        };
    };
    let attribute = Regex::new(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]").expect("valid regex");
    let whole_file = Regex::new(r"#!\[\s*cfg\s*\(\s*test\s*\)\s*\]").expect("valid regex");
    let mut spans: Vec<Span> = Vec::new();
    let mut doubts: Vec<Doubt> = Vec::new();
    if whole_file.is_match(text) {
        doubts.push(Doubt::WholeFile);
    }
    for found in attribute.find_iter(text) {
        // An item nested in a part that was found already is part of it.
        if spans
            .last()
            .is_some_and(|last| *last.lines.end() >= line_of(&masked, found.start()))
        {
            continue;
        }
        match item_end(&masked, found.end()) {
            Item::Ends(end) => spans.push(Span {
                lines: line_of(&masked, found.start())..=line_of(&masked, end),
                part: Part {
                    name: "cfg(test)".to_string(),
                    kind: Some(Kind::Test),
                },
            }),
            Item::Module => doubts.push(Doubt::ModuleInAnotherFile),
            Item::Unbalanced => doubts.push(Doubt::Unbalanced),
            Item::Member => {}
        }
    }
    doubts.dedup();
    Found { spans, doubts }
}

/// What a finder found in a source, and what it could not tell.
#[derive(Debug, Default)]
pub struct Found {
    pub spans: Vec<Span>,
    pub doubts: Vec<Doubt>,
}

/// Why test code of a file may be judged as the file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Doubt {
    /// The source is not valid UTF-8.
    Unreadable,
    /// An item whose end was not found (the braces do not match).
    Unbalanced,
    /// `#[cfg(test)] mod name;`: the code is in another file.
    ModuleInAnotherFile,
    /// `#![cfg(test)]`: the whole file is test code.
    WholeFile,
}

impl Doubt {
    /// The line of the report for the `files` that share it.
    pub fn reason(self, files: &str) -> String {
        match self {
            Doubt::Unreadable => format!(
                "{files}: not valid UTF-8, so their `#[cfg(test)]` code is judged as the file's, not as `tests`"
            ),
            Doubt::Unbalanced => format!(
                "{files}: easement could not find the end of a `#[cfg(test)]` item (the braces do not match), so it is judged as the file's, not as `tests`"
            ),
            Doubt::ModuleInAnotherFile => format!(
                "{files}: `#[cfg(test)] mod name;` names a file that is judged as its component's, not as `tests`; put the test code in the file itself to have it judged as `tests`"
            ),
            Doubt::WholeFile => format!(
                "{files}: `#![cfg(test)]` makes the whole file test code, but it is judged as its component's, not as `tests`"
            ),
        }
    }
}

/// How the item after a `#[cfg(test)]` attribute ends.
enum Item {
    Ends(usize),
    /// `mod name;`
    Module,
    /// A field, an argument, an arm: no item an attribute governs as a part.
    Member,
    Unbalanced,
}

/// `source` with the inside of comments, strings and characters blanked, so
/// that what is left is code: same length, same lines.
fn mask_rust(source: &[u8]) -> Vec<u8> {
    let mut out = source.to_vec();
    let blank = |out: &mut Vec<u8>, range: std::ops::Range<usize>| {
        for byte in &mut out[range] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < source.len() {
        match source[i] {
            b'/' if source.get(i + 1) == Some(&b'/') => {
                let end = source[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(source.len(), |n| i + n);
                blank(&mut out, i..end);
                i = end;
            }
            b'/' if source.get(i + 1) == Some(&b'*') => {
                let mut depth = 0;
                let mut end = i;
                while end < source.len() {
                    if source[end..].starts_with(b"/*") {
                        depth += 1;
                        end += 2;
                    } else if source[end..].starts_with(b"*/") {
                        depth -= 1;
                        end += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        end += 1;
                    }
                }
                blank(&mut out, i..end);
                i = end;
            }
            b'"' => {
                let end = string_end(source, i + 1);
                blank(&mut out, i + 1..end.saturating_sub(1).max(i + 1));
                i = end;
            }
            b'r' if is_raw_string(source, i) => {
                let hashes = source[i + 1..].iter().take_while(|&&b| b == b'#').count();
                let close: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                let start = i + 2 + hashes;
                let end = source[start..]
                    .windows(close.len())
                    .position(|w| w == close)
                    .map_or(source.len(), |n| start + n + close.len());
                blank(&mut out, start..end.saturating_sub(close.len()).max(start));
                i = end;
            }
            b'\'' => match char_end(source, i) {
                Some(end) => {
                    blank(&mut out, i + 1..end - 1);
                    i = end;
                }
                // A lifetime.
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    out
}

/// Whether the `r` at `at` opens a raw string (`r"`, `r#"`), not ends a name.
fn is_raw_string(source: &[u8], at: usize) -> bool {
    let is_word = |i: usize| source[i].is_ascii_alphanumeric() || source[i] == b'_';
    // `br"..."` is a raw byte string: the `b` is no part of a longer name.
    let before = match at.checked_sub(1) {
        Some(b) if source[b] == b'b' => b,
        _ => at,
    };
    let word_before = before > 0 && is_word(before - 1);
    let hashes = source[at + 1..].iter().take_while(|&&b| b == b'#').count();
    !word_before && source.get(at + 1 + hashes) == Some(&b'"')
}

/// The index after the string that starts at `from` (just past its opening quote).
fn string_end(source: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < source.len() {
        match source[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    source.len()
}

/// The index after the character literal at `at`, `None` for a lifetime.
fn char_end(source: &[u8], at: usize) -> Option<usize> {
    if source.get(at + 1) == Some(&b'\\') {
        // The escaped character comes first: `'\''`, `'\n'`, `'\u{7d}'`.
        let close = source.get(at + 3..)?.iter().position(|&b| b == b'\'')?;
        return Some(at + 3 + close + 1);
    }
    let rest = &source[at + 1..];
    let width = match rest.first()? {
        byte if *byte < 0x80 => 1,
        byte if byte >> 5 == 0b110 => 2,
        byte if byte >> 4 == 0b1110 => 3,
        _ => 4,
    };
    (rest.get(width) == Some(&b'\'')).then_some(at + 1 + width + 1)
}

/// The 1-based line of the byte at `at`.
fn line_of(source: &[u8], at: usize) -> usize {
    1 + source[..at.min(source.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

/// The byte at which the item after the attribute that ends at `from` ends:
/// its closing brace, or its `;`.
fn item_end(code: &[u8], from: usize) -> Item {
    let mut i = from;
    // Skip the other attributes of the item.
    loop {
        while code.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if !code[i..].starts_with(b"#[") {
            break;
        }
        let mut depth = 0;
        while i < code.len() {
            match code[i] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    let item = i;
    if !starts_item(&code[item..]) {
        return Item::Member;
    }
    let mut nesting = 0;
    while i < code.len() {
        match code[i] {
            b'(' | b'[' => nesting += 1,
            b')' | b']' => nesting -= 1,
            b';' if nesting == 0 => {
                return if is_module(&code[item..i]) {
                    Item::Module
                } else {
                    Item::Ends(i)
                };
            }
            b'}' if nesting == 0 => return Item::Unbalanced,
            b'{' if nesting == 0 => {
                let mut depth = 0;
                while i < code.len() {
                    match code[i] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Item::Ends(i);
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                return Item::Unbalanced;
            }
            _ => {}
        }
        i += 1;
    }
    Item::Unbalanced
}

/// Whether `code` starts an item, after its visibility and qualifiers, and not a field,
/// a match arm or an argument that an attribute can sit on too.
fn starts_item(code: &[u8]) -> bool {
    const KEYWORDS: [&str; 17] = [
        "mod",
        "fn",
        "use",
        "struct",
        "enum",
        "trait",
        "impl",
        "const",
        "static",
        "type",
        "union",
        "extern",
        "unsafe",
        "async",
        "macro_rules",
        "pub",
        "default",
    ];
    let text = String::from_utf8_lossy(&code[..code.len().min(64)]);
    let first = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or_default();
    KEYWORDS.contains(&first)
}

/// Whether the text of an item up to its `;` is `mod name`, with a visibility.
fn is_module(item: &[u8]) -> bool {
    let text = String::from_utf8_lossy(item);
    let words: Vec<&str> = text.split_whitespace().collect();
    let keyword = words.iter().position(|w| *w == "mod");
    keyword.is_some_and(|at| words[..at].iter().all(|w| w.starts_with("pub")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<RangeInclusive<usize>> {
        rust(source)
            .spans
            .into_iter()
            .map(|span| span.lines)
            .collect()
    }

    #[test]
    fn a_cfg_test_module_is_a_test_part_from_its_attribute_to_its_closing_brace() {
        let source = "pub fn run() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n\npub fn after() {}\n";
        let spans = rust(source).spans;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].lines, 3..=6);
        assert_eq!(spans[0].part.name, "cfg(test)");
        assert_eq!(spans[0].part.kind, Some(Kind::Test));
    }

    #[test]
    fn nested_braces_do_not_end_the_module_early() {
        let source = "#[cfg(test)]\nmod tests {\n    mod inner {\n        fn a() { if true { } }\n    }\n    fn b() {}\n}\nfn after() {}\n";
        assert_eq!(lines(source), [1..=7]);
    }

    #[test]
    fn braces_in_strings_comments_and_chars_do_not_count() {
        let source = concat!(
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    // }\n",
            "    /* } */\n",
            "    const A: &str = \"}\";\n",
            "    const B: &str = r#\"}\"#;\n",
            "    const C: char = '}';\n",
            "    fn d<'a>(x: &'a str) {}\n",
            "}\n",
            "fn after() {}\n",
        );
        assert_eq!(lines(source), [1..=9]);
    }

    #[test]
    fn a_cfg_test_function_and_a_cfg_test_use_are_parts_too() {
        let source =
            "#[cfg(test)]\nuse testkit::tablet;\n\n#[cfg(test)]\nfn helper() {\n}\nfn run() {}\n";
        assert_eq!(lines(source), [1..=2, 4..=6]);
    }

    #[test]
    fn other_attributes_between_the_cfg_and_the_item_stay_in_the_part() {
        let source = "#[cfg(test)]\n#[allow(dead_code)]\nmod tests {\n}\n";
        assert_eq!(lines(source), [1..=4]);
    }

    #[test]
    fn only_a_plain_cfg_test_is_a_part() {
        let source = "#[cfg(not(test))]\nmod real {\n}\n#[cfg(feature = \"x\")]\nmod x {\n}\n#[cfg( test )]\nmod spaced {\n}\n";
        assert_eq!(lines(source), [7..=9]);
    }

    #[test]
    fn a_cfg_test_module_in_another_file_is_no_part_of_this_one() {
        assert!(lines("#[cfg(test)]\nmod tests;\nfn run() {}\n").is_empty());
    }

    #[test]
    fn raw_byte_strings_and_escaped_quote_chars_hide_their_braces() {
        let source = "#[cfg(test)]\nmod tests {\n    const A: &[u8] = br#\"} \\\"#;\n    const B: char = '\\'';\n    const C: char = '}';\n}\nfn after() {}\n";
        assert_eq!(lines(source), [1..=6]);
    }

    #[test]
    fn an_attribute_on_something_that_is_no_item_is_no_part() {
        let source = "struct S {\n    #[cfg(test)]\n    field: u8,\n    other: u8,\n}\nfn run() { let x = 1; }\n";
        assert!(lines(source).is_empty());
    }

    #[test]
    fn a_visibility_or_a_qualifier_before_the_keyword_still_makes_an_item() {
        let source =
            "#[cfg(test)]\npub(crate) fn a() {\n}\n#[cfg(test)]\nunsafe impl Send for S {\n}\n";
        assert_eq!(lines(source), [1..=3, 4..=6]);
    }

    #[test]
    fn what_the_finder_could_not_tell_it_says() {
        assert_eq!(
            rust("#[cfg(test)]\nmod tests {\n    fn a() {\n}\n").doubts,
            [Doubt::Unbalanced]
        );
        assert_eq!(
            rust("#[cfg(test)]\nmod tests;\n").doubts,
            [Doubt::ModuleInAnotherFile]
        );
        assert_eq!(
            rust("#![cfg(test)]\nfn a() {}\n").doubts,
            [Doubt::WholeFile]
        );
        assert!(rust("#[cfg(test)]\nmod tests {\n}\n").doubts.is_empty());
        assert!(
            rust("struct S {\n    #[cfg(test)]\n    f: u8,\n}\n")
                .doubts
                .is_empty()
        );
    }
}
