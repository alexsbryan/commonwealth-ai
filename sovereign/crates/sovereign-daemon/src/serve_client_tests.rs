// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve_client`'s tests, under `#[path]` so their names are unchanged.

use super::*;

fn node(entry: Option<&str>, entry_node: Option<&str>) -> NodeSection {
    NodeSection {
        entry: entry.map(str::to_string),
        entry_node: entry_node.map(str::to_string),
        ..NodeSection::default()
    }
}

/// Every config dials serve unless this process hosts it
/// (pb-serve-distributes). Failing input: drop `hosting` from
/// `with_hosting`'s guard, and svrn alone goes Hosted. That no in-process
/// arm returns is `no_engine_census`'s, which reads the sources.
#[test]
fn a_default_config_dials_serve() {
    assert_eq!(
        ServingPath::DialsServe.with_hosting(false, &SetupConfig::unconfigured()),
        ServingPath::DialsServe
    );
    assert_eq!(ServingPath::DialsServe.status_line(), "serve");
}

/// The hosted path (pb-stock-binary): a process that hosts serve serves from
/// it where serve would be this host's anyway, and nowhere else. Failing
/// input: drop the terminal or the `[node] entry` guard from `with_hosting`,
/// and the second or third case goes Hosted.
#[test]
fn hosting_turns_only_this_hosts_dialing_path_hosted() {
    let cfg = |node_section: NodeSection| SetupConfig {
        node: node_section,
        ..SetupConfig::unconfigured()
    };
    let plain = cfg(NodeSection::default());
    assert_eq!(
        ServingPath::DialsServe.with_hosting(true, &plain),
        ServingPath::Hosted
    );
    assert_eq!(
        ServingPath::Hosted.status_line(),
        "serve (this process)".to_string()
    );
    // svrn alone dials.
    assert_eq!(
        ServingPath::DialsServe.with_hosting(false, &plain),
        ServingPath::DialsServe
    );
    // An operator-named serve stays dialed; a terminal dials its entry node.
    let named = cfg(node(Some("http://10.0.0.9:9748/v1"), None));
    assert_eq!(
        ServingPath::DialsServe.with_hosting(true, &named),
        ServingPath::DialsServe
    );
    let terminal = cfg(node(None, Some("hub")));
    assert_eq!(
        ServingPath::DialsServe.with_hosting(true, &terminal),
        ServingPath::DialsServe
    );
}

/// A stub serve on a free loopback port whose engine-state route waits
/// `hold` before it answers the empty view.
async fn stub_serve(hold: std::time::Duration) -> String {
    use axum::routing::get;
    let app = axum::Router::new().route(
        sovereign_contracts::engine_state::ENGINE_STATE_PATH,
        get(move || async move {
            tokio::time::sleep(hold).await;
            axum::Json(sovereign_contracts::engine_state::EngineState::default())
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

#[tokio::test]
async fn a_serve_that_holds_the_route_past_the_bound_is_named_not_waited_on() {
    let base = stub_serve(std::time::Duration::from_secs(30)).await;
    let started = std::time::Instant::now();
    let read = read_engine_state(&base).await;
    let took = started.elapsed();
    assert_eq!(read, EngineStateRead::DidNotAnswerInTime);
    assert!(
        took < sovereign_turn_client::reach::PROBE_TIMEOUT + std::time::Duration::from_secs(1),
        "the read waited {took:?}, past the bound plus 1 s"
    );
}

#[tokio::test]
async fn a_serve_that_answers_is_read_and_not_observed_yet_stays_none() {
    let base = stub_serve(std::time::Duration::ZERO).await;
    match read_engine_state(&base).await {
        EngineStateRead::Answered(state) => assert_eq!(state.device_memory, None),
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[tokio::test]
async fn no_serve_at_the_base_is_unreachable_not_empty() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(matches!(
        read_engine_state(&base).await,
        EngineStateRead::Unreachable(_)
    ));
}

async fn stub(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

fn served() -> sovereign_contracts::engine_state::ServedSelf {
    sovereign_contracts::engine_state::ServedSelf {
        primary_model: "big".into(),
        medium_model: "big".into(),
        fast_model: "small".into(),
        embed_model: "Qwen3-Embedding-0.6B-Q8_0".into(),
        embed_family: sovereign_contracts::model_family::ModelFamily::Qwen3Embedding,
        context_size: Some(8192),
        ..Default::default()
    }
}

/// The loopback mode follows the base's source: this host's serve answers
/// for this node's models, an operator-named entry never does.
#[test]
fn only_the_default_base_answers_for_this_nodes_models() {
    use sovereign_contracts::{InferenceProvider, Speed};
    for (source, expected) in [
        (ServeBaseSource::Default, "big"),
        (ServeBaseSource::NodeEntry, "primary"),
    ] {
        let serve = ServeBase {
            base: "http://127.0.0.1:1".into(),
            source,
        };
        let provider = loopback_provider(&serve, served(), 4096);
        assert_eq!(provider.model_id_for(Speed::Slow), expected, "{source:?}");
    }
}

/// A query embedded through the loopback provider carries the same
/// instruction prefix the engine applies in process; the terminal arm's
/// empty prefix would embed it as a document.
#[tokio::test]
async fn the_loopback_provider_prepares_a_query_the_way_the_engine_does() {
    use axum::routing::post;
    use sovereign_contracts::InferenceProvider;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let seen_in = std::sync::Arc::clone(&seen);
    let base = stub(axum::Router::new().route(
        "/v1/embeddings",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let seen_in = std::sync::Arc::clone(&seen_in);
            async move {
                *seen_in.lock().unwrap() = body["input"].as_str().unwrap_or("").to_string();
                axum::Json(serde_json::json!({
                    "object": "list", "model": "e",
                    "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}]
                }))
            }
        }),
    ))
    .await;
    let serve = ServeBase {
        base,
        source: ServeBaseSource::Default,
    };
    let provider = loopback_provider(&serve, served(), 4096);
    provider
        .embed_query("who wrote it")
        .await
        .expect("embedded");
    let input = seen.lock().unwrap().clone();
    let expected = sovereign_contracts::model_family::ModelFamily::Qwen3Embedding
        .default_quirks()
        .embed
        .expect("quirks")
        .query_instruction;
    assert!(input.starts_with(&expected), "query sent as {input:?}");
    assert_eq!(
        provider.model_id_for(sovereign_contracts::Speed::Slow),
        "big"
    );
}

#[tokio::test]
async fn the_self_report_is_read_from_serve() {
    use axum::routing::get;
    let base = stub(axum::Router::new().route(
        sovereign_contracts::engine_state::SERVED_SELF_PATH,
        get(|| async { axum::Json(served()) }),
    ))
    .await;
    let read = read_served_self(&base).await.expect("read");
    assert_eq!(read.primary_model, "big");
}

/// A reload serve refuses is an Err naming the refusal, never a
/// success-shaped reload.
#[tokio::test]
async fn a_refused_reload_is_named() {
    use axum::routing::post;
    let base = stub(axum::Router::new().route(
        sovereign_contracts::engine_state::RELOAD_PATH,
        post(|| async {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "reload: the serving assembly refused: no such file",
            )
        }),
    ))
    .await;
    let err = forward_reload(&base).await.expect_err("refused");
    assert!(
        err.contains("HTTP 503") && err.contains("no such file"),
        "{err}"
    );
}

/// A forwarded read relays serve's status and body as sent, query and
/// refusal included, so a setup read answers alike from either process.
#[tokio::test]
async fn a_forwarded_read_relays_serves_status_and_body() {
    use axum::extract::RawQuery;
    use axum::routing::get;
    let base = stub(axum::Router::new().route(
        "/v1/admin/setup/catalog",
        get(|RawQuery(q): RawQuery| async move {
            (
                axum::http::StatusCode::BAD_REQUEST,
                format!("{{\"error\":\"{}\"}}", q.unwrap_or_default()),
            )
        }),
    ))
    .await;
    let (status, body) = forward_get(&base, "/v1/admin/setup/catalog?profile=nope")
        .await
        .expect("answered");
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body, br#"{"error":"profile=nope"}"#);
}

#[tokio::test]
async fn a_forwarded_read_to_no_serve_is_named() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let err = forward_get(&base, "/v1/admin/hardware")
        .await
        .expect_err("no serve");
    assert!(
        err.contains("not reachable") && err.contains("/v1/admin/hardware"),
        "{err}"
    );
}

/// svrn brings nothing up (phase-b-29 Q2): with no serve answering, boot's
/// wait names the absence and starts no process, even with a serve binary
/// named where the old bring-up looked for one. Failing input: re-add the
/// bring-up arm to `ensure_serve`, and the stand-in binary leaves its marker.
#[tokio::test]
async fn a_standalone_svrn_with_no_serve_spawns_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let bin = dir.path().join("sovereign-serve");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\ntouch {}\nsleep 5\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("SOVEREIGN_SERVE_BIN", &bin);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let serve = ServeBase {
        base: format!("http://{}", listener.local_addr().unwrap()),
        source: ServeBaseSource::Default,
    };
    drop(listener);
    let err = ensure_serve(&serve, std::time::Duration::from_secs(1))
        .await
        .expect_err("no serve answers");
    assert!(!err.is_empty());
    // A spawned stand-in would have touched the marker by now.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !marker.exists(),
        "svrn spawned a serve: the stock install hosts serve, and svrn alone only dials"
    );
}

#[test]
fn no_entry_dials_the_default_base() {
    let resolved = resolve_serve_base(&node(None, None));
    assert_eq!(resolved.base, "http://127.0.0.1:9748");
    assert_eq!(resolved.source, ServeBaseSource::Default);
}

#[test]
fn an_address_entry_is_the_base_without_its_v1() {
    for entry in ["http://127.0.0.1:18748/v1", "http://127.0.0.1:18748/v1/"] {
        let resolved = resolve_serve_base(&node(Some(entry), None));
        assert_eq!(resolved.base, "http://127.0.0.1:18748");
        assert_eq!(resolved.source, ServeBaseSource::NodeEntry);
    }
}

#[test]
fn an_identity_binding_never_names_serve() {
    let resolved = resolve_serve_base(&node(None, Some("ab12")));
    assert_eq!(resolved.base, default_serve_base());
    assert_eq!(resolved.source, ServeBaseSource::Default);
}

/// serve's NER route: 503 for the first `failures` probes, then "no model".
async fn ner_route(
    failures: usize,
    probes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> String {
    use axum::response::IntoResponse;
    use axum::routing::post;
    use std::sync::atomic::Ordering;
    stub(axum::Router::new().route(
        sovereign_contracts::ner::NER_PATH,
        post(move || {
            let probes = std::sync::Arc::clone(&probes);
            async move {
                if probes.fetch_add(1, Ordering::SeqCst) < failures {
                    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "loading").into_response()
                } else {
                    axum::Json(serde_json::json!({"extractor": null, "mentions": []}))
                        .into_response()
                }
            }
        }),
    ))
    .await
}

/// A NER probe that never answers is an Err naming it, never "serve has no
/// NER model" for the life of the process.
#[tokio::test]
async fn a_ner_probe_that_keeps_erring_is_named_not_none() {
    let probes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let base = ner_route(usize::MAX, std::sync::Arc::clone(&probes)).await;
    let err = resolve_serve_ner(&base, std::time::Duration::from_millis(2500))
        .await
        .map(|h| h.is_some())
        .expect_err("serve never answered");
    assert!(err.contains("did not answer"), "{err}");
    assert!(
        probes.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "retried"
    );
}

/// A probe that errs once and then answers is that answer.
#[tokio::test]
async fn a_ner_probe_is_retried_until_serve_answers() {
    let probes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let base = ner_route(1, std::sync::Arc::clone(&probes)).await;
    let handle = resolve_serve_ner(&base, std::time::Duration::from_secs(10))
        .await
        .expect("answered on the second probe");
    assert!(handle.is_none(), "serve answered: no model");
    assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// The loader's env contract reaches svrn's boot only through the
/// distribution (pb-serve-distributes): applied with this invocation's args
/// and the config's `[shared_model]` when handed in, and reported absent
/// when not. Failing input: drop the call in `apply_env_contract`, and the
/// first assertion goes red.
#[test]
fn the_env_contract_is_the_distributions_to_hand_in() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let seen = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&seen);
    let compose = |_: std::path::PathBuf, _: std::path::PathBuf| async {
        Err::<HostedParts, String>("not composed here".to_string())
    };
    let shared = sovereign_contracts::setup_config::SharedModelSection::default();
    let args = vec!["--rpc-worker=127.0.0.1:1".to_string()];
    let handed = HostedServe::new(String::new(), compose).env_contract(move |a, _| {
        assert_eq!(a, ["--rpc-worker=127.0.0.1:1"]);
        counted.fetch_add(1, Ordering::SeqCst);
    });
    assert!(handed.apply_env_contract(&args, &shared));
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    let bare = HostedServe::new(String::new(), compose);
    assert!(!bare.apply_env_contract(&args, &shared));
}

/// The NER handle a distribution hands boot is the one svrn's ingest and
/// retrieval get (pb-serve-distributes); with none handed there is none.
/// Failing input: have `ner_handle` ignore the source, and the first
/// assertion reads `None`.
#[test]
fn the_ner_handle_is_the_distributions_in_process_kind() {
    struct Named;
    impl sovereign_contracts::ner::LabeledEntityExtractor for Named {
        fn model_id(&self) -> &str {
            "handed-ner"
        }
        fn labels(&self) -> Vec<String> {
            Vec::new()
        }
        fn threshold(&self) -> f32 {
            0.5
        }
        fn generation(&self) -> sovereign_contracts::ner::GlinerGeneration {
            sovereign_contracts::ner::GlinerGeneration::V2
        }
        fn extract_mentions(
            &self,
            _: &str,
        ) -> sovereign_contracts::error::Result<Vec<sovereign_contracts::ner::EntityMention>>
        {
            Ok(Vec::new())
        }
    }
    let compose = |_: std::path::PathBuf, _: std::path::PathBuf| async {
        Err::<HostedParts, String>("not composed here".to_string())
    };
    let handed = HostedServe::new(String::new(), compose).ner(|| {
        Some(std::sync::Arc::new(Named)
            as std::sync::Arc<
                dyn sovereign_contracts::ner::LabeledEntityExtractor,
            >)
    });
    assert_eq!(
        handed
            .ner_handle()
            .map(|h| h.model_id().to_string())
            .as_deref(),
        Some("handed-ner")
    );
    let mut taken = handed;
    assert!(taken.take_ner().is_some() && taken.ner_handle().is_none());
    assert!(HostedServe::new(String::new(), compose)
        .ner_handle()
        .is_none());
}
