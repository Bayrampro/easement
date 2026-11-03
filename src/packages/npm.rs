//! `package.json`: the dependency tables, and the name a package of the project is
//! known by.

use super::{Declared, Source};
use crate::lang::parts::{Kind, Part};

const SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// The dependencies a `package.json` declares, with the line of each; `None` if it is
/// no JSON.
pub(super) fn declared(text: &str) -> Option<Vec<Declared>> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut found = Vec::new();
    for (section, name, line) in keys(text) {
        if !SECTIONS.contains(&section.as_str()) {
            continue;
        }
        let Some(version) = json[&section][&name].as_str() else {
            continue;
        };
        let (package, source) = dependency(&name, version);
        found.push(Declared {
            part: (section == "devDependencies").then(|| Part {
                name: section.clone(),
                kind: Some(Kind::Test),
            }),
            ..Declared::new(line, &name, &package, source)
        });
    }
    Some(found)
}

/// The package a dependency is and where it comes from, by what its version says: a
/// path (`file:`, `link:`, `workspace:../x`), an alias (`npm:real@1`, `workspace:real@*`)
/// or a range of the package that goes by its name.
fn dependency(name: &str, version: &str) -> (String, Source) {
    let alias = |text: &str| match text.rfind('@') {
        Some(at) if at > 0 => text[..at].to_string(),
        _ => name.to_string(),
    };
    if let Some(path) = version
        .strip_prefix("file:")
        .or_else(|| version.strip_prefix("link:"))
    {
        return (name.to_string(), Source::Path(path.to_string()));
    }
    if let Some(rest) = version.strip_prefix("workspace:") {
        return if rest.starts_with('.') {
            (name.to_string(), Source::Path(rest.to_string()))
        } else {
            (alias(rest), Source::Package)
        };
    }
    if let Some(rest) = version.strip_prefix("npm:") {
        let real = match rest.rfind('@') {
            Some(at) if at > 0 => rest[..at].to_string(),
            _ => rest.to_string(),
        };
        return (real, Source::Package);
    }
    (name.to_string(), Source::Package)
}

/// The name of the package a `package.json` declares.
pub(super) fn name(text: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    json["name"].as_str().map(str::to_string)
}

/// The keys of the objects directly under the top-level keys, as (top-level key, key,
/// line): where each dependency of a table stands.
fn keys(text: &str) -> Vec<(String, String, usize)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut section = String::new();
    let mut depth = 0_usize;
    let mut line = 1;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => line += 1,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let start_line = line;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'\n') {
                        line += 1;
                    }
                    j += 1;
                }
                let string = String::from_utf8_lossy(&bytes[i + 1..j.min(bytes.len())]).to_string();
                let next = bytes[(j + 1).min(bytes.len())..]
                    .iter()
                    .find(|b| !b.is_ascii_whitespace());
                if next == Some(&b':') {
                    match depth {
                        1 => section = string,
                        2 => found.push((section.clone(), string, start_line)),
                        _ => {}
                    }
                }
                i = j;
            }
            _ => {}
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::super::Source;
    use super::*;

    type Row = (usize, String, String, Source, Option<String>);

    fn rows(text: &str) -> Vec<Row> {
        declared(text)
            .unwrap()
            .into_iter()
            .map(|d| (d.line, d.name, d.package, d.source, d.part.map(|p| p.name)))
            .collect()
    }

    #[test]
    fn a_package_json_declares_a_dependency_on_the_line_of_its_name() {
        let text = concat!(
            "{\n",                                               // 1
            "  \"name\": \"web\",\n",                            // 2
            "  \"dependencies\": {\n",                           // 3
            "    \"api\": \"workspace:*\",\n",                   // 4
            "    \"lodash\": \"^4\",\n",                         // 5
            "    \"local\": \"file:../local\",\n",               // 6
            "    \"alias\": \"npm:real-pkg@^1\"\n",              // 7
            "  },\n",                                            // 8
            "  \"devDependencies\": {\n",                        // 9
            "    \"testkit\": \"workspace:^\"\n",                // 10
            "  },\n",                                            // 11
            "  \"peerDependencies\": { \"react\": \"^18\" },\n", // 12
            "  \"optionalDependencies\": {\n",                   // 13
            "    \"fsevents\": \"^2\"\n",                        // 14
            "  },\n",                                            // 15
            "  \"scripts\": { \"build\": \"tsc\" }\n",           // 16
            "}\n",                                               // 17
        );
        let n = |x: &str| x.to_string();
        assert_eq!(
            rows(text),
            [
                (4, n("api"), n("api"), Source::Package, None),
                (5, n("lodash"), n("lodash"), Source::Package, None),
                (6, n("local"), n("local"), Source::Path(n("../local")), None),
                (7, n("alias"), n("real-pkg"), Source::Package, None),
                (
                    10,
                    n("testkit"),
                    n("testkit"),
                    Source::Package,
                    Some(n("devDependencies"))
                ),
                (12, n("react"), n("react"), Source::Package, None),
                (14, n("fsevents"), n("fsevents"), Source::Package, None),
            ]
        );
        let kinds: Vec<_> = declared(text)
            .unwrap()
            .into_iter()
            .map(|d| d.part.and_then(|p| p.kind))
            .collect();
        assert_eq!(kinds[4], Some(crate::lang::parts::Kind::Test));
        assert_eq!(kinds[5], None);
    }

    #[test]
    fn a_scoped_alias_keeps_its_scope_and_a_name_is_read() {
        let text = "{\n  \"name\": \"@acme/web\",\n  \"dependencies\": { \"a\": \"npm:@acme/real@1.0.0\" }\n}\n";
        assert_eq!(rows(text)[0].2, "@acme/real");
        assert_eq!(name(text), Some("@acme/web".to_string()));
    }

    #[test]
    fn a_package_json_that_is_no_json_is_unread_not_empty() {
        assert!(declared("{ \"dependencies\": ").is_none());
        assert_eq!(declared("{}").map(|v| v.len()), Some(0));
        assert_eq!(name("{}"), None);
    }

    #[test]
    fn a_key_with_a_quote_or_a_brace_in_a_string_does_not_confuse_the_lines() {
        let text = "{\n  \"description\": \"a } \\\" { b\",\n  \"dependencies\": {\n    \"x\": \"1\"\n  }\n}\n";
        assert_eq!(rows(text)[0].0, 4);
    }

    #[test]
    fn the_workspace_protocol_names_a_path_or_an_alias_as_well_as_a_range() {
        let text = "{\n  \"dependencies\": {\n    \"a\": \"workspace:^\",\n    \"b\": \"workspace:../b\",\n    \"c\": \"workspace:real@*\",\n    \"d\": \"workspace:@acme/real@^1\"\n  }\n}\n";
        let got: Vec<_> = rows(text).into_iter().map(|r| (r.1, r.2, r.3)).collect();
        let n = |x: &str| x.to_string();
        assert_eq!(
            got,
            [
                (n("a"), n("a"), Source::Package),
                (n("b"), n("b"), Source::Path(n("../b"))),
                (n("c"), n("real"), Source::Package),
                (n("d"), n("@acme/real"), Source::Package),
            ]
        );
    }
}
