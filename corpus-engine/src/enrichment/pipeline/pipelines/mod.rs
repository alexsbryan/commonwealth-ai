//! Concrete `Pipeline` implementations.
//!
//! Each submodule contributes one pipeline + its prompt assets.
//! Registration happens in `super::registry::PipelineRegistry::builtin`.

pub mod conversation_atlas;
pub mod engineering_atlas;
pub mod literary;
pub mod literary_atlas;
pub mod obsidian_atlas;
pub mod philosophy_atlas;
pub mod referential_atlas;
