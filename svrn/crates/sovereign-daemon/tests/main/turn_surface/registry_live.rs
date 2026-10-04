// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite (pc-corpus-registry-live).
//!
//! A corpus ingested while the daemon runs is searchable by the next grounded
//! turn. The daemon resolves every turn to the local owner, so the turn's
//! ceiling is the corpus registry (`corpus_state`), and until this row the
//! registry was written at boot only: a corpus that landed after boot was on
//! disk, listed by the engine, and outside every turn's ceiling until the next
//! restart. The named failing input: drop the reconcile from
//! `Runtime::principal_scope` and the turn below never shows its model the
//! corpus's chunk.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use sovereign_contracts::traits::StateStore;
use sovereign_contracts::types::{CompletionRequest, Intent, TurnFrame, TurnMode, TurnRequest};
use sovereign_daemon::turn_http::turn_router;

use super::{create_conversation, open_stream, send_request};
use crate::common::{
    desktop_services, spawn_router, stub_runtime_with_engine, DesktopParts, TestProvider,
};

const DIMS: usize = 4;
const CORPUS: &str = "late-corpus";
const MARKER: &str = "registrylive-marker";

/// One chunk carrying [`MARKER`], indexed and marked complete: what a finished
/// ingest leaves under the index dir.
async fn ingest(indexes_dir: &std::path::Path) {
    use corpus_index::index::{CorpusIndex, InsertChunk};
    let index = CorpusIndex::create(
        &indexes_dir.join(CORPUS),
        CORPUS,
        CORPUS,
        "qwen3-embedding-0.6b",
        DIMS,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: format!("{MARKER}: free will and determinism"),
                title: Some("Late".into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some("late".into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; DIMS],
        )])
        .await
        .unwrap();
    index.build_indexes(true, true, None).await.unwrap();
    index.mark_ingestion_complete().unwrap();
}

#[tokio::test]
async fn a_corpus_ingested_while_the_daemon_runs_is_in_the_next_grounded_turn() {
    let log: Arc<Mutex<Vec<CompletionRequest>>> = Arc::default();
    let provider = TestProvider::new()
        .with_stream_chunks(vec!["answered.".to_string()])
        .with_complete_text("answered.")
        .with_embed_marker(|_| vec![0.0; DIMS])
        .with_request_log(Arc::clone(&log));
    let provider: Arc<dyn sovereign_contracts::traits::InferenceProvider> = Arc::new(provider);

    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let engine = Arc::new(
        crate::common::reading_double(
            indexes.clone(),
            Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; DIMS]) })),
        )
        .without_foreground_signal()
        .with_builtin_corpora(Vec::new()),
    );
    // The boot's wiring (`daemon_cmd/boot.rs`): an unkeyed daemon resolves
    // every turn to the local owner, so the ceiling is the registry.
    let mut runtime = stub_runtime_with_engine(
        Arc::clone(&provider),
        Some(Arc::clone(&store)),
        engine.clone(),
    );
    Arc::get_mut(&mut runtime).unwrap().corpus_principal =
        Some(Arc::new(sovereign_daemon::principal::LocalOwnerPrincipal));
    // The boot's reconcile, over an engine holding nothing yet.
    sovereign_core::corpus_registry::reconcile_corpus_registry(engine.as_ref(), store.as_ref())
        .await;
    assert!(store.list_corpus_states().await.unwrap().is_empty());

    let daemon = sovereign_daemon::EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        sovereign_contracts::setup_config::SetupConfig::unconfigured(),
        desktop_services(DesktopParts {
            provider,
            store: Arc::clone(&store),
            runtime,
            ..DesktopParts::new(engine)
        }),
    );
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    // The ingest lands after the daemon is serving.
    ingest(&indexes).await;

    // An unscoped conversation, the shape `svrn chat ask` sends.
    let conv = create_conversation(&format!("http://{addr}")).await;
    let mut ws = open_stream(addr, &conv).await;
    send_request(
        &mut ws,
        TurnRequest::Message {
            content: "what is free will and determinism?".into(),
            mode: TurnMode::Grounded,
            intent: Some(Intent::KnowledgeQuery),
            sampling: None,
            rerank: None,
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
    assert!(
        matches!(terminal, TurnFrame::Complete { .. }),
        "{terminal:?}"
    );

    let shown = format!("{:?}", log.lock().unwrap());
    assert!(
        shown.contains(MARKER),
        "the next grounded turn searched the corpus ingested after boot \
         (its chunk never reached a model call)"
    );
    let registered: Vec<String> = store
        .list_corpus_states()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.corpus_id)
        .collect();
    assert_eq!(
        registered,
        vec![CORPUS.to_string()],
        "the turn brought the registry up to the engine"
    );
}
