// SPDX-License-Identifier: AGPL-3.0-or-later
//! The phone's CACHE-AND-VIEW shapes — what a Tauri command hands the
//! WebView, and what the SQLite cache rows deserialize into.
//!
//! These are deliberately NOT the wire. The turn protocol lives in
//! `sovereign-contracts` and is consumed through `sovereign-turn-client`
//! (see the note at the bottom of this file); what remains here is the
//! projection the mobile UI reads, plus the handful of REST envelopes the
//! tenant-front `sovereign-server` serves that the turn client does not.
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
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub citations: Vec<Citation>,
    /// The chat-UI `metadata` blob (`{provenance, retrieved_chunks}`),
    /// built host-client-side from `provenance`/`citations` so a
    /// reopened (hydrated) message renders citations and resolves
    /// reader clicks identically to a freshly-streamed one. The host
    /// never sends this key — it's populated on the hydrate path (see
    /// `commands::conversation`), mirroring the WS `metadata_blob`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// The spec's `RESPONSE_PROVENANCE` and `CITATION` — taken from the
/// contract, not re-declared here.
///
/// Until 2026-09-10 this file carried `ProvenanceDto` / `SourceDto` /
/// `CitationDto`, hand-copied field by field from the same projection the
/// host serializes. They were byte-compatible on the day they were written
/// and had already fallen behind by two fields (`Citation::url`,
/// `Citation::provenance_tier`) — the phone could not render a source URL
/// the wire had been carrying for weeks, and nothing reported it, because a
/// mirror cannot fail: it just quietly describes less than arrives.
///
/// Re-exported rather than imported at each site so the `remote::dto` path
/// every caller already spells keeps working, and so the ONE line that says
/// where these types come from is here.
pub use sovereign_contracts::types::projection::{Citation, Provenance};

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

// ─── The turn protocol ────────────────────────────────────────
//
// NOT HERE, and that is the point of sv-surface R6. This file used to end
// with a `ServerEvent` enum hand-copied from `sovereign_server::approval`
// — four variants plus `#[serde(other)] Ignored`, which is a catch-all that
// makes every frame the phone does not understand look exactly like a frame
// that does not exist.
//
// The wire vocabulary is `sovereign_contracts::types::{TurnFrame, TurnPrompt,
// TurnAnswer, TurnNotice, TurnRequest}` and the client that speaks it is
// `sovereign-turn-client`. `remote::stream` consumes both directly, so the
// phone gains `Prompt`, `Notice::ResolveAck` and `Notice::TurnSettled` by
// construction rather than by someone remembering to copy them across.
//
// `tests/census.rs` fails if a mirror grows back.
