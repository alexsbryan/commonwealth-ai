// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tree-sitter grammar a next-edit lane parses with, supplied by the
//! caller.
//!
//! The extension → grammar registry (the thing that decides `.tsx` is a
//! DIFFERENT grammar from `.ts`, not a suffix of it) lives in
//! `corpus-engine`, and this crate may not name it: the code-intel package
//! reaches only its own crates plus the shared leaves, and `corpus-engine`
//! is neither (`docs/CODE_TOOLING_BOUNDARY.md` §3 rule 4;
//! `quality/ARCH_LAYERS.toml` `[[package]] code-intel`). A second table
//! here is the one thing the lanes' own comments forbid — `.tsx` routing
//! would drift between the two — so the lookup arrives as a value at the
//! boundary that already holds an engine: the route shell. The lanes stay
//! pure: one lookup, one tree.
//!
//! This is the package's usual injection shape (`docs/CODE_TOOLING_BOUNDARY.md`
//! §3 rule 5: every LLM-bound path takes an injected completion function);
//! the grammar is injected for the same reason the completion is.

/// One grammar: the registry's language id — the lanes' allow-lists key on
/// it — and the tree-sitter handle itself.
pub struct Grammar {
    pub id: &'static str,
    pub language: tree_sitter::Language,
}

/// Extension (without the leading dot) → grammar. `None` is "no grammar is
/// registered for this extension", which every lane treats as "cannot
/// judge": the site set is left exactly as it was rather than guessed at.
pub type GrammarLookup = fn(&str) -> Option<Grammar>;
