// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client half of the turn protocol — how a surface asks a serving host
//! for a turn.
//!
//! # Why this exists
//!
//! `quality/TOPOLOGY.md` §3.5 draws every surface — desktop, `svrn chat`, the
//! server, the bench — above ONE process that assembles a `Runtime`, with a
//! turn protocol between them. Phase 5 built the whole serving side of that
//! sentence: [`sovereign_contracts::types::TurnRequest`] /
//! [`sovereign_contracts::types::TurnFrame`] are the protocol,
//! `sovereign_core::runtime::serve_turn` drives a turn, and
//! `sovereign_mesh::turn_http` is the door the daemon opens.
//!
//! Nothing spoke it. The only Rust code that had ever sent a `TurnRequest`
//! was two integration tests, each with its own hand-rolled WebSocket dance —
//! so "a host stops assembling and starts connecting" was a sentence with no
//! implementation on the connecting side, and phase 6 would have grown one
//! copy per host. That is precisely how the `Runtime` recipe became three
//! copies (§10 phase 5c) and how the enrichment catalog became three copies
//! of one `config.json` before it.
//!
//! # Where it lives, and why not with the hosts
//!
//! The contract layer, beside [`oicp_client`](https://docs.rs/) — the
//! existing precedent in this workspace for "protocol types plus the client
//! that speaks them". Its entire non-leaf dependency is
//! `sovereign-contracts`; it cannot see a `Runtime`, a store or a corpus, and
//! that is the point. A surface should be able to depend on this and on
//! nothing else that a serving host needs.
//!
//! # What it is the mirror of
//!
//! [`run_turn`] is the client-side twin of `serve_turn`: one implementation
//! of "drive one turn to completion and tell me what it did". Five CLI ask
//! commands each had their own version of the in-process equivalent, and each
//! ended by going back to the store to find out what the turn had done —
//! only that works from inside the process that owns the store. Here the
//! answer arrives as a [`TurnOutcome`], because `Complete` is a value that
//! serializes.
//!
//! # The socket is split (sv-surface R2, 2026-09-09)
//!
//! A turn socket answers questions WHILE it streams: the host parks its
//! executor on a [`TurnFrame::Prompt`] and the surface owes it a
//! [`TurnRequest::Answer`] — from whatever task renders the card, which is
//! never the task draining tokens. Until R2 this crate could not express
//! that: `next_frame` held `&mut self` across every read, so nothing could
//! write while a drain ran (G11), the URL could not claim approvals at all
//! (G12), and the drain stopped at `Complete`, one frame before the
//! re-synthesised answer arrives (G13). [`TurnClient::connect_with`] claims;
//! [`TurnStream::sender`] is the cloneable write half that answers
//! mid-drain; [`TurnStream::drain_after_complete`] reads the post-turn
//! window. The one-shot drains (`run_turn`, `connect`) are byte-for-byte
//! the pre-R2 behaviour — an unclaimed plain stream.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use sovereign_contracts::error::{Error, Result};

// "Ensure a backend is reachable" belongs to the client, not to the
// application — sv-surface's `sv-no-daemon-management` bar. See `reach.rs`.
pub mod reach;
// The mesh half of the client: `/v1/mesh/*` on the client port and the two
// `/internal/*` mesh-admin reads. An `impl TurnClient` block, so the surface
// is unchanged — see `mesh.rs` for why it is not in this file.
mod mesh;

#[cfg(feature = "bundled-backend")]
pub use reach::BundledBackend;
pub use reach::{NotReachable, Reached, ServingHost, CAN_BRING_UP_A_BACKEND};

// ─── The protocol, re-exported ─────────────────────────────────
//
// Every type below already crosses this crate's public surface: a
// caller cannot read a `TurnFrame`, answer a `TurnPrompt` or match on
// a `Provenance` without naming them. Re-exporting them means a
// consumer of `TurnClient` needs ONE dependency, not two — today
// `sovereign-mobile` declares `sovereign-contracts` solely to spell
// types it only ever gets from this client, and `quality/baselines/
// fan_in.tsv` counts every such declaration against contracts.
//
// This is a re-export, not a wrapper: `sovereign_turn_client::TurnFrame`
// IS `sovereign_contracts::types::TurnFrame`, the same type with the
// same `serde` shape. A parallel mirror here would be the §10.6 twin
// (`mobile-wire-mirror` in `quality/twin-plants.toml` is the census
// that catches exactly that), so nothing below is redefined.
pub use sovereign_contracts::error::{Error as TurnError, Result as TurnResult};
pub use sovereign_contracts::types::approval::ResolveOutcome;
pub use sovereign_contracts::types::projection::{Citation, Provenance, ProvenanceSource};
pub use sovereign_contracts::types::{
    ActionPreview, EpistemicState, Intent, NarrationPhase, TurnAnswer, TurnFrame, TurnMode,
    TurnNotice, TurnPrompt, TurnRequest,
};

/// What one turn did, assembled from the terminal `Complete` frame.
///
/// This is the value that used to be a re-read of the store. A surface that
/// holds one of these learned everything the turn produced without being in
/// the process that produced it — the property TOPOLOGY §3.5 is built on.
#[derive(Debug, Clone, Default)]
pub struct TurnOutcome {
    /// The assistant message the turn produced.
    pub message_id: String,
    /// The full answer text, accumulated from every `Token` frame.
    pub text: String,
    /// How the answer was produced — model, routing tier, latency.
    pub provenance: Option<Provenance>,
    /// Corpus-grounded citations, in retrieval rank order.
    pub citations: Vec<Citation>,
    /// The typed epistemic ledger, when the turn stamped one.
    pub epistemic_state: Option<EpistemicState>,
    /// The background task the turn spawned, on the agentic path.
    pub task: Option<sovereign_contracts::types::projection::TaskSummary>,
    /// How the turn was SERVED — routed intent, grounding-gate outcome and
    /// the stage ledger, projected from the persisted message metadata.
    /// `None` when the turn reported none of the three; a caller must not
    /// read that as "the ledger was empty" (ARCH §18.3).
    pub metadata: Option<sovereign_contracts::types::projection::TurnMetadata>,
}

/// What a caller wants to watch while a turn runs.
///
/// Both hooks are optional and default to dropping the signal. They are
/// separate rather than one `FnMut(TurnFrame)` because a caller almost always
/// treats them differently: tokens go to stdout as they arrive, narration
/// goes to stderr as progress. A caller that genuinely wants raw frames
/// should use [`TurnStream`] directly.
#[derive(Default)]
pub struct TurnObserver<'a> {
    /// Called with each token delta, in order. Append to render the answer.
    pub on_token: Option<&'a mut (dyn FnMut(&str) + Send)>,
    /// Called with each narration phase — what the host is doing right now.
    pub on_narration: Option<&'a mut (dyn FnMut(&NarrationPhase, &str, u64) + Send)>,
    /// Called when the host reports this turn is queued behind others.
    pub on_queue_position: Option<&'a mut (dyn FnMut(u32, u64) + Send)>,
    /// Called with each Notice — what the host said that no answer is owed
    /// (step progress, the re-synthesised answer, a proposed lesson, a
    /// resolve acknowledgement). sv-surface R2/G13: notices are part of
    /// the stream, not noise around it, and the two that fire AFTER the
    /// terminal frame reach [`TurnStream::drain_after_complete`] through
    /// this same hook.
    pub on_notice: Option<&'a mut (dyn FnMut(&TurnNotice) + Send)>,
}

/// What bounds ONE non-streaming exchange with a serving host.
///
/// Until 2026-09-11 there was nothing here: `TurnClient` built a bare
/// `reqwest::Client`, which has NO timeout of any kind, and roughly thirty
/// desktop commands inherited that — a daemon that accepted the connection
/// and then went silent hung the calling Tauri command forever, with no
/// knob anywhere to bound it. Meanwhile every hand-rolled `reqwest` call in
/// `sovereign-desktop` picked its own number (2 s, 3 s, 5 s, 10 s, 30 s,
/// 300 s, 600 s, 3600 s across eleven modules), so the migration onto this
/// client was moving callers from many private deciders onto none at all.
///
/// Three bounds rather than one, because they answer different questions
/// and only the first two have a defensible universal value:
///
/// - `connect` — is anything LISTENING. A daemon that is not up fails here,
///   and it fails fast because nothing about the route changes the answer.
/// - `read` — has the host gone silent. It bounds the wait for the next
///   bytes and RESETS on progress, so a route that legitimately takes
///   minutes (a governance seed, an enrichment retry) survives it while a
///   host that died mid-answer does not.
/// - `total` — a hard ceiling on the whole exchange, `None` by default.
///   There is no honest universal value: this client's sixty-odd routes run
///   from a cached catalog read to a model-backed pass, so a number that
///   suits one truncates another. A CALLER that knows its own route names
///   one; the crate does not invent it (ARCH principle 6 — a truncated
///   answer reported as an error is fine, a guessed ceiling is not).
///
/// NOT the turn stream. `connect`/`connect_with` open a WebSocket through
/// `tokio_tungstenite`, which never touches this client, and a turn is
/// meant to stay open for as long as the host is generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestBudget {
    /// Bound on establishing the TCP/TLS connection.
    pub connect: Duration,
    /// Bound on the wait for the next bytes of an answer. Resets on
    /// progress, so it bounds SILENCE, not duration.
    pub read: Duration,
    /// Hard ceiling on the whole exchange. `None` leaves it bounded by
    /// `read` alone.
    pub total: Option<Duration>,
}

/// A host that is not listening answers this question immediately; the
/// budget only has to cover a loopback or LAN handshake.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Sixty seconds of NO bytes at all. Chosen against the slowest thing a
/// host does between writes on these routes — a model-backed pass that
/// emits nothing until it is done — not against total call duration, which
/// this bound deliberately does not constrain.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

impl Default for RequestBudget {
    fn default() -> Self {
        Self {
            connect: DEFAULT_CONNECT_TIMEOUT,
            read: DEFAULT_READ_TIMEOUT,
            total: None,
        }
    }
}

/// A connection to a serving host's turn surface.
#[derive(Debug, Clone)]
pub struct TurnClient {
    base: String,
    http: reqwest::Client,
    budget: RequestBudget,
}

/// How to open one conversation's turn stream (sv-surface R2 / G12).
///
/// Defaults are the pre-R2 behaviour byte-for-byte: an unclaimed plain
/// stream. Every field is opt-in, and each one carries an obligation the
/// doc names.
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    /// Claim this conversation's approvals on the upgrade
    /// (`?approvals=true`): the host puts its consent questions to this
    /// socket, as `TurnFrame::Prompt`, and expects
    /// [`TurnRequest::Answer`] back — [`TurnSender::send_answer`] is the
    /// spelling.
    ///
    /// Off by default, and the default is load-bearing: a reader that
    /// installs no answer handler (`svrn chat`, the bench harness, a
    /// one-shot drain) must keep running under the host's own
    /// non-interactive channel. Claiming for such a reader turns the
    /// host's auto-answer into a hang — the exact inversion C1 removed.
    pub claim_approvals: bool,
}

/// A conversation the host created and now owns a row for.
#[derive(Debug, Clone)]
pub struct CreatedConversation {
    /// The host-minted conversation id. The host mints it rather than the
    /// client so the row and the id come into existence together — see
    /// `turn_http::create_conversation`.
    pub id: String,
    /// Unix seconds the host recorded.
    pub created_at: i64,
    /// The corpus allow-list the host seeded, as it echoed it. `None` when
    /// none was requested.
    pub enabled_corpora: Option<Vec<String>>,
}

/// One row of the host's conversation list — `GET /v1/conversations`.
#[derive(Debug, Clone)]
pub struct ListedConversation {
    /// The id the client created the conversation with (the host strips any
    /// internal scoping before answering).
    pub id: String,
    /// Display title; `None` until the host sets one.
    pub title: Option<String>,
    /// Creation time (Unix seconds).
    pub created_at: i64,
    /// Last-append time (Unix seconds).
    pub updated_at: i64,
}

/// One message of a conversation's history, as `GET /v1/conversations/{id}`
/// serves it — content plus the projections the host persisted.
#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub id: String,
    /// Wire role string (`"user"` / `"assistant"` / `"system"`).
    pub role: String,
    pub content: String,
    pub created_at: i64,
    pub provenance: Option<Provenance>,
    pub citations: Vec<Citation>,
    pub epistemic_state: Option<EpistemicState>,
    /// The persisted metadata blob the three fields above are projected
    /// from, verbatim; `None` for a message the host stored without one.
    pub metadata: Option<serde_json::Value>,
}

/// One hit of `GET /v1/conversations/search` — a matching message and the
/// conversation it belongs to (sv-surface rung 6).
#[derive(Debug, Clone)]
pub struct SearchedMessage {
    pub content: String,
    pub conversation_id: String,
}

/// One clipped insight as `GET /v1/insights*` serves it — the wire
/// projection: every renderable field, embedding stripped, `created_at`
/// RFC 3339. Mirrors `sovereign_mesh::insight_http::InsightEntry` (the
/// envelope lives behind the route; the client parses the same bytes).
#[derive(Debug, Clone, Deserialize)]
pub struct InsightEntry {
    pub id: String,
    pub clipped_text: String,
    pub message_id: String,
    pub paragraph_index: usize,
    pub source: sovereign_contracts::types::InsightSource,
    pub position: Option<sovereign_contracts::types::InsightPosition>,
    pub adjacent: Vec<String>,
    pub created_at: String,
    pub sink_state: sovereign_contracts::types::InsightSinkState,
}

/// The answer to `GET /v1/insights/sinks` — the wire form of the
/// desktop's `get_sink_status` (sv-surface D2's one owed route).
///
/// Mirrored here rather than made generic, unlike the atlas and meshapp
/// families below: two fields over a three-field row is exactly the
/// `InsightEntry` case this crate already resolves by mirroring, and a
/// caller that had to name `sovereign_mesh::insight_http::
/// SinkStatusResponse` would be taking a dependency on the HOST to read
/// its own settings pane.
#[derive(Debug, Clone, Deserialize)]
pub struct SinkStatus {
    /// The registry's own fold. Read THIS, not `sinks.is_empty()` — a
    /// registry can hold a sink that is registered and not reachable.
    pub any_connected: bool,
    pub sinks: Vec<SinkInfo>,
}

/// One registered insight sink.
#[derive(Debug, Clone, Deserialize)]
pub struct SinkInfo {
    pub id: String,
    pub display_name: String,
    pub connected: bool,
}

/// Page size [`TurnClient::corpus_atoms_all`] asks for. The host caps
/// at `reading_http::ATOMS_PAGE_MAX` and says what it applied, so this
/// is a hint about round-trip count, never a correctness knob.
pub const ATOMS_PAGE_REQUEST: usize = 1_000;

/// One page of `GET /internal/corpus/{corpus}/atoms` — the wire form of
/// the desktop's `load_atoms` primitive (sv-surface D3).
///
/// `T` is `corpus_engine::enrichment::atlas::AtomEnvelope` for every
/// real caller; this crate cannot name that type (see the note above
/// [`TurnClient::corpus_atoms`]), so the caller supplies it.
///
/// Mirrors `sovereign_mesh::reading_http::CorpusAtomsPage`. `total` and
/// `next_offset` are the reason this is a struct rather than a bare
/// `Vec`: a client must be able to tell a finished read from a clipped
/// one without inferring it from a length (ARCH §18.3).
#[derive(Debug, Clone, Deserialize)]
pub struct AtomsPage<T> {
    pub corpus_id: String,
    pub schema_version: String,
    /// Atom count in the whole atlas, not in this page.
    pub total: usize,
    pub offset: usize,
    /// The limit the host ACTUALLY applied, after clamping.
    pub limit: usize,
    /// Offset to ask for next; `None` when the read is finished.
    #[serde(default)]
    pub next_offset: Option<usize>,
    pub atoms: Vec<T>,
}

/// The clip payload for [`TurnClient::clip_insight`].
pub struct ClipInsight<'a> {
    pub clipped_text: &'a str,
    pub message_id: &'a str,
    pub paragraph_index: usize,
    pub source: sovereign_contracts::types::InsightSource,
    pub position: Option<sovereign_contracts::types::InsightPosition>,
}

/// The tool-outcome payload for [`TurnClient::notes_tool_outcome`] — the
/// wire form of `sovereign_core::dossier::record_tool_outcome`'s args.
pub struct ToolOutcome<'a> {
    /// Per-conversation-turn opaque id — the approval `key` the surface
    /// minted, used as the session-id proxy for the audit trail.
    pub session_id: &'a str,
    pub conversation_id: Option<&'a str>,
    pub tool_id: &'a str,
    pub outcome: sovereign_contracts::types::ToolDecisionOutcome,
    pub reasoning: &'a str,
    pub extras_summary: Option<String>,
    pub evidence_ids: Vec<String>,
    pub turn_index: usize,
}

/// `GET /v1/conversations/{id}`'s answer — the conversation with its full
/// history.
#[derive(Debug, Clone)]
pub struct ConversationHistory {
    pub id: String,
    pub title: Option<String>,
    pub messages: Vec<ConversationMessage>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TurnClient {
    /// `base` is the host root, e.g. `http://127.0.0.1:9741` — no `/v1`.
    ///
    /// Carries [`RequestBudget::default`]; [`Self::with_budget`] is the
    /// spelling for a caller that knows its own route.
    pub fn new(base: impl Into<String>) -> Self {
        Self::with_budget(base, RequestBudget::default())
    }

    /// [`Self::new`] with the exchange bounds named.
    ///
    /// Panics only where `reqwest::Client::new` itself does — a TLS backend
    /// that will not initialise. That is reqwest's own documented contract
    /// for an infallible constructor, and it is a panic rather than a
    /// substitution: a client built without the budget it was asked for
    /// would be the success-shaped wrong answer (ARCH principle 6).
    pub fn with_budget(base: impl Into<String>, budget: RequestBudget) -> Self {
        let mut base = base.into();
        while base.ends_with('/') {
            base.pop();
        }
        let http = reqwest::Client::builder()
            .connect_timeout(budget.connect)
            .read_timeout(budget.read)
            .build()
            .expect("reqwest client with a request budget (TLS backend init)");
        Self { base, http, budget }
    }

    /// The bounds this client applies to every non-streaming exchange.
    pub fn budget(&self) -> RequestBudget {
        self.budget
    }

    /// `POST /v1/conversations` — seed the row before the first message.
    ///
    /// Seeding is what makes `skill_id` load-bearing: the host tags the
    /// conversation so its very first turn routes into that agent loop,
    /// rather than one untagged turn happening first. `enabled_corpora` is
    /// the retrieval allow-list (`Conversation::enabled_corpora`) for the
    /// same reason — it has to be on the row before the first turn reads
    /// it. `None` searches every installed corpus; the host validates a
    /// `Some` and refuses an unknown id with its installed list, which this
    /// returns verbatim as the error text.
    pub async fn create_conversation(
        &self,
        skill_id: Option<&str>,
        enabled_corpora: Option<&[String]>,
    ) -> Result<CreatedConversation> {
        let (url, ctx) = self.target("POST", "/v1/conversations");
        // A refusal arrives as the host's own words, not a generic status
        // line: a daemon that serves no turns says so ("this daemon serves
        // no turns (mesh-admin)") and that is the sentence the operator
        // needs. `host_words` is where that unwrapping lives now — this
        // method's private copy of it was the original (§10.6).
        let body = self
            .required(
                self.http
                    .post(&url)
                    .json(&create_conversation_body(skill_id, enabled_corpora)),
                &url,
                &ctx,
            )
            .await?;
        let v: serde_json::Value = parse(&body, &ctx)?;
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::Inference(format!("{ctx}: response carried no id")))?
            .to_string();
        let created_at = v.get("created_at").and_then(|x| x.as_i64()).unwrap_or(0);
        let echoed: Option<Vec<String>> = v
            .get("enabled_corpora")
            .and_then(|e| serde_json::from_value(e.clone()).ok());
        verify_allow_list_echo(enabled_corpora, echoed.as_deref())
            .map_err(|why| Error::Inference(format!("{ctx}: {why}")))?;
        Ok(CreatedConversation {
            id,
            created_at,
            enabled_corpora: echoed,
        })
    }

    /// `POST /v1/conversations/{id}/end` — run the conversation-end
    /// memory-extraction pass.
    ///
    /// A lifecycle call, not a turn. A REPL calls it when the user quits; a
    /// one-shot ask does not call it at all.
    pub async fn end_conversation(&self, conversation_id: &str) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::POST,
            format!("/v1/conversations/{conversation_id}/end"),
            None::<&()>,
        )
        .await
    }

    /// `GET /v1/conversations` — the host's conversation list.
    ///
    /// `limit`/`offset` pass through as query parameters; `None` takes the
    /// host's defaults (both sovereign-server and the daemon use 20 / 0).
    /// Both hosts answer the same envelope, so this is the one spelling a
    /// surface needs against either.
    pub async fn list_conversations(
        &self,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<ListedConversation>> {
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(offset) = offset {
            query.push(("offset", offset.to_string()));
        }
        self.listed_conversations(&query).await
    }

    /// `GET /v1/conversations?skill_id=` — the conversations belonging to
    /// ONE SURFACE, newest first.
    ///
    /// `None` is the DEFAULT surface (rows whose `skill_id IS NULL`), which
    /// is what a chat sidebar renders; `Some("inner-work")` is that
    /// surface. There is deliberately no "every surface" spelling here —
    /// [`Self::list_conversations`] is that, and keeping them apart is what
    /// stops a scoped caller widening its own visibility by passing a
    /// nullable id.
    pub async fn list_conversations_for_surface(
        &self,
        surface_skill_id: Option<&str>,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<ListedConversation>> {
        // Always SENT, empty for the default surface: absent would mean
        // "no scoping" and page everything (the route's own table).
        let mut query = vec![("skill_id", surface_skill_id.unwrap_or("").to_string())];
        Self::paged(&mut query, limit, offset);
        self.listed_conversations(&query).await
    }

    /// `GET /v1/conversations?corpus_id=` — a notebook's Ask-tab history:
    /// the default-surface conversations whose retrieval allow-list names
    /// this corpus. Everything-scoped conversations are excluded, so the
    /// tab shows only threads the user actually had while scoped to it.
    pub async fn list_conversations_for_corpus(
        &self,
        corpus_id: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<ListedConversation>> {
        let mut query = vec![("corpus_id", corpus_id.to_string())];
        Self::paged(&mut query, limit, offset);
        self.listed_conversations(&query).await
    }

    fn paged(query: &mut Vec<(&'static str, String)>, limit: Option<usize>, offset: Option<usize>) {
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(offset) = offset {
            query.push(("offset", offset.to_string()));
        }
    }

    /// The one GET + envelope fold the three listings share (§10.6).
    async fn listed_conversations(
        &self,
        query: &[(&str, String)],
    ) -> Result<Vec<ListedConversation>> {
        let wire: ConversationListWire = self
            .internal_get("/v1/conversations".to_string(), query)
            .await?;
        Ok(wire
            .conversations
            .into_iter()
            .map(|c| ListedConversation {
                id: c.id,
                title: c.title,
                created_at: c.created_at,
                updated_at: c.updated_at,
            })
            .collect())
    }

    /// `GET /v1/conversations/{id}` — one conversation with its full
    /// message history, each message already carrying the projections the
    /// host persisted (provenance, citations, epistemic ledger).
    ///
    /// A missing conversation surfaces as the host's error text (both hosts
    /// say `Conversation not found`), not a generic 404 line.
    pub async fn get_conversation(&self, conversation_id: &str) -> Result<ConversationHistory> {
        let wire: ConversationWire = self
            .internal_get(format!("/v1/conversations/{conversation_id}"), &[])
            .await?;
        Ok(ConversationHistory {
            id: wire.id,
            title: wire.title,
            messages: wire
                .messages
                .into_iter()
                .map(|m| ConversationMessage {
                    id: m.id,
                    role: m.role,
                    content: m.content,
                    created_at: m.created_at,
                    provenance: m.provenance,
                    citations: m.citations,
                    epistemic_state: m.epistemic_state,
                    metadata: m.metadata,
                })
                .collect(),
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        })
    }

    /// `DELETE /v1/conversations/{id}` — 204 on success, the host's error
    /// words otherwise. Idempotent from the caller's point of view: the row's
    /// absence afterward is observable via [`Self::get_conversation`]'s
    /// "not found".
    pub async fn delete_conversation(&self, conversation_id: &str) -> Result<()> {
        self.internal_delete(format!("/v1/conversations/{conversation_id}"))
            .await
    }

    /// `PATCH /v1/conversations/{id}` — rename a conversation.
    ///
    /// The host trims and clamps the title (200 characters) and refuses an
    /// empty one in its own words, so a surface offering a rename box
    /// carries no copy of that rule. 404 when the row is gone.
    pub async fn rename_conversation(&self, conversation_id: &str, title: &str) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::PATCH,
            format!("/v1/conversations/{conversation_id}"),
            Some(&serde_json::json!({ "title": title })),
        )
        .await
    }

    /// `PUT /v1/conversations/{id}/enabled-corpora` — replace the
    /// per-conversation retrieval allow-list.
    ///
    /// `None` clears it, which the column reads as "search every installed
    /// corpus". A `Some` is validated against what this host can actually
    /// search: an unknown id comes back as the host's refusal naming the
    /// installed list, and an empty list is refused rather than stored as
    /// "search nothing". That validation is the reason this is a route and
    /// not a store write — the desktop wrote the column locally until
    /// sv-surface, and in attach mode that is not the row the turn reads.
    pub async fn set_enabled_corpora(
        &self,
        conversation_id: &str,
        enabled_corpora: Option<&[String]>,
    ) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::PUT,
            format!("/v1/conversations/{conversation_id}/enabled-corpora"),
            Some(&serde_json::json!({ "enabled_corpora": enabled_corpora })),
        )
        .await
    }

    /// `POST /v1/conversations/{id}/messages` — the one-shot REST turn.
    ///
    /// The same driver the stream runs, collected: the reply arrives whole
    /// rather than as frames. Returns the same [`TurnOutcome`] value
    /// [`Self::run_turn`] does — a caller can mix transports (stream the long
    /// turns, one-shot the short ones) and hold one result type. The REST
    /// envelope carries no `metadata` block, so `TurnOutcome::metadata` is
    /// `None` here by construction, never "the turn had none".
    pub async fn send_message(&self, conversation_id: &str, content: &str) -> Result<TurnOutcome> {
        let wire: MessageResponseWire = self
            .internal_post_json(
                format!("/v1/conversations/{conversation_id}/messages"),
                &serde_json::json!({ "content": content }),
            )
            .await?;
        Ok(TurnOutcome {
            message_id: wire.message_id,
            text: wire.content,
            provenance: wire.provenance,
            citations: wire.citations,
            epistemic_state: wire.epistemic_state,
            task: wire.task,
            metadata: None,
        })
    }

    /// `GET /v1/conversations/search?q=` — full-text message search across
    /// conversations (sv-surface rung 6). The host's `search_messages`
    /// decider, capped at 50 the way the route caps — the cap has one home,
    /// server-side.
    pub async fn search_conversations(&self, query: &str) -> Result<Vec<SearchedMessage>> {
        let wire: SearchResponseWire = self
            .internal_get(
                "/v1/conversations/search".to_string(),
                &[("q", query.to_string())],
            )
            .await?;
        Ok(wire
            .results
            .into_iter()
            .map(|r| SearchedMessage {
                content: r.content,
                conversation_id: r.conversation_id,
            })
            .collect())
    }

    /// `DELETE /v1/memories/{id}` — tombstone a memory (soft delete; the
    /// row is preserved for audit and excluded from recall). 204 on
    /// success; a missing row surfaces as the host's error words.
    pub async fn delete_memory(&self, memory_id: &str) -> Result<()> {
        self.internal_delete(format!("/v1/memories/{memory_id}"))
            .await
    }

    /// `POST /v1/memories/{id}/weaken` — halve a memory's confidence with
    /// the standard decay floor. THE one decider for the halving lives in
    /// the host; this returns the new confidence so a caller can render it.
    pub async fn weaken_memory(&self, memory_id: &str) -> Result<f64> {
        let wire: WeakenResponseWire = self
            .internal_post_bare(format!("/v1/memories/{memory_id}/weaken"))
            .await?;
        Ok(wire.confidence)
    }

    /// `GET /v1/skills` — every skill the SERVING runtime registered,
    /// each flagged with whether that runtime has it active
    /// (sv-surface D9).
    ///
    /// `T` is the caller's own row type — `sovereign-desktop`'s
    /// `commands::SkillEntry`. Generic rather than mirrored (the
    /// `corpus_atoms` convention, not the `InsightEntry` one) because a
    /// mirror here would be a SECOND spelling of a row the surface
    /// already declares for its frontend bridge, and this crate exists
    /// to delete those.
    ///
    /// The envelope (`{"skills": [...]}`) is unwrapped here so the
    /// caller names one type, not two.
    pub async fn list_skills<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            skills: Vec<T>,
        }
        let wire: Wire<T> = self.internal_get("/v1/skills".to_string(), &[]).await?;
        Ok(wire.skills)
    }

    /// `GET /v1/conversations/{id}/provenance` — the most recent
    /// witness-turn provenance frame the SERVING runtime captured for
    /// this conversation, or `None` when it has run no witness turn
    /// (sv-surface D9).
    ///
    /// `T` is `sovereign_core::runtime::TurnProvenance`. Generic for
    /// the `list_skills` reason plus a harder one: that struct is
    /// twenty-odd fields over five nested types, and a mirror of it in
    /// this crate would be the largest twin the campaign has minted.
    ///
    /// `Ok(None)` is a SUCCESS — "no witness turn yet" is an answer the
    /// inner-work pane renders, not an absence to report as an error
    /// (ARCH §18.3).
    pub async fn last_turn_provenance<T: serde::de::DeserializeOwned>(
        &self,
        conversation_id: &str,
    ) -> Result<Option<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            provenance: Option<T>,
        }
        let wire: Wire<T> = self
            .internal_get(
                format!("/v1/conversations/{conversation_id}/provenance"),
                &[],
            )
            .await?;
        Ok(wire.provenance)
    }

    /// `GET /v1/insights?limit=` — the insight collection, newest first.
    pub async fn list_insights(&self, limit: Option<usize>) -> Result<Vec<InsightEntry>> {
        let wire: InsightListWire = self
            .internal_get("/v1/insights".to_string(), &limit_query(limit))
            .await?;
        Ok(wire.insights)
    }

    /// `GET /v1/insights/sinks` — every registered insight sink and
    /// whether it is reachable right now.
    ///
    /// An empty `sinks` with `any_connected: false` is a real answer —
    /// nobody configured a vault on this daemon. A daemon with no
    /// insight service at all answers 503, which arrives here as an
    /// `Err` naming that reason, so the two stay different facts.
    pub async fn insight_sinks(&self) -> Result<SinkStatus> {
        self.internal_get("/v1/insights/sinks".to_string(), &[])
            .await
    }

    /// `GET /v1/insights/search?q=` — full-text search over the collection.
    pub async fn search_insights(&self, query: &str) -> Result<Vec<InsightEntry>> {
        let wire: InsightListWire = self
            .internal_get(
                "/v1/insights/search".to_string(),
                &[("q", query.to_string())],
            )
            .await?;
        Ok(wire.insights)
    }

    /// `DELETE /v1/insights/{id}` — soft-delete a clip. 204 on success.
    pub async fn delete_insight(&self, insight_id: &str) -> Result<()> {
        self.internal_delete(format!("/v1/insights/{insight_id}"))
            .await
    }

    /// `POST /v1/insights/clip` — clip a passage. The host embeds, finds
    /// adjacent nodes, persists, and answers the created row (with the
    /// embedding stripped — the wire projection).
    pub async fn clip_insight(&self, clip: ClipInsight<'_>) -> Result<InsightEntry> {
        let wire: ClipResponseWire = self
            .internal_post_json(
                "/v1/insights/clip".to_string(),
                &serde_json::json!({
                    "clipped_text": clip.clipped_text,
                    "message_id": clip.message_id,
                    "paragraph_index": clip.paragraph_index,
                    "source": clip.source,
                    "position": clip.position,
                }),
            )
            .await?;
        Ok(wire.insight)
    }

    /// `POST /v1/notes/tool-outcome` — record a tool-decision outcome into
    /// the host's notes dossier. Fire-and-soft-fail by DESIGN at the call
    /// site (the in-process path skipped a missing NoteStore silently);
    /// this method reports the host's answer so the caller decides what is
    /// fatal, never the wire (§18.3).
    pub async fn notes_tool_outcome(&self, outcome: ToolOutcome<'_>) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::POST,
            "/v1/notes/tool-outcome".to_string(),
            Some(&serde_json::json!({
                "session_id": outcome.session_id,
                "conversation_id": outcome.conversation_id,
                "tool_id": outcome.tool_id,
                "outcome": outcome.outcome,
                "reasoning": outcome.reasoning,
                "extras": {
                    "summary": outcome.extras_summary,
                    "evidence_ids": outcome.evidence_ids,
                    "turn_index": outcome.turn_index,
                },
            })),
        )
        .await
    }

    // ── The reading + atlas surfaces (sv-surface D3 / D4) ─────────
    //
    // These answer with types owned by `corpus-engine-vocab`
    // (`AtomEnvelope`) and `sovereign-tools` (`atlas_view::*`), both of
    // which sit ABOVE this crate's Tier-0 dependency budget
    // (`quality/ARCH_LAYERS.toml:103-111` — "its only non-leaf
    // dependency is `sovereign-contracts`"). Two ways out of that, and
    // only one is honest: mirror each type here as a Deserialize twin —
    // the `InsightEntry` pattern, which is fine for nine fields and an
    // ARCH §10.6 disaster for `AtomEnvelope`'s twelve variants over
    // eleven structs — or let the CALLER name the type it wants and
    // parse into it.
    //
    // The caller names it. Every method below is generic over its
    // response, and its doc comment names the exact type the bytes ARE,
    // so the desktop writes `client.corpus_atoms::<AtomEnvelope>(…)` and
    // gets back the same `Vec<AtomEnvelope>` its in-process
    // `load_atoms` returned. No twin, no layer edge, and the repoint is
    // a call-site change rather than a rewrite.

    /// `GET /internal/corpus/{corpus}/atoms?offset=&limit=` — one page
    /// of a corpus's atlas atoms.
    ///
    /// The wire form of the desktop's `commands::meshapp::load_atoms`
    /// primitive — the read behind `meshapp_read_corpus`,
    /// `meshapp_search_parcels` and `meshapp_parcel_analytics`. `T` is
    /// `corpus_engine::enrichment::atlas::AtomEnvelope`.
    ///
    /// `limit` is a REQUEST, not a guarantee: the host clamps it to
    /// `reading_http::ATOMS_PAGE_MAX` and reports what it actually
    /// applied in [`AtomsPage::limit`]. Read [`AtomsPage::next_offset`],
    /// never `atoms.len()`, to decide whether the read is finished —
    /// or call [`Self::corpus_atoms_all`], which does that for you.
    pub async fn corpus_atoms<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<AtomsPage<T>> {
        self.internal_get(
            format!("/internal/corpus/{corpus_id}/atoms"),
            &[("offset", offset.to_string()), ("limit", limit.to_string())],
        )
        .await
    }

    /// [`Self::corpus_atoms_all`] for a caller to whom "this corpus has
    /// no atlas" is an ANSWER, not a failure.
    ///
    /// The host answers 404 for a corpus that is not installed OR whose
    /// `atlas/` dir is absent, and says so in those words
    /// (`reading_http.rs:427`). For the starter-questions surface those
    /// are one fact — there are no atom-derived starters, fall back to
    /// excerpts — and it is the same fact the in-process caller read off
    /// `atlas_dir.exists()` before this route existed.
    ///
    /// `Ok(None)` is ONLY that 404, and only on the FIRST page. A 404
    /// part-way through the paging loop means the atlas vanished under
    /// the read and stays an `Err`: it is a different event, and
    /// collapsing the two would hand the caller a short list that looks
    /// complete (ARCH principle 6). Every other status is still the
    /// host's own words.
    pub async fn corpus_atoms_all_if_present<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Option<Vec<T>>> {
        let first: AtomsPage<T> = match self
            .internal_get_opt(
                format!("/internal/corpus/{corpus_id}/atoms"),
                &[
                    ("offset", "0".to_string()),
                    ("limit", ATOMS_PAGE_REQUEST.to_string()),
                ],
            )
            .await?
        {
            Some(page) => page,
            None => return Ok(None),
        };

        let mut out: Vec<T> = first.atoms;
        let mut offset = 0usize;
        let mut next = first.next_offset;
        loop {
            let Some(n) = next else { return Ok(Some(out)) };
            if n <= offset {
                return Err(Error::Inference(format!(
                    "GET /internal/corpus/{corpus_id}/atoms: host advertised \
                     next_offset={n} at offset {offset} — the page would not advance"
                )));
            }
            offset = n;
            let page = self
                .corpus_atoms::<T>(corpus_id, offset, ATOMS_PAGE_REQUEST)
                .await?;
            if page.atoms.is_empty() && page.next_offset.is_some() {
                return Err(Error::Inference(format!(
                    "GET /internal/corpus/{corpus_id}/atoms: host returned 0 rows at \
                     offset {offset} while still advertising a next offset"
                )));
            }
            out.extend(page.atoms);
            next = page.next_offset;
        }
    }

    /// Every atom in a corpus's atlas, paged until the host says the
    /// read is finished — the drop-in replacement for the desktop's
    /// in-process `load_atoms`, which returns the whole vec because it
    /// never crossed a socket.
    ///
    /// Pages at the host's default size. The loop terminates on
    /// `next_offset: None`, and defends against a host that would
    /// otherwise spin it: a page that returns zero rows while still
    /// advertising a next offset is a broken host, and this reports
    /// that rather than looping forever (ARCH §18.3 — the absence is
    /// named, not defaulted to "done").
    pub async fn corpus_atoms_all<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<T>> {
        let mut out: Vec<T> = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self
                .corpus_atoms::<T>(corpus_id, offset, ATOMS_PAGE_REQUEST)
                .await?;
            let got = page.atoms.len();
            out.extend(page.atoms);
            match page.next_offset {
                None => return Ok(out),
                Some(next) if got > 0 && next > offset => offset = next,
                Some(next) => {
                    return Err(Error::Inference(format!(
                        "GET /internal/corpus/{corpus_id}/atoms: host advertised \
                         next_offset={next} after returning {got} rows at offset \
                         {offset} — the page would not advance"
                    )))
                }
            }
        }
    }

    /// `GET /internal/atlas/corpora` — every installed corpus that has
    /// an atlas, with per-atom-type counts.
    ///
    /// `T` is `Vec<sovereign_tools::atlas_view::AtlasCorpusSummary>`.
    pub async fn atlas_corpora<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        self.internal_get("/internal/atlas/corpora".to_string(), &[])
            .await
    }

    /// `GET /internal/atlas/{corpus}/report` — what the last build
    /// found. `T` is `sovereign_tools::atlas_view::AtlasBuildReport`.
    ///
    /// A corpus whose report step never ran answers `reported: false`,
    /// which is a SUCCESS, not an error.
    pub async fn atlas_build_report<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/atlas/{corpus_id}/report"), &[])
            .await
    }

    /// `GET /internal/atlas/{corpus}/members` — the member atlases of a
    /// collection corpus. `T` is
    /// `Vec<sovereign_tools::atlas_view::AtlasMemberSummary>`.
    ///
    /// An empty list is the right answer for an ordinary corpus.
    pub async fn atlas_members<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/atlas/{corpus_id}/members"), &[])
            .await
    }

    /// `POST /internal/atlas/{corpus}/atoms` — filterable, paginated
    /// atom browse. `T` is `sovereign_tools::atlas_view::AtomListPage`.
    ///
    /// `request` serialises to `{"filter": …, "page": …}` — the two
    /// arguments `FileAtlasReader::list_atoms` takes; either key may be
    /// omitted for that type's `Default`. A POST because `AtomFilter`
    /// carries a list of subtype names, which no flat query string
    /// expresses without inventing a second encoding.
    pub async fn atlas_atoms<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        request: &B,
    ) -> Result<T> {
        self.internal_post_json(format!("/internal/atlas/{corpus_id}/atoms"), request)
            .await
    }

    /// `GET /internal/atlas/{corpus}/subgraph?max_nodes=` — the curated
    /// landscape map. `T` is
    /// `sovereign_tools::atlas_view::AtlasSubgraph`.
    ///
    /// `max_nodes: None` leaves the cap to the host, which applies
    /// `atlas_view::DEFAULT_MAX_NODES` — one decider for that number,
    /// and it is not this crate's.
    pub async fn atlas_subgraph<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        max_nodes: Option<usize>,
    ) -> Result<T> {
        let query: Vec<(&str, String)> = max_nodes
            .into_iter()
            .map(|n| ("max_nodes", n.to_string()))
            .collect();
        self.internal_get(format!("/internal/atlas/{corpus_id}/subgraph"), &query)
            .await
    }

    /// `GET /internal/atlas/{corpus}/atoms/{atom_id}` — the full
    /// inspector record. `T` is
    /// `sovereign_tools::atlas_view::AtomDetail`.
    ///
    /// `Ok(None)` for a 404, which is the host saying the atom is not in
    /// this corpus's atoms.json — a stale UI link, or extraction
    /// renumbered ids. Every OTHER non-success is an `Err`: a corpus
    /// that will not open must not read as "atom absent", which is the
    /// exact confusion `daemon_reading_get` shipped by mapping ANY 404
    /// to `Ok(None)` (see `reading_http_e2e`'s unpromoted-partition
    /// case).
    pub async fn atlas_atom_detail<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        atom_id: &str,
    ) -> Result<Option<T>> {
        self.internal_get_opt(format!("/internal/atlas/{corpus_id}/atoms/{atom_id}"), &[])
            .await
    }

    // ── The conversation-tiered browse (sv-surface D4 remainder) ──
    //
    // Six methods for `atlas_http`'s six conv routes. Generic for the
    // family reason: this crate is Tier-0 and cannot name
    // `atlas_view::ConvListPage` or `conv_tiered::EntityAggregateRow`
    // without taking a dependency on `sovereign-tools` and
    // `sovereign-core`. The caller names its type; each doc says
    // exactly which one.
    //
    // A 503 here means the daemon wired no conversation-tiered reader
    // and a 501 means the reader declines that method by name. Both
    // arrive as `Err` carrying the host's words — neither is an empty
    // page (§18.3), which is what the pane would otherwise render as
    // "you have no conversations".

    /// `GET /internal/atlas/conv/corpora` — every corpus with at least
    /// one conversation, with its state buckets. `T` is
    /// `sovereign_tools::atlas_view::ConvCorpusSummary`.
    pub async fn conv_corpora<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.internal_get("/internal/atlas/conv/corpora".to_string(), &[])
            .await
    }

    /// `GET /internal/atlas/conv/{corpus}/conversations?filter=&offset=`
    /// — one page of conversations, newest first. `T` is
    /// `sovereign_tools::atlas_view::ConvListPage`.
    ///
    /// The page SIZE is the host's (200) and is not a parameter here —
    /// one decider, and it is not this crate's. Follow
    /// `next_offset` until it is `null`; the key is always present, so
    /// an old host cannot read as "the end".
    pub async fn conv_conversations<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        filter: Option<&str>,
        offset: Option<u64>,
    ) -> Result<T> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(f) = filter {
            query.push(("filter", f.to_string()));
        }
        if let Some(o) = offset {
            query.push(("offset", o.to_string()));
        }
        self.internal_get(
            format!("/internal/atlas/conv/{corpus_id}/conversations"),
            &query,
        )
        .await
    }

    /// `GET /internal/atlas/conv/{corpus}/conversations/{conv_uuid}` —
    /// the RAPTOR tree and any active summary correction. `T` is
    /// `sovereign_tools::atlas_view::ConvDetailView`.
    ///
    /// `Ok(None)` is the 404 and ONLY the 404: a conversation the store
    /// does not have. A daemon with no reader is a 503 and stays an
    /// `Err` — the two must not collapse into the same answer.
    pub async fn conv_detail<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Option<T>> {
        self.internal_get_opt(
            format!("/internal/atlas/conv/{corpus_id}/conversations/{conv_uuid}"),
            &[],
        )
        .await
    }

    /// `GET …/conversations/{conv_uuid}/entities` — the salience-ranked
    /// chip row. `T` is `sovereign_tools::atlas_view::ConvEntityChip`.
    pub async fn conv_entities<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Vec<T>> {
        self.internal_get(
            format!("/internal/atlas/conv/{corpus_id}/conversations/{conv_uuid}/entities"),
            &[],
        )
        .await
    }

    /// `GET /internal/atlas/conv/{corpus}/entities/aggregate?text=` —
    /// one entity's roll-up across the corpus. `T` is
    /// `sovereign_core::conv_tiered::EntityAggregateRow`.
    ///
    /// The two drawer caps (20 co-occurring, 10 conversations) are the
    /// host's, for the `conv_conversations` reason.
    pub async fn conv_entity_aggregate<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        text: &str,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/atlas/conv/{corpus_id}/entities/aggregate"),
            &[("text", text.to_string())],
        )
        .await
    }

    /// `GET /internal/atlas/conv/{corpus}/chunk-entity-progress` — how
    /// far chunk-level entity extraction has got. `T` is
    /// `sovereign_core::conv_tiered::ChunkEntityProgressRow`.
    ///
    /// `Ok(None)` here is an explicit `null` BODY on a 200 — the corpus
    /// exists and extraction never ran. It is not a 404, and a 404 from
    /// this route is an `Err` (there is no such corpus-shaped absence
    /// to report), so "never extracted" cannot arrive as "no route".
    pub async fn conv_chunk_entity_progress<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Option<T>> {
        self.internal_get(
            format!("/internal/atlas/conv/{corpus_id}/chunk-entity-progress"),
            &[],
        )
        .await
    }

    // ── The recipe-author project store (sv-surface D6) ──────────
    //
    // Answers are `sovereign_mesh::features_http::ProjectEntry`.
    // Generic over `T` for the family reason; the store's whole surface
    // is three methods, so these three routes are all of it.

    /// `GET /v1/features/projects?include_archived=` — every
    /// recipe-author project, newest-updated first. `T` is
    /// `sovereign_mesh::features_http::ProjectEntry`.
    ///
    /// An empty vec is a real answer — a fresh install has no projects,
    /// and the Welcome pane branches on exactly that.
    pub async fn feature_projects<T: serde::de::DeserializeOwned>(
        &self,
        include_archived: bool,
    ) -> Result<Vec<T>> {
        let wire: ProjectListWire<T> = self
            .internal_get(
                "/v1/features/projects".to_string(),
                &[("include_archived", include_archived.to_string())],
            )
            .await?;
        Ok(wire.projects)
    }

    /// `GET /v1/features/projects/{id}` — one project. `T` is
    /// `sovereign_mesh::features_http::ProjectEntry`.
    ///
    /// `Ok(None)` for a 404, safe here for [`Self::note_get`]'s reason:
    /// this path has one 404 and it means "no such project". A daemon
    /// whose `features.db` never opened answers 503.
    pub async fn feature_project<T: serde::de::DeserializeOwned>(
        &self,
        id: &str,
    ) -> Result<Option<T>> {
        self.internal_get_opt(format!("/v1/features/projects/{id}"), &[])
            .await
    }

    /// `POST /v1/features/projects` — provision a project; answers the
    /// row that was written. `T` is
    /// `sovereign_mesh::features_http::ProjectEntry`.
    ///
    /// `body` serialises to `{id, title, charter_md}`. The ID IS THE
    /// CALLER'S: `RecipeProject` mints it from the project's essence
    /// before provisioning, and the host does not invent one (ARCH
    /// §7.5). A duplicate id comes back as a 409 and an empty one as a
    /// 400 — both `Err`, both naming which.
    pub async fn feature_project_create<
        B: serde::Serialize + ?Sized,
        T: serde::de::DeserializeOwned,
    >(
        &self,
        body: &B,
    ) -> Result<T> {
        self.internal_post_json("/v1/features/projects".to_string(), body)
            .await
    }

    // ── Notes CRUD (sv-surface D6) ───────────────────────────────
    //
    // The answers are `sovereign_contracts::daemon_wire::NoteEntry`, a
    // twenty-field projection of `corpus_engine_notes::Note`. Generic
    // over `T` for the same reason the two families below are: a twin
    // here would be twenty fields this crate would have to keep in step
    // with a store schema it cannot see change (ARCH §10.6). The
    // no-payload writes return plain `bool`/`String`, which need no
    // type from anywhere.

    /// `POST /v1/notes/query` — filtered notes, newest first when
    /// `query` is absent. `T` is `sovereign_contracts::daemon_wire::NoteEntry`.
    ///
    /// A POST because the filter carries three LISTS (`symbols`,
    /// `files`, `kinds`) and no flat query string expresses those
    /// without a second encoding — the call `atlas_atoms` already makes.
    ///
    /// An empty vec is a real answer for every filter, never an error.
    /// `include_retired` must be `true` to see struck-through rows —
    /// the lesson pane passes `true` because it renders whole supersede
    /// chains.
    pub async fn notes_list<T: serde::de::DeserializeOwned>(
        &self,
        query: Option<&str>,
        kinds: &[&str],
        limit: Option<usize>,
        include_retired: bool,
    ) -> Result<Vec<T>> {
        let wire: NoteListWire<T> = self
            .internal_post_json(
                "/v1/notes/query".to_string(),
                &serde_json::json!({
                    "query": query,
                    "kinds": kinds,
                    "limit": limit,
                    "include_retired": include_retired,
                }),
            )
            .await?;
        Ok(wire.notes)
    }

    /// `GET /v1/notes/{id}` — one note. `T` is
    /// `sovereign_contracts::daemon_wire::NoteEntry`.
    ///
    /// `Ok(None)` for a 404, and that mapping is safe HERE where it is
    /// not on the meshapp routes: this path has exactly one 404 — "no
    /// note with that id". A daemon with no note store answers 503 and
    /// every read failure answers 500, so an absent store cannot arrive
    /// as an absent note.
    pub async fn note_get<T: serde::de::DeserializeOwned>(&self, id: &str) -> Result<Option<T>> {
        self.internal_get_opt(format!("/v1/notes/{id}"), &[]).await
    }

    /// `POST /v1/notes` — write one note; answers the id the store
    /// minted.
    ///
    /// `body` serialises to `sovereign_mesh::notes_http::
    /// CreateNoteRequest`: `kind`, `content`, `session_id`, `scope` and
    /// `source` are required, the rest default. `scope` and `source`
    /// are the enums' own strings and an unrecognised one is a 400 — the
    /// host will not guess `global`/`agent` for you, because a wrong
    /// guess on `scope` is what puts a node-local note on the mesh.
    pub async fn note_create<B: serde::Serialize + ?Sized>(&self, body: &B) -> Result<String> {
        let wire: CreatedIdWire = self
            .internal_post_json("/v1/notes".to_string(), body)
            .await?;
        Ok(wire.id)
    }

    /// `PATCH /v1/notes/{id}/payload` — replace the opaque structured
    /// payload. `false` means no such note; the payload's schema is the
    /// caller's and crosses as a string.
    pub async fn note_set_payload(&self, id: &str, payload_json: &str) -> Result<bool> {
        self.note_affecting(
            reqwest::Method::PATCH,
            format!("/v1/notes/{id}/payload"),
            &serde_json::json!({ "payload_json": payload_json }),
        )
        .await
    }

    /// `POST /v1/notes/{id}/retire` — strike a note through, KEEPING the
    /// row so a successor can point back at it. `false` means no such
    /// note. This is not [`Self::note_delete`].
    pub async fn note_retire(&self, id: &str, reason: &str) -> Result<bool> {
        self.note_affecting(
            reqwest::Method::POST,
            format!("/v1/notes/{id}/retire"),
            &serde_json::json!({ "reason": reason }),
        )
        .await
    }

    /// `DELETE /v1/notes/{id}` — real deletion, no tombstone. `false`
    /// means the row was not there, which is an answer and not an error.
    pub async fn note_delete(&self, id: &str) -> Result<bool> {
        let wire: AffectedWire = self.internal_delete_json(format!("/v1/notes/{id}")).await?;
        Ok(wire.existed)
    }

    /// The two body-carrying writes that answer `{existed}` differ only
    /// in method, path and body; spelled once.
    async fn note_affecting(
        &self,
        method: reqwest::Method,
        path: String,
        body: &serde_json::Value,
    ) -> Result<bool> {
        let (url, ctx) = self.target(method.as_str(), &path);
        let answer = self
            .required(self.http.request(method, &url).json(body), &url, &ctx)
            .await?;
        let wire: AffectedWire = parse(&answer, &ctx)?;
        Ok(wire.existed)
    }

    // ── The MeshApp explorer surface (sv-surface D3) ─────────────
    //
    // Thirteen reads whose answers are `sovereign_meshapp::*` DTOs —
    // a `capabilities`-layer crate this Tier-0 client may not name, for
    // the reason spelled out above the atlas block. Same resolution:
    // the CALLER names the type, and each doc comment below names the
    // exact one, so the desktop writes
    // `client.meshapp_graph::<Vec<GraphNodeDto>>(...)` and gets back
    // what its in-process `sovereign_meshapp::graph_nodes` returned.
    //
    // None of these maps a 404 to `Ok(None)`. A meshapp 404 means one
    // of three different things — the corpus is not installed, it has
    // no graph to explore, or the id asked for is not in it — and the
    // desktop commands these replace reported all three as one `Err`
    // carrying the host's words. `host_words` keeps the status AND the
    // reason in the message, so that parity holds without inventing an
    // absence the caller cannot distinguish (the trap
    // [`Self::atlas_atom_detail`] documents).

    /// `GET /internal/meshapp/{corpus}/graph?node_type=&limit=` —
    /// degree-ranked entities, highest first. The wire form of
    /// `meshapp_graph`. `T` is `Vec<sovereign_meshapp::GraphNodeDto>`.
    ///
    /// `limit` is a REQUEST: the host applies its own default (50) when
    /// absent and clamps to its maximum (500). The clamp lives there so
    /// there is one of it.
    pub async fn meshapp_graph<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        node_type: Option<&str>,
        limit: Option<usize>,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/meshapp/{corpus_id}/graph"),
            &node_list_query(node_type, limit),
        )
        .await
    }

    /// `GET /internal/meshapp/{corpus}/nodes/{id}` — one entity plus
    /// every incident edge, each quoting its evidence. The wire form of
    /// `meshapp_node`. `T` is `sovereign_meshapp::NodeDetailDto`.
    pub async fn meshapp_node<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/meshapp/{corpus_id}/nodes/{id}"), &[])
            .await
    }

    /// `GET /internal/meshapp/{corpus}/chunks/{chunk_id}` — one chunk's
    /// title and content by numeric id. `T` is
    /// `sovereign_meshapp::ChunkDto`. Added 2026-09-11 so the desktop's
    /// focused-passage preamble (`commands/chat.rs`) reads the chunk from
    /// the daemon instead of opening the index with its own engine.
    pub async fn meshapp_chunk<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        chunk_id: u64,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/meshapp/{corpus_id}/chunks/{chunk_id}"),
            &[],
        )
        .await
    }

    /// `GET /internal/meshapp/{corpus}/findings?pattern=` — the wire
    /// form of `meshapp_findings`. `T` is
    /// `Vec<sovereign_meshapp::FindingDto>`.
    ///
    /// An empty list is a legitimate answer for a corpus whose graph
    /// carries no findings; it is never a 404.
    pub async fn meshapp_findings<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        pattern: Option<&str>,
    ) -> Result<T> {
        let query: Vec<(&str, String)> = pattern
            .into_iter()
            .map(|p| ("pattern", p.to_string()))
            .collect();
        self.internal_get(format!("/internal/meshapp/{corpus_id}/findings"), &query)
            .await
    }

    /// `GET /internal/meshapp/{corpus}/entities?q=&node_type=&limit=` —
    /// case-folded substring search over name, aliases and attributes.
    /// The wire form of `meshapp_search_entities`. `T` is
    /// `Vec<sovereign_meshapp::GraphNodeDto>`.
    ///
    /// A blank `query` answers `[]` at the host WITHOUT loading the
    /// graph — the command's own short-circuit, kept, so a cleared
    /// search box costs one round-trip and no disk.
    pub async fn meshapp_search_entities<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        query: &str,
        node_type: Option<&str>,
        limit: Option<usize>,
    ) -> Result<T> {
        let mut params = vec![("q", query.to_string())];
        params.extend(node_list_query(node_type, limit));
        self.internal_get(format!("/internal/meshapp/{corpus_id}/entities"), &params)
            .await
    }

    /// `GET /internal/meshapp/{corpus}/claims?limit=` — Claim atoms with
    /// attribution and cited evidence. The wire form of
    /// `meshapp_claims`. `T` is `Vec<sovereign_meshapp::ClaimDto>`.
    pub async fn meshapp_claims<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        limit: Option<usize>,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/meshapp/{corpus_id}/claims"),
            &limit_query(limit),
        )
        .await
    }

    /// `GET /internal/meshapp/{corpus}/questions?limit=` — Question
    /// atoms, the open inquiries a corpus raises. The wire form of
    /// `meshapp_questions`. `T` is
    /// `Vec<sovereign_meshapp::QuestionDto>`.
    pub async fn meshapp_questions<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        limit: Option<usize>,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/meshapp/{corpus_id}/questions"),
            &limit_query(limit),
        )
        .await
    }

    /// `GET /internal/meshapp/{corpus}/reconciliation` — the atlas's
    /// cross-origin identity merges, richest first. The wire form of
    /// `meshapp_reconciliation`. `T` is
    /// `Vec<sovereign_meshapp::ReconciliationMergeDto>`.
    pub async fn meshapp_reconciliation<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/meshapp/{corpus_id}/reconciliation"), &[])
            .await
    }

    /// `GET /internal/meshapp/{corpus}/subgraph?node_type=&limit=` —
    /// top-degree nodes plus induced edges, for a node-link map. The
    /// wire form of `meshapp_subgraph`. `T` is
    /// `sovereign_meshapp::SubgraphDto`.
    ///
    /// NOT [`Self::atlas_subgraph`], which is `atlas_view`'s curated
    /// landscape map over a different projection and a different shape.
    pub async fn meshapp_subgraph<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        node_type: Option<&str>,
        limit: Option<usize>,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/meshapp/{corpus_id}/subgraph"),
            &node_list_query(node_type, limit),
        )
        .await
    }

    /// `GET /internal/meshapp/{corpus}/stats` — headline scale and
    /// provenance counts for a banner. The wire form of
    /// `meshapp_corpus_stats`. `T` is
    /// `sovereign_meshapp::CorpusStatsDto`.
    pub async fn meshapp_corpus_stats<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/meshapp/{corpus_id}/stats"), &[])
            .await
    }

    /// `GET /internal/meshapp/{corpus}/timeline` — documents bucketed
    /// by month. The wire form of `meshapp_timeline`. `T` is
    /// `sovereign_meshapp::TimelineDto`.
    pub async fn meshapp_timeline<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/meshapp/{corpus_id}/timeline"), &[])
            .await
    }

    /// `GET /internal/meshapp/{corpus}/chunks/{chunk_id}` — one chunk's
    /// full text by the NUMERIC id an edge carries. The wire form of
    /// `meshapp_read_chunk`. `T` is `sovereign_meshapp::ChunkDto`.
    ///
    /// Takes a `u64` rather than the command's `String`: the command
    /// parsed it and reported a non-numeric id as its own error, and a
    /// typed argument makes that unrepresentable instead of re-checked
    /// (ARCH §7 — structural, not remembered). The host still refuses a
    /// non-numeric segment with a 400, because it is reachable by curl.
    pub async fn meshapp_read_chunk<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        chunk_id: u64,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/meshapp/{corpus_id}/chunks/{chunk_id}"),
            &[],
        )
        .await
    }

    /// `GET /internal/meshapp/{corpus}/documents?limit_docs=` — the
    /// latest source documents with their chunks and metadata-derived
    /// outbound links, newest first. The wire form of
    /// `meshapp_document_feed`. `T` is
    /// `sovereign_meshapp::DocumentFeedDto`.
    pub async fn meshapp_document_feed<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        limit_docs: Option<usize>,
    ) -> Result<T> {
        let query: Vec<(&str, String)> = limit_docs
            .into_iter()
            .map(|n| ("limit_docs", n.to_string()))
            .collect();
        self.internal_get(format!("/internal/meshapp/{corpus_id}/documents"), &query)
            .await
    }

    /// `GET /internal/meshapp/{corpus}/wrapped` — the precomputed
    /// Wrapped story-card artifact. The wire form of
    /// `meshapp_wrapped_artifact`. `T` is
    /// `sovereign_meshapp::wrapped::WrappedArtifact`.
    ///
    /// The host serves its cache when fresh and rebuilds otherwise — a
    /// pure Rust fold over the corpus, no inference — so this call can
    /// be slow on a cold corpus. It is not a streaming route and does
    /// not report progress; that is the shape the command had.
    pub async fn meshapp_wrapped_artifact<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/meshapp/{corpus_id}/wrapped"), &[])
            .await
    }

    /// One GET against a loopback `/internal/...` read surface, parsed
    /// as `T`. Every plain atlas and meshapp read goes through here, so
    /// the URL join, the non-success rendering and the parse-error
    /// context are decided once (ARCH §10.6) rather than twenty times.
    ///
    /// `query` is appended only when non-empty, so a path with no
    /// parameters is requested verbatim — a trailing `?` is a different
    /// URL to a router that matches on the whole thing.

    // ── The local-corpus manager (sv-surface D5) ─────────────────
    //
    // Fifteen methods for `lc_http`'s fifteen routes over the
    // daemon's OWN `LocalCorpusManager`. Generic for the family
    // reason: this crate is Tier-0 and cannot name
    // `sovereign_tools::local_corpus::*`. Each doc names the type.
    //
    // A 503 from any of these is "this daemon installed no local-corpus
    // runtime" and arrives as an `Err`, never as an empty list — the
    // pane would render an empty list as "you have no vaults".

    /// `GET /internal/corpus/local` — every registered local corpus.
    /// `T` is `sovereign_tools::local_corpus::config::LocalCorpusConfig`.
    ///
    /// An empty vec is a real answer (a fresh install), which is why an
    /// absent runtime is an `Err` and not one.
    pub async fn lc_list<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.internal_get("/internal/corpus/local".to_string(), &[])
            .await
    }

    /// `POST /internal/corpus/local` — register (or re-register) one
    /// local corpus with the daemon's manager. `B` and `T` are both
    /// `sovereign_tools::local_corpus::config::LocalCorpusConfig`.
    ///
    /// The answer is the config AS REGISTERED, and its `id` is the only
    /// id to use afterwards: the manager keeps the EXISTING id when this
    /// path is already registered under one, so the id sent and the id
    /// kept can differ. Ingesting under the id you SENT is the
    /// `404 … is not registered locally` this method exists to end.
    ///
    /// Idempotent — re-registering the same id overwrites, mirroring
    /// `LocalCorpusManager::register`. There is no 409.
    pub async fn lc_register<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        config: &B,
    ) -> Result<T> {
        self.internal_post_json("/internal/corpus/local".to_string(), config)
            .await
    }

    /// `GET /internal/corpus/local/ocr-available` — whether this daemon
    /// can OCR a scanned PDF. `T` is
    /// `sovereign_contracts::daemon_wire::OcrAvailability`.
    ///
    /// The command it replaces degraded a missing manager to `false`;
    /// this does not, because "OCR unavailable" and "no runtime" want
    /// different remedies from the pane.
    pub async fn lc_ocr_available<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        self.internal_get("/internal/corpus/local/ocr-available".to_string(), &[])
            .await
    }

    /// `GET /internal/corpus/local/incomplete-jobs` — every ingest that
    /// started and never finished. `T` is
    /// `sovereign_tools::local_corpus::IncompleteJob`.
    pub async fn lc_incomplete_jobs<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.internal_get("/internal/corpus/local/incomplete-jobs".to_string(), &[])
            .await
    }

    /// `GET /internal/corpus/local/{corpus}` — one corpus's config.
    /// `T` is `sovereign_tools::local_corpus::config::LocalCorpusConfig`.
    ///
    /// `Ok(None)` is the 404 and only the 404: this corpus is not
    /// registered. An absent runtime stays an `Err`.
    pub async fn lc_get<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Option<T>> {
        self.internal_get_opt(format!("/internal/corpus/local/{corpus_id}"), &[])
            .await
    }

    /// `DELETE /internal/corpus/local/{corpus}` — unregister and drop
    /// the index. Answers `204` with no body.
    pub async fn lc_remove(&self, corpus_id: &str) -> Result<()> {
        self.internal_delete(format!("/internal/corpus/local/{corpus_id}"))
            .await
    }

    /// `POST /internal/corpus/local/{corpus}/cancel` — ask an in-flight
    /// ingest to stop. `T` is `sovereign_contracts::daemon_wire::CancelAck`.
    ///
    /// `cancelled: false` on a 200 means there was nothing running —
    /// a successful call, not a failure.
    pub async fn lc_cancel<T: serde::de::DeserializeOwned>(&self, corpus_id: &str) -> Result<T> {
        self.internal_post_empty(format!("/internal/corpus/local/{corpus_id}/cancel"))
            .await
    }

    /// `GET /internal/corpus/local/{corpus}/git` — is the vault a git
    /// worktree, and is it clean? `T` is
    /// `sovereign_tools::local_corpus::git::GitStatus`; `Ok(None)` is
    /// the route's explicit `null` — not a repository.
    pub async fn lc_check_git<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Option<T>> {
        self.internal_get(format!("/internal/corpus/local/{corpus_id}/git"), &[])
            .await
    }

    /// `POST /internal/corpus/local/{corpus}/write-tags` — write the
    /// clustered tags into the vault's front-matter. `T` is
    /// `sovereign_tools::local_corpus::writeback::WriteBackResult`.
    ///
    /// `git_commit` defaults to `false` at the HOST when omitted; this
    /// method always sends it so the caller's intent is explicit on
    /// the wire.
    pub async fn lc_write_tags<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        git_commit: bool,
    ) -> Result<T> {
        self.internal_post_json(
            format!("/internal/corpus/local/{corpus_id}/write-tags"),
            &serde_json::json!({ "git_commit": git_commit }),
        )
        .await
    }

    /// `GET /internal/corpus/local/{corpus}/snapshots` — the
    /// pre-write-back snapshots this daemon holds. `T` is
    /// `sovereign_tools::local_corpus::writeback::SnapshotMeta`.
    pub async fn lc_snapshots<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<T>> {
        self.internal_get(format!("/internal/corpus/local/{corpus_id}/snapshots"), &[])
            .await
    }

    /// `POST /internal/corpus/local/{corpus}/rollback` — restore one
    /// snapshot. `T` is
    /// `sovereign_tools::local_corpus::writeback::RollbackResult`.
    ///
    /// `snapshot_path` is a value from `lc_snapshots`, not a path the
    /// caller composed.
    pub async fn lc_rollback<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        snapshot_path: &str,
    ) -> Result<T> {
        self.internal_post_json(
            format!("/internal/corpus/local/{corpus_id}/rollback"),
            &serde_json::json!({ "snapshot_path": snapshot_path }),
        )
        .await
    }

    /// `POST /internal/corpus/local/{corpus}/clean` — strip
    /// written-back tags from the vault. `T` is
    /// `sovereign_tools::local_corpus::writeback::CleanResult`.
    pub async fn lc_clean<T: serde::de::DeserializeOwned>(&self, corpus_id: &str) -> Result<T> {
        self.internal_post_empty(format!("/internal/corpus/local/{corpus_id}/clean"))
            .await
    }

    /// `POST /internal/corpus/local/{corpus}/preview` — what write-back
    /// WOULD do. `T` is
    /// `sovereign_tools::local_corpus::preview::VaultPreview`;
    /// `config` serialises to a `ClusterConfig`, and `None` leaves the
    /// thresholds to the host's `ClusterConfig::default()` — one
    /// decider, and it is not this crate's.
    pub async fn lc_preview<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        config: Option<&B>,
    ) -> Result<T> {
        let body = match config {
            Some(c) => serde_json::json!({ "config": c }),
            None => serde_json::json!({}),
        };
        self.internal_post_json(format!("/internal/corpus/local/{corpus_id}/preview"), &body)
            .await
    }

    /// `POST /internal/corpus/local/{corpus}/search` — search one local
    /// corpus. `T` is `sovereign_contracts::daemon_wire::LocalSearchHit`.
    ///
    /// `limit: None` is the host's 10.
    pub async fn lc_search<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<T>> {
        let mut body = serde_json::json!({ "query": query });
        if let Some(n) = limit {
            body["limit"] = serde_json::json!(n);
        }
        self.internal_post_json(format!("/internal/corpus/local/{corpus_id}/search"), &body)
            .await
    }

    /// `POST /internal/corpus/local/{corpus}/ingest` — submit the
    /// ingest as a JOB. `T` is `sovereign_contracts::daemon_wire::IngestJobAck`.
    ///
    /// Answers `202` as soon as the job is spawned; the ack carries
    /// `progress_route`, the EXISTING watch-status route that reports
    /// it. There is no second job table to poll and this method
    /// deliberately does not invent a wait loop — a caller that wants
    /// one polls the route the ack names.
    pub async fn lc_ingest<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        with_ocr: Option<bool>,
    ) -> Result<T> {
        let body = match with_ocr {
            Some(v) => serde_json::json!({ "with_ocr": v }),
            None => serde_json::json!({}),
        };
        self.internal_post_json(format!("/internal/corpus/local/{corpus_id}/ingest"), &body)
            .await
    }

    // ── Governance (sv-surface D8) ───────────────────────────────
    //
    // The read is generic for the atlas family's reason: its payload
    // carries `corpus_engine::enrichment::GovernanceView` — rules,
    // tensions, dispositions, integrity issues, nine structs deep — and
    // this crate is Tier-0 and cannot name it. Mirroring it would be the
    // §10.6 smell wearing a client's clothes. The WRITES answer op ids
    // and counts, which need no type from anywhere.

    /// `GET /internal/governance/{corpus}/view` — everything a Conflicts
    /// panel renders, in one call. `T` is
    /// `sovereign_mesh::governance_http::GovernanceViewPayload`.
    ///
    /// A corpus with no atlas is an `Err` carrying the host's 404 words,
    /// never an empty view: "not enriched yet" and "no conflicts" are
    /// different banners.
    pub async fn governance_view<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/governance/{corpus_id}/view"), &[])
            .await
    }

    /// `POST .../tensions/{tension}/resolve` — keep one rule; the other
    /// is superseded. Answers the two appended op ids, in append order.
    ///
    /// `keep_rule_id` must be one of the conflict's own two rules; the
    /// host answers 400 naming it when it is not, rather than picking one.
    pub async fn governance_resolve(
        &self,
        corpus_id: &str,
        tension_id: &str,
        keep_rule_id: &str,
        rationale: &str,
    ) -> Result<Vec<String>> {
        let wire: OpIdsWire = self
            .internal_post_json(
                format!("/internal/governance/{corpus_id}/tensions/{tension_id}/resolve"),
                &serde_json::json!({
                    "keep_rule_id": keep_rule_id,
                    "rationale": rationale,
                }),
            )
            .await?;
        Ok(wire.op_ids)
    }

    /// `POST .../tensions/{tension}/accept` — both rules stay in force.
    /// An empty rationale is refused by the host (400): an accepted
    /// conflict that records no reason is the one decision a later reader
    /// cannot act on.
    pub async fn governance_accept(
        &self,
        corpus_id: &str,
        tension_id: &str,
        rationale: &str,
    ) -> Result<Vec<String>> {
        let wire: OpIdsWire = self
            .internal_post_json(
                format!("/internal/governance/{corpus_id}/tensions/{tension_id}/accept"),
                &serde_json::json!({ "rationale": rationale }),
            )
            .await?;
        Ok(wire.op_ids)
    }

    /// `POST .../tensions/{tension}/dismiss` — detector noise. The note is
    /// optional, which is exactly what separates a dismissal from an
    /// acceptance; `None` sends an absent key, not an empty string.
    pub async fn governance_dismiss(
        &self,
        corpus_id: &str,
        tension_id: &str,
        rationale: Option<&str>,
    ) -> Result<Vec<String>> {
        let body = match rationale {
            Some(r) => serde_json::json!({ "rationale": r }),
            None => serde_json::json!({}),
        };
        let wire: OpIdsWire = self
            .internal_post_json(
                format!("/internal/governance/{corpus_id}/tensions/{tension_id}/dismiss"),
                &body,
            )
            .await?;
        Ok(wire.op_ids)
    }

    /// `POST .../tensions/{tension}/undo` — revert the adjudication
    /// bundle atomically. Answers the appended revert op id.
    ///
    /// An OPEN conflict is a 400, not a silent no-op: "there was nothing
    /// to undo" is a fact the caller's button state depends on.
    pub async fn governance_undo(&self, corpus_id: &str, tension_id: &str) -> Result<String> {
        let wire: OpIdWire = self
            .internal_post_empty(format!(
                "/internal/governance/{corpus_id}/tensions/{tension_id}/undo"
            ))
            .await?;
        Ok(wire.op_id)
    }

    /// `POST /internal/governance/{corpus}/seed` — establish or refresh
    /// the governed rule baseline. Answers how many rules were NEWLY
    /// asserted; `0` is the steady state and a success.
    pub async fn governance_seed(&self, corpus_id: &str) -> Result<u32> {
        let wire: SeededWire = self
            .internal_post_empty(format!("/internal/governance/{corpus_id}/seed"))
            .await?;
        Ok(wire.seeded)
    }

    /// `POST /internal/governance/{corpus}/post-build-seed` — migrate atom
    /// ids to content-hash THEN seed, in that order.
    ///
    /// Call this after an atlas build, unconditionally: a non-governance
    /// corpus answers `0` and touches nothing. Do NOT compose it out of
    /// [`Self::governance_seed`] plus a migrate — the order is what keeps
    /// every past decision resolving across a rebuild, and the host owns it.
    pub async fn governance_post_build_seed(&self, corpus_id: &str) -> Result<u32> {
        let wire: SeededWire = self
            .internal_post_empty(format!("/internal/governance/{corpus_id}/post-build-seed"))
            .await?;
        Ok(wire.seeded)
    }

    /// `POST /internal/governance/{corpus}/recipe` — lay down the
    /// governance recipe for a freshly-added folder corpus. Answers the
    /// path it wrote, ON THE HOST.
    ///
    /// The path is the daemon's `recipes_dir()`, which is the point: a
    /// caller writing this itself would put it where the daemon never
    /// looks and the corpus would enrich down the wrong pipeline.
    pub async fn governance_write_recipe(
        &self,
        corpus_id: &str,
        display_name: &str,
        source_path: &str,
    ) -> Result<String> {
        let wire: RecipePathWire = self
            .internal_post_json(
                format!("/internal/governance/{corpus_id}/recipe"),
                &serde_json::json!({
                    "display_name": display_name,
                    "source_path": source_path,
                }),
            )
            .await?;
        Ok(wire.path)
    }

    // ── MCP configuration (sv-surface D8) ────────────────────────

    /// `GET /v1/mcp/servers` — the host's configured MCP servers and what
    /// its live tool registry holds for each. `T` is
    /// `sovereign_contracts::daemon_wire::McpServersResponse`.
    ///
    /// Read `mount.reason` before rendering any connection affordance:
    /// the host reports `live_tool_count` and deliberately reports NO
    /// connect flag, because it keeps no connection manager to ask. A
    /// caller drawing a green dot from a non-zero count would be
    /// inventing the fact the host declined to invent.
    ///
    /// The two config WRITES (`add`/`remove` a server) have no route:
    /// this daemon owns no config-write path. They are not missing from
    /// this client by oversight.
    pub async fn mcp_servers<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        self.internal_get("/v1/mcp/servers".to_string(), &[]).await
    }

    /// `POST /v1/mcp/servers/test` — probe a server WITHOUT persisting
    /// it, answering its tool count. `token` is the one the operator just
    /// typed and has not saved; `None` falls back to the stored secret.
    ///
    /// A reachable server exporting zero tools answers `0` — a success.
    /// An unreachable one is an `Err` carrying the host's 502 words.
    pub async fn mcp_test_connection(
        &self,
        name: &str,
        url: &str,
        bearer: bool,
        token: Option<&str>,
    ) -> Result<usize> {
        let mut body = serde_json::json!({ "name": name, "url": url, "bearer": bearer });
        if let Some(t) = token {
            body["token"] = serde_json::Value::String(t.to_string());
        }
        let wire: ToolCountWire = self
            .internal_post_json("/v1/mcp/servers/test".to_string(), &body)
            .await?;
        Ok(wire.tool_count)
    }

    /// `PUT /v1/mcp/servers/{name}/token` — store the bearer token. A
    /// BLANK token clears it, which is the secret store's own contract
    /// and not a second rule invented here.
    pub async fn mcp_set_token(&self, name: &str, token: &str) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::PUT,
            format!("/v1/mcp/servers/{name}/token"),
            Some(&serde_json::json!({ "token": token })),
        )
        .await
    }

    /// `DELETE /v1/mcp/servers/{name}/token` — clear a stored token. A
    /// no-op when none is set, and `Ok` either way: nothing to delete is
    /// not a failed delete.
    pub async fn mcp_clear_token(&self, name: &str) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::DELETE,
            format!("/v1/mcp/servers/{name}/token"),
            None::<&()>,
        )
        .await
    }

    // ── Recipe-author projects (sv-surface D8) ───────────────────
    //
    // Generic for the family reason above: these payloads carry
    // `ArtifactKind`, `ProjectSummary` and `CheckpointMeta` from
    // `sovereign-recipe-author`, a crate this one does not and should not
    // link. The two that answer a bare string or a nullable id are
    // concrete.

    /// `GET /v1/recipe-projects` — every project with its sidecar
    /// summary, newest-updated first. `T` is
    /// `sovereign_contracts::daemon_wire::RecipeProjectListEntry`.
    ///
    /// An empty vec is a real answer: a fresh install has no projects and
    /// a Welcome pane branches on exactly that.
    pub async fn recipe_projects<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.internal_get("/v1/recipe-projects".to_string(), &[])
            .await
    }

    /// `POST /v1/recipe-projects` — provision the row AND the artifact
    /// tree. `T` is `RecipeProjectListEntry`.
    ///
    /// The feature id is MINTED BY THE HOST from the project's essence,
    /// so `body` is `{title, charter_md, artifact_kind?}` and carries no
    /// id. `feature_project_create` is the other door — that one takes
    /// an id because it is the STORE's route and this is the
    /// composition's (ARCH §7.5).
    pub async fn recipe_project_create<
        B: serde::Serialize + ?Sized,
        T: serde::de::DeserializeOwned,
    >(
        &self,
        body: &B,
    ) -> Result<T> {
        self.internal_post_json("/v1/recipe-projects".to_string(), body)
            .await
    }

    /// `GET /v1/recipe-projects/{id}/dashboard` — the one big read. `T` is
    /// `sovereign_contracts::daemon_wire::RecipeAuthorDashboardState`.
    ///
    /// Its `validation` is an OPTION and `validation_unavailable` says
    /// why when it is `None`. A host that cannot judge an artifact
    /// reports that it did not judge — it does not answer `ok: false`.
    pub async fn recipe_project_dashboard<T: serde::de::DeserializeOwned>(
        &self,
        feature_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/v1/recipe-projects/{feature_id}/dashboard"), &[])
            .await
    }

    /// `PUT /v1/recipe-projects/{id}/toml` — validate, then write only if
    /// valid. `T` is
    /// `sovereign_contracts::daemon_wire::RecipeValidationReport`.
    ///
    /// A recipe that does not parse comes back `Ok` with `ok: false` and
    /// the errors, and NOTHING was written — that is a successful
    /// validation with a negative verdict, and a caller keeps its
    /// in-flight text and re-saves. An `Err` means the request itself
    /// failed; a 501 in particular means this host does not link the
    /// parser for that artifact kind and refused to write unjudged bytes.
    pub async fn recipe_project_save_toml<T: serde::de::DeserializeOwned>(
        &self,
        feature_id: &str,
        edited_toml: &str,
    ) -> Result<T> {
        self.internal_put_json(
            format!("/v1/recipe-projects/{feature_id}/toml"),
            &serde_json::json!({ "edited_toml": edited_toml }),
        )
        .await
    }

    /// `POST /v1/recipe-projects/{id}/link-recent-artifact` — register the
    /// artifact the agent wrote THIS turn onto the project's summary.
    ///
    /// `since_unix` is the turn's start time, so a chat-only turn links
    /// nothing and answers `None`. Idempotent and cheap; call it on every
    /// turn-complete.
    pub async fn recipe_project_link_recent(
        &self,
        feature_id: &str,
        since_unix: i64,
    ) -> Result<Option<String>> {
        let wire: LinkRecentWire = self
            .internal_post_json(
                format!("/v1/recipe-projects/{feature_id}/link-recent-artifact"),
                &serde_json::json!({ "since_unix": since_unix }),
            )
            .await?;
        Ok(wire.artifact_id)
    }

    /// `POST /v1/recipe-projects/{id}/checkpoints/{checkpoint}/restore` —
    /// restore a snapshot and lay down the restore anchor. `T` is
    /// `sovereign_contracts::daemon_wire::RestoreCheckpointOutcome`.
    pub async fn recipe_project_restore_checkpoint<T: serde::de::DeserializeOwned>(
        &self,
        feature_id: &str,
        checkpoint_id: &str,
    ) -> Result<T> {
        self.internal_post_empty(format!(
            "/v1/recipe-projects/{feature_id}/checkpoints/{checkpoint_id}/restore"
        ))
        .await
    }

    /// `GET /v1/recipe-projects/{id}/prelude` — the per-turn situated
    /// context block to prepend to the partner's message.
    ///
    /// The block ends with `[Partner says]\n`; concatenate the user's text
    /// straight onto it. Cheap enough to re-render every turn, which is
    /// what keeps the agent's view of project state fresh.
    pub async fn recipe_project_prelude(&self, feature_id: &str) -> Result<String> {
        let wire: PreludeWire = self
            .internal_get(format!("/v1/recipe-projects/{feature_id}/prelude"), &[])
            .await?;
        Ok(wire.prelude)
    }

    // ── Insights by id (sv-surface D8) ───────────────────────────

    /// `POST /v1/insights/by-id` — fetch insight nodes by id.
    ///
    /// A POST because the input is a LIST and no flat query string
    /// carries one without a second encoding — the call `atlas_atoms` and
    /// `notes_list` already make.
    ///
    /// Read [`InsightsById::missing`]: the store drops ids that name no
    /// live row, so a short `insights` alone would not say WHICH went
    /// missing. The ORDER of `insights` is the store's, not the request's.
    pub async fn insights_by_id(&self, ids: &[String]) -> Result<InsightsById> {
        self.internal_post_json(
            "/v1/insights/by-id".to_string(),
            &serde_json::json!({ "ids": ids }),
        )
        .await
    }

    /// `GET /internal/corpus/local/{corpus}/ingest/progress` — how far
    /// along an ingest is, and what it INDEXED once it is done. `T` is
    /// `sovereign_contracts::daemon_wire::IngestProgressView<Stats>` (the
    /// route's `sovereign_mesh::lc_http::IngestProgress` for a caller that
    /// links the daemon).
    ///
    /// This is the route [`Self::lc_ingest`]'s ack names, and the one to
    /// poll. Read `finished`, not the phase: the phase file describes the
    /// atlas build on one path and stops at `Scanning` forever on the
    /// other, and only the terminal receipt answers "is the ingest done".
    /// The counts live on `outcome.stats` and exist only once `finished`.
    ///
    /// A registered corpus that has never ingested answers `200` with
    /// both halves null — not a 404, which means no such corpus.
    pub async fn lc_ingest_progress<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/corpus/local/{corpus_id}/ingest/progress"),
            &[],
        )
        .await
    }

    /// `POST /internal/corpus/local/{corpus}/cluster` — run clustering +
    /// LLM labelling on an ingested vault as a JOB. Answers the ingest
    /// job's ack shape (`T` is
    /// `sovereign_contracts::daemon_wire::IngestJobAck`): the host's job
    /// id and the progress route that reports it. `config` serialises to
    /// a `ClusterConfig`; `None` leaves the thresholds to the host's
    /// `ClusterConfig::default()` — the same one decider `lc_preview`
    /// defers to.
    pub async fn lc_cluster<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        config: Option<&B>,
    ) -> Result<T> {
        let body = match config {
            Some(c) => serde_json::json!({ "config": c }),
            None => serde_json::json!({}),
        };
        self.internal_post_json(format!("/internal/corpus/local/{corpus_id}/cluster"), &body)
            .await
    }

    /// `POST /internal/corpus/local/pre-scan` — register the corpus for a
    /// user-picked path on the daemon's manager and classify what an
    /// ingest would read. `body` serialises to
    /// `sovereign_mesh::lc_http::PreScanRequest` (`path`, `source_type`,
    /// optional `display_name`); `T` is `sovereign_contracts::daemon_wire::PreScanAnswerView<_>`, whose
    /// `corpus_id` is the id the registry KEPT — use that one.
    pub async fn lc_pre_scan<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        body: &B,
    ) -> Result<T> {
        self.internal_post_json("/internal/corpus/local/pre-scan".to_string(), body)
            .await
    }

    /// `GET /internal/corpus/local/{corpus}/cluster/progress?after=N` —
    /// the frames a cluster job has appended from the caller's cursor
    /// on. `T` is `sovereign_contracts::daemon_wire::ClusterProgressView<_>`; its `next`
    /// is the cursor to send on the following call.
    pub async fn lc_cluster_progress<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        after: usize,
    ) -> Result<T> {
        self.internal_get(
            format!("/internal/corpus/local/{corpus_id}/cluster/progress"),
            &[("after", after.to_string())],
        )
        .await
    }

    // ─── sv-surface: the reading family (reading_http) ───────
    //
    // Three of `reading_http`'s six routes, added for the CLI rung:
    // `svrn reading-diag` walked the deref chain by re-deriving it over
    // `corpus_engine` — its own `atom_brief`, its own edge fold, its own
    // `detect_atom_spans` call — beside the handlers that already answer
    // the same five questions. The file's own banner called those helpers
    // "(mirror reading_http)", which is the §10.6 smell naming itself.
    //
    // Generic for the family reason (see `corpus_atoms` above): this
    // crate is Tier-0 and cannot name `reading_http`'s response types.
    // It could not reuse them even with an edge — every one is
    // `Serialize`-only, and `atom_type` / `edge_type` / `role` are
    // `&'static str`, which no `Deserialize` impl can fill. The caller
    // declares a mirror carrying `String` in those three positions; each
    // doc below names the exact type the bytes are.

    /// `GET /internal/corpus/{corpus}/chunks/{chunk_id}/neighbors?radius=`
    /// — the center chunk with its prev/next siblings inside
    /// `source_doc_id`. `T` is `sovereign_mesh::reading_http::NeighborWindowResponse`.
    ///
    /// The center chunk carries `atom_spans` already detected against
    /// its own text, so a caller that wants both the window and the
    /// atom mentions pays ONE round trip and runs no detector of its
    /// own.
    ///
    /// `Ok(None)` for a 404 — the host saying no chunk in this corpus
    /// carries that id. Every other non-success is an `Err`, so a
    /// corpus that will not open never reads as "chunk absent"
    /// (§18.3, and the confusion `atlas_atom_detail`'s doc records).
    pub async fn reading_neighbors<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        chunk_id: u64,
        radius: usize,
    ) -> Result<Option<T>> {
        self.internal_get_opt(
            format!("/internal/corpus/{corpus_id}/chunks/{chunk_id}/neighbors"),
            &[("radius", radius.to_string())],
        )
        .await
    }

    /// `GET /internal/corpus/{corpus}/atoms/{atom_id}` — the one-hop
    /// card: surface fields, related edges, cross-corpus bridges. `T`
    /// is `sovereign_mesh::reading_http::AtomCard`.
    ///
    /// `related` is UNCAPPED and resolved: the host drops an edge whose
    /// other end is not in this atlas, so `related.len()` counts rows a
    /// caller can name rather than raw edge hits.
    ///
    /// `Ok(None)` for a 404 — no such atom in this corpus's atoms.json.
    pub async fn reading_atom_card<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        atom_id: &str,
    ) -> Result<Option<T>> {
        self.internal_get_opt(format!("/internal/corpus/{corpus_id}/atoms/{atom_id}"), &[])
            .await
    }

    /// `GET /internal/corpus/{corpus}/atoms/{atom_id}/elsewhere` — the
    /// sections this atom is evidenced in, each with a resolved
    /// `chunk_id` when the index carries one. `T` is
    /// `sovereign_mesh::reading_http::AtomElsewhere`.
    ///
    /// A `SectionRef` with no `chunk_id` is the answer "this section is
    /// in the atom's evidence and no chunk claims it" — a legacy ingest
    /// or a partial reshard — not a failure.
    ///
    /// `Ok(None)` for a 404.
    pub async fn reading_atom_elsewhere<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
        atom_id: &str,
    ) -> Result<Option<T>> {
        self.internal_get_opt(
            format!("/internal/corpus/{corpus_id}/atoms/{atom_id}/elsewhere"),
            &[],
        )
        .await
    }

    // ─── sv-surface D9a: the documents family ────────────────

    /// `GET /v1/documents` — every ingested document asset. `T` is
    /// `sovereign_core::types::DocumentAsset`.
    ///
    /// Generic on the caller's row type for the `list_skills` reason:
    /// this crate does not depend on `sovereign-core`, and mirroring
    /// `DocumentAsset` (a skeleton over four nested types) here would
    /// mint a twin the campaign exists to delete.
    pub async fn list_documents<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            documents: Vec<T>,
        }
        let wire: Wire<T> = self.internal_get("/v1/documents".to_string(), &[]).await?;
        Ok(wire.documents)
    }

    /// `GET /v1/documents/{id}` — one asset record.
    ///
    /// `Ok(None)` is the 404 and ONLY the 404: this daemon stores no
    /// such asset. Every other failure stays an `Err`, because "not
    /// stored" and "the store would not answer" ask for different
    /// remedies from the pane (ARCH §18.3).
    pub async fn get_document<T: serde::de::DeserializeOwned>(
        &self,
        asset_id: &str,
    ) -> Result<Option<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            document: T,
        }
        let wire: Option<Wire<T>> = self
            .internal_get_opt(format!("/v1/documents/{asset_id}"), &[])
            .await?;
        Ok(wire.map(|w| w.document))
    }

    /// `DELETE /v1/documents/{id}` — remove an asset and its chunks.
    pub async fn delete_document(&self, asset_id: &str) -> Result<()> {
        self.internal_delete(format!("/v1/documents/{asset_id}"))
            .await
    }

    /// `POST /v1/documents/{id}/skeleton` — rebuild the structural
    /// skeleton from stored chunks; answers the REFRESHED record.
    /// `T` is `sovereign_core::types::DocumentAsset`.
    pub async fn rebuild_document_skeleton<T: serde::de::DeserializeOwned>(
        &self,
        asset_id: &str,
    ) -> Result<T> {
        #[derive(Deserialize)]
        struct Wire<T> {
            document: T,
        }
        let wire: Wire<T> = self
            .internal_post_empty(format!("/v1/documents/{asset_id}/skeleton"))
            .await?;
        Ok(wire.document)
    }

    /// `GET /v1/documents/legacy` — documents in the old `documents`
    /// table that no asset owns. `T` is
    /// `sovereign_contracts::daemon_wire::LegacyDocumentEntry`.
    pub async fn list_legacy_documents<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            documents: Vec<T>,
        }
        let wire: Wire<T> = self
            .internal_get("/v1/documents/legacy".to_string(), &[])
            .await?;
        Ok(wire.documents)
    }

    /// `POST /v1/documents` — ingest a local file as a document asset,
    /// as a JOB. Answers the PENDING record (202) whose id every progress
    /// frame is stamped with; poll [`Self::document_ingest_progress`]
    /// for the frames. `T` is `sovereign_core::types::DocumentAsset`.
    pub async fn upload_document<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        #[derive(Deserialize)]
        struct Wire<T> {
            document: T,
        }
        let wire: Wire<T> = self
            .internal_post_json(
                "/v1/documents".to_string(),
                &serde_json::json!({ "path": path }),
            )
            .await?;
        Ok(wire.document)
    }

    /// `GET /v1/documents/{id}/progress?after=N` — the frames an upload
    /// job appended from the caller's cursor on. `T` is
    /// `sovereign_contracts::daemon_wire::DocumentIngestProgress`; its
    /// `next` is the cursor to send on the following call.
    pub async fn document_ingest_progress<T: serde::de::DeserializeOwned>(
        &self,
        asset_id: &str,
        after: usize,
    ) -> Result<T> {
        self.internal_get(
            format!("/v1/documents/{asset_id}/progress"),
            &[("after", after.to_string())],
        )
        .await
    }

    /// `POST /v1/documents/legacy` — chunk a local file into the legacy
    /// `documents` table (the old paperclip path). `T` is
    /// `sovereign_contracts::daemon_wire::IngestLegacyResponse`.
    pub async fn ingest_legacy_document<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T> {
        self.internal_post_json(
            "/v1/documents/legacy".to_string(),
            &serde_json::json!({ "path": path }),
        )
        .await
    }

    /// `POST /v1/documents/{id}/ask` — ask a question of a document asset,
    /// as a JOB. The daemon persists the question into `conversation_id`
    /// before answering 202; poll [`Self::ask_document_progress`] for the
    /// operation frames and the outcome. `T` is
    /// `sovereign_contracts::daemon_wire::AskJobAck`.
    pub async fn ask_document<T: serde::de::DeserializeOwned>(
        &self,
        asset_id: &str,
        question: &str,
        conversation_id: &str,
    ) -> Result<T> {
        self.internal_post_json(
            format!("/v1/documents/{asset_id}/ask"),
            &serde_json::json!({ "question": question, "conversation_id": conversation_id }),
        )
        .await
    }

    /// `GET /v1/documents/{id}/ask/{job_id}?after=N` — the frames an ask
    /// job appended from the caller's cursor on, and its `outcome` once
    /// `finished`. `T` is `sovereign_contracts::daemon_wire::AskProgress`.
    pub async fn ask_document_progress<T: serde::de::DeserializeOwned>(
        &self,
        asset_id: &str,
        job_id: &str,
        after: usize,
    ) -> Result<T> {
        self.internal_get(
            format!("/v1/documents/{asset_id}/ask/{job_id}"),
            &[("after", after.to_string())],
        )
        .await
    }

    /// `POST /v1/documents/legacy/promote` — mint an asset over chunks
    /// already in the store. `T` is
    /// `sovereign_core::types::DocumentAsset`.
    pub async fn promote_legacy_document<T: serde::de::DeserializeOwned>(
        &self,
        source: &str,
    ) -> Result<T> {
        #[derive(Deserialize)]
        struct Wire<T> {
            document: T,
        }
        let wire: Wire<T> = self
            .internal_post_json(
                "/v1/documents/legacy/promote".to_string(),
                &serde_json::json!({ "source": source }),
            )
            .await?;
        Ok(wire.document)
    }

    // ─── sv-surface D9a: the catalogue and the shelf ─────────

    /// `GET /internal/corpus/catalog` — the built-in catalogue unioned
    /// with every installed index it does not name. `T` is
    /// `sovereign_mesh::corpus_catalog_http::CatalogEntry`.
    ///
    /// The `"installing"` state the picker also renders is NOT here —
    /// the daemon's in-flight decider is `GET /internal/corpus/status`
    /// (rung 1) and the caller composes the two.
    pub async fn corpus_catalog<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            corpora: Vec<T>,
        }
        let wire: Wire<T> = self
            .internal_get("/internal/corpus/catalog".to_string(), &[])
            .await?;
        Ok(wire.corpora)
    }

    /// `GET /internal/corpus/notebooks` — the unified Library shelf.
    /// `T` is `sovereign_mesh::corpus_catalog_http::NotebookRow`.
    ///
    /// A 503 here means the daemon has no local-corpus registry, so it
    /// cannot say which rows are the user's own. That is an `Err`, not
    /// an empty shelf — the swallow this route was built to delete.
    pub async fn corpus_notebooks<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            notebooks: Vec<T>,
        }
        let wire: Wire<T> = self
            .internal_get("/internal/corpus/notebooks".to_string(), &[])
            .await?;
        Ok(wire.notebooks)
    }

    /// `GET /internal/corpus/diagnose` — the engine's indexes-dir
    /// report, verbatim.
    pub async fn corpus_diagnose(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Wire {
            report: String,
        }
        let wire: Wire = self
            .internal_get("/internal/corpus/diagnose".to_string(), &[])
            .await?;
        Ok(wire.report)
    }

    /// `POST /internal/corpus/{corpus}/index/build` — build an installed
    /// corpus's vector + FTS indexes as a daemon job. `T` is
    /// `sovereign_contracts::daemon_wire::IngestJobAck` (202). A corpus
    /// already building answers 409 by name; an unknown one 404.
    pub async fn corpus_index_build<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_post_json(
            format!("/internal/corpus/{corpus_id}/index/build"),
            &serde_json::json!({}),
        )
        .await
    }

    /// `GET /internal/corpus/{corpus}/index/progress` — `T` is
    /// `sovereign_contracts::daemon_wire::IndexBuildProgress`. `Idle` for a
    /// corpus nobody asked to build in this daemon's lifetime.
    pub async fn corpus_index_progress<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<T> {
        self.internal_get(format!("/internal/corpus/{corpus_id}/index/progress"), &[])
            .await
    }

    /// `GET /internal/corpus/{corpus}/health` — enrichment health for
    /// one installed corpus. `T` is
    /// `sovereign_mesh::corpus_catalog_http::CorpusHealth`.
    ///
    /// `Ok(None)` is the 404 and only the 404: no index for this
    /// corpus opened. "Installed but never enriched" comes back as an
    /// `Ok(Some(_))` with zeroes, which is a different fact.
    pub async fn corpus_health<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Option<T>> {
        self.internal_get_opt(format!("/internal/corpus/{corpus_id}/health"), &[])
            .await
    }

    /// `GET /internal/corpus/{corpus}/coverage-card` — the typed
    /// authoritative-store card. `T` is
    /// `corpus_engine::enrichment::atlas::analysis::sec_facts::CoverageCard`.
    ///
    /// `Ok(None)` is a 200 with `card: null` — "this corpus declares
    /// no typed store" is an answer, not an absence (ARCH §18.3).
    pub async fn corpus_coverage_card<T: serde::de::DeserializeOwned>(
        &self,
        corpus_id: &str,
    ) -> Result<Option<T>> {
        #[derive(Deserialize)]
        struct Wire<T> {
            card: Option<T>,
        }
        let wire: Wire<T> = self
            .internal_get(format!("/internal/corpus/{corpus_id}/coverage-card"), &[])
            .await?;
        Ok(wire.card)
    }

    /// `POST /internal/corpus/{corpus}/retry-enrichment` — re-parse
    /// stored skeleton failures with the repair parser. No inference.
    /// Answers `(salvaged, still_failed)`.
    pub async fn corpus_retry_enrichment(&self, corpus_id: &str) -> Result<(u64, u64)> {
        #[derive(Deserialize)]
        struct Wire {
            salvaged: u64,
            still_failed: u64,
        }
        let wire: Wire = self
            .internal_post_empty(format!("/internal/corpus/{corpus_id}/retry-enrichment"))
            .await?;
        Ok((wire.salvaged, wire.still_failed))
    }

    // ─── sv-surface D9a: the skill toggle and readiness ──────

    /// `PUT /v1/skills/{id}/active` — put one skill in, or out of, the
    /// SERVING runtime's active set. `T` is
    /// `sovereign_mesh::turn_extras_http::SkillWireEntry`.
    ///
    /// Answers the registry's read-back, not the caller's request, so
    /// a surface renders what the daemon holds rather than what it
    /// asked for. `Ok(None)` is the 404 and only the 404: no such
    /// skill is registered on this daemon.
    pub async fn set_skill_active<T: serde::de::DeserializeOwned>(
        &self,
        skill_id: &str,
        active: bool,
    ) -> Result<Option<T>> {
        self.internal_put_json_opt(
            format!("/v1/skills/{skill_id}/active"),
            &serde_json::json!({ "active": active }),
        )
        .await
    }

    /// `GET /v1/ready` — is the backend that will answer my turns up.
    ///
    /// `Ok(false)` for the 503: the daemon answered and said it serves
    /// no turns, which is a fact a splash can act on. An `Err` means
    /// nothing answered at all — a different fact, and the reason this
    /// does not collapse both into a bool (ARCH §18.3).
    ///
    /// This is NOT an identity probe. WHO is on the port is
    /// `/status.process.pid`'s single answer (3c7ad5933); compose the
    /// two rather than asking this one to grow a second pid.
    pub async fn backend_ready(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct Wire {
            ready: bool,
        }
        // The 503 is this route's `absent`: the daemon ANSWERED and said
        // it serves no turns. Nothing answering at all stays an `Err` —
        // a different fact, and the reason this does not collapse both
        // into a bool (ARCH §18.3).
        let (url, ctx) = self.target("GET", "/v1/ready");
        let answer = self
            .exchange(
                self.http.get(&url),
                &url,
                &ctx,
                Some(reqwest::StatusCode::SERVICE_UNAVAILABLE),
            )
            .await?;
        match answer {
            Some(body) => Ok(parse::<Wire>(&body, &ctx)?.ready),
            None => Ok(false),
        }
    }

    // ─── The internal exchange: one sentence, six named shapes ────
    //
    // Every method above that talks to the host says one of six
    // sentences: GET a thing, GET a thing that may not be there, POST
    // a body, PUT a body, PUT a body whose answer may not be there,
    // DELETE. Until 2026-09-10 thirty-three of them spelled that out
    // by hand — 720 lines of build-url → send → read-body →
    // status-match → parse, with the 404-is-an-answer rule written
    // out nine separate times. Nine copies of one rule is nine places
    // free to widen it independently, which is exactly the bug
    // `daemon_reading_get` shipped by mapping ANY 404 to `Ok(None)`
    // (ARCH §10.6, §18.3). The six below are the only place any of it
    // is spelled now.

    /// Send a prepared request and apply the status rule.
    ///
    /// `absent` names the ONE status this call reads as an ANSWER
    /// rather than a failure — `NOT_FOUND` for the reads whose 404
    /// means "this host has no such row", `SERVICE_UNAVAILABLE` for
    /// [`Self::backend_ready`]. Every other non-success becomes the
    /// host's own words: a corpus that will not open must never arrive
    /// as "the atom is absent".
    ///
    /// `Ok(None)` is `absent`; `Ok(Some(body))` is a success body the
    /// caller may parse or discard.
    async fn exchange(
        &self,
        req: reqwest::RequestBuilder,
        url: &str,
        ctx: &str,
        absent: Option<reqwest::StatusCode>,
    ) -> Result<Option<String>> {
        // The ONE place the whole-exchange ceiling is applied, so the
        // thirteen shapes below cannot each answer it differently
        // (ARCH principle 8). `connect` and `read` are already on the
        // client itself and reach this request without being spelled.
        let req = match self.budget.total {
            Some(total) => req.timeout(total),
            None => req,
        };
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("{ctx}: {e}")))?;
        let (status, body) = read_body(resp, url).await?;
        if absent == Some(status) {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(host_words(status, &body, ctx));
        }
        Ok(Some(body))
    }

    /// [`Self::exchange`] for a call that named NO absent status, so a
    /// body is owed. The `None` arm is structurally unreachable; it is
    /// reported rather than defaulted, because a success-shaped value
    /// invented here is the §18.3 failure itself.
    async fn required(&self, req: reqwest::RequestBuilder, url: &str, ctx: &str) -> Result<String> {
        match self.exchange(req, url, ctx, None).await? {
            Some(body) => Ok(body),
            None => Err(Error::Inference(format!("{ctx}: no answer"))),
        }
    }

    /// The url and the error-context string for one call — the two
    /// strings every shape below needs and none of them should spell.
    fn target(&self, method: &str, path: &str) -> (String, String) {
        let url = format!("{}{path}", self.base);
        let ctx = format!("{method} {url}");
        (url, ctx)
    }

    /// `GET`, parsed answer. Any non-success is the host's words.
    pub(crate) async fn internal_get<T: serde::de::DeserializeOwned>(
        &self,
        path: String,
        query: &[(&str, String)],
    ) -> Result<T> {
        let (url, ctx) = self.target("GET", &path);
        let mut req = self.http.get(&url);
        if !query.is_empty() {
            req = req.query(query);
        }
        parse(&self.required(req, &url, &ctx).await?, &ctx)
    }

    /// `GET` where a 404 is an ANSWER — the host has no such row —
    /// and EVERY other non-success stays an error.
    ///
    /// THE one implementation of that rule. Nine families used to
    /// spell it themselves; each spelling was a separate chance to
    /// widen "no such row" into "the store would not answer", and the
    /// two ask the caller for different remedies (ARCH §18.3).
    async fn internal_get_opt<T: serde::de::DeserializeOwned>(
        &self,
        path: String,
        query: &[(&str, String)],
    ) -> Result<Option<T>> {
        let (url, ctx) = self.target("GET", &path);
        // An empty slice attaches nothing, so the url stays
        // byte-for-byte the one a caller with no parameters sent.
        let mut req = self.http.get(&url);
        if !query.is_empty() {
            req = req.query(query);
        }
        self.exchange(req, &url, &ctx, Some(reqwest::StatusCode::NOT_FOUND))
            .await?
            .map(|b| parse(&b, &ctx))
            .transpose()
    }

    /// `POST` with a JSON body, parsed answer. The POST twin of
    /// [`Self::internal_get`].
    pub(crate) async fn internal_post_json<
        B: serde::Serialize + ?Sized,
        T: serde::de::DeserializeOwned,
    >(
        &self,
        path: String,
        body: &B,
    ) -> Result<T> {
        let (url, ctx) = self.target("POST", &path);
        let answer = self
            .required(self.http.post(&url).json(body), &url, &ctx)
            .await?;
        parse(&answer, &ctx)
    }

    /// `POST` with an EMPTY JSON body (`{}`) — the shape for routes
    /// whose whole input is the corpus id in the path and whose handler
    /// still extracts a body.
    async fn internal_post_empty<T: serde::de::DeserializeOwned>(&self, path: String) -> Result<T> {
        self.internal_post_json(path, &serde_json::json!({})).await
    }

    /// `POST` with NO body at all, parsed answer. Deliberately distinct
    /// from [`Self::internal_post_empty`]: the two put different bytes
    /// on the wire, and a route whose handler takes no body extractor
    /// must keep receiving none.
    pub(crate) async fn internal_post_bare<T: serde::de::DeserializeOwned>(
        &self,
        path: String,
    ) -> Result<T> {
        let (url, ctx) = self.target("POST", &path);
        let answer = self.required(self.http.post(&url), &url, &ctx).await?;
        parse(&answer, &ctx)
    }

    /// `PUT` with a JSON body, parsed answer.
    async fn internal_put_json<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        path: String,
        body: &B,
    ) -> Result<T> {
        let (url, ctx) = self.target("PUT", &path);
        let answer = self
            .required(self.http.put(&url).json(body), &url, &ctx)
            .await?;
        parse(&answer, &ctx)
    }

    /// [`Self::internal_put_json`] under [`Self::internal_get_opt`]'s
    /// 404 rule — the shape for a write whose SUBJECT may not exist.
    async fn internal_put_json_opt<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        path: String,
        body: &B,
    ) -> Result<Option<T>> {
        let (url, ctx) = self.target("PUT", &path);
        let answer = self
            .exchange(
                self.http.put(&url).json(body),
                &url,
                &ctx,
                Some(reqwest::StatusCode::NOT_FOUND),
            )
            .await?;
        answer.map(|a| parse(&a, &ctx)).transpose()
    }

    /// `DELETE` whose answer is only its status — the 204 shape. The
    /// body is read (a connection is not returned to the pool
    /// otherwise) and discarded, because these routes send none.
    async fn internal_delete(&self, path: String) -> Result<()> {
        let (url, ctx) = self.target("DELETE", &path);
        self.exchange(self.http.delete(&url), &url, &ctx, None)
            .await?;
        Ok(())
    }

    /// `DELETE` whose answer is a VALUE — the notes `{existed}` shape,
    /// where "the row was not there" is a fact the host states rather
    /// than a status the caller has to infer (ARCH §18.3).
    async fn internal_delete_json<T: serde::de::DeserializeOwned>(
        &self,
        path: String,
    ) -> Result<T> {
        let (url, ctx) = self.target("DELETE", &path);
        let answer = self.required(self.http.delete(&url), &url, &ctx).await?;
        parse(&answer, &ctx)
    }

    /// A write whose answer is only its status, at any method — the
    /// `POST …/end` and `POST …/tool-outcome` shape. `body` is
    /// `None` for the routes that take no body at all, and their
    /// request stays byte-for-byte the bodiless one they always sent.
    pub(crate) async fn internal_write_no_answer<B: serde::Serialize + ?Sized>(
        &self,
        method: reqwest::Method,
        path: String,
        body: Option<&B>,
    ) -> Result<()> {
        let (url, ctx) = self.target(method.as_str(), &path);
        let mut req = self.http.request(method, &url);
        if let Some(body) = body {
            req = req.json(body);
        }
        self.exchange(req, &url, &ctx, None).await?;
        Ok(())
    }

    /// The WebSocket URL for one conversation's turn stream.
    fn stream_url(&self, conversation_id: &str, options: &StreamOptions) -> String {
        let ws = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            format!("ws://{}", self.base)
        };
        let mut url = format!("{ws}/v1/conversations/{conversation_id}/stream");
        if options.claim_approvals {
            // The host's own spelling (`StreamParams::approvals`) — one
            // query key, and its absence is the unclaimed byte-for-byte
            // URL every client sent before this field existed.
            url.push_str("?approvals=true");
        }
        url
    }

    /// Open the turn stream for a conversation, unclaimed.
    pub async fn connect(&self, conversation_id: &str) -> Result<TurnStream> {
        self.connect_with(conversation_id, StreamOptions::default())
            .await
    }

    /// Open the turn stream with options — the claimed spelling (G12).
    ///
    /// `claim_approvals` is the same bargain the host's `?approvals=true`
    /// makes: the host puts this conversation's consent questions to THIS
    /// socket as `TurnFrame::Prompt` and parks its executor until
    /// [`TurnSender::send_answer`] replies. Claiming installs the
    /// obligation — a claimed socket with no answer path turns the host's
    /// auto-answer into a hang — so [`Self::connect`] stays the unclaimed
    /// default and one-shot drains keep working exactly as before.
    pub async fn connect_with(
        &self,
        conversation_id: &str,
        options: StreamOptions,
    ) -> Result<TurnStream> {
        let url = self.stream_url(conversation_id, &options);
        let (socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| Error::Inference(format!("connect {url}: {e}")))?;

        // Split the socket, the daemon's own writer shape (turn_http):
        // one writer task fed by an unbounded channel, the reader kept by
        // the stream. Before this split (sv-surface R2/G11) `next_frame`
        // held `&mut self` across every read, so nothing else in the
        // process could put an `Answer` on the wire while a drain was
        // running — exactly the shape a Tauri command needs to refuse.
        // Now the write half is a cloneable [`TurnSender`] that answers
        // mid-drain.
        let (sink, stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut sink = sink;
            while let Some(text) = rx.recv().await {
                if sink.send(WsMessage::Text(text.into())).await.is_err() {
                    break; // the host went away; the reader will see it
                }
            }
            // rx exhausted = every sender dropped = the stream is closing;
            // dropping the sink here closes the write half cleanly.
        });
        Ok(TurnStream {
            socket: stream,
            tx: TurnSender { tx },
        })
    }

    /// Drive ONE turn to completion — the client-side mirror of
    /// `sovereign_core::runtime::serve_turn`.
    ///
    /// Connects, sends the message, forwards every frame to `observer`, and
    /// returns what the turn produced. A `StreamError` frame becomes an
    /// `Err`, because a client that received tokens and then nothing cannot
    /// tell a finished turn from a dead one (ARCH §18.3) — and neither can a
    /// caller that got an `Ok` with a half-written answer in it.
    pub async fn run_turn(
        &self,
        conversation_id: &str,
        content: &str,
        mode: TurnMode,
        intent: Option<Intent>,
        observer: &mut TurnObserver<'_>,
    ) -> Result<TurnOutcome> {
        let mut stream = self.connect(conversation_id).await?;
        stream.send_message(content, mode, intent).await?;
        stream.drain_turn(observer).await
    }
}

/// A host that scoped the conversation echoes the allow-list; one that
/// predates the field drops the unknown key and answers 200 with an
/// UNSCOPED row. The echo is the only way to tell them apart, and telling
/// them apart is the whole point: `--corpus` that silently did nothing is
/// the failure this field was added to close (§18.3).
fn verify_allow_list_echo(
    requested: Option<&[String]>,
    echoed: Option<&[String]>,
) -> std::result::Result<(), String> {
    match (requested, echoed) {
        (None, _) => Ok(()),
        (Some(want), Some(got)) if want == got => Ok(()),
        (Some(want), got) => Err(format!(
            "the host accepted the conversation but did not seed the corpus allow-list \
             {want:?} (it echoed {got:?}) — it predates `enabled_corpora` support or ignored \
             it, and NOTHING was scoped. Rebuild and restart the daemon (`svrn daemon stop && \
             svrn daemon start`), or drop --corpus."
        )),
    }
}

/// The `POST /v1/conversations` body. Pure so the wire shape is pinned by a
/// test: `enabled_corpora` is OMITTED when `None`, so a client that never
/// scopes sends byte-for-byte what it sent before the field existed — the
/// same discipline `TurnMode` keeps on `TurnRequest`.
fn create_conversation_body(
    skill_id: Option<&str>,
    enabled_corpora: Option<&[String]>,
) -> serde_json::Value {
    let mut body = serde_json::json!({ "skill_id": skill_id });
    if let Some(allow) = enabled_corpora {
        body["enabled_corpora"] = serde_json::json!(allow);
    }
    body
}

// ─── The CRUD wire envelopes, private parse shapes ─────────────────
//
// Both hosts (sovereign-server and the daemon's turn_http) answer these
// envelopes field-for-field — that is sv-surface 5c's compat bar. They are
// private and Deserialize-only: what a caller receives are the domain values
// above, and the shapes are pinned by tests against fixture bytes rather
// than trusted from either host.

#[derive(Debug, Deserialize)]
struct ConversationListWire {
    conversations: Vec<ConversationListEntryWire>,
}

#[derive(Debug, Deserialize)]
struct ConversationListEntryWire {
    id: String,
    title: Option<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Deserialize)]
struct ConversationWire {
    id: String,
    title: Option<String>,
    messages: Vec<ConversationMessageWire>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Deserialize)]
struct ConversationMessageWire {
    id: String,
    role: String,
    content: String,
    created_at: i64,
    #[serde(default)]
    provenance: Option<Provenance>,
    #[serde(default)]
    citations: Vec<Citation>,
    #[serde(default)]
    epistemic_state: Option<EpistemicState>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SearchResponseWire {
    #[serde(default)]
    results: Vec<SearchedMessageWire>,
}

#[derive(Debug, Deserialize)]
struct SearchedMessageWire {
    content: String,
    conversation_id: String,
}

#[derive(Debug, Deserialize)]
struct WeakenResponseWire {
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct InsightListWire {
    #[serde(default)]
    insights: Vec<InsightEntry>,
}

#[derive(Debug, Deserialize)]
struct ClipResponseWire {
    insight: InsightEntry,
}

#[derive(Debug, Deserialize)]
struct MessageResponseWire {
    message_id: String,
    #[serde(default)]
    role: String,
    content: String,
    #[serde(default)]
    task: Option<sovereign_contracts::types::projection::TaskSummary>,
    #[serde(default)]
    provenance: Option<Provenance>,
    #[serde(default)]
    citations: Vec<Citation>,
    #[serde(default)]
    epistemic_state: Option<EpistemicState>,
}

/// Read a response to `(status, body text)`. Shared by every CRUD method so
/// the error shape is spelled once.
async fn read_body(resp: reqwest::Response, url: &str) -> Result<(reqwest::StatusCode, String)> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| Error::Inference(format!("{url}: reading body: {e}")))?;
    Ok((status, body))
}

/// A non-success answer becomes the HOST's words, not a generic status line
/// — the same bargain `create_conversation` makes. Both hosts wrap errors as
/// `{"error": "..."}`; unwrap that, falling back to the raw body.
fn host_words(status: reqwest::StatusCode, body: &str, ctx: &str) -> Error {
    let reason = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    Error::Inference(format!("{ctx}: {status}: {reason}"))
}

/// `{"projects": [...]}` — the envelope `GET /v1/features/projects`
/// answers in.
/// The nodes `POST /v1/insights/by-id` resolved, and the ids it could
/// not. Mirrored (not generic) because [`InsightEntry`] already is:
/// the same bytes, one more field.
#[derive(Debug, Clone, Deserialize)]
pub struct InsightsById {
    pub insights: Vec<InsightEntry>,
    /// Ids that named no live insight — deleted, or never there. Empty on
    /// the happy path, and always present so a caller never has to infer
    /// an absence from a short list.
    pub missing: Vec<String>,
}

#[derive(Deserialize)]
struct OpIdsWire {
    op_ids: Vec<String>,
}

#[derive(Deserialize)]
struct OpIdWire {
    op_id: String,
}

#[derive(Deserialize)]
struct SeededWire {
    seeded: u32,
}

#[derive(Deserialize)]
struct RecipePathWire {
    path: String,
}

#[derive(Deserialize)]
struct ToolCountWire {
    tool_count: usize,
}

#[derive(Deserialize)]
struct LinkRecentWire {
    artifact_id: Option<String>,
}

#[derive(Deserialize)]
struct PreludeWire {
    prelude: String,
}

#[derive(Deserialize)]
struct ProjectListWire<T> {
    projects: Vec<T>,
}

/// `{"notes": [...]}` — the envelope `POST /v1/notes/query` answers in.
#[derive(Deserialize)]
struct NoteListWire<T> {
    notes: Vec<T>,
}

/// `{"id": "..."}` — what `POST /v1/notes` answers.
#[derive(Deserialize)]
struct CreatedIdWire {
    id: String,
}

/// `{"existed": bool}` — what retire / patch / delete answer. The flag
/// is NAMED rather than encoded as a status, because "the row was not
/// there" is a real answer to a delete and the caller should not have
/// to infer it (ARCH §18.3).
#[derive(Deserialize)]
struct AffectedWire {
    existed: bool,
}

/// `?node_type=&limit=` — the pair `meshapp_graph`, `meshapp_subgraph`
/// and `meshapp_search_entities` all take. Absent keys mean "the host's
/// default", which is where both defaults live.
fn node_list_query(node_type: Option<&str>, limit: Option<usize>) -> Vec<(&'static str, String)> {
    let mut q = Vec::new();
    if let Some(t) = node_type {
        q.push(("node_type", t.to_string()));
    }
    q.extend(limit_query(limit));
    q
}

/// `?limit=` on its own — `meshapp_claims` and `meshapp_questions`.
fn limit_query(limit: Option<usize>) -> Vec<(&'static str, String)> {
    limit
        .into_iter()
        .map(|n| ("limit", n.to_string()))
        .collect()
}

/// Parse a success body as `T`, naming the request in the error.
fn parse<T: serde::de::DeserializeOwned>(body: &str, ctx: &str) -> Result<T> {
    serde_json::from_str(body)
        .map_err(|e| Error::Inference(format!("{ctx}: malformed response: {e}")))
}

#[cfg(test)]
mod create_body_tests {
    use super::{create_conversation_body, verify_allow_list_echo};

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| x.to_string()).collect()
    }

    /// The stale-daemon shape: 200, an id, and no echo. Must be an error,
    /// because the alternative is a turn that searched everything under a
    /// flag that said otherwise.
    #[test]
    fn a_host_that_drops_the_allow_list_is_refused() {
        let err = verify_allow_list_echo(Some(&s(&["sep"])), None).unwrap_err();
        assert!(err.contains("NOTHING was scoped"), "{err}");
        assert!(err.contains("svrn daemon stop"), "{err}");
        let err = verify_allow_list_echo(Some(&s(&["sep"])), Some(&s(&["gutenberg"]))).unwrap_err();
        assert!(err.contains("[\"sep\"]"), "{err}");
    }

    #[test]
    fn a_faithful_echo_and_an_unscoped_create_both_pass() {
        verify_allow_list_echo(Some(&s(&["sep"])), Some(&s(&["sep"]))).unwrap();
        verify_allow_list_echo(None, None).unwrap();
        verify_allow_list_echo(None, Some(&s(&["sep"]))).unwrap();
    }

    #[test]
    fn an_unscoped_create_omits_the_allow_list_key() {
        let body = create_conversation_body(None, None);
        assert_eq!(body, serde_json::json!({ "skill_id": null }));
    }

    #[test]
    fn a_scoped_create_carries_the_allow_list_verbatim() {
        let allow = vec!["sep".to_string(), "gutenberg".to_string()];
        let body = create_conversation_body(Some("recipe-author"), Some(&allow));
        assert_eq!(
            body,
            serde_json::json!({
                "skill_id": "recipe-author",
                "enabled_corpora": ["sep", "gutenberg"],
            })
        );
    }
}

/// The CRUD parse shapes, pinned against fixture bytes that carry every
/// optional key a host may send — a drift in either host's envelope shows up
/// here as a parse failure rather than a silently-missing field (§18.3).
/// The rung-6 sibling envelopes, pinned against fixture bytes the daemon
/// serves — a drift in either host's envelope shows up here as a parse
/// failure rather than a silently-missing field (§18.3), the same bargain
/// the CRUD shapes above make.
#[cfg(test)]
mod rung6_wire_tests {
    use super::{
        parse, ClipResponseWire, InsightEntry, InsightListWire, SearchResponseWire,
        SearchedMessageWire, WeakenResponseWire,
    };

    #[test]
    fn a_full_search_response_parses() {
        let wire: SearchResponseWire = parse(
            r#"{"results":[{"content":"a.","conversation_id":"c1"},{"content":"b.","conversation_id":"c2"}]}"#,
            "test",
        )
        .unwrap();
        assert_eq!(wire.results.len(), 2);
        assert_eq!(wire.results[0].conversation_id, "c1");
        let _: Vec<SearchedMessageWire> = wire.results;
    }

    #[test]
    fn an_empty_search_response_parses() {
        let wire: SearchResponseWire = parse(r#"{"results":[]}"#, "test").unwrap();
        assert!(wire.results.is_empty());
    }

    #[test]
    fn a_weaken_response_carries_the_new_confidence() {
        let wire: WeakenResponseWire = parse(r#"{"confidence":0.4}"#, "test").unwrap();
        assert!((wire.confidence - 0.4).abs() < 1e-9);
    }

    #[test]
    fn a_full_insight_entry_parses() {
        let wire: InsightListWire = parse(
            r#"{"insights":[{"id":"u1","clipped_text":"t","message_id":"m1","paragraph_index":2,
                "source":{"corpus_id":"sep","article_title":"Free Will","conversation_id":"00000000-0000-0000-0000-0000000000f1"},
                "position":{"name":"Compatibilism","style":"Compatibilism"},
                "adjacent":["a"],"created_at":"2026-09-09T00:00:00Z","sink_state":"Local"}]}"#,
            "test",
        )
        .unwrap();
        let e: &InsightEntry = &wire.insights[0];
        assert_eq!(e.source.corpus_id.as_deref(), Some("sep"));
        assert_eq!(e.position.as_ref().unwrap().name, "Compatibilism");
    }

    #[test]
    fn a_clip_response_unwraps_the_entry() {
        let wire: ClipResponseWire = parse(
            r#"{"insight":{"id":"u2","clipped_text":"x","message_id":"m2","paragraph_index":0,
                "source":{"corpus_id":null,"article_title":null,"conversation_id":"00000000-0000-0000-0000-0000000000f2"},
                "adjacent":[],"created_at":"2026-09-09T00:00:00Z","sink_state":"Local"}}"#,
            "test",
        )
        .unwrap();
        assert_eq!(wire.insight.id, "u2");
        assert!(wire.insight.position.is_none());
    }
}

#[cfg(test)]
mod crud_wire_tests {
    use super::{parse, ConversationListWire, ConversationWire, MessageResponseWire};

    #[test]
    fn a_full_list_entry_parses() {
        let wire: ConversationListWire = parse(
            r#"{"conversations":[{"id":"alpha","title":"Free will","created_at":100,"updated_at":101}]}"#,
            "test",
        )
        .unwrap();
        let c = &wire.conversations[0];
        assert_eq!(c.id, "alpha");
        assert_eq!(c.title.as_deref(), Some("Free will"));
    }

    #[test]
    fn an_untitled_entry_parses_with_title_absent() {
        let wire: ConversationListWire = parse(
            r#"{"conversations":[{"id":"beta","created_at":100,"updated_at":100}]}"#,
            "test",
        )
        .unwrap();
        assert_eq!(wire.conversations[0].title, None);
    }

    #[test]
    fn a_full_conversation_with_projected_history_parses() {
        let wire: ConversationWire = parse(
            r#"{
                "id": "alpha",
                "title": "Free will",
                "created_at": 100,
                "updated_at": 101,
                "messages": [
                    {"id":"m1","role":"user","content":"q?","created_at":100},
                    {"id":"m2","role":"assistant","content":"a.","created_at":101,
                     "provenance":{"inference_backend":"test-provider"},
                     "citations":[{"corpus_id":"sep","chunk_id":"7","snippet":"s","score":0.9,"rank":0}],
                     "epistemic_state":{"version":1,"demands":[],"holdings":[],"gaps":[],"verdict":"grounded","citations":[]}}
                ]
            }"#,
            "test",
        )
        .unwrap();
        assert_eq!(wire.messages.len(), 2);
        assert_eq!(wire.messages[0].provenance, None);
        let m2 = &wire.messages[1];
        assert_eq!(
            m2.provenance.as_ref().unwrap().inference_backend,
            "test-provider"
        );
        assert_eq!(m2.citations[0].corpus_id, "sep");
        assert_eq!(m2.epistemic_state.as_ref().unwrap().version, 1);
    }

    #[test]
    fn a_full_message_response_parses_into_the_turn_outcome_fields() {
        let wire: MessageResponseWire = parse(
            r#"{
                "message_id": "m3",
                "role": "assistant",
                "content": "one two",
                "task": {"id": "t1", "status": "Running", "steps_completed": 2},
                "provenance": {"inference_backend": "test-provider"},
                "citations": [],
                "epistemic_state": null
            }"#,
            "test",
        )
        .unwrap();
        assert_eq!(wire.message_id, "m3");
        assert_eq!(wire.content, "one two");
        assert_eq!(wire.task.as_ref().unwrap().steps_completed, 2);
        // `null` epistemic_state degrades to None, matching the hosts'
        // projection contract.
        assert_eq!(wire.epistemic_state, None);
    }
}

/// One conversation's open turn stream.
///
/// Held separately from [`TurnClient`] because the host allows exactly one
/// in-flight turn per socket and refuses a second BY NAME rather than
/// queueing it — so the socket, not the client, is the thing whose state
/// matters to a caller.
///
/// The socket is SPLIT (sv-surface R2/G11): this struct owns the read
/// half, and the write half is a cloneable [`TurnSender`] — get it from
/// [`Self::sender`] — so an answer can go onto the wire while a drain is
/// mid-read. That is not a convenience: the desktop's Tauri commands
/// answer prompts from a different task than the one rendering tokens,
/// and before the split that shape could not be written against this
/// crate at all.
pub struct TurnStream {
    socket: futures::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    tx: TurnSender,
}

/// The write half of a turn socket — send requests, including the answers
/// to prompts, from ANY task that holds a clone (sv-surface R2/G11).
///
/// Cheap to clone (one channel sender). A send after the stream closed
/// fails by name; a send into a host that went gone surfaces at the next
/// read as the connection ending, which is where the reader already
/// looks.
#[derive(Debug, Clone)]
pub struct TurnSender {
    tx: mpsc::UnboundedSender<String>,
}

impl TurnSender {
    /// Queue one request onto the socket. THE write (ARCH §10.6) — the
    /// `send_*` methods differ only in the value they build.
    pub fn send(&self, req: TurnRequest) -> Result<()> {
        let text = serde_json::to_string(&req)
            .map_err(|e| Error::Serialization(format!("serializing TurnRequest: {e}")))?;
        self.tx
            .send(text)
            .map_err(|_| Error::Inference("turn socket writer is gone".to_string()))
    }

    /// Cancel the in-flight turn (G2). The host trips the turn's own
    /// cancellation, so the turn ends the way a cancelled turn always
    /// ended — its `Complete` carries `finish_reason: "cancelled"` —
    /// rather than dying to a task abort with no terminal frame.
    pub fn send_cancel(&self) -> Result<()> {
        self.send(TurnRequest::Cancel {})
    }

    /// Answer a parked [`TurnFrame::Prompt`] — the reply carrying the same
    /// `id` the question arrived with (G12's second half: a claimed socket
    /// now has something to answer WITH).
    pub fn send_answer(&self, id: &str, answer: &TurnAnswer) -> Result<()> {
        self.send(TurnRequest::Answer {
            id: id.to_string(),
            answer: answer.clone(),
        })
    }

    /// Ask for a turn.
    pub async fn send_message(
        &self,
        content: &str,
        mode: TurnMode,
        intent: Option<Intent>,
    ) -> Result<()> {
        self.send(TurnRequest::Message {
            content: content.to_string(),
            mode,
            intent,
        })
    }

    /// Continue an earlier turn under a picked intent — the wire form of
    /// `Runtime::resume_session_stream`, and what a clarification card's
    /// option click becomes once the surface holds no `Runtime`.
    ///
    /// A `session_id` the host no longer holds is not an error: sessions are
    /// dropped ~30s after their turn and the resume path reads nothing out of
    /// one. A `session_id` on a DIFFERENT conversation is refused by the host
    /// with a `TurnFrame::StreamError` naming it.
    pub async fn send_resume(
        &self,
        content: &str,
        session_id: &str,
        intent_hint: &str,
    ) -> Result<()> {
        self.send(TurnRequest::Resume {
            content: content.to_string(),
            session_id: session_id.to_string(),
            intent_hint: intent_hint.to_string(),
        })
    }

    /// Cancel the in-flight turn and re-answer the SAME message under a
    /// different intent — the wire form of `Runtime::redirect_turn_stream`.
    ///
    /// No `content` parameter, deliberately: the host re-answers the message
    /// the session already holds, so there is nothing here that could disagree
    /// with what was asked. The session must be live and on this stream's own
    /// conversation; both misses come back as a named `StreamError`.
    pub async fn send_redirect(&self, session_id: &str, intent_hint: &str) -> Result<()> {
        self.send(TurnRequest::Redirect {
            session_id: session_id.to_string(),
            intent_hint: intent_hint.to_string(),
        })
    }
}

impl TurnStream {
    /// The write half, cloneable — answer prompts mid-drain (G11).
    pub fn sender(&self) -> TurnSender {
        self.tx.clone()
    }

    /// Ask for a turn.
    pub async fn send_message(
        &mut self,
        content: &str,
        mode: TurnMode,
        intent: Option<Intent>,
    ) -> Result<()> {
        self.tx.send_message(content, mode, intent).await
    }

    /// Continue an earlier turn under a picked intent — the wire form of
    /// `Runtime::resume_session_stream`, and what a clarification card's
    /// option click becomes once the surface holds no `Runtime`.
    ///
    /// A `session_id` the host no longer holds is not an error: sessions are
    /// dropped ~30s after their turn and the resume path reads nothing out of
    /// one. A `session_id` on a DIFFERENT conversation is refused by the host
    /// with a `TurnFrame::StreamError` naming it.
    pub async fn send_resume(
        &mut self,
        content: &str,
        session_id: &str,
        intent_hint: &str,
    ) -> Result<()> {
        self.tx.send_resume(content, session_id, intent_hint).await
    }

    /// Cancel the in-flight turn and re-answer the SAME message under a
    /// different intent — the wire form of `Runtime::redirect_turn_stream`.
    ///
    /// No `content` parameter, deliberately: the host re-answers the message
    /// the session already holds, so there is nothing here that could disagree
    /// with what was asked. The session must be live and on this stream's own
    /// conversation; both misses come back as a named `StreamError`.
    pub async fn send_redirect(&mut self, session_id: &str, intent_hint: &str) -> Result<()> {
        self.tx.send_redirect(session_id, intent_hint).await
    }

    /// Read the next protocol frame, or `None` when the host hung up.
    ///
    /// Non-text frames are skipped rather than surfaced: ping/pong are the
    /// codec's business, and they are the reason the host spawns its turn
    /// instead of awaiting it inline.
    ///
    /// This is also the CONTINUOUS-read primitive: it has no opinion about
    /// turn boundaries, so a host that keeps talking after the terminal
    /// frame (the post-turn notices, G13) is read by staying in this loop
    /// — [`Self::drain_after_complete`] is the packaged spelling.
    pub async fn next_frame(&mut self) -> Result<Option<TurnFrame>> {
        while let Some(msg) = self.socket.next().await {
            let msg = msg.map_err(|e| Error::Inference(format!("turn stream: {e}")))?;
            let text = match msg {
                WsMessage::Text(t) => t.to_string(),
                WsMessage::Close(_) => return Ok(None),
                _ => continue,
            };
            let frame: TurnFrame = serde_json::from_str(&text)
                .map_err(|e| Error::Inference(format!("turn stream: unparseable frame: {e}")))?;
            return Ok(Some(frame));
        }
        Ok(None)
    }

    /// Read frames until the turn ends, forwarding each to `observer`.
    pub async fn drain_turn(&mut self, observer: &mut TurnObserver<'_>) -> Result<TurnOutcome> {
        let mut outcome = TurnOutcome::default();
        loop {
            let Some(frame) = self.next_frame().await? else {
                // The socket closed without a `Complete`. That is a dropped
                // turn, not an empty one — say so rather than returning the
                // partial text as if it were the answer.
                return Err(Error::Inference(
                    "turn stream closed before the turn completed".to_string(),
                ));
            };
            match frame {
                TurnFrame::Token { message_id, chunk } => {
                    outcome.message_id = message_id;
                    outcome.text.push_str(&chunk);
                    if let Some(f) = observer.on_token.as_deref_mut() {
                        f(&chunk);
                    }
                }
                TurnFrame::Narration {
                    phase,
                    text,
                    elapsed_ms,
                    ..
                } => {
                    if let Some(f) = observer.on_narration.as_deref_mut() {
                        f(&phase, &text, elapsed_ms);
                    }
                }
                TurnFrame::QueuePosition {
                    position,
                    estimated_wait_ms,
                } => {
                    if let Some(f) = observer.on_queue_position.as_deref_mut() {
                        f(position, estimated_wait_ms);
                    }
                }
                TurnFrame::Complete {
                    message_id,
                    provenance,
                    citations,
                    epistemic_state,
                    task,
                    metadata,
                } => {
                    outcome.message_id = message_id;
                    outcome.provenance = provenance;
                    outcome.citations = citations;
                    outcome.epistemic_state = epistemic_state;
                    outcome.task = task;
                    outcome.metadata = metadata;
                    return Ok(outcome);
                }
                // A host only sends a Prompt to the socket that CLAIMED
                // this conversation's approvals, and this drain installs
                // no answer path. Erroring names the gap; ignoring it
                // would park the host's turn on a decision nobody is
                // going to make (ARCH §18.3). The claimed spelling is
                // `next_frame` plus `Self::sender()`'s `send_answer`.
                TurnFrame::Prompt { id, prompt } => {
                    use sovereign_contracts::types::TurnPrompt;
                    let kind = match prompt {
                        TurnPrompt::Approval { .. } => "approve a step",
                        TurnPrompt::UserInput { .. } => "answer a question",
                        TurnPrompt::Information { .. } => "fill an information request",
                    };
                    return Err(Error::Inference(format!(
                        "turn stream: the host asked this client to {kind} (prompt id \
                         {id}), but this drain installs no answer path — read with \
                         next_frame and reply with TurnStream::sender().send_answer"
                    )));
                }
                TurnFrame::Notice { notice } => {
                    if let Some(f) = observer.on_notice.as_deref_mut() {
                        f(&notice);
                    }
                }
                TurnFrame::StreamError {
                    message,
                    retry_after_secs,
                } => {
                    // The shed case keeps its own variant: "the host is busy,
                    // come back in N seconds" and "the turn failed" are
                    // different things to a caller, and collapsing them into
                    // one error string is the §18.3 smell `QueueShed`'s own
                    // doc comment names.
                    return Err(match retry_after_secs {
                        Some(retry_after_secs) => Error::QueueShed {
                            position: 0,
                            predicted_wait_ms: retry_after_secs * 1000,
                            retry_after_secs,
                        },
                        None => Error::Inference(message),
                    });
                }
            }
        }
    }

    /// Read what the host says AFTER the terminal frame (sv-surface
    /// R2/G13) — the post-turn window.
    ///
    /// `MessageRefined` and `LessonProposed` fire after `Complete`
    /// (post-stream refinement, a detached capture spawn) and the socket
    /// stays open host-side until it closes, so "the turn ended" and
    /// "the host is done talking" are different moments. This drains
    /// the second: every `Notice` goes to `observer.on_notice`, and any
    /// non-`Notice` frame in the window is an error BY NAME — a token
    /// after the terminal frame is a protocol violation a caller must
    /// not swallow.
    ///
    /// It ends on `Notice::TurnSettled`, which is the host SAYING it has
    /// nothing further for this turn (sv-surface RB1); the notice is
    /// forwarded to the observer first, so a caller that wants to see the
    /// bookend can. A host closing the socket also ends it `Ok(())` —
    /// that was the ONLY ending until RB1, which is why the daemon leaked
    /// a socket per turn: nobody closed, so this never returned.
    ///
    /// Returning here does not end the socket. A caller that wants
    /// another turn sends the next request — inside the host's idle
    /// window, which is the daemon's `IDLE_AFTER_SETTLED`.
    pub async fn drain_after_complete(&mut self, observer: &mut TurnObserver<'_>) -> Result<()> {
        loop {
            let Some(frame) = self.next_frame().await? else {
                return Ok(());
            };
            match frame {
                TurnFrame::Notice { notice } => {
                    let settled = matches!(notice, TurnNotice::TurnSettled { .. });
                    if let Some(f) = observer.on_notice.as_deref_mut() {
                        f(&notice);
                    }
                    if settled {
                        return Ok(());
                    }
                }
                other => {
                    return Err(Error::Inference(format!(
                        "turn stream: unexpected frame after the turn completed: {other:?}"
                    )));
                }
            }
        }
    }
}

/// The R2 split, proven against a fake host on a real socket: the claim
/// rides the URL (G12), an answer flows while the reader is mid-drain
/// (G11), and the post-terminal window is readable (G13).
#[cfg(test)]
mod stream_split_tests {
    use futures::{SinkExt, StreamExt};
    use tokio::time::Duration;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::{StreamOptions, TurnAnswer, TurnClient, TurnFrame, TurnNotice, TurnObserver};

    /// `?approvals=true` appears when claimed and NOT otherwise — the
    /// unclaimed URL is byte-for-byte what every client sent before the
    /// field existed (the same discipline `TurnMode` keeps).
    #[test]
    fn a_claimed_url_carries_the_query_and_a_plain_one_does_not() {
        let client = TurnClient::new("http://127.0.0.1:9741");
        assert_eq!(
            client.stream_url("c1", &StreamOptions::default()),
            "ws://127.0.0.1:9741/v1/conversations/c1/stream",
            "the unclaimed URL must not carry a query"
        );
        assert_eq!(
            client.stream_url(
                "c1",
                &StreamOptions {
                    claim_approvals: true
                }
            ),
            "ws://127.0.0.1:9741/v1/conversations/c1/stream?approvals=true",
            "the claim is the host's own `StreamParams::approvals` spelling"
        );
    }

    /// THE G11 property, end to end: the host asks BEFORE streaming and
    /// will not stream until answered, the reader is inside `next_frame`
    /// when the answer goes out, and the answer is put on the wire by the
    /// SENDER — a handle the reader does not own. Before the split this
    /// test could not be written: `send_answer` did not exist, and any
    /// write would have needed `&mut` of the very stream being read.
    #[tokio::test]
    async fn an_answer_flows_while_the_reader_is_mid_drain() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let host = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            // Ask first. The preview is a minimal ActionPreview.
            ws.send(WsMessage::Text(
                r#"{"type":"prompt","data":{"id":"step:1","prompt":{"approval":{"preview":{"tool_id":"shell","description":"Run it","params":{"cmd":"ls"}}}}}}"#.into(),
            ))
            .await
            .unwrap();
            // Hold the turn until the answer arrives — the parked executor.
            let Some(Ok(answer)) = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("the host was answered rather than left parked")
            else {
                panic!("connection died before the answer");
            };
            // Then stream the turn to completion.
            ws.send(WsMessage::Text(
                r#"{"type":"token","data":{"message_id":"m1","chunk":"done"}}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"complete","data":{"message_id":"m1"}}"#.into(),
            ))
            .await
            .unwrap();
            answer.to_string()
        });

        let client = TurnClient::new(format!("http://{addr}"));
        let mut stream = client
            .connect_with(
                "c1",
                StreamOptions {
                    claim_approvals: true,
                },
            )
            .await
            .unwrap();
        // The write half, taken BEFORE the read loop — a clone the reader
        // does not hold.
        let sender = stream.sender();

        let mut answered: Vec<String> = Vec::new();
        let mut text = String::new();
        loop {
            let frame = stream
                .next_frame()
                .await
                .expect("the stream stays readable")
                .expect("the host closes only after Complete");
            match frame {
                TurnFrame::Prompt { id, .. } => {
                    // The reader is mid-loop here; the answer goes out
                    // through the sender alone.
                    sender
                        .send_answer(&id, &TurnAnswer::Approved(true))
                        .expect("the answer is queued while the reader runs");
                    answered.push(id);
                }
                TurnFrame::Token { chunk, .. } => text.push_str(&chunk),
                TurnFrame::Complete { .. } => break,
                _ => {}
            }
        }
        assert_eq!(answered, ["step:1"], "the prompt was answered by id");
        assert_eq!(text, "done");

        let on_the_wire = host.await.unwrap();
        assert!(
            on_the_wire.contains(r#""type":"answer""#),
            "the host received an Answer frame; got {on_the_wire}"
        );
        assert!(
            on_the_wire.contains(r#""id":"step:1""#) && on_the_wire.contains(r#""approved":true"#),
            "the answer echoes the prompt's id with the consent; got {on_the_wire}"
        );
    }

    /// G13: `MessageRefined` fires AFTER the terminal `Complete`, the
    /// socket stays open, and the post-turn window is readable to its end.
    #[tokio::test]
    async fn post_complete_notices_reach_the_observer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"complete","data":{"message_id":"m1"}}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"notice","data":{"notice":{"message_refined":{"conversation_id":"c1","message_id":"m1","new_content":"the revised answer."}}}}"#
                    .into(),
            ))
            .await
            .unwrap();
            // Host-side the socket closes when it is done talking; the
            // drain must read that as the end, not an error. (A CLEAN
            // close — a reset without the handshake is a different fact
            // and the client is right to surface it.)
            ws.close(None).await.unwrap();
        });

        let client = TurnClient::new(format!("http://{addr}"));
        let mut stream = client.connect("c1").await.unwrap();
        let mut observer = TurnObserver::default();
        let outcome = stream.drain_turn(&mut observer).await.unwrap();
        assert_eq!(outcome.message_id, "m1");

        let mut seen: Vec<String> = Vec::new();
        let mut on_notice = |n: &TurnNotice| {
            if let TurnNotice::MessageRefined(p) = n {
                seen.push(p.new_content.clone());
            }
        };
        let mut observer = TurnObserver {
            on_notice: Some(&mut on_notice),
            ..Default::default()
        };
        stream.drain_after_complete(&mut observer).await.unwrap();
        assert_eq!(
            seen,
            ["the revised answer.".to_string()],
            "the post-terminal notice reached the caller"
        );
    }

    /// sv-surface RB1: the drain ends on `TurnSettled` — the host SAYING it
    /// is done — and does not need the socket to close for it.
    ///
    /// That distinction is the whole leak: the only ending this had was a
    /// host close, the daemon never closed, and so every turn left a socket,
    /// a writer task and an approval channel alive on both ends. The fake
    /// host below deliberately STAYS OPEN after settling; before the fix
    /// this test hangs until its timeout rather than returning.
    #[tokio::test]
    async fn the_post_turn_drain_ends_when_the_host_says_the_turn_settled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let host = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"complete","data":{"message_id":"m1"}}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"notice","data":{"notice":{"message_refined":{"conversation_id":"c1","message_id":"m1","new_content":"the revised answer."}}}}"#
                    .into(),
            ))
            .await
            .unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"notice","data":{"notice":{"turn_settled":{"message_id":"m1"}}}}"#
                    .into(),
            ))
            .await
            .unwrap();
            // Still open, and staying open — the client must not need this.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });

        let client = TurnClient::new(format!("http://{addr}"));
        let mut stream = client.connect("c1").await.unwrap();
        let mut observer = TurnObserver::default();
        stream.drain_turn(&mut observer).await.unwrap();

        let mut seen: Vec<String> = Vec::new();
        let mut on_notice = |n: &TurnNotice| match n {
            TurnNotice::MessageRefined(p) => seen.push(p.new_content.clone()),
            TurnNotice::TurnSettled { message_id } => seen.push(format!("settled:{message_id}")),
            _ => {}
        };
        let mut observer = TurnObserver {
            on_notice: Some(&mut on_notice),
            ..Default::default()
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.drain_after_complete(&mut observer),
        )
        .await
        .expect("the drain ends on TurnSettled without waiting for a close")
        .expect("and ends cleanly");
        assert_eq!(
            seen,
            ["the revised answer.".to_string(), "settled:m1".to_string()],
            "the post-turn notices AND the bookend reached the caller, in order"
        );
        host.abort();
    }

    /// A non-Notice frame in the post-turn window is a protocol violation
    /// and is refused by name — a caller must not swallow a token that
    /// arrived after the terminal frame (§18.3).
    #[tokio::test]
    async fn an_unexpected_frame_after_complete_is_an_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"complete","data":{"message_id":"m1"}}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(WsMessage::Text(
                r#"{"type":"token","data":{"message_id":"m1","chunk":"?"}}"#.into(),
            ))
            .await
            .unwrap();
        });

        let client = TurnClient::new(format!("http://{addr}"));
        let mut stream = client.connect("c1").await.unwrap();
        let mut observer = TurnObserver::default();
        stream.drain_turn(&mut observer).await.unwrap();
        let err = stream
            .drain_after_complete(&mut observer)
            .await
            .expect_err("a token after Complete is refused");
        assert!(
            err.to_string()
                .contains("unexpected frame after the turn completed"),
            "the error names the violation; got {err}"
        );
    }
}

/// The exchange budget, proven on a real socket against a host that
/// misbehaves in the two ways that actually happen: it accepts and then
/// says nothing, or it answers slowly but never stops making progress.
///
/// Before 2026-09-11 the first case had no bound at all — `reqwest::Client`
/// carries no default timeout — and every one of this client's callers
/// inherited the hang.
#[cfg(test)]
mod request_budget_tests {
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{RequestBudget, TurnClient};

    /// The route under test is any plain GET; `contribution_view` is the
    /// shortest one to spell. What is being proven is the exchange
    /// helper's bounds, not this route.
    async fn ask(client: &TurnClient) -> super::Result<Vec<serde_json::Value>> {
        client.contribution_view::<Vec<serde_json::Value>>().await
    }

    /// Read the request line and headers so the client's write completes,
    /// then hand the caller the socket to answer on — or not.
    async fn accept_one(listener: TcpListener) -> tokio::net::TcpStream {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await.unwrap();
        sock
    }

    #[tokio::test]
    async fn a_host_that_accepts_and_then_says_nothing_is_an_error_not_a_hang() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let sock = accept_one(listener).await;
            // Hold the connection open and answer NOTHING. This is the
            // shape a wedged daemon has: the port is bound, the accept
            // succeeds, no bytes ever come back.
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(sock);
        });

        let client = TurnClient::with_budget(
            base,
            RequestBudget {
                connect: Duration::from_secs(2),
                read: Duration::from_millis(300),
                total: None,
            },
        );
        let started = Instant::now();
        let outcome = tokio::time::timeout(Duration::from_secs(5), ask(&client))
            .await
            .expect("the read bound must end the exchange; a hang here is the defect itself");
        assert!(
            outcome.is_err(),
            "a silent host is an error, never an empty answer"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the read bound should have fired in ~300ms, took {:?}",
            started.elapsed()
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_slow_but_progressing_host_outlives_the_read_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut sock = accept_one(listener).await;
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
            // Three gaps, each under the read bound, summing to more than
            // it: this is the route that takes minutes and is healthy.
            for chunk in [&b"1\r\n[\r\n"[..], &b"1\r\n]\r\n"[..], &b"0\r\n\r\n"[..]] {
                tokio::time::sleep(Duration::from_millis(250)).await;
                sock.write_all(chunk).await.unwrap();
                sock.flush().await.unwrap();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let client = TurnClient::with_budget(
            base,
            RequestBudget {
                connect: Duration::from_secs(2),
                read: Duration::from_millis(600),
                total: None,
            },
        );
        let started = Instant::now();
        let rows = tokio::time::timeout(Duration::from_secs(5), ask(&client))
            .await
            .expect("no hang")
            .expect("a host making progress is not a timeout");
        assert!(rows.is_empty());
        assert!(
            started.elapsed() > Duration::from_millis(600),
            "the exchange must have outlasted the read bound to prove anything; took {:?}",
            started.elapsed()
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_total_ceiling_cuts_what_the_read_bound_would_have_allowed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut sock = accept_one(listener).await;
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
            for chunk in [&b"1\r\n[\r\n"[..], &b"1\r\n]\r\n"[..], &b"0\r\n\r\n"[..]] {
                tokio::time::sleep(Duration::from_millis(250)).await;
                sock.write_all(chunk).await.unwrap();
                sock.flush().await.unwrap();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        // Same host, same read bound as the test above — only `total`
        // differs, so a failure here can be nothing else.
        let client = TurnClient::with_budget(
            base,
            RequestBudget {
                connect: Duration::from_secs(2),
                read: Duration::from_millis(600),
                total: Some(Duration::from_millis(300)),
            },
        );
        let outcome = tokio::time::timeout(Duration::from_secs(5), ask(&client))
            .await
            .expect("no hang");
        assert!(
            outcome.is_err(),
            "the caller's own ceiling must cut an exchange the read bound allows"
        );
        server.abort();
    }

    #[test]
    fn the_default_budget_bounds_silence_and_ceilings_nothing() {
        let budget = TurnClient::new("http://127.0.0.1:9741").budget();
        assert_eq!(budget, RequestBudget::default());
        assert_eq!(budget.connect, Duration::from_secs(5));
        assert_eq!(budget.read, Duration::from_secs(60));
        assert!(
            budget.total.is_none(),
            "a universal ceiling over sixty routes would truncate the slow ones; \
             a caller that knows its route names its own"
        );
    }
}
