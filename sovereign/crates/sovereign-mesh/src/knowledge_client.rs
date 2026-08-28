// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MeshKnowledgeClient` — the Sovereign-side reqwest wrapper that
//! bridges `sovereign-core::Runtime` to the local Commonwealth
//! daemon's `/v1/knowledge/search` endpoint.
//!
//! Design invariant: Sovereign NEVER talks to a peer directly. It
//! talks to its own in-process Commonwealth daemon (usually
//! `http://127.0.0.1:9741`), which then fans out to peers over the
//! internal API. That keeps cross-mesh routing logic centralised in
//! Commonwealth — if you ever need to trace *why* a given SEP chunk
//! was returned, you read the Commonwealth daemon's logs, not
//! Sovereign's.
//!
//! Concrete use case: the Joiner types "is free will compatible with
//! determinism?". `Runtime::prepare_knowledge_context` embeds the
//! query, calls `self.mesh_knowledge.search(...)`, which this client
//! turns into a single HTTP POST. The response lists SEP passages
//! (from the Founder's index) annotated with `peer_node_id` /
//! `peer_name` in `metadata`. We unpack them into `MeshScoredChunk`s
//! so the Runtime can surface provenance without knowing what
//! `oicp-types` is called.
use std::time::Duration;

use async_trait::async_trait;
use commonwealth_inference::oicp::{KnowledgeSearchRequest, KnowledgeSearchResponse};
use sovereign_core::traits::{
    CorpusUnavailable, MeshKnowledgeSource, MeshScoredChunk, MeshSearchOutcome,
    UnavailabilityReason,
};

/// Every corpus the fan-out was ASKED for, reported unreachable.
///
/// The transport/status/parse failure paths below used to `return Vec::new()`
/// — an `Err` collapsed into a success-shaped value (ARCH §18.3), which is
/// exactly how a peer-only question came back answered from an unrelated
/// local corpus (§9.6). A failure now names what it lost. `corpora == None`
/// (unsealed broad-research fan-out) has no named set to report, so the
/// outcome is empty and the local leg stands alone — the honest bound of what
/// this seam can know.
fn all_requested_unreachable(corpora: Option<&[String]>) -> Vec<CorpusUnavailable> {
    corpora
        .unwrap_or_default()
        .iter()
        .map(|c| CorpusUnavailable::new(c.clone(), UnavailabilityReason::PeerUnreachable))
        .collect()
}

/// 6s end-to-end budget. The Commonwealth handler's per-peer
/// timeout is 3s, and it fans out in parallel, so 6s leaves headroom
/// for several peers + our own local search. Longer than that and
/// the user would notice the UI hanging; if they really want
/// slow/distant meshes, `GroundingConfig` in Commonwealth is the
/// place to tune that.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(6);

/// Talks to `http://<base_url>/v1/knowledge/search`. Cheap to clone.
pub struct MeshKnowledgeClient {
    http: reqwest::Client,
    base_url: String,
}

impl MeshKnowledgeClient {
    /// Construct a client that posts to the given base URL. In
    /// desktop bootstrap, that's `http://127.0.0.1:9741` — the
    /// client API port of our own embedded Commonwealth daemon.
    /// Returns `Err` only if the reqwest client itself fails to
    /// build (vanishingly rare in practice — usually bad TLS
    /// config, which doesn't apply to localhost HTTP).
    pub fn new(base_url: impl Into<String>) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder().timeout(CLIENT_TIMEOUT).build()?;
        Ok(Self {
            http,
            base_url: base_url.into(),
        })
    }
}

#[async_trait]
impl MeshKnowledgeSource for MeshKnowledgeClient {
    async fn search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
        corpora: Option<&[String]>,
    ) -> MeshSearchOutcome {
        let body = KnowledgeSearchRequest {
            query_embedding: query_embedding.to_vec(),
            query_text: query_text.to_string(),
            // Seal the fan-out to the conversation's enabled corpora.
            // `None` lets Commonwealth search every hosted corpus (the
            // broad-research case); when the conversation is sealed
            // (e.g. to a single novel) we pass the seal so the SOURCE
            // search is scoped — the difference between a 316-row search
            // and opening a 1.9M-row `wikipedia` index it would only
            // discard, which OOM-kills the daemon.
            corpora: corpora.map(|c| c.to_vec()),
            limit: Some(limit as u32),
        };
        let url = format!("{}/v1/knowledge/search", self.base_url);
        let response = match self.http.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                // Transport failure (daemon not running, port not
                // bound, local firewall) — NEVER propagate. The Runtime
                // degrades to local-only, but it degrades OUT LOUD: every
                // corpus we asked for is reported unreachable.
                let unavailable = all_requested_unreachable(corpora);
                tracing::warn!(
                    url = %url,
                    error = %e,
                    unavailable = unavailable.len(),
                    "mesh knowledge client: transport error — reporting requested corpora unavailable"
                );
                return MeshSearchOutcome {
                    chunks: Vec::new(),
                    unavailable,
                };
            }
        };

        if !response.status().is_success() {
            // 503 = our own daemon has no CorpusEngine yet (common
            // during startup), or a peer yielding to its local user;
            // other statuses are unexpected. Either way the corpora we
            // asked for were not searched — say so.
            let unavailable = all_requested_unreachable(corpora);
            tracing::warn!(
                url = %url,
                status = %response.status(),
                unavailable = unavailable.len(),
                "mesh knowledge client: non-success status — reporting requested corpora unavailable"
            );
            return MeshSearchOutcome {
                chunks: Vec::new(),
                unavailable,
            };
        }

        let parsed: KnowledgeSearchResponse = match response.json().await {
            Ok(p) => p,
            Err(e) => {
                let unavailable = all_requested_unreachable(corpora);
                tracing::warn!(
                    url = %url,
                    error = %e,
                    unavailable = unavailable.len(),
                    "mesh knowledge client: malformed response — reporting requested corpora unavailable"
                );
                return MeshSearchOutcome {
                    chunks: Vec::new(),
                    unavailable,
                };
            }
        };

        // The daemon computed this one function away from the response and
        // we used to throw it on the floor. It is the §9.6 red.
        let unavailable: Vec<CorpusUnavailable> = parsed
            .corpora_unavailable
            .iter()
            .map(|c| CorpusUnavailable::new(c.clone(), UnavailabilityReason::PeerUnreachable))
            .collect();
        let total = parsed.results.len();
        let results: Vec<MeshScoredChunk> = parsed
            .results
            .into_iter()
            .map(|r| {
                // `/v1/knowledge/search` stashes peer attribution
                // in `metadata["peer_name"]` when the hit came from
                // a fan-out leg. Absent for locally-served hits.
                let peer_name = r.metadata.get("peer_name").cloned();
                // The wire spelling is parsed HERE and nowhere else: one
                // boundary, the canonical parsers, and a typo reads as absent
                // rather than as a class (TOPOLOGY §10 rung 9.1).
                let custody = r
                    .custody
                    .as_deref()
                    .and_then(sovereign_contracts::types::Custody::parse_wire);
                let grain = r
                    .grain
                    .as_deref()
                    .and_then(sovereign_contracts::types::Grain::parse_wire);
                MeshScoredChunk {
                    content: r.content,
                    title: r.title,
                    corpus_id: r.corpus_id,
                    url: r.url,
                    score: r.score,
                    peer_name,
                    chunk_id: r.chunk_id,
                    source_doc_id: r.source_doc_id,
                    custody,
                    grain,
                }
            })
            .collect();
        tracing::info!(
            hits = total,
            unavailable = unavailable.len(),
            unavailable_corpora = ?unavailable.iter().map(|u| u.corpus_id.as_str()).collect::<Vec<_>>(),
            "mesh knowledge client: received"
        );
        MeshSearchOutcome {
            chunks: results,
            unavailable,
        }
    }
}
