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
    /// rather than one untagged turn happening first.
    pub async fn create_conversation(&self, skill_id: Option<&str>) -> Result<CreatedConversation> {
        let url = format!("{}/v1/conversations", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "skill_id": skill_id }))
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
            return Err(Error::Inference(format!(
                "POST {url}: {status}: {}",
                body.trim()
            )));
        }

        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| Error::Inference(format!("POST {url}: malformed response: {e}")))?;
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::Inference(format!("POST {url}: response carried no id")))?
            .to_string();
        let created_at = v.get("created_at").and_then(|x| x.as_i64()).unwrap_or(0);
        Ok(CreatedConversation { id, created_at })
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
        let req = TurnRequest::Message {
            content: content.to_string(),
            mode,
            intent,
        };
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
                } => {
                    outcome.message_id = message_id;
                    outcome.provenance = provenance;
                    outcome.citations = citations;
                    outcome.epistemic_state = epistemic_state;
                    outcome.task = task;
                    return Ok(outcome);
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
