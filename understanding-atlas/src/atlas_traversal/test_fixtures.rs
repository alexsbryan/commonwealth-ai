// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test fixtures built from language types.
//!
//! The moved classifier/traversal tests used `corpus-engine`'s
//! `recipe_templates::numismatics_policies`, which parses the shipped
//! `numismatics` recipe template — a host dependency (recipe TOML parsing) the
//! pure tier may not carry, even as a dev edge (boundary-gate counts dev
//! edges). This fixture builds the same `OntologyV1` the template declares and
//! folds it through the language's own `into_policies()`, so the tests keep
//! exercising a declared ontology without linking `corpus-engine`.
//!
//! The declaration mirrors `sovereign-recipes/_templates/ontology-v1/
//! numismatics/recipe.toml`'s `[enrichment.ontology]` block. If that template
//! changes, this fixture is a snapshot that may need the same edit — the
//! template's own `recipe_templates` test pins the shipped copy.

use understanding_vocab::ontology::decl::{
    AttrDecl, AttrFamily, Force, OntologyTypeDecl, OntologyV1, TensionDecl, TypeKind,
};
use understanding_vocab::ontology::OntologyPolicies;

fn attr_text(name: &str, values: &[&str]) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family: AttrFamily::Text {
            values: values.iter().map(|s| (*s).to_string()).collect(),
        },
        description: String::new(),
    }
}

fn attr_quantity(name: &str, unit: Option<&str>) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family: AttrFamily::Quantity {
            unit: unit.map(str::to_string),
        },
        description: String::new(),
    }
}

fn attr_time(name: &str, range: bool) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family: AttrFamily::Time { range },
        description: String::new(),
    }
}

fn attr_ref(name: &str, of: &str) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family: AttrFamily::Ref { of: of.into() },
        description: String::new(),
    }
}

/// The shipped `numismatics` declaration, as a leaf-built `OntologyPolicies`.
pub(crate) fn numismatics_policies() -> OntologyPolicies {
    OntologyV1 {
        guidance: "This corpus is numismatic scholarship: coin catalogues, hoard \
reports and dating arguments. Each coin type is a thing in its own right — ruler, \
mint, denomination, metal, weight, and when it was struck. Attributions are claims \
about a coin made by a scholar, graded by the kind of evidence offered."
            .into(),
        types: vec![
            OntologyTypeDecl {
                name: "coin".into(),
                kind: TypeKind::Entity,
                description: "A coin type: ruler, mint, denomination, metal.".into(),
                attributes: vec![
                    attr_ref("ruler", "ruler"),
                    attr_ref("mint", "mint"),
                    attr_text("denomination", &[]),
                    attr_text("metal", &["gold", "silver", "billon", "copper"]),
                    attr_quantity("weight", Some("g")),
                    attr_time("struck", true),
                ],
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "sceatta".into(),
                kind: TypeKind::Entity,
                specializes: Some("coin".into()),
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "ruler".into(),
                kind: TypeKind::Entity,
                role_of: Some("person".into()),
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "mint".into(),
                kind: TypeKind::Entity,
                description: "A place where coins were struck.".into(),
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "attribution".into(),
                kind: TypeKind::Claim,
                force: Some(Force::Assertive),
                subject: Some("coin".into()),
                attributes: vec![attr_time("proposed_date", true)],
                grades: vec![
                    "die-link".into(),
                    "hoard-context".into(),
                    "stylistic".into(),
                    "metrological".into(),
                ],
                ..Default::default()
            },
        ],
        tension: TensionDecl {
            between: vec!["attribution".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .into_policies()
}
