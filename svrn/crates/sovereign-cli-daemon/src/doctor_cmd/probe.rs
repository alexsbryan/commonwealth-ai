// SPDX-License-Identifier: AGPL-3.0-or-later
//! Transport probes for `svrn doctor` — is the port open, does the endpoint
//! answer. Pure helpers with no check semantics: they report reachability and
//! nothing else, so a check that needs "unreachable" and one that needs
//! "unreachable is fine here" both build on the same answer.

use std::time::Duration;

use tokio::net::TcpStream;
use tokio::time::timeout;

// ── TCP probe ─────────────────────────────────────────────────────────────────

pub(super) async fn tcp_connectable(host: &str, port: u16) -> bool {
    timeout(Duration::from_secs(2), TcpStream::connect((host, port)))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

pub(super) async fn http_get_json(url: &str) -> Option<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(url).send().await.ok()?;
    if resp.status().is_success() {
        resp.json().await.ok()
    } else {
        None
    }
}

pub(super) async fn http_post_json(
    url: &str,
    body: serde_json::Value,
) -> Option<reqwest::Response> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    client.post(url).json(&body).send().await.ok()
}
