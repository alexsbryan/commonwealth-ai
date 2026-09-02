// SPDX-License-Identifier: AGPL-3.0-or-later
//! Concrete `Pipeline` implementations.
//!
//! Each submodule contributes one pipeline + its prompt assets.
//! Registration happens in `super::registry::PipelineRegistry::builtin`.

pub mod configurable_atlas;
pub mod conversation_atlas;
pub mod engineering_atlas;
pub mod genre;
pub mod literary;
pub mod literary_atlas;
// `obsidian_atlas` removed when vault corpora moved to the tiered
// RAPTOR + GLiNER surface (FolderTieredProvider). Operators wanting
// atoms.json output against a vault can pass `--pipeline literary_atlas`.
pub mod ontology_parse;
pub mod ontology_schema;
pub mod parse_policy;
pub mod philosophy_atlas;
pub mod referential_atlas;
pub mod sketch_parse;

/// Test-only fixture: the shipped `numismatics` ontology template, parsed into
/// policies. One definition for every ontology test in this module tree, so a
/// test can never pass against a declaration nobody ships.
#[cfg(test)]
pub(crate) fn numismatics_policies() -> crate::enrichment::ontology::OntologyPolicies {
    let toml = crate::recipe_templates::load_builtin("numismatics")
        .expect("numismatics is a shipped ontology template");
    crate::recipe::Recipe::from_toml(toml)
        .expect("the shipped template parses")
        .custom_atlas_spec()
        .expect("it declares an [enrichment.ontology] block")
        .policies()
}
