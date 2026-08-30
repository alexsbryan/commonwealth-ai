// SPDX-License-Identifier: AGPL-3.0-or-later
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

/// A well-shaped empty response for early-return paths (bad request /
/// embedding unavailable) — keeps the endpoint's contract intact even
/// when it can't search.
fn empty_knowledge_response() -> KnowledgeSearchResponse {
    KnowledgeSearchResponse {
        results: Vec::new(),
        corpora_searched: Vec::new(),
        corpora_unavailable: Vec::new(),
        total_chunks_searched: None,
    }
}

pub async fn knowledge_search(
    State(state): State<AppState>,
    Json(mut request): Json<KnowledgeSearchRequest>,
) -> (StatusCode, Json<KnowledgeSearchResponse>) {
    let limit = request.effective_limit() as usize;

    // OICP thin-client search (v0.4 §6): a client MAY send only `query`
    // / `query_text` and let the host embed it — the host owns the embed
    // model, so a client built against the manifest alone need not embed.
    // When `query_embedding` is absent we embed the text here with the
    // SAME query-instruction prefix we advertise in the manifest (read
    // from the identical source routes_oicp uses), so the query vector
    // lands in the corpus's space. Mesh peers pre-embed (query_embedding
    // non-empty) and skip this; the vector we fill then fans out to peers
    // unchanged, so no hop re-embeds.
    if request.query_embedding.is_empty() {
        if request.query_text.trim().is_empty() {
            return (StatusCode::BAD_REQUEST, Json(empty_knowledge_response()));
        }
        let Some(local) = state.inner.local_inference.as_ref() else {
            tracing::warn!("knowledge search: text-only query but no local inference to embed it");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(empty_knowledge_response()),
            );
        };
        let prefix = state
            .inner
            .inference_store
            .get_local_embed_model()
            .map(|e| e.query_instruction_prefix)
            .unwrap_or_default();
        match local
            .embed(&format!("{prefix}{}", request.query_text))
            .await
        {
            Ok(v) => request.query_embedding = v,
            Err(e) => {
                tracing::warn!(error = %e, "knowledge search: host-side query embedding failed");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(empty_knowledge_response()),
                );
            }
        }
    }

    let self_id = *state.inner.self_node_id_swap.load_full().as_ref();

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
    //
    // Also collect a "peer roster" log payload — a per-member
    // (name, status, hosted_corpora) tuple that we emit at info
    // level so the operator can answer "why isn't my peer serving
    // hits?" without attaching a debugger. If you see your Founder
    // listed here with `corpora=[]`, that's the bug: nothing to
    // federate because no complete corpora are published from that
    // side. If they're missing from the roster entirely, gossip
    // hasn't converged yet.
    let (peer_offerings, target_corpora_if_unconstrained, peer_roster) = {
        let mesh = state.inner.mesh.read().await;
        let mut offerings: Vec<PeerOffering> = Vec::new();
        let mut union: HashSet<String> = local_corpora.clone();
        let mut roster: Vec<(String, String, Vec<String>)> = Vec::new();
        for (_, member) in mesh.members.iter() {
            if member.node_id == self_id {
                continue;
            }
            let corpora: Vec<String> = member
                .capabilities
                .hosted_corpora
                .iter()
                .map(|c| c.corpus_id.clone())
                .collect();
            roster.push((
                member.name.clone(),
                format!("{:?}", member.status),
                corpora.clone(),
            ));
            if !is_queryable(member) {
                continue;
            }
            for c in &corpora {
                union.insert(c.clone());
            }
            if !corpora.is_empty() {
                offerings.push(PeerOffering {
                    node_id: member.node_id,
                    node_name: member.name.clone(),
                    contact: commonwealth_transport::peer_contact(member),
                    corpora,
                });
            }
        }
        (offerings, union, roster)
    };

    tracing::info!(
        local_corpora = ?local_corpora,
        peer_roster = ?peer_roster,
        offerings = peer_offerings.len(),
        "knowledge: fan-out plan — peer roster & local view"
    );

    // Step 3: resolve the actual target set. If the caller passed
    // `corpora`, honour it; otherwise search every corpus reachable
    // on the mesh (local OR peer-hosted).
    // What the CALLER named, kept for the response envelope: `target_corpora`
    // below loses the distinction between "asked for these" and "asked for
    // everything", and the envelope needs it to tell an unserved corpus from
    // one that was never in scope.
    let requested_corpora: Option<Vec<String>> = match &request.corpora {
        Some(c) if !c.is_empty() => Some(c.clone()),
        _ => None,
    };
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
                                    // Provenance the SERVING index stamped,
                                    // forwarded rather than dropped (TOPOLOGY §10
                                    // rung 9.1). `stamped_custody` and not
                                    // `custody` so "this index recorded no class"
                                    // stays ABSENT on the wire rather than
                                    // becoming the string "unknown" — the
                                    // requester joins absence into a refusal.
                                    custody: r
                                        .provenance
                                        .stamped_custody()
                                        .map(|c| c.as_str().to_string()),
                                    grain: Some(r.provenance.grain().as_str().to_string()),
                                    content: r.content,
                                    title: r.title,
                                    corpus_id: corpus_id.clone(),
                                    url: r.url,
                                    score: r.score,
                                    metadata: HashMap::new(),
                                    chunk_id: r.chunk_id,
                                    source_doc_id: r.source_doc_id,
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
    let mut fanout_jobs: HashMap<
        NodeId,
        (String, commonwealth_transport::PeerContact, Vec<String>),
    > = HashMap::new();
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
                offering.contact.clone(),
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
                    requested_corpora.as_deref(),
                    limit,
                );
            }
        };

        let transport = state.peer_transport();
        let mut futures = Vec::new();
        for (node_id, (node_name, contact, corpora)) in fanout_jobs.into_iter() {
            let http = http.clone();
            let transport = transport.clone();
            let query_embedding = request.query_embedding.clone();
            let query_text = request.query_text.clone();
            let limit_u32 = request.effective_limit();
            let requester_id = self_id;
            let fanout_inner = state.inner.clone();
            futures.push(tokio::spawn(async move {
                // Hold the live fan-out gauge up for the lifetime of this peer
                // request; the RAII guard decrements even if the task is
                // cancelled or panics, so `BoundedFanOut` never sees a leak.
                let _fanout_guard = FanoutGuard::new(fanout_inner);
                fanout_one_peer(
                    http,
                    transport,
                    requester_id,
                    node_id,
                    node_name,
                    contact,
                    corpora,
                    query_embedding,
                    query_text,
                    limit_u32,
                )
                .await
            }));
        }

        let mut peers_succeeded = 0usize;
        let mut peers_failed = 0usize;
        for f in futures {
            match f.await {
                Ok(PeerOutcome::Served {
                    results,
                    corpora_served,
                    corpora_unavailable: peer_unavailable,
                }) => {
                    peers_succeeded += 1;
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
                    peers_failed += 1;
                    for c in failed_corpora {
                        corpora_unavailable.insert(c);
                    }
                }
                Err(e) => {
                    peers_failed += 1;
                    tracing::warn!(error = %e, "knowledge: fan-out join failed");
                }
            }
        }
        // Single-line summary the operator can grep for: if
        // peers_succeeded == 0 AND peers_failed > 0, every peer we
        // tried was unreachable — that's the AP-isolation /
        // stale-address failure mode. If peers_succeeded > 0 but
        // corpora_unavailable is non-empty, specific corpora were
        // missing on each peer tried.
        tracing::info!(
            peers_succeeded,
            peers_failed,
            corpora_unavailable = ?corpora_unavailable,
            "knowledge: fan-out complete"
        );
    }

    build_response(
        all_results,
        corpora_searched,
        corpora_unavailable,
        requested_corpora.as_deref(),
        limit,
    )
}

/// RAII gauge guard for `AppStateInner::fanout_inflight`: increments on
/// construction and decrements on drop, so the live count of outbound peer
/// fan-out requests is correct even if a spawned fan-out task panics or is
/// cancelled. One is held inside each fan-out task; the companion read is
/// [`AppState::fanout_inflight_count`], surfaced over HTTP via `glassbox_signals`
/// and asserted by the `BoundedFanOut` soak invariant.
struct FanoutGuard(std::sync::Arc<crate::state::AppStateInner>);
impl FanoutGuard {
    fn new(inner: std::sync::Arc<crate::state::AppStateInner>) -> Self {
        inner
            .fanout_inflight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(inner)
    }
}
impl Drop for FanoutGuard {
    fn drop(&mut self) {
        self.0
            .fanout_inflight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A peer's advertised knowledge offering, cloned out of the mesh
/// read-lock so fan-out can run without holding the lock.
struct PeerOffering {
    node_id: NodeId,
    node_name: String,
    contact: commonwealth_transport::PeerContact,
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
///
/// `requester_id` is THIS daemon's `self_node_id`. It rides on the
/// outbound `X-Node-Id` header so the peer's
/// `routes_internal::knowledge_search` can stamp emitted
/// `KnowledgeQueryServed` events with the correct `for_node`.
/// Without this stamp, peer-side ledger emission silently skips
/// every fan-out request (`parse_x_node_id` returns `None` →
/// emission gated off) and the §10 intra-mesh accounting contract
/// is broken in the most common case. The single-daemon test
/// `knowledge_served_e2e` exercises the post-stamp emission path;
/// the two-daemon `knowledge_fanout_e2e::fan_out_stamps_x_node_id_so_peer_emits_ledger`
/// test pins this header-stamping contract end-to-end.
async fn fanout_one_peer(
    http: reqwest::Client,
    transport: std::sync::Arc<dyn commonwealth_transport::PeerTransport>,
    requester_id: NodeId,
    node_id: NodeId,
    node_name: String,
    contact: commonwealth_transport::PeerContact,
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
    // Hex-encode the requester id for the `X-Node-Id` header.
    // Matches the format `commonwealth_api::headers::parse_x_node_id`
    // expects (32 hex chars, lowercase). One small allocation per
    // peer per query; the OICP wire shape already costs more.
    let requester_hex: String = requester_id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    // The transport resolves and orders the candidates (ranked
    // addresses, last-working promoted to the front). This retired
    // the fan-out-local copy of gossip's `last_working_address_cache`
    // — the transport is shared state in `AppState`, so knowledge
    // fan-out and gossip now feed the same reachability hint instead
    // of converging two duplicate caches.
    let endpoints = transport
        .endpoints(
            &contact,
            commonwealth_transport::TrafficClass::KnowledgeSearch,
        )
        .await;
    for ep in &endpoints {
        let url = format!("{}/internal/knowledge/search", ep.base_url);
        match http
            .post(&url)
            .header("X-Node-Id", &requester_hex)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<KnowledgeSearchResponse>().await {
                    Ok(parsed) => {
                        // Pin this endpoint as the preferred starting
                        // point for the next fan-out round.
                        transport.note_success(
                            node_id,
                            commonwealth_transport::TrafficClass::KnowledgeSearch,
                            ep,
                        );
                        tracing::info!(
                            peer = %node_id,
                            peer_name = %node_name,
                            addr = %ep.label,
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
                                r.metadata.insert("peer_name".into(), peer_tag_name.clone());
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
                            addr = %ep.label,
                            error = %e,
                            "knowledge: fan-out deserialise failed"
                        );
                    }
                }
            }
            Ok(resp) => {
                tracing::warn!(
                    peer = %node_id,
                    addr = %ep.label,
                    status = %resp.status(),
                    "knowledge: fan-out non-success status"
                );
            }
            Err(e) => {
                // Info-level (was debug) — this is the log the user
                // needs to see when "I can tell my Founder has SEP,
                // so why aren't we fetching it?" The common cause is
                // the advertised peer address being unreachable from
                // here (AP isolation, stale cached address, VPN down).
                tracing::info!(
                    peer = %node_id,
                    addr = %ep.label,
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
///
/// `requested` is the corpus set the CALLER named (`None` when the request was
/// unconstrained). Every named corpus that nobody searched is reported as
/// unavailable — see the note on the loop below.
fn build_response(
    mut all_results: Vec<KnowledgeResult>,
    corpora_searched: HashSet<String>,
    mut corpora_unavailable: HashSet<String>,
    requested: Option<&[String]>,
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

    // A NAMED CORPUS THAT NOBODY SEARCHED IS UNAVAILABLE, NOT EMPTY.
    //
    // The two loss families above only cover corpora somebody TRIED: a local
    // index that failed to open, or a peer that was dialed and did not serve.
    // A corpus the caller asked for that no live member advertises is in
    // neither — it never reached `fanout_jobs`, so the response came back
    // well-shaped, `results: []`, `corpora_unavailable: []`. Indistinguishable
    // from "searched it, found nothing".
    //
    // Measured 2026-08-29 on flight 49188146: a rented peer asked for `sep`,
    // the founder was switched to `query_sharing = false`, and the pod's next
    // query returned exactly that shape — zero hits, nothing flagged. The same
    // shape appears whenever the hosting peer simply goes offline mid-run,
    // which is the failure this whole path is supposed to make visible: a
    // measurement scored on a pool a corpus fell out of, reported as a number
    // (ARCH §18.3 — absence is REPORTED, never defaulted).
    //
    // Only for an EXPLICIT corpus list. An unconstrained request means "search
    // whatever the mesh can reach", and nothing can be missing from that.
    if let Some(named) = requested {
        for c in named {
            if !corpora_searched.contains(c) {
                corpora_unavailable.insert(c.clone());
            }
        }
    }

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
    // `is_dialable` accepts an iroh-only peer (pubkey + relay/direct,
    // no gossiped IP — the no-VPN case); the seam still decides the
    // KnowledgeSearch route per dial.
    matches!(m.status, NodeStatus::Online | NodeStatus::Busy) && m.is_dialable()
}
