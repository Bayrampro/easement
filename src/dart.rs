//! Dart semantic evaluation. Disabled unless a component enables a semantic rule.
pub(crate) mod config;
pub(crate) mod facts;

use crate::check::{RuleId, SemanticDetail, SemanticGap, Violation};
use crate::components::ComponentMap;
use crate::config::Config;
use anyhow::{Result, bail};
use config::{Contract, Role, Selector, TypePolicy};
use facts::{Declaration, Location, Type, Use};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct Evaluation {
    pub violations: Vec<Violation>,
    pub gaps: Vec<SemanticGap>,
    pub ran: usize,
    pub cached: usize,
}

pub fn evaluate(
    root: &Path,
    config: &Config,
    map: &ComponentMap,
    files: &[String],
    mode: crate::indexer::CacheUse,
) -> Result<Evaluation> {
    let enabled: Vec<_> = files
        .iter()
        .filter(|f| {
            crate::relpath::extension(f) == "dart"
                && map
                    .component_of(f)
                    .and_then(|name| config.rules.get(name))
                    .is_some_and(|r| r.dart.enabled())
        })
        .cloned()
        .collect();
    let mut result = Evaluation {
        violations: Vec::new(),
        gaps: Vec::new(),
        ran: 0,
        cached: 0,
    };
    if enabled.is_empty() {
        return Ok(result);
    }
    let extracted = crate::indexer::dart::semantics::run(root, files, &enabled, mode)?;
    result.ran = extracted.ran;
    result.cached = extracted.cached;
    let mut known = BTreeMap::new();
    for facts in &extracted.facts {
        for declaration in &facts.declarations {
            known.insert(declaration.identity.clone(), declaration.location.clone());
            for used in declaration.api.iter().chain(&declaration.uses) {
                inventory(&used.ty, &mut known);
            }
            for ty in &declaration.supertypes {
                inventory(ty, &mut known);
            }
        }
    }
    let incomplete = extracted.facts.iter().any(|facts| !facts.gaps.is_empty());
    for assignment in &config.dart.roles {
        for selector in &assignment.types {
            if !known.contains_key(selector) && incomplete {
                for file in &enabled {
                    if let Some(rule) = map.component_of(file).and_then(|c| config.rules.get(c)) {
                        for id in active(&rule.dart) {
                            result.gap(id, &Location { file: file.clone(), line: 1, column: 1 }, &format!("{}#{}", selector.library, selector.name), "explicit selector cannot be resolved because required semantic facts are missing");
                        }
                    }
                }
            } else if !known.contains_key(selector) {
                bail!(
                    "unknown explicit Dart selector {}#{}; use the declaration's defining library URI and exact name; ensure its package is covered by an enabled Dart rule",
                    selector.library,
                    selector.name
                );
            }
        }
    }
    let mut roles = BTreeMap::new();
    for (selector, location) in &known {
        if let Some(role) = role(config, map, selector, &location.file)? {
            roles.insert(selector.clone(), role);
        }
    }
    for association in &config.dart.implementations {
        if incomplete
            && (!known.contains_key(&association.implementation)
                || !known.contains_key(&Selector {
                    library: association.contract.library.clone(),
                    name: association.contract.name.clone(),
                }))
        {
            continue;
        }
        if !known.contains_key(&association.implementation) {
            bail!(
                "unknown Dart implementation selector {:?}",
                association.implementation
            );
        }
        if roles.get(&association.implementation) != Some(&Role::RepositoryImplementation) {
            bail!(
                "Dart implementation association requires repository-implementation role: {:?}",
                association.implementation
            );
        }
        let contract = Selector {
            library: association.contract.library.clone(),
            name: association.contract.name.clone(),
        };
        if !known.contains_key(&contract) {
            bail!("unknown Dart contract selector {contract:?}");
        }
        if roles.get(&contract) != Some(&Role::RepositoryContract) {
            bail!("Dart association target needs repository-contract role: {contract:?}");
        }
    }
    let checked: BTreeSet<_> = enabled.iter().map(String::as_str).collect();
    let mut emitted = BTreeSet::new();
    for facts in &extracted.facts {
        for gap in &facts.gaps {
            if !checked.contains(gap.file.as_str()) {
                // Errors in a dependency can invalidate facts at an enabled use site.
                for file in &enabled {
                    if let Some(rule) = map.component_of(file).and_then(|c| config.rules.get(c)) {
                        for id in active(&rule.dart) {
                            result.gap(
                                id,
                                &Location {
                                    file: file.clone(),
                                    line: 1,
                                    column: 1,
                                },
                                "<dependency>",
                                &format!("{}:{}: {}", gap.file, gap.line, gap.reason),
                            );
                        }
                    }
                }
            } else if let Some(rule) = map
                .component_of(&gap.file)
                .and_then(|c| config.rules.get(c))
            {
                for id in active(&rule.dart) {
                    result.gap(
                        id,
                        &Location {
                            file: gap.file.clone(),
                            line: gap.line,
                            column: 1,
                        },
                        "<unresolved>",
                        &gap.reason,
                    );
                }
            }
        }
        for declaration in &facts.declarations {
            if !checked.contains(declaration.location.file.as_str()) {
                continue;
            }
            let Some(component) = map.component_of(&declaration.location.file) else {
                continue;
            };
            let rule = &config.rules[component];
            let source_role = roles.get(&declaration.identity).copied();
            if source_role == Some(Role::RepositoryContract)
                && let Some(policy) = &rule.dart.repository_declaration
                && (policy
                    .require
                    .iter()
                    .any(|m| !declaration.modifiers.contains(m))
                    || declaration.kind != "CLASS")
            {
                result.violations.push(violation(
                    RuleId::DartRepositoryDeclaration,
                    component,
                    declaration,
                    &Use {
                        member: declaration.identity.name.clone(),
                        operation: "declaration".into(),
                        location: declaration.location.clone(),
                        origin: declaration.location.clone(),
                        ty: Type {
                            display: declaration.identity.name.clone(),
                            nullable: false,
                            library: Some(declaration.identity.library.clone()),
                            name: Some(declaration.identity.name.clone()),
                            alias: false,
                            origin: None,
                            unresolved: false,
                            children: Vec::new(),
                        },
                    },
                    &declaration.identity,
                    vec![declaration.identity.name.clone()],
                    (
                        &format!("requires {} class", policy.require.join(" ")),
                        Some(rule),
                    ),
                ));
            }
            if source_role == Some(Role::RepositoryImplementation)
                && rule.dart.repository_implementation
            {
                let associations: Vec<_> = config
                    .dart
                    .implementations
                    .iter()
                    .filter(|a| a.implementation == declaration.identity)
                    .collect();
                if associations.is_empty() {
                    result.gap(RuleId::DartRepositoryImplementation, &declaration.location, &declaration.identity.name, "implementation has no explicit contract association; add dart.implementations");
                }
                for association in associations {
                    if !declaration
                        .supertypes
                        .iter()
                        .any(|t| contract_matches(&association.contract, t))
                    {
                        if incomplete {
                            result.gap(RuleId::DartRepositoryImplementation, &declaration.location, &declaration.identity.name, "interface relationship cannot be proved while required semantic facts are incomplete");
                            continue;
                        }
                        let target = Selector {
                            library: association.contract.library.clone(),
                            name: association.contract.name.clone(),
                        };
                        result.violations.push(violation(
                            RuleId::DartRepositoryImplementation,
                            component,
                            declaration,
                            &Use {
                                member: declaration.identity.name.clone(),
                                operation: "supertype".into(),
                                location: declaration.location.clone(),
                                origin: declaration.location.clone(),
                                ty: Type {
                                    display: target.name.clone(),
                                    nullable: false,
                                    library: Some(target.library.clone()),
                                    name: Some(target.name.clone()),
                                    alias: false,
                                    origin: None,
                                    unresolved: false,
                                    children: Vec::new(),
                                },
                            },
                            &target,
                            vec![target.name.clone()],
                            (
                                &format!(
                                    "must implement {}#{} with configured type arguments",
                                    target.library, target.name
                                ),
                                Some(rule),
                            ),
                        ));
                    }
                }
            }
            for (id, policy, defaults, signatures_only) in [
                (
                    RuleId::DartContractApiTypes,
                    &rule.dart.contract_api_types,
                    &[Role::RepositoryContract][..],
                    true,
                ),
                (
                    RuleId::DartUseCaseDependencies,
                    &rule.dart.use_case_dependencies,
                    &[Role::UseCase][..],
                    false,
                ),
                (
                    RuleId::DartDomainTypeBoundaries,
                    &rule.dart.domain_type_boundaries,
                    &[Role::Entity, Role::RepositoryContract, Role::UseCase][..],
                    false,
                ),
            ] {
                let Some(policy) = policy else {
                    continue;
                };
                let subjects = if policy.roles.is_empty() {
                    defaults
                } else {
                    &policy.roles
                };
                if !source_role.is_some_and(|role| subjects.contains(&role)) {
                    continue;
                }
                let uses: Vec<_> = declaration
                    .api
                    .iter()
                    .chain(declaration.uses.iter().filter(|_| !signatures_only))
                    .collect();
                for used in uses {
                    let mut matches = Vec::new();
                    let mut unresolved = false;
                    find(
                        &used.ty,
                        policy,
                        &roles,
                        &mut Vec::new(),
                        &mut Vec::new(),
                        &mut matches,
                        &mut unresolved,
                    );
                    if unresolved {
                        result.gap(
                            id,
                            &used.location,
                            &declaration.identity.name,
                            "resolved type graph contains an invalid or unsupported type",
                        );
                    }
                    for (target, path, origins) in matches {
                        let key = (
                            id,
                            declaration.identity.clone(),
                            if signatures_only {
                                used.member.clone()
                            } else {
                                declaration.identity.name.clone()
                            },
                            used.location.file.clone(),
                            used.location.line,
                            signatures_only.then(|| used.operation.clone()),
                            target.clone(),
                        );
                        if !emitted.insert(key) {
                            continue;
                        }
                        let constraint = format!(
                            "rules.{component}.dart.{}",
                            match id {
                                RuleId::DartContractApiTypes => "contract_api_types",
                                RuleId::DartUseCaseDependencies => "use_case_dependencies",
                                _ => "domain_type_boundaries",
                            }
                        );
                        let mut found = violation(
                            id,
                            component,
                            declaration,
                            used,
                            &target,
                            path,
                            (&constraint, Some(rule)),
                        );
                        found.evidence.extend(
                            origins
                                .into_iter()
                                .filter(|at| !at.file.is_empty())
                                .map(|at| (at.file, at.line)),
                        );
                        result.violations.push(found);
                    }
                }
            }
        }
    }
    Ok(result)
}

impl Evaluation {
    fn gap(&mut self, rule: RuleId, at: &Location, declaration: &str, reason: &str) {
        if self.gaps.iter().any(|g| {
            g.rule == rule
                && g.file == at.file
                && g.line == at.line
                && g.declaration == declaration
                && g.reason == reason
        }) {
            return;
        }
        self.gaps.push(SemanticGap { rule, file: at.file.clone(), line: at.line, declaration: declaration.into(), reason: reason.into(), recheck: "repair source, role assignments or dependency preparation; run easement check --no-cache without --diff".into() });
    }
}

fn active(rules: &config::Rules) -> Vec<RuleId> {
    let mut ids = Vec::new();
    if rules.repository_declaration.is_some() {
        ids.push(RuleId::DartRepositoryDeclaration);
    }
    if rules.contract_api_types.is_some() {
        ids.push(RuleId::DartContractApiTypes);
    }
    if rules.repository_implementation {
        ids.push(RuleId::DartRepositoryImplementation);
    }
    if rules.use_case_dependencies.is_some() {
        ids.push(RuleId::DartUseCaseDependencies);
    }
    if rules.domain_type_boundaries.is_some() {
        ids.push(RuleId::DartDomainTypeBoundaries);
    }
    ids
}

fn inventory(ty: &Type, known: &mut BTreeMap<Selector, Location>) {
    if let (Some(library), Some(name), Some(origin)) = (&ty.library, &ty.name, &ty.origin) {
        known
            .entry(Selector {
                library: library.clone(),
                name: name.clone(),
            })
            .or_insert_with(|| origin.clone());
    }
    for child in &ty.children {
        inventory(child, known);
    }
}

fn role(
    config: &Config,
    map: &ComponentMap,
    selector: &Selector,
    file: &str,
) -> Result<Option<Role>> {
    let mut explicit = BTreeSet::new();
    let mut defaults = BTreeSet::new();
    for assignment in &config.dart.roles {
        if assignment.types.contains(selector) {
            explicit.insert(assignment.role);
        }
        if (!file.is_empty()
            && assignment
                .paths
                .iter()
                .any(|p| globset::Glob::new(p).is_ok_and(|g| g.compile_matcher().is_match(file))))
            || map
                .component_of(file)
                .is_some_and(|c| assignment.components.iter().any(|name| name == c))
        {
            defaults.insert(assignment.role);
        }
    }
    let roles = if explicit.is_empty() {
        defaults
    } else {
        explicit
    };
    if roles.len() > 1 {
        bail!(
            "conflicting Dart roles at the same precedence for {}#{}: {roles:?}; use one explicit assignment to override path/component defaults",
            selector.library,
            selector.name
        );
    }
    Ok(roles.first().copied())
}

fn contract_matches(contract: &Contract, ty: &Type) -> bool {
    if ty.alias {
        return ty.children.iter().any(|t| contract_matches(contract, t));
    }
    ty.nullable == contract.nullable
        && ty.library.as_deref() == Some(&contract.library)
        && ty.name.as_deref() == Some(&contract.name)
        && contract.arguments.len() == ty.children.len()
        && contract
            .arguments
            .iter()
            .zip(&ty.children)
            .all(|(a, t)| contract_matches(a, t))
}

type Match = (Selector, Vec<String>, Vec<Location>);
fn find(
    ty: &Type,
    policy: &TypePolicy,
    roles: &BTreeMap<Selector, Role>,
    path: &mut Vec<String>,
    origins: &mut Vec<Location>,
    matches: &mut Vec<Match>,
    unresolved: &mut bool,
) {
    path.push(ty.display.clone());
    if let Some(origin) = &ty.origin {
        origins.push(origin.clone());
    }
    *unresolved |= ty.unresolved;
    if let (Some(library), Some(name)) = (&ty.library, &ty.name) {
        let selector = Selector {
            library: library.clone(),
            name: name.clone(),
        };
        let forbidden = roles
            .get(&selector)
            .is_some_and(|r| policy.forbid_roles.contains(r))
            || policy.forbid_types.iter().any(|p| {
                globset::Glob::new(&p.library).is_ok_and(|g| g.compile_matcher().is_match(library))
                    && globset::Glob::new(&p.name).is_ok_and(|g| g.compile_matcher().is_match(name))
            });
        if forbidden {
            matches.push((selector, path.clone(), origins.clone()));
        }
    }
    for child in &ty.children {
        find(child, policy, roles, path, origins, matches, unresolved);
    }
    if ty.origin.is_some() {
        origins.pop();
    }
    path.pop();
}

fn violation(
    rule: RuleId,
    component: &str,
    declaration: &Declaration,
    used: &Use,
    target: &Selector,
    path: Vec<String>,
    constraint: (&str, Option<&crate::config::Rule>),
) -> Violation {
    let (constraint, policy) = constraint;
    let declaration_name = format!(
        "{}#{}::{}",
        declaration.identity.library, declaration.identity.name, used.member
    );
    let offending = format!("{}#{}", target.library, target.name);
    let mut evidence = vec![
        (used.location.file.clone(), used.location.line),
        (declaration.location.file.clone(), declaration.location.line),
    ];
    evidence.extend(
        declaration
            .uses
            .iter()
            .filter(|used| used.operation == "inheritance")
            .map(|used| (used.location.file.clone(), used.location.line)),
    );
    if !used.origin.file.is_empty() {
        evidence.push((used.origin.file.clone(), used.origin.line));
    }
    let signature_slot = if rule == RuleId::DartContractApiTypes {
        format!(" [{}]", used.operation)
    } else {
        String::new()
    };
    Violation {
        semantic: Some(SemanticDetail {
            declaration: declaration_name.clone(),
            offending_type: offending.clone(),
            type_path: path.clone(),
            constraint: constraint.into(),
            column: used.location.column,
            operation: used.operation.clone(),
            origin: (!used.origin.file.is_empty())
                .then(|| (used.origin.file.clone(), used.origin.line)),
        }),
        rule,
        file: used.location.file.clone(),
        line: used.location.line,
        message: format!(
            "{declaration_name}{signature_slot}: {constraint}; forbidden or missing {offending} through {}",
            path.join(" -> ")
        ),
        because: policy.and_then(|r| r.because.clone()),
        from: component.into(),
        to: target.library.clone(),
        target: format!("{declaration_name} ({}) -> {offending}", used.operation),
        specifier: target.library.clone(),
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_use_component_and_path_defaults_with_explicit_precedence() {
        let config: Config = serde_norway::from_str(
            r#"version: 1
components:
  domain: {in: lib/domain}
  data: {in: lib/data}
dart:
  roles:
    - {role: entity, components: [domain]}
    - {role: dto, paths: ['lib/data/**']}
    - role: repository-contract
      types: [{library: 'package:example/mixed.dart', name: Mixed}]
"#,
        )
        .unwrap();
        let map = ComponentMap::new(&config).unwrap();
        let plain = Selector {
            library: "package:example/plain.dart".into(),
            name: "Plain".into(),
        };
        assert_eq!(
            role(&config, &map, &plain, "lib/domain/plain.dart").unwrap(),
            Some(Role::Entity)
        );
        assert_eq!(
            role(&config, &map, &plain, "lib/data/plain.dart").unwrap(),
            Some(Role::Dto)
        );
        let mixed = Selector {
            library: "package:example/mixed.dart".into(),
            name: "Mixed".into(),
        };
        assert_eq!(
            role(&config, &map, &mixed, "lib/data/mixed.dart").unwrap(),
            Some(Role::RepositoryContract)
        );
    }
}
