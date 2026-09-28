// SPDX-License-Identifier: AGPL-3.0-or-later
//! The execute origin's doors, dialed over its loopback port the way cw-rails'
//! donor dials them.
use super::*;
use sovereign_contracts::oicp::work::exec::JobError;
use sovereign_contracts::oicp::JobKind;

fn an_engine() -> (tempfile::TempDir, Arc<corpus_engine::CorpusEngine>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let recipes = dir.path().join("recipes");
    let indexes = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes).expect("recipes dir");
    std::fs::create_dir_all(&indexes).expect("indexes dir");
    let embed: corpus_index::types::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }));
    (
        dir,
        Arc::new(corpus_engine::CorpusEngine::new(recipes, indexes, embed)),
    )
}

fn a_unit(payload: serde_json::Value) -> JobUnit {
    JobUnit {
        kind: JobKind::parse(crate::ingest_executor::INGEST_KIND).expect("kind"),
        unit_hash: "a".repeat(64),
        payload,
        requirements: Default::default(),
        tenant: None,
    }
}

/// Serve the origin with cw-rails pointed at a closed port: the doors answer
/// whether or not the registration is taken.
async fn served() -> (tempfile::TempDir, String) {
    let (dir, engine) = an_engine();
    let origin = Arc::new(WorkOrigin::new(
        IngestExecutor::new(engine),
        NodeId::from_u128(0x44ae),
    ));
    let handle = spawn(origin, "http://127.0.0.1:1".to_string())
        .await
        .expect("a loopback port");
    let base = format!("http://{}", handle.addr);
    // The handle lives as long as the test's runtime does.
    std::mem::forget(handle);
    (dir, base)
}

/// The donor's describe read names the kind, the executor's own descriptor,
/// and the registrant's node as the credit identity (phase-b-39 fork 1).
#[tokio::test]
async fn describe_names_the_kind_and_the_credit_identity() {
    let (_dir, base) = served().await;
    let d: ExecDescription = reqwest::get(format!("{base}{EXEC_DESCRIBE_PATH}"))
        .await
        .expect("describe")
        .json()
        .await
        .expect("an ExecDescription");
    assert_eq!(
        d.descriptor.kind.to_string(),
        crate::ingest_executor::INGEST_KIND
    );
    assert_eq!(d.credit_node, Some(NodeId::from_u128(0x44ae)));
}

/// The validate door is the executor's own pure check, before any lease: a
/// payload that does not parse is refused by name, and the control passes.
#[tokio::test]
async fn validate_answers_the_executors_refusal_before_the_lease() {
    let (_dir, base) = served().await;
    let client = reqwest::Client::new();
    let refused: Result<(), WorkRefusal> = client
        .post(format!("{base}{EXEC_VALIDATE_PATH}"))
        .json(&a_unit(serde_json::json!({ "corpus_id": "" })))
        .send()
        .await
        .expect("validate")
        .json()
        .await
        .expect("a verdict");
    assert!(
        matches!(refused, Err(WorkRefusal::PayloadNotCanonical { .. })),
        "got {refused:?}"
    );
    let ok: Result<(), WorkRefusal> = client
        .post(format!("{base}{EXEC_VALIDATE_PATH}"))
        .json(&a_unit(serde_json::json!({
            "corpus_id": "sep", "recipe_id": "sep", "unit_id": 0,
            "unit": {"kind": "HfFile", "value": 0}
        })))
        .send()
        .await
        .expect("validate")
        .json()
        .await
        .expect("a verdict");
    assert!(ok.is_ok(), "the control: a well-formed slice, got {ok:?}");
}

/// The run door streams until a `Done` carrying `execute`'s own answer: here
/// the executor's re-validation refuses, which the donor reports as a
/// `NeverRan` Fail. A cancel naming nothing in flight is a 404, not a 200.
#[tokio::test]
async fn run_answers_the_executors_outcome_as_the_last_line() {
    let (dir, base) = served().await;
    let client = reqwest::Client::new();
    let body = client
        .post(format!("{base}{EXEC_RUN_PATH}"))
        .json(&ExecRun {
            unit: a_unit(serde_json::json!({ "corpus_id": "" })),
            workdir: dir.path().to_path_buf(),
        })
        .send()
        .await
        .expect("run")
        .text()
        .await
        .expect("the event lines");
    let last = body.lines().last().expect("at least the Done line");
    match serde_json::from_str::<ExecEvent>(last).expect("an ExecEvent") {
        ExecEvent::Done {
            outcome: Err(JobError::Refused(WorkRefusal::PayloadNotCanonical { .. })),
        } => {}
        other => panic!("expected a refused Done, got {other:?}"),
    }
    let status = client
        .post(format!("{base}{EXEC_CANCEL_PATH}"))
        .json(&ExecCancel {
            unit_hash: "f".repeat(64),
        })
        .send()
        .await
        .expect("cancel")
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}
