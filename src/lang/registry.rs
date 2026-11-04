//! What easement knows of each language, told by the extensions of its files.
//! Adding a language is one entry here, plus an indexer in `indexer::INDEXERS`
//! or an adapter in this module; the rules ask this table, never an extension.

use super::parts::{self, Found};
use crate::relpath;
use crate::stdlib;

/// The file that declares the external modules of a language's files, which
/// the `manifests` rule reads.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Manifest {
    Npm,
    Go,
    Cargo,
}

impl Manifest {
    pub fn file_name(self) -> &'static str {
        match self {
            Manifest::Npm => "package.json",
            Manifest::Go => "go.mod",
            Manifest::Cargo => "Cargo.toml",
        }
    }
}

#[derive(Debug)]
pub struct Language {
    pub name: &'static str,
    /// Extensions of its source files.
    pub extensions: &'static [&'static str],
    /// Globs of its test files, which `tests: auto` puts into the `tests` component.
    pub tests: &'static [&'static str],
    /// Globs of files that `tests` matches but that are not test files.
    pub not_tests: &'static [&'static str],
    /// Whether a module belongs to its standard library, for `$std`; `None`
    /// when easement cannot tell.
    pub std: Option<fn(&str) -> bool>,
    /// Extensions of the files `unused` judges: those whose import graph shows
    /// every use of another file. Empty when it judges none.
    pub unused: &'static [&'static str],
    /// Files its toolchain or a convention runs without an import, which
    /// `unused` counts as reached.
    pub conventional: &'static [&'static str],
    /// The manifest the `manifests` rule checks its imports against.
    pub manifest: Option<Manifest>,
    /// Finds the parts of a source file that a dependency belongs to instead of the
    /// file, for example the code of a test build. `None` when the file is one unit.
    pub parts: Option<fn(&str, &str) -> Found>,
}

const TYPESCRIPT_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

pub static GO: Language = Language {
    name: "go",
    extensions: &["go"],
    tests: &["**/*_test.go"],
    not_tests: &[],
    std: Some(stdlib::go),
    unused: &["go"],
    conventional: &[],
    manifest: Some(Manifest::Go),
    parts: None,
};

/// TypeScript and JavaScript, which one indexer reads and the bundlers resolve alike.
pub static TYPESCRIPT: Language = Language {
    name: "typescript",
    extensions: TYPESCRIPT_EXTENSIONS,
    tests: &["**/*.test.*", "**/*.spec.*", "**/__tests__/**"],
    not_tests: &[],
    std: None,
    unused: TYPESCRIPT_EXTENSIONS,
    conventional: &["**/*.d.ts"],
    manifest: Some(Manifest::Npm),
    parts: None,
};

pub static PYTHON: Language = Language {
    name: "python",
    extensions: &["py", "pyi"],
    tests: &["**/test_*.py", "**/*_test.py"],
    not_tests: &[],
    std: Some(stdlib::python),
    // A stub (`.pyi`) is imported by type checkers, not by the program.
    unused: &["py"],
    conventional: &["**/conftest.py", "**/__main__.py", "**/setup.py"],
    manifest: None,
    parts: None,
};

pub static JAVA: Language = Language {
    name: "java",
    extensions: &["java"],
    tests: &["**/*Test.java", "**/*Tests.java"],
    not_tests: &[],
    std: Some(stdlib::java),
    unused: &["java"],
    conventional: &["**/package-info.java", "**/module-info.java"],
    manifest: None,
    parts: None,
};

/// Read by imports alone (an adapter), so `unused` cannot judge it.
pub static KOTLIN: Language = Language {
    name: "kotlin",
    extensions: &["kt", "kts"],
    tests: &["**/*Test.kt", "**/*Tests.kt"],
    not_tests: &[],
    std: Some(stdlib::kotlin),
    unused: &[],
    conventional: &[],
    manifest: None,
    parts: None,
};

pub static CSHARP: Language = Language {
    name: "csharp",
    extensions: &["cs"],
    tests: &["**/*Test.cs", "**/*Tests.cs"],
    not_tests: &[],
    std: Some(stdlib::csharp),
    unused: &["cs"],
    conventional: &[],
    manifest: None,
    parts: None,
};

pub static PHP: Language = Language {
    name: "php",
    extensions: &["php"],
    tests: &["**/*Test.php"],
    not_tests: &[],
    std: Some(stdlib::php),
    unused: &["php"],
    conventional: &[],
    manifest: None,
    parts: None,
};

/// Read by imports alone (an adapter), so `unused` cannot judge it.
pub static SWIFT: Language = Language {
    name: "swift",
    extensions: &["swift"],
    tests: &["**/*Tests.swift"],
    not_tests: &[],
    std: Some(stdlib::swift),
    unused: &[],
    conventional: &[],
    manifest: None,
    parts: None,
};

/// Ruby loads by convention (autoloading), so `unused` cannot judge it.
pub static RUBY: Language = Language {
    name: "ruby",
    extensions: &["rb"],
    tests: &[],
    not_tests: &[],
    std: None,
    unused: &[],
    conventional: &[],
    manifest: None,
    parts: None,
};

pub static RUST: Language = Language {
    name: "rust",
    extensions: &["rs"],
    // Cargo's integration tests; a `tests` module under `src/` is the crate's
    // own code, and `parts` takes it for tests when `#[cfg(test)]` declares it.
    tests: &["**/tests/**/*.rs"],
    not_tests: &["**/src/**/tests/**/*.rs"],
    std: Some(stdlib::rust),
    unused: &["rs"],
    conventional: &[
        "**/build.rs",
        "**/src/main.rs",
        "**/src/lib.rs",
        "**/src/bin/**",
        "**/examples/**",
        "**/benches/**",
        "**/tests/**",
    ],
    manifest: Some(Manifest::Cargo),
    parts: Some(parts::rust),
};

/// Every language, in the order their test globs are listed.
pub static LANGUAGES: [&Language; 10] = [
    &GO,
    &TYPESCRIPT,
    &PYTHON,
    &JAVA,
    &KOTLIN,
    &CSHARP,
    &PHP,
    &SWIFT,
    &RUBY,
    &RUST,
];

/// The language of a file, by its extension.
pub fn of(file: &str) -> Option<&'static Language> {
    let ext = relpath::extension(file);
    LANGUAGES
        .into_iter()
        .find(|language| language.extensions.contains(&ext))
}

/// The kind of manifest a file is, by its name.
pub fn manifest_of(file: &str) -> Option<Manifest> {
    let name = file.rsplit('/').next().unwrap_or(file);
    [Manifest::Npm, Manifest::Go, Manifest::Cargo]
        .into_iter()
        .find(|manifest| manifest.file_name() == name)
}

/// Whether the file at `path` is of `language`.
pub fn is(path: &std::path::Path, language: &Language) -> bool {
    path.to_str()
        .and_then(of)
        .is_some_and(|found| std::ptr::eq(found, language))
}

/// The test globs of every language, which `tests: auto` covers.
pub fn test_patterns() -> Vec<&'static str> {
    LANGUAGES
        .iter()
        .flat_map(|language| language.tests.iter().copied())
        .collect()
}

/// The files the test globs match that are not tests, which `tests: auto` gives up.
pub fn not_test_patterns() -> Vec<&'static str> {
    LANGUAGES
        .iter()
        .flat_map(|language| language.not_tests.iter().copied())
        .collect()
}
