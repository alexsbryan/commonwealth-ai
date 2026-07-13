// SPDX-License-Identifier: AGPL-3.0-or-later
//! Client for the desktop command bridge (`sovereign-desktop`'s
//! debug-only loopback HTTP surface, `src-tauri/src/command_bridge.rs`).
//!
//! Benches use this as an alternate **answer source**: instead of
//! building an in-process Runtime that delegates inference to the
//! daemon (the default "direct" transport), questions are dispatched
//! through the production desktop command surface — the same
//! `#[tauri::command]` handlers the UI invokes, running in a real
//! desktop process. Banks and scorers are reused untouched, so a score
//! delta between the two transports isolates the desktop layer (its
//! Runtime wiring, command glue, and embedded inference) rather than
//! bank or rubric drift.
//!
//! The desktop must be running with `SOVEREIGN_COMMAND_BRIDGE=1`
//! (the real-mode e2e harness's global-setup launches it that way, or
//! launch `target/debug/sovereign-desktop` by hand).

use serde::de::DeserializeOwned;
use serde_json::Value;

pub const DEFAULT_BRIDGE_URL: &str = "http://127.0.0.1:9745";

pub struct BridgeClient {
    base: String,
    http: reqwest::Client,
}

impl BridgeClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Fail fast with a actionable message when the bridge isn't up.
    pub async fn healthz(&self) -> Result<(), String> {
        let url = format!("{}/healthz", self.base);
        let resp = self.http.get(&url).send().await.map_err(|e| {
            format!(
                "desktop bridge unreachable at {url}: {e}\n\
                     Launch the desktop with SOVEREIGN_COMMAND_BRIDGE=1 \
                     (e.g. via the real-mode e2e global-setup) first."
            )
        })?;
        if !resp.status().is_success() {
            return Err(format!("bridge healthz returned {}", resp.status()));
        }
        Ok(())
    }

    /// Dispatch one Tauri command through the production invoke path.
    /// Rejections come back as the command's error value, same as the
    /// frontend `invoke()` sees.
    pub async fn invoke<T: DeserializeOwned>(
        &self,
        cmd: &str,
        args: Value,
        spec: &str,
    ) -> Result<T, String> {
        let resp = self
            .http
            .post(format!("{}/invoke", self.base))
            .header("x-sovereign-spec", spec)
            .json(&serde_json::json!({ "cmd": cmd, "args": args }))
            .send()
            .await
            .map_err(|e| format!("bridge invoke {cmd}: transport: {e}"))?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("bridge invoke {cmd}: bad response body: {e}"))?;
        if body["ok"].as_bool() != Some(true) {
            return Err(format!("bridge invoke {cmd} failed: {}", body["error"]));
        }
        serde_json::from_value(body["result"].clone())
            .map_err(|e| format!("bridge invoke {cmd}: result shape: {e}"))
    }

    /// Snapshot of the bridge's replay ring (`GET /events/recent`),
    /// optionally filtered to rows with `seq >= since_seq`. Used to
    /// await terminal events (message-complete) without racing the
    /// live SSE stream.
    pub async fn events_recent(&self, since_seq: u64) -> Result<Vec<EventRow>, String> {
        let body: Value = self
            .http
            .get(format!("{}/events/recent?since_seq={since_seq}", self.base))
            .send()
            .await
            .map_err(|e| format!("bridge events/recent: {e}"))?
            .json()
            .await
            .map_err(|e| format!("bridge events/recent: body: {e}"))?;
        serde_json::from_value(body["rows"].clone())
            .map_err(|e| format!("bridge events/recent: rows shape: {e}"))
    }

    /// Register an event name with the bridge's lazy listen_any layer
    /// so subsequent emissions land in the replay ring.
    pub async fn listen(&self, event: &str) -> Result<(), String> {
        self.http
            .post(format!("{}/listen", self.base))
            .json(&serde_json::json!({ "event": event }))
            .send()
            .await
            .map_err(|e| format!("bridge listen {event}: {e}"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EventRow {
    pub seq: u64,
    pub event: String,
    pub payload: Value,
}

/// Run one chat turn through the desktop's production path — fresh
/// conversation, sealed to `corpus` via the same
/// `set_conversation_enabled_corpora` command the corpus chip strip
/// uses, dispatched with `send_message_stream`, terminal text read
/// from the bridge's replay ring. Returns the same [`LiveAnswer`]
/// shape `run_live` produces so every downstream judge/check is
/// shared verbatim between transports.
///
/// `retrieved_chunk_texts` parity note: message metadata carries
/// 200-char snippets, but the deterministic chaos checks substring-
/// match signature quotes against FULL chunk text — so each citation
/// is resolved through `read_get_chunk` (the reading surface; the
/// sealed corpus is installed on the desktop, so resolution is local),
/// falling back to the snippet only if resolution fails.
///
/// Call `client.listen("message-complete")` once before the first
/// turn so completions land in the replay ring.
/// One bridge-dispatched turn: the transport-shared [`LiveAnswer`]
/// plus the raw message metadata (provenance, retrieved_chunks) for
/// callers that read routing/glassbox fields.
pub struct BridgeTurn {
    pub answer: super::live_runner::LiveAnswer,
    pub metadata: Value,
}

pub async fn run_bridge_live(
    client: &BridgeClient,
    corpus: Option<&str>,
    question: &str,
    spec: &str,
) -> Result<BridgeTurn, String> {
    let conv: Value = client
        .invoke("create_conversation", serde_json::json!({}), spec)
        .await?;
    let conv_id = conv["id"]
        .as_str()
        .ok_or("create_conversation returned no id")?
        .to_string();
    if let Some(corpus) = corpus {
        client
            .invoke::<Value>(
                "set_conversation_enabled_corpora",
                serde_json::json!({ "conversationId": conv_id, "enabledCorpora": [corpus] }),
                spec,
            )
            .await?;
    }

    let since_seq = client
        .events_recent(0)
        .await?
        .last()
        .map(|r| r.seq + 1)
        .unwrap_or(0);
    let started: Value = client
        .invoke(
            "send_message_stream",
            serde_json::json!({ "message": question, "conversationId": conv_id }),
            spec,
        )
        .await?;
    let message_id = started["message_id"]
        .as_str()
        .ok_or("send_message_stream returned no message_id")?
        .to_string();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let complete = loop {
        let rows = client.events_recent(since_seq).await?;
        if let Some(row) = rows.iter().find(|r| {
            r.event == "message-complete"
                && r.payload["message_id"].as_str() == Some(message_id.as_str())
        }) {
            break row.payload.clone();
        }
        if let Some(err) = rows.iter().find(|r| {
            r.event == "message-error"
                && r.payload["message_id"].as_str() == Some(message_id.as_str())
        }) {
            return Err(format!("desktop turn errored: {}", err.payload["message"]));
        }
        if std::time::Instant::now() > deadline {
            return Err("desktop turn did not complete within 300s".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    };

    let raw = complete["full_text"].as_str().unwrap_or_default();
    let mut chunk_texts = Vec::new();
    if let Some(chunks) = complete["metadata"]["retrieved_chunks"].as_array() {
        for c in chunks {
            let resolved: Option<Value> = match (c["corpus_id"].as_str(), c["chunk_id"].as_u64()) {
                (Some(cid), Some(chid)) => client
                    .invoke(
                        "read_get_chunk",
                        serde_json::json!({ "corpusId": cid, "chunkId": chid }),
                        spec,
                    )
                    .await
                    .ok()
                    .flatten(),
                _ => None,
            };
            match resolved.as_ref().and_then(|r| r["content"].as_str()) {
                Some(content) => chunk_texts.push(content.to_string()),
                None => {
                    if let Some(snippet) = c["snippet"].as_str() {
                        chunk_texts.push(snippet.to_string());
                    }
                }
            }
        }
    }

    let gate = &complete["metadata"]["grounding_gate"];
    Ok(BridgeTurn {
        answer: super::live_runner::LiveAnswer {
            visible: super::live_runner::strip_think(raw),
            retrieved_chunk_texts: chunk_texts,
            // The gate's own decision (+ debug-gated draft), recovered from the
            // bridge response metadata — the same signal as the in-process path.
            gate_action: gate["action"].as_str().map(str::to_string),
            draft: gate["draft"].as_str().map(str::to_string),
            // Carry the full bridge metadata on the answer too, so the parity
            // harness reads the SAME `LiveAnswer.metadata` channel for both
            // transports (BridgeTurn.metadata is kept for existing callers).
            metadata: complete["metadata"].clone(),
        },
        metadata: complete["metadata"].clone(),
    })
}
