//! The parts of a file that a dependency can belong to instead of the file:
//! code a language only builds for its tests, for example.

use std::ops::RangeInclusive;
use std::sync::LazyLock;

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

impl Span {
    /// Test-only code on `lines`: `1..=usize::MAX` for a whole file.
    pub fn test(lines: RangeInclusive<usize>) -> Self {
        Self {
            lines,
            part: Part {
                name: "cfg(test)".to_string(),
                kind: Some(Kind::Test),
            },
        }
    }
}

/// The test-only items of a Rust source: the attribute, then the item up to its closing
/// brace (or its `;`), and the whole file for `#![cfg(test)]`. `mod tests;` is no part
/// of this file: its code is in another, which `files` names.
pub fn rust(path: &str, source: &str) -> Found {
    let masked = mask_rust(source.as_bytes());
    let Ok(text) = std::str::from_utf8(&masked) else {
        return Found {
            doubts: vec![Doubt::Unreadable],
            ..Found::default()
        };
    };
    static ATTRIBUTE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"#(!?)\[\s*cfg\s*\(").expect("valid regex"));
    let mut found = Found::default();
    // The bytes of the test items, `spans` as the code they cover.
    let mut test_code: Vec<RangeInclusive<usize>> = Vec::new();
    for attribute in ATTRIBUTE.captures_iter(text) {
        let whole = attribute.get(0).expect("the whole match");
        let Some((content, attribute_end)) = cfg_attribute(text, whole.end()) else {
            continue;
        };
        if !only_a_test_build(content) {
            continue;
        }
        let start = whole.start();
        // What sits inside an inline module cannot be placed from this file alone.
        let inside_module = brace_depth(&masked[..start]) > 0;
        // `#![cfg(test)]`: the file is test code from its first line to its last.
        if !attribute[1].is_empty() {
            if inside_module {
                found.doubts.push(Doubt::InsideModule);
            } else {
                found
                    .spans
                    .push(Span::test(1..=text.lines().count().max(1)));
                test_code.push(0..=masked.len());
            }
            continue;
        }
        // An item nested in a part that was found already is part of it.
        if test_code.last().is_some_and(|last| last.contains(&start)) {
            continue;
        }
        match item_end(&masked, attribute_end) {
            Item::Ends(end) => {
                found
                    .spans
                    .push(Span::test(line_of(&masked, start)..=line_of(&masked, end)));
                test_code.push(start..=end);
            }
            Item::Module(name) => {
                let module = place(path, &name, inside_module, &attributes_before(text, start));
                found.test_module(module);
            }
            Item::ModulePath => found.doubts.push(Doubt::ModulePath),
            Item::Unbalanced => found.doubts.push(Doubt::Unbalanced),
            Item::Member => {}
        }
    }
    other_modules(path, text, &masked, &test_code, &mut found);
    found.doubts.dedup();
    found
}

/// The `mod name;` of a file that no `#[cfg(test)]` governs. Those in test code are
/// test code too; the others are kept in `modules`, to be followed when the file turns
/// out to be test code as a whole.
fn other_modules(
    path: &str,
    text: &str,
    masked: &[u8],
    test_code: &[RangeInclusive<usize>],
    found: &mut Found,
) {
    static DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(?:r#)?[A-Za-z_][A-Za-z0-9_]*\s*;")
            .expect("valid regex")
    });
    for declaration in DECLARATION.find_iter(text) {
        let start = declaration.start();
        let in_test_code = test_code.iter().any(|item| item.contains(&start));
        let attributes = attributes_before(text, start);
        // `#[cfg(test)] mod name;` outside test code is the attribute loop's.
        if !in_test_code && attributes.iter().any(|a| is_test_cfg(a)) {
            continue;
        }
        let Some(name) = module_name(&masked[start..declaration.end() - 1]) else {
            continue;
        };
        let inside_module = brace_depth(&masked[..start]) > 0;
        let module = place(path, &name, inside_module, &attributes);
        if in_test_code {
            found.test_module(module);
        } else {
            found.modules.push(module);
        }
    }
}

/// Where the code of the module `name`, declared in `path` after `attributes`, is.
fn place(path: &str, name: &str, inside_module: bool, attributes: &[&str]) -> Module {
    if inside_module {
        Module::Unplaced(Doubt::InsideModule)
    } else if attributes.iter().any(|a| is_path(a)) {
        Module::Unplaced(Doubt::ModulePath)
    } else {
        Module::Files(module_files(path, name))
    }
}

/// The contents of the outer attributes (`#[...]`) right before the item at `start`.
fn attributes_before(text: &str, start: usize) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut attributes = Vec::new();
    let mut end = text[..start].trim_end().len();
    while end > 0 && bytes[end - 1] == b']' {
        let mut depth = 0;
        let Some(open) = (0..end).rev().find(|&i| {
            match bytes[i] {
                b']' => depth += 1,
                b'[' => depth -= 1,
                _ => {}
            }
            depth == 0
        }) else {
            break;
        };
        if open == 0 || bytes[open - 1] != b'#' {
            break;
        }
        attributes.push(&text[open + 1..end - 1]);
        end = text[..open - 1].trim_end().len();
    }
    attributes
}

/// Whether the content of an attribute is a `cfg` that only a test build satisfies.
fn is_test_cfg(attribute: &str) -> bool {
    attribute
        .trim()
        .strip_prefix("cfg")
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(only_a_test_build)
}

/// Whether the content of an attribute is a `path = "..."`.
fn is_path(attribute: &str) -> bool {
    attribute.trim_start().starts_with("path")
}

/// How many `{` of the code before a point are still open.
fn brace_depth(code: &[u8]) -> usize {
    code.iter().fold(0_usize, |depth, byte| match byte {
        b'{' => depth + 1,
        b'}' => depth.saturating_sub(1),
        _ => depth,
    })
}

/// The files the module `name` declared in the Rust file `path` may be in: a `mod.rs`,
/// `lib.rs` or `main.rs` owns its directory, any other file one named after it.
fn module_files(path: &str, name: &str) -> Vec<String> {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let base = match file {
        "mod.rs" | "lib.rs" | "main.rs" => dir.to_string(),
        other => {
            let stem = other.strip_suffix(".rs").unwrap_or(other);
            if dir.is_empty() {
                stem.to_string()
            } else {
                format!("{dir}/{stem}")
            }
        }
    };
    let join = |rest: String| {
        if base.is_empty() {
            rest
        } else {
            format!("{base}/{rest}")
        }
    };
    vec![join(format!("{name}.rs")), join(format!("{name}/mod.rs"))]
}

/// The content of the `cfg(...)` whose parentheses open just before `from`, and the byte
/// after the `]` that closes the attribute; `None` when it does not end as one.
fn cfg_attribute(text: &str, from: usize) -> Option<(&str, usize)> {
    let mut depth = 1;
    let mut end = None;
    for (offset, byte) in text.as_bytes()[from..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(from + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let rest = &text[end + 1..];
    let after = rest.trim_start();
    after
        .starts_with(']')
        .then(|| (&text[from..end], end + 1 + (rest.len() - after.len()) + 1))
}

/// Whether a `cfg` predicate holds only in a test build: `test`, or `all(…)` with `test`
/// among its arguments. `any` and `not` do not.
fn only_a_test_build(predicate: &str) -> bool {
    let predicate = predicate.trim();
    if predicate == "test" {
        return true;
    }
    let Some(arguments) = predicate
        .strip_prefix("all")
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let mut depth = 0;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in arguments.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&arguments[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&arguments[start..]);
    parts.iter().any(|argument| argument.trim() == "test")
}

/// What a finder found in a source, and what it could not tell.
#[derive(Debug, Default)]
pub struct Found {
    pub spans: Vec<Span>,
    pub doubts: Vec<Doubt>,
    /// The test-only `mod name;` (one `#[cfg(test)]` governs, or one in test code): for
    /// each, the files its code may be in, of which the one that exists is test code as
    /// a whole.
    pub files: Vec<Vec<String>>,
    /// The other `mod name;` of the file, test code as well when the file is.
    pub modules: Vec<Module>,
}

impl Found {
    /// A test-only `mod name;`: its file is test code as a whole, or it is a doubt.
    fn test_module(&mut self, module: Module) {
        match module {
            Module::Files(candidates) => self.files.push(candidates),
            Module::Unplaced(doubt) => self.doubts.push(doubt),
        }
    }
}

/// A `mod name;` whose code is in another file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Module {
    /// The files its code may be in.
    Files(Vec<String>),
    /// Where its file is cannot be told from the declaring file alone.
    Unplaced(Doubt),
}

/// Why test code of a file may be judged as the file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Doubt {
    /// The source is not valid UTF-8.
    Unreadable,
    /// An item whose end was not found (the braces do not match).
    Unbalanced,
    /// A test-only `mod name;` names a file the project does not have.
    ModuleInAnotherFile,
    /// An inner `#![cfg(test)]`, or a test-only `mod name;`, inside an inline module.
    InsideModule,
    /// A `#[path]` puts the file of a test-only `mod name;` somewhere else.
    ModulePath,
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
            Doubt::InsideModule => format!(
                "{files}: a `#![cfg(test)]`, or a test-only `mod name;`, inside an inline module is not placed, so it is judged as the file's, not as `tests`"
            ),
            Doubt::ModulePath => format!(
                "{files}: a `#[path]` puts the file of a test-only `mod name;` somewhere easement does not look, so it is judged as its component's, not as `tests`"
            ),
            Doubt::ModuleInAnotherFile => format!(
                "{files}: a test-only `mod name;` names a file that is not in the project, so it is not judged as `tests`; the file is expected next to the module or under a directory of its name"
            ),
        }
    }
}

/// How the item after a `#[cfg(test)]` attribute ends.
enum Item {
    Ends(usize),
    /// `mod name;`
    Module(String),
    /// `#[path = "..."] mod name;`: its file is not where the name says.
    ModulePath,
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
    let mut path_attribute = false;
    // Skip the other attributes of the item.
    loop {
        while code.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if !code[i..].starts_with(b"#[") {
            break;
        }
        path_attribute |= code[i + 2..]
            .iter()
            .skip_while(|b| b.is_ascii_whitespace())
            .take(4)
            .copied()
            .eq(*b"path");
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
                return match module_name(&code[item..i]) {
                    Some(_) if path_attribute => Item::ModulePath,
                    Some(name) => Item::Module(name),
                    None => Item::Ends(i),
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

/// The name of the module, when the text of an item up to its `;` is `mod name`, with a
/// visibility.
fn module_name(item: &[u8]) -> Option<String> {
    static GROUP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\([^)]*\)").expect("valid regex"));
    let text = String::from_utf8_lossy(item);
    // `pub(crate)` and `pub(in crate::a)` are one word of visibility.
    let text = GROUP.replace_all(&text, " ");
    let words: Vec<&str> = text.split_whitespace().collect();
    let keyword = words.iter().position(|w| *w == "mod")?;
    words[..keyword]
        .iter()
        .all(|word| *word == "pub")
        .then(|| words.get(keyword + 1))
        .flatten()
        .map(|name| name.trim_start_matches("r#").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<RangeInclusive<usize>> {
        rust("src/lib.rs", source)
            .spans
            .into_iter()
            .map(|span| span.lines)
            .collect()
    }

    #[test]
    fn a_cfg_test_module_is_a_test_part_from_its_attribute_to_its_closing_brace() {
        let source = "pub fn run() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n\npub fn after() {}\n";
        let spans = rust("src/lib.rs", source).spans;
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
            rust("src/lib.rs", "#[cfg(test)]\nmod tests {\n    fn a() {\n}\n").doubts,
            [Doubt::Unbalanced]
        );
        assert!(
            rust("src/lib.rs", "#[cfg(test)]\nmod tests {\n}\n")
                .doubts
                .is_empty()
        );
        assert!(
            rust(
                "src/lib.rs",
                "struct S {\n    #[cfg(test)]\n    f: u8,\n}\n"
            )
            .doubts
            .is_empty()
        );
    }

    #[test]
    fn a_cfg_test_module_in_another_file_names_the_files_it_may_be_in() {
        let found = rust(
            "src/lib.rs",
            "#[cfg(test)]\nmod tests;\n\n#[cfg(test)]\npub(crate) mod helpers;\nmod real;\n",
        );
        assert_eq!(
            found.files,
            [
                ["src/tests.rs", "src/tests/mod.rs"],
                ["src/helpers.rs", "src/helpers/mod.rs"]
            ]
        );
        assert!(found.spans.is_empty() && found.doubts.is_empty());
    }

    #[test]
    fn the_other_modules_of_a_file_are_followed_only_if_it_is_test_code() {
        let found = rust(
            "src/store/db.rs",
            "mod a;\npub(crate) mod b;\n#[cfg(test)]\nmod t;\n#[path = \"x.rs\"]\n#[allow(dead_code)]\nmod moved;\nmod inline {\n    mod c;\n}\n",
        );
        assert_eq!(
            found.modules,
            [
                Module::Files(vec![
                    "src/store/db/a.rs".into(),
                    "src/store/db/a/mod.rs".into()
                ]),
                Module::Files(vec![
                    "src/store/db/b.rs".into(),
                    "src/store/db/b/mod.rs".into()
                ]),
                Module::Unplaced(Doubt::ModulePath),
                Module::Unplaced(Doubt::InsideModule),
            ]
        );
        // `mod t;` is test code whatever its file is: it is in `files`, not here.
        assert_eq!(
            found.files,
            [["src/store/db/t.rs", "src/store/db/t/mod.rs"]]
        );
        assert!(found.doubts.is_empty());
    }

    #[test]
    fn the_modules_of_test_code_are_test_code() {
        // A `#![cfg(test)]` file: what it declares is test-only as well.
        let found = rust(
            "src/lib.rs",
            "#![cfg(test)]\nmod a;\n#[path = \"x.rs\"]\nmod b;\nmod inline {\n    mod c;\n}\n",
        );
        assert_eq!(found.files, [["src/a.rs", "src/a/mod.rs"]]);
        assert_eq!(found.doubts, [Doubt::ModulePath, Doubt::InsideModule]);
        assert!(found.modules.is_empty());
        // A declaration after a test item on its line is not in it.
        let after = rust("src/lib.rs", "#[cfg(test)] mod x {} mod same;\n");
        assert_eq!(
            after.modules,
            [Module::Files(vec![
                "src/same.rs".into(),
                "src/same/mod.rs".into()
            ])]
        );
        assert!(after.files.is_empty());
        // An inline test module: the file of what it declares is not placed.
        let inline = rust("src/lib.rs", "#[cfg(test)]\nmod tests {\n    mod sub;\n}\n");
        assert_eq!(inline.doubts, [Doubt::InsideModule]);
        assert!(inline.files.is_empty() && inline.modules.is_empty());
    }

    #[test]
    fn an_inner_cfg_test_makes_the_whole_file_a_test_part() {
        let found = rust("src/lib.rs", "//! docs\n#![cfg(test)]\n\nfn a() {}\n");
        let lines: Vec<_> = found.spans.iter().map(|s| s.lines.clone()).collect();
        assert_eq!(lines, [1..=4]);
        assert_eq!(found.spans[0].part.kind, Some(Kind::Test));
        assert!(found.doubts.is_empty());
    }

    #[test]
    fn a_cfg_that_only_a_test_build_satisfies_is_a_test_part() {
        let source = concat!(
            "#[cfg(all(test, feature = \"x\"))]\nmod a {\n}\n",
            "#[cfg(all(test, not(windows)))]\nmod b {\n}\n",
            "#[cfg(any(test, unix))]\nmod c {\n}\n",
            "#[cfg(all(unix, not(test)))]\nmod d {\n}\n",
            "#[cfg(all(unix, test))]\nmod e {\n}\n",
        );
        assert_eq!(lines(source), [1..=3, 4..=6, 13..=15]);
    }

    #[test]
    fn a_module_file_is_next_to_its_declaring_file_or_under_it() {
        let files = |path: &str| rust(path, "#[cfg(test)]\nmod t;\n").files;
        // A `mod.rs`, `lib.rs` or `main.rs` owns its directory.
        assert_eq!(
            files("src/store/mod.rs"),
            [["src/store/t.rs", "src/store/t/mod.rs"]]
        );
        assert_eq!(files("src/lib.rs"), [["src/t.rs", "src/t/mod.rs"]]);
        assert_eq!(files("lib.rs"), [["t.rs", "t/mod.rs"]]);
        // Any other file has a directory of its own name.
        assert_eq!(
            files("src/store/db.rs"),
            [["src/store/db/t.rs", "src/store/db/t/mod.rs"]]
        );
    }

    #[test]
    fn odd_forms_of_a_cfg_are_told_apart() {
        let source = concat!(
            "#[cfg_attr(test, derive(Debug))]\nstruct A;\n",
            "#[cfg(all())]\nmod b {\n}\n",
            "#[cfg (all (test, unix))]\nmod c {\n}\n",
            "#[cfg(not(all(test, unix)))]\nmod d {\n}\n",
            "#[cfg(all(test))]\nmod e {\n}\n",
        );
        assert_eq!(lines(source), [6..=8, 12..=14]);
    }

    #[test]
    fn a_module_with_a_raw_name_or_a_scoped_visibility_still_names_its_file() {
        let found = rust(
            "src/lib.rs",
            "#[cfg(test)]\nmod r#type;\n#[cfg(test)]\npub(in crate::a) mod scoped;\n#[cfg(test)]\npub  mod  spaced ;\n",
        );
        let names: Vec<_> = found.files.iter().map(|f| f[0].clone()).collect();
        assert_eq!(names, ["src/type.rs", "src/scoped.rs", "src/spaced.rs"]);
    }

    #[test]
    fn what_cannot_be_placed_from_the_file_alone_is_said() {
        // An inner attribute of an inline module, a module file declared in one, and
        // a module file that a `#[path]` puts somewhere else.
        let nested = rust(
            "src/lib.rs",
            "mod m {\n    #![cfg(test)]\n    fn a() {}\n}\n",
        );
        assert!(nested.spans.is_empty());
        assert_eq!(nested.doubts, [Doubt::InsideModule]);
        let declared = rust("src/lib.rs", "mod a {\n    #[cfg(test)]\n    mod t;\n}\n");
        assert!(declared.files.is_empty());
        assert_eq!(declared.doubts, [Doubt::InsideModule]);
        let before = rust("src/lib.rs", "#[path = \"x.rs\"]\n#[cfg(test)]\nmod t;\n");
        assert!(before.files.is_empty());
        assert_eq!(before.doubts, [Doubt::ModulePath]);
        let after = rust("src/lib.rs", "#[cfg(test)]\n#[path = \"x.rs\"]\nmod t;\n");
        assert_eq!(after.doubts, [Doubt::ModulePath]);
    }
}
