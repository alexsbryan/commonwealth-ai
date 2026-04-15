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
use sovereign_core::traits::{MeshKnowledgeSource, MeshScoredChunk};

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
    ) -> Vec<MeshScoredChunk> {
        let body = KnowledgeSearchRequest {
            query_embedding: query_embedding.to_vec(),
            query_text: query_text.to_string(),
            corpora: None, // Let Commonwealth decide — it knows the
                           // mesh's hosted corpora, we don't here.
            limit: Some(limit as u32),
        };
        let url = format!("{}/v1/knowledge/search", self.base_url);
        let response = match self.http.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                // Transport failure (daemon not running, port not
                // bound, local firewall) — NEVER propagate.
                // Runtime degrades to local-only on an empty vec.
                tracing::debug!(
                    url = %url,
                    error = %e,
                    "mesh knowledge client: transport error"
                );
                return Vec::new();
            }
        };

        if !response.status().is_success() {
            // 503 = our own daemon has no CorpusEngine yet (common
            // during startup); other statuses are unexpected.
            tracing::debug!(
                url = %url,
                status = %response.status(),
                "mesh knowledge client: non-success status"
            );
            return Vec::new();
        }

        let parsed: KnowledgeSearchResponse = match response.json().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    url = %url,
                    error = %e,
                    "mesh knowledge client: malformed response"
                );
                return Vec::new();
            }
        };

        let total = parsed.results.len();
        let results: Vec<MeshScoredChunk> = parsed
            .results
            .into_iter()
            .map(|r| {
                // `/v1/knowledge/search` stashes peer attribution
                // in `metadata["peer_name"]` when the hit came from
                // a fan-out leg. Absent for locally-served hits.
                let peer_name = r.metadata.get("peer_name").cloned();
                MeshScoredChunk {
                    content: r.content,
                    title: r.title,
                    corpus_id: r.corpus_id,
                    url: r.url,
                    score: r.score,
                    peer_name,
                }
            })
            .collect();
        tracing::info!(
            hits = total,
            "mesh knowledge client: received"
        );
        results
    }
}
