// SPDX-License-Identifier: AGPL-3.0-or-later
//! An install whose run panics ends as a recorded failure (PL6), with the
//! panic's words, and frees the corpus for a retry.
use std::time::Duration;

use axum::http::StatusCode;
use sovereign_contracts::daemon_wire::IngestProgress;

use super::ingests::{Ingests, Run};
use super::{install, test_state, wait_until_progress, InstallResp};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_ingest_reports_failed_and_frees_the_corpus() {
    let corpus_id = "paniccorpus";
    let ingests = Ingests::new(&[corpus_id], Run::Panics);
    let (state, _engine) = test_state(ingests.double());

    let (status, body) = install(&state, corpus_id).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let snapshot = wait_until_progress(
        &state,
        |s| {
            matches!(
                s.progress.get(corpus_id),
                Some(IngestProgress::Failed { .. })
            )
        },
        Duration::from_secs(60),
        "a panicked install recorded as Failed",
    )
    .await;
    let Some(IngestProgress::Failed { message }) = snapshot.progress.get(corpus_id) else {
        panic!("expected a Failed record, got {snapshot:?}");
    };
    assert!(
        message.contains("panicked") && message.contains("No custom extractor registered"),
        "the failure names the panic in its own words: {message}"
    );

    ingests.set_run(Run::Completes);
    let (status, body) = install(&state, corpus_id).await;
    assert_eq!(status, StatusCode::OK);
    let resp: InstallResp = serde_json::from_slice(&body).unwrap();
    assert!(
        resp.spawned,
        "the panicked task left the corpus in active_ingests, so the retry was refused"
    );
}
