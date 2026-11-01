use anyhow::{Context, Result};
use globset::{Glob, GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};

use crate::config::Config;

/// Maps repository-relative paths (files or directories, `/`-separated) to components.
///
/// A pattern names a module, whether it is a directory or a file: `P` matches
/// `P` itself, everything below it (`P/**`) and a file `P.<ext>`. So
/// `src/app/cli` covers both `cli.py` and the `cli/` package. When several
/// components match, the higher `priority` wins, then the longest pattern. A
/// component skips the files its `except` patterns match.
pub struct ComponentMap {
    entries: Vec<Entry>,
    /// Component -> matchers of the paths it gives up, for components with `except`.
    except: std::collections::HashMap<String, Vec<[GlobMatcher; 3]>>,
    /// Files that may belong to no component in strict mode.
    unowned: GlobSet,
    /// Source files that may stay uncovered by an indexer.
    uncovered_ok: GlobSet,
    /// Component -> matchers of its public paths, for components that declare them.
    public: std::collections::HashMap<String, Vec<[GlobMatcher; 3]>>,
    /// Component -> what it may do with external modules, for components that restrict it.
    externals: std::collections::HashMap<String, Externals>,
}

struct Externals {
    /// `None`: any external module may be used.
    allowed: Option<GlobSet>,
    forbidden: GlobSet,
    /// The reason given with each forbidden pattern, in the order of `forbidden`.
    reasons: Vec<Option<String>>,
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

struct Entry {
    component: String,
    pattern: String,
    pattern_len: usize,
    /// Number of `/`-separated parts of the pattern: where an instance of an isolated component ends.
    parts: usize,
    priority: i32,
    isolated: bool,
    matchers: [GlobMatcher; 3],
}

impl ComponentMap {
    pub fn new(config: &Config) -> Result<Self> {
        let mut entries = Vec::new();
        let mut public = std::collections::HashMap::new();
        let mut externals = std::collections::HashMap::new();
        for (name, rule) in &config.rules {
            if rule.may_use_external.is_some() || !rule.forbid_external.is_empty() {
                externals.insert(
                    name.clone(),
                    Externals {
                        allowed: rule
                            .may_use_external
                            .as_deref()
                            .map(|p| glob_set(name, "may_use_external", p.iter().map(|l| l.name())))
                            .transpose()?,
                        forbidden: glob_set(
                            name,
                            "forbid_external",
                            rule.forbid_external.iter().map(|l| l.name()),
                        )?,
                        reasons: rule
                            .forbid_external
                            .iter()
                            .map(|l| l.because().map(str::to_string))
                            .collect(),
                    },
                );
            }
        }
        let mut except = std::collections::HashMap::new();
        for (name, component) in &config.components {
            for pattern in component.patterns.as_vec() {
                let trimmed = pattern.trim_end_matches('/');
                entries.push(Entry {
                    component: name.clone(),
                    pattern: trimmed.to_string(),
                    pattern_len: trimmed.len(),
                    parts: trimmed.split('/').count(),
                    priority: component.priority,
                    isolated: component.isolated,
                    matchers: module_matchers(name, pattern)?,
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
        let mut unowned = GlobSetBuilder::new();
        for pattern in &config.unowned {
            let pattern = pattern.trim_end_matches('/');
            for glob in [pattern.to_string(), format!("{pattern}/**")] {
                unowned.add(
                    Glob::new(&glob).with_context(|| format!("bad unowned pattern `{pattern}`"))?,
                );
            }
        }
        let mut uncovered_ok = GlobSetBuilder::new();
        for accepted in &config.uncovered_ok {
            for pattern in accepted.patterns() {
                let pattern = pattern.trim_end_matches('/');
                for glob in [pattern.to_string(), format!("{pattern}/**")] {
                    uncovered_ok.add(
                        Glob::new(&glob)
                            .with_context(|| format!("bad uncovered_ok pattern `{pattern}`"))?,
                    );
                }
            }
        }
        Ok(Self {
            entries,
            except,
            unowned: unowned.build()?,
            uncovered_ok: uncovered_ok.build()?,
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

    /// Whether `uncovered_ok` accepts that no indexer covers `rel_path`.
    pub fn is_uncovered_ok(&self, rel_path: &str) -> bool {
        self.uncovered_ok.is_match(rel_path)
    }

    /// Why `component` may not use the external module `specifier`, if it may not.
    pub fn external_problem(
        &self,
        component: &str,
        specifier: &str,
    ) -> Option<ExternalProblem<'_>> {
        let rules = self.externals.get(component)?;
        if let Some(index) =
            prefixes(specifier).find_map(|p| rules.forbidden.matches(p).first().copied())
        {
            return Some(ExternalProblem {
                what: "is forbidden",
                because: rules.reasons[index].as_deref(),
            });
        }
        match &rules.allowed {
            Some(allowed) if !prefixes(specifier).any(|p| allowed.is_match(p)) => {
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

    pub fn component_of(&self, rel_path: &str) -> Option<&str> {
        self.matching(rel_path).map(|e| e.component.as_str())
    }

    /// The component of `rel_path` and the `in` pattern that put it there.
    pub fn component_and_pattern(&self, rel_path: &str) -> Option<(&str, &str)> {
        self.matching(rel_path)
            .map(|e| (e.component.as_str(), e.pattern.as_str()))
    }

    fn matching(&self, rel_path: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.matchers.iter().any(|m| m.is_match(rel_path)))
            .filter(|e| {
                !self
                    .except
                    .get(&e.component)
                    .is_some_and(|ms| ms.iter().any(|ms| ms.iter().any(|m| m.is_match(rel_path))))
            })
            .max_by_key(|e| (e.priority, e.pattern_len))
    }

    /// For a path in an isolated component: the instance it belongs to, such
    /// as `src/pages/contacts` for `src/pages/*`. A file `contacts.tsx` and a
    /// directory `contacts/` are the same instance.
    pub fn instance_of(&self, rel_path: &str) -> Option<String> {
        let entry = self.matching(rel_path).filter(|e| e.isolated)?;
        let segments: Vec<&str> = rel_path.split('/').collect();
        let n = entry.parts.min(segments.len());
        let instance = segments[..n].join("/");
        if segments.len() == n
            && let Some((stem, ext)) = instance.rsplit_once('.')
            && !stem.ends_with('/')
            && !ext.is_empty()
            && ext.chars().all(|c| c.is_ascii_alphabetic())
        {
            return Some(stem.to_string());
        }
        Some(instance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(yaml: &str) -> ComponentMap {
        let config: Config = serde_norway::from_str(yaml).unwrap();
        ComponentMap::new(&config).unwrap()
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
        assert!(m.external_problem("domain", "moment/locale/ru").is_some());
        assert!(m.external_problem("domain", "@acme/sdk/v2").is_some());
        assert!(m.external_problem("domain", "lodash").is_none());
        assert!(m.external_problem("ui", "react-dom").is_some());
        assert!(m.external_problem("ui", "react/jsx-runtime").is_none());
        assert!(m.external_problem("ui", "java.util.List").is_none());
        assert!(m.external_problem("ui", "java.io.File").is_some());
        assert!(m.external_problem("other", "anything").is_none());
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
