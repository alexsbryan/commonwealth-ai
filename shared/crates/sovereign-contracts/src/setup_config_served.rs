// SPDX-License-Identifier: AGPL-3.0-or-later
//! What this node serves, by name: the model ids and context window a
//! caller addresses it by. A sibling of `setup_config.rs`, which is at its
//! arch-gate ceiling.

use super::{default_context_size, SetupConfig};

impl SetupConfig {
    /// The chat context window this node budgets against.
    ///
    /// Delegates to `[models] context_size` on a holder. On a terminal there is
    /// no local slot to read, so this is the documented default — and it is an
    /// APPROXIMATION of the entry node's real window, not a reading of it. The
    /// value drives client-side prompt budgeting only; the entry node enforces
    /// its own limit and refuses an over-long prompt on its own terms, so a
    /// mismatch costs a rejected turn rather than a silently truncated one.
    pub fn effective_context_size(&self) -> u32 {
        self.models
            .as_ref()
            .map(|m| m.effective_context_size())
            .unwrap_or_else(default_context_size)
    }

    /// The primary model's GGUF file stem — the id the slot manager resolves
    /// by, and the name a caller passes as `model` over HTTP.
    ///
    /// `None` on a terminal (no slots) and on a primary path with no stem.
    /// Collapses a chain that was copy-pasted at six call sites
    /// (`audit_extract`, `code_cmd`, `chat_cmd::bootstrap`,
    /// `recipe_agent_live_trial`, `deep_research::launch`, `mesh_bench`), each
    /// of which had to be updated in lockstep to stay right (§10.6).
    pub fn primary_model_stem(&self) -> Option<String> {
        self.models.as_ref()?.primary_stem()
    }

    /// The embed model's GGUF file stem. `None` on a terminal.
    ///
    /// Callers that merely LABEL may fall back to a default name; callers that
    /// actually embed must not, because this name decides which vector space
    /// the result lands in (`sovereign-cli-shared::models`'s doc states that
    /// split, and `build_daemon_embed_fn` is the side that refuses).
    pub fn embed_model_stem(&self) -> Option<String> {
        match self.engine.own_embed_path() {
            Some(path) => path.file_stem()?.to_str().map(str::to_string),
            None => self.models.as_ref()?.embed_stem(),
        }
    }

    /// The embed model this node's own embedding calls land in — the local
    /// GGUF's stem on a holder, the ENTRY NODE's recorded id on a terminal.
    ///
    /// A terminal embeds over HTTP, so the vector space its text lands in is
    /// the entry node's. Anything keyed on "which space is this" — the corpus
    /// engine's cache key, the provider's `embed_model_id()`, a label — wants
    /// this one.
    ///
    /// **Not** what this node advertises to peers; that is
    /// [`advertised_embed_model_id`], and the two differ on purpose. Answering
    /// both from one accessor is what made a terminal offer its entry node's
    /// model as its own (§10.6): the chain `stem → entry` is right for the
    /// first question and a capability lie for the second.
    ///
    /// `None` means this node cannot name its embedding space at all — an
    /// unconfigured node, or a terminal whose entry node declares no embed
    /// slot. Callers that persist or compare under this name must map `None`
    /// to the trait's `"unknown"` sentinel rather than to an empty string:
    /// `""` is not the sentinel, so it reads downstream as a real model named
    /// empty-string and matches other rows stored the same way (§18.3, and
    /// `sovereign-core::memory`'s `model_known` check is the reader).
    pub fn local_embed_model_id(&self) -> Option<String> {
        self.embed_model_stem()
            .or_else(|| self.node.entry_embed_model.clone())
    }

    /// The embed model this node offers MESH PEERS — `None` on a terminal.
    ///
    /// Deliberately local-only, and deliberately its own name rather than a
    /// call to [`embed_model_stem`]: this is the one question whose answer must
    /// never fall back to the entry node. A terminal can embed, but only by
    /// forwarding, so advertising an embed model here would have the
    /// collaborative-ingestion planner partition work onto a node that can only
    /// proxy every chunk straight back to the machine the planner was trying to
    /// spread load off (`sovereign-mesh::capabilities`, whose own doc names
    /// `None` as "don't include me in distribution").
    pub fn advertised_embed_model_id(&self) -> Option<String> {
        self.embed_model_stem()
    }
}
