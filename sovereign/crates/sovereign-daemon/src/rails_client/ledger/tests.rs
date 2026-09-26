// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dialing ledger reports a named absence when nothing answers, and the
//! inference cache serves no reading it never took and keeps the last one
//! across a failed refill.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use commonwealth_core::activity::ActivityEventKind;
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::model::{ModelArchitecture, ModelInfo};
use sovereign_mesh::ledger_port::{
    ActivityLedgerPort, ContributionLedgerPort, InferenceStatePort, PeerPreferencesPort,
    ProcessedShardsPort,
};

use super::{InferenceCache, NeverFilled, RailsLedger};

const ME: u128 = 0xDAE;

/// Port 1 on loopback: nothing listens, so every dial is refused.
fn dead() -> RailsLedger {
    RailsLedger::new("http://127.0.0.1:1", NodeId::from_u128(ME))
}

fn model(id: u128) -> ModelInfo {
    ModelInfo {
        id: ModelId::from_u128(id),
        name: "m".into(),
        repo: "test/m".into(),
        file: "m.gguf".into(),
        size_bytes: 1,
        total_layers: 1,
        architecture: ModelArchitecture::Qwen,
        available_on: HashMap::new(),
        oicp_capabilities: Default::default(),
        quantization: "Q4_K_M".into(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    }
}

fn assert_absent<T: std::fmt::Debug>(r: Result<T, sovereign_mesh::ledger_port::LedgerAbsent>) {
    let e = r.expect_err("a dead dial has no answer");
    assert!(
        e.0.contains("not reachable at http://127.0.0.1:1"),
        "the absence names the rails daemon: {e}"
    );
}

#[tokio::test]
async fn every_port_reports_a_named_absence_on_a_dead_dial() {
    let l = dead();
    assert_eq!(
        ContributionLedgerPort::self_node_id(&l),
        NodeId::from_u128(ME),
        "the port's identity is answered without a dial"
    );
    assert_absent(
        ContributionLedgerPort::record(
            &l,
            LedgerEventKind::InferenceReceived {
                from_node: NodeId::from_u128(2),
                model_id: "m".into(),
                tokens_generated: 1,
            },
        )
        .await,
    );
    assert_absent(ContributionLedgerPort::events(&l).await);
    assert_absent(l.current_contributions(&HashMap::new(), 30).await);
    assert_absent(
        ActivityLedgerPort::record(
            &l,
            ActivityEventKind::LocalInferenceServed {
                model_id: "m".into(),
                prompt_tokens: 1,
                completion_tokens: 1,
                wall_seconds: 0.1,
            },
        )
        .await,
    );
    assert_absent(ActivityLedgerPort::events(&l).await);
    assert_absent(l.current_activity(7).await);
    assert_absent(l.list().await);
    assert_absent(PeerPreferencesPort::get(&l, &NodeId::from_u128(2)).await);
    assert_absent(l.publish("c", &[0]).await);
    assert_absent(l.union("c").await);
    assert_absent(l.get_plan().await);
    assert_absent(l.set_plan(&Default::default()).await);
    assert_absent(l.get_model_info(ModelId::from_u128(1)).await);
    assert_absent(InferenceStatePort::set_model_info(&l, &model(1)).await);
    assert_absent(l.remove_model_info(ModelId::from_u128(1)).await);
    assert_absent(InferenceStatePort::list_models_with_origins(&l).await);
    assert_absent(l.get_llama_address(ModelId::from_u128(1)).await);
    assert_absent(l.set_llama_address(ModelId::from_u128(1), "a").await);
    assert_absent(InferenceStatePort::get_local_embed_model(&l).await);
}

#[tokio::test]
async fn a_cache_that_never_filled_reports_absence_not_an_empty_map() {
    let cache = InferenceCache::new(Arc::new(dead()), NodeId::from_u128(ME));
    assert_eq!(cache.list_models().err(), Some(NeverFilled));
    assert!(cache.refill().await.is_err(), "the dead dial is reported");
    assert_eq!(cache.list_models().err(), Some(NeverFilled));
    assert_eq!(cache.list_models_with_origins().err(), Some(NeverFilled));
    assert_eq!(cache.get_local_embed_model(), Err(NeverFilled));
}

/// A stand-in for the two doors a refill reads; `up` false turns both into
/// a 500 so a refill fails against a live listener.
async fn doors(up: Arc<AtomicBool>) -> String {
    let models_up = up.clone();
    let app = axum::Router::new()
        .route(
            "/v1/ledger/inference/models",
            axum::routing::get(move || {
                let up = models_up.load(Ordering::SeqCst);
                async move {
                    if up {
                        Json(vec![(NodeId::from_u128(5), model(1))]).into_response()
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            }),
        )
        .route(
            "/v1/ledger/inference/embed-model",
            axum::routing::get(move || {
                let up = up.load(Ordering::SeqCst);
                async move {
                    if up {
                        Json(serde_json::Value::Null).into_response()
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_failed_refill_keeps_the_last_value() {
    let up = Arc::new(AtomicBool::new(true));
    let base = doors(up.clone()).await;
    let cache = InferenceCache::new(
        Arc::new(RailsLedger::new(base, NodeId::from_u128(ME))),
        NodeId::from_u128(ME),
    );
    cache.refill().await.unwrap();
    let first = cache.list_models().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(
        cache.list_models_with_origins().unwrap()[0].0,
        NodeId::from_u128(5)
    );
    assert_eq!(cache.get_local_embed_model(), Ok(None));

    up.store(false, Ordering::SeqCst);
    let e = cache.refill().await.expect_err("the doors refused");
    assert!(e.0.contains("refused: 500"), "{e}");
    assert_eq!(
        cache.list_models().unwrap().keys().collect::<Vec<_>>(),
        first.keys().collect::<Vec<_>>()
    );

    // The synchronous write lands in a filled cache at once, as this node's.
    cache.set_model_info(&model(2));
    let origins = cache.list_models_with_origins().unwrap();
    assert!(origins
        .iter()
        .any(|(n, m)| *n == NodeId::from_u128(ME) && m.id == ModelId::from_u128(2)));
}

/// An activity event served by the stand-in door reads back through the
/// dialed port unchanged, as cw-rails' `GET /v1/ledger/activity` serves it.
#[tokio::test]
async fn a_recorded_activity_event_reads_back_through_the_dial() {
    let recorded = vec![commonwealth_core::activity::ActivityEvent {
        node_id: NodeId::from_u128(ME),
        timestamp: 1,
        kind: ActivityEventKind::LocalInferenceServed {
            model_id: "m".into(),
            prompt_tokens: 3,
            completion_tokens: 4,
            wall_seconds: 0.1,
        },
    }];
    assert_eq!(recorded.len(), 1);

    let served = recorded.clone();
    let app = axum::Router::new().route(
        "/v1/ledger/activity",
        axum::routing::get(move || {
            let served = served.clone();
            async move { Json(served) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let l = RailsLedger::new(format!("http://{addr}"), NodeId::from_u128(ME));
    assert_eq!(ActivityLedgerPort::events(&l).await.unwrap(), recorded);
}
