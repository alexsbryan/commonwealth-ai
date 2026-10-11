// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP v0.5 §4, install carrying its recipe (ADDRESSED_TEXT §5.6), driven
//! through the real `client_router`.
//!
//! The node's engine is `IngestPortDouble`: the daemon's decisions (the
//! status an outcome answers, idempotence by the recipe's bytes, reingest on
//! a different recipe) are the subject. What the engine does with the recipe
//! — the one load boundary, `Recipe::from_toml`, and the stamp its run
//! writes — is corpus-engine's `recipe_install_port`. Here the double's run
//! leaves what that run leaves: a canonical corpus and its recipe stamp.
//!
//! Until 2026-10-08 the route took no recipe and projected the install
//! outcome to a bool, so an invalid recipe, invalid parameters and an
//! unknown id all answered 200 `spawned: false` (appendix defect 2).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use corpus_index::corpus::{recipe_sha256, Corpus};
use corpus_index::ingest_port::daemon::{IngestResult, InstallRefusal, PreparedInstall};
use corpus_index::ingest_port::double::IngestPortDouble;
use kernel_types::NodeId;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{fabric, node, serving, AppState};
use tower::ServiceExt;

use crate::common::ledger_double::RecordingLedger;

const RECIPE_A: &str = "[corpus]\nid = \"notes\"\nname = \"Notes\"\n";
const RECIPE_B: &str = "[corpus]\nid = \"notes\"\nname = \"Notes, revised\"\n";

/// The double's install of a supplied recipe: a stand-in loader (the real
/// one is corpus-engine's), and a run that leaves a canonical corpus with
/// its recipe stamp, as the engine's run does.
fn engine(indexes: std::path::PathBuf) -> IngestPortDouble {
    let at = indexes.clone();
    IngestPortDouble::new()
        .with_index_dir(indexes)
        .on_prepare_recipe_install(move |id, toml| {
            if !toml.contains("[corpus]") {
                return Err(InstallRefusal::InvalidRecipe(
                    "TOML parse error: expected `[corpus]`".into(),
                ));
            }
            let (dir, id, toml) = (at.clone(), id.to_string(), toml.to_string());
            Ok(PreparedInstall {
                opts_out_of_auto_enrichment: true,
                run: Box::new(move |_progress| {
                    Box::pin(async move {
                        let corpus = Corpus::named(&dir, &id).expect("an id");
                        std::fs::create_dir_all(corpus.root()).unwrap();
                        std::fs::write(corpus.meta_path(), "{}").unwrap();
                        corpus.stamp_recipe_sha256(&recipe_sha256(&toml)).unwrap();
                        Ok(IngestResult {
                            corpus_id: id,
                            chunks_created: 1,
                            index_size_bytes: 0,
                            duration_secs: 0,
                            docs_skipped: 0,
                        })
                    })
                }),
            })
        })
        .on_prepare_registry_install(|id| {
            Err(InstallRefusal::RecipeNotFound(format!(
                "No registry entry for corpus {id}"
            )))
        })
}

fn state(engine: Arc<IngestPortDouble>) -> AppState {
    let id = NodeId::from_u128(1);
    AppState::new_with_seeds(
        id,
        Some(engine),
        None,
        fabric::FabricSeed::default(),
        serving::ServingSeed::default(),
        node::NodeSeed::default(),
        Arc::new(RecordingLedger::new(id)).seed(),
    )
}

/// POST `/oicp/v1/corpus/install` from loopback; the status and the body.
async fn install(state: AppState, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/oicp/v1/corpus/install")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:55003".parse::<SocketAddr>().unwrap(),
    ));
    let resp = client_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// Wait until no install of `corpus_id` is in flight.
async fn settled(state: &AppState, corpus_id: &str) {
    for _ in 0..200 {
        if !state
            .inner
            .ingest
            .active_ingests
            .read()
            .await
            .contains(corpus_id)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the install of {corpus_id} never settled");
}

/// A recipe that does not load is the caller's to fix: 400, naming it.
/// Red against the parent's handler, which ignored `recipe_toml` and answered
/// 200 `spawned: false`.
#[tokio::test]
async fn a_recipe_that_does_not_load_is_400() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(Arc::new(engine(tmp.path().to_path_buf())));
    let (status, body) = install(
        state,
        serde_json::json!({"corpus_id": "notes", "recipe_toml": "this is not a recipe"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("invalid recipe"),
        "the refusal must name the recipe: {body}"
    );
}

/// The same `(corpus_id, recipe_sha256)` twice is `spawned: false`, and the
/// reply names the sha both times; a different recipe for the installed
/// corpus reingests it, removing the old index first. Red against the
/// parent's handler, which installed by id and spawned nothing.
#[tokio::test]
async fn a_second_identical_install_is_spawned_false_and_a_new_recipe_reingests() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().to_path_buf();
    let removed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&removed);
    let at = indexes.clone();
    let engine = Arc::new(
        engine(indexes.clone()).on_remove_corpus_everything(move |id| {
            seen.lock().unwrap().push(id.to_string());
            let _ = std::fs::remove_dir_all(at.join(id));
            Ok(())
        }),
    );
    let state = state(Arc::clone(&engine));
    let first = serde_json::json!({"corpus_id": "notes", "recipe_toml": RECIPE_A});

    let (status, body) = install(state.clone(), first.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spawned"], true, "{body}");
    assert_eq!(body["recipe_sha256"], recipe_sha256(RECIPE_A), "{body}");
    settled(&state, "notes").await;
    assert_eq!(
        Corpus::recipe_sha256_in(indexes.join("notes")),
        Some(recipe_sha256(RECIPE_A)),
        "the run stamps the recipe"
    );

    let (status, body) = install(state.clone(), first).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spawned"], false,
        "an identical install spawned again: {body}"
    );
    assert_eq!(body["recipe_sha256"], recipe_sha256(RECIPE_A));
    assert!(removed.lock().unwrap().is_empty(), "nothing was replaced");

    let (status, body) = install(
        state.clone(),
        serde_json::json!({"corpus_id": "notes", "recipe_toml": RECIPE_B}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spawned"], true, "a new recipe must reingest: {body}");
    settled(&state, "notes").await;
    assert_eq!(*removed.lock().unwrap(), vec!["notes".to_string()]);
    assert_eq!(
        Corpus::recipe_sha256_in(indexes.join("notes")),
        Some(recipe_sha256(RECIPE_B))
    );
}

/// By id, an id the registry does not know is a 404 and invalid parameters a
/// 400, the statuses the internal route already answered; until 2026-10-08
/// the OICP route answered both 200 `spawned: false`.
#[tokio::test]
async fn by_id_an_unknown_corpus_is_404_and_bad_parameters_are_400() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(Arc::new(engine(tmp.path().to_path_buf())));
    let (status, body) = install(state, serde_json::json!({"corpus_id": "nope"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let tmp = tempfile::tempdir().unwrap();
    let engine = IngestPortDouble::new()
        .with_index_dir(tmp.path())
        .on_prepare_registry_install(|_| {
            Err(InstallRefusal::InvalidParameters(
                "missing required parameter `cik`".into(),
            ))
        });
    let (status, body) = install(
        state_from(engine),
        serde_json::json!({"corpus_id": "sec", "parameters": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

fn state_from(engine: IngestPortDouble) -> AppState {
    state(Arc::new(engine))
}
