// SPDX-License-Identifier: AGPL-3.0-or-later
use super::guard::LocalOnly;
use super::*;
use axum::routing::{get, post};
use std::sync::{Arc, Mutex};

/// A tracing writer into a shared buffer, so a test reads what the shell
/// printed rather than what it was meant to print.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn capturing() -> (Captured, impl tracing::Subscriber) {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    (captured, subscriber)
}

async fn ok() -> &'static str {
    "ok"
}

fn bundles() -> Vec<RouteBundle> {
    vec![
        RouteBundle::new("alpha")
            .route("/a", get(ok))
            .route("/a/{id}", post(ok)),
        RouteBundle::<Arc<str>>::new("beta")
            .route("/b", get(|_: LocalOnly| ok()))
            .with_state(Arc::from("state")),
    ]
}

#[test]
fn the_mount_trace_names_every_bundle_and_every_route() {
    let (captured, subscriber) = capturing();
    tracing::subscriber::with_default(subscriber, || {
        let _ = mount(bundles());
    });
    let trace = captured.text();
    assert!(
        trace.contains("shell: mounted bundle=\"alpha\" routes=/a /a/{id}"),
        "{trace}"
    );
    assert!(
        trace.contains("shell: mounted bundle=\"beta\" routes=/b"),
        "{trace}"
    );
}

#[test]
fn a_fallback_is_named_in_the_trace() {
    let bundle = RouteBundle::<()>::new("static")
        .route("/op", post(ok))
        .fallback(ok);
    assert_eq!(bundle.routes(), ["/op", "*"]);
}

/// The peer address reaches the handler (`LocalOnly` answers 500 without
/// it), and shutdown is a value: resolving it stops `serve`, which returns.
#[tokio::test]
async fn serve_hands_the_peer_address_through_and_stops_on_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(serve([listener], bundles(), async move {
        let _ = stopped.await;
    }));

    let resp = reqwest::get(format!("http://{addr}/b")).await.unwrap();
    assert_eq!(resp.status(), 200, "LocalOnly saw the loopback peer");
    let resp = reqwest::get(format!("http://{addr}/a")).await.unwrap();
    assert_eq!(resp.status(), 200);

    stop.send(()).unwrap();
    let done = tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("serve returns once shutdown resolves")
        .unwrap();
    assert!(done.is_ok(), "{done:?}");
}

#[tokio::test]
async fn body_limits_refuse_a_body_over_the_cap() {
    let app = body_limits(
        Router::new().route(
            "/in",
            post(|body: String| async move { body.len().to_string() }),
        ),
        16,
        Duration::from_secs(30),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    let client = reqwest::Client::new();
    let small = client
        .post(format!("http://{addr}/in"))
        .body("x".repeat(16))
        .send()
        .await
        .unwrap();
    assert_eq!(small.status(), 200);
    let big = client
        .post(format!("http://{addr}/in"))
        .body("x".repeat(17))
        .send()
        .await
        .unwrap();
    assert_eq!(big.status(), 413);
}

#[tokio::test]
async fn bind_with_retry_gives_up_on_a_held_port_and_names_it() {
    let held = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = held.local_addr().unwrap();
    let err = bind_with_retry(addr, "test API").await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    assert!(
        err.to_string()
            .starts_with(&format!("bind test API on {addr} failed after 5 attempts")),
        "{err}"
    );
    drop(held);
    assert!(bind_with_retry(addr, "test API").await.is_ok());
}
