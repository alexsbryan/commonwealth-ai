// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve_dial`'s tests: the embed dial router fit and router-cache take,
//! against a stub serve on a free `SOVEREIGN_SERVE_PORT`.

use super::*;
use sovereign_contracts::venue::SERVE_PORT_ENV;

/// A stub serve reporting `embed_model` and answering `/v1/embeddings`.
async fn stub_serve(embed_model: &str) -> u16 {
    use axum::routing::{get, post};
    let served = serde_json::to_value(ServedSelf {
        embed_model: embed_model.to_string(),
        ..ServedSelf::default()
    })
    .unwrap();
    let app = axum::Router::new()
        .route(
            sovereign_contracts::engine_state::SERVED_SELF_PATH,
            get(move || {
                let served = served.clone();
                async move { axum::Json(served) }
            }),
        )
        .route(
            "/v1/embeddings",
            post(|| async {
                axum::Json(serde_json::json!({
                    "data": [{"embedding": [0.25, 0.5], "index": 0}]
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    port
}

/// One test, because the three cases move one process-wide variable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_embed_dial_embeds_on_serve_and_refuses_by_name() {
    let model = std::path::Path::new("/models/want-embed.gguf");

    // serve holding the model: the provider embeds through it.
    let port = stub_serve("want-embed").await;
    std::env::set_var(SERVE_PORT_ENV, port.to_string());
    let provider = serve_embedder("router-cache rebuild", model)
        .await
        .expect("serve holds the model");
    assert_eq!(provider.embed("hello").await.unwrap(), vec![0.25, 0.5]);

    // serve holding another model: refused, naming both.
    let port = stub_serve("other-embed").await;
    std::env::set_var(SERVE_PORT_ENV, port.to_string());
    let err = serve_embedder("router-cache rebuild", model)
        .await
        .err()
        .expect("another model is refused");
    assert!(
        err.contains("other-embed") && err.contains("want-embed"),
        "{err}"
    );

    // Nothing on the port: refused, naming serve and the base.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    std::env::set_var(SERVE_PORT_ENV, port.to_string());
    let err = serve_embedder("router-cache rebuild", model)
        .await
        .err()
        .expect("no serve is refused");
    assert!(
        err.contains("serve") && err.contains(&format!("127.0.0.1:{port}")),
        "{err}"
    );
    let err = serve_ner("svrn chat")
        .await
        .err()
        .expect("no serve is refused for NER too");
    assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");

    std::env::remove_var(SERVE_PORT_ENV);
}
