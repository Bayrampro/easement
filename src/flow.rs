//! The references of an index, attributed to the definition that holds them.
//!
//! `must_not_reach` follows imports between files, so calling one function of a
//! file reaches everything the file imports. With `reach: symbols` a chain follows
//! the definitions a call references instead: a node is a method or function (a
//! symbol with the lines it spans) or what a file holds outside any of them (its
//! class bodies, constants, top-level code), and an edge is a reference at a line.
//!
//! A dependency that no reference accounts for stays an edge to everything in its
//! target, and so does a class or constant used with no method named, so no import
//! is dropped for want of a range.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::scip::{Document, Scope};

/// A part of a file: one of its methods, or, with no scope, the code that is in none.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Node {
    pub file: String,
    pub scope: Option<String>,
}

impl Node {
    pub fn file(file: &str) -> Self {
        Self {
            file: file.to_string(),
            scope: None,
        }
    }

    /// The node as a graph key; `file_of` takes the file back.
    pub fn key(&self) -> String {
        match &self.scope {
            Some(scope) => format!("{}{SEPARATOR}{scope}", self.file),
            None => self.file.clone(),
        }
    }
}

const SEPARATOR: char = '\u{1}';

/// The file of a node key.
pub fn file_of(key: &str) -> &str {
    key.split_once(SEPARATOR).map_or(key, |(file, _)| file)
}

/// A reference at `line` of the source node's file to the target node.
#[derive(Debug, Clone)]
pub struct Edge {
    pub from: Node,
    pub to: Node,
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Flow {
    /// The methods of each file that the index gave a range.
    scopes: BTreeMap<String, Vec<Scope>>,
    pub edges: Vec<Edge>,
}

impl Flow {
    /// The innermost scopes that hold `line` of `file`, or the file's own code.
    pub fn owners(&self, file: &str, line: usize) -> Vec<Node> {
        let holding: Vec<&Scope> = self
            .scopes
            .get(file)
            .into_iter()
            .flatten()
            .filter(|scope| scope.first <= line && line <= scope.last)
            .collect();
        let Some(smallest) = holding.iter().map(|s| s.last - s.first).min() else {
            return vec![Node::file(file)];
        };
        // Two scopes of one size on one line cannot be told apart: both hold it.
        holding
            .into_iter()
            .filter(|scope| scope.last - scope.first == smallest)
            .map(|scope| Node {
                file: file.to_string(),
                scope: Some(scope.symbol.clone()),
            })
            .collect()
    }

    /// Everything in `file`: its own code and each of its methods.
    pub fn nodes_of(&self, file: &str) -> Vec<Node> {
        std::iter::once(Node::file(file))
            .chain(
                self.scopes
                    .get(file)
                    .into_iter()
                    .flatten()
                    .map(|scope| Node {
                        file: file.to_string(),
                        scope: Some(scope.symbol.clone()),
                    }),
            )
            .collect()
    }
}

/// The references of `documents`, each from the definition that holds it to the
/// definition it names. A symbol that more than one file defines (a Ruby module
/// reopened in each file under it) names no one place, so its references are left to
/// the dependencies, which keep reaching a whole file.
pub fn build(documents: &[Document]) -> Flow {
    let mut defined: HashMap<&str, &str> = HashMap::new();
    let mut reopened: HashSet<&str> = HashSet::new();
    let mut scoped: HashSet<&str> = HashSet::new();
    let mut flow = Flow::default();
    for document in documents {
        for symbol in &document.definitions {
            if *defined.entry(symbol).or_insert(&document.path) != document.path.as_str() {
                reopened.insert(symbol);
            }
        }
        for scope in &document.scopes {
            scoped.insert(&scope.symbol);
        }
        if !document.scopes.is_empty() {
            flow.scopes
                .insert(document.path.clone(), document.scopes.clone());
        }
    }
    // A method reaches the code of its own file that is in none: the class body and the
    // constants that run when the file loads.
    let own: Vec<Edge> = flow
        .scopes
        .iter()
        .flat_map(|(file, scopes)| {
            scopes.iter().map(|scope| Edge {
                from: Node {
                    file: file.clone(),
                    scope: Some(scope.symbol.clone()),
                },
                to: Node::file(file),
                line: scope.first,
            })
        })
        .collect();
    flow.edges.extend(own);
    for document in documents {
        // The files whose method a line names: such a line reaches that method, and a
        // reference to the class around it (`Document` of `Document.host`) adds nothing.
        let named: HashSet<(usize, &str)> = document
            .references
            .iter()
            .filter(|r| scoped.contains(r.symbol.as_str()) && !reopened.contains(r.symbol.as_str()))
            .filter_map(|r| defined.get(r.symbol.as_str()).map(|file| (r.line, *file)))
            .collect();
        for reference in &document.references {
            let symbol = reference.symbol.as_str();
            let Some(&file) = defined.get(symbol) else {
                continue;
            };
            if reopened.contains(symbol) {
                continue;
            }
            let targets = if scoped.contains(symbol) {
                vec![Node {
                    file: file.to_string(),
                    scope: Some(symbol.to_string()),
                }]
            } else if symbol.ends_with('/') || named.contains(&(reference.line, file)) {
                // An import runs the code of the file that is in no method; the names it
                // brings are reached by their own references.
                vec![Node::file(file)]
            } else {
                // A class or constant used with no method named (an opaque receiver):
                // any of the file's methods may be what runs.
                flow.nodes_of(file)
            };
            for from in flow.owners(&document.path, reference.line) {
                for to in &targets {
                    if from != *to {
                        flow.edges.push(Edge {
                            from: from.clone(),
                            to: to.clone(),
                            line: reference.line,
                        });
                    }
                }
            }
        }
    }
    flow
}
