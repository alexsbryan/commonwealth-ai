// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression (order deep-research-t0, red R-1): the workflow store path
//! must stamp `indexes_built` — the readiness flag `corpus_search.rs`
//! Filter 2 gates chat retrieval on.
//!
//! Prior state: `tool:corpus_store` called `mark_ingestion_complete()` but
//! never `mark_indexes_built()`, so a corpus built by the documented
//! `svrn corpus ingest` (the shipped `notebook` workflow) landed with
//! `indexes_built: false` while the ingest printed "searchable". Filter 2
//! drops unstamped corpora "on EVERY path so the model can't fabricate over
//! the void" — workflow-ingested corpora were invisible to retrieval.
//! Measured red: needle-rig-baseline.sh exit 4, `kq_fanout_corpora=0`
//! (invariant note 89d5f75a).

use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::CorpusIndex;
use sovereign_tools::corpus_store::CorpusStoreTool;

fn ctx() -> ToolContext {
    ToolContext {
        conversation_id: "test".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
        turn_index: 0,
    }
}

/// The store step — the workflow-ingest write path — must leave the corpus
/// retrieval-visible: `indexes_built` stamped, exactly as the bespoke
/// ingest path does (`mark_indexes_built`, corpus-engine/src/engine/
/// ingest.rs). `build_indexes: false` is deliberate: a small corpus is
/// served by flat scan, so the stamp must NOT be hidden inside the build
/// branch — it is readiness, not a build receipt.
#[tokio::test]
async fn workflow_store_marks_indexes_built_so_retrieval_serves_the_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path().join("indexes");

    let params = serde_json::json!({
        "corpus": "conrad",
        "chunks": serde_json::json!([
            { "text": "Mr Verloc kept a shabby shop in Soho", "index": 0 },
            { "text": "Winnie Verloc guarded her brother Stevie above all", "index": 1 }
        ]).to_string(),
        "embeddings": serde_json::json!([
            [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2]
        ]).to_string(),
        "source_doc_id": "secret-agent",
        "index_dir": index_dir.to_string_lossy(),
        "build_indexes": false
    });

    let out = CorpusStoreTool.execute(&params, &ctx()).await.unwrap();
    match out {
        StepOutput::Text(t) => assert!(t.contains("stored 2"), "{t}"),
        o => panic!("unexpected output: {o:?}"),
    }

    let corpus_path = index_dir.join("conrad");
    assert!(
        CorpusIndex::indexes_are_built(&corpus_path),
        "workflow store must stamp indexes_built — corpus_search.rs Filter 2 \
         drops unstamped corpora from retrieval on EVERY path (red R-1, note 89d5f75a)"
    );
    assert!(
        CorpusIndex::is_ingestion_complete(&corpus_path),
        "store must still finalize the corpus (ingestion_complete) — not regressed"
    );
}
