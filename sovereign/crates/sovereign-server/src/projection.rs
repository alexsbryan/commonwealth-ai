//! Projection of persisted `Message.metadata` into the typed,
//! client-facing provenance + citation surface.
//!
//! The Runtime persists rich provenance into each assistant message's
//! `metadata` JSON — shape `{"provenance": ResponseProvenance,
//! "retrieved_chunks": [{title, corpus_id, url, snippet, score,
//! chunk_id, source_doc_id}], ...}` (see
//! `sovereign_core::runtime` streaming + synthesis persistence sites and
//! `sovereign_core::types::ResponseProvenance`).
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

use serde::Serialize;
use serde_json::Value;

/// Client-facing provenance — the spec's `RESPONSE_PROVENANCE`.
///
/// A reduction of `sovereign_core::types::ResponseProvenance`: the
/// fields the thin client needs to *show the work ran on the host/mesh*
/// (acceptance criterion 5), plus the per-corpus `sources` the routing
/// footer renders. Richer desktop-only fields (token budgets, finish
/// reason, context window) are intentionally dropped here — the desktop
/// reads the raw blob directly; the mobile contract is this subset.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Provenance {
    /// Model + serving node, e.g. `"Qwen3.5-9B.Q8_0 @ peer BeefyMac"`.
    /// Maps from `ResponseProvenance.inference_backend`.
    pub inference_backend: String,
    /// Coarse routing tier / intent label (e.g. `"KnowledgeQuery"`).
    /// Prefers `coarse_intent`, falling back to `intent`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_tier: Option<String>,
    /// Time-to-first-token (ms). `None` today — the runtime does not
    /// yet stamp TTFT on the persisted provenance (documented gap;
    /// populated once the streaming path captures it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    /// Total turn latency (ms). Maps from `total_latency_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
    /// Per-corpus retrieval origins (origin + count [+ peer]). Lets the
    /// client render "From <corpus> (N)" without re-deriving it from
    /// the citation list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ProvenanceSource>,
}

/// One retrieval origin within [`Provenance::sources`]. Mirrors the
/// load-bearing fields of `sovereign_core::types::SourceSummary`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProvenanceSource {
    pub origin: String,
    pub count: u64,
    /// Human-readable peer name when this corpus's hits were served by
    /// a mesh peer (e.g. `"BeefyMac"`); `None` for locally-hosted.
    #[serde(skip_serializing_if = "Option::is_none")]
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
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Citation {
    pub corpus_id: String,
    pub chunk_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub snippet: String,
    pub score: f64,
    /// 0-based retrieval rank — the chunk's position in the runtime's
    /// scored `retrieved_chunks` list (preserved even when earlier
    /// entries are filtered out for lacking a corpus handle).
    pub rank: usize,
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

    Some(Provenance {
        inference_backend,
        routing_tier,
        ttft_ms: None,
        total_ms,
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
    let chunk_id = non_empty_str(c.get("chunk_id"))?;
    let snippet = c
        .get("snippet")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let score = c.get("score").and_then(Value::as_f64).unwrap_or(0.0);
    let title = non_empty_str(c.get("title"));
    Some(Citation {
        corpus_id,
        chunk_id,
        title,
        snippet,
        score,
        rank,
    })
}

/// Return the string value only when it is present and non-empty.
fn non_empty_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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
                "inference_backend": "Qwen3.5-9B.Q8_0 @ peer BeefyMac",
                "total_latency_ms": 1234,
                "tokens_used": 42,
                "sources": [
                    {"origin": "sep", "count": 6, "from_peer": "BeefyMac"},
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
        assert_eq!(prov.inference_backend, "Qwen3.5-9B.Q8_0 @ peer BeefyMac");
        assert_eq!(prov.routing_tier.as_deref(), Some("LOOKUP")); // coarse preferred
        assert_eq!(prov.total_ms, Some(1234));
        assert_eq!(prov.ttft_ms, None);
        assert_eq!(prov.sources.len(), 2);
        assert_eq!(prov.sources[0].origin, "sep");
        assert_eq!(prov.sources[0].from_peer.as_deref(), Some("BeefyMac"));
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
}
