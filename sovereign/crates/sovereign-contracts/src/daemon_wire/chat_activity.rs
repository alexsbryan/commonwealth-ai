// SPDX-License-Identifier: AGPL-3.0-or-later
//! Chat-activity rollup — the answer of `GET /v1/admin/chat-activity`
//! (`sovereign_mesh::admin_http`).
//!
//! Defined at this layer rather than in `sovereign-store`, where the three
//! structs lived until 2026-09-12, for the reason the module header gives:
//! the surface that RENDERS this rollup parses it off a socket, and naming
//! the answer must not cost it a link to the store that computed it. The
//! desktop's Mesh Health pane spelled `sovereign_store::sqlite::
//! ChatActivitySummary` and paid a `sovereign-desktop -> sovereign-store`
//! edge for three `u64`s and two `Vec`s. `sovereign-store` re-exports all
//! three at their historical paths, so the store side is unchanged.

use serde::{Deserialize, Serialize};

/// Per-corpus chunk-retrieval rollup for the chat activity surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCorpusUsage {
    pub origin: String,
    pub chunks: u64,
    /// True when these chunks came from a mesh peer (the provenance
    /// `SourceSummary.from_peer` was set).
    pub from_peer: bool,
}

/// Per-model turn + token rollup for the chat activity surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatModelUsage {
    pub model: String,
    pub turns: u64,
    pub tokens_generated: u64,
}

/// Read-side rollup of the user's own chat usage, derived entirely from the
/// `ResponseProvenance` already persisted under `metadata["provenance"]` on
/// each assistant message. There is no new write path: every turn already
/// records tokens + retrieved sources, so the summary is *derived* rather
/// than separately recorded — the data is durable because the messages are.
///
/// **Whose messages.** The rollup is over the store the DAEMON holds, which
/// since 2026-09-12 is the only store a turn is ever written to. The
/// paragraph this doc replaced said "chat runs in the in-process Runtime (it
/// never crosses a daemon HTTP boundary, so the daemon's Activity ledger
/// can't see it)" — that stopped being true at sv-surface R5, when every
/// turn became the daemon's, and the desktop kept summarising its OWN
/// `sovereign.db`: on an attached boot, a file no turn had written to since.
/// The numbers were not wrong, they were about nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatActivitySummary {
    pub window_days: u32,
    pub turns: u64,
    pub tokens_generated: u64,
    pub chunks_retrieved: u64,
    pub by_corpus: Vec<ChatCorpusUsage>,
    pub by_model: Vec<ChatModelUsage>,
}
