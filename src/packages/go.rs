//! `go.mod`: the `require` directives, the local `replace` ones, and the module path a
//! module of the project is known by.

use super::{Declared, Source};

/// A directive of a `go.mod`.
struct Directive {
    verb: String,
    words: Vec<String>,
    line: usize,
    /// Marked `// indirect`: the module needs it only for a module it requires.
    indirect: bool,
}

/// The directives of a `go.mod`: a block `require ( … )` is one directive for each of
/// its lines. Comments are dropped. `None` for a block that never closes.
fn directives(text: &str) -> Option<Vec<Directive>> {
    let mut found = Vec::new();
    let mut block: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let (code, comment) = line.split_once("//").unwrap_or((line, ""));
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let words: Vec<String> = code
            .split_whitespace()
            .map(|word| word.trim_matches(['"', '`']).to_string())
            .collect();
        let indirect = comment.trim() == "indirect";
        match &block {
            Some(_) if code == ")" => block = None,
            Some(verb) => found.push(Directive {
                verb: verb.clone(),
                words,
                line: index + 1,
                indirect,
            }),
            None if words.len() == 2 && words[1] == "(" => block = Some(words[0].clone()),
            None => found.push(Directive {
                verb: words[0].clone(),
                words: words[1..].to_vec(),
                line: index + 1,
                indirect,
            }),
        }
    }
    block.is_none().then_some(found)
}

/// The modules a `go.mod` requires directly, with the line of each. A `replace` by a
/// directory next to the module makes it a package of the project by path. `None` for a
/// `go.mod` that cannot be read.
pub(super) fn declared(text: &str) -> Option<Vec<Declared>> {
    let directives = directives(text)?;
    // module -> the relative directory it is replaced by
    let replaced: Vec<(&str, &str)> = directives
        .iter()
        .filter(|d| d.verb == "replace")
        .filter_map(|d| {
            let arrow = d.words.iter().position(|w| w == "=>")?;
            let target = d.words.get(arrow + 1)?;
            target
                .starts_with('.')
                .then(|| (d.words[0].as_str(), target.as_str()))
        })
        .collect();
    Some(
        directives
            .iter()
            .filter(|d| d.verb == "require" && !d.words.is_empty() && !d.indirect)
            .map(|d| {
                let module = d.words[0].clone();
                let source = replaced
                    .iter()
                    .find(|(from, _)| *from == module)
                    .map_or(Source::Package, |(_, dir)| Source::Path((*dir).to_string()));
                Declared::new(d.line, &module, &module, source)
            })
            .collect(),
    )
}

/// The module path a `go.mod` declares.
pub(super) fn name(text: &str) -> Option<String> {
    directives(text)?
        .into_iter()
        .find(|d| d.verb == "module")
        .and_then(|d| d.words.into_iter().next())
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
            "\tgithub.com/lib/pq v1.10.0\n",                    // 7
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

    #[test]
    fn what_a_module_does_not_import_directly_is_no_requirement() {
        let text = "module a\n\nrequire (\n\texample.com/direct v1.0.0\n\texample.com/transitive v1.0.0 // indirect\n)\n";
        let got: Vec<_> = rows(text).into_iter().map(|r| r.1).collect();
        assert_eq!(got, ["example.com/direct"]);
    }

    #[test]
    fn only_a_relative_directory_makes_a_replace_local() {
        let text = "module a\n\nrequire (\n\texample.com/rel v1.0.0\n\texample.com/abs v1.0.0\n\texample.com/back v1.0.0\n)\n\nreplace example.com/rel => ./rel\nreplace example.com/abs => /opt/abs\nreplace example.com/back => `../back`\n";
        let got: Vec<_> = rows(text).into_iter().map(|r| (r.1, r.3)).collect();
        assert_eq!(
            got,
            [
                (
                    "example.com/rel".to_string(),
                    Source::Path("./rel".to_string())
                ),
                ("example.com/abs".to_string(), Source::Package),
                (
                    "example.com/back".to_string(),
                    Source::Path("../back".to_string())
                ),
            ]
        );
    }

    #[test]
    fn a_block_that_never_closes_is_unread_not_empty() {
        assert!(declared("module a\n\nrequire (\n\texample.com/x v1.0.0\n").is_none());
    }
}
