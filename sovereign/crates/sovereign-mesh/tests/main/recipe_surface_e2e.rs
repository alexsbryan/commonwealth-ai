// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-registry write and read the desktop stops doing in-process
//! (thin-desktop order, 2026-09-11, `recipe_http`): importing an authored
//! recipe into the local registry and reading a recipe's `[parameters]`.
//!
//! Against a REAL daemon whose engine owns a temp recipes dir — the fault
//! these routes exist to prevent is the desktop writing a recipe into a
//! directory the daemon's registry does not resolve through.

use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::recipe_http::recipe_router;

use crate::common;
use crate::common::spawn_router;

const EMBED_DIM: usize = 8;

/// A recipe that passes offline validation (no reachability check on the
/// download URL) and declares two parameters, one with a default.
const RECIPE_TOML: &str = r#"
[corpus]
id = "author-test"
name = "Author test"
description = "an authored recipe"
license = "Public Domain"
size_compressed_gb = 0.001
size_indexed_gb = 0.001

[parameters.since]
type = "date"
required = false
description = "Only records after this date."
default = "2020-01-01"

[parameters.tickers]
type = "list"
required = true
description = "Tickers to include."

[acquire]
type = "bulk_download"
url = "https://example.invalid/author-test.txt"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 256

[index]
fts = true
vector = true
"#;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly (`meshapp_surface_e2e`'s allow).
#[allow(clippy::unwrap_used)]
async fn build_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&indexes).unwrap();
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes.clone(), indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_engine(engine),
    );
    (daemon, tmp, recipes)
}

/// A valid recipe lands under the DAEMON's recipes dir as
/// `<id>/recipe.toml` with a `registry.toml` entry carrying its sha256,
/// and is then resolvable by the parameters route through the same
/// registry — the round trip the install form depends on.
#[tokio::test]
async fn import_writes_the_daemons_registry_and_parameters_read_it_back() {
    let (daemon, _tmp, recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/import"))
        .json(&serde_json::json!({ "toml_text": RECIPE_TOML }))
        .send()
        .await
        .expect("recipe_router reachable");
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    assert_eq!(status, 200, "{body:#?}");
    assert_eq!(body["success"], true, "a valid recipe imports: {body:#?}");
    assert_eq!(body["corpus_id"], "author-test");
    let landed = recipes.join("author-test").join("recipe.toml");
    assert_eq!(
        body["recipe_path"],
        landed.display().to_string(),
        "the path answered is the daemon's, not the client's"
    );
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), RECIPE_TOML);
    let registry = std::fs::read_to_string(recipes.join("registry.toml")).unwrap();
    assert!(
        registry.contains("id = \"author-test\"") && registry.contains("sha256 = \""),
        "the local registry carries the entry and its digest:\n{registry}"
    );

    let resp = client
        .get(format!(
            "http://{addr}/internal/corpus/recipes/author-test/parameters"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let schema: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(schema["corpus_id"], "author-test");
    let params = schema["parameters"].as_array().unwrap();
    assert_eq!(params.len(), 2);
    let since = params.iter().find(|p| p["name"] == "since").unwrap();
    assert_eq!(since["kind"], "date");
    assert_eq!(since["required"], false);
    assert_eq!(
        since["default"], "2020-01-01",
        "the TOML default crosses as JSON"
    );
    let tickers = params.iter().find(|p| p["name"] == "tickers").unwrap();
    assert_eq!(tickers["kind"], "list");
    assert_eq!(tickers["required"], true);
    assert_eq!(tickers["default"], serde_json::Value::Null);
}

/// A recipe that parses but fails validation is a 200 with
/// `success: false` and the errors — the form's own verdict shape — and
/// nothing is written; a body that is not a recipe is a 400.
#[tokio::test]
async fn import_refuses_an_invalid_recipe_without_writing_and_400s_on_non_toml() {
    let (daemon, _tmp, recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();

    // No `[acquire]`/`[extract]`/`[chunk]` — parses as a recipe? It must
    // not: the schema requires them, so this is the 400 arm.
    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/import"))
        .json(&serde_json::json!({ "toml_text": "[corpus]\nid = \"broken\"\n" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "not a recipe is a refusal");
    assert!(
        !recipes.join("broken").exists(),
        "nothing is written on refusal"
    );

    // Parameters for a recipe nobody imported: a 404 naming it.
    let resp = client
        .get(format!(
            "http://{addr}/internal/corpus/recipes/no-such-recipe/parameters"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("no-such-recipe"));
}

// ─── The recipe dry run (svt-6) ────────────────────────────────

/// A recipe whose acquirer is a LOCAL FILE, so a sampled run really
/// acquires, extracts and chunks with no network reachable at all — the
/// job arm is exercised end to end rather than through its failure path.
fn local_recipe(source: &std::path::Path, name: &str) -> String {
    format!(
        r#"
[corpus]
id = "dry-run-test"
name = "{name}"
description = "a recipe tested over a local file"
license = "Public Domain"
size_compressed_gb = 0.001
size_indexed_gb = 0.001

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 0

[index]
fts = true
vector = false
"#,
        path = source.display()
    )
}

/// Two paragraphs, so chunking has something to count.
const SAMPLE_TEXT: &str = "Alpha paragraph, long enough to survive a chunker.\n\n\
                           Beta paragraph, likewise long enough to be kept.\n";

/// `sample_size: 0` is answered INLINE with the strict verdict, and
/// `source_reachable` distinguishes "not asked" from "asked and it did not
/// answer" (ARCH principle 6).
///
/// Watched to fail: `passed: report.passed()` in `dry_run_report` replaced
/// with `passed: true` — the empty-name case below goes red naming the
/// verdict. Reverted.
#[tokio::test]
async fn a_validation_only_dry_run_is_inline_and_reports_the_strict_verdict() {
    let (daemon, tmp, _recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();
    let source = tmp.path().join("source.txt");
    std::fs::write(&source, SAMPLE_TEXT).expect("write sample source");

    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/test"))
        .json(&serde_json::json!({
            "toml_text": local_recipe(&source, "Dry run test"),
            "sample_size": 0,
            "offline": true,
        }))
        .send()
        .await
        .expect("recipe_router reachable");
    assert_eq!(resp.status().as_u16(), 200, "sample 0 answers inline");
    let body: serde_json::Value = resp.json().await.expect("a report");
    assert_eq!(body["passed"], true, "{body:#?}");
    assert_eq!(body["recipe_id"], "dry-run-test");
    assert_eq!(body["recipe_name"], "Dry run test");
    assert_eq!(
        body["records_attempted"], 0,
        "validation only: extraction did not run"
    );
    assert_eq!(
        body["source_reachable"],
        serde_json::Value::Null,
        "offline: the probe was NOT ASKED, which is not `false`"
    );
    assert!(
        body["report_markdown"]
            .as_str()
            .expect("markdown inline")
            .contains("Dry run test"),
        "the rendered report travels inline, not a path: {body:#?}"
    );

    // Same recipe, `offline: false` — now the probe IS asked, so the field
    // carries a verdict rather than staying absent. (The URL is a
    // `file://`, so the answer is `false`; the ASKING is what is pinned.)
    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/test"))
        .json(&serde_json::json!({
            "toml_text": local_recipe(&source, "Dry run test"),
            "sample_size": 0,
            "offline": false,
        }))
        .send()
        .await
        .expect("recipe_router reachable");
    let body: serde_json::Value = resp.json().await.expect("a report");
    assert!(
        !body["source_reachable"].is_null(),
        "not offline: the probe ran and its verdict is reported: {body:#?}"
    );

    // A recipe that PARSES and fails validation is a 200 carrying the
    // failure — the positive control for `passed`, which is
    // `TestReport::passed()` and not "produced chunks".
    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/test"))
        .json(&serde_json::json!({
            "toml_text": local_recipe(&source, ""),
            "sample_size": 0,
            "offline": true,
        }))
        .send()
        .await
        .expect("recipe_router reachable");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.expect("a report");
    assert_eq!(
        body["passed"], false,
        "an empty `corpus.name` is a validation error: {body:#?}"
    );
    let errors = body["errors"].as_array().expect("errors array");
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().is_some_and(|s| s.contains("corpus.name"))),
        "the error names the field: {body:#?}"
    );
}

/// A SAMPLED dry run acquires, so it is a job: 202 + an ack naming its
/// progress route, the report arriving on that route. A second run of the
/// same recipe while one is in flight is refused by name, and a job id this
/// daemon never minted is a 404 rather than an idle-looking shape.
///
/// Watched to fail: the `live_dry_run_for` 409 branch deleted — the second
/// POST answers 202 and this goes red on the conflict assertion. Reverted.
#[tokio::test]
async fn a_sampled_dry_run_is_a_job_and_reports_its_own_report() {
    let (daemon, tmp, _recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();
    let source = tmp.path().join("source.txt");
    std::fs::write(&source, SAMPLE_TEXT).expect("write sample source");
    let toml_text = local_recipe(&source, "Dry run test");

    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/test"))
        .json(&serde_json::json!({
            "toml_text": toml_text, "sample_size": 5, "offline": true,
        }))
        .send()
        .await
        .expect("recipe_router reachable");
    assert_eq!(resp.status().as_u16(), 202, "a sample is a job, not a wait");
    let ack: serde_json::Value = resp.json().await.expect("an ack");
    assert_eq!(ack["ok"], true);
    assert_eq!(ack["corpus_id"], "dry-run-test");
    let job_id = ack["job_id"].as_str().expect("a job id").to_string();
    assert_eq!(
        ack["progress_route"],
        format!("/internal/corpus/recipes/test/{job_id}/progress"),
        "the ack names where to read it — a job id with no reporter is how a \
         caller invents a poll loop of its own"
    );

    // Poll the route the ack named until the run lands.
    let mut report = serde_json::Value::Null;
    for _ in 0..200 {
        let p: serde_json::Value = client
            .get(format!(
                "http://{addr}{}",
                ack["progress_route"].as_str().unwrap_or("")
            ))
            .send()
            .await
            .expect("progress route reachable")
            .json()
            .await
            .expect("progress json");
        assert_eq!(p["job_id"], job_id.as_str());
        assert_eq!(p["recipe_id"], "dry-run-test");
        match p["state"].as_str() {
            Some("running") => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Some("complete") => {
                report = p["report"].clone();
                break;
            }
            other => panic!("dry run reported {other:?}: {p:#?}"),
        }
    }
    assert!(!report.is_null(), "the sampled run never reported");
    assert_eq!(report["passed"], true, "{report:#?}");
    assert_eq!(
        report["records_attempted"], 1,
        "one local file, one record: {report:#?}"
    );
    assert!(
        report["total_chunks"].as_u64().unwrap_or(0) >= 1,
        "the sample really chunked: {report:#?}"
    );

    // A job id nobody minted is a 404 naming it — not a shape that reads
    // like a run which has not started yet.
    let resp = client
        .get(format!(
            "http://{addr}/internal/corpus/recipes/test/no-such-job/progress"
        ))
        .send()
        .await
        .expect("progress route reachable");
    assert_eq!(resp.status().as_u16(), 404);
    let body: serde_json::Value = resp.json().await.expect("an error body");
    assert!(
        body["error"].as_str().unwrap_or("").contains("no-such-job"),
        "{body:#?}"
    );
}

/// The job routes under `/internal/corpus/recipes/` must not shadow
/// `{corpus}/parameters`.
///
/// **This went red when it was written, and the defect was real (svt-6).**
/// With the progress routes spelled `.../recipes/test/{job}`, axum prefers
/// the STATIC `test` segment over `{corpus}`, so
/// `GET /internal/corpus/recipes/test/parameters` reached the DRY-RUN handler
/// and answered "no recipe dry run `parameters` on this daemon" — a recipe
/// whose id is `test` or `harness` could no longer have its install form
/// rendered. The fix is the trailing `/progress`: six segments cannot collide
/// with the five-segment `{corpus}/parameters`.
///
/// Watched to fail: drop `/progress` from either job route and the second
/// loop goes red naming the handler that answered instead.
#[tokio::test]
async fn the_dry_run_progress_route_does_not_shadow_the_parameters_route() {
    let (daemon, _tmp, _recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();

    // The job routes reach their own handlers.
    for family in ["test", "harness"] {
        let resp = client
            .get(format!(
                "http://{addr}/internal/corpus/recipes/{family}/some-job/progress"
            ))
            .send()
            .await
            .expect("recipe_router reachable");
        let body: serde_json::Value = resp.json().await.expect("json");
        assert!(
            body["error"].as_str().unwrap_or("").contains("some-job"),
            "`{family}/{{job}}/progress` must reach its own handler: {body:#?}"
        );
    }

    // And a recipe literally named `test` or `harness` still resolves its
    // parameter schema through the REGISTRY handler — the 404 names the
    // recipe, not a job.
    for family in ["test", "harness"] {
        let resp = client
            .get(format!(
                "http://{addr}/internal/corpus/recipes/{family}/parameters"
            ))
            .send()
            .await
            .expect("recipe_router reachable");
        let body: serde_json::Value = resp.json().await.expect("json");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains(&format!("recipe `{family}`")),
            "`{{corpus}}/parameters` must still reach the registry handler for \
             a recipe named `{family}`: {body:#?}"
        );
    }
}

/// The authoring harness is a daemon job over the daemon's OWN frozen-sample
/// store, and its card carries exactly the fields the ladder renders.
///
/// This is what retires the desktop's last `state.corpus_engine` read: the
/// rung-6 verify runs against `<index_dir>/<recipe-id>` on this side, and
/// `enrich: false` leaves the rung ABSENT rather than passing.
///
/// Watched to fail: `green: run.green()` in `run_harness_job` replaced with
/// `green: true` — the card assertion below stays green (this recipe passes),
/// so the sabotage that actually bites is `frozen_captured_now: false`
/// hardcoded, which goes red on the first-capture assertion. Both watched;
/// reverted.
#[tokio::test]
async fn the_authoring_harness_is_a_job_over_the_daemons_own_sample_store() {
    let (daemon, tmp, _recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();
    let source = tmp.path().join("source.txt");
    std::fs::write(&source, SAMPLE_TEXT).expect("write sample source");
    let toml_text = local_recipe(&source, "Dry run test");

    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/harness"))
        .json(&serde_json::json!({
            "toml_text": toml_text, "sample_size": 5, "enrich": false,
        }))
        .send()
        .await
        .expect("recipe_router reachable");
    assert_eq!(
        resp.status().as_u16(),
        202,
        "the harness captures on its first run, so it is never awaited"
    );
    let ack: serde_json::Value = resp.json().await.expect("an ack");
    let job_id = ack["job_id"].as_str().expect("a job id").to_string();
    assert_eq!(ack["corpus_id"], "dry-run-test");
    assert_eq!(
        ack["progress_route"],
        format!("/internal/corpus/recipes/harness/{job_id}/progress")
    );

    let mut card = serde_json::Value::Null;
    for _ in 0..400 {
        let p: serde_json::Value = client
            .get(format!(
                "http://{addr}/internal/corpus/recipes/harness/{job_id}/progress"
            ))
            .send()
            .await
            .expect("progress route reachable")
            .json()
            .await
            .expect("progress json");
        match p["state"].as_str() {
            Some("running") => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Some("complete") => {
                card = p["card"].clone();
                break;
            }
            _ => panic!("harness reported: {p:#?}"),
        }
    }
    assert!(!card.is_null(), "the harness run never reported");

    // The card's keys ARE the ladder's contract — `HarnessLadderCard.svelte`
    // reads each of these by name.
    for key in [
        "green",
        "run",
        "ran_at_unix",
        "frozen_docs",
        "frozen_captured_at",
        "frozen_captured_now",
    ] {
        assert!(
            card.get(key).is_some(),
            "the card is missing `{key}`, which the ladder renders: {card:#?}"
        );
    }
    assert_eq!(
        card["frozen_captured_now"], true,
        "the FIRST run performs the one networked capture: {card:#?}"
    );
    assert_eq!(card["frozen_docs"], 1, "one local file, one doc: {card:#?}");
    assert!(
        card["run"]["stages"]
            .as_array()
            .is_some_and(|s| !s.is_empty()),
        "the per-stage ladder crosses the wire: {card:#?}"
    );
    // `enrich: false` leaves rung 6 ABSENT — not a passing verdict it never
    // earned (ARCH principle 6).
    assert!(
        !card["run"]["stages"]
            .as_array()
            .expect("stages")
            .iter()
            .any(|s| s["stage"] == "enrich"),
        "rung 6 did not run and must not appear: {card:#?}"
    );

    // The sample is now frozen under the DAEMON's data root, not the
    // client's — the whole point of the route.
    assert!(
        tmp.path()
            .join("harness")
            .join("dry-run-test")
            .join("capture.json")
            .exists(),
        "the frozen sample lands under the daemon's own data dir"
    );

    // A second run reuses the frozen sample: no capture, and the ladder
    // still lands.
    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/harness"))
        .json(&serde_json::json!({
            "toml_text": toml_text, "sample_size": 5, "enrich": false,
        }))
        .send()
        .await
        .expect("recipe_router reachable");
    assert_eq!(resp.status().as_u16(), 202);
    let ack: serde_json::Value = resp.json().await.expect("an ack");
    let job_id = ack["job_id"].as_str().expect("a job id").to_string();
    let mut second = serde_json::Value::Null;
    for _ in 0..400 {
        let p: serde_json::Value = client
            .get(format!(
                "http://{addr}/internal/corpus/recipes/harness/{job_id}/progress"
            ))
            .send()
            .await
            .expect("progress route reachable")
            .json()
            .await
            .expect("progress json");
        match p["state"].as_str() {
            Some("running") => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Some("complete") => {
                second = p["card"].clone();
                break;
            }
            _ => panic!("harness reported: {p:#?}"),
        }
    }
    assert_eq!(
        second["frozen_captured_now"], false,
        "the second run is offline against the same frozen sample: {second:#?}"
    );

    // A job id nobody minted is a 404 naming it.
    let resp = client
        .get(format!(
            "http://{addr}/internal/corpus/recipes/harness/nope/progress"
        ))
        .send()
        .await
        .expect("progress route reachable");
    assert_eq!(resp.status().as_u16(), 404);
}
