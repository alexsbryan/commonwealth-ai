// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! **A keyed daemon identifies every caller by API key** — the proof of
//! pb-distribution-onprem-identity (decision phase-b-86).
//!
//! Driven through the stock composition over a real listener on 127.0.0.1: the
//! client router with the turn and document families merged in after it, then
//! [`api_keys::seal`] on the finished router, exactly as `start_daemon` builds
//! it. Every request below arrives from loopback, which is the case on-prem's
//! nginx produces; a keyed daemon must grant it nothing.
//!
//! The named failing inputs: grant loopback in `keyed_auth_layer` and
//! `no_key_is_refused_even_from_loopback` goes red; make
//! `PrincipalScope::admits` ignore the grant and
//! `a_key_never_retrieves_outside_the_corpus_grant` goes red. The last test is
//! the regression guard: a daemon with NO keys serves loopback as before.
//!
//! pb-distribution-onprem-routes adds the granted reads (`granted_http`) and
//! `/health`: each answers under a key and is refused without one, except
//! `/health`. Drop the grant check in `granted_http::reading_window` and
//! `the_reading_window_refuses_a_corpus_outside_the_grant` goes red.

use std::net::SocketAddr;
use std::sync::Arc;

use kernel_types::NodeId;
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::traits::{PrincipalResolver, StateStore};
use sovereign_contracts::types::{CorpusState, CorpusVisibility};
use sovereign_core::context::{build_context, PrincipalScope};
use sovereign_daemon::api_keys::{self, KeyedOwners};
use sovereign_daemon::client_tokens::{
    client_tokens_dir, ClientTokenStore, Loopback, LoopbackPosture, KEY_ADMIN_GROUP,
};
use sovereign_daemon::documents_http::documents_router;
use sovereign_daemon::granted_http::granted_router;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{AppState, NodeSeed};
use sovereign_daemon::turn_http::turn_router;
use sovereign_daemon::EmbeddedDaemon;

use crate::common::{
    desktop_services, spawn_router, stub_runtime_with_engine, DesktopParts, TestProvider,
};
use crate::turn_surface::corpus_scoping::install_corpus;

const ALICE: &str = "alice-key-0000000000000000000000000000000000000000000000000000";
const BOB: &str = "bob-key-00000000000000000000000000000000000000000000000000000000";
const IT: &str = "it-key-000000000000000000000000000000000000000000000000000000000";

/// The corpus grant `[retrieval] corpora` names; `secret` is installed beside
/// it and must never be reachable by a key.
fn grant() -> Vec<String> {
    vec!["firm-docs".to_string()]
}

struct Daemon {
    _tmp: tempfile::TempDir,
    base: String,
    store: Arc<dyn StateStore>,
}

/// A serving daemon holding `keys` (none ⇒ unkeyed), with `firm-docs` and
/// `secret` installed, composed and sealed the way `start_daemon` does it.
async fn daemon(keys_on_disk: &[(&str, &[&str], &str)]) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    install_corpus(&indexes, "firm-docs").await;
    install_corpus(&indexes, "secret").await;
    let dir = client_tokens_dir(tmp.path());
    // The install writes the keys (`svrn daemon key --add`, declared `none`);
    // the daemon then reads them with no declaration of its own, which is
    // the upgrade path: keys on disk and nothing declared infer `none`.
    let installer = ClientTokenStore::load(
        Some(dir.clone()),
        LoopbackPosture {
            loopback: Loopback::None,
            declared: true,
        },
    );
    for (sub, groups, token) in keys_on_disk {
        let groups: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
        installer.mint(sub, &groups, token.to_string()).unwrap();
    }
    let posture = LoopbackPosture::resolve(None, Some(&dir)).unwrap();
    let tokens = Arc::new(ClientTokenStore::load(Some(dir), posture));

    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let provider: Arc<dyn sovereign_contracts::traits::InferenceProvider> =
        Arc::new(TestProvider::new());
    let engine = Arc::new(
        crate::common::reading_double(
            indexes,
            Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
        )
        .with_builtin_corpora(Vec::new()),
    );
    let mut runtime = stub_runtime_with_engine(
        Arc::clone(&provider),
        Some(Arc::clone(&store)),
        engine.clone(),
    );
    // The boot's branch (`daemon_cmd/boot.rs`): a keyed store commissions the
    // turn with `KeyedOwners` over `[retrieval] corpora`.
    if tokens.is_keyed() {
        Arc::get_mut(&mut runtime).unwrap().corpus_principal =
            Some(Arc::new(KeyedOwners { grant: grant() }));
    }
    let services = desktop_services(DesktopParts {
        provider,
        store: Arc::clone(&store),
        runtime,
        ..DesktopParts::new(engine)
    });
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    );

    let state = AppState::new_with_node(
        NodeId::from_u128(1),
        NodeSeed {
            named_client_tokens: tokens,
            ..Default::default()
        },
    );
    let router = client_router(state.clone())
        .merge(turn_router(Arc::clone(&daemon)))
        .merge(documents_router(Arc::clone(&daemon)))
        .merge(granted_router(Arc::clone(&daemon)));
    let addr: SocketAddr = spawn_router(api_keys::seal(router, &state)).await;
    Daemon {
        _tmp: tmp,
        base: format!("http://{addr}"),
        store,
    }
}

fn keyed_set() -> Vec<(&'static str, &'static [&'static str], &'static str)> {
    vec![
        ("alice", &[], ALICE),
        ("bob", &[], BOB),
        ("it", &[KEY_ADMIN_GROUP], IT),
    ]
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn body(resp: reqwest::Response) -> serde_json::Value {
    resp.json().await.unwrap_or(serde_json::Value::Null)
}

async fn create(d: &Daemon, key: &str, corpora: Option<&[&str]>) -> reqwest::Response {
    let mut req = serde_json::json!({});
    if let Some(c) = corpora {
        req["enabled_corpora"] = serde_json::json!(c);
    }
    client()
        .post(format!("{}/v1/conversations", d.base))
        .bearer_auth(key)
        .json(&req)
        .send()
        .await
        .unwrap()
}

async fn list_ids(d: &Daemon, key: &str) -> Vec<String> {
    let resp = client()
        .get(format!("{}/v1/conversations", d.base))
        .bearer_auth(key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    body(resp).await["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn no_key_is_refused_even_from_loopback() {
    let d = daemon(&keyed_set()).await;
    let resp = client()
        .get(format!("{}/v1/conversations", d.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a keyed daemon grants a loopback caller nothing"
    );
    let err = body(resp).await["error"].as_str().unwrap_or("").to_string();
    assert!(
        err.contains("API key"),
        "the refusal names the key it needs: {err}"
    );

    let resp = client()
        .get(format!("{}/v1/conversations", d.base))
        .bearer_auth("not-a-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Every route, not only the merged families: the client router's own.
    let resp = client()
        .get(format!("{}/v1/models", d.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_keys_conversations_are_its_own() {
    let d = daemon(&keyed_set()).await;
    let resp = create(&d, ALICE, None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let conv = body(resp).await["id"].as_str().unwrap().to_string();

    assert_eq!(list_ids(&d, ALICE).await, vec![conv.clone()]);
    assert!(
        list_ids(&d, BOB).await.is_empty(),
        "alice's conversation is absent from bob's list"
    );
    let get = |key: &'static str| {
        let url = format!("{}/v1/conversations/{conv}", d.base);
        async move {
            client()
                .get(url)
                .bearer_auth(key)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(get(ALICE).await, reqwest::StatusCode::OK);
    assert_eq!(get(BOB).await, reqwest::StatusCode::NOT_FOUND);

    // The row is stored under its owner, the deleted server's tenant scheme.
    let stored: Vec<String> = d
        .store
        .list_conversations(10, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(stored, vec![format!("alice:{conv}")]);
}

#[tokio::test]
async fn a_key_never_retrieves_outside_the_corpus_grant() {
    let d = daemon(&keyed_set()).await;
    // Seed: naming a corpus outside the grant is refused, and the refusal's
    // list of what may be named does not reveal it.
    let resp = create(&d, ALICE, Some(&["secret"])).await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err = body(resp).await["error"].as_str().unwrap_or("").to_string();
    let installed = err.split("installed:").nth(1).unwrap_or("").to_string();
    assert!(installed.contains("firm-docs"), "{err}");
    assert!(
        !installed.contains("secret"),
        "the grant hides `secret`: {err}"
    );
    assert_eq!(
        create(&d, ALICE, Some(&["firm-docs"])).await.status(),
        reqwest::StatusCode::OK
    );

    // The turn's ceiling, from the resolver the keyed daemon commissions: an
    // unsealed conversation still reaches only the grant.
    for id in ["firm-docs", "secret"] {
        d.store
            .save_corpus_state(&CorpusState {
                corpus_id: id.into(),
                installed_at: 0,
                source_date: String::new(),
                chunks_count: 1,
                index_size_mb: 0,
                last_updated: 0,
                version: 0,
                deleted_at: None,
                vector_index_ready: false,
                visibility: CorpusVisibility::default(),
            })
            .await
            .unwrap();
    }
    let owners = KeyedOwners { grant: grant() };
    let scope = PrincipalScope::from_resolver(Some(&owners as &dyn PrincipalResolver), "alice:c");
    let ctx = build_context(d.store.as_ref(), "alice:c", "", scope)
        .await
        .unwrap();
    assert_eq!(ctx.corpus_ceiling, Some(grant()));
    assert_eq!(ctx.installed_corpora, grant());
}

#[tokio::test]
async fn a_lawyer_key_cannot_ingest_a_server_side_path() {
    let d = daemon(&keyed_set()).await;
    let post = |key: &'static str, path: &'static str| {
        let url = format!("{}{path}", d.base);
        async move {
            client()
                .post(url)
                .bearer_auth(key)
                .json(&serde_json::json!({ "path": "/etc/hostname" }))
                .send()
                .await
                .unwrap()
        }
    };
    for path in [
        "/v1/documents",
        "/v1/documents/legacy",
        "/internal/corpus/local",
    ] {
        let resp = post(ALICE, path).await;
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN, "{path}");
        let err = body(resp).await["error"].as_str().unwrap_or("").to_string();
        assert!(
            err.contains("alice") && err.contains(KEY_ADMIN_GROUP),
            "the refusal names the key and the group it lacks: {err}"
        );
    }
    let resp = post(IT, "/v1/documents").await;
    assert!(
        !matches!(
            resp.status(),
            reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::UNAUTHORIZED
        ),
        "an admin key reaches the ingest route's handler, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn an_unkeyed_daemon_serves_loopback_as_before() {
    let d = daemon(&[]).await;
    let resp = client()
        .get(format!("{}/v1/conversations", d.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "no keys, no seal: the loopback owner is admitted with no credential"
    );
}

/// The one chunk `install_corpus` wrote into `corpus`: whatever id the index
/// assigned, read back rather than guessed.
async fn chunk_id(d: &Daemon, corpus: &str) -> u64 {
    let index = corpus_index::index::CorpusIndex::open(&d._tmp.path().join("indexes").join(corpus))
        .await
        .unwrap();
    let hits = index.search(&[0.0_f32; 4], "", 1).await.unwrap();
    hits[0].chunk_id.expect("the fixture chunk has an id")
}

async fn get(d: &Daemon, key: Option<&str>, path: &str) -> reqwest::Response {
    let req = client().get(format!("{}{path}", d.base));
    match key {
        Some(k) => req.bearer_auth(k),
        None => req,
    }
    .send()
    .await
    .unwrap()
}

#[tokio::test]
async fn the_granted_reads_answer_under_a_key_and_refuse_without_one() {
    let d = daemon(&keyed_set()).await;
    let window = format!(
        "/v1/corpora/firm-docs/chunks/{}",
        chunk_id(&d, "firm-docs").await
    );
    for path in ["/v1/corpora", window.as_str(), "/v1/tools"] {
        assert_eq!(
            get(&d, None, path).await.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{path} without a key"
        );
        let resp = get(&d, Some(ALICE), path).await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "{path} under a lawyer key"
        );
    }

    let corpora = body(get(&d, Some(ALICE), "/v1/corpora").await).await;
    let ids: Vec<&str> = corpora["corpora"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["firm-docs"],
        "only the grant, never `secret`: {corpora}"
    );

    let tools = body(get(&d, Some(ALICE), "/v1/tools").await).await;
    // The stub Runtime registers no tools; the onprem binary's e2e reads a
    // real registry.
    let tools = tools["tools"].as_array().expect("a `tools` array");
    assert!(
        tools
            .iter()
            .all(|t| t["requires_approval"].is_boolean() && t["id"].is_string()),
        "{tools:?}"
    );

    let health = get(&d, None, "/health").await;
    assert_eq!(
        health.status(),
        reqwest::StatusCode::OK,
        "liveness needs no key"
    );
    assert_eq!(health.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn the_reading_window_refuses_a_corpus_outside_the_grant() {
    let d = daemon(&keyed_set()).await;
    let (firm, secret) = (
        chunk_id(&d, "firm-docs").await,
        chunk_id(&d, "secret").await,
    );
    let window = body(
        get(
            &d,
            Some(ALICE),
            &format!("/v1/corpora/firm-docs/chunks/{firm}?radius=2"),
        )
        .await,
    )
    .await;
    assert_eq!(window["center"]["content"], "one chunk", "{window}");
    assert_eq!(window["center"]["corpus_id"], "firm-docs", "{window}");
    assert!(
        window["prev"].is_array() && window["next"].is_array(),
        "{window}"
    );

    let resp = get(
        &d,
        Some(ALICE),
        &format!("/v1/corpora/secret/chunks/{secret}"),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    let err = body(resp).await["error"].as_str().unwrap_or("").to_string();
    assert!(
        err.contains("'secret'") && err.contains("alice"),
        "the refusal names the corpus and the key: {err}"
    );
}

#[tokio::test]
async fn an_unkeyed_daemon_serves_the_granted_reads_to_loopback() {
    let d = daemon(&[]).await;
    let corpora = body(get(&d, None, "/v1/corpora").await).await;
    let mut ids: Vec<&str> = corpora["corpora"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        ["firm-docs", "secret"],
        "the local owner's grant is every corpus"
    );
    assert_eq!(
        get(
            &d,
            None,
            &format!("/v1/corpora/secret/chunks/{}", chunk_id(&d, "secret").await)
        )
        .await
        .status(),
        reqwest::StatusCode::OK
    );
}
