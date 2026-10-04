// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite (pb-bench-dials-rerank).
//!
//! A turn's `rerank` pins reach ITS retrieval and no other turn's. The corpus
//! holds two chunks of one article and one of another; per-article dedup
//! keeps one chunk per article, so the pinned turn's model calls see one
//! `alpha` chunk and the unpinned turn's, on the same daemon, both. The two
//! `alpha` chunks carry different titles: dedup keys on `source_doc_id`, and
//! source expansion, which runs after retrieval, re-fetches by title — one
//! shared title would put the deduped chunk back and hide the pin.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use sovereign_contracts::traits::StateStore;
use sovereign_contracts::types::{
    CompletionRequest, Intent, RerankOverrides, TurnFrame, TurnMode, TurnRequest,
};
use sovereign_daemon::turn_http::turn_router;

use super::{open_stream, send_request};
use crate::common::{spawn_router, TestProvider};

type Log = Arc<Mutex<Vec<CompletionRequest>>>;

const DIMS: usize = 4;
const ALPHA: [&str; 2] = ["rerankmarker-alpha-one", "rerankmarker-alpha-two"];
const BETA: &str = "rerankmarker-beta";

/// The corpus_scoping daemon (installed indexes read through the reading
/// double), with an embedder the turn's retrieval can call.
fn serving_daemon(
    provider: TestProvider,
) -> (
    tempfile::TempDir,
    Arc<sovereign_daemon::EmbeddedDaemon>,
    Arc<dyn StateStore>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let engine = Arc::new(
        crate::common::reading_double(
            tmp.path().join("indexes"),
            Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; DIMS]) })),
        )
        .without_foreground_signal()
        .with_builtin_corpora(Vec::new()),
    );
    let services =
        crate::common::desktop_services_with_store(engine, Arc::clone(&store), Arc::new(provider));
    let daemon = sovereign_daemon::EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        sovereign_contracts::setup_config::SetupConfig::unconfigured(),
        services,
    );
    (tmp, daemon, store)
}

/// `free-will` with two chunks of article `alpha` (titled apart) and one of
/// `beta`.
async fn install_corpus(indexes_dir: &std::path::Path) {
    use corpus_index::index::{CorpusIndex, InsertChunk};
    let index = CorpusIndex::create(
        &indexes_dir.join("free-will"),
        "free-will",
        "free-will",
        "qwen3-embedding-0.6b",
        DIMS,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    let chunk = |marker: &str, doc: &str, title: &str| {
        (
            InsertChunk {
                content: format!("{marker}: free will and determinism"),
                title: Some(title.into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some(doc.into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; DIMS],
        )
    };
    index
        .insert_batch(&[
            chunk(ALPHA[0], "alpha", "Alpha I"),
            chunk(ALPHA[1], "alpha", "Alpha II"),
            chunk(BETA, "beta", "Beta"),
        ])
        .await
        .unwrap();
    index.build_indexes(true, true, None).await.unwrap();
    index.mark_ingestion_complete().unwrap();
}

/// Ask one knowledge turn on a fresh conversation sealed to the corpus, and
/// return its terminal frame and which markers its model calls were shown.
async fn ask(
    addr: std::net::SocketAddr,
    log: &Log,
    rerank: Option<RerankOverrides>,
) -> (TurnFrame, Vec<&'static str>) {
    log.lock().unwrap().clear();
    let conv = reqwest::Client::new()
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["free-will"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .expect("create response carries an id")
        .to_string();
    let mut ws = open_stream(addr, &conv).await;
    send_request(
        &mut ws,
        TurnRequest::Message {
            content: "what is free will and determinism?".into(),
            mode: TurnMode::Grounded,
            intent: Some(Intent::KnowledgeQuery),
            sampling: None,
            rerank,
        },
    )
    .await;
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("a TurnFrame");
            if matches!(
                frame,
                TurnFrame::Complete { .. } | TurnFrame::StreamError { .. }
            ) {
                return frame;
            }
        }
        panic!("the socket closed before a terminal frame");
    })
    .await
    .expect("the turn ended within 60s");
    let shown = format!("{:?}", log.lock().unwrap());
    let seen = [ALPHA[0], ALPHA[1], BETA]
        .into_iter()
        .filter(|m| shown.contains(m))
        .collect();
    (terminal, seen)
}

#[tokio::test]
async fn a_turns_rerank_pin_dedups_its_retrieval_and_no_other_turns() {
    let log: Log = Arc::default();
    let provider = TestProvider::new()
        .with_stream_chunks(vec!["answered.".to_string()])
        .with_complete_text("answered.")
        .with_embed_marker(|_| vec![0.0; DIMS])
        .with_request_log(Arc::clone(&log));
    let (tmp, daemon, _store) = serving_daemon(provider);
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes")).await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let pin = RerankOverrides {
        enabled: Some(true),
        ..Default::default()
    };
    let (frame, pinned) = ask(addr, &log, Some(pin)).await;
    assert!(matches!(frame, TurnFrame::Complete { .. }), "{frame:?}");
    assert!(
        pinned.contains(&BETA),
        "the pinned turn retrieved the corpus: {pinned:?}"
    );
    assert_eq!(
        pinned.iter().filter(|m| ALPHA.contains(m)).count(),
        1,
        "the pinned turn kept one chunk per article: {pinned:?}"
    );

    // The next turn on the SAME daemon carries no pin: it runs at the
    // daemon's config (no dedup), which it would not if the pin had landed
    // on the process-wide lane.
    let (frame, unpinned) = ask(addr, &log, None).await;
    assert!(matches!(frame, TurnFrame::Complete { .. }), "{frame:?}");
    assert_eq!(
        unpinned,
        vec![ALPHA[0], ALPHA[1], BETA],
        "an unpinned turn keeps both chunks of one article"
    );
}
