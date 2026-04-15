//! `/v1/knowledge/search` — the mesh-aware knowledge endpoint.
//!
//! This is the point where a Joiner's local Sovereign runtime (via
//! `MeshKnowledgeClient`) discovers that the SEP corpus it doesn't
//! host is hosted by a peer, and actually retrieves passages from
//! that peer. The handler:
//!
//!   1. Searches our own local `CorpusEngine` for any requested
//!      corpus we host (cheap path, no HTTP).
//!   2. Walks the live mesh `MemberRecord`s — specifically each
//!      member's `capabilities.hosted_corpora` — to find peers that
//!      host corpora we don't, and fires `/internal/knowledge/search`
//!      at them in parallel.
//!   3. Merges results by score (with `(corpus_id, content)` dedupe
//!      so a replica doesn't double-count), truncates to `limit`,
//!      returns the union.
//!
//! Per-peer errors (timeout, 503, unreachable) are swallowed into
//! `corpora_unavailable` — one sleepy peer must not take the whole
//! query down.
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use commonwealth_inference::oicp::{
    KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse,
};

use crate::state::AppState;

/// Same budget we use for gossip — if a peer can't answer in 3s,
/// treat them as absent and move on. Keeps UI-latency bounded even
/// when the mesh has degraded connectivity.
const PEER_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn knowledge_search(
    State(state): State<AppState>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> (StatusCode, Json<KnowledgeSearchResponse>) {
    let limit = request.effective_limit() as usize;
    let self_id = state.inner.self_node_id;

    // Step 1: figure out what corpora are locally installed, keyed
    // by id. This drives the "search here vs. fan out" split.
    let local_corpora: HashSet<String> = match &state.inner.corpus_engine {
        Some(e) => e
            .installed_indexes()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|i| i.corpus_id)
            .collect(),
        None => HashSet::new(),
    };

    // Step 2: scan live mesh members for peers hosting additional
    // corpora. We clone what we need out of the lock so we can drop
    // the read before firing async HTTP calls.
    let (peer_offerings, target_corpora_if_unconstrained) = {
        let mesh = state.inner.mesh.read().await;
        let mut offerings: Vec<PeerOffering> = Vec::new();
        let mut union: HashSet<String> = local_corpora.clone();
        for (_, member) in mesh.members.iter() {
            if member.node_id == self_id {
                continue;
            }
            if !is_queryable(member) {
                continue;
            }
            let corpora: Vec<String> = member
                .capabilities
                .hosted_corpora
                .iter()
                .map(|c| c.corpus_id.clone())
                .collect();
            for c in &corpora {
                union.insert(c.clone());
            }
            if !corpora.is_empty() {
                offerings.push(PeerOffering {
                    node_id: member.node_id,
                    node_name: member.name.clone(),
                    addresses: member.addresses.clone(),
                    corpora,
                });
            }
        }
        (offerings, union)
    };

    // Step 3: resolve the actual target set. If the caller passed
    // `corpora`, honour it; otherwise search every corpus reachable
    // on the mesh (local OR peer-hosted).
    let target_corpora: HashSet<String> = match &request.corpora {
        Some(c) if !c.is_empty() => c.iter().cloned().collect(),
        _ => target_corpora_if_unconstrained,
    };

    // Step 4: search locally for target corpora we host.
    let local_targets: Vec<String> = target_corpora
        .iter()
        .filter(|c| local_corpora.contains(*c))
        .cloned()
        .collect();
    let mut all_results: Vec<KnowledgeResult> = Vec::new();
    let mut corpora_searched: HashSet<String> = HashSet::new();
    let mut corpora_unavailable: HashSet<String> = HashSet::new();

    if !local_targets.is_empty() {
        if let Some(engine) = state.inner.corpus_engine.as_ref() {
            for corpus_id in &local_targets {
                match engine.open_index_for_corpus(corpus_id).await {
                    Ok(index) => match index
                        .search(&request.query_embedding, &request.query_text, limit)
                        .await
                    {
                        Ok(results) => {
                            corpora_searched.insert(corpus_id.clone());
                            all_results.extend(results.into_iter().map(|r| {
                                KnowledgeResult {
                                    content: r.content,
                                    title: r.title,
                                    corpus_id: corpus_id.clone(),
                                    url: r.url,
                                    score: r.score,
                                    metadata: HashMap::new(),
                                }
                            }));
                        }
                        Err(e) => {
                            tracing::warn!(
                                corpus = corpus_id,
                                error = %e,
                                "knowledge: local search failed"
                            );
                            corpora_unavailable.insert(corpus_id.clone());
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            corpus = corpus_id,
                            error = %e,
                            "knowledge: open_index_for_corpus failed"
                        );
                        corpora_unavailable.insert(corpus_id.clone());
                    }
                }
            }
        }
    }

    // Step 5: fan out to peers. A peer is a fan-out candidate for a
    // corpus if they advertise it AND we either don't host it
    // locally OR want to broaden the hit set — for v1 we only fan
    // out for corpora WE DON'T HAVE. Broadening to replicas is a
    // future refinement once the merge-dedupe is proven.
    let mut fanout_jobs: HashMap<NodeId, (String, Vec<SocketAddr>, Vec<String>)> =
        HashMap::new();
    for offering in &peer_offerings {
        let relevant: Vec<String> = offering
            .corpora
            .iter()
            .filter(|c| target_corpora.contains(*c))
            .filter(|c| !local_corpora.contains(*c))
            .cloned()
            .collect();
        if relevant.is_empty() {
            continue;
        }
        fanout_jobs.insert(
            offering.node_id,
            (
                offering.node_name.clone(),
                offering.addresses.clone(),
                relevant,
            ),
        );
    }

    if !fanout_jobs.is_empty() {
        let http = match reqwest::Client::builder().timeout(PEER_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "knowledge: HTTP client build failed");
                // Return what we have locally; don't fail the whole
                // request over a transport construction error.
                return build_response(
                    all_results,
                    corpora_searched,
                    corpora_unavailable,
                    limit,
                );
            }
        };

        let mut futures = Vec::new();
        for (node_id, (node_name, addrs, corpora)) in fanout_jobs.into_iter() {
            let http = http.clone();
            let query_embedding = request.query_embedding.clone();
            let query_text = request.query_text.clone();
            let limit_u32 = request.effective_limit();
            futures.push(tokio::spawn(async move {
                fanout_one_peer(
                    http,
                    node_id,
                    node_name,
                    addrs,
                    corpora,
                    query_embedding,
                    query_text,
                    limit_u32,
                )
                .await
            }));
        }

        for f in futures {
            match f.await {
                Ok(PeerOutcome::Served {
                    results,
                    corpora_served,
                    corpora_unavailable: peer_unavailable,
                }) => {
                    for c in corpora_served {
                        corpora_searched.insert(c);
                    }
                    for c in peer_unavailable {
                        corpora_unavailable.insert(c);
                    }
                    all_results.extend(results);
                }
                Ok(PeerOutcome::Failed {
                    corpora_unavailable: failed_corpora,
                }) => {
                    for c in failed_corpora {
                        corpora_unavailable.insert(c);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "knowledge: fan-out join failed");
                }
            }
        }
    }

    build_response(all_results, corpora_searched, corpora_unavailable, limit)
}

/// A peer's advertised knowledge offering, cloned out of the mesh
/// read-lock so fan-out can run without holding the lock.
struct PeerOffering {
    node_id: NodeId,
    node_name: String,
    addresses: Vec<SocketAddr>,
    corpora: Vec<String>,
}

enum PeerOutcome {
    Served {
        results: Vec<KnowledgeResult>,
        corpora_served: Vec<String>,
        corpora_unavailable: Vec<String>,
    },
    Failed {
        corpora_unavailable: Vec<String>,
    },
}

/// Query a single peer. Try each of their advertised addresses in
/// order until one works (same "first reachable wins" policy as
/// gossip and the join handshake). Any successful response is
/// annotated with the peer's id/name so the caller's merge step
/// can surface attribution to the UI.
async fn fanout_one_peer(
    http: reqwest::Client,
    node_id: NodeId,
    node_name: String,
    addresses: Vec<SocketAddr>,
    corpora: Vec<String>,
    query_embedding: Vec<f32>,
    query_text: String,
    limit: u32,
) -> PeerOutcome {
    let body = KnowledgeSearchRequest {
        query_embedding,
        query_text,
        corpora: Some(corpora.clone()),
        limit: Some(limit),
    };
    for addr in &addresses {
        let url = format!("http://{addr}/internal/knowledge/search");
        match http.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<KnowledgeSearchResponse>().await {
                    Ok(parsed) => {
                        tracing::info!(
                            peer = %node_id,
                            peer_name = %node_name,
                            addr = %addr,
                            corpora = ?corpora,
                            hits = parsed.results.len(),
                            "knowledge: fan-out served"
                        );
                        let peer_tag_id = node_id.to_string();
                        let peer_tag_name = node_name.clone();
                        let results: Vec<KnowledgeResult> = parsed
                            .results
                            .into_iter()
                            .map(|mut r| {
                                r.metadata
                                    .insert("peer_node_id".into(), peer_tag_id.clone());
                                r.metadata
                                    .insert("peer_name".into(), peer_tag_name.clone());
                                r
                            })
                            .collect();
                        return PeerOutcome::Served {
                            results,
                            corpora_served: parsed.corpora_searched,
                            corpora_unavailable: parsed.corpora_unavailable,
                        };
                    }
                    Err(e) => {
                        tracing::warn!(
                            peer = %node_id,
                            addr = %addr,
                            error = %e,
                            "knowledge: fan-out deserialise failed"
                        );
                    }
                }
            }
            Ok(resp) => {
                tracing::warn!(
                    peer = %node_id,
                    addr = %addr,
                    status = %resp.status(),
                    "knowledge: fan-out non-success status"
                );
            }
            Err(e) => {
                tracing::debug!(
                    peer = %node_id,
                    addr = %addr,
                    error = %e,
                    "knowledge: fan-out transport error, trying next address"
                );
            }
        }
    }
    PeerOutcome::Failed {
        corpora_unavailable: corpora,
    }
}

/// Merge, dedupe, rerank, and wrap in the response envelope.
/// Dedupe key is `(corpus_id, content)` so a peer-hosted replica
/// doesn't double-count when the same chunk text shows up from two
/// sources. Score ties use the first-seen record.
fn build_response(
    mut all_results: Vec<KnowledgeResult>,
    corpora_searched: HashSet<String>,
    mut corpora_unavailable: HashSet<String>,
    limit: usize,
) -> (StatusCode, Json<KnowledgeSearchResponse>) {
    // Dedupe: keep the highest-scored entry for each (corpus, content).
    all_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut seen: HashSet<(String, String)> = HashSet::new();
    all_results.retain(|r| seen.insert((r.corpus_id.clone(), r.content.clone())));
    all_results.truncate(limit);

    // Corpora that appear in `unavailable` but also appear in
    // `searched` shouldn't be flagged — a replica found it even if a
    // primary missed.
    for c in &corpora_searched {
        corpora_unavailable.remove(c);
    }

    (
        StatusCode::OK,
        Json(KnowledgeSearchResponse {
            results: all_results,
            corpora_searched: corpora_searched.into_iter().collect(),
            corpora_unavailable: corpora_unavailable.into_iter().collect(),
            total_chunks_searched: None,
        }),
    )
}

/// A member is fan-out-worthy if we think they can answer us. We're
/// permissive with `Busy` (a node serving inference still answers
/// knowledge search cheaply) and strict with `Offline`.
fn is_queryable(m: &MemberRecord) -> bool {
    matches!(m.status, NodeStatus::Online | NodeStatus::Busy)
        && !m.addresses.is_empty()
}
