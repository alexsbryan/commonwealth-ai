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
//! which only works from inside the process that owns the store. Here the
//! answer arrives as a [`TurnOutcome`], because `Complete` is a value that
//! serializes.

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::projection::{Citation, Provenance};
use sovereign_contracts::types::{
    EpistemicState, Intent, NarrationPhase, TurnFrame, TurnMode, TurnRequest,
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
}

/// A connection to a serving host's turn surface.
#[derive(Debug, Clone)]
pub struct TurnClient {
    base: String,
    http: reqwest::Client,
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
    fn stream_url(&self, conversation_id: &str) -> String {
        let ws = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            format!("ws://{}", self.base)
        };
        format!("{ws}/v1/conversations/{conversation_id}/stream")
    }

    /// Open the turn stream for a conversation.
    pub async fn connect(&self, conversation_id: &str) -> Result<TurnStream> {
        let url = self.stream_url(conversation_id);
        let (socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| Error::Inference(format!("connect {url}: {e}")))?;
        Ok(TurnStream { socket })
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
pub struct TurnStream {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl TurnStream {
    /// Ask for a turn.
    pub async fn send_message(
        &mut self,
        content: &str,
        mode: TurnMode,
        intent: Option<Intent>,
    ) -> Result<()> {
        self.send(TurnRequest::Message {
            content: content.to_string(),
            mode,
            intent,
        })
        .await
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
        self.send(TurnRequest::Resume {
            content: content.to_string(),
            session_id: session_id.to_string(),
            intent_hint: intent_hint.to_string(),
        })
        .await
    }

    /// Cancel the in-flight turn and re-answer the SAME message under a
    /// different intent — the wire form of `Runtime::redirect_turn_stream`.
    ///
    /// No `content` parameter, deliberately: the host re-answers the message
    /// the session already holds, so there is nothing here that could disagree
    /// with what was asked. The session must be live and on this stream's own
    /// conversation; both misses come back as a named `StreamError`.
    pub async fn send_redirect(&mut self, session_id: &str, intent_hint: &str) -> Result<()> {
        self.send(TurnRequest::Redirect {
            session_id: session_id.to_string(),
            intent_hint: intent_hint.to_string(),
        })
        .await
    }

    /// Serialize one request onto the socket. THE write (ARCH §10.6) — the
    /// three `send_*` methods differ only in the value they build.
    async fn send(&mut self, req: TurnRequest) -> Result<()> {
        let text = serde_json::to_string(&req)
            .map_err(|e| Error::Serialization(format!("serializing TurnRequest: {e}")))?;
        self.socket
            .send(WsMessage::Text(text.into()))
            .await
            .map_err(|e| Error::Inference(format!("sending turn: {e}")))
    }

    /// Read the next protocol frame, or `None` when the host hung up.
    ///
    /// Non-text frames are skipped rather than surfaced: ping/pong are the
    /// codec's business, and they are the reason the host spawns its turn
    /// instead of awaiting it inline.
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
                // A host only sends these to the socket that CLAIMED this
                // conversation's approvals (`?approvals=1` on the upgrade),
                // and this reader installs no handler for them. Erroring names
                // the gap; ignoring it would park the host's turn on a
                // decision nobody is going to make (ARCH §18.3).
                TurnFrame::ApprovalRequest {
                    task_id, step_id, ..
                } => {
                    return Err(Error::Inference(format!(
                        "turn stream: the host asked to approve step {step_id} of task \
                         {task_id}, but this client did not claim approvals for the \
                         conversation"
                    )));
                }
                TurnFrame::UserInputRequest { task_id, .. } => {
                    return Err(Error::Inference(format!(
                        "turn stream: the host put a question to the user for task \
                         {task_id}, but this client did not claim approvals for the \
                         conversation"
                    )));
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
}
