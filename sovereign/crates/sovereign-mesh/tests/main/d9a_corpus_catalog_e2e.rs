// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus_catalog_http` end to end — the catalogue and the notebook
//! shelf (sv-surface D9a).
//!
//! The fixture writes real `_corpus_meta.json` files under the
//! engine's index dir, because `installed_indexes()` is the decider
//! both routes fold over and a stubbed engine cannot exhibit its two
//! filters (shards out, `ingestion_in_progress` out). This is
//! `loopback_parity`'s rung-1 fixture shape, deliberately: the status
//! route and these read the same directory and must agree about what
//! is in it.
//!
//! # The local-corpus singleton, and why the shelf test tolerates it
//!
//! `watched_folder_runtime` holds the manager in a process-wide
//! `OnceLock`, and `lc_surface_e2e` installs one in the same test
//! binary. Whichever file runs first wins, and `install` on the loser
//! is a silent no-op. So the shelf case here installs a manager (in
//! case it runs first) and then asserts ONLY on rows whose corpus ids
//! are tempdir-unique — no local registry of either fixture can carry
//! them, so `source_kind`, `name` and `scope` are decided by the
//! catalogue/installed branches and the answer is the same whichever
//! manager is live. Asserting on a vault row here would be a test
//! whose verdict depends on file order.
//!
//! # Red-watch (2026-09-10, run)
//!
//! Every route moved to a planted path
//! (`/internal/corpus/catalog-planted`, …) with the handlers, the
//! DTOs and the two loopback layers untouched. `pass: 0 fail: 4`:
//!
//! ```text
//! catalog_unions_the_builtins_with_the_installed_indexes_it_does_not_name
//! notebooks_shelf_names_and_orders_the_installed_set
//! health_and_retry_are_404_for_a_corpus_with_no_index
//! catalog_without_a_corpus_engine_is_the_named_503
//! ```
//!
//! The first two go red on the BODY — a fallback answers with no JSON,
//! so there is no `corpora` / `notebooks` key. The 404 case is the
//! interesting one: a planted path 404s TOO, so its status line proves
//! nothing, and it goes red only because the assertion under it
//! requires the error body to NAME the corpus it could not open (ARCH
//! §18.1). The 503 case goes red on the status line for the same
//! reason its sibling does, with the same naming assertion behind it.
//! Restored: 4/4 green.
//!
//! A SECOND sabotage covers the layer rather than the path: pulling
//! `loopback_only` off this router (and off `documents_http`) turns
//! `loopback_parity::every_router_refuses_a_request_no_handler_of_ours
//! _can_refuse` red on the new legs, reporting a 405 where the guard
//! should have answered 403. Restored: green.

use std::sync::Arc;

use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::corpus_catalog_http::corpus_catalog_router;
use sovereign_mesh::daemon::EmbeddedDaemon;

use crate::common::{desktop_services_with_engine, mesh_admin_services, spawn_router};

/// One installed corpus on disk, in the shape `installed_indexes()`
/// reads.
///
/// Through `CorpusIndex::create` + `mark_ingestion_complete`, NOT by
/// hand-writing `_corpus_meta.json`: the decider opens the LanceDB
/// index and calls `info()`, so a meta-only fixture is skipped and
/// every assertion below would pass over an empty list — a test that
/// could not fail (ARCH §18.1). Watched: the first draft did exactly
/// that and reported 0 rows.
async fn create_index(
    indexes: &std::path::Path,
    corpus_id: &str,
    created_at: u64,
    in_progress: bool,
) {
    let path = indexes.join(corpus_id);
    let index = corpus_engine::CorpusIndex::create(
        &path,
        corpus_id,
        &format!("{corpus_id} (fixture)"),
        "qwen-embedding-0.6b",
        8,
        false,
        "private",
    )
    .await
    .expect("the fixture index is created through the engine's own writer");
    if !in_progress {
        index
            .mark_ingestion_complete()
            .expect("mark_ingestion_complete");
    }
    // Stamp `created_at` explicitly. Two indexes created in the same
    // second carry the same freshness, and the shelf's sort would then
    // be decided by its alphabetical TIE-BREAK — a case that passes
    // whether or not the primary key works (ARCH §18.1). The stamp is
    // written into the engine's own meta file, after its writer made
    // one, so the shape is the writer's and only the value is the
    // fixture's.
    let meta_path = corpus_engine::Corpus::meta_in(&path);
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    meta["created_at"] = serde_json::json!(created_at);
    meta["last_updated"] = serde_json::json!(created_at);
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();
}

/// A serving daemon whose engine's index dir carries `corpora`, each
/// `(id, created_at, ingestion_in_progress)`.
async fn daemon_with_indexes(
    corpora: &[(&str, u64, bool)],
) -> (tempfile::TempDir, tempfile::TempDir, Arc<EmbeddedDaemon>) {
    let engine_tmp = tempfile::tempdir().unwrap();
    let indexes = engine_tmp.path().join("indexes");
    let recipes = engine_tmp.path().join("recipes");
    std::fs::create_dir_all(&indexes).unwrap();
    std::fs::create_dir_all(&recipes).unwrap();
    for (id, created_at, in_progress) in corpora {
        create_index(&indexes, id, *created_at, *in_progress).await;
    }
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        recipes,
        indexes,
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) })),
    ));
    let root = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        root.path().to_path_buf(),
        SetupConfig::unconfigured(),
        desktop_services_with_engine(engine),
    );
    (engine_tmp, root, daemon)
}

#[tokio::test]
async fn catalog_unions_the_builtins_with_the_installed_indexes_it_does_not_name() {
    // `sf-assessor-roll` is the canonical mesh-app case: installed on
    // disk, named by no built-in recipe. Emitting only built-ins made
    // it report as MISSING and hung the "Get data" poll for ~15 min.
    let (_e, _r, daemon) = daemon_with_indexes(&[
        ("sf-assessor-roll", 1_757_000_100, false),
        ("half-ingested", 1_757_000_200, true),
    ])
    .await;
    let addr = spawn_router(corpus_catalog_router(daemon)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/internal/corpus/catalog"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let rows = body["corpora"].as_array().expect("a `corpora` array");
    let local = rows
        .iter()
        .find(|r| r["id"] == "sf-assessor-roll")
        .unwrap_or_else(|| panic!("the installed-but-uncatalogued corpus must appear: {body}"));
    assert_eq!(
        local["status"], "installed",
        "an index on disk is installed, whatever the catalogue says: {local}"
    );
    assert_eq!(
        local["catalog_status"], "hidden",
        "it satisfies an installed-status check without crowding the \
         picker's 'Coming soon' rail"
    );
    assert_eq!(local["name"], "sf-assessor-roll (fixture)");
    assert_eq!(
        local["embedding_dimensions"], 8,
        "the index's OWN meta crosses, not a catalogue default — the \
         fixture writer was given 8"
    );
    assert_eq!(local["embedding_model"], "qwen-embedding-0.6b");

    assert!(
        !rows.iter().any(|r| r["id"] == "half-ingested"),
        "an ingest that never completed is not installed — \
         `installed_indexes` filters it and this route does not \
         re-derive that: {body}"
    );

    // Every built-in the registry snapshot carries is present, and
    // uninstalled ones say so rather than being omitted.
    assert!(
        rows.iter()
            .any(|r| r["status"] == "not_installed" && r["chunks_count"].is_null()),
        "an uninstalled catalogue row carries no chunk count: {body}"
    );
    assert!(
        !rows.iter().any(|r| r["status"] == "installing"),
        "'installing' is the caller's overlay over /internal/corpus/status, \
         not a third state re-derived here: {body}"
    );
}

#[tokio::test]
async fn notebooks_shelf_names_and_orders_the_installed_set() {
    // Tempdir-unique ids: no local-corpus registry in this binary can
    // claim them, so the assertions below hold whichever manager the
    // singleton ended up with (see the module header).
    // `older` sorts alphabetically AFTER `newer`, and carries the
    // later stamp — so an order that puts `older` first can only come
    // from the freshness key, and the tie-break cannot produce it.
    let (_e, _r, daemon) = daemon_with_indexes(&[
        ("d9a-shelf-newer", 1_757_000_100, false),
        ("d9a-shelf-older", 1_757_000_900, false),
        ("d9a-shelf-inflight", 1_757_000_500, true),
    ])
    .await;
    // In case this file runs before `lc_surface_e2e`: the shelf needs
    // SOME registry installed or it answers its named 503.
    install_a_manager_if_none().await;
    let addr = spawn_router(corpus_catalog_router(daemon)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/internal/corpus/notebooks"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let rows = body["notebooks"].as_array().expect("a `notebooks` array");
    let ours: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| {
            r["id"]
                .as_str()
                .unwrap_or_default()
                .starts_with("d9a-shelf-")
        })
        .collect();
    assert_eq!(
        ours.len(),
        2,
        "the in-flight index is not a notebook (installed_indexes filters it): {body}"
    );
    assert_eq!(
        ours[0]["id"], "d9a-shelf-older",
        "most-recently-INDEXED first — the id that sorts last \
         alphabetically but carries the later stamp, so only the \
         freshness key can produce this order: {body}"
    );
    assert_eq!(
        ours[0]["source_kind"], "installed",
        "no local config and no catalogue entry: recipe/CLI/mesh-app installed"
    );
    assert_eq!(
        ours[0]["name"], "d9a-shelf-older (fixture)",
        "the on-disk index name is the fallback when nothing better names it"
    );
    assert_eq!(
        ours[0]["scope"], "local",
        "a notebook with no local-corpus config defaults to local scope"
    );
    assert!(
        !ours[0]["explorable"].as_bool().unwrap(),
        "no atoms.json and no conv-tiered enrichment: not explorable"
    );
    assert!(
        ours[0]["open_conflicts"].is_null(),
        "no governance oplog means NO count, which is what gates the \
         Conflicts tab off — not Some(0), which shows it: {body}"
    );
}

#[tokio::test]
async fn health_and_retry_are_404_for_a_corpus_with_no_index() {
    let (_e, _r, daemon) = daemon_with_indexes(&[]).await;
    let addr = spawn_router(corpus_catalog_router(daemon)).await;

    let resp = reqwest::get(format!("http://{addr}/internal/corpus/ghost/health"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "an index that will not open is a 404, not an Ok(None) the panel \
         renders as 'never enriched'"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap_or_default().contains("ghost"),
        "the 404 names the corpus it could not open, got {body}"
    );

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/corpus/ghost/retry-enrichment"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn catalog_without_a_corpus_engine_is_the_named_503() {
    let root = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        root.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    let addr = spawn_router(corpus_catalog_router(daemon)).await;

    for path in [
        "/internal/corpus/catalog",
        "/internal/corpus/notebooks",
        "/internal/corpus/diagnose",
        "/internal/corpus/anything/health",
        "/internal/corpus/anything/coverage-card",
    ] {
        let resp = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(
            resp.status(),
            503,
            "{path}: a mesh-admin daemon holds no CorpusEngine"
        );
        // The status alone is not the gate — a routerless daemon is
        // also "not 200". The body must NAME the missing object.
        let body: serde_json::Value = resp.json().await.unwrap();
        let msg = body["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains("CorpusEngine") || msg.contains("local-corpus runtime"),
            "{path}: the 503 must say WHICH object is absent, got {body}"
        );
    }
}

/// Install a local-corpus manager if the process has none yet.
/// `install` is a `OnceLock::set`, so this is a no-op when
/// `lc_surface_e2e` got there first — which is the point.
async fn install_a_manager_if_none() {
    use sovereign_mesh::watched_folder_runtime;
    if watched_folder_runtime::manager().is_some() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(data_dir.join("indexes")).unwrap();
    std::fs::create_dir_all(data_dir.join("recipes")).unwrap();
    // The singleton holds paths into this dir for the process
    // lifetime; dropping the guard would pull them out from under it.
    std::mem::forget(tmp);
    let store: Arc<dyn sovereign_core::traits::StateStore> =
        Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        data_dir.join("recipes"),
        data_dir.join("indexes"),
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) })),
    ));
    let manager = Arc::new(
        sovereign_tools::local_corpus::LocalCorpusManager::init(
            engine,
            store,
            None,
            data_dir.clone(),
            data_dir.join("vault-snapshots"),
        )
        .await
        .expect("manager init"),
    );
    watched_folder_runtime::install(
        manager,
        Arc::new(sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry::new()),
    );
}
