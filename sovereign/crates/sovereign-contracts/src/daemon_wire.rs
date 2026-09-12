// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire shapes a daemon's HTTP routes answer with — named by the daemon that
//! serves them AND by every client that parses them (sv-surface svt-3).
//!
//! # Why they are here and not beside their routes
//!
//! Each of these was defined inside a `sovereign_mesh::*_http` module, which
//! is where the route that emits it lives. That is the right place for a
//! ROUTE. It is the wrong place for a TYPE, because a client that only wants
//! to parse the answer had to link the whole serving host to name it: the
//! desktop's `use sovereign_mesh::lc_http::OcrAvailability` is one `bool` on
//! the wire and a `sovereign-desktop -> sovereign-mesh` layer edge in
//! `quality/ARCH_LAYERS.toml`.
//!
//! Moving them down is what lets the answer keep ONE definition (ARCH
//! principle 8) while the client stops linking the server. Every type here is
//! pure serde over primitives — no handle, no store, no engine — which is the
//! test for whether a wire shape belongs at this layer at all. A DTO that
//! closes over a runtime type stays where it is until the type it closes
//! over has a home down here too: `OriginKind` got one in `oicp_types::
//! origin` (2026-09-11), which is what let the whole mesh view come down
//! (`mesh.rs`). Two still do not — `mesh_http::StatusResponse` (over the
//! worker-eligibility view and a cross-family transport path) and
//! `lc_http::IngestProgress` (over the enrichment phase file) — and for
//! those the client is owed a READ, not the type: `MeshStatusSummary` and
//! `IngestProgressView` are the fields a client reads, parsed from the same
//! bytes and pinned to the route's type by a test in `sovereign-mesh`.
//!
//! `sovereign-mesh` re-exports every item below at its historical
//! `*_http::Name` path, so the routes, their tests and the CLI are unchanged
//! — this is a relocation, not a rename.

use serde::{Deserialize, Serialize};

// Per-family files, re-exported flat so every wire shape keeps the one
// path `sovereign_contracts::daemon_wire::Name` (the size ratchet is per
// crate, not per file; the split is for the reader).
pub mod documents;
pub mod enrich;
pub mod ingest;
pub mod local_corpus;
pub mod mesh;
pub mod meshapp;
pub mod recipe_projects;
pub mod workflows;

pub use documents::*;
pub use enrich::*;
pub use ingest::*;
pub use local_corpus::*;
pub use mesh::*;
pub use meshapp::*;
pub use recipe_projects::*;
pub use workflows::*;
pub mod provenance;
pub use provenance::*;
pub mod research;
pub use research::*;

// ─── Local corpus — `/internal/corpus/local/…` (`lc_http`) ──────

/// Answer of `GET /internal/corpus/local/ocr-available`. A named
/// field, not a bare `true`: "OCR is unavailable" and "this daemon did
/// not understand the question" must not both read as `false`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OcrAvailability {
    /// Whether this daemon can read a scanned page.
    pub available: bool,
}

/// Answer of `POST …/{corpus}/cancel`. `cancelled` is "there WAS an
/// in-flight job and it is now cancelled" — deliberately not the
/// `AckResponse.ok` field, which means "the call succeeded". Both are
/// true for a cancel that found nothing to cancel, and collapsing them
/// would tell the pane a job was stopped when none was running.
#[derive(Debug, Serialize, Deserialize)]
pub struct CancelAck {
    /// The corpus the cancel was addressed to.
    pub corpus_id: String,
    /// There WAS an in-flight job and it is now cancelled.
    pub cancelled: bool,
}

/// Answer of `POST …/{corpus}/ingest` — the job id, and where to read
/// its progress. `corpus_watch_http::EnrichJobAck`'s shape plus the
/// route that reports it, because a job id with no named reporter is
/// how a caller ends up inventing a poll loop of its own.
#[derive(Debug, Serialize, Deserialize)]
pub struct IngestJobAck {
    /// The corpus being ingested.
    pub corpus_id: String,
    /// The host's job id. The job is the daemon's, so a client that
    /// minted a second id for it would be two names for one thing
    /// (ARCH principle 8).
    pub job_id: String,
    /// The call was accepted.
    pub ok: bool,
    /// The route that reports this job. Always populated.
    pub progress_route: String,
}

/// One search hit. A wire twin of the desktop's `LocalSearchHit` by
/// NECESSITY, not by choice: `manager.search` answers
/// `Vec<ScoredChunk>`, and `ScoredChunk` is deliberately
/// non-serialisable ("in-process ranking currency only",
/// `sovereign-contracts/src/types/mod.rs`). The desktop already
/// projects into exactly these four fields before handing them to the
/// pane; the projection lives with the route and the name is kept so the
/// repoint is a changed `use`, not a changed call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSearchHit {
    /// The matched chunk's text.
    pub content: String,
    /// The source document's title, when the chunk carries one.
    pub title: Option<String>,
    /// The corpus the hit came from.
    pub corpus_id: String,
    /// Relevance score, as the ranker produced it.
    pub score: f32,
}

// ─── Notes — `/v1/notes/…` (`notes_http`) ───────────────────────

/// One note on the wire — every field of `corpus_engine_notes::Note`,
/// which is `#[derive(Debug, Clone)]` and carries no serde impls of its
/// own.
///
/// Projected rather than derived-on: `Note` is a store type with three
/// enum-shaped `String` fields (`scope`, `source`) and a `Vec<NoteScope>`
/// nowhere in sight, and putting `Serialize` on it would make the store
/// crate own a wire contract it has no reason to. This is the ONE
/// projection for the family — `Deserialize` too, so a caller parses
/// back into the same struct the daemon emitted rather than a hand-rolled
/// twin that can drift (the `atlas_view` property that makes a repoint a
/// repoint).
///
/// Nothing is dropped. `payload_json` in particular crosses verbatim as
/// a STRING, not as parsed JSON: it is the caller's schema
/// (`LessonPayload`, the recipe-author kinds), the store never parsed it,
/// and re-encoding it here would make the router a second decider about
/// a shape it does not own.
///
/// The `Note -> NoteEntry` projection stays beside the route, in
/// `sovereign_mesh::notes_http::note_entry`: the store type is three
/// layers up from here and the kernel cannot name it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteEntry {
    /// Note id.
    pub id: String,
    /// The note's kind tag (`decision`, `todo`, a skill-defined kind …).
    pub kind: String,
    /// The note body.
    pub content: String,
    /// Symbols this note is filed against.
    pub symbols: Vec<String>,
    /// Files this note is filed against.
    pub files: Vec<String>,
    /// The session that wrote it.
    pub session_id: String,
    /// RFC 3339.
    pub created_at: String,
    /// The tool that wrote it, when a tool did.
    pub tool_name: Option<String>,
    /// Unix seconds; `None` means active.
    pub retired_at: Option<i64>,
    /// Who retired it.
    pub retired_by: Option<String>,
    /// `"global"` | `"feature"` | `"session"`.
    pub scope: String,
    /// The feature this note is scoped to, for `scope == "feature"`.
    pub feature_id: Option<String>,
    /// The narrower note this one was promoted from.
    pub promoted_from: Option<String>,
    /// A related entity the note names.
    pub related_entity: Option<String>,
    /// `"agent"` | `"committed"` | `"extracted"` | `"inferred"` | `"observed"`.
    pub source: String,
    /// The note this one supersedes.
    pub supersedes: Option<String>,
    /// The caller's own schema, verbatim. Never re-encoded here.
    pub payload_json: Option<String>,
    /// The mesh node this note arrived from, for a gossiped note.
    pub origin_node_id: Option<String>,
    /// Unix seconds the origin sent it.
    pub sent_at: Option<i64>,
    /// Unix seconds this node received it.
    pub received_at: Option<i64>,
}

// ─── Documents — `/v1/documents/…` (`documents_http`) ───────────

/// A document in the legacy `documents` table with no `DocumentAsset`
/// record — an upload from the old paperclip path.
///
/// The desktop's `LegacyDocumentEntry`, moved: it was `Serialize`-only
/// up there (a Tauri return), and a wire type has to parse back, which
/// is why this carries `Deserialize` too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyDocumentEntry {
    /// The chunk store's `source` key — the promotion handle.
    pub source: String,
    /// Last path segment of `source`.
    pub filename: String,
    /// Chunks this document contributed.
    pub chunk_count: usize,
    /// Words across those chunks.
    pub word_count: usize,
}

// ─── Conversations — `/v1/conversations/…` (`turn_http`) ────────

/// One row of `GET /v1/conversations`.
///
/// One schema, served over HTTP and returned to a webview by a client that
/// parses it. The hand-kept desktop twin this replaced had already drifted
/// from the wire's `skip_serializing_if` — it emitted `"title": null` where
/// the wire omits the key — which is the exact byte-compat break that one
/// shared definition makes impossible.
#[derive(Debug, Serialize)]
pub struct ConversationListEntry {
    /// Conversation id.
    pub id: String,
    /// The title, once one has been derived. Omitted, never `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

/// Answer of `POST /v1/conversations`.
#[derive(Debug, Serialize)]
pub struct CreateConversationResponse {
    /// The new conversation's id.
    pub id: String,
    /// Unix seconds.
    pub created_at: i64,
    /// The allow-list that was seeded, echoed back VERBATIM when one was
    /// sent and omitted otherwise. The echo is what lets a client tell a
    /// daemon that scoped the conversation from one that predates the field
    /// and ignored it — serde drops unknown keys, so without this a stale
    /// daemon would mint an unscoped conversation and say nothing
    /// (ARCH principle 6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_corpora: Option<Vec<String>>,
}

// ─── External MCP config — `/v1/mcp/servers` (`mcp_config_http`) ─

/// One configured MCP server, joined with what the daemon's tool registry
/// actually holds for it.
///
/// `connected` / `tool_count` / `error` are deliberately absent — the daemon
/// keeps no `McpServerManager`, so they have no source there — and
/// [`Self::live_tool_count`] is the observation served in their place, with
/// [`McpMountStatus::reason`] carrying the absence in words.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    /// The server's configured name.
    pub name: String,
    /// Its endpoint.
    pub url: String,
    /// Operator-supplied description.
    pub description: Option<String>,
    /// Whether the operator has this server turned on.
    pub enabled: bool,
    /// Whether the server is configured for bearer auth.
    pub bearer: bool,
    /// Env var the bearer token is read from — the headless / CI
    /// override. `None` for no-auth servers.
    pub token_env: Option<String>,
    /// Whether a token is currently stored in the secret file for this
    /// server (the primary path).
    pub has_token: bool,
    /// Tools in the daemon's live registry whose id carries this server's
    /// `mcp_<name>_` prefix. `0` on a daemon with no `/mcp` mount at all —
    /// which [`McpMountStatus`] distinguishes from "mounted, zero tools".
    pub live_tool_count: usize,
}

/// Whether the daemon has a tool mount to count against, and — when it
/// does not — why.
///
/// Mirrors `sovereign_mesh::daemon_services::McpSurface`, which exists for
/// exactly this reason: "this host serves no tools" and "`notes.db` would
/// not open" are different operational facts with different fixes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpMountStatus {
    /// `true` when a tool registry was available to fold the counts over.
    /// When `false` every `live_tool_count` above is `0` because there was
    /// nothing to count, NOT because the servers registered nothing.
    pub mounted: bool,
    /// Total tools in the daemon's registry, MCP and native alike — the
    /// denominator for the per-server counts.
    pub total_tools: usize,
    /// Why connect status and connect errors are not in this payload, in
    /// words, on every response. Always present: an absence a caller has
    /// to infer from a missing key is indistinguishable from an old host
    /// (ARCH principle 6).
    pub reason: String,
}

/// Answer of `GET /v1/mcp/servers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServersResponse {
    /// The configured servers, annotated with live registry counts.
    pub servers: Vec<McpServerView>,
    /// Whether there was a registry to count against at all.
    pub mount: McpMountStatus,
}

/// Where one corpus's index build stands. Answer of
/// `GET /internal/corpus/{corpus}/index/progress`; the build itself is
/// accepted by `POST /internal/corpus/{corpus}/index/build` with an
/// [`IngestJobAck`]. Added 2026-09-11 (sv-surface svt-3) so the desktop's
/// `build_corpus_index` stops opening the index with an engine of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexBuildProgress {
    pub corpus_id: String,
    /// The daemon's job id from the ack; empty when no build has been asked
    /// for this corpus in this daemon's lifetime.
    pub job_id: String,
    pub state: IndexBuildState,
    /// Whole-percent progress of the current sub-phase, 0..=100.
    pub pct: u64,
    /// Set only in [`IndexBuildState::Error`]; the failure text verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The three states a build can report. `Idle` is "never asked", reported
/// rather than defaulted to a finished shape (ARCH principle 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexBuildState {
    Idle,
    Building,
    Complete,
    Error,
}
