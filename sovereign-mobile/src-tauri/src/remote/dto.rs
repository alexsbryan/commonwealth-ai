//! Serde mirror of the Phase-1 `sovereign-server` JSON. Kept in one
//! place so the wire contract has a single definition on the client.
//! The data DTOs are also `Serialize` so commands can return them
//! across the Tauri boundary to the WebView.
//!
//! Version fields (`synced_version`/`server_version`) are `Option` and
//! currently absent on the wire — the Phase-1 projection doesn't yet
//! surface the Lamport `version`. Follow-up: have the server include it
//! on `MessageEntry` + a `synced_version` on conversations so the cache
//! reconcile is precise rather than `updated_at`-based.

use serde::{Deserialize, Serialize};

// ─── REST ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationDto {
    pub id: String,
    pub title: Option<String>,
    #[serde(default)]
    pub messages: Vec<MessageDto>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub synced_version: Option<i64>,
    /// `true` once the host has indexed this conversation into the
    /// per-identity conversation corpus (then it's retrievable like any
    /// other corpus). The phone neither builds nor stores that corpus —
    /// it only reflects this flag. `false` until the server surfaces it.
    #[serde(default)]
    pub indexed_in_corpus: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDto {
    pub id: String,
    #[serde(default)]
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub status: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub server_version: Option<i64>,
    #[serde(default)]
    pub provenance: Option<ProvenanceDto>,
    #[serde(default)]
    pub citations: Vec<CitationDto>,
}

/// The spec's `RESPONSE_PROVENANCE` (server's reduced projection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceDto {
    pub inference_backend: String,
    #[serde(default)]
    pub routing_tier: Option<String>,
    #[serde(default)]
    pub ttft_ms: Option<i64>,
    #[serde(default)]
    pub total_ms: Option<i64>,
    #[serde(default)]
    pub sources: Vec<SourceDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDto {
    pub origin: String,
    #[serde(default)]
    pub count: i64,
    #[serde(default)]
    pub from_peer: Option<String>,
}

/// The spec's `CITATION` — the `(corpus_id, chunk_id)` handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationDto {
    pub corpus_id: String,
    pub chunk_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub rank: i64,
}

/// One chunk in a reading window — the full passage text (not the
/// truncated citation snippet) served by the host's corpus engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadChunkDto {
    pub chunk_id: u64,
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// A cited passage + its surrounding context — the reader's payload.
/// Mirrors the server's `ReadingWindowResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadingWindowDto {
    pub corpus_id: String,
    pub found: bool,
    #[serde(default)]
    pub center: Option<ReadChunkDto>,
    #[serde(default)]
    pub prev: Vec<ReadChunkDto>,
    #[serde(default)]
    pub next: Vec<ReadChunkDto>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorpusListDto {
    #[serde(default)]
    pub corpora: Vec<CorpusRefDto>,
}

/// The spec's `CORPUS_REF`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusRefDto {
    pub corpus_id: String,
    pub display_name: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub chunk_count: i64,
    /// Privacy posture: `"local"` (private to this host) vs `"mesh"`.
    /// The phone badges `local` sources as private-to-this-host (§7).
    #[serde(default)]
    pub scope: Option<String>,
    /// `false` = never sharded/gossiped to peers.
    #[serde(default)]
    pub mesh_shared: bool,
}

// ─── WebSocket ServerEvent ────────────────────────────────────
//
// Mirrors `sovereign_server::approval::ServerEvent` — tagged
// `{ "type": "...", "data": { ... } }`, snake_case. v1 consumes the
// streaming variants; the approval/step variants are accepted-and-
// ignored (tool approvals are out of scope). Deserialize-only:
// `#[serde(other)]` is not valid for serialization.

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerEvent {
    Token {
        message_id: String,
        chunk: String,
    },
    Complete {
        message_id: String,
        #[serde(default)]
        provenance: Option<ProvenanceDto>,
        #[serde(default)]
        citations: Vec<CitationDto>,
    },
    StreamError {
        message: String,
        #[serde(default)]
        retry_after_secs: Option<u64>,
    },
    #[serde(other)]
    Ignored,
}
