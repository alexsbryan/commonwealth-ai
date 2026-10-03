// SPDX-License-Identifier: AGPL-3.0-or-later
//! The execute origin's half of the donor's tests (cw-lift 5g,
//! pb-work-donor), beside the donor's own so neither enters ARCH §3.1's band.
use super::*;
use crate::donor::tests::{a_unit, section, StubExecutor};
use crate::donor::{donor_registry, resolve_offer, DONOR_ISOLATION};
use commonwealth_work::sandbox::Sandbox;

/// A stand-in execute origin on loopback: it describes `ingest:v1` as the svrn
/// daemon's does, credits node 0x44ae, passes every validate, and answers a
/// run with one progress line and a passed verdict.
async fn a_fake_origin() -> std::net::SocketAddr {
    use axum::routing::{get, post};
    use oicp_types::work::exec::{
        ExecDescription, ExecEvent, EXEC_DESCRIBE_PATH, EXEC_RUN_PATH, EXEC_VALIDATE_PATH,
    };
    let describe = || async {
        axum::Json(ExecDescription {
            descriptor: StubExecutor(JobKind::parse("ingest:v1").unwrap()).descriptor(),
            credit_node: Some(NodeId::from_u128(0x44ae)),
        })
    };
    let run = || async {
        let lines = [
            ExecEvent::Progress {
                note: "half".into(),
            },
            ExecEvent::Done {
                outcome: Ok((
                    kernel_types::Judgement::passed(
                        "ingest:v1 u",
                        kernel_types::Reason::literal("slice ingested"),
                    ),
                    serde_json::json!({ "partition_chunks_total": 3 }),
                )),
            },
        ]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect::<String>();
        lines
    };
    let app = axum::Router::new()
        .route(EXEC_DESCRIBE_PATH, get(describe))
        .route(
            EXEC_VALIDATE_PATH,
            post(|| async { axum::Json(Ok::<(), WorkRefusal>(())) }),
        )
        .route(EXEC_RUN_PATH, post(run));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// The control for the waiting half of
/// `an_offered_kind_waits_for_its_origin_and_is_offered_when_it_registers`,
/// through the real path: a registered execute origin describes itself, the
/// donor registers it under its kind, and `ingest:v1` is offered at this
/// build's floor. Without it that test is satisfiable by a `donor_registry`
/// that registers nothing; the failing input is a registry that forgets the
/// origins arm — the node would never offer the kind a daemon serves.
#[tokio::test]
async fn a_registered_origin_registers_its_kind_and_can_offer_it() {
    let addr = a_fake_origin().await;
    let origin = Arc::new(
        OriginExecutor::describe("cwth/work/ingest:v1", addr)
            .await
            .expect("the origin describes itself"),
    );
    assert_eq!(origin.credit_node(), Some(NodeId::from_u128(0x44ae)));
    let registry = donor_registry(Sandbox::Direct, &[Arc::clone(&origin)]);
    let kinds: Vec<String> = registry.kinds().iter().map(|k| k.to_string()).collect();
    assert!(
        kinds.iter().any(|k| k == "ingest:v1"),
        "a registered origin's kind must be registered, got {kinds:?}"
    );
    let offer = resolve_offer(
        &section(&["ingest:v1"]),
        &registry,
        "linux",
        "x86_64",
        DONOR_ISOLATION,
    )
    .expect("ingest:v1 is registered once its origin is")
    .expect("kinds are set, so there is an offer");
    assert_eq!(offer.kinds.len(), 1);
    assert_eq!(offer.isolation, DONOR_ISOLATION);

    // The forward: the origin's own validate before the lease, then its
    // outcome whole — the verdict and result it answered, progress on the way.
    let unit = JobUnit {
        kind: JobKind::parse("ingest:v1").unwrap(),
        ..a_unit()
    };
    assert_eq!(origin.validate_remote(&unit).await, Ok(Ok(())));
    let workdir = tempfile::tempdir().unwrap();
    let (judgement, result) = origin
        .execute(&unit, &JobContext::new(workdir.path()))
        .await
        .expect("the origin's verdict");
    assert_eq!(judgement.verdict(), kernel_types::Verdict::Passed);
    assert_eq!(result["partition_chunks_total"], 3);

    // A slot that describes another kind is refused by name.
    let wrong = OriginExecutor::describe("cwth/work/process:v1", addr).await;
    assert!(wrong.is_err(), "a slot/kind mismatch must not register");
}
