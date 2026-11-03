use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tree_sitter::{Language, Query};

use super::{Adapter, Import, Target, captured_nodes, parse, relative};

const QUERY: &str = "(import_declaration) @node";

/// Words that may sit between `import` and the module name: `import struct A.B`.
const KINDS: &[&str] = &[
    "typealias",
    "struct",
    "class",
    "enum",
    "protocol",
    "let",
    "var",
    "func",
];

/// Swift. An import names a module, and the modules of the project are the
/// directories under `Sources/` next to a `Package.swift` (SwiftPM layout):
/// `import Core` resolves to the directory `Sources/Core`. Every other module,
/// `Foundation` included, is external. `import struct Core.User` is the module `Core`.
pub struct Swift {
    language: Language,
    query: Query,
    /// Module name -> its directory, relative to the root.
    modules: BTreeMap<String, PathBuf>,
}

impl Swift {
    pub fn new(root: &Path) -> Result<Self> {
        let language: Language = tree_sitter_swift::LANGUAGE.into();
        let query = Query::new(&language, QUERY)?;
        let mut modules = BTreeMap::new();
        for entry in crate::scan::walker(root).build().flatten() {
            if entry.file_name() != "Package.swift" {
                continue;
            }
            let Some(package) =
                relative(root, entry.path()).and_then(|rel| rel.parent().map(Path::to_path_buf))
            else {
                continue;
            };
            let Ok(sources) = std::fs::read_dir(root.join(&package).join("Sources")) else {
                continue;
            };
            for dir in sources.flatten().filter(|d| d.path().is_dir()) {
                let name = dir.file_name().to_string_lossy().to_string();
                modules
                    .entry(name.clone())
                    .or_insert_with(|| package.join("Sources").join(name));
            }
        }
        Ok(Self {
            language,
            query,
            modules,
        })
    }
}

/// `@testable import struct Acme.Models.User` -> `Acme.Models.User`
fn specifier(declaration: &str) -> String {
    let words: Vec<&str> = declaration
        .split_whitespace()
        .skip_while(|w| *w != "import")
        .skip(1)
        .collect();
    let name = match words.first() {
        Some(first) if KINDS.contains(first) => words.get(1),
        other => other,
    };
    name.map(|n| n.trim_end_matches(';').to_string())
        .unwrap_or_default()
}

impl Adapter for Swift {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn handles(&self, path: &Path) -> bool {
        super::registry::is(path, &super::registry::SWIFT)
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
        let module = specifier.split('.').next().unwrap_or(specifier);
        match self.modules.get(module) {
            Some(dir) => Target::Internal(dir.clone().into()),
            None => Target::External(specifier.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_every_import_form() {
        let swift = Swift::new(Path::new("/nonexistent")).unwrap();
        let src = "import Foundation\nimport struct Acme.Models.User\n@testable import Core\nclass A {}\n";
        let got: Vec<_> = swift
            .imports(Path::new("A.swift"), src)
            .unwrap()
            .into_iter()
            .map(|i| (i.specifier, i.line))
            .collect();
        assert_eq!(
            got,
            [
                ("Foundation".to_string(), 1),
                ("Acme.Models.User".to_string(), 2),
                ("Core".to_string(), 3),
            ]
        );
    }

    #[test]
    fn resolves_modules_of_the_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for path in [
            "Package.swift",
            "Sources/Core/User.swift",
            "Sources/App/main.swift",
            "Packages/Net/Package.swift",
            "Packages/Net/Sources/Net/Client.swift",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let swift = Swift::new(root).unwrap();
        let go = |spec: &str| swift.resolve(Path::new("A.swift"), spec);
        assert_eq!(go("Core"), Target::Internal("Sources/Core".into()));
        assert_eq!(go("Core.User"), Target::Internal("Sources/Core".into()));
        assert_eq!(
            go("Net"),
            Target::Internal("Packages/Net/Sources/Net".into())
        );
        assert_eq!(go("Foundation"), Target::External("Foundation".into()));
    }
}
