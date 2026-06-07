//! Map the server's reduced provenance + citations into the `metadata`
//! blob shape the shared `@sovereign/chat-ui` `RoutingMeta` /
//! `SourceAttribution` components expect — the one place the spec's
//! `RESPONSE_PROVENANCE` + `CITATION` meet the reused desktop renderer.
//!
//! Desktop persists a rich `ResponseProvenance` and reads it as
//! `metadata.provenance` (`{ intent, sources, inference_backend,
//! total_latency_ms, ... }`) plus `metadata.retrieved_chunks`. The
//! mobile host only sends the reduced projection, so we reconstruct the
//! subset of that shape the renderer reads; absent fields stay absent
//! (the components treat them as optional).

use serde_json::{json, Value};

use crate::remote::dto::{CitationDto, ProvenanceDto};

/// Build the `metadata` object emitted on `message-complete` →
/// rendered by `RoutingMeta` (provenance footer) + `SourceAttribution`
/// (citation list / click-to-read).
pub fn metadata_blob(provenance: Option<&ProvenanceDto>, citations: &[CitationDto]) -> Value {
    let provenance_json = provenance.map(|p| {
        json!({
            // RoutingMeta reads `intent` for the tier label.
            "intent": p.routing_tier,
            "inference_backend": p.inference_backend,
            "total_latency_ms": p.total_ms,
            "ttft_ms": p.ttft_ms,
            // Cutoff legibility: AssistantMessage shows the "response was
            // cut off" chip + Continue button when finish_reason == "length".
            "finish_reason": p.finish_reason,
            "max_tokens_budget": p.max_tokens_budget,
            "completion_tokens": p.completion_tokens,
            "sources": p.sources.iter().map(|s| json!({
                "origin": s.origin,
                "count": s.count,
                "from_peer": s.from_peer,
            })).collect::<Vec<_>>(),
        })
    });

    // SourceAttribution renders citations from `retrieved_chunks`,
    // resolving clicks against (corpus_id, chunk_id).
    let retrieved_chunks: Vec<Value> = citations
        .iter()
        .map(|c| {
            json!({
                "title": c.title,
                "corpus_id": c.corpus_id,
                "chunk_id": c.chunk_id,
                "snippet": c.snippet,
                "score": c.score,
                "provenance_tier": "corpus",
            })
        })
        .collect();

    json!({
        "streamed": true,
        "provenance": provenance_json,
        "retrieved_chunks": retrieved_chunks,
    })
}
