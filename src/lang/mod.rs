use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, Tree};

pub mod kotlin;
pub mod registry;
pub mod swift;

/// One dependency found in a source file.
#[derive(Debug, Clone)]
pub struct Import {
    /// The specifier as written in the source, without quotes.
    pub specifier: String,
    /// 1-based line number.
    pub line: usize,
}

/// Where an import points to after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A file or package directory inside the project, relative to the root.
    Internal(PathBuf),
    /// A third-party or standard-library module.
    External(String),
}

/// A language adapter: extracts imports from a file and resolves them.
///
/// Adding a language means implementing this trait; the core never sees
/// language-specific details.
pub trait Adapter {
    fn name(&self) -> &'static str;
    fn handles(&self, path: &Path) -> bool;
    fn imports(&self, path: &Path, source: &str) -> Result<Vec<Import>>;
    fn resolve(&self, file: &Path, specifier: &str) -> Target;
}

pub fn adapters(root: &Path) -> Result<Vec<Box<dyn Adapter>>> {
    Ok(vec![
        Box::new(kotlin::Kotlin::new(root)?),
        Box::new(swift::Swift::new(root)?),
    ])
}

pub(crate) fn parse(language: &Language, path: &Path, source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser.set_language(language)?;
    parser
        .parse(source, None)
        .with_context(|| format!("cannot parse {}", path.display()))
}

/// Every node captured as `@node` by `query`, in document order.
pub(crate) fn captured_nodes<'t>(query: &Query, tree: &'t Tree, source: &str) -> Vec<Node<'t>> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut nodes = Vec::new();
    while let Some(m) = matches.next() {
        nodes.extend(m.captures().iter().map(|c| c.node));
    }
    nodes
}

/// Path relative to `root` with `/` separators, if `path` is inside `root`.
pub fn relative(root: &Path, path: &Path) -> Option<PathBuf> {
    path.strip_prefix(root).ok().map(Path::to_path_buf)
}
