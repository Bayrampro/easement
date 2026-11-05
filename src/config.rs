use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::check::RuleId;
use crate::decision::Grant;
use crate::lang::registry;
use crate::stdlib::STD;
use anyhow::{Context, Result, anyhow, bail};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;
use serde_norway::{Mapping, Value};

pub const DEFAULT_CONFIG: &str = "easement.yml";
pub const DEFAULT_BASELINE: &str = "easement.baseline.yml";

/// Where editors find the schema that `easement schema` prints.
const SCHEMA_ID: &str =
    "https://raw.githubusercontent.com/mokevnin/easement/main/easement.schema.json";

/// The JSON Schema of `easement.yml`, for editors: the config types with their
/// doc comments as descriptions.
pub fn schema() -> Schema {
    let mut settings = schemars::generate::SchemaSettings::draft07();
    settings
        .transforms
        .push(Box::new(schemars::transform::RecursiveTransform(
            unwrap_lines,
        )));
    settings.into_generator().into_root_schema_for::<Config>()
}

/// A doc comment keeps its line breaks; an editor shows a description as one
/// paragraph, so only a blank line stays a break.
fn unwrap_lines(schema: &mut Schema) {
    if let Some(serde_json::Value::String(text)) = schema.get_mut("description") {
        *text = text
            .split("\n\n")
            .map(|paragraph| paragraph.replace('\n', " "))
            .collect::<Vec<_>>()
            .join("\n\n");
    }
}

/// What the file holds besides the fields of [`Config`]: `extends`, which is
/// merged away before the config is read, and the free `x-` keys for anchors. A
/// file that `extends` another may leave out even `version` and `components`,
/// so the top level requires nothing.
fn config_schema_extras(schema: &mut Schema) {
    schema.insert("$id".into(), SCHEMA_ID.into());
    schema.insert("title".into(), "easement.yml".into());
    schema.remove("required");
    schema.insert(
        "patternProperties".into(),
        serde_json::json!({ "^x-": {
            "description": "Free keys that only hold YAML anchors: `x-ban: &ban {...}`, then `<<: *ban` in a rule."
        }}),
    );
    if let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        properties.insert(
            "extends".into(),
            serde_json::json!({
                "description": "Config files merged underneath this one, relative to this file. Patterns stay relative to the root of the config being loaded.",
                "anyOf": [
                    { "type": "string" },
                    { "type": "array", "items": { "type": "string" } }
                ]
            }),
        );
    }
}

/// `warn` takes the rules about the code, not those about the config.
fn warn_schema(_: &mut SchemaGenerator) -> Schema {
    let rules: Vec<&str> = RuleId::ALL
        .into_iter()
        .filter(|rule| !rule.is_config_rule())
        .map(RuleId::as_str)
        .collect();
    json_schema!({
        "type": "array",
        "items": { "enum": rules }
    })
}

/// Configuration of the easement architecture quality gate.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = config_schema_extras)]
pub struct Config {
    /// Config format version.
    #[schemars(extend("const" = 1))]
    pub version: u32,
    /// Named parts of the system and the paths they cover.
    #[schemars(extend("minProperties" = 1))]
    pub components: BTreeMap<String, Component>,
    /// Components every other component may depend on.
    #[serde(default)]
    pub common: Vec<String>,
    /// Per component: what it may depend on and use.
    #[serde(default)]
    pub rules: BTreeMap<String, Rule>,
    /// Names that no checked file may use, components and files that belong to none alike,
    /// as `package.Name` or `import/path.Name`. A component's own `forbid_symbol` adds to
    /// it. An item may carry its own reason.
    #[serde(default)]
    pub forbid_symbol: Vec<Listed>,
    /// Globs of paths to skip.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Globs of files (generated code) that stay in the graph as targets, so components can
    /// own them and rules can allow or forbid importing them, but are never checked as the
    /// source of a dependency. `exclude` wins over it.
    #[serde(default)]
    pub target_only: Vec<String>,
    /// Path of the baseline file, default easement.baseline.yml.
    #[serde(default)]
    pub baseline: Option<PathBuf>,
    /// Fail on dependency cycles between components: true for all, or a list of components.
    #[serde(default)]
    pub no_cycles: NoCycles,
    /// What `must_not_reach` follows: `files` (the default) takes an import for everything
    /// its file imports; `symbols` follows the definitions a call references, so calling
    /// one pure method does not reach what another method of its file uses. A file the
    /// index gave no ranges keeps reaching as a whole.
    #[serde(default)]
    pub reach: Reach,
    /// Components from the top layer to the bottom one: a layer may depend on any layer below
    /// it, never on one above. A list in place of a name holds sibling components of one
    /// layer, which do not depend on each other; {name, closed: true} is a layer the ones
    /// above cannot reach past.
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// Fail on checked files that belong to no component, except those matching `unowned`.
    #[serde(default)]
    pub strict: bool,
    /// Globs of files and directories that may belong to no component in strict mode.
    #[serde(default)]
    pub unowned: Vec<String>,
    /// Source files that no indexer covers and that may stay that way (globs, each with the
    /// reason). Others are listed in the report as not covered, and the result is marked
    /// partial.
    #[serde(default)]
    pub uncovered_ok: Vec<Accepted>,
    /// How the Go indexer builds the project.
    #[serde(default)]
    pub go: GoSettings,
    /// `auto` puts the test files of every language (`*_test.go`, `*.test.ts`, `test_*.py`,
    /// `*Test.java`, …) into a component named `tests`, which `rules.tests` can restrict.
    /// Without such a rule tests may import anything.
    #[serde(default)]
    pub tests: Option<TestsMode>,
    /// Files the program starts from (cmd/**, src/main.ts). With it, a checked file of a
    /// component that no chain of imports from them, from a test or from a file of no
    /// component reaches fails as unused. Go is judged by package; Kotlin, Swift and Ruby are
    /// not judged.
    #[serde(default)]
    pub entry: Vec<String>,
    /// A Markdown file with a Mermaid graph or flowchart of the components (or a .mmd file).
    /// A dependency between components the diagram has no arrow for fails as diagram. Nodes
    /// match components by id or label.
    #[serde(default)]
    pub diagram: Option<PathBuf>,
    /// Every external module a file imports has to be a dependency of the nearest
    /// package.json, go.mod or Cargo.toml above it, else it fails as undeclared. Other
    /// languages are not checked.
    #[serde(default)]
    pub manifests: bool,
    /// Manifests are checked files: each dependency a `Cargo.toml`, `package.json` or `go.mod`
    /// declares is a dependency of the component that owns it, on a package of the project or
    /// on an external module, so the rules apply to it. A dev-dependency is test code, which
    /// `tests: auto` takes. Other manifests are not read yet.
    #[serde(default)]
    pub packages: bool,
    /// Rules whose violations are reported as warnings and do not fail the check. Rules about
    /// the config, such as unmatched, cannot be listed.
    #[serde(default)]
    #[schemars(schema_with = "warn_schema")]
    pub warn: Vec<String>,
}

/// Go files behind a build constraint (`//go:build race`) are indexed, and so
/// seen, only for the tags the indexer builds with.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoSettings {
    /// Build tags to index with, for files behind `//go:build`. Files behind other tags are
    /// not built, so they count as not covered.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TestsMode {
    Auto,
}

/// The component that `tests: auto` creates.
pub const TESTS: &str = "tests";

/// A name in a rule's list, optionally with a reason of its own that is shown
/// instead of the reason of the whole rule.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Listed {
    Plain(String),
    Reasoned(Reasoned),
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reasoned {
    name: String,
    /// Why this item is listed; shown instead of the reason of the whole rule.
    #[serde(default)]
    because: Option<String>,
}

impl Listed {
    pub fn name(&self) -> &str {
        match self {
            Listed::Plain(name) | Listed::Reasoned(Reasoned { name, .. }) => name,
        }
    }

    pub fn because(&self) -> Option<&str> {
        match self {
            Listed::Plain(_) => None,
            Listed::Reasoned(Reasoned { because, .. }) => because.as_deref(),
        }
    }
}

/// An `uncovered_ok` entry: a glob, or `name` with one glob or a list of them
/// that share a reason.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Accepted {
    Plain(String),
    Reasoned(AcceptedGlobs),
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptedGlobs {
    /// A glob or a list of globs.
    name: OneOrMany,
    /// Why the files may stay uncovered. Read by people, not by the check.
    #[serde(default, rename = "because")]
    _because: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl Accepted {
    pub fn patterns(&self) -> Vec<&str> {
        match self {
            Accepted::Plain(name) => vec![name],
            Accepted::Reasoned(AcceptedGlobs { name, .. }) => match name {
                OneOrMany::One(name) => vec![name],
                OneOrMany::Many(names) => names.iter().map(String::as_str).collect(),
            },
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NoCycles {
    #[default]
    Off,
    All(bool),
    Among(Vec<String>),
}

impl NoCycles {
    pub fn covers(&self, component: &str) -> bool {
        match self {
            NoCycles::Off => false,
            NoCycles::All(on) => *on,
            NoCycles::Among(list) => list.iter().any(|c| c == component),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// A glob or a list of globs naming a module: a directory or a file. A named placeholder
    /// such as <feature> matches one directory like *, and both ends of an import that name
    /// it must fill it the same way.
    #[serde(rename = "in")]
    pub patterns: Patterns,
    /// When set, other components may import only these paths.
    #[serde(default)]
    pub public: Option<Patterns>,
    /// Why the public API or the importer list exists; shown with their violations.
    #[serde(default)]
    pub because: Option<String>,
    /// Files matching these patterns do not belong to this component, even though `in`
    /// matches them.
    #[serde(default)]
    pub except: Option<Patterns>,
    /// Which component wins when several match a file: the higher priority, then the longer
    /// pattern. Default 0.
    #[serde(default)]
    pub priority: i32,
    /// The directories matched by `in` (each pattern needs a `*`, no `**`) are instances that
    /// must not import each other.
    #[serde(default)]
    pub isolated: bool,
    /// When set, other components may use only these names from this component's packages.
    /// Needs an adapter that reports symbols (Go).
    #[serde(default)]
    pub public_symbols: Option<Vec<String>>,
    /// When set, only these components (and the component itself) may import it.
    #[serde(default)]
    pub allow_importers: Option<Vec<String>>,
    /// The component may match no file. Otherwise an `in` pattern that matches no file of the
    /// project fails as `unmatched`. Default false.
    #[serde(default)]
    pub optional: bool,
    /// Who to ask about the component and its rules: a team or a person. Printed with the
    /// violations of the component.
    #[serde(default)]
    pub owner: Option<String>,
    /// Globs of type names that belong in this component (*Repository): a class, struct,
    /// interface or trait so named that another component defines fails as naming. Types are
    /// read from the index; Kotlin and Swift are not seen.
    #[serde(default)]
    pub defines: Vec<String>,
    /// With `packages`, the part of the files `in` matches that the component owns, as the
    /// language or manifest names it: `feature/fake` for the Cargo feature `fake`. The file
    /// itself stays with the component that has no `part`.
    #[serde(default)]
    pub part: Option<String>,
}

/// One layer of `layers`: a component, sibling components that do not
/// depend on each other, or a closed component that the layers above it
/// cannot reach past.
#[derive(Debug, JsonSchema)]
#[schemars(untagged)]
pub enum Layer {
    One(String),
    Siblings(#[schemars(length(min = 2))] Vec<String>),
    Named(NamedLayer),
}

impl<'de> Deserialize<'de> for Layer {
    /// By the shape of the value, so a mistyped key of a named layer
    /// (`close:`) is named in the error, not lost in "no variant matched".
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match Value::deserialize(deserializer)? {
            Value::String(name) => Ok(Layer::One(name)),
            seq @ Value::Sequence(_) => serde_norway::from_value(seq)
                .map(Layer::Siblings)
                .map_err(|e| D::Error::custom(format!("a list of sibling layers: {e}"))),
            map @ Value::Mapping(_) => serde_norway::from_value(map)
                .map(Layer::Named)
                .map_err(|e| D::Error::custom(format!("a layer {{ name, closed }}: {e}"))),
            _ => Err(D::Error::custom(
                "a layer is a component name, a list of siblings or { name, closed }",
            )),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NamedLayer {
    name: String,
    /// The layers above may not reach past this one.
    #[serde(default)]
    closed: bool,
}

impl Layer {
    pub fn names(&self) -> Vec<&str> {
        match self {
            Layer::One(name) | Layer::Named(NamedLayer { name, .. }) => vec![name],
            Layer::Siblings(names) => names.iter().map(String::as_str).collect(),
        }
    }

    fn closed(&self) -> bool {
        matches!(self, Layer::Named(NamedLayer { closed: true, .. }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Patterns {
    One(String),
    Many(#[schemars(length(min = 1))] Vec<String>),
}

impl Patterns {
    pub fn as_vec(&self) -> Vec<&str> {
        match self {
            Patterns::One(p) => vec![p.as_str()],
            Patterns::Many(ps) => ps.iter().map(String::as_str).collect(),
        }
    }
}

/// What a chain of `must_not_reach` follows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    #[default]
    Files,
    Symbols,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Components this one may depend on. An item may carry its own reason, shown by
    /// `explain`.
    #[serde(default)]
    pub may_depend_on: Vec<Listed>,
    /// Components that are never allowed, even if common or layers would allow them. An item
    /// may carry its own reason.
    #[serde(default)]
    pub must_not_depend_on: Vec<Listed>,
    /// Components this component must not depend on, directly or through any chain of
    /// imports, even when every hop is allowed. An item may carry its own reason.
    #[serde(default)]
    pub must_not_reach: Vec<Listed>,
    /// External modules this component may use; unset means no restriction. A pattern covers
    /// submodules: lodash covers lodash/fp. $std stands for the standard library of the
    /// importing file's language. An item may carry its own reason.
    #[serde(default)]
    pub may_use_external: Option<Vec<Listed>>,
    /// External modules this component must not use. $std stands for the standard library of
    /// the importing file's language. An item may carry its own reason.
    #[serde(default)]
    pub forbid_external: Vec<Listed>,
    /// Names this component must not use, as `package.Name` or `import/path.Name`
    /// (`ent.Client`). Needs an adapter that reports symbols (Go). An item may carry its own
    /// reason.
    #[serde(default)]
    pub forbid_symbol: Vec<Listed>,
    /// Components that every directory of this component must import at least once. An item
    /// may carry its own reason.
    #[serde(default)]
    pub must_depend_on: Vec<Listed>,
    /// External modules that every directory of this component must use; a pattern covers
    /// submodules. An item may carry its own reason.
    #[serde(default)]
    pub must_use_external: Vec<Listed>,
    /// File names or globs that every directory of this component must contain. An item may
    /// carry its own reason.
    #[serde(default)]
    pub requires_file: Vec<Listed>,
    /// File names or globs that no file of this component may match. An item may carry its
    /// own reason.
    #[serde(default)]
    pub forbid_file: Vec<Listed>,
    /// Regular expressions that some file in every directory of this component must match,
    /// for dependencies written as comments (`//go:embed`, `goverter:converter`). An item may
    /// carry its own reason.
    #[serde(default)]
    pub requires_directive: Vec<Listed>,
    /// Most components this one may depend on.
    #[serde(default)]
    pub max_fan_out: Option<usize>,
    /// Highest instability, out / (in + out), from 0 (stable) to 1.
    #[serde(default)]
    pub max_instability: Option<f64>,
    /// Why the rule exists; shown with its violations.
    #[serde(default)]
    pub because: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let value = load_value(path, &mut Vec::new())?;
        // The merged value has no line numbers, so the error names the path to the field.
        let config: Config = serde_path_to_error::deserialize(value)
            .with_context(|| format!("invalid config {}", path.display()))?;
        let config = config.with_tests()?;
        config.validate()?;
        Ok(config)
    }

    /// Fails when `name` is no component, naming the one it is likely a typo of.
    pub fn require_component(&self, name: &str) -> Result<()> {
        if self.components.contains_key(name) {
            return Ok(());
        }
        Err(unknown_component(
            name,
            "",
            self.components.keys().map(String::as_str),
        ))
    }

    /// Whether any rule needs the uses of names, not only the imports.
    pub fn uses_symbols(&self) -> bool {
        self.components.values().any(|c| c.public_symbols.is_some())
            || !self.forbid_symbol.is_empty()
            || self.rules.values().any(|r| !r.forbid_symbol.is_empty())
    }

    /// The names that `forbid_symbol` lists, without the qualifier: `locale` of `I18n.locale`.
    pub fn forbidden_symbol_names(&self) -> std::collections::HashSet<String> {
        self.forbid_symbol
            .iter()
            .chain(
                self.rules
                    .values()
                    .flat_map(|rule| rule.forbid_symbol.iter()),
            )
            .filter_map(|listed| listed.name().rsplit_once('.'))
            .map(|(_, name)| name.to_string())
            .collect()
    }

    /// Adds the `tests` component when `tests: auto` asks for it.
    fn with_tests(mut self) -> Result<Self> {
        if self.tests.is_none() {
            return Ok(self);
        }
        if self.components.contains_key(TESTS) {
            bail!("`tests: auto` creates a component named `{TESTS}`, which is already declared");
        }
        self.components.insert(
            TESTS.to_string(),
            Component {
                patterns: Patterns::Many(
                    registry::test_patterns()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                ),
                public: None,
                because: None,
                except: Some(Patterns::Many(
                    registry::not_test_patterns()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                )),
                priority: 1_000_000,
                isolated: false,
                public_symbols: None,
                allow_importers: None,
                // A project may have no tests yet.
                optional: true,
                owner: None,
                defines: Vec::new(),
                part: None,
            },
        );
        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported config version {}, expected 1", self.version);
        }
        if self.components.is_empty() {
            bail!("config declares no components");
        }
        for name in &self.warn {
            match RuleId::ALL.into_iter().find(|r| r.as_str() == name) {
                Some(rule) if rule.is_config_rule() => bail!(
                    "`{name}` is about the config, so it cannot be a warning: fix the config instead"
                ),
                Some(_) => {}
                None => {
                    let known: Vec<&str> = RuleId::ALL
                        .into_iter()
                        .filter(|r| !r.is_config_rule())
                        .map(RuleId::as_str)
                        .collect();
                    bail!(
                        "unknown rule `{name}` in warn; one of: {}",
                        known.join(", ")
                    );
                }
            }
        }
        for tag in &self.go.tags {
            if tag.is_empty()
                || !tag
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            {
                bail!("go.tags: `{tag}` is not a build tag (letters, digits, `_` and `.`)");
            }
        }
        let known: BTreeSet<&str> = self.components.keys().map(String::as_str).collect();
        let check = |name: &str, place: &str| -> Result<()> {
            if !known.contains(name) {
                return Err(unknown_component(
                    name,
                    &format!(" in {place}"),
                    known.iter().copied(),
                ));
            }
            Ok(())
        };
        for name in &self.common {
            check(name, "common")?;
        }
        for symbol in self.forbid_symbol.iter().map(Listed::name) {
            if !symbol.contains('.') {
                bail!("forbid_symbol `{symbol}` must look like `package.Name`");
            }
        }
        for (from, rule) in &self.rules {
            check(from, "rules")?;
            for to in rule.may_depend_on.iter().map(Listed::name) {
                check(to, &format!("rules.{from}.may_depend_on"))?;
            }
            for to in rule.must_not_depend_on.iter().map(Listed::name) {
                check(to, &format!("rules.{from}.must_not_depend_on"))?;
                if rule.may_depend_on.iter().any(|c| c.name() == to) {
                    bail!("rules.{from} both allows and forbids `{to}`");
                }
            }
            for to in rule.must_not_reach.iter().map(Listed::name) {
                check(to, &format!("rules.{from}.must_not_reach"))?;
                if to == from {
                    bail!("rules.{from}.must_not_reach names `{from}` itself");
                }
                if rule.may_depend_on.iter().any(|c| c.name() == to) {
                    bail!("rules.{from} both allows `{to}` and forbids reaching it");
                }
            }
            for to in rule.must_depend_on.iter().map(Listed::name) {
                check(to, &format!("rules.{from}.must_depend_on"))?;
            }
            if let Some(name) = rule
                .may_use_external
                .iter()
                .flatten()
                .chain(&rule.forbid_external)
                .map(Listed::name)
                .find(|n| n.starts_with('$') && *n != STD)
            {
                bail!(
                    "rules.{from}: unknown `{name}`; the only `$` name is `$std`, the standard library"
                );
            }
            if rule.must_use_external.iter().any(|l| l.name() == STD) {
                bail!(
                    "rules.{from}.must_use_external: `$std` works only in may_use_external and forbid_external"
                );
            }
            for pattern in rule
                .must_use_external
                .iter()
                .chain(&rule.requires_file)
                .chain(&rule.forbid_file)
                .map(Listed::name)
            {
                globset::Glob::new(pattern)
                    .with_context(|| format!("bad pattern `{pattern}` in rules.{from}"))?;
            }
            for symbol in rule.forbid_symbol.iter().map(Listed::name) {
                if !symbol.contains('.') {
                    bail!("rules.{from}.forbid_symbol `{symbol}` must look like `package.Name`");
                }
            }
            for pattern in rule.requires_directive.iter().map(Listed::name) {
                regex::Regex::new(pattern).with_context(|| {
                    format!("bad regular expression `{pattern}` in rules.{from}.requires_directive")
                })?;
            }
            if rule
                .max_instability
                .is_some_and(|i| !(0.0..=1.0).contains(&i))
            {
                bail!("rules.{from}.max_instability must be between 0 and 1");
            }
        }
        // Named placeholders belong to `in` patterns only.
        let plain = |place: &str, patterns: Vec<&str>| -> Result<()> {
            if let Some(p) = patterns.iter().find(|p| p.contains('<')) {
                bail!("`{p}` in {place}: a placeholder like `<feature>` works only in `in`");
            }
            Ok(())
        };
        plain("exclude", self.exclude.iter().map(String::as_str).collect())?;
        plain(
            "target_only",
            self.target_only.iter().map(String::as_str).collect(),
        )?;
        plain("unowned", self.unowned.iter().map(String::as_str).collect())?;
        plain("entry", self.entry.iter().map(String::as_str).collect())?;
        plain(
            "uncovered_ok",
            self.uncovered_ok
                .iter()
                .flat_map(Accepted::patterns)
                .collect(),
        )?;
        for (name, component) in &self.components {
            if component.patterns.as_vec().is_empty() {
                bail!("component `{name}` has no `in` patterns");
            }
            if component.part.is_some() && component.isolated {
                bail!("component `{name}` owns a `part`, which cannot be `isolated`");
            }
            for pattern in component.patterns.as_vec() {
                crate::components::glob_and_placeholders(pattern)
                    .with_context(|| format!("components.{name}.in"))?;
            }
            plain(
                &format!("components.{name}.public"),
                component
                    .public
                    .as_ref()
                    .map(Patterns::as_vec)
                    .unwrap_or_default(),
            )?;
            for pattern in &component.defines {
                globset::Glob::new(pattern).with_context(|| {
                    format!("bad pattern `{pattern}` in components.{name}.defines")
                })?;
            }
            plain(
                &format!("components.{name}.except"),
                component
                    .except
                    .as_ref()
                    .map(Patterns::as_vec)
                    .unwrap_or_default(),
            )?;
            if component
                .public
                .as_ref()
                .is_some_and(|p| p.as_vec().is_empty())
            {
                bail!("component `{name}` has an empty `public` list");
            }
            for importer in component.allow_importers.iter().flatten() {
                check(importer, &format!("components.{name}.allow_importers"))?;
            }
            if component.isolated
                && component
                    .patterns
                    .as_vec()
                    .iter()
                    .any(|p| p.contains("**") || !(p.contains('*') || p.contains('<')))
            {
                bail!(
                    "component `{name}` is isolated, so every `in` pattern needs a `*` or a placeholder like `<page>` to tell the instances apart, and no `**`"
                );
            }
        }
        let mut layers = BTreeSet::new();
        if let Some(Layer::Siblings(names)) = self
            .layers
            .iter()
            .find(|l| matches!(l, Layer::Siblings(n) if n.len() < 2))
        {
            bail!(
                "layers: `{names:?}` lists fewer than two siblings; write a lone layer as its name"
            );
        }
        for name in self.layers.iter().flat_map(Layer::names) {
            check(name, "layers")?;
            if !layers.insert(name) {
                bail!("component `{name}` is listed twice in layers");
            }
        }
        if let NoCycles::Among(list) = &self.no_cycles {
            for name in list {
                check(name, "no_cycles")?;
            }
        }
        Ok(())
    }

    /// The owner of each component that names one.
    pub fn owners(&self) -> std::collections::BTreeMap<String, String> {
        self.components
            .iter()
            .filter_map(|(name, c)| Some((name.clone(), c.owner.clone()?)))
            .collect()
    }

    /// Whether violations of `rule` are warnings rather than failures.
    pub fn warns(&self, rule: RuleId) -> bool {
        self.warn.iter().any(|w| w == rule.as_str())
    }

    pub fn reason(&self, from: &str) -> Option<&str> {
        self.rules.get(from).and_then(|r| r.because.as_deref())
    }

    pub fn forbids(&self, from: &str, to: &str) -> bool {
        self.rules.get(from).is_some_and(|r| {
            r.must_not_depend_on
                .iter()
                .chain(&r.must_not_reach)
                .any(|c| c.name() == to)
        })
    }

    /// Whether `must_not_reach` of `from` names `to`: not even a chain of
    /// imports through other components may reach it.
    pub fn forbids_reach(&self, from: &str, to: &str) -> bool {
        self.rules
            .get(from)
            .is_some_and(|r| r.must_not_reach.iter().any(|c| c.name() == to))
    }

    /// The reason given with `to` in the `must_not_depend_on` or
    /// `must_not_reach` of `from`, if any.
    pub fn forbid_reason(&self, from: &str, to: &str) -> Option<&str> {
        let rule = self.rules.get(from)?;
        rule.must_not_depend_on
            .iter()
            .chain(&rule.must_not_reach)
            .find(|c| c.name() == to)?
            .because()
    }

    /// The reason given with `to` in the `may_depend_on` of `from`, if any.
    pub fn allow_reason(&self, from: &str, to: &str) -> Option<&str> {
        self.rules
            .get(from)?
            .may_depend_on
            .iter()
            .find(|c| c.name() == to)?
            .because()
    }

    /// What lets `from` depend on `to`, when something does and nothing
    /// forbids it.
    pub fn grant(&self, from: &str, to: &str) -> Option<Grant> {
        if from == to {
            return Some(Grant::SameComponent);
        }
        if self.forbids(from, to) {
            return None;
        }
        if self.tests.is_some() && from == TESTS && !self.rules.contains_key(TESTS) {
            Some(Grant::Tests)
        } else if self
            .rules
            .get(from)
            .is_some_and(|r| r.may_depend_on.iter().any(|c| c.name() == to))
        {
            Some(Grant::MayDependOn)
        } else if self.common.iter().any(|c| c == to) {
            Some(Grant::Common)
        } else if self.above(from, to) {
            Some(Grant::Layers)
        } else {
            None
        }
    }

    /// Why `layers` keep `from` off `to` although both are layers, if they do:
    /// they are siblings, or a closed layer lies between them.
    pub fn layer_block(&self, from: &str, to: &str) -> Option<String> {
        let at = |name: &str| self.layers.iter().position(|l| l.names().contains(&name));
        let (f, t) = (at(from)?, at(to)?);
        if f == t && from != to {
            return Some("they are siblings in one layer".to_string());
        }
        let closed = self.layers.get(f + 1..t)?.iter().find(|l| l.closed())?;
        Some(format!(
            "the closed layer {} is between them",
            closed.names().join(", ")
        ))
    }

    /// Whether `from` is a higher layer than `to`, with no closed layer between.
    fn above(&self, from: &str, to: &str) -> bool {
        let at = |name: &str| self.layers.iter().position(|l| l.names().contains(&name));
        // A closed layer between them stops the layers above from reaching past it.
        matches!((at(from), at(to)), (Some(f), Some(t))
            if f < t && !self.layers[f + 1..t].iter().any(Layer::closed))
    }
}

/// The error for `name`, which is no component in `known`: `place` says where it
/// was found (` in common`, or nothing), and the error names the component
/// `name` is likely a typo of (a few edits away, a transposition counting as one).
fn unknown_component<'a>(
    name: &str,
    place: &str,
    known: impl IntoIterator<Item = &'a str>,
) -> anyhow::Error {
    let limit = (name.chars().count() / 3).max(1);
    let closest = known
        .into_iter()
        .map(|k| (strsim::damerau_levenshtein(name, k), k))
        .filter(|(distance, _)| *distance <= limit)
        .min();
    match closest {
        Some((_, k)) => anyhow!("unknown component `{name}`{place}; did you mean `{k}`?"),
        None => anyhow!("unknown component `{name}`{place}"),
    }
}

/// Reads a config file and merges the files it `extends` underneath it.
///
/// `extends` is a path or a list of paths, relative to the file that names them.
/// Patterns in every file stay relative to the root of the config being loaded.
/// Keys that start with `x-` are dropped, so they can hold anchors for `<<`.
/// `components` and `rules` merge by name, the later definition replacing the
/// earlier one; `common` and `exclude` are concatenated; any other key is
/// replaced by the extending file.
fn load_value(path: &Path, stack: &mut Vec<PathBuf>) -> Result<Value> {
    let canonical = dunce::canonicalize(path)
        .with_context(|| format!("cannot read config {}", path.display()))?;
    if stack.contains(&canonical) {
        bail!("config {} extends itself", path.display());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read config {}", path.display()))?;
    let mut value = serde_norway::from_str::<Value>(&text)
        .with_context(|| format!("invalid config {}", path.display()))?;
    // `<<: *anchor` shares a part of a rule between components.
    value
        .apply_merge()
        .with_context(|| format!("invalid config {}", path.display()))?;
    let Value::Mapping(mut own) = value else {
        bail!("config {} is not a mapping", path.display());
    };
    // `x-` keys are only places to define anchors.
    own.retain(|key, _| !key.as_str().is_some_and(|k| k.starts_with("x-")));
    let bases = match own.remove("extends") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(one)) => vec![one],
        Some(other) => serde_norway::from_value::<Vec<String>>(other)
            .with_context(|| format!("`extends` in {} must be a path or a list", path.display()))?,
    };
    stack.push(canonical);
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut merged = Mapping::new();
    for base in bases {
        let Value::Mapping(base) = load_value(&dir.join(base), stack)? else {
            unreachable!("load_value returns mappings");
        };
        merge(&mut merged, base);
    }
    stack.pop();
    merge(&mut merged, own);
    Ok(Value::Mapping(merged))
}

fn merge(into: &mut Mapping, from: Mapping) {
    for (key, value) in from {
        let name = key.as_str().unwrap_or_default();
        match (into.get_mut(&key), value) {
            (Some(Value::Mapping(old)), Value::Mapping(new))
                if name == "components" || name == "rules" =>
            {
                for (k, v) in new {
                    old.insert(k, v);
                }
            }
            (Some(Value::Sequence(old)), Value::Sequence(new))
                if matches!(name, "common" | "exclude" | "target_only") =>
            {
                for v in new {
                    if !old.contains(&v) {
                        old.push(v);
                    }
                }
            }
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extends_merges_by_name_and_detects_loops() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        write(
            "base.yml",
            "version: 1
components:
  a: { in: a }
  b: { in: b }
common: [b]
exclude: [gen]
target_only: ['**/*.gen.ts']
rules:
  a: { may_depend_on: [b], because: base }
",
        );
        write(
            "easement.yml",
            "extends: base.yml
components:
  c: { in: c }
  b: { in: other/b }
common: [c]
exclude: [vendor]
target_only: ['**/*.pb.go']
rules:
  a: { because: own }
",
        );
        let c = Config::load(&dir.path().join("easement.yml")).unwrap();
        assert_eq!(c.components.len(), 3);
        assert_eq!(c.components["b"].patterns.as_vec(), ["other/b"]);
        assert_eq!(c.common, ["b", "c"]);
        assert_eq!(c.exclude, ["gen", "vendor"]);
        assert_eq!(c.target_only, ["**/*.gen.ts", "**/*.pb.go"]);
        // The rule of the extending file replaces the base one as a whole.
        assert_eq!(c.reason("a"), Some("own"));
        assert!(c.rules["a"].may_depend_on.is_empty());

        write("loop.yml", "extends: loop.yml\nversion: 1\n");
        let err = Config::load(&dir.path().join("loop.yml")).unwrap_err();
        assert!(err.to_string().contains("extends itself"));
    }

    #[test]
    fn a_top_level_forbid_symbol_counts_as_a_use_of_names_and_must_name_a_package() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("easement.yml");
        std::fs::write(
            &path,
            "version: 1
components:
  a: { in: a }
forbid_symbol: [\"d.T\"]
",
        )
        .unwrap();
        let c = Config::load(&path).unwrap();
        assert!(c.uses_symbols());
        assert!(c.forbidden_symbol_names().contains("T"));
        std::fs::write(
            &path,
            "version: 1
components:
  a: { in: a }
forbid_symbol: [T]
",
        )
        .unwrap();
        let error = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(error.contains("must look like `package.Name`"), "{error}");
    }

    #[test]
    fn merge_keys_share_a_part_of_several_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("easement.yml");
        std::fs::write(
            &path,
            "version: 1
components:
  a: { in: a }
  b: { in: b }
  c: { in: c }
x-ban: &ban
  forbid_symbol:
    - { name: \"d.T\", because: no raw clients }
rules:
  a: { <<: *ban, may_depend_on: [b] }
  b: { <<: *ban }
",
        )
        .unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.rules["a"].forbid_symbol.len(), 1);
        assert_eq!(c.rules["b"].forbid_symbol[0].name(), "d.T");
        assert_eq!(c.rules["a"].may_depend_on.len(), 1);
        assert!(c.rules["b"].may_depend_on.is_empty());

        // Only keys that start with `x-` are free; a typo is still an error.
        std::fs::write(&path, "version: 1\ncomponents:\n  a: { in: a }\nban: {}\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn a_config_error_names_the_path_to_the_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("easement.yml");
        std::fs::write(
            &path,
            "version: 1\ncomponents:\n  a: { in: a }\n  b: { in: b, publik: [x] }\n",
        )
        .unwrap();
        let err = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(
            err.contains("components.b.publik: unknown field `publik`"),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_component_suggests_the_closest_name() {
        let yaml = |common: &str| {
            format!(
                "version: 1\ncomponents:\n  billing: {{ in: b }}\n  shared: {{ in: s }}\ncommon: [{common}]\n"
            )
        };
        let err = parse(&yaml("shraed")).unwrap_err().to_string();
        assert_eq!(
            err,
            "unknown component `shraed` in common; did you mean `shared`?"
        );
        // Nothing is close enough: no guess.
        let err = parse(&yaml("payments")).unwrap_err().to_string();
        assert_eq!(err, "unknown component `payments` in common");
    }

    #[test]
    fn go_tags_must_look_like_build_tags() {
        let ok = parse("version: 1\ncomponents:\n  a: { in: a }\ngo: { tags: [race, e2e_1] }\n");
        assert_eq!(ok.unwrap().go.tags, ["race", "e2e_1"]);
        for bad in ["\"a b\"", "\"a,b\"", "\"\"", "\"-x\""] {
            let yaml =
                format!("version: 1\ncomponents:\n  a: {{ in: a }}\ngo: {{ tags: [{bad}] }}\n");
            assert!(parse(&yaml).is_err(), "{bad}");
        }
    }

    fn parse(yaml: &str) -> Result<Config> {
        let config: Config = serde_norway::from_str(yaml)?;
        let config = config.with_tests()?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn a_part_cannot_be_isolated() {
        let err = parse(
            "version: 1
components:
  a: { in: 'crates/*/Cargo.toml', part: feature/fake, isolated: true }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("part"), "{err}");
    }

    #[test]
    fn allows_self_common_and_declared() {
        let c = parse(
            "version: 1
components:
  a: { in: a }
  b: { in: b }
  c: { in: c }
  m: { in: m }
common: [m]
rules:
  a: { may_depend_on: [b] }
",
        )
        .unwrap();
        assert!(c.grant("a", "a").is_some());
        assert!(c.grant("a", "b").is_some());
        assert!(c.grant("c", "m").is_some());
        assert!(!c.grant("a", "c").is_some());
        assert!(!c.grant("b", "a").is_some());
    }

    #[test]
    fn sibling_layers_are_independent_and_a_closed_layer_cannot_be_skipped() {
        let c = parse(
            "version: 1
components:
  web: { in: web }
  api: { in: api }
  jobs: { in: jobs }
  service: { in: service }
  repo: { in: repo }
layers:
  - web
  - [api, jobs]
  - { name: service, closed: true }
  - repo
",
        )
        .unwrap();
        assert!(c.grant("web", "api").is_some());
        assert!(!c.grant("api", "jobs").is_some());
        assert!(!c.grant("jobs", "api").is_some());
        assert!(c.grant("api", "service").is_some());
        assert!(c.grant("service", "repo").is_some());
        // Nothing above `service` reaches past it.
        assert!(!c.grant("api", "repo").is_some());
        assert!(!c.grant("web", "repo").is_some());
        assert!(!c.grant("repo", "web").is_some());
        assert_eq!(
            c.layer_block("api", "repo").as_deref(),
            Some("the closed layer service is between them")
        );
        assert_eq!(
            c.layer_block("api", "jobs").as_deref(),
            Some("they are siblings in one layer")
        );
        assert_eq!(c.layer_block("service", "repo"), None);
        let typo = "version: 1\ncomponents:\n  a: { in: a }\nlayers: [{ name: a, close: true }]\n";
        assert!(format!("{:#}", parse(typo).unwrap_err()).contains("unknown field `close`"));
        let lone = "version: 1\ncomponents:\n  a: { in: a }\nlayers: [[a]]\n";
        assert!(format!("{:#}", parse(lone).unwrap_err()).contains("fewer than two siblings"));
        let twice =
            "version: 1\ncomponents:\n  a: { in: a }\n  b: { in: b }\nlayers: [a, [a, b]]\n";
        assert!(format!("{:#}", parse(twice).unwrap_err()).contains("twice"));
    }

    #[test]
    fn layers_allow_downwards_and_forbidden_wins() {
        let c = parse(
            "version: 1
components:
  top: { in: top }
  mid: { in: mid }
  low: { in: low }
  m: { in: m }
common: [m]
layers: [top, mid, low]
rules:
  top: { must_not_depend_on: [low, m] }
",
        )
        .unwrap();
        assert!(c.grant("top", "mid").is_some());
        assert!(c.grant("mid", "low").is_some());
        assert!(!c.grant("mid", "top").is_some());
        assert!(!c.grant("low", "mid").is_some());
        assert!(!c.grant("top", "low").is_some());
        assert!(!c.grant("top", "m").is_some());
        assert!(c.grant("mid", "m").is_some());
    }

    #[test]
    fn isolated_needs_a_star_and_importers_must_exist() {
        let err = parse(
            "version: 1
components:
  a: { in: src/a, isolated: true }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("isolated"));
        let err = parse(
            "version: 1
components:
  a: { in: 'src/**', isolated: true }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("isolated"));
        let err = parse(
            "version: 1
components:
  a: { in: a, allow_importers: [ghost] }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }

    #[test]
    fn listed_names_carry_their_own_reason() {
        let c = parse(
            "version: 1
components:
  a: { in: a }
  b: { in: b }
  c: { in: c }
rules:
  a:
    must_not_depend_on: [{ name: b, because: why b }, c]
    because: why a
",
        )
        .unwrap();
        assert!(c.forbids("a", "b") && c.forbids("a", "c"));
        assert_eq!(c.forbid_reason("a", "b"), Some("why b"));
        assert_eq!(c.forbid_reason("a", "c"), None);
        assert!(
            parse(
                "version: 1
components:
  a: { in: a }
rules:
  a: { forbid_external: [{ name: x, because: y, oops: z }] }
"
            )
            .is_err()
        );
    }

    #[test]
    fn checks_placeholders() {
        let error = |component: &str, extra: &str| {
            let yaml = format!("version: 1\ncomponents:\n  a: {component}\n{extra}");
            format!("{:#}", parse(&yaml).unwrap_err())
        };
        assert!(error("{ in: 'src/x<f>/ui' }", "").contains("whole segment"));
        assert!(error("{ in: 'src/<F>/ui' }", "").contains("whole segment"));
        assert!(error("{ in: 'src/**/<f>' }", "").contains("after `**`"));
        assert!(error("{ in: 'src/<f>/<f>' }", "").contains("twice"));
        assert!(error("{ in: src/a }", "exclude: ['src/<f>']\n").contains("works only in `in`"));
        assert!(error("{ in: src/a, public: 'src/<f>/api' }", "").contains("works only in `in`"));
        assert!(parse("version: 1\ncomponents:\n  a: { in: 'src/<f>', isolated: true }\n").is_ok());
    }

    #[test]
    fn checks_std_macros() {
        let error = |rule: &str| {
            let yaml = format!("version: 1\ncomponents:\n  a: {{ in: a }}\nrules:\n  a: {rule}\n");
            format!("{:#}", parse(&yaml).unwrap_err())
        };
        assert!(error("{ may_use_external: [$stdlib] }").contains("the only `$` name is `$std`"));
        assert!(error("{ must_use_external: [$std] }").contains("works only in"));
        assert!(parse("version: 1\ncomponents:\n  a: { in: a }\nrules:\n  a: { forbid_external: [$std] }\n").is_ok());
    }

    #[test]
    fn rejects_contradictions() {
        let err = parse(
            "version: 1
components:
  a: { in: a }
  b: { in: b }
rules:
  a: { may_depend_on: [b], must_not_depend_on: [b] }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("both allows and forbids"));
        let err = parse(
            "version: 1
components:
  a: { in: a }
  b: { in: b }
rules:
  a: { may_depend_on: [b], must_not_reach: [b] }
",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("both allows `b` and forbids reaching it")
        );
        let err = parse(
            "version: 1
components:
  a: { in: a }
rules:
  a: { must_not_reach: [a] }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("names `a` itself"));
        let err = parse(
            "version: 1
components:
  a: { in: a }
layers: [a, a]
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("twice"));
        let err = parse(
            "version: 1
components:
  a: { in: a }
rules:
  a: { max_instability: 1.5 }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("max_instability"));
    }

    #[test]
    fn tests_auto_adds_a_component_that_is_open_unless_ruled() {
        let open = parse(
            "version: 1
components:
  a: { in: a }
tests: auto
",
        )
        .unwrap();
        assert!(open.components.contains_key("tests"));
        assert!(open.grant("tests", "a").is_some());
        assert!(!open.grant("a", "tests").is_some());
        let ruled = parse(
            "version: 1
components:
  a: { in: a }
  helper: { in: testhelper }
tests: auto
rules:
  tests: { may_depend_on: [helper] }
",
        )
        .unwrap();
        assert!(ruled.grant("tests", "helper").is_some());
        assert!(!ruled.grant("tests", "a").is_some());
        let err = parse(
            "version: 1
components:
  tests: { in: t }
tests: auto
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("already declared"));
    }

    #[test]
    fn may_depend_on_carries_reasons() {
        let c = parse(
            "version: 1
components:
  a: { in: a }
  b: { in: b }
rules:
  a: { may_depend_on: [{ name: b, because: why b }] }
",
        )
        .unwrap();
        assert!(c.grant("a", "b").is_some());
        assert_eq!(c.allow_reason("a", "b"), Some("why b"));
    }

    #[test]
    fn rejects_unknown_component() {
        let err = parse(
            "version: 1
components:
  a: { in: a }
rules:
  a: { may_depend_on: [ghost] }
",
        )
        .unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }
}
