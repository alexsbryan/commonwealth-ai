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

pub(crate) const RECIPE_A: &str = "[corpus]\nid = \"notes\"\nname = \"Notes\"\n";
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
                        // A real index: an install whose records cannot be made is
                        // unreadable, never held as "kept none".
                        crate::common::fixture_library::install(&dir, &id, &[], true).await;
                        corpus.stamp_recipe_sha256(&recipe_sha256(&toml)).unwrap();
                        Ok(IngestResult {
                            corpus_id: id,
                            chunks_created: 1,
                            index_size_bytes: 0,
                            duration_secs: 0,
                            docs_skipped: 0,
                            documents: Vec::new(),
                            documents_unlisted: 0,
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
pub(crate) async fn install(
    state: AppState,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    install_raw(state, body.to_string()).await
}

/// [`install`] with the body as sent, so a test sets its size to the byte.
async fn install_raw(state: AppState, body: String) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/oicp/v1/corpus/install")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .body(Body::from(body))
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
pub(crate) async fn settled(state: &AppState, corpus_id: &str) {
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

/// GET `/oicp/v1/corpus/progress` from loopback.
pub(crate) async fn progress(state: AppState) -> oicp_types::CorpusProgressResponse {
    let mut req = Request::get("/oicp/v1/corpus/progress")
        .header("host", "127.0.0.1:9741")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:55003".parse::<SocketAddr>().unwrap(),
    ));
    let resp = client_router(state).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The double's install of a supplied recipe that runs once `gate` is
/// notified, then reads one document and fails on another; a reinstall
/// removes the last one's directory.
fn gated_engine(indexes: std::path::PathBuf, gate: Arc<tokio::sync::Notify>) -> IngestPortDouble {
    use corpus_index::ingest_port::daemon::{DocumentOutcome, DocumentState};
    use kernel_types::Sha256Hash;

    let at = indexes.clone();
    let removes = indexes.clone();
    IngestPortDouble::new()
        .with_index_dir(indexes)
        .on_prepare_recipe_install(move |id, toml| {
            let (dir, id, toml, gate) =
                (at.clone(), id.to_string(), toml.to_string(), gate.clone());
            Ok(PreparedInstall {
                opts_out_of_auto_enrichment: true,
                run: Box::new(move |_progress| {
                    Box::pin(async move {
                        gate.notified().await;
                        let corpus = Corpus::named(&dir, &id).expect("an id");
                        // A real index: an install whose records cannot be made is
                        // unreadable, never held as "kept none".
                        crate::common::fixture_library::install(&dir, &id, &[], true).await;
                        corpus.stamp_recipe_sha256(&recipe_sha256(&toml)).unwrap();
                        Ok(IngestResult {
                            corpus_id: id,
                            chunks_created: 2,
                            index_size_bytes: 0,
                            duration_secs: 0,
                            docs_skipped: 1,
                            documents: vec![
                                DocumentOutcome {
                                    source: Some("okafor2019".into()),
                                    state: DocumentState::Ready {
                                        text_sha256: Some(Sha256Hash::of_str("okafor")),
                                    },
                                },
                                DocumentOutcome {
                                    source: Some("scans/broken.pdf".into()),
                                    state: DocumentState::Failed {
                                        reason: "custom extractor 'pdf' failed: no text layer"
                                            .into(),
                                    },
                                },
                            ],
                            documents_unlisted: 0,
                        })
                    })
                }),
            })
        })
        .on_remove_corpus_everything(move |id| {
            let _ = std::fs::remove_dir_all(removes.join(id));
            Ok(())
        })
}

/// OICP v0.6 §2.9: the install is listed on the first poll after its 202,
/// before the engine has reported any progress, and once it returns its
/// entry lists each document: the ready ones by their text names, the
/// unreadable one failed with its reason, none dropped. Red against the
/// parent, whose progress listed only what the engine's callback had
/// written and carried no documents.
#[tokio::test]
async fn an_install_is_listed_from_its_spawn_and_then_lists_each_document() {
    use kernel_types::Sha256Hash;
    use oicp_types::{IngestDocumentState, IngestPhase};

    let tmp = tempfile::tempdir().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let engine = gated_engine(tmp.path().to_path_buf(), release.clone());
    let state = state(Arc::new(engine));

    let (status, body) = install(
        state.clone(),
        serde_json::json!({"corpus_id": "notes", "recipe_toml": RECIPE_A}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spawned"], true);
    let first = progress(state.clone()).await;
    let entry = first
        .progress
        .get("notes")
        .unwrap_or_else(|| panic!("listed on the first poll after its 202: {first:?}"));
    assert_eq!(entry.phase, IngestPhase::Pending);
    assert!(entry.documents.is_empty(), "no documents before it returns");

    release.notify_one();
    settled(&state, "notes").await;
    let done = progress(state.clone()).await;
    let docs = &done.progress["notes"].documents;
    assert_eq!(docs.len(), 2, "{done:?}");
    assert_eq!(docs[0].source.as_deref(), Some("okafor2019"));
    assert_eq!(
        docs[0].state,
        IngestDocumentState::Ready {
            text_sha256: Some(Sha256Hash::of_str("okafor").to_hex()),
            record: None,
        }
    );
    assert_eq!(docs[1].source.as_deref(), Some("scans/broken.pdf"));
    assert!(
        matches!(&docs[1].state, IngestDocumentState::Failed { reason } if reason.contains("no text layer")),
        "{:?}",
        docs[1]
    );
    assert_eq!(done.progress["notes"].documents_unlisted, None);
}

/// Platform row PV3g (zoracite 611ef02): a held corpus re-installed under a
/// new recipe is `pending` on the first poll after its 202, with no
/// documents, never the last install's `complete` (v0.6 §2.9 "From the
/// spawn"; the documents reset, "a new attempt starts it again"). Red against
/// d95ba219e, whose progress kept the last install's terminal entry until the
/// engine's first callback, so zoracite's Q1 and Q3 live tests read the
/// library installed with no documents in it.
#[tokio::test]
async fn a_reinstall_is_pending_from_its_spawn_not_the_last_install_complete() {
    use oicp_types::IngestPhase;

    let tmp = tempfile::tempdir().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let state = state(Arc::new(gated_engine(
        tmp.path().to_path_buf(),
        release.clone(),
    )));
    let (status, body) = install(
        state.clone(),
        serde_json::json!({"corpus_id": "notes", "recipe_toml": RECIPE_A}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spawned"], true, "{body}");
    release.notify_one();
    settled(&state, "notes").await;
    assert_eq!(
        progress(state.clone()).await.progress["notes"].phase,
        IngestPhase::Complete
    );

    let (status, body) = install(
        state.clone(),
        serde_json::json!({"corpus_id": "notes", "recipe_toml": RECIPE_B}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spawned"], true, "a new recipe reads again: {body}");
    let first = progress(state.clone()).await;
    let entry = &first.progress["notes"];
    assert_eq!(entry.phase, IngestPhase::Pending, "{first:?}");
    assert!(entry.documents.is_empty(), "{first:?}");

    release.notify_one();
    settled(&state, "notes").await;
    let done = progress(state).await;
    assert_eq!(done.progress["notes"].phase, IngestPhase::Complete);
    assert_eq!(done.progress["notes"].documents.len(), 2, "{done:?}");
}

/// The double's install of a supplied recipe that reads one document and
/// fails on another, as `an_install_is_listed_from_its_spawn_…`'s does, with
/// no gate: a library with one unreadable file.
fn documented_engine(indexes: std::path::PathBuf) -> IngestPortDouble {
    use corpus_index::ingest_port::daemon::{DocumentOutcome, DocumentState};
    let at = indexes.clone();
    let removes = indexes.clone();
    IngestPortDouble::new()
        .with_index_dir(indexes)
        .on_prepare_recipe_install(move |id, toml| {
            let (dir, id, toml) = (at.clone(), id.to_string(), toml.to_string());
            Ok(PreparedInstall {
                opts_out_of_auto_enrichment: true,
                run: Box::new(move |_progress| {
                    Box::pin(async move {
                        let corpus = Corpus::named(&dir, &id).expect("an id");
                        // A real index: an install whose records cannot be made is
                        // unreadable, never held as "kept none".
                        crate::common::fixture_library::install(
                            &dir,
                            &id,
                            &[crate::common::fixture_library::Doc {
                                name: "okafor2019".into(),
                                metadata: None,
                                text: "okafor".into(),
                            }],
                            true,
                        )
                        .await;
                        corpus.stamp_recipe_sha256(&recipe_sha256(&toml)).unwrap();
                        Ok(IngestResult {
                            corpus_id: id,
                            chunks_created: 1,
                            index_size_bytes: 0,
                            duration_secs: 0,
                            docs_skipped: 1,
                            documents: vec![
                                DocumentOutcome {
                                    source: Some("okafor2019".into()),
                                    state: DocumentState::Ready {
                                        text_sha256: Some(kernel_types::Sha256Hash::of_str(
                                            "okafor",
                                        )),
                                    },
                                },
                                DocumentOutcome {
                                    source: Some("scans/broken.pdf".into()),
                                    state: DocumentState::Failed {
                                        reason: "no text layer".into(),
                                    },
                                },
                            ],
                            documents_unlisted: 0,
                        })
                    })
                }),
            })
        })
        .on_remove_corpus_everything(move |id| {
            let _ = std::fs::remove_dir_all(removes.join(id));
            Ok(())
        })
}

/// Platform row PH (zoracite orders/s6-lean-route): a held corpus's
/// documents survive a restart. A corpus installed with one unreadable file,
/// then a new daemon over the same indexes with nothing in memory: the same
/// install is `spawned: false`, and progress still lists the corpus complete
/// with the ready document and the failed one. Red against the parent, whose
/// list lived only in the daemon's memory, so the restarted daemon listed no
/// such corpus and §2.9 and v0.4 §5.3 rule 4 together said nothing of its
/// documents.
#[tokio::test]
async fn a_held_corpus_lists_its_documents_after_a_restart() {
    use oicp_types::{IngestDocumentState, IngestPhase};

    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().to_path_buf();
    let body = serde_json::json!({"corpus_id": "notes", "recipe_toml": RECIPE_A});

    let before = state(Arc::new(documented_engine(indexes.clone())));
    let (status, reply) = install(before.clone(), body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["spawned"], true, "{reply}");
    settled(&before, "notes").await;

    let after = state(Arc::new(documented_engine(indexes)));
    let (status, reply) = install(after.clone(), body).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["spawned"], false, "the corpus is held: {reply}");
    let listed = progress(after).await;
    let entry = listed
        .progress
        .get("notes")
        .unwrap_or_else(|| panic!("a held corpus is listed after a restart: {listed:?}"));
    assert_eq!(entry.phase, IngestPhase::Complete);
    assert_eq!(entry.documents.len(), 2, "{entry:?}");
    assert_eq!(entry.documents[0].source.as_deref(), Some("okafor2019"));
    assert!(matches!(
        entry.documents[0].state,
        IngestDocumentState::Ready { .. }
    ));
    assert_eq!(
        entry.documents[1].source.as_deref(),
        Some("scans/broken.pdf")
    );
    assert!(matches!(
        entry.documents[1].state,
        IngestDocumentState::Failed { .. }
    ));
}

/// A corpus stamped with the same recipe but holding no documents record (an
/// install from before PH) is not answered `spawned: false`, which would
/// leave its documents unknown for good: the install reads it again, and its
/// documents are listed.
#[tokio::test]
async fn a_held_corpus_with_no_documents_record_is_read_again() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().to_path_buf();
    let corpus = Corpus::named(&indexes, "notes").unwrap();
    std::fs::create_dir_all(corpus.root()).unwrap();
    std::fs::write(corpus.meta_path(), "{}").unwrap();
    corpus
        .stamp_recipe_sha256(&recipe_sha256(RECIPE_A))
        .unwrap();

    let state = state(Arc::new(documented_engine(indexes)));
    let (status, reply) = install(
        state.clone(),
        serde_json::json!({"corpus_id": "notes", "recipe_toml": RECIPE_A}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["spawned"], true, "no documents record: {reply}");
    settled(&state, "notes").await;
    assert_eq!(progress(state).await.progress["notes"].documents.len(), 2);
}

/// The `max_install_bytes` the manifest states (OICP v0.6 §2.12).
async fn stated_limit(state: AppState) -> usize {
    let mut req = Request::get("/oicp/v1/capabilities")
        .header("host", "127.0.0.1:9741")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:55003".parse::<SocketAddr>().unwrap(),
    ));
    let resp = client_router(state).oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let m: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let limit = m["knowledge"]["ingest"]["max_install_bytes"].as_u64();
    usize::try_from(limit.expect("the manifest states max_install_bytes")).unwrap()
}

/// An install of `notes` exactly `bytes` long: RECIPE_A's table, then a TOML
/// comment padded with `x` to the size.
fn padded_install(bytes: usize) -> String {
    let head =
        r#"{"corpus_id":"notes","recipe_toml":"[corpus]\nid = \"notes\"\nname = \"Notes\"\n# "#;
    let tail = r#""}"#;
    let pad = bytes - head.len() - tail.len();
    format!("{head}{}{tail}", "x".repeat(pad))
}

/// v0.6 §2.12: an inline library whose install body is exactly the stated
/// limit, far past the client surface's 8 MiB, is read and installed. Red
/// against the parent's route, which read every body to the surface's bound
/// and refused a 22,627,179-byte library `413` before the handler ran.
#[tokio::test]
async fn an_install_of_the_stated_size_past_the_surface_limit_installs() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(Arc::new(engine(tmp.path().to_path_buf())));
    let limit = stated_limit(state.clone()).await;
    assert!(
        limit > sovereign_daemon::server::MAX_REQUEST_BODY_BYTES,
        "the install limit must be past the surface's, or §2.12 changes nothing: {limit}"
    );
    let body = padded_install(limit);
    assert_eq!(body.len(), limit);
    let (status, reply) = install_raw(state.clone(), body).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["spawned"], true, "{reply}");
    settled(&state, "notes").await;
}

/// One byte past the stated limit is `413 install too large`, naming the
/// limit, never axum's unnamed 413 and never a prefix read.
#[tokio::test]
async fn an_install_one_byte_past_the_stated_limit_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(Arc::new(engine(tmp.path().to_path_buf())));
    let limit = stated_limit(state.clone()).await;
    let (status, reply) = install_raw(state.clone(), padded_install(limit + 1)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{reply}");
    assert_eq!(reply["error"], "install too large", "{reply}");
    assert_eq!(reply["max_install_bytes"], limit as u64, "{reply}");
    assert!(
        !state
            .inner
            .ingest
            .active_ingests
            .read()
            .await
            .contains("notes"),
        "nothing of a refused install runs"
    );
}
