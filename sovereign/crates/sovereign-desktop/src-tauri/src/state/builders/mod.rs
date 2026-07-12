// SPDX-License-Identifier: AGPL-3.0-or-later
//! Construction helpers extracted from `bootstrap_with_progress`
//! (§3.3 `state.rs` decomposition). Each builder is a **pure relocation**
//! of a self-contained phase — same statements, same order, same `?`
//! semantics — invoked in place from `bootstrap_with_progress`.
//!
//! Only phases that are genuinely contiguous and self-contained live
//! here: `config`, `builtin_skills`, `health`, `store`, `inference`,
//! and `knowledge_view`. Two parts of bootstrap are intentionally NOT
//! builders because they are *interleaved*, not contiguous:
//!
//! - The `tools` registry is mutated across the whole bootstrap (before
//!   AND after the corpus / health phases, because the later tools
//!   depend on `corpus_engine`).
//! - The embedded-`EmbeddedDaemon` wiring is spread over four scattered
//!   sites (`set_corpus_engine` / `set_state_store`, then
//!   `set_inference_provider`, the CliSetup/MCP block, then
//!   `set_embed_model_info` ~300 lines later) and is order-constrained:
//!   `set_corpus_engine` *must* run before `try_resume` starts the HTTP
//!   listener, or the first gossip rounds advertise empty corpora.
//!
//! Extracting either would require reordering — unsafe to do blind in a
//! startup path with no CI coverage (it needs a loaded GGUF). Both stay
//! inline until they can be smoke-tested via `cargo tauri dev`.

pub mod health;
pub mod inference;
pub mod knowledge_view;
pub mod model_compat;
pub mod store;

#[cfg(test)]
pub(crate) mod test_support;
