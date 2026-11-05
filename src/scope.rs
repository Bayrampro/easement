use std::collections::BTreeSet;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::components::ComponentMap;
use crate::config::Config;

/// What a run reports on: the whole project, or the files of some components
/// and/or paths. The whole project is still read, because cycles, limits and
/// every edge need all of it; the scope narrows the report and its counters.
#[derive(Default)]
pub struct Scope {
    components: BTreeSet<String>,
    paths: Option<GlobSet>,
}

impl Scope {
    pub fn new(config: &Config, components: &[String], paths: &[String]) -> Result<Self> {
        for name in components {
            config.require_component(name).context("--component")?;
        }
        let paths = if paths.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for path in paths {
                let path = path.trim_start_matches("./").trim_end_matches('/');
                for glob in [path.to_string(), format!("{path}/**")] {
                    builder.add(Glob::new(&glob)?);
                }
            }
            Some(builder.build()?)
        };
        Ok(Self {
            components: components.iter().cloned().collect(),
            paths,
        })
    }

    pub fn is_whole(&self) -> bool {
        self.components.is_empty() && self.paths.is_none()
    }

    pub fn contains_owner(&self, file: &str, owner: Option<&str>) -> bool {
        (self.components.is_empty() || owner.is_some_and(|name| self.components.contains(name)))
            && self.paths.as_ref().is_none_or(|paths| paths.is_match(file))
    }

    /// Missing facts may belong to any configured part of a source file.
    pub fn contains_coverage(&self, map: &ComponentMap, file: &str) -> bool {
        (self.components.is_empty()
            || map
                .possible_components_of_file(file)
                .iter()
                .any(|component| self.components.contains(*component)))
            && self.paths.as_ref().is_none_or(|paths| paths.is_match(file))
    }

    /// Whether `file` is reported on: it matches every restriction that was given.
    pub fn contains(&self, map: &ComponentMap, file: &str) -> bool {
        (self.components.is_empty()
            || map
                .component_of(file)
                .is_some_and(|c| self.components.contains(c)))
            && self.paths.as_ref().is_none_or(|p| p.is_match(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricts_by_component_and_path() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  web: { in: web }
  api: { in: api }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let all = Scope::default();
        assert!(all.contains(&map, "anything/x.ts"));

        let web = Scope::new(&config, &["web".into()], &[]).unwrap();
        assert!(web.contains(&map, "web/a.ts"));
        assert!(!web.contains(&map, "api/a.ts"));
        assert!(!web.contains(&map, "other/a.ts"));

        let path = Scope::new(&config, &[], &["web/pages".into()]).unwrap();
        assert!(path.contains(&map, "web/pages/a.ts"));
        assert!(!path.contains(&map, "web/other/a.ts"));

        let both = Scope::new(&config, &["web".into()], &["api".into()]).unwrap();
        assert!(!both.contains(&map, "web/a.ts"));

        let err = Scope::new(&config, &["ghost".into()], &[]).err().unwrap();
        assert_eq!(format!("{err:#}"), "--component: unknown component `ghost`");
    }
}
