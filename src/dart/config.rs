//! Opt-in Dart policy. Component ownership remains in the shared configuration.
use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Entity,
    Dto,
    RepositoryContract,
    RepositoryImplementation,
    UseCase,
    ViewModel,
    View,
    Screen,
    Bootstrap,
}

/// A declaration identified by its resolved library URI and exact name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub library: String,
    pub name: String,
}

/// An instantiated contract. Arguments match resolved identities recursively.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub library: String,
    pub name: String,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default)]
    pub arguments: Vec<Contract>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Role assignments; exact types override component and path defaults.
    #[serde(default)]
    pub roles: Vec<Assignment>,
    /// Explicit implementation-to-instantiated-contract associations.
    #[serde(default)]
    pub implementations: Vec<Implementation>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub role: Role,
    #[serde(default)]
    pub components: Vec<String>,
    /// Project-relative file globs. A directory needs a trailing /**.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub types: Vec<Selector>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Implementation {
    pub implementation: Selector,
    pub contract: Contract,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    /// At most one resolved Flutter widget class per physical source file.
    #[serde(default)]
    pub one_widget_per_file: bool,
    /// Extract named widget-returning helpers into dedicated widgets.
    #[serde(default)]
    pub no_widget_helpers: bool,
    /// Required class modifiers for repository-contract declarations.
    pub repository_declaration: Option<DeclarationPolicy>,
    /// Forbidden types in public repository-contract API signatures.
    pub contract_api_types: Option<TypePolicy>,
    /// Verify explicit implementation-to-contract associations.
    #[serde(default)]
    pub repository_implementation: bool,
    /// Forbidden implementation types in use-case signatures and bodies.
    pub use_case_dependencies: Option<TypePolicy>,
    /// Forbidden UI/infrastructure identities in configured domain roles.
    pub domain_type_boundaries: Option<TypePolicy>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclarationPolicy {
    pub require: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypePolicy {
    /// Subject roles; empty uses the rule's documented default roles.
    #[serde(default)]
    pub roles: Vec<Role>,
    #[serde(default)]
    pub forbid_roles: Vec<Role>,
    /// Library URI and declaration-name globs, matched against resolved identity.
    #[serde(default)]
    pub forbid_types: Vec<Selector>,
}

impl Rules {
    pub fn enabled(&self) -> bool {
        self.one_widget_per_file
            || self.no_widget_helpers
            || self.repository_declaration.is_some()
            || self.contract_api_types.is_some()
            || self.repository_implementation
            || self.use_case_dependencies.is_some()
            || self.domain_type_boundaries.is_some()
    }
}

pub fn validate(config: &crate::config::Config) -> Result<()> {
    for assignment in &config.dart.roles {
        if assignment.components.is_empty()
            && assignment.paths.is_empty()
            && assignment.types.is_empty()
        {
            bail!("dart.roles: assignment needs components, paths or types");
        }
        for component in &assignment.components {
            config.require_component(component)?;
        }
        for pattern in &assignment.paths {
            globset::Glob::new(pattern).context("invalid dart.roles path glob")?;
        }
        for selector in &assignment.types {
            exact(selector)?;
        }
    }
    for association in &config.dart.implementations {
        exact(&association.implementation)?;
        contract(&association.contract)?;
    }
    for (component, rule) in &config.rules {
        if let Some(policy) = &rule.dart.repository_declaration {
            if policy.require.is_empty() {
                bail!("rules.{component}.dart.repository_declaration.require cannot be empty");
            }
            for modifier in &policy.require {
                if !["abstract", "interface", "base", "final", "sealed"]
                    .contains(&modifier.as_str())
                {
                    bail!("rules.{component}.dart: unknown declaration modifier {modifier}");
                }
            }
        }
        for policy in [
            &rule.dart.contract_api_types,
            &rule.dart.use_case_dependencies,
            &rule.dart.domain_type_boundaries,
        ]
        .into_iter()
        .flatten()
        {
            if policy.forbid_roles.is_empty() && policy.forbid_types.is_empty() {
                bail!(
                    "rules.{component}.dart: a type constraint needs forbid_roles or forbid_types"
                );
            }
            for selector in &policy.forbid_types {
                globset::Glob::new(&selector.library).context("invalid Dart library glob")?;
                globset::Glob::new(&selector.name).context("invalid Dart declaration glob")?;
            }
        }
    }
    Ok(())
}

fn exact(selector: &Selector) -> Result<()> {
    if selector.library.is_empty()
        || selector.name.is_empty()
        || selector.library.contains(['*', '?', '['])
        || selector.name.contains(['*', '?', '['])
    {
        bail!(
            "Dart explicit selector needs an exact library URI and declaration name: {selector:?}"
        );
    }
    Ok(())
}
fn contract(value: &Contract) -> Result<()> {
    exact(&Selector {
        library: value.library.clone(),
        name: value.name.clone(),
    })?;
    for argument in &value.arguments {
        contract(argument)?;
    }
    Ok(())
}
