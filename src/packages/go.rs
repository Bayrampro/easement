//! `go.mod`: the `require` directives, the local `replace` ones, and the module path a
//! module of the project is known by.

use super::{Declared, Source};

/// The directives of a `go.mod` as (verb, the words after it, line): a block
/// `require ( … )` is one directive for each of its lines. Comments are dropped.
fn directives(text: &str) -> Vec<(String, Vec<String>, usize)> {
    let mut found = Vec::new();
    let mut block: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default().trim();
        if code.is_empty() {
            continue;
        }
        let words: Vec<String> = code
            .split_whitespace()
            .map(|word| word.trim_matches('"').to_string())
            .collect();
        match &block {
            Some(_) if code == ")" => block = None,
            Some(verb) => found.push((verb.clone(), words, index + 1)),
            None if words.len() == 2 && words[1] == "(" => block = Some(words[0].clone()),
            None => found.push((words[0].clone(), words[1..].to_vec(), index + 1)),
        }
    }
    found
}

/// The modules a `go.mod` requires, with the line of each. A `replace` by a local
/// directory makes it a package of the project by path.
pub(super) fn declared(text: &str) -> Option<Vec<Declared>> {
    let directives = directives(text);
    // module -> the local directory it is replaced by
    let replaced: Vec<(&str, &str)> = directives
        .iter()
        .filter(|(verb, _, _)| verb == "replace")
        .filter_map(|(_, words, _)| {
            let arrow = words.iter().position(|w| w == "=>")?;
            let target = words.get(arrow + 1)?;
            (target.starts_with('.') || target.starts_with('/'))
                .then(|| (words[0].as_str(), target.as_str()))
        })
        .collect();
    Some(
        directives
            .iter()
            .filter(|(verb, words, _)| verb == "require" && !words.is_empty())
            .map(|(_, words, line)| {
                let module = words[0].clone();
                let source = replaced
                    .iter()
                    .find(|(from, _)| *from == module)
                    .map_or(Source::Package, |(_, dir)| Source::Path((*dir).to_string()));
                Declared {
                    line: *line,
                    package: module.clone(),
                    name: module,
                    source,
                    part: None,
                    target: None,
                    feature: None,
                    optional: false,
                }
            })
            .collect(),
    )
}

/// The module path a `go.mod` declares.
pub(super) fn name(text: &str) -> Option<String> {
    directives(text)
        .into_iter()
        .find(|(verb, _, _)| verb == "module")
        .and_then(|(_, words, _)| words.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::super::Source;
    use super::*;

    type Row = (usize, String, String, Source);

    fn rows(text: &str) -> Vec<Row> {
        declared(text)
            .unwrap()
            .into_iter()
            .map(|d| (d.line, d.name, d.package, d.source))
            .collect()
    }

    #[test]
    fn a_go_mod_requires_a_module_on_the_line_of_its_path() {
        let text = concat!(
            "module example.com/app\n",                         // 1
            "\n",                                               // 2
            "go 1.22\n",                                        // 3
            "\n",                                               // 4
            "require (\n",                                      // 5
            "\texample.com/lib v1.0.0\n",                       // 6
            "\tgithub.com/lib/pq v1.10.0 // indirect\n",        // 7
            ")\n",                                              // 8
            "\n",                                               // 9
            "require github.com/x/y v1.2.3\n",                  // 10
            "\n",                                               // 11
            "replace example.com/lib => ../lib\n",              // 12
            "replace (\n",                                      // 13
            "\tgithub.com/x/y v1.2.3 => ./vendor/y\n",          // 14
            "\texample.com/other => example.com/fork v1.0.0\n", // 15
            ")\n",                                              // 16
        );
        let n = |x: &str| x.to_string();
        assert_eq!(
            rows(text),
            [
                (
                    6,
                    n("example.com/lib"),
                    n("example.com/lib"),
                    Source::Path(n("../lib"))
                ),
                (
                    7,
                    n("github.com/lib/pq"),
                    n("github.com/lib/pq"),
                    Source::Package
                ),
                (
                    10,
                    n("github.com/x/y"),
                    n("github.com/x/y"),
                    Source::Path(n("./vendor/y"))
                ),
            ]
        );
        assert_eq!(name(text), Some(n("example.com/app")));
    }

    #[test]
    fn a_commented_out_requirement_and_a_quoted_path_are_read_as_go_does() {
        let text = "module \"example.com/app\"\n\nrequire (\n\t// example.com/gone v1.0.0\n\t\"example.com/kept\" v1.0.0\n)\n";
        let got: Vec<_> = rows(text).into_iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(got, [(5, "example.com/kept".to_string())]);
        assert_eq!(name(text), Some("example.com/app".to_string()));
    }

    #[test]
    fn a_go_mod_with_no_requirements_declares_nothing() {
        assert_eq!(declared("module a\n").map(|v| v.len()), Some(0));
        assert_eq!(name("go 1.22\n"), None);
    }
}
