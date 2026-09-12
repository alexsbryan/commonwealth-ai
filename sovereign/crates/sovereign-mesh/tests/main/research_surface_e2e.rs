// SPDX-License-Identifier: AGPL-3.0-or-later
//! `research_http` end to end — deep research as a daemon JOB
//! (sv-surface, 2026-09-11).
//!
//! # What is pinned, and what is not
//!
//! A research run needs a draft model, an embed model and a search
//! backend; this test binary has none. So the LOOP is stubbed behind
//! `ResearchLauncher` — the seam `research_router_with` exists for — and
//! what is pinned is the JOB CONTRACT: `202` + ack shape, the cursored
//! frame log (`started`, then `live` once the charter lands), `409` for
//! a second concurrent run, abort landing a terminal frame, `404` on a
//! job the daemon never accepted or has finished, `400` on an empty
//! question, the shelf and the report route. The loop's own behaviour
//! is `sovereign-core`'s to test; the run-dir readers that came down from
//! the desktop have unit tests beside them in `research_http.rs`.
//!
//! The job table is process-global and "one run at a time" is its rule,
//! so the lifecycle is ONE sequential test; the read-only cases stand
//! alone and never POST.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use sovereign_contracts::daemon_wire::{
    ResearchCapabilities, ResearchFrame, ResearchJobAck, ResearchProgress, ResearchRequest,
    ResearchRunSummary,
};
use sovereign_mesh::research_http::{research_router_with, LaunchedRun, ResearchLauncher};

use crate::common::spawn_router;

/// A launcher that writes the artifacts the poller reads and drives to a
/// report when aborted (or after a ceiling, so a test cannot hang).
struct StubLauncher {
    base: PathBuf,
    minted: AtomicUsize,
}

/// The charter the poller needs: `question`, `created_at_unix`, and the
/// `charter.max_rounds` / `charter.consent` it projects. Spelled as JSON
/// rather than through the ICD type so this file pins the BYTES the
/// route reads, which is what a run dir written by another version of
/// the loop would hand it.
fn charter_json(run_id: &str, question: &str) -> String {
    format!(
        r#"{{"icd":"charter","version":1,"run_id":"{run_id}","question":"{question}",
            "seed_id":null,"created_at_unix":100,"frozen":true,
            "charter":{{"max_rounds":2,"evidence_window_max_chunks":20,
              "containment":{{"trigger":"witness","extraction_max_tokens":256,"specifics_max":3}},
              "triage":{{"code_set_k":3,"eps_quota":0.1,"content_coverage_floor":0.25,"prose_line_floor":500}},
              "budget":{{"web_search_queries":4,"web_fetch_pages":4}},
              "custody":{{"stamp_required":true,"unknown_refuses":true}},
              "url_constraint":{{"enabled":true,"layer":"strict"}},
              "consent":{{"run-id":"{run_id}","granted-at-unix":100,"release-floor":"public-web"}}}}}}"#
    )
}

impl ResearchLauncher for StubLauncher {
    fn runs_base(&self) -> PathBuf {
        self.base.clone()
    }

    fn capabilities(&self) -> ResearchCapabilities {
        ResearchCapabilities {
            flags: vec!["--stub".to_string()],
            error: None,
        }
    }

    fn launch(
        &self,
        request: ResearchRequest,
        abort: Arc<AtomicBool>,
    ) -> BoxFuture<'static, Result<LaunchedRun, String>> {
        let n = self.minted.fetch_add(1, Ordering::SeqCst);
        let run_id = format!("dr-{}", 1_000 + n);
        let run_dir = self.base.join(&run_id);
        Box::pin(async move {
            std::fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
            std::fs::write(
                run_dir.join("charter.json"),
                charter_json(&run_id, &request.question),
            )
            .map_err(|e| e.to_string())?;
            let dir = run_dir.clone();
            let drive: BoxFuture<'static, Result<(), String>> = Box::pin(async move {
                let started = std::time::Instant::now();
                while !abort.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(8) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                std::fs::write(dir.join("report.md"), "# Stub report\n").map_err(|e| e.to_string())
            });
            Ok(LaunchedRun {
                run_id,
                run_dir,
                drive,
            })
        })
    }
}

/// The fixture above is hand-spelled BYTES deliberately — it stands in for
/// a run dir written by some other version of the loop, which is the thing
/// the route has to survive. That only holds if the bytes are ones the loop
/// would actually write, and on the first pass they were not: the charter
/// gave `prose_line_floor` a fraction where the ICD declares a `usize`, and
/// spelled the consent grant snake_case where `ConsentGrant` renames to
/// kebab. Either one made `RunDirPoller::snapshot` answer `None` forever,
/// which surfaced four seconds downstream as "no live frame" rather than as
/// a parse error. Assert the contract HERE, where the failure names it.
#[test]
fn the_charter_fixture_is_bytes_the_icd_can_read() {
    let raw = charter_json("dr-1000", "When did Apollo 11 land?");
    let charter: sovereign_core::deep_research::icd::Charter =
        serde_json::from_str(&raw).expect("the fixture parses as the ICD Charter");
    assert_eq!(charter.run_id, "dr-1000");
    assert_eq!(charter.charter.max_rounds, 2);
    assert_eq!(
        charter
            .charter
            .consent
            .expect("the fixture carries a consent grant")
            .release_floor
            .as_str(),
        "public-web",
    );
}

fn stub_router(base: &std::path::Path) -> axum::Router {
    research_router_with(Arc::new(StubLauncher {
        base: base.to_path_buf(),
        minted: AtomicUsize::new(0),
    }))
}

async fn progress(
    client: &reqwest::Client,
    addr: &str,
    job: &str,
    after: usize,
) -> ResearchProgress {
    client
        .get(format!(
            "http://{addr}/v1/research/{job}/progress?after={after}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_run_is_accepted_cursored_refused_twice_aborted_and_lands_a_report() {
    let base = tempfile::tempdir().unwrap();
    let addr = spawn_router(stub_router(base.path())).await;
    let client = reqwest::Client::new();

    // Accept: 202 + the ack names the run dir and the progress route.
    let resp = client
        .post(format!("http://{addr}/v1/research"))
        .json(&ResearchRequest {
            question: "When did Apollo 11 land?".to_string(),
            max_rounds: Some(2),
            ..Default::default()
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let ack: ResearchJobAck = resp.json().await.unwrap();
    assert!(ack.ok);
    assert!(ack.job_id.starts_with("dr-"), "{}", ack.job_id);
    assert_eq!(
        ack.progress_route,
        format!("/v1/research/{}/progress", ack.job_id)
    );
    assert!(
        PathBuf::from(&ack.run_dir).join("charter.json").is_file(),
        "the run dir is real on disk when the ack returns"
    );

    // The first frame is `started`, carrying the same run dir.
    let p0 = progress(&client, &addr.to_string(), &ack.job_id, 0).await;
    assert!(!p0.finished);
    assert!(matches!(
        &p0.frames[0],
        ResearchFrame::Started { run_id, run_dir } if *run_id == ack.job_id && *run_dir == ack.run_dir
    ));

    // The poller reads the charter within its first tick or two and
    // appends ONE `live` frame; the cursor past it serves nothing new.
    let mut live_seen = None;
    for _ in 0..40 {
        let p = progress(&client, &addr.to_string(), &ack.job_id, 0).await;
        if let Some(f) = p
            .frames
            .iter()
            .find(|f| matches!(f, ResearchFrame::Live { .. }))
        {
            live_seen = Some((f.clone(), p.next));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (live, next) = live_seen.expect("a live frame once charter.json is readable");
    match live {
        ResearchFrame::Live {
            round,
            max_rounds,
            stage,
            consent,
            ..
        } => {
            assert_eq!(round, None, "no gap list yet");
            assert_eq!(max_rounds, Some(2));
            assert_eq!(stage, "planning");
            assert_eq!(consent.unwrap().release_floor, "public-web");
        }
        other => panic!("{other:?}"),
    }
    let tail = progress(&client, &addr.to_string(), &ack.job_id, next).await;
    assert!(
        tail.frames.is_empty(),
        "nothing past the cursor: {:?}",
        tail.frames
    );
    assert_eq!(tail.next, next);
    assert_eq!(tail.stage, "planning");

    // One run at a time: the second is refused by name.
    let second = client
        .post(format!("http://{addr}/v1/research"))
        .json(&ResearchRequest {
            question: "another".to_string(),
            ..Default::default()
        })
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409);
    let body: serde_json::Value = second.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains(&ack.job_id),
        "the refusal names the live run: {body}"
    );

    // The shelf sees it live, with no terminal state yet.
    let shelf: Vec<ResearchRunSummary> = client
        .get(format!("http://{addr}/v1/research/runs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = shelf
        .iter()
        .find(|r| r.run_id == ack.job_id)
        .expect("on the shelf");
    assert!(row.live);
    assert_eq!(row.terminal_state, None);
    assert!(!row.report_present);
    assert_eq!(row.question.as_deref(), Some("When did Apollo 11 land?"));

    // The active census names it.
    let active: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/v1/research/active"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["run_id"], ack.job_id);

    // Abort: 200, and the loop lands a report — the terminal frame is
    // `report_ready` and the log is `finished`.
    let ab = client
        .post(format!("http://{addr}/v1/research/{}/abort", ack.job_id))
        .send()
        .await
        .unwrap();
    assert_eq!(ab.status(), 200);
    let ab: serde_json::Value = ab.json().await.unwrap();
    assert_eq!(ab["aborted"], true);

    let mut terminal = None;
    for _ in 0..80 {
        let p = progress(&client, &addr.to_string(), &ack.job_id, 0).await;
        if p.finished {
            terminal = Some(p);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let p = terminal.expect("the job finishes after abort");
    match p.frames.last().unwrap() {
        ResearchFrame::ReportReady { report } => {
            assert_eq!(report.run_id, ack.job_id);
            assert_eq!(report.report_md, "# Stub report\n");
            assert_eq!(
                report.terminal_state, "interrupted",
                "no manifest — reported, not defaulted"
            );
        }
        other => panic!("terminal frame: {other:?}"),
    }

    // A finished job is not live: abort is 404, and a new run is accepted.
    let ab2 = client
        .post(format!("http://{addr}/v1/research/{}/abort", ack.job_id))
        .send()
        .await
        .unwrap();
    assert_eq!(ab2.status(), 404);
    let report: serde_json::Value = client
        .get(format!(
            "http://{addr}/v1/research/runs/{}/report",
            ack.job_id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["report_md"], "# Stub report\n");

    let third = client
        .post(format!("http://{addr}/v1/research"))
        .json(&ResearchRequest {
            question: "after the first finished".to_string(),
            ..Default::default()
        })
        .send()
        .await
        .unwrap();
    assert_eq!(third.status(), 202);
    let third: ResearchJobAck = third.json().await.unwrap();
    assert_ne!(third.job_id, ack.job_id);
    // Leave nothing live for the other tests: stop it and wait it out.
    let _ = client
        .post(format!("http://{addr}/v1/research/{}/abort", third.job_id))
        .send()
        .await;
    for _ in 0..80 {
        if progress(&client, &addr.to_string(), &third.job_id, 0)
            .await
            .finished
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn capabilities_are_the_launchers_and_an_empty_question_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let addr = spawn_router(stub_router(base.path())).await;
    let caps: ResearchCapabilities =
        reqwest::get(format!("http://{addr}/v1/research/capabilities"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(caps.flags, vec!["--stub".to_string()]);
    assert_eq!(caps.error, None);

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/research"))
        .json(&ResearchRequest::default())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("question"),
        "{body}"
    );
}

#[tokio::test]
async fn unknown_jobs_and_reportless_runs_are_404_by_name() {
    let base = tempfile::tempdir().unwrap();
    // A run dir on the shelf that never reached a report.
    let stale = base.path().join("dr-7");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("charter.json"), charter_json("dr-7", "stale")).unwrap();
    let addr = spawn_router(stub_router(base.path())).await;
    let client = reqwest::Client::new();

    let p = client
        .get(format!("http://{addr}/v1/research/dr-ghost/progress"))
        .send()
        .await
        .unwrap();
    assert_eq!(p.status(), 404);
    let body: serde_json::Value = p.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("dr-ghost"),
        "{body}"
    );

    let a = client
        .post(format!("http://{addr}/v1/research/dr-ghost/abort"))
        .send()
        .await
        .unwrap();
    assert_eq!(a.status(), 404);

    let shelf: Vec<ResearchRunSummary> = client
        .get(format!("http://{addr}/v1/research/runs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = shelf
        .iter()
        .find(|r| r.run_id == "dr-7")
        .expect("stale run listed");
    assert!(!row.live, "nobody is driving it");
    assert_eq!(row.terminal_state, None);

    let r = client
        .get(format!("http://{addr}/v1/research/runs/dr-7/report"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let body: serde_json::Value = r.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("no report.md"),
        "{body}"
    );
    let r = client
        .get(format!("http://{addr}/v1/research/runs/dr-none/report"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}
