// SPDX-License-Identifier: AGPL-3.0-or-later
//! Projection of persisted `Message.metadata` into the typed,
//! client-facing provenance + citation surface.
//!
//! The Runtime persists rich provenance into each assistant message's
//! `metadata` JSON — shape `{"provenance": ResponseProvenance,
//! "retrieved_chunks": [{title, corpus_id, url, snippet, score,
//! chunk_id, source_doc_id}], ...}` (see
//! `sovereign_core::runtime` streaming + synthesis persistence sites and
//! [`crate::types::ResponseProvenance`]).
//!
//! The REST + WebSocket surfaces project that blob into a stable shape
//! matching the mobile data model (`MOBILE.md` → `RESPONSE_PROVENANCE`
//! + `CITATION`). This module is the single place that reads the
//! metadata blob, so the wire contract has one definition.
//!
//! **Graceful degradation is the contract**: handlers that don't
//! persist provenance (conation / commissive / metalingual / ask_move /
//! recipe_author) leave `metadata` without a `provenance` key, and the
//! projection returns `(None, vec![])`. A message with no metadata at
//! all (e.g. a user message) does the same. Callers must treat both
//! fields as optional.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Client-facing provenance — the spec's `RESPONSE_PROVENANCE`.
///
/// A reduction of [`crate::types::ResponseProvenance`]: the
/// fields the thin client needs to *show the work ran on the host/mesh*
/// (acceptance criterion 5), plus the per-corpus `sources` the routing
/// footer renders. Richer desktop-only fields (token budgets, finish
/// reason, context window) are intentionally dropped here — the desktop
/// reads the raw blob directly; the mobile contract is this subset.
///
/// NOT [`crate::types::epistemic::Provenance`], which is the
/// evidence-BASIS enum (Corpus/Memory/…); this is the mobile projection of
/// `ResponseProvenance` described above.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    /// Model + serving node, e.g. `"Qwen3.5-9B.Q8_0 @ peer mac-peer"`.
    /// Maps from `ResponseProvenance.inference_backend`.
    pub inference_backend: String,
    /// Coarse routing tier / intent label (e.g. `"KnowledgeQuery"`).
    /// Prefers `coarse_intent`, falling back to `intent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_tier: Option<String>,
    /// Time-to-first-token (ms). `None` today — the runtime does not
    /// yet stamp TTFT on the persisted provenance (documented gap;
    /// populated once the streaming path captures it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    /// Total turn latency (ms). Maps from `total_latency_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
    /// OpenAI-style finish reason ("stop", "length", "error", …). The
    /// mobile cutoff affordance keys on `"length"` to show the
    /// "response was cut off" chip + Continue button. Dropped from the
    /// "desktop-only" exclusion above precisely because the thin client
    /// has no other way to know the answer was truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// `max_tokens` budget the turn ran under — lets the cutoff chip
    /// say "hit the N-token limit". Maps from `max_tokens_budget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_budget: Option<u64>,
    /// Completion tokens generated (provider-reported or estimated).
    /// Maps from `completion_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    /// Per-corpus retrieval origins (origin + count [+ peer]). Lets the
    /// client render "From <corpus> (N)" without re-deriving it from
    /// the citation list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ProvenanceSource>,
}

/// One retrieval origin within [`Provenance::sources`]. Mirrors the
/// load-bearing fields of [`crate::types::SourceSummary`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvenanceSource {
    /// Corpus this slice of the retrieval came from (e.g. `"sep"`).
    pub origin: String,
    /// How many retrieved chunks it contributed.
    pub count: u64,
    /// Human-readable peer name when this corpus's hits were served by
    /// a mesh peer (e.g. `"mac-peer"`); `None` for locally-hosted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_peer: Option<String>,
}

/// Client-facing citation — the spec's `CITATION`. Carries the host's
/// `(corpus_id, chunk_id)` handle so the client can prove the answer is
/// grounded in an installed corpus (acceptance criterion 4) and render
/// the snippet on tap.
///
/// Only **corpus-grounded** retrieved chunks become citations: an entry
/// must carry both a non-empty `corpus_id` and a non-empty `chunk_id`.
/// Web-fetch results (which carry a `url` but no corpus handle) are not
/// emitted here — they are not citations into an installed corpus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Citation {
    /// Installed corpus the chunk belongs to.
    pub corpus_id: String,
    /// Handle for the chunk within that corpus.
    pub chunk_id: String,
    /// Document title, when the chunk carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The quoted text the client renders on tap.
    pub snippet: String,
    /// Retrieval score as the host ranked it.
    pub score: f64,
    /// 0-based retrieval rank — the chunk's position in the runtime's
    /// scored `retrieved_chunks` list (preserved even when earlier
    /// entries are filtered out for lacking a corpus handle).
    pub rank: usize,
    /// Source URL, when the chunk carries one.
    ///
    /// Added for phase 6: `svrn chat ask` prints it in the sources footer
    /// whose "whole point is diagnostic visibility", and a surface can only
    /// render what the protocol carries. Dropping it would have made
    /// converting that host to a client a quiet downgrade of the exact thing
    /// the host exists to show. Optional and `skip_serializing_if`, so a
    /// citation without one is byte-identical to before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Grounding tier the retrieval assigned this chunk. Same reason as
    /// [`Self::url`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_tier: Option<String>,
}

/// The background task a turn spawned, projected for a client.
///
/// A turn that runs the agentic path produces one; a plain chat turn does
/// not. It reached a client through exactly one door before phase 6 —
/// `sovereign-server`'s non-streaming REST route, which read `Response.task`
/// directly — while the STREAMING path, the door both apps actually use,
/// called the same handler, received the same `Response`, and kept only
/// `message.id` and `message.content`. The task was dropped on the floor, so
/// the same turn asked two ways reported different things.
///
/// It travels in the persisted metadata blob now, like `provenance`,
/// `citations` and `epistemic_state` — one mechanism, so both doors project
/// the same value rather than one of them knowing a fact the other cannot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSummary {
    /// Task id — also what progress events correlate on.
    pub id: String,
    /// Lifecycle state, rendered (`Running`, `Paused`, `Completed`, …).
    pub status: String,
    /// How many steps of the plan have finished.
    pub steps_completed: usize,
}

/// What the turn's persisted metadata says about HOW it was served —
/// the three facts the phase-6 typed projection dropped on the floor.
///
/// # Why it is three fields and not the blob
///
/// `svrn chat ask --format json` used to be a host: it ran the turn in
/// process, then went back to the store and read the row it had just
/// written. Phase 6 made it a surface and the answer now ARRIVES, which is
/// the right shape — but the projection it arrives through carries
/// `provenance`, `citations`, `epistemic_state` and `task`, and the blob
/// also holds three things nothing else on the wire can answer:
///
/// - `routed_intent` — WHICH route the turn took, by variant name. The
///   sibling `intent` key in the blob is a hardcoded path label and the
///   knowledge handler serves both `KnowledgeQuery` and `ComparisonQuery`,
///   so reading a route off answer prose was the alternative.
/// - `grounding_gate` — what the hold→verify→retry→abstain ladder did:
///   `action`, and on the citation exit `mode`, `quotes`, `located`,
///   `openable`.
/// - `stage_attribution` — the per-turn stack ledger ([`TurnStageLedger`]),
///   which stage spent the turn's time.
///
/// Shipping the WHOLE blob instead would have put `retrieved_chunks` (20
/// chunks on a normal turn) on every terminal frame, and duplicated
/// `provenance` and `epistemic_state`, which are already projected beside
/// this. Three named fields, and every one of them `Option` with
/// `skip_serializing_if`: **absent stays absent**. A turn that opened no
/// ledger has no `stage_attribution` key — not `null`, not `{}` — which is
/// the same rule [`TurnStageLedger`]'s own doc states, so "not measured" and
/// "measured, nothing to report" stay distinguishable (ARCH §18.3).
///
/// `grounding_gate` is a `Value` and that is DECLARED DEBT, not an
/// oversight: the gate has sixteen exits and each writes a different key
/// set (`runtime/grounding/inner.rs`), so a struct here would silently drop
/// whatever the exit of the day added. Typing it is its own piece of work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnMetadata {
    /// The route this turn actually took, by variant name (`DeepQuery`,
    /// `KnowledgeQuery`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_intent: Option<String>,
    /// The grounding gate's own glassbox block. Absent when the gate was
    /// off or out of scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_gate: Option<Value>,
    /// The per-turn stage attribution. Absent on any surface that did not
    /// open a ledger — never an empty ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_attribution: Option<crate::types::TurnStageLedger>,
}

impl TurnMetadata {
    /// True when this carries nothing. The projection returns `None` rather
    /// than an empty `TurnMetadata`, so a client cannot tell "the host does
    /// not send this" from "the turn had nothing to report" by mistake.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routed_intent.is_none()
            && self.grounding_gate.is_none()
            && self.stage_attribution.is_none()
    }
}

/// Project the three how-it-was-served facts out of the persisted blob.
///
/// `None` when the blob has none of them — see [`TurnMetadata::is_empty`].
/// A malformed `stage_attribution` degrades to absent rather than failing
/// the whole projection, the same bargain [`project_epistemic_state`]
/// makes: one bad key must not cost the caller the other two.
pub fn project_turn_metadata(meta: &Option<Value>) -> Option<TurnMetadata> {
    let meta = meta.as_ref()?;
    let non_null = |k: &str| -> Option<&Value> {
        match meta.get(k) {
            Some(v) if !v.is_null() => Some(v),
            _ => None,
        }
    };
    let out = TurnMetadata {
        routed_intent: non_null("routed_intent")
            .and_then(Value::as_str)
            .map(str::to_string),
        grounding_gate: non_null("grounding_gate").cloned(),
        stage_attribution: non_null("stage_attribution")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
    };
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Project the persisted task block, when the turn spawned one.
pub fn project_task(meta: &Option<Value>) -> Option<TaskSummary> {
    let t = meta.as_ref()?.get("task")?;
    Some(TaskSummary {
        id: t.get("id").and_then(Value::as_str)?.to_string(),
        status: t
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        steps_completed: t
            .get("steps_completed")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
    })
}

/// Project a persisted `Message.metadata` blob into the typed
/// `(provenance, citations)` surface. The single reader of the metadata
/// shape; see the module docs for the contract.
pub fn project_message_metadata(meta: &Option<Value>) -> (Option<Provenance>, Vec<Citation>) {
    let Some(meta) = meta else {
        return (None, Vec::new());
    };
    (project_provenance(meta), project_citations(meta))
}

/// Project the persisted `epistemic_state` blob into the typed ledger
/// (EPISTEMIC_STATE.md, initiative I2-C). The runtime stamps it on
/// ledger-bearing turns; it is absent on old messages and `null` when
/// the `SOVEREIGN_EPISTEMIC_STATE` kill switch is off — both degrade to
/// `None`. `EpistemicState` is `Deserialize`, so this is a direct typed
/// round-trip; a malformed blob degrades to `None` rather than failing
/// the whole projection. Mobile *rendering* stays deferred — this closes
/// the wire gap so the ledger reaches the REST + WS surfaces.
pub fn project_epistemic_state(
    meta: &Option<Value>,
) -> Option<crate::types::epistemic::EpistemicState> {
    let es = meta.as_ref()?.get("epistemic_state")?;
    if es.is_null() {
        return None;
    }
    serde_json::from_value(es.clone()).ok()
}

fn project_provenance(meta: &Value) -> Option<Provenance> {
    let prov = meta.get("provenance")?;
    // A `provenance` key that isn't an object is malformed metadata —
    // treat it as absent rather than panic.
    if !prov.is_object() {
        return None;
    }

    let inference_backend = prov
        .get("inference_backend")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // routing_tier: prefer the coarse classification, fall back to the
    // fine intent. Either may be absent on older messages.
    let routing_tier = prov
        .get("coarse_intent")
        .and_then(Value::as_str)
        .or_else(|| prov.get("intent").and_then(Value::as_str))
        .map(str::to_string);

    let total_ms = prov.get("total_latency_ms").and_then(Value::as_u64);

    let sources = prov
        .get("sources")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(project_source).collect())
        .unwrap_or_default();

    let finish_reason = prov
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let max_tokens_budget = prov.get("max_tokens_budget").and_then(Value::as_u64);
    let completion_tokens = prov.get("completion_tokens").and_then(Value::as_u64);

    Some(Provenance {
        inference_backend,
        routing_tier,
        ttft_ms: None,
        total_ms,
        finish_reason,
        max_tokens_budget,
        completion_tokens,
        sources,
    })
}

fn project_source(v: &Value) -> Option<ProvenanceSource> {
    let origin = v.get("origin").and_then(Value::as_str)?.to_string();
    let count = v.get("count").and_then(Value::as_u64).unwrap_or(0);
    let from_peer = v
        .get("from_peer")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(ProvenanceSource {
        origin,
        count,
        from_peer,
    })
}

fn project_citations(meta: &Value) -> Vec<Citation> {
    let Some(chunks) = meta.get("retrieved_chunks").and_then(Value::as_array) else {
        return Vec::new();
    };
    chunks
        .iter()
        .enumerate()
        .filter_map(|(rank, c)| project_citation(c, rank))
        .collect()
}

fn project_citation(c: &Value, rank: usize) -> Option<Citation> {
    // Corpus-grounded only: require both handle components, non-empty.
    let corpus_id = non_empty_str(c.get("corpus_id"))?;
    let chunk_id = chunk_id_str(c.get("chunk_id"))?;
    let snippet = c
        .get("snippet")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let score = c.get("score").and_then(Value::as_f64).unwrap_or(0.0);
    let title = non_empty_str(c.get("title"));
    let url = non_empty_str(c.get("url"));
    let provenance_tier = non_empty_str(c.get("provenance_tier"));
    Some(Citation {
        corpus_id,
        chunk_id,
        title,
        snippet,
        score,
        rank,
        url,
        provenance_tier,
    })
}

/// Return the string value only when it is present and non-empty.
fn non_empty_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Coerce a `chunk_id` to its string handle. The runtime persists
/// `retrieved_chunks[].chunk_id` as a **numeric** chunk index (e.g.
/// `1396570`), but a string handle (`"sep:free-will:3"`) is also valid.
/// Accept either; reject only absent / null / empty-string. Without this
/// every numeric-id chunk was dropped, so corpus-grounded answers surfaced
/// `sources` but zero clickable `citations`.
fn chunk_id_str(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_metadata() -> Value {
        json!({
            "streamed": true,
            "intent": "knowledge_query",
            "provenance": {
                "intent": "KnowledgeQuery",
                "coarse_intent": "LOOKUP",
                "search_method": "CorpusEngine",
                "inference_backend": "Qwen3.5-9B.Q8_0 @ peer mac-peer",
                "total_latency_ms": 1234,
                "tokens_used": 42,
                "sources": [
                    {"origin": "sep", "count": 6, "from_peer": "mac-peer"},
                    {"origin": "wikipedia", "count": 2}
                ]
            },
            "retrieved_chunks": [
                {"title": "Free Will", "corpus_id": "sep", "url": null,
                 "snippet": "Compatibilism holds that...", "score": 0.91,
                 "chunk_id": "sep:free-will:3", "source_doc_id": "free-will"},
                {"title": "Determinism", "corpus_id": "sep", "url": null,
                 "snippet": "Determinism is the thesis...", "score": 0.74,
                 "chunk_id": "sep:determinism:1"}
            ]
        })
    }

    #[test]
    fn projects_full_provenance_and_citations() {
        let (prov, cites) = project_message_metadata(&Some(full_metadata()));
        let prov = prov.expect("provenance present");
        assert_eq!(prov.inference_backend, "Qwen3.5-9B.Q8_0 @ peer mac-peer");
        assert_eq!(prov.routing_tier.as_deref(), Some("LOOKUP")); // coarse preferred
        assert_eq!(prov.total_ms, Some(1234));
        assert_eq!(prov.ttft_ms, None);
        assert_eq!(prov.sources.len(), 2);
        assert_eq!(prov.sources[0].origin, "sep");
        assert_eq!(prov.sources[0].from_peer.as_deref(), Some("mac-peer"));
        assert_eq!(prov.sources[1].from_peer, None);

        assert_eq!(cites.len(), 2);
        assert_eq!(cites[0].corpus_id, "sep");
        assert_eq!(cites[0].chunk_id, "sep:free-will:3");
        assert_eq!(cites[0].title.as_deref(), Some("Free Will"));
        assert!(cites[0].snippet.starts_with("Compatibilism"));
        assert_eq!(cites[0].rank, 0);
        assert_eq!(cites[1].rank, 1);
    }

    #[test]
    fn none_metadata_yields_nothing() {
        let (prov, cites) = project_message_metadata(&None);
        assert!(prov.is_none());
        assert!(cites.is_empty());
    }

    #[test]
    fn metadata_without_provenance_degrades_gracefully() {
        // A handler that persists only an intent tag (conation/commissive
        // style) — no provenance object, no retrieved_chunks.
        let meta = json!({ "intent": "conation" });
        let (prov, cites) = project_message_metadata(&Some(meta));
        assert!(prov.is_none());
        assert!(cites.is_empty());
    }

    #[test]
    fn falls_back_to_fine_intent_when_no_coarse() {
        let meta = json!({
            "provenance": { "intent": "SimpleQuery", "inference_backend": "local" }
        });
        let (prov, _) = project_message_metadata(&Some(meta));
        let prov = prov.unwrap();
        assert_eq!(prov.routing_tier.as_deref(), Some("SimpleQuery"));
        assert_eq!(prov.total_ms, None);
        assert!(prov.sources.is_empty());
    }

    #[test]
    fn web_only_chunks_are_not_citations() {
        // A web-fetch result carries a url but no corpus handle — it
        // must not surface as a corpus citation.
        let meta = json!({
            "provenance": { "inference_backend": "x" },
            "retrieved_chunks": [
                {"title": "News", "corpus_id": "", "url": "https://example.com",
                 "snippet": "...", "score": 0.5, "chunk_id": null},
                {"title": "Real", "corpus_id": "wikipedia",
                 "snippet": "grounded", "score": 0.8, "chunk_id": "wikipedia:42"}
            ]
        });
        let (_, cites) = project_message_metadata(&Some(meta));
        assert_eq!(cites.len(), 1, "only the corpus-grounded chunk");
        assert_eq!(cites[0].corpus_id, "wikipedia");
        assert_eq!(cites[0].chunk_id, "wikipedia:42");
        // rank preserves the original retrieved_chunks index (1), not the
        // post-filter position (0).
        assert_eq!(cites[0].rank, 1);
    }

    #[test]
    fn projects_epistemic_state_when_present() {
        let meta = json!({
            "provenance": { "inference_backend": "local" },
            "epistemic_state": {
                "version": 1,
                "demands": [],
                "holdings": [{
                    "claim": "The knife was a carving knife",
                    "provenance": { "corpus": { "corpus_id": "secret-agent", "chunk_id": null } },
                    "verification": "verified"
                }],
                "gaps": [],
                "verdict": "grounded"
            }
        });
        let ledger = project_epistemic_state(&Some(meta)).expect("ledger present");
        assert_eq!(ledger.version, 1);
        assert_eq!(ledger.holdings.len(), 1);
        assert_eq!(ledger.verdict, crate::types::TurnVerdict::Grounded);
    }

    #[test]
    fn epistemic_state_absent_or_null_degrades_to_none() {
        // Old message: no key at all.
        assert!(project_epistemic_state(&Some(json!({ "intent": "x" }))).is_none());
        // Kill switch off: the key is present but null.
        assert!(project_epistemic_state(&Some(json!({ "epistemic_state": null }))).is_none());
        // No metadata at all.
        assert!(project_epistemic_state(&None).is_none());
    }

    #[test]
    fn numeric_chunk_id_is_a_valid_citation() {
        // The runtime persists chunk_id as a numeric chunk index, not a
        // string — a corpus-grounded answer must still produce citations.
        let meta = json!({
            "provenance": { "inference_backend": "local" },
            "retrieved_chunks": [
                {"title": "Free Will", "corpus_id": "wikipedia",
                 "snippet": "Compatibilism holds...", "score": 0.91,
                 "chunk_id": 1396570}
            ]
        });
        let (_, cites) = project_message_metadata(&Some(meta));
        assert_eq!(cites.len(), 1, "numeric chunk_id must not be dropped");
        assert_eq!(cites[0].corpus_id, "wikipedia");
        assert_eq!(cites[0].chunk_id, "1396570");
        assert!(cites[0].snippet.starts_with("Compatibilism"));
    }
}
