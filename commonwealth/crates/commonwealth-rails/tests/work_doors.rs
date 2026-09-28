// SPDX-License-Identifier: AGPL-3.0-or-later
//! The submitter's work doors (pb-work-doors) against a solo cw-rails: seal,
//! submit, refusals and attribution, each answering from the one
//! implementation in commonwealth-work.

use std::path::Path;
use std::sync::Arc;

use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{api, RailsDaemon, RailsNode};
use commonwealth_work::process::{ProcessPayload, PROCESS_KIND};
use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
use commonwealth_work::refusal::{UnmetRequirement, WorkRefusal};
use commonwealth_work::{ActorKey, HandoffId, WorkAct};
use oicp_types::{Isolation, JobKind, JobRequirements, JobUnit, WorkOffer};
use serde_json::{json, Value};

fn hermetic() -> Config {
    Config {
        name: "work-node".to_string(),
        listen: 0xFFFF,
        relay: RelaySection {
            urls: Vec::new(),
            discovery: Some("none".to_string()),
        },
        media: MediaSection {
            origin: None,
            allow: Vec::new(),
        },
        gossip_interval_secs: 1,
        offline_threshold_secs: 60,
    }
}

async fn start(dir: &Path) -> (Arc<RailsDaemon>, String) {
    let node = RailsNode::bind(dir.to_path_buf(), hermetic())
        .await
        .expect("node binds");
    let daemon = Arc::new(RailsDaemon::start_from_disk(node).await.expect("starts"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = api::router(daemon.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (daemon, format!("http://{addr}"))
}

async fn post(base: &str, path: &str, body: &Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn projection(base: &str) -> WorkProjection {
    reqwest::get(format!("{base}/v1/work/projection"))
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn kind() -> JobKind {
    JobKind::parse(PROCESS_KIND).unwrap()
}

fn requirements() -> JobRequirements {
    JobRequirements {
        repo_rev: Some("aa".repeat(20)),
        os: Some(std::env::consts::OS.to_string()),
        arch: Some(std::env::consts::ARCH.to_string()),
        isolation: None,
        preconditions: Vec::new(),
    }
}

async fn seal_one(base: &str, argv: &[&str]) -> JobUnit {
    let payload = ProcessPayload::command(argv.iter().map(|s| s.to_string()).collect());
    let (status, body) = post(
        base,
        "/v1/work/seal",
        &json!({
            "kind": kind(),
            "units": [{ "payload": payload, "requirements": requirements() }],
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let mut units: Vec<JobUnit> = serde_json::from_value(body["units"].clone()).unwrap();
    units.remove(0)
}

/// The seal door answers the identity `seal::unit_hash` computes, and the
/// submit door opens a handoff signed as this node whose unit is queued.
#[tokio::test]
async fn a_sealed_unit_submits_as_this_node_and_is_queued() {
    let dir = tempfile::tempdir().unwrap();
    let (daemon, base) = start(dir.path()).await;
    let unit = seal_one(&base, &["true"]).await;
    assert_eq!(
        unit.unit_hash,
        commonwealth_work::unit_hash(&unit.kind, &unit.payload).unwrap(),
        "the door's seal is the one seal"
    );

    let (status, body) = post(
        &base,
        "/v1/work/submit",
        &json!({ "kind": kind(), "units": [unit.clone()], "ttl_secs": 60 }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let handoff: HandoffId = serde_json::from_value(body["handoff"].clone()).unwrap();
    assert_eq!(body["units"][0]["unit_hash"], json!(unit.unit_hash));
    assert!(
        body["seq"].is_u64(),
        "the append door's answer rides along: {body}"
    );

    let proj = projection(&base).await;
    let h = proj.handoffs.get(&handoff).expect("the handoff is folded");
    assert_eq!(
        h.submitter.as_str(),
        daemon.node.pubkey().to_string(),
        "the Submit is signed as this node"
    );
    assert!(matches!(
        h.units[&unit.unit_hash].status,
        WorkUnitStatus::Queued { prior_attempts: 0 }
    ));
    daemon.node.endpoint.close().await;
}

/// **The PLANT the row names.** A unit whose declared hash is not its
/// payload's is refused by the submit door, naming the identity it declared,
/// and nothing reaches the journal.
#[tokio::test]
async fn a_unit_whose_hash_does_not_cover_its_payload_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (daemon, base) = start(dir.path()).await;
    let mut unit = seal_one(&base, &["true"]).await;
    unit.payload = json!(ProcessPayload::command(vec!["false".to_string()]));

    let (status, body) = post(
        &base,
        "/v1/work/submit",
        &json!({ "kind": kind(), "units": [unit.clone()] }),
    )
    .await;
    assert_eq!(status, 422, "{body}");
    let why = body["error"].as_str().unwrap_or_default();
    assert!(
        why.contains(&unit.unit_hash),
        "names the declared identity: {why}"
    );
    assert!(
        projection(&base).await.handoffs.is_empty(),
        "a refused submission writes nothing"
    );
    daemon.node.endpoint.close().await;
}

/// The refusals door runs `may_take` for every published offer: an offer
/// from another OS refuses the unit as `requirement-unmet`, typed.
#[tokio::test]
async fn the_refusal_survey_is_may_take_over_every_offer() {
    let dir = tempfile::tempdir().unwrap();
    let (daemon, base) = start(dir.path()).await;
    let unit = seal_one(&base, &["true"]).await;
    let (_, body) = post(
        &base,
        "/v1/work/submit",
        &json!({ "kind": kind(), "units": [unit.clone()], "ttl_secs": 600 }),
    )
    .await;
    let handoff: HandoffId = serde_json::from_value(body["handoff"].clone()).unwrap();

    let offer = WorkAct::Offer(WorkOffer {
        kinds: vec![kind()],
        max_concurrent: 1,
        yield_to_foreground: false,
        isolation: Isolation::Subprocess,
        os: "plan9".to_string(),
        arch: std::env::consts::ARCH.to_string(),
        repos: Vec::new(),
        accept_from: None,
    });
    let (status, answer) = post(
        &base,
        "/v1/rail/append?namespace=work",
        &json!({ "op": "record", "payload": offer }),
    )
    .await;
    assert_eq!(status, 200, "{answer}");

    let now_ms = commonwealth_core::clock::unix_now_millis();
    let (status, body) = post(
        &base,
        "/v1/work/refusals",
        &json!({ "handoff": handoff, "units": [unit.unit_hash], "at_ms": now_ms }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let survey: std::collections::BTreeMap<String, Vec<(ActorKey, Result<(), WorkRefusal>)>> =
        serde_json::from_value(body).unwrap();
    let verdicts = &survey[&unit.unit_hash];
    assert_eq!(verdicts.len(), 1, "one offer, one verdict");
    match &verdicts[0].1 {
        Err(WorkRefusal::RequirementUnmet(UnmetRequirement::Os { host, .. })) => {
            assert_eq!(host, "plan9")
        }
        other => panic!("expected requirement-unmet on os, got {other:?}"),
    }
    daemon.node.endpoint.close().await;
}

/// With no image named, the attribution door answers this host's reading at
/// the caller's rev — `of_sandbox` over `Sandbox::Direct`.
#[tokio::test]
async fn the_attribution_door_answers_for_the_callers_rev() {
    let dir = tempfile::tempdir().unwrap();
    let (daemon, base) = start(dir.path()).await;
    let rev = "bb".repeat(20);
    let got: Value = reqwest::get(format!("{base}/v1/work/attribution?repo_rev={rev}"))
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["repo_rev"], json!(rev));
    assert_eq!(got["os"], json!(std::env::consts::OS));
    assert_eq!(got["arch"], json!(std::env::consts::ARCH));
    daemon.node.endpoint.close().await;
}

/// The yield door (pb-work-donor): a posted deadline is in force until it
/// passes, a later post extends it, an earlier one never shortens it, and a
/// malformed body is a 422 that holds nothing.
#[tokio::test]
async fn the_yield_door_holds_the_latest_deadline_until_it_passes() {
    let dir = tempfile::tempdir().unwrap();
    let (daemon, base) = start(dir.path()).await;
    assert_eq!(daemon.work_yield.yielding_at(0), None, "nobody posted");
    let (status, _) = post(&base, "/v1/work/yield", &json!({ "until": 5 })).await;
    assert_eq!(status, 422);
    assert_eq!(daemon.work_yield.yielding_at(0), None);
    let (status, _) = post(&base, "/v1/work/yield", &json!({ "until_ms": 2_000 })).await;
    assert_eq!(status, 200);
    assert_eq!(daemon.work_yield.yielding_at(1_000), Some(2_000));
    assert_eq!(daemon.work_yield.yielding_at(2_000), None, "passed");
    post(&base, "/v1/work/yield", &json!({ "until_ms": 1_500 })).await;
    assert_eq!(daemon.work_yield.yielding_at(1_000), Some(2_000));
    post(&base, "/v1/work/yield", &json!({ "until_ms": 9_000 })).await;
    assert_eq!(daemon.work_yield.yielding_at(2_000), Some(9_000));
    daemon.node.endpoint.close().await;
}
