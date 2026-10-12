// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/mesh/media/reload`: a running cw-rails serves the media origin
//! its rails.toml names NOW, with the credentials declared for it, and
//! refuses a file that no longer loads without changing what it serves.
//! `svrn mesh media offer | admit | withdraw` write the file and call this,
//! so an offer changes without restarting the endpoint.

use std::path::Path;
use std::sync::Arc;

use commonwealth_media::origins::{Admit, RegisteredOrigin};
use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{api, RailsDaemon, RailsNode};

fn hermetic() -> Config {
    Config {
        name: "holder".to_string(),
        listen: 0xFFFF,
        relay: RelaySection {
            urls: Vec::new(),
            discovery: Some("none".to_string()),
        },
        media: MediaSection {
            origin: None,
            allow: Vec::new(),
        },
        gossip_interval_secs: 1,
        offline_threshold_secs: 60,
        work_offer: Default::default(),
        source: None,
    }
}

async fn serve(dir: &Path) -> String {
    let node = RailsNode::bind(dir.to_path_buf(), hermetic())
        .await
        .expect("node binds");
    let daemon = Arc::new(RailsDaemon::start_from_disk(node).await.expect("starts"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, api::router(daemon)).await.unwrap();
    });
    format!("http://{addr}")
}

/// The media row of `GET /v1/mesh/origins`, if one is served.
async fn media_row(base: &str) -> Option<RegisteredOrigin> {
    let v: serde_json::Value = reqwest::get(format!("{base}/v1/mesh/origins"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows: Vec<RegisteredOrigin> = serde_json::from_value(v["origins"].clone()).unwrap();
    rows.into_iter().find(|r| r.slot == "cwth/media/0")
}

async fn reload(base: &str) -> (u16, serde_json::Value) {
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/mesh/media/reload"))
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

async fn published_origin(base: &str) -> serde_json::Value {
    let v: serde_json::Value = reqwest::get(format!("{base}/v1/mesh/publish"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["media_origin"].clone()
}

#[tokio::test]
async fn a_reload_serves_the_media_origin_rails_toml_now_names() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(dir.path()).await;
    assert!(media_row(&base).await.is_none(), "started with no [media]");

    let toml = dir.path().join("rails.toml");
    std::fs::write(
        &toml,
        "[media]\norigin = \"127.0.0.1:8096\"\nallow = [\"Cy\"]\n",
    )
    .unwrap();
    commonwealth_media::write_declared_in(
        &commonwealth_media::dir_under(dir.path()),
        "authorization",
        "viewer-token",
    )
    .unwrap();
    let (code, body) = reload(&base).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["origin"], "127.0.0.1:8096");
    assert_eq!(body["declared"], 1);
    let row = media_row(&base).await.expect("the offer is served");
    assert_eq!(row.addr.port(), 8096);
    assert_eq!(row.admit, Admit::Members(vec!["Cy".to_string()]));
    assert_eq!(published_origin(&base).await, "127.0.0.1:8096");

    std::fs::write(&toml, "[media]\norigin = \"not an address\"\n").unwrap();
    let (code, body) = reload(&base).await;
    assert_eq!(code, 422, "a file that does not load is refused: {body}");
    assert_eq!(
        media_row(&base).await.map(|r| r.addr.port()),
        Some(8096),
        "a refused reload changes nothing"
    );

    std::fs::write(&toml, "").unwrap();
    let (code, body) = reload(&base).await;
    assert_eq!(code, 200, "{body}");
    assert!(media_row(&base).await.is_none(), "no [media] withdraws it");
    assert!(published_origin(&base).await.is_null());
}
