use anyhow::{Context, Result};
use globset::{Glob, GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use serde::Serialize;

use crate::config::{Config, TESTS};
use crate::lang::Unit;
use crate::stdlib::{STD, is_std};

/// Maps repository-relative paths (files or directories, `/`-separated) to components.
///
/// A pattern names a module, whether it is a directory or a file: `P` matches
/// `P` itself, everything below it (`P/**`) and a file `P.<ext>`. So
/// `src/app/cli` covers both `cli.py` and the `cli/` package. When several
/// components match, the higher `priority` wins, then the longest pattern. A
/// component skips the files its `except` patterns match.
pub struct ComponentMap {
    entries: Vec<Entry>,
    /// Whether `tests: auto` made a `tests` component.
    tests: bool,
    /// Component -> matchers of the paths it gives up, for components with `except`.
    except: std::collections::HashMap<String, Vec<[GlobMatcher; 3]>>,
    /// Files that may belong to no component in strict mode.
    unowned: GlobSet,
    /// Files that `exclude` hides.
    excluded: GlobSet,
    /// Source files that may stay uncovered by an indexer.
    uncovered_ok: GlobSet,
    /// The `uncovered_ok` entries, in the order of the config.
    accepting: Vec<Accepting>,
    /// The index in `accepting` of the entry each glob of `uncovered_ok` comes from.
    accepting_of_glob: Vec<usize>,
    /// Component -> matchers of its public paths, for components that declare them.
    public: std::collections::HashMap<String, Vec<[GlobMatcher; 3]>>,
    /// Component -> what it may do with external modules, for components that restrict it.
    externals: std::collections::HashMap<String, Externals>,
}

/// An `uncovered_ok` entry: its globs as written and why.
struct Accepting {
    patterns: Vec<String>,
    because: Option<String>,
}

/// The files no indexer read that one `uncovered_ok` entry accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptedGap {
    /// The globs of the entry.
    pub uncovered_ok: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
    pub files: Vec<String>,
}

/// `$std` in `forbid_external`, with the reason of its item.
struct StdForbidden {
    because: Option<String>,
}

struct Externals {
    /// `None`: any external module may be used.
    allowed: Option<GlobSet>,
    /// `$std` is in `may_use_external`.
    allowed_std: bool,
    forbidden: GlobSet,
    /// The reason given with each forbidden pattern, in the order of `forbidden`.
    reasons: Vec<Option<String>>,
    /// `$std` is in `forbid_external`.
    forbidden_std: Option<StdForbidden>,
}

/// Why a component may not use an external module: what is wrong, and the
/// reason attached to the forbidden pattern, if it has one.
pub struct ExternalProblem<'a> {
    pub what: &'static str,
    pub because: Option<&'a str>,
}

fn glob_set<'a>(
    component: &str,
    key: &str,
    patterns: impl IntoIterator<Item = &'a str>,
) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern)
                .with_context(|| format!("bad pattern `{pattern}` in {key} of `{component}`"))?,
        );
    }
    Ok(builder.build()?)
}

/// A specifier and its prefixes cut at the separators of the languages (`/`,
/// `.`, `::`, `\`), so the pattern `lodash` covers `lodash/fp`, `java.util`
/// covers `java.util.List` and `serde` covers `serde::Deserialize`.
fn prefixes(specifier: &str) -> impl Iterator<Item = &str> {
    specifier
        .match_indices(['/', '.', ':', '\\'])
        .filter(|(i, sep)| *sep != ":" || specifier[i + 1..].starts_with(':'))
        .map(|(i, _)| &specifier[..i])
        .chain(std::iter::once(specifier))
}

fn module_matchers(component: &str, pattern: &str) -> Result<[GlobMatcher; 3]> {
    let pattern = pattern.trim_end_matches('/');
    let compile = |p: &str| -> Result<GlobMatcher> {
        Ok(GlobBuilder::new(p)
            .literal_separator(true)
            .build()
            .with_context(|| format!("bad pattern `{p}` in component `{component}`"))?
            .compile_matcher())
    };
    Ok([
        compile(pattern)?,
        compile(&format!("{pattern}/**"))?,
        compile(&format!("{pattern}.*"))?,
    ])
}

/// The glob of an `in` pattern, where a named placeholder (`<feature>`)
/// matches like `*`, and the placeholder names by segment index.
pub fn glob_and_placeholders(pattern: &str) -> Result<(String, Vec<(usize, String)>)> {
    let mut glob = Vec::new();
    let mut names: Vec<(usize, String)> = Vec::new();
    let mut deep = false;
    for (i, segment) in pattern.trim_end_matches('/').split('/').enumerate() {
        if !segment.contains(['<', '>']) {
            deep |= segment.contains("**");
            glob.push(segment.to_string());
            continue;
        }
        let name = segment
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .with_context(|| {
                format!("`{segment}` in `{pattern}`: a placeholder is a whole segment like `<feature>`, of a-z and _")
            })?;
        if deep {
            anyhow::bail!("`<{name}>` in `{pattern}` comes after `**`, so it has no fixed place");
        }
        if names.iter().any(|(_, n)| n == name) {
            anyhow::bail!("`<{name}>` appears twice in `{pattern}`");
        }
        names.push((i, name.to_string()));
        glob.push("*".to_string());
    }
    Ok((glob.join("/"), names))
}

struct Entry {
    component: String,
    /// The pattern as written, placeholders included.
    pattern: String,
    /// Named placeholders of the pattern by segment index.
    captures: Vec<(usize, String)>,
    pattern_len: usize,
    /// Number of `/`-separated parts of the pattern: where an instance of an isolated component ends.
    parts: usize,
    priority: i32,
    isolated: bool,
    /// The part of a file it owns, when it owns a part and not the file.
    part: Option<String>,
    matchers: [GlobMatcher; 3],
}

impl ComponentMap {
    pub fn new(config: &Config) -> Result<Self> {
        let mut entries = Vec::new();
        let mut public = std::collections::HashMap::new();
        let mut externals = std::collections::HashMap::new();
        for (name, rule) in &config.rules {
            if rule.may_use_external.is_some() || !rule.forbid_external.is_empty() {
                let std_item = |l: &&crate::config::Listed| l.name() == STD;
                let forbidden: Vec<_> = rule
                    .forbid_external
                    .iter()
                    .filter(|l| !std_item(l))
                    .collect();
                externals.insert(
                    name.clone(),
                    Externals {
                        allowed: rule
                            .may_use_external
                            .as_deref()
                            .map(|p| {
                                glob_set(
                                    name,
                                    "may_use_external",
                                    p.iter().filter(|l| !std_item(l)).map(|l| l.name()),
                                )
                            })
                            .transpose()?,
                        allowed_std: rule.may_use_external.iter().flatten().any(|l| std_item(&l)),
                        forbidden: glob_set(
                            name,
                            "forbid_external",
                            forbidden.iter().map(|l| l.name()),
                        )?,
                        reasons: forbidden
                            .iter()
                            .map(|l| l.because().map(str::to_string))
                            .collect(),
                        forbidden_std: rule.forbid_external.iter().find(std_item).map(|l| {
                            StdForbidden {
                                because: l.because().map(str::to_string),
                            }
                        }),
                    },
                );
            }
        }
        let mut except = std::collections::HashMap::new();
        for (name, component) in &config.components {
            for pattern in component.patterns.as_vec() {
                let trimmed = pattern.trim_end_matches('/');
                let (glob, captures) = glob_and_placeholders(trimmed)?;
                entries.push(Entry {
                    component: name.clone(),
                    pattern: trimmed.to_string(),
                    captures,
                    pattern_len: glob.len(),
                    parts: glob.split('/').count(),
                    priority: component.priority,
                    isolated: component.isolated,
                    part: component.part.clone(),
                    matchers: module_matchers(name, &glob)?,
                });
            }
            if let Some(patterns) = &component.except {
                let matchers = patterns
                    .as_vec()
                    .into_iter()
                    .map(|p| module_matchers(name, p))
                    .collect::<Result<Vec<_>>>()?;
                except.insert(name.clone(), matchers);
            }
            if let Some(patterns) = &component.public {
                let matchers = patterns
                    .as_vec()
                    .into_iter()
                    .map(|p| module_matchers(name, p))
                    .collect::<Result<Vec<_>>>()?;
                public.insert(name.clone(), matchers);
            }
        }
        let mut uncovered_ok = GlobSetBuilder::new();
        let mut accepting = Vec::new();
        let mut accepting_of_glob = Vec::new();
        for (entry, accepted) in config.uncovered_ok.iter().enumerate() {
            accepting.push(Accepting {
                patterns: accepted
                    .patterns()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                because: accepted.because().map(str::to_string),
            });
            for pattern in accepted.patterns() {
                let pattern = pattern.trim_end_matches('/');
                for glob in [pattern.to_string(), format!("{pattern}/**")] {
                    uncovered_ok.add(
                        Glob::new(&glob)
                            .with_context(|| format!("bad uncovered_ok pattern `{pattern}`"))?,
                    );
                    accepting_of_glob.push(entry);
                }
            }
        }
        Ok(Self {
            entries,
            tests: config.tests.is_some(),
            except,
            unowned: crate::scan::glob_set(&config.unowned, "unowned")?,
            excluded: crate::scan::glob_set(&config.exclude, "exclude")?,
            uncovered_ok: uncovered_ok.build()?,
            accepting,
            accepting_of_glob,
            public,
            externals,
        })
    }

    /// Whether any of the external modules matches the pattern, which covers
    /// submodules the way `forbid_external` does.
    pub fn external_matches<'a>(
        pattern: &str,
        modules: impl IntoIterator<Item = &'a String>,
    ) -> bool {
        let Ok(glob) = Glob::new(pattern) else {
            return false;
        };
        let matcher = glob.compile_matcher();
        modules
            .into_iter()
            .any(|module| prefixes(module).any(|p| matcher.is_match(p)))
    }

    /// Whether strict mode lets `rel_path` belong to no component.
    pub fn is_unowned(&self, rel_path: &str) -> bool {
        self.unowned.is_match(rel_path)
    }

    /// Whether `exclude` hides `rel_path`, a file or a directory: a directory
    /// is hidden when a pattern hides everything below it (`src/legacy/**`).
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        self.excluded.is_match(rel_path) || self.excluded.is_match(format!("{rel_path}/-"))
    }

    /// Whether `uncovered_ok` accepts that no indexer covers `rel_path`.
    pub fn is_uncovered_ok(&self, rel_path: &str) -> bool {
        self.uncovered_ok.is_match(rel_path)
    }

    /// The `uncovered_ok` entries that accept some of the `unread` files, each
    /// with the files it accepts, in the order of the config. A file two entries
    /// accept belongs to the first; an entry that accepts none of them is no gap.
    pub fn accepted_gaps<'a>(
        &self,
        unread: impl IntoIterator<Item = &'a String>,
    ) -> Vec<AcceptedGap> {
        let mut by_entry: std::collections::BTreeMap<usize, Vec<String>> =
            std::collections::BTreeMap::new();
        for file in unread {
            let first = self
                .uncovered_ok
                .matches(file)
                .into_iter()
                .map(|glob| self.accepting_of_glob[glob])
                .min();
            if let Some(entry) = first {
                by_entry.entry(entry).or_default().push(file.clone());
            }
        }
        by_entry
            .into_iter()
            .map(|(entry, mut files)| {
                files.sort();
                let accepting = &self.accepting[entry];
                AcceptedGap {
                    uncovered_ok: accepting.patterns.clone(),
                    because: accepting.because.clone(),
                    files,
                }
            })
            .collect()
    }

    /// Whether a rule of `component` names `$std`.
    pub fn uses_std(&self, component: &str) -> bool {
        self.externals
            .get(component)
            .is_some_and(|r| r.allowed_std || r.forbidden_std.is_some())
    }

    /// Why `component` may not use the external module `specifier`, imported
    /// by `file`, if it may not.
    pub fn external_problem(
        &self,
        component: &str,
        file: &str,
        specifier: &str,
    ) -> Option<ExternalProblem<'_>> {
        let rules = self.externals.get(component)?;
        let std = || is_std(file, specifier);
        if let Some(forbidden) = rules.forbidden_std.as_ref().filter(|_| std()) {
            return Some(ExternalProblem {
                what: "is forbidden",
                because: forbidden.because.as_deref(),
            });
        }
        if let Some(index) =
            prefixes(specifier).find_map(|p| rules.forbidden.matches(p).first().copied())
        {
            return Some(ExternalProblem {
                what: "is forbidden",
                because: rules.reasons[index].as_deref(),
            });
        }
        match &rules.allowed {
            Some(allowed)
                if !prefixes(specifier).any(|p| allowed.is_match(p))
                    && !(rules.allowed_std && std()) =>
            {
                Some(ExternalProblem {
                    what: "is not in may_use_external",
                    because: None,
                })
            }
            _ => None,
        }
    }

    /// Whether `rel_path` may be imported from outside `component`: always,
    /// unless the component declares a public API that does not cover it.
    pub fn is_public(&self, component: &str, rel_path: &str) -> bool {
        self.public.get(component).is_none_or(|matchers| {
            matchers
                .iter()
                .any(|ms| ms.iter().any(|m| m.is_match(rel_path)))
        })
    }

    /// Every `in` pattern, as (component, pattern), that matches none of
    /// `files`, in the order of the config.
    pub fn unmatched_patterns(&self, files: &[String]) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .filter(|e| {
                !files
                    .iter()
                    .any(|f| e.matchers.iter().any(|m| m.is_match(f)))
            })
            .map(|e| (e.component.as_str(), e.pattern.as_str()))
            .collect()
    }

    /// The component of a node of the graph: code only a test build has belongs to
    /// the `tests` of `tests: auto`, whatever file it sits in.
    pub fn component_of_unit(&self, unit: &Unit) -> Option<&str> {
        if self.tests && unit.is_test() {
            return Some(TESTS);
        }
        if let Some(part) = unit.part_name()
            && let Some(entry) = self.matching_part(&unit.key(), Some(part))
        {
            return Some(entry.component.as_str());
        }
        self.component_of(&unit.key())
    }

    pub fn component_of(&self, rel_path: &str) -> Option<&str> {
        self.matching(rel_path).map(|e| e.component.as_str())
    }

    /// Owners whose code an incomplete file could contain, including named
    /// parts and test-only code. Coverage must not assume an unseen part absent.
    pub fn possible_components_of_file(&self, file: &str) -> std::collections::BTreeSet<&str> {
        let mut owners: std::collections::BTreeSet<&str> =
            self.component_of(file).into_iter().collect();
        owners.extend(
            self.entries
                .iter()
                .filter(|entry| {
                    entry.part.is_some()
                        && entry.matchers.iter().any(|matcher| matcher.is_match(file))
                })
                .map(|entry| entry.component.as_str()),
        );
        if self.tests
            && crate::lang::registry::of(file).is_some_and(|language| language.parts.is_some())
        {
            owners.insert(TESTS);
        }
        owners
    }

    /// The component of `rel_path` and the `in` pattern that put it there.
    pub fn component_and_pattern(&self, rel_path: &str) -> Option<(&str, &str)> {
        self.matching(rel_path)
            .map(|e| (e.component.as_str(), e.pattern.as_str()))
    }

    fn matching(&self, rel_path: &str) -> Option<&Entry> {
        self.matching_part(rel_path, None)
    }

    /// The entry that owns `part` of `rel_path`, or the file itself for `None`.
    fn matching_part(&self, rel_path: &str, part: Option<&str>) -> Option<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.part.as_deref() == part)
            .filter(|e| e.matchers.iter().any(|m| m.is_match(rel_path)))
            .filter(|e| {
                !self
                    .except
                    .get(&e.component)
                    .is_some_and(|ms| ms.iter().any(|ms| ms.iter().any(|m| m.is_match(rel_path))))
            })
            .max_by_key(|e| (e.priority, e.pattern_len))
    }

    /// The values the named placeholders of `rel_path`'s pattern take:
    /// `feature` is `orders` for `src/features/orders/ui/View.kt` under
    /// `src/features/<feature>/ui`. A file `orders.kt` and a directory
    /// `orders/` take the same value; a path that ends above a placeholder
    /// takes none.
    pub fn captures_of(&self, rel_path: &str) -> Vec<(&str, String)> {
        let Some(entry) = self.matching(rel_path) else {
            return Vec::new();
        };
        let segments: Vec<&str> = rel_path.split('/').collect();
        entry
            .captures
            .iter()
            .filter_map(|(i, name)| {
                let segment = segments.get(*i)?;
                let value = if *i + 1 == segments.len() {
                    without_extension(segment)
                } else {
                    segment
                };
                Some((name.as_str(), value.to_string()))
            })
            .collect()
    }

    /// The named placeholders of every `in` pattern of `component`.
    pub fn placeholders(&self, component: &str) -> std::collections::BTreeSet<&str> {
        self.entries
            .iter()
            .filter(|e| e.component == component)
            .flat_map(|e| e.captures.iter().map(|(_, name)| name.as_str()))
            .collect()
    }

    /// For a path in an isolated component: the instance it belongs to, such
    /// as `src/pages/contacts` for `src/pages/*`. A file `contacts.tsx` and a
    /// directory `contacts/` are the same instance.
    pub fn instance_of(&self, rel_path: &str) -> Option<String> {
        let entry = self.matching(rel_path).filter(|e| e.isolated)?;
        let segments: Vec<&str> = rel_path.split('/').collect();
        let n = entry.parts.min(segments.len());
        let instance = segments[..n].join("/");
        if segments.len() == n {
            let last = segments[n - 1];
            let stem = without_extension(last);
            return Some(format!(
                "{}{stem}",
                &instance[..instance.len() - last.len()]
            ));
        }
        Some(instance)
    }
}

/// A file name without its extension (`contacts.tsx` is `contacts`), so a
/// file and a directory of one name are the same module.
fn without_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && !ext.is_empty()
                && ext.chars().all(|c| c.is_ascii_alphabetic()) =>
        {
            stem
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn map(yaml: &str) -> ComponentMap {
        let config: Config = serde_norway::from_str(yaml).unwrap();
        ComponentMap::new(&config).unwrap()
    }

    #[test]
    fn unread_files_are_accepted_by_the_first_uncovered_ok_entry_that_names_them() {
        let map = map("version: 1
components:
  app: { in: app }
uncovered_ok:
  - { name: [gen/**, tools/**], because: generated }
  - nothing/**
  - tools/a.go
");
        let unread: Vec<String> = ["tools/a.go", "gen/b.go", "app/c.go"]
            .map(String::from)
            .into();
        // tools/a.go is accepted by two entries and belongs to the first; an
        // entry that accepts none of the files is no gap.
        assert_eq!(
            map.accepted_gaps(&unread),
            [AcceptedGap {
                uncovered_ok: vec!["gen/**".into(), "tools/**".into()],
                because: Some("generated".into()),
                files: vec!["gen/b.go".into(), "tools/a.go".into()],
            }]
        );
    }

    #[test]
    fn test_code_of_a_file_is_tests_only_under_tests_auto() {
        use crate::lang::parts::{Kind, Part};
        let unit = Unit::in_part(
            Path::new("src/store.rs"),
            Part {
                name: "cfg(test)".to_string(),
                kind: Some(Kind::Test),
            },
        );
        let own = map("version: 1
components:
  store: { in: src/store.rs }
  tests: { in: spec }
");
        assert_eq!(own.component_of_unit(&unit), Some("store"));
        let auto = map("version: 1
components:
  store: { in: src/store.rs }
tests: auto
");
        assert_eq!(auto.component_of_unit(&unit), Some("tests"));
    }

    #[test]
    fn a_component_with_a_part_owns_only_that_part_of_a_file() {
        use crate::lang::parts::Part;
        let m = map("version: 1
components:
  crate: { in: crates/a/Cargo.toml }
  fakes: { in: 'crates/**/Cargo.toml', part: feature/fake }
");
        let part = |name: &str| {
            Unit::in_part(
                Path::new("crates/a/Cargo.toml"),
                Part {
                    name: name.to_string(),
                    kind: None,
                },
            )
        };
        assert_eq!(m.component_of_unit(&part("feature/fake")), Some("fakes"));
        assert_eq!(m.component_of_unit(&part("feature/other")), Some("crate"));
        // The file itself is not the part: another crate's manifest has no owner.
        assert_eq!(m.component_of("crates/a/Cargo.toml"), Some("crate"));
        assert_eq!(m.component_of("crates/b/Cargo.toml"), None);
    }

    #[test]
    fn a_pattern_matches_its_directory_or_a_file_of_its_name() {
        let m = map("version: 1
components:
  cli: { in: src/app/cli }
  pages: { in: 'src/pages/*' }
  ghost: { in: src/gone }
");
        let files = ["src/app/cli.py".to_string(), "src/pages/a/x.ts".to_string()];
        assert_eq!(m.unmatched_patterns(&files), vec![("ghost", "src/gone")]);
    }

    #[test]
    fn placeholders_capture_and_do_not_win_on_length() {
        let m = map("version: 1
components:
  slice: { in: 'src/features/<feature>' }
  ui: { in: 'src/features/<feature>/ui' }
  wild: { in: 'src/apps/*' }
  named: { in: 'src/apps/<app>' }
");
        assert_eq!(
            m.captures_of("src/features/orders/ui/View.kt"),
            vec![("feature", "orders".to_string())]
        );
        // A file and a directory of one name are one slice.
        assert_eq!(
            m.captures_of("src/features/orders.kt"),
            vec![("feature", "orders".to_string())]
        );
        assert!(m.captures_of("src/features").is_empty());
        // `<app>` is longer as text than `*`, but the patterns are equal.
        assert_eq!(m.component_of("src/apps/a/x.ts"), Some("wild"));
    }

    #[test]
    fn std_keeps_the_reasons_of_the_other_items_in_place() {
        let m = map("version: 1
components:
  core: { in: core }
rules:
  core:
    may_use_external: [$std, okhttp3]
    forbid_external:
      - { name: $std, because: std is wrapped }
      - { name: okhttp3.internal, because: internals change }
");
        assert!(m.uses_std("core"));
        let reason = |file, module| m.external_problem("core", file, module).map(|p| p.because);
        assert_eq!(
            reason("A.kt", "java.util.UUID"),
            Some(Some("std is wrapped"))
        );
        assert_eq!(
            reason("A.kt", "okhttp3.internal.X"),
            Some(Some("internals change"))
        );
        assert_eq!(reason("A.kt", "okhttp3.Client"), None);
        assert!(
            m.external_problem("core", "A.kt", "retrofit2.Call")
                .is_some()
        );
    }

    #[test]
    fn matches_dirs_files_and_prefers_specific() {
        let m = map("version: 1
components:
  handler: { in: 'internal/handlers/*' }
  service: { in: 'internal/services/**' }
  domain: { in: internal/domain }
  repo: { in: 'internal/domain/*/repository' }
");
        assert_eq!(m.component_of("internal/handlers/books"), Some("handler"));
        assert_eq!(
            m.component_of("internal/handlers/books/h.go"),
            Some("handler")
        );
        assert_eq!(
            m.component_of("internal/services/a/b/c.go"),
            Some("service")
        );
        assert_eq!(m.component_of("internal/domain"), Some("domain"));
        assert_eq!(
            m.component_of("internal/domain/books/model.go"),
            Some("domain")
        );
        assert_eq!(
            m.component_of("internal/domain/books/repository"),
            Some("repo")
        );
        assert_eq!(
            m.component_of("internal/domain/books/repository/pg.go"),
            Some("repo")
        );
        assert_eq!(m.component_of("internal/domain.go"), Some("domain"));
        assert_eq!(m.component_of("internal/domainx/a.go"), None);
        assert_eq!(m.component_of("cmd/main.go"), None);
    }

    #[test]
    fn external_rules_match_prefixes() {
        let m = map("version: 1
components:
  domain: { in: src/domain }
  ui: { in: src/ui }
rules:
  domain: { forbid_external: [moment, '@acme/*'] }
  ui: { may_use_external: [react, java.util] }
");
        assert!(
            m.external_problem("domain", "x.ts", "moment/locale/ru")
                .is_some()
        );
        assert!(
            m.external_problem("domain", "x.ts", "@acme/sdk/v2")
                .is_some()
        );
        assert!(m.external_problem("domain", "x.ts", "lodash").is_none());
        assert!(m.external_problem("ui", "x.ts", "react-dom").is_some());
        assert!(
            m.external_problem("ui", "x.ts", "react/jsx-runtime")
                .is_none()
        );
        assert!(m.external_problem("ui", "x.ts", "java.util.List").is_none());
        assert!(m.external_problem("ui", "x.ts", "java.io.File").is_some());
        assert!(m.external_problem("other", "x.ts", "anything").is_none());
        assert_eq!(
            prefixes("serde::de::Deserialize").collect::<Vec<_>>(),
            ["serde", "serde::de", "serde::de::Deserialize"]
        );
        assert_eq!(
            prefixes("Illuminate\\Support\\Str").collect::<Vec<_>>(),
            [
                "Illuminate",
                "Illuminate\\Support",
                "Illuminate\\Support\\Str"
            ]
        );
    }

    #[test]
    fn priority_beats_length_and_except_hands_files_on() {
        let m = map("version: 1
components:
  feature: { in: src/deep/feature, except: '**/*.stories.tsx' }
  src: { in: src }
  tests: { in: '**/*.test.ts', priority: 1 }
  long: { in: src/deep/feature/long/path }
");
        // `feature` gives its stories up, so the shorter `src` gets them.
        assert_eq!(m.component_of("src/deep/feature/a.ts"), Some("feature"));
        assert_eq!(
            m.component_of("src/deep/feature/a.stories.tsx"),
            Some("src")
        );
        // A priority outranks a longer pattern.
        assert_eq!(
            m.component_of("src/deep/feature/long/path/x.ts"),
            Some("long")
        );
        assert_eq!(
            m.component_of("src/deep/feature/long/path/x.test.ts"),
            Some("tests")
        );
    }

    #[test]
    fn instances_of_isolated_components() {
        let m = map("version: 1
components:
  pages: { in: 'src/pages/*', isolated: true }
  plain: { in: src/plain }
");
        let instance = |p: &str| m.instance_of(p);
        assert_eq!(
            instance("src/pages/contacts/index.ts").as_deref(),
            Some("src/pages/contacts")
        );
        assert_eq!(
            instance("src/pages/contacts").as_deref(),
            Some("src/pages/contacts")
        );
        // A file and a directory of the same name are one instance.
        assert_eq!(
            instance("src/pages/contacts.tsx").as_deref(),
            Some("src/pages/contacts")
        );
        assert_eq!(instance("src/plain/a.ts"), None);
        assert_eq!(instance("src/elsewhere/a.ts"), None);
    }

    #[test]
    fn unowned_covers_directories() {
        let m = map("version: 1
components:
  a: { in: a }
unowned: [gen, 'tools/*.ts']
");
        assert!(m.is_unowned("gen/x/y.ts"));
        assert!(m.is_unowned("tools/x.ts"));
        assert!(!m.is_unowned("other/x.ts"));
    }

    #[test]
    fn public_api() {
        let m = map("version: 1
components:
  billing: { in: src/billing, public: [src/billing/index, src/billing/api] }
  shop: { in: src/shop }
");
        assert!(m.is_public("billing", "src/billing/index.ts"));
        assert!(m.is_public("billing", "src/billing/api/types.ts"));
        assert!(!m.is_public("billing", "src/billing/internal/db.ts"));
        assert!(m.is_public("shop", "src/shop/anything.ts"));
    }
}
