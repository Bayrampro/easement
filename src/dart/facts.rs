//! Versioned semantic facts. No architecture policy is passed to the extractor.
use super::config::Selector;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct Location {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Facts {
    pub format_version: u32,
    pub extractor_version: String,
    pub analyzer_version: String,
    pub sdk_version: String,
    pub files: Vec<String>,
    pub declarations: Vec<Declaration>,
    pub gaps: Vec<Gap>,
    pub ui_classes: Vec<UiClass>,
    pub callables: Vec<Callable>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Declaration {
    pub identity: Selector,
    pub kind: String,
    pub location: Location,
    pub modifiers: Vec<String>,
    pub api: Vec<Use>,
    pub uses: Vec<Use>,
    pub supertypes: Vec<Type>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Use {
    pub member: String,
    pub operation: String,
    pub location: Location,
    pub origin: Location,
    #[serde(rename = "type")]
    pub ty: Type,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Type {
    pub display: String,
    pub nullable: bool,
    pub library: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub alias: bool,
    pub origin: Option<Location>,
    #[serde(default)]
    pub unresolved: bool,
    pub children: Vec<Type>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Gap {
    pub file: String,
    pub line: usize,
    pub reason: String,
}

/// Analyzer classifications are tri-state: unknown must never mean passing.
#[derive(Debug, Deserialize, Serialize)]
pub struct UiClass {
    pub identity: Selector,
    pub location: Location,
    pub widget: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Callable {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub enclosing: Option<String>,
    pub location: Location,
    pub widget_return: Option<bool>,
    pub framework_build: bool,
    pub override_targets: Vec<String>,
    pub return_type: Type,
    pub returns: Vec<Use>,
}
