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
pub mod ontology_prompt;
pub mod ontology_schema;
pub mod parse_policy;
pub mod philosophy_atlas;
pub mod referential_atlas;
pub mod sketch_parse;
