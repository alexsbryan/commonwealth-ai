// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two D6 surfaces this batch opens over stores the daemon already
//! holds: notes CRUD (`notes_http`) and the recipe-author project store
//! (`features_http`).
//!
//! D2's owed route (`GET /v1/insights/sinks`) is tested beside its four
//! siblings in `loopback_parity.rs`, which already carries the
//! `InsightService` fixture — a second copy of that fixture here would
//! be the twin this campaign deletes.
//!
//! Exercised against real stores — a real `notes.db` and a real
//! `features.db` in a temp dir, opened the way production opens them —
//! because the fault these routes exist to prevent is TWO handles on one
//! file, and a stubbed store cannot exhibit it.
//!
//! # Red-watch (2026-09-10, run, not asserted)
//!
//! Both routers were emptied — handlers and DTOs left in place, so the
//! suite still built — and these cases re-run against the routerless
//! daemon. ALL FIVE failed, `pass: 0 fail: 5`:
//!
//! ```text
//! notes_crud_round_trips_…                    :103  left: 404  right: 201
//!                                                   "a write answers CREATED"
//! retire_keeps_the_row_and_delete_removes_it  :227  decode EOF (404, empty body)
//! note_get_404s_on_an_unknown_id_…            :293  decode EOF (404, empty body)
//! feature_projects_round_trip_…               :349  decode EOF (404, empty body)
//! project_get_404s_on_an_unknown_id_…         :424  decode EOF (404, empty body)
//! ```
//!
//! Two need their caveat stated here rather than only in a log.
//! `note_get_404s_on_an_unknown_id_…` and
//! `project_get_404s_on_an_unknown_id_…` assert a 404, and a routerless
//! daemon 404s too — so their STATUS lines passed under sabotage. What
//! made each red is the line after: the body must PARSE and must name
//! the id, and an empty 404 body cannot. The status alone is not a gate
//! (ARCH §18.1).

use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::features_http::features_router;
use sovereign_mesh::notes_http::notes_router;
use sovereign_store::recipe_project_store::RecipeProjectStore;

use crate::common;
use crate::common::spawn_router;

/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly. The scoped allow is
/// `atlas_surface_e2e`'s.
#[allow(clippy::unwrap_used)]
async fn build_store_daemon(
    with_features: bool,
) -> (Arc<EmbeddedDaemon>, Arc<NoteStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&indexes).unwrap();
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(CorpusEngine::new(
        recipes,
        indexes,
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) })),
    ));

    let notes = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
    let features = with_features
        .then(|| Arc::new(RecipeProjectStore::open(&tmp.path().join("features.db")).unwrap()));

    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_note_and_feature_stores(engine, Arc::clone(&notes), features),
    );
    (daemon, notes, tmp)
}

// ── Notes CRUD (D6) ───────────────────────────────────────────────

/// The four store operations the desktop's lesson pane performs — write,
/// read back, amend the payload, delete — all round-trip through the
/// daemon's OWN `notes.db`.
///
/// The write is asserted by READING IT BACK through a different route,
/// not by trusting the write's own 201: a handler that answered a
/// plausible id without persisting anything would pass a status check.
#[tokio::test]
async fn notes_crud_round_trips_through_the_daemons_own_store() {
    let (daemon, notes, _tmp) = build_store_daemon(false).await;
    let addr = spawn_router(notes_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();
    let base = format!("http://{addr}/v1/notes");

    // CREATE
    let resp = http
        .post(&base)
        .json(&serde_json::json!({
            "kind": "lesson",
            "content": "Prefer the wire form.",
            "session_id": "conv-1",
            "scope": "global",
            "source": "agent",
            "payload_json": "{\"enabled\":true}",
        }))
        .send()
        .await
        .expect("notes_router reachable");
    assert_eq!(resp.status(), 201, "a write answers CREATED");
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().expect("the store's id").to_string();

    // The row is in the DAEMON's store, not merely in the response.
    let row = notes
        .read_note_by_id(&id)
        .await
        .unwrap()
        .expect("the route wrote through to the store the daemon holds");
    assert_eq!(row.content, "Prefer the wire form.");

    // GET
    let one: serde_json::Value = http
        .get(format!("{base}/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["kind"], "lesson");
    assert_eq!(
        one["payload_json"], "{\"enabled\":true}",
        "the payload crosses as an opaque STRING — this router must not \
         re-encode a schema it does not own: {one:#?}",
    );

    // LIST, filtered by kind. A POST because the filter carries lists —
    // `atlas_http`'s call, made here for the same reason.
    let listed: serde_json::Value = http
        .post(format!("{base}/query"))
        .json(&serde_json::json!({
            "kinds": ["lesson"], "limit": 500, "include_retired": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = listed["notes"].as_array().expect("the notes envelope");
    assert_eq!(rows.len(), 1, "one lesson written, one listed: {listed:#?}");
    assert_eq!(rows[0]["id"], id.as_str());

    // A kind nobody wrote is an empty list, not an error.
    let empty: serde_json::Value = http
        .post(format!("{base}/query"))
        .json(&serde_json::json!({ "kinds": ["no-such-kind"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty["notes"].as_array().unwrap().len(), 0);

    // PATCH the payload, and read the NEW bytes back.
    let patched: serde_json::Value = http
        .patch(format!("{base}/{id}/payload"))
        .json(&serde_json::json!({ "payload_json": "{\"enabled\":false}" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["existed"], true);
    let after: serde_json::Value = http
        .get(format!("{base}/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after["payload_json"], "{\"enabled\":false}",
        "the patch must persist — `existed: true` on its own proves only \
         that a row was found: {after:#?}",
    );

    // DELETE, then prove it is gone.
    let deleted: serde_json::Value = http
        .delete(format!("{base}/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(deleted["existed"], true);
    assert_eq!(
        http.get(format!("{base}/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        404,
        "a hard delete is real deletion, no tombstone",
    );
}

/// Retire is not delete: the row survives, stamped, so a successor can
/// point back at it. That distinction is the whole reason the lesson
/// pane can render a supersede chain.
#[tokio::test]
async fn retire_keeps_the_row_and_delete_removes_it() {
    let (daemon, _notes, _tmp) = build_store_daemon(false).await;
    let addr = spawn_router(notes_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();
    let base = format!("http://{addr}/v1/notes");

    let created: serde_json::Value = http
        .post(&base)
        .json(&serde_json::json!({
            "kind": "lesson", "content": "The old rung.",
            "session_id": "conv-1", "scope": "global", "source": "agent",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let retired: serde_json::Value = http
        .post(format!("{base}/{id}/retire"))
        .json(&serde_json::json!({ "reason": "superseded by the new rung" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retired["existed"], true);

    let row: serde_json::Value = http
        .get(format!("{base}/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        row["retired_at"].is_i64(),
        "a retired note is STILL THERE, stamped — deleting it would \
         break the chain the pane renders: {row:#?}",
    );
    assert_eq!(row["retired_by"], "superseded by the new rung");

    // A default list hides it; `include_retired` shows it. Both are real
    // answers, and the pane depends on the difference.
    let hidden: serde_json::Value = http
        .post(format!("{base}/query"))
        .json(&serde_json::json!({ "kinds": ["lesson"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hidden["notes"].as_array().unwrap().len(), 0);
    let shown: serde_json::Value = http
        .post(format!("{base}/query"))
        .json(&serde_json::json!({ "kinds": ["lesson"], "include_retired": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shown["notes"].as_array().unwrap().len(), 1);
}

/// An unknown id is a 404 that NAMES it, and an unrecognised
/// `scope`/`source` is a 400 that names the value — never a silent fall
/// back to `global`/`agent`, which is how a node-local note ends up on
/// the mesh (ARCH §18.3).
#[tokio::test]
async fn note_get_404s_on_an_unknown_id_and_a_bad_scope_is_refused() {
    let (daemon, _notes, _tmp) = build_store_daemon(false).await;
    let addr = spawn_router(notes_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();
    let base = format!("http://{addr}/v1/notes");

    let resp = http.get(format!("{base}/nope-1234")).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("nope-1234"),
        "the 404 must NAME the id — the status alone is not a gate, a \
         routerless daemon 404s too: {body:#?}",
    );

    let resp = http
        .post(&base)
        .json(&serde_json::json!({
            "kind": "lesson", "content": "x", "session_id": "s",
            "scope": "planetary", "source": "agent",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "an unknown scope is refused, not guessed"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("planetary"),
        "the refusal must name the value it refused: {body:#?}",
    );

    // A delete of a row that was never there is an ANSWER, not an error.
    let gone: serde_json::Value = http
        .delete(format!("{base}/never-existed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        gone["existed"], false,
        "`existed: false` is the reported absence; a bare 204 would make \
         the caller infer it",
    );
}

// ── The recipe-author project store (D6) ──────────────────────────

/// Provision, read back, list — the three store operations, over the
/// daemon's own `features.db`.
#[tokio::test]
async fn feature_projects_round_trip_and_a_duplicate_id_is_a_conflict() {
    let (daemon, _notes, _tmp) = build_store_daemon(true).await;
    let addr = spawn_router(features_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();
    let base = format!("http://{addr}/v1/features/projects");

    // A fresh install lists nothing — and that is a 200, because the
    // Welcome pane branches on the empty list to show its tutorial.
    let empty: serde_json::Value = http.get(&base).send().await.unwrap().json().await.unwrap();
    assert_eq!(
        empty["projects"].as_array().unwrap().len(),
        0,
        "'no projects yet' is an answer, not a 404: {empty:#?}",
    );

    let resp = http
        .post(&base)
        .json(&serde_json::json!({
            "id": "feat-quiet-hours",
            "title": "Quiet hours",
            "charter_md": "# Charter\n\nMake the levy legible.",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let row: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(row["title"], "Quiet hours");
    assert!(
        row["created_at"].as_i64().unwrap() > 0,
        "the provision answers the ROW, so a caller need not GET it back \
         to learn the timestamps: {row:#?}",
    );

    let one: serde_json::Value = http
        .get(format!("{base}/feat-quiet-hours"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        one["charter_md"], "# Charter\n\nMake the levy legible.",
        "the charter crosses WHOLE — clipping it here would make this \
         route a decider about a length the sidebar owns: {one:#?}",
    );

    let listed: serde_json::Value = http.get(&base).send().await.unwrap().json().await.unwrap();
    assert_eq!(listed["projects"].as_array().unwrap().len(), 1);

    // Same id twice is the CALLER's mistake and is actionable — a retry
    // with this body can never succeed — so it is a 409, not a 500.
    let dup = http
        .post(&base)
        .json(&serde_json::json!({
            "id": "feat-quiet-hours", "title": "Again", "charter_md": "",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), 409);
    let body: serde_json::Value = dup.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("already exists"),
        "the conflict must say what conflicted: {body:#?}",
    );
}

/// An unknown project id is a 404 naming it; a daemon whose
/// `features.db` never opened is a 503 naming THAT. Two different facts,
/// which is the whole reason the field is an `Option` rather than a
/// route that is simply not mounted (ARCH §18.3).
#[tokio::test]
async fn project_get_404s_on_an_unknown_id_and_no_store_is_a_named_503() {
    let (daemon, _notes, _tmp) = build_store_daemon(true).await;
    let addr = spawn_router(features_router(Arc::clone(&daemon))).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/features/projects/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("nope"),
        "the 404 must NAME the id — the status alone is not a gate: {body:#?}",
    );

    let (bare, _notes, _tmp2) = build_store_daemon(false).await;
    let addr = spawn_router(features_router(Arc::clone(&bare))).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/features/projects"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "no features.db is a SERVICE state, not an empty list — an empty \
         200 would tell the pane the user has no projects",
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("features.db"),
        "the 503 must name what did not open: {body:#?}",
    );
}
