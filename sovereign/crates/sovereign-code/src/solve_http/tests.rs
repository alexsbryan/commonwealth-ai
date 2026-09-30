//! The solve surface's route tests (moved from the svrn daemon at
//! pb-meshapp-solve).

use super::*;
use axum::body::Body;
use axum::http::Request;
use std::net::SocketAddr;
use std::process::Command;
use tower::ServiceExt;

fn fresh_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "t@t.t"],
        vec!["config", "user.name", "t"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(&args)
            .output();
    }
    tmp
}

/// Router with ConnectInfo pre-injected as loopback, mirroring
/// what `into_make_service_with_connect_info` provides live.
fn app(jobs: Arc<SolveJobs>) -> Router {
    let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
    solve_router(jobs).layer(Extension(axum::extract::ConnectInfo(loopback)))
}

fn submit_body(workdir: &std::path::Path) -> String {
    serde_json::json!({
        "workdir": workdir,
        "goal": "add an is_palindrome function to utils.py",
    })
    .to_string()
}

async fn post_submit(app: &Router, body: String) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .uri("/v1/solve/jobs")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn submit_returns_202_with_detected() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1")); // port 1: backend unreachable, job just errors in background
    let app = app(Arc::clone(&jobs));
    let repo = fresh_repo();
    std::fs::write(repo.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    // Commit it — a dirty tree is (correctly) refused at submit.
    for args in [vec!["add", "-A"], vec!["commit", "-m", "manifest"]] {
        let _ = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(&args)
            .output();
    }
    let (status, v) = post_submit(&app, submit_body(repo.path())).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert!(v["job_id"].as_str().is_some());
    assert_eq!(v["detected"]["framework"], "cargo");
    assert_eq!(v["detected"]["test_command"], "cargo test --quiet");
    assert_eq!(v["detected"]["model"], DEFAULT_MODEL);
    // Cleanly stop the background runner before the tempdir drops.
    let id = v["job_id"].as_str().unwrap();
    jobs.cancel(id);
}

#[tokio::test]
async fn dirty_workdir_is_refused_422() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let app = app(jobs);
    let repo = fresh_repo();
    std::fs::write(repo.path().join("wip.txt"), "x").unwrap();
    let (status, v) = post_submit(&app, submit_body(repo.path())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"], "dirty_workdir");
    assert_eq!(v["kind"], "uncommitted_changes");
}

#[tokio::test]
async fn one_job_per_workdir_conflicts_409() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let app = app(Arc::clone(&jobs));
    let repo = fresh_repo();
    let (s1, v1) = post_submit(&app, submit_body(repo.path())).await;
    assert_eq!(s1, StatusCode::ACCEPTED);
    let (s2, v2) = post_submit(&app, submit_body(repo.path())).await;
    assert_eq!(s2, StatusCode::CONFLICT, "{v2}");
    assert_eq!(v2["error"], "workdir_busy");
    assert_eq!(v2["job_id"], v1["job_id"]);
    jobs.cancel(v1["job_id"].as_str().unwrap());
}

#[tokio::test]
async fn global_capacity_enforced_429() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let app = app(Arc::clone(&jobs));
    let repos: Vec<_> = (0..3).map(|_| fresh_repo()).collect();
    let mut ids = Vec::new();
    for repo in repos.iter().take(MAX_RUNNING_JOBS) {
        let (s, v) = post_submit(&app, submit_body(repo.path())).await;
        assert_eq!(s, StatusCode::ACCEPTED);
        ids.push(v["job_id"].as_str().unwrap().to_string());
    }
    let (s, v) = post_submit(&app, submit_body(repos[MAX_RUNNING_JOBS].path())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"], "at_capacity");
    for id in ids {
        jobs.cancel(&id);
    }
}

#[tokio::test]
async fn cancel_flips_state_and_emits_done_event() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let app = app(Arc::clone(&jobs));
    let repo = fresh_repo();
    let (_, v) = post_submit(&app, submit_body(repo.path())).await;
    let id = v["job_id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .uri(format!("/v1/solve/jobs/{id}"))
        .method("DELETE")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let job = jobs.get(&id).unwrap();
    assert_eq!(job.state.lock().unwrap().label(), "cancelled");
    // Block-scoped: the ring guard must not span the second-cancel
    // await below (clippy::await_holding_lock is scope-based).
    {
        let ring = job.events.lock().unwrap();
        assert!(
            ring.iter().any(|e| e.is_done()),
            "cancel must emit a done event"
        );
    }

    // Second cancel: already finished.
    let req = Request::builder()
        .uri(format!("/v1/solve/jobs/{id}"))
        .method("DELETE")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn status_reports_state_and_rounds() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let app = app(Arc::clone(&jobs));
    let repo = fresh_repo();
    let (_, v) = post_submit(&app, submit_body(repo.path())).await;
    let id = v["job_id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .uri(format!("/v1/solve/jobs/{id}"))
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["job_id"], id);
    assert!(v["rounds"].is_array());
    assert!(v["detected"]["framework"].as_str().is_some());
    jobs.cancel(&id);
}

#[tokio::test]
async fn unknown_job_404s() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let app = app(jobs);
    let req = Request::builder()
        .uri("/v1/solve/jobs/nope")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn split_verb_requires_max_lines() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let app = app(jobs);
    let repo = fresh_repo();
    let body = serde_json::json!({
        "workdir": repo.path(),
        "goal": "split the big file",
        "verb": "split",
    })
    .to_string();
    let (status, v) = post_submit(&app, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn non_loopback_callers_are_rejected() {
    let jobs = Arc::new(SolveJobs::new("http://127.0.0.1:1"));
    let remote: SocketAddr = "10.0.0.7:1234".parse().unwrap();
    let app = solve_router(jobs).layer(Extension(axum::extract::ConnectInfo(remote)));
    let req = Request::builder()
        .uri("/v1/solve/jobs/nope")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

fn note_json(workdir: &std::path::Path, rev: &str) -> serde_json::Value {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["notes", "--ref=bench", "show", rev])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn commit_reached_lands_commit_and_per_commit_note() {
    let tmp = fresh_repo();
    std::fs::write(tmp.path().join("artifact.json"), "[1]\n").unwrap();
    let r1 = commit_reached(
        tmp.path(),
        "job-aaaa1111",
        "Make the artifact pass",
        2,
        15,
        0,
        "commonwealth/primary",
    )
    .unwrap();
    assert!(!r1.sha.is_empty());
    assert!(r1.noted);
    let status = Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.stdout.is_empty(), "tree must be clean after landing");
    let doc = note_json(tmp.path(), "HEAD");
    assert_eq!(doc["jobs"]["job-aaaa1111"]["status"], "reached");
    assert_eq!(doc["jobs"]["job-aaaa1111"]["commit"], r1.sha.as_str());
    assert_eq!(doc["jobs"]["job-aaaa1111"]["tests_passed"], 15);

    // A second landing creates its OWN commit whose note carries
    // only its own job; the first job's note stays on the first
    // commit (per-commit notes: the lineage IS the log).
    std::fs::write(tmp.path().join("artifact.json"), "[1, 2]\n").unwrap();
    let r2 = commit_reached(
        tmp.path(),
        "job-bbbb2222",
        "Grow the artifact",
        1,
        15,
        0,
        "commonwealth/primary",
    )
    .unwrap();
    assert_ne!(r1.sha, r2.sha);
    let head_note = note_json(tmp.path(), "HEAD");
    assert!(
        head_note["jobs"].get("job-aaaa1111").is_none(),
        "new commit's note must not carry prior jobs"
    );
    assert_eq!(head_note["jobs"]["job-bbbb2222"]["commit"], r2.sha.as_str());
    let prior_note = note_json(tmp.path(), "HEAD~1");
    assert_eq!(
        prior_note["jobs"]["job-aaaa1111"]["commit"],
        r1.sha.as_str(),
        "prior commit keeps its own note"
    );
}

#[test]
fn commit_reached_failure_is_reported_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(commit_reached(tmp.path(), "job-cccc3333", "g", 1, 1, 0, "m").is_err());
}
