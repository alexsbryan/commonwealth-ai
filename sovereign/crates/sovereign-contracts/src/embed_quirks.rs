// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE embed-quirks table: how a text must be prepared before an embedding
//! model sees it, and which family a given model id belongs to.
//!
//! # Why it lives here
//!
//! `EmbedQuirks` was defined in `sovereign-core::model_family` and applied in
//! `sovereign-inference`'s embed slot, with the EOS string written as a literal
//! at three call sites. That was fine while the daemon was the only embedder.
//! It is not fine now: `corpus-mcp` embeds against a bare OpenAI-compatible
//! endpoint, may NOT depend on `sovereign-core` (`tests/no_inference_stack.rs`,
//! boundary-gate), and must produce the SAME vector space the corpus was built
//! in or its queries land beside the index rather than in it.
//!
//! `sovereign-contracts` is the only crate all three consumers —
//! `sovereign-core`, `corpus-engine` and `corpus-mcp` — already depend on, so
//! this home costs nobody a new dependency edge (ARCH §8, §19). It already
//! depends on `oicp-types`, where [`PoolingStrategy`], [`NormalizationStrategy`]
//! and `EmbedModelInfo` live, so the strategy enums are reused rather than
//! copied a third time. `kernel-types` is lower still, but `corpus-mcp` does not
//! depend on it and it is the identity/provenance kernel — an instruction table
//! is model configuration, which is this crate's business.
//!
//! # The one decider (ARCH §10.6)
//!
//! Every string here appears once. [`EmbedQuirks::prepare_document`] and
//! [`EmbedQuirks::prepare_query`] are the only two places that assemble an
//! embed input, and both the daemon's embed slot and `corpus-mcp`'s HTTP client
//! call them. A caller that formats its own prefix is a bug.
//!
//! # Measured, 2026-09-07 (note 500f1229, artifacts under
//! `test-artifacts/ei6b-mechanism/`)
//!
//! Against `wessex-hoard`, a corpus the current stack built, cosine of a
//! re-embedding against the stored vector:
//!
//! | side     | raw text | prepared here |
//! |----------|----------|---------------|
//! | document | 0.9968   | 0.9999        |
//! | query (atlas seed table) | 0.8605 | 0.9888 |
//!
//! The query instruction is worth +0.128 mean cosine. It is not a nicety.

use oicp_types::{NormalizationStrategy, PoolingStrategy};
use serde::{Deserialize, Serialize};

/// How one embedding model's inputs must be prepared, and what shape it emits.
///
/// Applied by the CALLER, before tokenization — the instruction and the EOS
/// marker are ordinary text as far as the model is concerned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedQuirks {
    pub pooling: PoolingStrategy,
    pub normalize: NormalizationStrategy,
    /// Prepended to query-side inputs at inference time.
    /// Empty string = no instruction prefix.
    pub query_instruction: String,
    /// Prepended to document-side inputs at ingestion time.
    pub document_instruction: String,
    /// The literal appended to every input, or `None` for a model that wants
    /// none.
    ///
    /// ONE field rather than an `append_eos_token: bool` beside a hardcoded
    /// `"<|endoftext|>"`: whether to append and what to append are one
    /// decision, and while they were two the string was a literal in three
    /// call sites of `embed_slot.rs` and would have had to become a fourth in
    /// `corpus-mcp` (ARCH §10.6). Deserialises from `"embed.eos_token"` in a
    /// `models.toml` `quirks_override`.
    #[serde(default)]
    pub eos_token: Option<String>,
    /// Output vector dimensionality. Used to validate index compatibility
    /// at open time and to reject mismatched BYOM swaps at startup.
    pub output_dimensions: usize,
}

impl EmbedQuirks {
    /// The Qwen3-Embedding family — the one row in the table today.
    ///
    /// `output_dimensions` is the 0.6B value; the 4B (2560) and 8B (4096)
    /// variants override it in `models.toml`.
    pub fn qwen3_embedding() -> Self {
        Self {
            pooling: PoolingStrategy::Last,
            normalize: NormalizationStrategy::Application,
            query_instruction: "Instruct: Given a search query, retrieve \
                                relevant passages that answer the query\nQuery: "
                .into(),
            document_instruction: String::new(),
            eos_token: Some("<|endoftext|>".into()),
            output_dimensions: 1024,
        }
    }

    /// Which family a model id belongs to, and its quirks — `None` when the id
    /// matches no known family.
    ///
    /// `None` is a REFUSAL, not a default (ARCH §18.3). A caller that cannot
    /// name the family must send raw text and SAY so; silently applying some
    /// other family's instruction would put its vectors in a space nothing
    /// else occupies, exit 0, and be discovered as bad retrieval months later.
    /// That is precisely the failure this module was written after.
    ///
    /// `stem` is a model id with any `.gguf` suffix already stripped — see
    /// `corpus-mcp`'s `embed_model_stem`, which is the one normaliser.
    pub fn for_model_stem(stem: &str) -> Option<(&'static str, Self)> {
        let s = stem.to_ascii_lowercase();
        if s.contains("qwen3-embedding") || s.contains("qwen-embedding") {
            Some(("Qwen3Embedding", Self::qwen3_embedding()))
        } else {
            None
        }
    }

    /// The document-side input: `document_instruction` + text + EOS.
    pub fn prepare_document(&self, text: &str) -> String {
        self.prepare(&self.document_instruction, text)
    }

    /// The query-side input: `query_instruction` + text + EOS.
    ///
    /// Not a twin of [`prepare_document`](Self::prepare_document) by accident —
    /// on an asymmetric, instruction-aware embedder the two sides are
    /// deliberately different spaces, and mixing them is the substitution
    /// §18.3 forbids.
    pub fn prepare_query(&self, text: &str) -> String {
        self.prepare(&self.query_instruction, text)
    }

    fn prepare(&self, instruction: &str, text: &str) -> String {
        let eos = self.eos_token.as_deref().unwrap_or("");
        let mut s = String::with_capacity(instruction.len() + text.len() + eos.len());
        s.push_str(instruction);
        s.push_str(text);
        s.push_str(eos);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwen3_embedding_is_last_pooled_and_application_normalised() {
        let q = EmbedQuirks::qwen3_embedding();
        assert!(matches!(q.pooling, PoolingStrategy::Last));
        assert!(matches!(q.normalize, NormalizationStrategy::Application));
        assert!(q.document_instruction.is_empty());
        assert!(q.query_instruction.starts_with("Instruct: "));
        assert!(q.query_instruction.ends_with("Query: "));
        assert_eq!(q.eos_token.as_deref(), Some("<|endoftext|>"));
    }

    /// The two sides must not collapse into one another. The daemon's atlas
    /// seed tables are query-side and its chunks are document-side; a caller
    /// that used the wrong one measured 0.8605 where 0.9888 was available.
    #[test]
    fn document_and_query_preparation_differ() {
        let q = EmbedQuirks::qwen3_embedding();
        let d = q.prepare_document("hello");
        let s = q.prepare_query("hello");
        assert_eq!(d, "hello<|endoftext|>");
        assert!(s.starts_with("Instruct: "));
        assert!(s.ends_with("hello<|endoftext|>"));
        assert_ne!(d, s);
    }

    #[test]
    fn no_eos_token_appends_nothing() {
        let mut q = EmbedQuirks::qwen3_embedding();
        q.eos_token = None;
        assert_eq!(q.prepare_document("hello"), "hello");
    }

    /// An unrecognised family is `None`, never a default family. The whole
    /// point of the refusal (ARCH §18.3).
    #[test]
    fn unknown_model_stem_gets_no_quirks() {
        assert!(EmbedQuirks::for_model_stem("nomic-embed-text").is_none());
        assert!(EmbedQuirks::for_model_stem("mxbai-embed-large-v1").is_none());
        assert!(EmbedQuirks::for_model_stem("").is_none());
    }

    #[test]
    fn qwen3_embedding_ids_resolve_however_they_are_cased_or_labelled() {
        // The three labels this repo's own corpora were built under.
        for id in [
            "Qwen3-Embedding-0.6B-Q8_0",
            "qwen3-embedding-0.6b-q8_0",
            "qwen-embedding-0.6b",
        ] {
            let (family, q) = EmbedQuirks::for_model_stem(id)
                .unwrap_or_else(|| panic!("`{id}` must resolve to a family"));
            assert_eq!(family, "Qwen3Embedding");
            assert_eq!(q, EmbedQuirks::qwen3_embedding());
        }
    }
}
