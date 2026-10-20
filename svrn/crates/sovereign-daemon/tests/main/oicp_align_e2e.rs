// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /oicp/v1/align` through the real client router (OICP v0.5 §2.3).
//!
//! The library is the conformance suite's own fixture
//! (`cmnwlth/crates/oicp-conformance/fixture/library.json`), stored the way
//! ingest stores it: each document's text through the one writer, its chunks
//! stamped with the text's name, an FTS index over them. The planted misquote
//! is the `evidence.align` case — one word changed — and it must come back as
//! exactly one `substituted` at that word's ranges, with `span.exact` the
//! text's own `[start, end)`.
//!
//! Watched red against a planted mapping that turned `Substituted` into an
//! `omitted` plus an `added` (see the commit that added this file).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use corpus_index::index::{CorpusIndex, DocumentRecord};
use kernel_types::{NodeId, Sha256Hash};
use oicp_types::evidence::{AlignResponse, DifferenceKind};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common::fixture_library::{digest_of, install, library, state_over, Doc, DIM};

async fn align(state: &AppState, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/oicp/v1/align")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55002".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = client_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// `s`'s code points `[a, b)`.
fn cp(s: &str, a: u64, b: u64) -> String {
    s.chars().skip(a as usize).take((b - a) as usize).collect()
}

/// Align's candidates come from lexical search, which compares no vectors,
/// so a corpus built at another embedding width than the loaded model's is
/// aligned against like any other. Red while the route opened it through the
/// query path, whose width gate listed it unavailable ("corpus unreadable").
#[tokio::test]
async fn a_corpus_built_at_another_embedding_width_is_aligned_against() {
    let lib = library();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &lib.corpus_id, &lib.documents, true).await;
    let embed: corpus_index::types::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; DIM]) }));
    let engine = crate::common::reading_double(indexes.clone(), embed)
        .on_texts_digest(digest_of)
        .opening_indexes_at_embedding_width(DIM * 2);
    let state =
        AppState::new_with_platform_and_engine(NodeId::from_u128(1), Some(Arc::new(engine)));

    let plant = &lib.plant;
    let planted = plant.sentence.replacen(&plant.word, &plant.planted, 1);
    let (status, body) = align(&state, serde_json::json!({ "quote": planted })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resp: AlignResponse = serde_json::from_value(body.clone()).unwrap();
    assert!(resp.corpora_unavailable.is_empty(), "{body}");
    assert_eq!(resp.corpora.len(), 1, "{body}");
    assert!(!resp.alignments.is_empty(), "the misquote aligns: {body}");
}

#[tokio::test]
async fn one_changed_word_is_one_substitution_at_its_ranges() {
    let lib = library();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &lib.corpus_id, &lib.documents, true).await;
    let state = state_over(indexes).await;

    let plant = &lib.plant;
    let planted = plant.sentence.replacen(&plant.word, &plant.planted, 1);
    let at = planted.find(&plant.planted).unwrap();
    let word = [
        planted[..at].chars().count() as u64,
        (planted[..at].chars().count() + plant.planted.chars().count()) as u64,
    ];
    let source = lib
        .documents
        .iter()
        .find(|d| d.name == plant.document)
        .unwrap();

    let (status, body) = align(&state, serde_json::json!({ "quote": planted })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resp: AlignResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(resp.aligner, quote_align::ALIGNER_ID);
    let Some(top) = resp.alignments.first() else {
        panic!("the misquote must align: {body}");
    };
    let span = &top.span;
    assert_eq!(span.corpus_id, lib.corpus_id);
    assert_eq!(
        span.document.text_sha256,
        Sha256Hash::of_str(&source.text).to_hex()
    );
    assert_eq!(span.document.source.id, source.name);
    assert_eq!(
        span.exact,
        cp(&source.text, span.start, span.end),
        "span.exact must be the text's own [start, end)"
    );
    let [d] = top.differences.as_slice() else {
        panic!("one changed word must be one difference: {body}");
    };
    assert_eq!(d.kind, DifferenceKind::Substituted, "{body}");
    assert_eq!(d.quote, word, "the quote range is the planted word: {body}");
    assert_eq!(
        cp(&source.text, d.source[0], d.source[1]),
        plant.word,
        "the source range names the word it replaced"
    );

    let records: Vec<DocumentRecord> = {
        let index = CorpusIndex::open(&tmp.path().join("indexes").join(&lib.corpus_id))
            .await
            .unwrap();
        index.documents().await.unwrap().unwrap().1
    };
    assert_eq!(
        resp.corpora
            .iter()
            .map(|c| (c.corpus_id.as_str(), c.texts_digest.clone()))
            .collect::<Vec<_>>(),
        vec![(lib.corpus_id.as_str(), digest_of(&records).to_hex())],
        "the reply names the digest of the corpus it aligned against"
    );
    assert!(resp.corpora_unavailable.is_empty(), "{body}");

    // The sentence as written is verbatim: no difference, full coverage.
    let (_, body) = align(&state, serde_json::json!({ "quote": plant.sentence })).await;
    let resp: AlignResponse = serde_json::from_value(body.clone()).unwrap();
    let top = resp.alignments.first().expect("the sentence aligns");
    assert!(top.differences.is_empty(), "{body}");
    assert_eq!(top.coverage, 1.0);
    assert_eq!(top.span.exact, plant.sentence);
}

#[tokio::test]
async fn every_requested_corpus_is_aligned_or_named_unavailable() {
    let lib = library();
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, &lib.corpus_id, &lib.documents, true).await;
    install(&indexes, "before-texts", &lib.documents, false).await;
    let state = state_over(indexes).await;

    let (status, body) = align(
        &state,
        serde_json::json!({
            "quote": lib.plant.sentence,
            "corpora": [lib.corpus_id, "before-texts", "nowhere"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resp: AlignResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        resp.corpora
            .iter()
            .map(|c| c.corpus_id.as_str())
            .collect::<Vec<_>>(),
        vec![lib.corpus_id.as_str()],
        "{body}"
    );
    let mut gone: Vec<(&str, &str)> = resp
        .corpora_unavailable
        .iter()
        .map(|u| (u.corpus_id.as_str(), u.reason.as_str()))
        .collect();
    gone.sort();
    assert_eq!(
        gone,
        vec![
            ("before-texts", "texts not stored"),
            ("nowhere", "corpus not held"),
        ],
        "{body}"
    );
    assert!(resp
        .alignments
        .iter()
        .all(|a| a.span.corpus_id == lib.corpus_id));

    let (status, body) = align(&state, serde_json::json!({ "quote": "   " })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "empty quote");
}

/// The manifest names the route it serves, and the feature travels with it
/// (§2.1): `evidence:align` iff `align_endpoint`, and never without
/// `evidence:text`.
#[tokio::test]
async fn the_manifest_advertises_align_with_its_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state_over(tmp.path().join("indexes")).await;
    let mut req = Request::get("/oicp/v1/capabilities")
        .header("host", "127.0.0.1:9741")
        .body(Body::empty())
        .unwrap();
    let addr: SocketAddr = "127.0.0.1:55003".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = client_router(state).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let m: oicp_types::ProviderManifest = serde_json::from_slice(&bytes).unwrap();
    let evidence = m
        .knowledge
        .and_then(|k| k.evidence)
        .expect("knowledge.evidence");
    assert_eq!(evidence.align_endpoint.as_deref(), Some("/oicp/v1/align"));
    assert!(m
        .features
        .iter()
        .any(|f| f == oicp_types::features::EVIDENCE_ALIGN));
    assert!(m
        .features
        .iter()
        .any(|f| f == oicp_types::features::EVIDENCE_TEXT));
}

/// A quotation standing word for word in a stored text is aligned there
/// however the lexical search ranked that text (OICP v0.6 §2.18, PAL1). The
/// decoy's twelve paragraphs say "sleep", "their" and "in" more often and in
/// fewer words than Mill's one, so its chunks fill the lexical top
/// `candidate_k`, and never in that order. Red on d6bf9075c's route: Mill was
/// no candidate, so the reply held no alignment (the holdout essay's dry run
/// fixed "in their sleep." to Malthus's "in their population.").
#[tokio::test]
async fn a_quotation_standing_verbatim_is_aligned_where_it_stands_behind_the_lexical_top() {
    let decoy = (0..12)
        .map(|i| format!("Sleep, sleep in their beds, their sleep; sleep came {i} times in sleep."))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mill = "They grow richer, as it were, in their sleep, without working, risking, or economizing. What claim have they, on the general principle of social justice, to this accession of riches?";
    let docs = vec![
        Doc {
            name: "decoy".into(),
            metadata: None,
            text: decoy,
        },
        Doc {
            name: "mill1848".into(),
            metadata: None,
            text: mill.into(),
        },
    ];
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    install(&indexes, "pal1-recall", &docs, true).await;
    let state = state_over(indexes).await;

    let (status, body) = align(&state, serde_json::json!({ "quote": "in their sleep." })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resp: AlignResponse = serde_json::from_value(body.clone()).unwrap();
    let Some(top) = resp.alignments.first() else {
        panic!("Mill's words must align: {body}");
    };
    assert_eq!(top.span.document.source.id, "mill1848", "{body}");
    assert_eq!(top.span.exact, "in their sleep,");
    let kinds: Vec<DifferenceKind> = top.differences.iter().map(|d| d.kind).collect();
    assert_eq!(
        kinds,
        [DifferenceKind::Substituted],
        "the stop for the comma alone: {body}"
    );
    assert!(
        resp.alignments
            .iter()
            .all(|a| a.span.document.source.id == "mill1848"),
        "no decoy span with a word changed: {body}"
    );
}
