// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
            post(|State(r): State<Arc<Rails>>| async move {
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
