use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_norway::{Mapping, Value};

pub const DEFAULT_CONFIG: &str = "easement.yml";
pub const DEFAULT_BASELINE: &str = "easement.baseline.yml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub components: BTreeMap<String, Component>,
    #[serde(default)]
    pub common: Vec<String>,
    #[serde(default)]
    pub rules: BTreeMap<String, Rule>,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Globs of files that stay in the graph as targets, so components can own them and
    /// rules can forbid or allow importing them, but are never checked as the source of
    /// a dependency (generated code). `exclude` wins over it.
    #[serde(default)]
    pub target_only: Vec<String>,
    #[serde(default)]
    pub baseline: Option<PathBuf>,
    /// Fail on dependency cycles between components: `true` for all of them,
    /// or a list to limit the check.
    #[serde(default)]
    pub no_cycles: NoCycles,
    /// Components from the top layer to the bottom one: a layer may depend on
    /// any layer below it, never on one above.
    #[serde(default)]
    pub layers: Vec<String>,
    /// Fail on files that belong to no component.
    #[serde(default)]
    pub strict: bool,
    /// Globs of files that may belong to no component in strict mode.
    #[serde(default)]
    pub unowned: Vec<String>,
    /// Source files that no indexer covers and that may stay that way (globs,
    /// each with the reason): they are left out of the report of uncovered files.
    #[serde(default)]
    pub uncovered_ok: Vec<Accepted>,
    /// How the Go indexer builds the project.
    #[serde(default)]
    pub go: GoSettings,
    /// `auto` puts the test files of every language into a component named
    /// `tests`, which `rules.tests` can restrict; without such a rule tests may
    /// import anything.
    #[serde(default)]
    pub tests: Option<TestsMode>,
}

/// Go files behind a build constraint (`//go:build race`) are indexed, and so
/// seen, only for the tags the indexer builds with.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoSettings {
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestsMode {
    Auto,
}

/// The component that `tests: auto` creates.
pub const TESTS: &str = "tests";

/// Test files by the conventions of the supported languages.
const TEST_PATTERNS: &[&str] = &[
    "**/*_test.go",
    "**/*.test.*",
    "**/*.spec.*",
    "**/__tests__/**",
    "**/test_*.py",
    "**/*_test.py",
    "**/*Test.java",
    "**/*Tests.java",
    "**/*Test.kt",
    "**/*Tests.kt",
    "**/*Test.cs",
    "**/*Tests.cs",
    "**/*Test.php",
    "**/*Tests.swift",
];

/// A name in a rule's list, optionally with a reason of its own that is shown
/// instead of the reason of the whole rule.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Listed {
    Plain(String),
    Reasoned(Reasoned),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reasoned {
    name: String,
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
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Accepted {
    Plain(String),
    Reasoned(AcceptedGlobs),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedGlobs {
    name: OneOrMany,
    /// Read by people, not by the check.
    #[serde(default, rename = "because")]
    _because: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Default, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    #[serde(rename = "in")]
    pub patterns: Patterns,
    /// When set, other components may import only these paths of this component.
    #[serde(default)]
    pub public: Option<Patterns>,
    /// Why the public API or the importer list exists; shown with their violations.
    #[serde(default)]
    pub because: Option<String>,
    /// Files matching these patterns do not belong to this component, even
    /// though `in` matches them.
    #[serde(default)]
    pub except: Option<Patterns>,
    /// Which component wins when several match a file: the higher priority,
    /// then the longer pattern.
    #[serde(default)]
    pub priority: i32,
    /// The directories matched by `in` are instances that must not import each other.
    #[serde(default)]
    pub isolated: bool,
    /// When set, other components may use only these names from this component's
    /// packages (`Scoped`, `Tx`). Needs an adapter that reports symbols (Go).
    #[serde(default)]
    pub public_symbols: Option<Vec<String>>,
    /// When set, only these components (and the component itself) may import it.
    #[serde(default)]
    pub allow_importers: Option<Vec<String>>,
    /// The component may match no file; otherwise an `in` pattern that
    /// matches nothing is an `unmatched` violation.
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Patterns {
    One(String),
    Many(Vec<String>),
}

impl Patterns {
    pub fn as_vec(&self) -> Vec<&str> {
        match self {
            Patterns::One(p) => vec![p.as_str()],
            Patterns::Many(ps) => ps.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub may_depend_on: Vec<Listed>,
    /// Components that are never allowed, even if `common` or `layers` would allow them.
    #[serde(default)]
    pub must_not_depend_on: Vec<Listed>,
    /// External modules the component may use; unset means no restriction.
    #[serde(default)]
    pub may_use_external: Option<Vec<Listed>>,
    /// External modules the component must not use.
    #[serde(default)]
    pub forbid_external: Vec<Listed>,
    /// Names the component must not use, as `package.Name` or `import/path.Name`:
    /// `ent.Client`, `entgo.io/ent.Client`. Needs an adapter that reports symbols (Go).
    #[serde(default)]
    pub forbid_symbol: Vec<Listed>,
    /// Components that every directory of this component must import at least once.
    #[serde(default)]
    pub must_depend_on: Vec<Listed>,
    /// External modules that every directory of this component must use.
    #[serde(default)]
    pub must_use_external: Vec<Listed>,
    /// File names or globs that every directory of this component must contain.
    #[serde(default)]
    pub requires_file: Vec<Listed>,
    /// File names or globs that no file of this component may match.
    #[serde(default)]
    pub forbid_file: Vec<Listed>,
    /// Regular expressions that some file in every directory of this component
    /// must match, for dependencies written as comments (`//go:embed`).
    #[serde(default)]
    pub requires_directive: Vec<Listed>,
    /// Most components the component may depend on.
    #[serde(default)]
    pub max_fan_out: Option<usize>,
    /// Highest instability, `out / (in + out)`, from 0 (stable) to 1.
    #[serde(default)]
    pub max_instability: Option<f64>,
    /// Why the rule exists; shown with its violations.
    #[serde(default)]
    pub because: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let value = load_value(path, &mut Vec::new())?;
        let config: Config = serde_norway::from_value(value)
            .with_context(|| format!("invalid config {}", path.display()))?;
        let config = config.with_tests()?;
        config.validate()?;
        Ok(config)
    }

    /// Whether any rule needs the uses of names, not only the imports.
    pub fn uses_symbols(&self) -> bool {
        self.components.values().any(|c| c.public_symbols.is_some())
            || self.rules.values().any(|r| !r.forbid_symbol.is_empty())
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
                patterns: Patterns::Many(TEST_PATTERNS.iter().map(|p| p.to_string()).collect()),
                public: None,
                because: None,
                except: None,
                priority: 1_000_000,
                isolated: false,
                public_symbols: None,
                allow_importers: None,
                // A project may have no tests yet.
                optional: true,
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
                bail!("unknown component `{name}` in {place}");
            }
            Ok(())
        };
        for name in &self.common {
            check(name, "common")?;
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
            for to in rule.must_depend_on.iter().map(Listed::name) {
                check(to, &format!("rules.{from}.must_depend_on"))?;
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
        for (name, component) in &self.components {
            if component.patterns.as_vec().is_empty() {
                bail!("component `{name}` has no `in` patterns");
            }
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
                    .any(|p| p.contains("**") || !p.contains('*'))
            {
                bail!(
                    "component `{name}` is isolated, so every `in` pattern needs a `*` to tell the instances apart and no `**`"
                );
            }
        }
        let mut layers = BTreeSet::new();
        for name in &self.layers {
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

    pub fn reason(&self, from: &str) -> Option<&str> {
        self.rules.get(from).and_then(|r| r.because.as_deref())
    }

    pub fn forbids(&self, from: &str, to: &str) -> bool {
        self.rules
            .get(from)
            .is_some_and(|r| r.must_not_depend_on.iter().any(|c| c.name() == to))
    }

    /// The reason given with `to` in the `must_not_depend_on` of `from`, if any.
    pub fn forbid_reason(&self, from: &str, to: &str) -> Option<&str> {
        self.rules
            .get(from)?
            .must_not_depend_on
            .iter()
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

    pub fn allows(&self, from: &str, to: &str) -> bool {
        from == to
            || self.tests.is_some() && from == TESTS && !self.rules.contains_key(TESTS)
            || !self.forbids(from, to)
                && (self.common.iter().any(|c| c == to)
                    || self.above(from, to)
                    || self
                        .rules
                        .get(from)
                        .is_some_and(|r| r.may_depend_on.iter().any(|c| c.name() == to)))
    }

    /// Whether `from` is a higher layer than `to`.
    fn above(&self, from: &str, to: &str) -> bool {
        let at = |name: &str| self.layers.iter().position(|l| l == name);
        matches!((at(from), at(to)), (Some(f), Some(t)) if f < t)
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
        assert!(c.allows("a", "a"));
        assert!(c.allows("a", "b"));
        assert!(c.allows("c", "m"));
        assert!(!c.allows("a", "c"));
        assert!(!c.allows("b", "a"));
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
        assert!(c.allows("top", "mid"));
        assert!(c.allows("mid", "low"));
        assert!(!c.allows("mid", "top"));
        assert!(!c.allows("low", "mid"));
        assert!(!c.allows("top", "low"));
        assert!(!c.allows("top", "m"));
        assert!(c.allows("mid", "m"));
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
        assert!(open.allows("tests", "a"));
        assert!(!open.allows("a", "tests"));
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
        assert!(ruled.allows("tests", "helper"));
        assert!(!ruled.allows("tests", "a"));
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
        assert!(c.allows("a", "b"));
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
