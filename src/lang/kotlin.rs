use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use tree_sitter::{Language, Query};

use super::{Adapter, Import, Target, captured_nodes, parse, relative};

const QUERY: &str = "(import) @node";

/// Modifiers that may precede the keyword of a top-level declaration.
const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "internal",
    "protected",
    "data",
    "sealed",
    "enum",
    "abstract",
    "open",
    "final",
    "annotation",
    "inline",
    "value",
    "suspend",
    "const",
    "lateinit",
    "expect",
    "actual",
    "tailrec",
    "operator",
    "infix",
    "external",
];

const KEYWORDS: &[&str] = &[
    "class",
    "interface",
    "object",
    "typealias",
    "fun",
    "val",
    "var",
];

/// Kotlin. Declarations are indexed by the `package` of their file, so source
/// roots and files that hold several classes need no configuration.
///
/// `import a.b.C`, `import a.b.C.Nested` and `import a.b.topLevelFun` resolve to
/// the file that declares the longest matching top-level name (`C`, found by
/// scanning the declarations at the start of lines, else by a file named `C.kt`);
/// `import a.b.*` to the first directory of the package. Names used by their
/// fully qualified name without an import are not seen.
pub struct Kotlin {
    language: Language,
    query: Query,
    /// `package.Name` of a top-level declaration -> file, relative to the root.
    declarations: BTreeMap<String, PathBuf>,
    /// Package -> directories holding it, relative to the root.
    packages: BTreeMap<String, BTreeSet<PathBuf>>,
}

impl Kotlin {
    pub fn new(root: &Path) -> Result<Self> {
        let language: Language = tree_sitter_kotlin_ng::LANGUAGE.into();
        let query = Query::new(&language, QUERY)?;
        let mut declarations = BTreeMap::new();
        let mut packages: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
        for entry in crate::scan::walker(root).build().flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "kt" || e == "kts") {
                continue;
            }
            let (Some(rel), Some(stem)) = (relative(root, path), path.file_stem()) else {
                continue;
            };
            let Ok(source) = std::fs::read_to_string(path) else {
                continue;
            };
            let package = package_of(&source).unwrap_or_default();
            let qualify = |name: &str| {
                if package.is_empty() {
                    name.to_string()
                } else {
                    format!("{package}.{name}")
                }
            };
            for name in top_level_names(&source) {
                declarations
                    .entry(qualify(&name))
                    .or_insert_with(|| rel.clone());
            }
            declarations
                .entry(qualify(&stem.to_string_lossy()))
                .or_insert_with(|| rel.clone());
            if let Some(dir) = rel.parent() {
                packages
                    .entry(package)
                    .or_default()
                    .insert(dir.to_path_buf());
            }
        }
        Ok(Self {
            language,
            query,
            declarations,
            packages,
        })
    }
}

/// The `package` header, read line by line without a full parse.
fn package_of(source: &str) -> Option<String> {
    source.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("package")?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let name = rest.trim().trim_end_matches(';').trim();
        (!name.is_empty()).then(|| name.to_string())
    })
}

/// Names of declarations that start at the beginning of a line, which is where
/// top-level ones are written: `data class User(`, `fun String.slug()`.
fn top_level_names(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| {
            let mut words = line.split_whitespace().peekable();
            while words
                .next_if(|w| MODIFIERS.contains(w) || w.starts_with('@'))
                .is_some()
            {}
            let keyword = words.next()?;
            if !KEYWORDS.contains(&keyword) {
                return None;
            }
            let word = loop {
                let word = words.next()?;
                // `fun <T> name`, `fun interface Name`, `companion object` are skipped over.
                if !word.starts_with('<') && word != "interface" {
                    break word;
                }
            };
            // `List<T>.second(` -> `List.second` -> `second`
            let head = word.split(['(', ':', '=', '{']).next()?;
            let mut depth = 0;
            let plain: String = head
                .chars()
                .filter(|c| match c {
                    '<' => {
                        depth += 1;
                        false
                    }
                    '>' => {
                        depth -= 1;
                        false
                    }
                    _ => depth == 0,
                })
                .collect();
            let name = plain.rsplit('.').next()?.to_string();
            (!name.is_empty()).then(|| name.trim_matches('`').to_string())
        })
        .collect()
}

/// `import a.b.C as D` -> `a.b.C`
fn specifier(declaration: &str) -> String {
    let body = declaration
        .trim()
        .trim_start_matches("import")
        .trim_end_matches(';')
        .trim();
    body.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

impl Adapter for Kotlin {
    fn name(&self) -> &'static str {
        "kotlin"
    }

    fn handles(&self, path: &Path) -> bool {
        super::registry::is(path, &super::registry::KOTLIN)
    }

    fn imports(&self, path: &Path, source: &str) -> Result<Vec<Import>> {
        let tree = parse(&self.language, path, source)?;
        Ok(captured_nodes(&self.query, &tree, source)
            .into_iter()
            .map(|node| Import {
                specifier: specifier(node.utf8_text(source.as_bytes()).unwrap_or_default()),
                line: node.start_position().row + 1,
            })
            .collect())
    }

    fn resolve(&self, _file: &Path, specifier: &str) -> Target {
        let name = specifier.trim_end_matches(".*");
        if specifier.ends_with(".*")
            && let Some(dir) = self.packages.get(name).and_then(|d| d.first())
        {
            return Target::Internal(dir.clone().into());
        }
        let parts: Vec<&str> = name.split('.').collect();
        for k in (1..=parts.len()).rev() {
            if let Some(file) = self.declarations.get(&parts[..k].join(".")) {
                return Target::Internal(file.clone().into());
            }
        }
        Target::External(specifier.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_package_and_top_level_names() {
        assert_eq!(
            package_of("// c\npackage com.acme.shop\n\nimport a.B").as_deref(),
            Some("com.acme.shop")
        );
        assert_eq!(package_of("class A"), None);
        let source = "package p\n\nimport a.B\n\n@Serializable\ndata class User(val id: Int)\n\
                      internal fun <T> List<T>.second(): T = this[1]\nfun String.slug() = this\n\
                      object Registry {\n    fun inner() {}\n}\nfun interface Handler { fun run() }\n\
                      typealias Id = Long\nconst val LIMIT = 3\n";
        assert_eq!(
            top_level_names(source),
            [
                "User", "second", "slug", "Registry", "Handler", "Id", "LIMIT"
            ]
        );
    }

    #[test]
    fn extracts_imports() {
        let kt = Kotlin::new(Path::new("/nonexistent")).unwrap();
        let src = "package p\n\nimport com.acme.model.User\nimport com.acme.util.*\nimport com.acme.x.Y as Z\n\nclass A\n";
        let got: Vec<_> = kt
            .imports(Path::new("A.kt"), src)
            .unwrap()
            .into_iter()
            .map(|i| (i.specifier, i.line))
            .collect();
        assert_eq!(
            got,
            [
                ("com.acme.model.User".to_string(), 3),
                ("com.acme.util.*".to_string(), 4),
                ("com.acme.x.Y".to_string(), 5),
            ]
        );
    }

    #[test]
    fn resolves_declarations_and_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (path, text) in [
            (
                "model/Models.kt",
                "package com.acme.model\n\ndata class User(val id: Int)\nclass Order\n",
            ),
            (
                "util/Strings.kt",
                "package com.acme.util\n\nfun String.slug() = this\n",
            ),
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let kt = Kotlin::new(root).unwrap();
        let go = |spec: &str| kt.resolve(Path::new("A.kt"), spec);
        assert_eq!(
            go("com.acme.model.User"),
            Target::Internal("model/Models.kt".into())
        );
        assert_eq!(
            go("com.acme.model.Order.Item"),
            Target::Internal("model/Models.kt".into())
        );
        assert_eq!(
            go("com.acme.util.slug"),
            Target::Internal("util/Strings.kt".into())
        );
        assert_eq!(
            go("com.acme.util.Strings"),
            Target::Internal("util/Strings.kt".into())
        );
        assert_eq!(go("com.acme.util.*"), Target::Internal("util".into()));
        assert_eq!(
            go("kotlin.collections.List"),
            Target::External("kotlin.collections.List".into())
        );
    }
}
