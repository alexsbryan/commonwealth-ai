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

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::projection::{Citation, Provenance};
use sovereign_contracts::types::{
    EpistemicState, Intent, NarrationPhase, TurnAnswer, TurnFrame, TurnMode, TurnNotice,
    TurnRequest,
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

/// A connection to a serving host's turn surface.
#[derive(Debug, Clone)]
pub struct TurnClient {
    base: String,
    http: reqwest::Client,
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
    pub fn new(base: impl Into<String>) -> Self {
        let mut base = base.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            base,
            http: reqwest::Client::new(),
        }
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
        let url = format!("{}/v1/conversations", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&create_conversation_body(skill_id, enabled_corpora))
            .send()
            .await
            .map_err(|e| Error::Inference(format!("POST {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Inference(format!("POST {url}: reading body: {e}")))?;
        if !status.is_success() {
            // The host's own words, not a generic status line: a daemon that
            // serves no turns says so ("this daemon serves no turns
            // (mesh-admin)") and that is the sentence the operator needs.
            // Both hosts wrap it as `{"error": "..."}`; unwrap that so the
            // sentence reads as prose, and fall back to the raw body when
            // the shape is anything else.
            let reason = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| body.trim().to_string());
            return Err(Error::Inference(format!("POST {url}: {status}: {reason}")));
        }

        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| Error::Inference(format!("POST {url}: malformed response: {e}")))?;
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::Inference(format!("POST {url}: response carried no id")))?
            .to_string();
        let created_at = v.get("created_at").and_then(|x| x.as_i64()).unwrap_or(0);
        let echoed: Option<Vec<String>> = v
            .get("enabled_corpora")
            .and_then(|e| serde_json::from_value(e.clone()).ok());
        verify_allow_list_echo(enabled_corpora, echoed.as_deref())
            .map_err(|why| Error::Inference(format!("POST {url}: {why}")))?;
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
        let url = format!("{}/v1/conversations/{conversation_id}/end", self.base);
        let resp = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("POST {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Inference(format!(
                "POST {url}: {status}: {}",
                body.trim()
            )));
        }
        Ok(())
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
        let url = format!("{}/v1/conversations", self.base);
        let mut req = self.http.get(&url);
        if let Some(limit) = limit {
            req = req.query(&[("limit", limit)]);
        }
        if let Some(offset) = offset {
            req = req.query(&[("offset", offset)]);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("GET {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("GET {url}")));
        }
        let wire: ConversationListWire = parse(&body, &format!("GET {url}"))?;
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
        let url = format!("{}/v1/conversations/{conversation_id}", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("GET {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("GET {url}")));
        }
        let wire: ConversationWire = parse(&body, &format!("GET {url}"))?;
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
        let url = format!("{}/v1/conversations/{conversation_id}", self.base);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("DELETE {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("DELETE {url}")));
        }
        Ok(())
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
        let url = format!("{}/v1/conversations/{conversation_id}/messages", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await
            .map_err(|e| Error::Inference(format!("POST {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("POST {url}")));
        }
        let wire: MessageResponseWire = parse(&body, &format!("POST {url}"))?;
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
        let url = format!("{}/v1/conversations/search", self.base);
        let resp = self
            .http
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| Error::Inference(format!("GET {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("GET {url}")));
        }
        let wire: SearchResponseWire = parse(&body, &format!("GET {url}"))?;
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
        let url = format!("{}/v1/memories/{memory_id}", self.base);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("DELETE {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("DELETE {url}")));
        }
        Ok(())
    }

    /// `POST /v1/memories/{id}/weaken` — halve a memory's confidence with
    /// the standard decay floor. THE one decider for the halving lives in
    /// the host; this returns the new confidence so a caller can render it.
    pub async fn weaken_memory(&self, memory_id: &str) -> Result<f64> {
        let url = format!("{}/v1/memories/{memory_id}/weaken", self.base);
        let resp = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("POST {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("POST {url}")));
        }
        let wire: WeakenResponseWire = parse(&body, &format!("POST {url}"))?;
        Ok(wire.confidence)
    }

    /// `GET /v1/insights?limit=` — the insight collection, newest first.
    pub async fn list_insights(&self, limit: Option<usize>) -> Result<Vec<InsightEntry>> {
        let url = format!("{}/v1/insights", self.base);
        let mut req = self.http.get(&url);
        if let Some(limit) = limit {
            req = req.query(&[("limit", limit)]);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("GET {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("GET {url}")));
        }
        let wire: InsightListWire = parse(&body, &format!("GET {url}"))?;
        Ok(wire.insights)
    }

    /// `GET /v1/insights/search?q=` — full-text search over the collection.
    pub async fn search_insights(&self, query: &str) -> Result<Vec<InsightEntry>> {
        let url = format!("{}/v1/insights/search", self.base);
        let resp = self
            .http
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| Error::Inference(format!("GET {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("GET {url}")));
        }
        let wire: InsightListWire = parse(&body, &format!("GET {url}"))?;
        Ok(wire.insights)
    }

    /// `DELETE /v1/insights/{id}` — soft-delete a clip. 204 on success.
    pub async fn delete_insight(&self, insight_id: &str) -> Result<()> {
        let url = format!("{}/v1/insights/{insight_id}", self.base);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("DELETE {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("DELETE {url}")));
        }
        Ok(())
    }

    /// `POST /v1/insights/clip` — clip a passage. The host embeds, finds
    /// adjacent nodes, persists, and answers the created row (with the
    /// embedding stripped — the wire projection).
    pub async fn clip_insight(&self, clip: ClipInsight<'_>) -> Result<InsightEntry> {
        let url = format!("{}/v1/insights/clip", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "clipped_text": clip.clipped_text,
                "message_id": clip.message_id,
                "paragraph_index": clip.paragraph_index,
                "source": clip.source,
                "position": clip.position,
            }))
            .send()
            .await
            .map_err(|e| Error::Inference(format!("POST {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("POST {url}")));
        }
        let wire: ClipResponseWire = parse(&body, &format!("POST {url}"))?;
        Ok(wire.insight)
    }

    /// `POST /v1/notes/tool-outcome` — record a tool-decision outcome into
    /// the host's notes dossier. Fire-and-soft-fail by DESIGN at the call
    /// site (the in-process path skipped a missing NoteStore silently);
    /// this method reports the host's answer so the caller decides what is
    /// fatal, never the wire (§18.3).
    pub async fn notes_tool_outcome(&self, outcome: ToolOutcome<'_>) -> Result<()> {
        let url = format!("{}/v1/notes/tool-outcome", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({
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
            }))
            .send()
            .await
            .map_err(|e| Error::Inference(format!("POST {url}: {e}")))?;
        let (status, body) = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(host_words(status, &body, &format!("POST {url}")));
        }
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
    /// the second: every `Notice` goes to `observer.on_notice`, the
    /// host closing the socket ends it `Ok(())`, and any other frame in
    /// the window is an error BY NAME — a token after the terminal
    /// frame is a protocol violation a caller must not swallow.
    pub async fn drain_after_complete(&mut self, observer: &mut TurnObserver<'_>) -> Result<()> {
        loop {
            let Some(frame) = self.next_frame().await? else {
                return Ok(());
            };
            match frame {
                TurnFrame::Notice { notice } => {
                    if let Some(f) = observer.on_notice.as_deref_mut() {
                        f(&notice);
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
