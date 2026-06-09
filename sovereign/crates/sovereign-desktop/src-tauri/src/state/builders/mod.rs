//! Construction helpers extracted from `bootstrap_with_progress`
//! (§3.3 `state.rs` decomposition). Each builder is a **pure relocation**
//! of a self-contained phase — same statements, same order, same `?`
//! semantics — invoked in place from `bootstrap_with_progress`.
//!
//! Only phases that are genuinely contiguous and self-contained live
//! here. The `tools` registry is intentionally NOT a builder: it is
//! mutated across the whole bootstrap (before AND after the corpus /
//! health phases, because the later tools depend on `corpus_engine`),
//! so extracting it would require reordering — unsafe to do blind in a
//! startup path that has no CI coverage (it needs a loaded GGUF). Those
//! phases stay inline until they can be smoke-tested via `cargo tauri dev`.

pub mod health;
pub mod store;

#[cfg(test)]
pub(crate) mod test_support;
