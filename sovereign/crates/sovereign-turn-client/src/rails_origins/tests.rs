// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use oicp_types::origin::{Admit, Framing, OriginRegistration};

#[derive(Default)]
struct Rails {
    registered: AtomicUsize,
    renewed: AtomicUsize,
    /// Each register's and renew's declared `loaded_models`, in order.
    declared: Mutex<Vec<Option<Vec<String>>>>,
}

/// A cw-rails double: every registration is taken, and the FIRST renew is
/// refused, as a cw-rails that restarted and lost the claim refuses it.
async fn double() -> (String, Arc<Rails>) {
    let rails = Arc::new(Rails::default());
    let app = Router::new()
        .route(
            "/v1/mesh/origins",
            post(
                |State(r): State<Arc<Rails>>, Json(req): Json<OriginRegistration>| async move {
                    let n = r.registered.fetch_add(1, Ordering::SeqCst);
                    r.declared
                        .lock()
                        .unwrap()
                        .push(req.claims.map(|c| c.loaded_models));
                    // A slow re-registration holds the lapse open long
                    // enough to observe: a watch keeps only the latest value.
                    if n > 0 {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    Json(serde_json::json!({
                        "claim_id": format!("c{n}"), "tie": format!("t{n}"),
                        "slots": [req.alpn], "expires_in_secs": 60
                    }))
                },
            ),
        )
        .route(
            "/v1/mesh/origins/{id}/renew",
            post(|State(r): State<Arc<Rails>>, Json(body): Json<serde_json::Value>| async move {
                r.declared.lock().unwrap().push(
                    serde_json::from_value::<Option<oicp_types::capabilities::NodeCapabilities>>(
                        body["claims"].clone(),
                    )
                    .unwrap()
                    .map(|c| c.loaded_models),
                );
                if r.renewed.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({"error": "no such claim"})),
                    )
                } else {
                    (StatusCode::OK, Json(serde_json::json!({"renewed": true})))
                }
            }),
        )
        .with_state(Arc::clone(&rails));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move { axum::serve(listener, app).await });
    (base, rails)
}

fn registration() -> OriginRegistration {
    OriginRegistration {
        alpn: "cwth/http/0".into(),
        prefixes: vec!["/internal/rpc-warm".into()],
        port: 1,
        admit: Admit::Members(Vec::new()),
        framing: Framing::Http,
        ttl_secs: Some(60),
        claims: None,
        namespaces: Vec::new(),
    }
}

#[tokio::test]
async fn a_refused_renew_registers_the_origin_again() {
    let (base, rails) = double().await;
    let task = tokio::spawn(super::keep_registered(
        base,
        registration(),
        60,
        Duration::from_millis(10),
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while rails.renewed.load(Ordering::SeqCst) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the loop never renewed twice"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();
    assert_eq!(
        rails.registered.load(Ordering::SeqCst),
        2,
        "one registration, then one more after the refused renew"
    );
}

/// The published tie is the live claim's: the first claim's after it
/// registers, none from the refused renew, and the second claim's once it
/// registers again. Failing input: publish once at the first registration,
/// or leave the lapsed tie standing after the refusal.
#[tokio::test]
async fn the_published_tie_follows_the_live_claim() {
    let (base, rails) = double().await;
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let task = tokio::spawn(super::keep_registered_tied(
        base,
        registration(),
        60,
        Duration::from_millis(10),
        Some(tx),
    ));
    let mut seen: Vec<Option<String>> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while seen.last() != Some(&Some("t1".to_string())) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the second claim's tie was never published; saw {seen:?}"
        );
        tokio::time::timeout(Duration::from_secs(1), rx.changed())
            .await
            .ok();
        let now = rx.borrow_and_update().clone();
        if seen.last() != Some(&now) {
            seen.push(now);
        }
    }
    task.abort();
    assert_eq!(
        seen,
        vec![Some("t0".to_string()), None, Some("t1".to_string())],
        "first claim, lapsed, second claim"
    );
    assert_eq!(rails.registered.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_absent_cw_rails_is_an_error_naming_the_url() {
    // Port 9 (discard) on loopback: nothing answers HTTP there.
    let err = super::register_origin("http://127.0.0.1:9", &registration())
        .await
        .expect_err("no cw-rails, no claim");
    assert!(err.contains("http://127.0.0.1:9/v1/mesh/origins"), "{err}");
}

/// The source is read at every register and renew, so the declaration
/// follows it; with no source a renew declares nothing and cw-rails keeps
/// what the registration declared. Failing input: a renew that never sends
/// the source's answer.
#[tokio::test]
async fn a_claims_source_is_declared_at_every_register_and_renew() {
    let (base, rails) = double().await;
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let source: super::ClaimsSource = Arc::new(move || {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let mut caps: oicp_types::capabilities::NodeCapabilities =
                serde_json::from_value(serde_json::json!({
                    "hardware": {"gpus": [], "system_ram_gb": 0, "cpu_cores": 0,
                                 "total_storage_gb": 0, "free_storage_gb": 0},
                    "available": {"free_vram_gb": 0.0, "free_ram_gb": 0.0,
                                  "free_storage_gb": 0.0, "gpu_utilization": 0.0,
                                  "cpu_utilization": 0.0, "available_for_mesh": true},
                    "hosted_corpora": [], "reported_at": 0
                }))
                .unwrap();
            caps.loaded_models = vec![format!("m{n}")];
            caps
        })
    });
    let task = tokio::spawn(super::keep_registered_declaring(
        base,
        registration(),
        60,
        Duration::from_millis(10),
        None,
        Some(source),
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while rails.renewed.load(Ordering::SeqCst) < 2 {
        assert!(tokio::time::Instant::now() < deadline, "never renewed twice");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();
    let seen = rails.declared.lock().unwrap().clone();
    // register m0, renew m1 (refused), register m2, renew m3.
    let want: Vec<Option<Vec<String>>> = (0..4).map(|n| Some(vec![format!("m{n}")])).collect();
    assert_eq!(seen[..4], want[..]);

    let (base, rails) = double().await;
    let task = tokio::spawn(super::keep_registered(
        base,
        registration(),
        60,
        Duration::from_millis(10),
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while rails.renewed.load(Ordering::SeqCst) < 2 {
        assert!(tokio::time::Instant::now() < deadline, "never renewed twice");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();
    assert!(
        rails.declared.lock().unwrap().iter().all(Option::is_none),
        "no source, no declaration on any renew"
    );
}
