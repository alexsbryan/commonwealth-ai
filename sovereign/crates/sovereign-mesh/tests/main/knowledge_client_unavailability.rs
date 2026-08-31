// SPDX-License-Identifier: AGPL-3.0-or-later
//! The §9.6 probe shape, as a lane.
//!
//! `research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md` §9.6
//! measured this on 2026-08-14: a request naming `maple-house` (hosted only on
//! a peer that was yielding to its local user) fanned out correctly, hit the
//! peer's 503, and the daemon logged
//! `corpora_unavailable={maple-house, commonwealth-ai-arch-principles,
//! commonwealth-ai-system-overview, proxy-cik0000012927, proxy-cik0000034088}`
//! — then returned five confidently-scored results (9.05–9.12) from
//! `sf-assessor-roll`, an unrelated local corpus of San Francisco parcel
//! records, with nothing in the response saying anything was missing.
//!
//! The failure was one line of omission: `MeshKnowledgeClient` parsed
//! `KnowledgeSearchResponse` and threw `corpora_unavailable` on the floor
//! (`sovereign/deploy/mesh/GROUND_TRUTH.md`: "The fan-out client discards
//! `corpora_searched`/`corpora_unavailable` and returns transport failure as
//! an empty vec"). These tests drive the real client over a real socket
//! against a stub daemon that replays exactly that response, and assert the
//! loss survives to the caller — which is what lets the answer surface name
//! it.
//!
//! Every case here is the SAME question: can the caller tell "the peers had
//! nothing" apart from "the peers refused"? Before this lane, it could not.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::routing::post;
use axum::{Json, Router};
use commonwealth_inference::oicp::{KnowledgeResult, KnowledgeSearchResponse};
use sovereign_core::traits::{MeshKnowledgeSource, UnavailabilityReason};
use sovereign_mesh::knowledge_client::MeshKnowledgeClient;

/// The five peer-only corpora from the §9.6 run, in the order the daemon
/// logged them.
const PEER_ONLY: [&str; 5] = [
    "maple-house",
    "commonwealth-ai-arch-principles",
    "commonwealth-ai-system-overview",
    "proxy-cik0000012927",
    "proxy-cik0000034088",
];

/// One of the five substituted hits: an unrelated local parcel-records
/// corpus, scored as confidently as a real answer.
fn substituted_hit(score: f32) -> KnowledgeResult {
    KnowledgeResult {
        // A substituted hit vouches for nothing.
        custody: None,
        grain: None,
        content: "Parcel 3721-014 · single-family · assessed value $1,204,000".into(),
        title: Some("SF Assessor Roll 2024".into()),
        corpus_id: "sf-assessor-roll".into(),
        url: None,
        score,
        metadata: HashMap::new(),
        chunk_id: Some(1),
        source_doc_id: None,
    }
}

/// Stand up a stub daemon on an ephemeral port that answers
/// `/v1/knowledge/search` with `body`, and return its base URL. The listener
/// lives as long as the spawned task, which the test's runtime drops at the
/// end of the test.
async fn stub_daemon(status: axum::http::StatusCode, body: KnowledgeSearchResponse) -> String {
    let app = Router::new().route(
        "/v1/knowledge/search",
        post(move || {
            let body = body.clone();
            async move { (status, Json(body)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn corpora() -> Vec<String> {
    PEER_ONLY.iter().map(|s| s.to_string()).collect()
}

/// THE §9.6 RED. The daemon knew all five corpora by name; the client used to
/// discard the list and hand the runtime five parcel records as if they were
/// the answer.
#[tokio::test]
async fn the_9_6_shape_carries_every_unavailable_corpus_to_the_caller() {
    let base = stub_daemon(
        axum::http::StatusCode::OK,
        KnowledgeSearchResponse {
            results: vec![
                substituted_hit(9.05),
                substituted_hit(9.07),
                substituted_hit(9.09),
                substituted_hit(9.11),
                substituted_hit(9.12),
            ],
            corpora_searched: vec!["sf-assessor-roll".into()],
            corpora_unavailable: corpora(),
            total_chunks_searched: None,
        },
    )
    .await;

    let client = MeshKnowledgeClient::new(base).expect("build client");
    let outcome = client
        .search("maple house", &[0.1, 0.2, 0.3], 10, Some(&corpora()))
        .await;

    assert_eq!(
        outcome.chunks.len(),
        5,
        "the substituted hits still arrive — this fix does not hide results, it labels them"
    );
    let lost: Vec<&str> = outcome
        .unavailable
        .iter()
        .map(|u| u.corpus_id.as_str())
        .collect();
    for expected in PEER_ONLY {
        assert!(
            lost.contains(&expected),
            "`{expected}` was unavailable and the caller must be told; got {lost:?}"
        );
    }
    assert!(
        outcome
            .unavailable
            .iter()
            .all(|u| u.reason == UnavailabilityReason::PeerUnreachable),
        "a corpus the daemon could not reach is a PEER loss, not a local readiness one"
    );
}

/// The §9.1.2 cause, at the client: the peer yields to its local user and
/// answers 503. Returning an empty vec here is an `Err` collapsed into a
/// success-shaped value (ARCH §18.3) — the caller cannot tell it from "the
/// peers genuinely had nothing".
#[tokio::test]
async fn a_503_reports_every_requested_corpus_rather_than_going_quiet() {
    let base = stub_daemon(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        KnowledgeSearchResponse::default(),
    )
    .await;

    let client = MeshKnowledgeClient::new(base).expect("build client");
    let outcome = client
        .search("maple house", &[0.1], 10, Some(&corpora()))
        .await;

    assert!(outcome.chunks.is_empty(), "a 503 serves nothing");
    let lost: Vec<&str> = outcome
        .unavailable
        .iter()
        .map(|u| u.corpus_id.as_str())
        .collect();
    assert_eq!(
        lost.len(),
        PEER_ONLY.len(),
        "every corpus we asked for went unsearched; got {lost:?}"
    );
}

/// Nothing is listening. The runtime is entitled to degrade to local-only —
/// it is not entitled to do so silently.
#[tokio::test]
async fn a_transport_failure_reports_rather_than_going_quiet() {
    // Bind and immediately drop, so the port is almost certainly free and
    // nothing is behind it.
    let dead = {
        let l = tokio::net::TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind");
        let addr = l.local_addr().expect("addr");
        drop(l);
        format!("http://{addr}")
    };

    let client = MeshKnowledgeClient::new(dead).expect("build client");
    let outcome = client
        .search("maple house", &[0.1], 10, Some(&corpora()))
        .await;

    assert!(outcome.chunks.is_empty());
    assert_eq!(
        outcome.unavailable.len(),
        PEER_ONLY.len(),
        "a dead daemon means every requested corpus went unsearched"
    );
}

/// THE NO-REGRESSION BAR. A healthy fan-out that lost nothing reports an
/// EMPTY list — not a `None`, not an omission, and above all not a phantom
/// loss that would put a "sources unavailable" line on a perfectly good
/// answer.
#[tokio::test]
async fn a_healthy_fanout_reports_no_losses_at_all() {
    let base = stub_daemon(
        axum::http::StatusCode::OK,
        KnowledgeSearchResponse {
            results: vec![substituted_hit(0.8)],
            corpora_searched: vec!["sf-assessor-roll".into()],
            corpora_unavailable: Vec::new(),
            total_chunks_searched: Some(1),
        },
    )
    .await;

    let client = MeshKnowledgeClient::new(base).expect("build client");
    let outcome = client
        .search("parcel 3721", &[0.1], 10, Some(&corpora()))
        .await;

    assert_eq!(outcome.chunks.len(), 1);
    assert!(
        outcome.unavailable.is_empty(),
        "nothing was lost, so nothing may be reported lost; got {:?}",
        outcome.unavailable
    );
}
