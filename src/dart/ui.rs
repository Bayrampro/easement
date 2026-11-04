//! Opt-in Flutter composition policy over analyzer-classified physical declarations.
use super::facts::{Facts, Location};
use super::{Evaluation, SemanticDetail, Violation};
use crate::check::RuleId;
use crate::components::ComponentMap;
use crate::config::Config;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn evaluate(
    facts: &[Facts],
    config: &Config,
    map: &ComponentMap,
    checked: &BTreeSet<&str>,
    result: &mut Evaluation,
) {
    let mut widgets = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for class in facts.iter().flat_map(|f| &f.ui_classes) {
        let at = &class.location;
        if !checked.contains(at.file.as_str()) || !seen.insert((class.identity.clone(), at.clone()))
        {
            continue;
        }
        let Some(component) = map.component_of(&at.file) else {
            continue;
        };
        let rules = &config.rules[component].dart;
        if class.widget == Some(true) {
            widgets
                .entry(at.file.clone())
                .or_insert_with(Vec::new)
                .push(class);
        } else if class.widget.is_none() {
            for (enabled, id) in [
                (rules.one_widget_per_file, RuleId::DartOneWidgetPerFile),
                (rules.no_widget_helpers, RuleId::DartNoWidgetHelpers),
            ] {
                if enabled {
                    result.gap(
                        id,
                        at,
                        &class.identity.name,
                        "Flutter widget ancestry could not be resolved",
                    );
                }
            }
        }
    }
    for (file, classes) in &mut widgets {
        classes.sort_by_key(|c| (c.location.line, c.location.column));
        let component = map.component_of(file).expect("checked component");
        let policy = &config.rules[component];
        if !policy.dart.one_widget_per_file {
            continue;
        }
        let names = classes
            .iter()
            .map(|c| c.identity.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        for class in classes.iter().skip(1) {
            let id = format!("{}#{}", class.identity.library, class.identity.name);
            let mut found = finding(
                RuleId::DartOneWidgetPerFile,
                component,
                &class.location,
                &id,
                (
                    &format!(
                        "{} declares multiple Flutter widgets: {names}; move {} into its own file",
                        file, class.identity.name
                    ),
                    "one Flutter widget class per physical source file",
                    "Widget",
                    "declaration",
                ),
            );
            found.because = policy.because.clone();
            found.evidence = classes
                .iter()
                .map(|c| (file.clone(), c.location.line))
                .collect();
            result.violations.push(found);
        }
    }
    let mut seen = BTreeSet::new();
    for callable in facts.iter().flat_map(|f| &f.callables) {
        let at = &callable.location;
        if !widgets.contains_key(&at.file) || !seen.insert((callable.id.clone(), at.clone())) {
            continue;
        }
        let component = map.component_of(&at.file).expect("checked component");
        let policy = &config.rules[component];
        if !policy.dart.no_widget_helpers || callable.framework_build {
            continue;
        }
        let rule = RuleId::DartNoWidgetHelpers;
        if callable.widget_return == Some(true) {
            let return_type = &callable.return_type.display;
            let evidence = callable
                .returns
                .iter()
                .map(|r| r.ty.display.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let message = format!(
                "{} returns {return_type} (resolved widget evidence: {evidence}); extract a dedicated widget class into its own file",
                callable.id
            );
            let mut found = finding(
                rule,
                component,
                at,
                &callable.id,
                (
                    &message,
                    "named widget-returning helpers must be extracted",
                    return_type,
                    &callable.kind,
                ),
            );
            found.because = policy.because.clone();
            if let Some(detail) = &mut found.semantic {
                detail
                    .type_path
                    .extend(callable.returns.iter().map(|r| r.ty.display.clone()));
            }
            found.evidence.extend(
                callable
                    .returns
                    .iter()
                    .map(|r| (r.location.file.clone(), r.location.line)),
            );
            result.violations.push(found);
        } else if callable.widget_return.is_none() {
            result.gap(
                rule,
                at,
                &callable.id,
                "helper return paths could not be reliably classified as widget or non-widget",
            );
        }
    }
}

fn finding(
    rule: RuleId,
    component: &str,
    at: &Location,
    id: &str,
    detail: (&str, &str, &str, &str),
) -> Violation {
    let (message, constraint, ty, operation) = detail;
    Violation {
        rule,
        file: at.file.clone(),
        line: at.line,
        message: message.into(),
        because: None,
        from: component.into(),
        to: "package:flutter/src/widgets/framework.dart".into(),
        target: id.into(),
        specifier: "package:flutter/src/widgets/framework.dart".into(),
        evidence: vec![(at.file.clone(), at.line)],
        semantic: Some(SemanticDetail {
            declaration: id.into(),
            offending_type: ty.into(),
            type_path: vec![ty.into()],
            constraint: constraint.into(),
            column: at.column,
            operation: operation.into(),
            origin: None,
        }),
    }
}
