use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::components::ComponentMap;
use crate::config::Config;
use crate::decision::{self, Refusal};

/// A path as the config sees it: relative to the project root, `/`-separated.
/// A path that exists relative to the working directory is taken from there,
/// anything else is already relative to the root.
pub fn root_relative(root: &Path, given: &Path) -> String {
    let resolved = dunce::canonicalize(given)
        .ok()
        .and_then(|abs| abs.strip_prefix(root).map(Path::to_path_buf).ok())
        .unwrap_or_else(|| PathBuf::from(given.to_string_lossy().trim_start_matches("./")));
    crate::scan::slash(&resolved)
}

/// Says which component a path belongs to and what that component may do.
pub fn explain(config: &Config, map: &ComponentMap, rel: &str) -> Result<String> {
    let Some((name, pattern)) = map.component_and_pattern(rel) else {
        bail!("{rel} belongs to no component, so easement does not check it");
    };
    let mut out = format!("{rel}\n  component: {name} (matches `{pattern}`)\n");
    let component = &config.components[name];
    if let Some(public) = &component.public {
        out.push_str(&format!(
            "  public API: {} (other components may import only these)\n",
            public.as_vec().join(", ")
        ));
    }
    if let Some(owner) = &component.owner {
        out.push_str(&format!("  owner: {owner}\n"));
    }
    if component.priority != 0 {
        out.push_str(&format!("  priority: {}\n", component.priority));
    }
    if let Some(except) = &component.except {
        out.push_str(&format!("  except: {}\n", except.as_vec().join(", ")));
    }
    if component.isolated {
        out.push_str("  isolated: instances of this component must not import each other\n");
        if let Some(instance) = map.instance_of(rel) {
            out.push_str(&format!("  instance: {instance}\n"));
        }
    }
    if let Some(importers) = &component.allow_importers {
        out.push_str(&format!(
            "  may be imported only from: {}\n",
            importers.join(", ")
        ));
    }
    let rule = config.rules.get(name);
    let mut allowed = Vec::new();
    let mut forbidden = Vec::new();
    let mut other = Vec::new();
    for to in config.components.keys().filter(|c| c.as_str() != name) {
        let verdict = decision::pair(config, map, name, to);
        match (&verdict.grant, verdict.importers) {
            (Err(Refusal::Prohibited { reach }), _) => forbidden.push(if *reach {
                format!("{to} (not even through others)")
            } else {
                to.clone()
            }),
            (Ok(grant), None) => {
                let because = verdict
                    .because
                    .map(|b| format!(": {b}"))
                    .unwrap_or_default();
                let conditions: String = verdict
                    .conditions(to)
                    .iter()
                    .map(|c| format!("; {c}"))
                    .collect();
                allowed.push(format!("{to} ({}{because}{conditions})", grant.as_str()));
            }
            (Ok(_), Some(importers)) => other.push(format!(
                "{to} (may be imported only from {})",
                importers.join(", ")
            )),
            (Err(Refusal::NotGranted { .. }), _) => other.push(to.clone()),
        }
    }
    let list = |items: &[String]| {
        if items.is_empty() {
            "nothing".to_string()
        } else {
            items.join(", ")
        }
    };
    out.push_str(&format!("  may depend on: {}\n", list(&allowed)));
    if !forbidden.is_empty() {
        out.push_str(&format!("  forbidden: {}\n", forbidden.join(", ")));
    }
    if !other.is_empty() {
        out.push_str(&format!("  not allowed: {}\n", other.join(", ")));
    }
    if let Some(rule) = rule {
        if let Some(external) = &rule.may_use_external {
            out.push_str(&format!(
                "  external modules: only {}\n",
                list(
                    &external
                        .iter()
                        .map(|l| l.name().to_string())
                        .collect::<Vec<_>>()
                )
            ));
        }
        if !rule.forbid_external.is_empty() {
            out.push_str(&format!(
                "  forbidden external modules: {}\n",
                rule.forbid_external
                    .iter()
                    .map(|l| l.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let names = |items: &[crate::config::Listed]| {
            items
                .iter()
                .map(|l| l.name())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !rule.must_depend_on.is_empty() {
            out.push_str(&format!(
                "  every directory must import: {}\n",
                names(&rule.must_depend_on)
            ));
        }
        if !rule.must_use_external.is_empty() {
            out.push_str(&format!(
                "  every directory must use external: {}\n",
                names(&rule.must_use_external)
            ));
        }
        if !rule.requires_file.is_empty() {
            out.push_str(&format!(
                "  every directory must contain: {}\n",
                names(&rule.requires_file)
            ));
        }
        if !rule.forbid_file.is_empty() {
            out.push_str(&format!(
                "  forbidden files: {}\n",
                names(&rule.forbid_file)
            ));
        }
        if !rule.requires_directive.is_empty() {
            out.push_str(&format!(
                "  every directory must match: {}\n",
                names(&rule.requires_directive)
            ));
        }
        if let Some(max) = rule.max_fan_out {
            out.push_str(&format!("  max_fan_out: {max}\n"));
        }
        if let Some(max) = rule.max_instability {
            out.push_str(&format!("  max_instability: {max}\n"));
        }
        if let Some(because) = &rule.because {
            out.push_str(&format!("  because: {because}\n"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_allowed_forbidden_and_unmapped() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  web: { in: src/web }
  core: { in: src/core }
  data: { in: src/data, public: src/data/api, allow_importers: [core], priority: 2 }
  pages: { in: 'src/pages/*', isolated: true }
  shared: { in: src/shared }
common: [shared]
layers: [web, core]
rules:
  web:
    must_not_depend_on: [data]
    must_not_reach: [pages]
    forbid_external: [moment]
    requires_file: [README.md]
    because: pages go through core
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let out = explain(&config, &map, "src/web/page.ts").unwrap();
        assert!(out.contains("component: web (matches `src/web`)"));
        assert!(out.contains("may depend on: core (layers), shared (common)"));
        assert!(out.contains("forbidden: data, pages (not even through others)"));
        assert!(out.contains("forbidden external modules: moment"));
        assert!(out.contains("every directory must contain: README.md"));
        assert!(out.contains("because: pages go through core"));
        let data = explain(&config, &map, "src/data/db.ts").unwrap();
        assert!(data.contains("public API: src/data/api"));
        assert!(data.contains("may be imported only from: core"));
        assert!(data.contains("priority: 2"));
        let page = explain(&config, &map, "src/pages/home/a.ts").unwrap();
        assert!(page.contains("instance: src/pages/home"));
        assert!(data.contains("not allowed: core, pages, web"));
        assert!(explain(&config, &map, "stray.ts").is_err());
    }

    #[test]
    fn an_allowed_component_names_its_surface_and_importers_it_does_not_list() {
        let config: Config = serde_norway::from_str(
            "version: 1
components:
  web: { in: src/web }
  billing: { in: src/billing, public: src/billing/api }
  data: { in: src/data, allow_importers: [billing] }
rules:
  web: { may_depend_on: [billing, data] }
",
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let out = explain(&config, &map, "src/web/page.ts").unwrap();
        assert!(
            out.contains(
                "may depend on: billing (may_depend_on; only through the public surface of billing: src/billing/api)\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("not allowed: data (may be imported only from billing)\n"),
            "{out}"
        );
    }
}
