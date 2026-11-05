//! The dependency decision: the one place where the policy judges a dependency.
//! `check` turns rejected decisions into violations, `why`, `explain` and
//! `suggest` render the verdict on a pair of components.

use crate::check::RuleId;
use crate::components::ComponentMap;
use crate::config::Config;
use crate::lang::Target;
use crate::scan::{Dep, slash};

/// What permits one component to depend on another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    SameComponent,
    /// The `tests` component of `tests: auto`, which no rule restricts.
    Tests,
    MayDependOn,
    Common,
    /// A lower layer, with no closed layer between.
    Layers,
}

impl Grant {
    /// How the policy names the grant.
    pub fn as_str(self) -> &'static str {
        match self {
            Grant::SameComponent => "same component",
            Grant::Tests => "tests: auto",
            Grant::MayDependOn => "may_depend_on",
            Grant::Common => "common",
            Grant::Layers => "layers",
        }
    }
}

/// Why no grant lets one component depend on another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A prohibition: `must_not_reach` when `reach`, else `must_not_depend_on`.
    Prohibited { reach: bool },
    /// Nothing grants it; `layers` says why they do not, when both are layers.
    NotGranted { layers: Option<String> },
}

/// The clause of the policy that rejects a dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clause<'a> {
    Refused(Refusal),
    /// `allow_importers` of the target does not list the source.
    ImporterNotListed(&'a [String]),
    /// The target path is outside the public surface of its component.
    OutsideSurface,
    /// Both ends fill the placeholder `name` of their `in` patterns differently.
    SliceMismatch {
        name: String,
        here: String,
        there: String,
    },
    /// Two instances of an isolated component.
    OtherInstance {
        here: String,
        there: String,
    },
    /// `exclude` hides the target, so no rule could judge it.
    ExcludedTarget,
    /// An external module the rules of the source do not let it use: what is wrong with it.
    External(&'static str),
}

impl Clause<'_> {
    /// The rule a dependency rejected by this clause violates.
    pub fn rule(&self) -> RuleId {
        match self {
            Clause::Refused(_) | Clause::ImporterNotListed(_) => RuleId::Dependency,
            Clause::OutsideSurface => RuleId::Privacy,
            Clause::SliceMismatch { .. } | Clause::OtherInstance { .. } => RuleId::Isolation,
            Clause::ExcludedTarget => RuleId::Excluded,
            Clause::External(_) => RuleId::External,
        }
    }
}

/// The verdict on one dependency.
#[derive(Debug)]
pub struct Decision<'a> {
    /// The component of the importing code.
    pub from: &'a str,
    /// The component of the target: none for an external module, a target
    /// `exclude` hides, or one that belongs to no component.
    pub to: Option<&'a str>,
    /// The target path, or the external module.
    pub target: String,
    /// The grant that allows it, or the clause that rejects it. Allowed with no
    /// grant when nothing in the policy restricts it: an allowed external
    /// module, a target in no component.
    pub grant: Result<Option<Grant>, Clause<'a>>,
    /// The reason the policy gives for the clause that rejected it.
    pub because: Option<&'a str>,
}

/// What the policy says of every dependency of one component on another,
/// before the paths of a dependency are known.
#[derive(Debug)]
pub struct Verdict<'a> {
    pub grant: Result<Grant, Refusal>,
    /// The reason given with the grant, or the one behind the refusal.
    pub because: Option<&'a str>,
    /// `allow_importers` of the target, when it does not list the source.
    pub importers: Option<&'a [String]>,
    /// The public surface of the target, when the target is another component that declares one.
    pub surface: Option<Vec<&'a str>>,
    /// Placeholders both components' `in` patterns name: a dependency must fill them alike.
    pub slices: Vec<&'a str>,
    /// Whether the pair is one isolated component, whose instances must not import each other.
    pub isolated: bool,
}

impl Verdict<'_> {
    /// Whether the pair is rejected whatever paths the dependency has.
    pub fn rejected(&self) -> bool {
        self.grant.is_err() || self.importers.is_some()
    }

    /// What the verdict leaves to the paths of each dependency on `to`, as a reader says it.
    pub fn conditions(&self, to: &str) -> Vec<String> {
        let mut found = Vec::new();
        if let Some(surface) = &self.surface {
            found.push(format!(
                "only through the public surface of {to}: {}",
                surface.join(", ")
            ));
        }
        for name in &self.slices {
            found.push(format!("only where both fill <{name}> alike"));
        }
        if self.isolated {
            found.push(format!("instances of {to} must not import each other"));
        }
        found
    }
}

/// The verdict on `from` depending on `to`.
pub fn pair<'a>(config: &'a Config, map: &'a ComponentMap, from: &str, to: &str) -> Verdict<'a> {
    let grant = match config.grant(from, to) {
        Some(grant) => Ok(grant),
        None if config.forbids(from, to) => Err(Refusal::Prohibited {
            reach: config.forbids_reach(from, to),
        }),
        None => Err(Refusal::NotGranted {
            layers: config.layer_block(from, to),
        }),
    };
    let because = match grant {
        Ok(_) => config.allow_reason(from, to),
        Err(_) => config
            .forbid_reason(from, to)
            .or_else(|| config.reason(from)),
    };
    let target = &config.components[to];
    let other = from != to;
    let ours = map.placeholders(from);
    Verdict {
        grant,
        because,
        importers: target
            .allow_importers
            .as_deref()
            .filter(|list| other && !list.iter().any(|c| c == from)),
        surface: target.public.as_ref().filter(|_| other).map(|p| p.as_vec()),
        slices: map
            .placeholders(to)
            .into_iter()
            .filter(|name| ours.contains(name))
            .collect(),
        isolated: !other && target.isolated,
    }
}

/// The verdict on one dependency; none when its source belongs to no component.
pub fn decide<'a>(config: &'a Config, map: &'a ComponentMap, dep: &Dep) -> Option<Decision<'a>> {
    let from = map.component_of_unit(&dep.from)?;
    let file = slash(&dep.file);
    let decision = |to, target, grant, because| Decision {
        from,
        to,
        target,
        grant,
        because,
    };
    let unit = match &dep.target {
        Target::External(module) => {
            return Some(match map.external_problem(from, &file, module) {
                Some(problem) => decision(
                    None,
                    module.clone(),
                    Err(Clause::External(problem.what)),
                    problem.because.or_else(|| config.reason(from)),
                ),
                None => decision(None, module.clone(), Ok(None), None),
            });
        }
        Target::Internal(unit) => unit,
    };
    let target = unit.key();
    if map.is_excluded(&target) {
        return Some(decision(None, target, Err(Clause::ExcludedTarget), None));
    }
    let Some(to) = map.component_of_unit(unit) else {
        return Some(decision(None, target, Ok(None), None));
    };
    let component = |name: &str| config.components[name].because.as_deref();
    let sliced = |clause| (Err(clause), component(to).or_else(|| config.reason(from)));
    let (grant, because) = if from == to {
        match (map.instance_of(&file), map.instance_of(&target)) {
            (Some(here), Some(there)) if here != there => (
                Err(Clause::OtherInstance { here, there }),
                component(from).or_else(|| config.reason(from)),
            ),
            _ => slice_mismatch(map, &file, &target)
                .map_or((Ok(Some(Grant::SameComponent)), None), sliced),
        }
    } else {
        let verdict = pair(config, map, from, to);
        if let Err(refusal) = &verdict.grant {
            (Err(Clause::Refused(refusal.clone())), verdict.because)
        } else if let Some(list) = verdict.importers {
            (Err(Clause::ImporterNotListed(list)), component(to))
        } else if !map.is_public(to, &target) {
            (Err(Clause::OutsideSurface), component(to))
        } else if let Some(clause) = slice_mismatch(map, &file, &target) {
            sliced(clause)
        } else {
            (Ok(verdict.grant.ok()), verdict.because)
        }
    };
    Some(decision(Some(to), target, grant, because))
}

/// The placeholder both ends name in their `in` patterns
/// (`src/features/<feature>/ui`) and fill differently, if there is one.
fn slice_mismatch<'a>(map: &ComponentMap, file: &str, target: &str) -> Option<Clause<'a>> {
    let theirs = map.captures_of(target);
    map.captures_of(file).into_iter().find_map(|(name, here)| {
        let (_, there) = theirs.iter().find(|(n, _)| *n == name)?;
        (here != *there).then(|| Clause::SliceMismatch {
            name: name.to_string(),
            here,
            there: there.clone(),
        })
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use std::path::Path;

    use crate::check;
    use crate::components::ComponentMap;
    use crate::config::Config;
    use crate::indexer::{CacheUse, Jobs};
    use crate::scan::{self, Cache};
    use crate::scope::Scope;

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            // A Cargo workspace keeps its manifests as `Cargo.toml.fixture`.
            let dest = to.join(if name == "Cargo.toml.fixture" {
                "Cargo.toml".into()
            } else {
                name
            });
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &dest);
            } else {
                fs::copy(entry.path(), dest).unwrap();
            }
        }
    }

    /// The packages a fixture's `modules.txt` lists, stubbed as installed for
    /// the TypeScript indexer, as `tests/cli.rs` does.
    fn stub_packages(root: &Path) {
        let Ok(list) = fs::read_to_string(root.join("modules.txt")) else {
            return;
        };
        for specifier in list.lines() {
            let mut parts = specifier.split('/');
            let first = parts.next().unwrap();
            let (name, rest): (String, Vec<&str>) = if first.starts_with('@') {
                (
                    format!("{first}/{}", parts.next().unwrap()),
                    parts.collect(),
                )
            } else {
                (first.to_string(), parts.collect())
            };
            let dir = root.join("node_modules").join(&name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("package.json"),
                format!(r#"{{"name":"{name}","version":"1.0.0","types":"index.d.ts"}}"#),
            )
            .unwrap();
            let stub = "declare const stub: any;\nexport default stub;\n";
            fs::write(dir.join("index.d.ts"), stub).unwrap();
            if !rest.is_empty() {
                let file = dir.join(format!("{}.d.ts", rest.join("/")));
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(file, stub).unwrap();
            }
        }
        fs::remove_file(root.join("modules.txt")).unwrap();
    }

    /// Runs `each` on the config, map and dependencies of every fixture project.
    fn each_fixture(each: impl Fn(&str, &Config, &ComponentMap, &[crate::scan::Dep]) + Sync) {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut names: Vec<String> = fs::read_dir(&fixtures)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // As in `tests/cli.rs`: no scip-ruby is built for Linux on Arm.
        let no_ruby = cfg!(all(target_os = "linux", target_arch = "aarch64"))
            && which::which("scip-ruby").is_err();
        names.retain(|name| !(no_ruby && name.starts_with("ruby")));
        std::thread::scope(|s| {
            for name in &names {
                let (fixtures, each) = (&fixtures, &each);
                s.spawn(move || {
                    let tmp = tempfile::tempdir().unwrap();
                    copy_dir(&fixtures.join(name), tmp.path());
                    stub_packages(tmp.path());
                    if tmp.path().join("composer.json").exists() {
                        // scip-php reads the autoloader.
                        let composer = which::which("composer").expect("composer is needed");
                        let status = std::process::Command::new(composer)
                            .args(["install", "--no-interaction", "--quiet"])
                            .current_dir(tmp.path())
                            .status()
                            .expect("composer runs");
                        assert!(status.success(), "composer install in {name}");
                    }
                    let config = Config::load(&tmp.path().join("easement.yml")).unwrap();
                    let mut cache = Cache::new(CacheUse::Off, Jobs::default());
                    let scan = scan::scan(tmp.path(), &config, &mut cache).unwrap();
                    let map = ComponentMap::new(&config).unwrap();
                    each(name, &config, &map, &scan.deps);
                });
            }
        });
    }

    /// Each rejected decision is a dependency violation of `check`, and each of
    /// those a rejected decision.
    #[test]
    fn rejected_decisions_are_the_violations_check_reports_of_dependencies() {
        each_fixture(|name, config, map, deps| {
            let mut rejected: Vec<String> = deps
                .iter()
                .filter_map(|dep| Some((dep, decide(config, map, dep)?)))
                .filter_map(|(dep, d)| match &d.grant {
                    Err(clause) => Some(format!(
                        "{} {}:{} {} -> {} {}",
                        clause.rule().as_str(),
                        slash(&dep.file),
                        dep.line,
                        d.from,
                        d.to.unwrap_or_default(),
                        d.target
                    )),
                    Ok(_) => None,
                })
                .collect();
            let (found, _, _) = check::evaluate(config, map, &Scope::default(), deps);
            let mut violations: Vec<String> = found
                .iter()
                .map(|v| {
                    format!(
                        "{} {}:{} {} -> {} {}",
                        v.rule.as_str(),
                        v.file,
                        v.line,
                        v.from,
                        v.to,
                        v.target
                    )
                })
                .collect();
            rejected.sort();
            violations.sort();
            assert_eq!(rejected, violations, "{name}");
        });
    }
}
