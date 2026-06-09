// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP reverse proxy for /app/{app_id}/* routes.
//!
//! Forwards requests to the app's local port. The `app_port` must be looked
//! up from the AppRegistry (stored alongside the manifest) or an app-process
//! table maintained by the daemon. For now, the port is stored in `AppEntry`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

/// Registry of locally running app ports: app_id → port.
#[derive(Clone, Default)]
pub struct AppPortMap {
    ports: Arc<RwLock<HashMap<String, u16>>>,
}

impl AppPortMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set(&self, app_id: &str, port: u16) {
        self.ports.write().await.insert(app_id.to_string(), port);
    }

    pub async fn remove(&self, app_id: &str) {
        self.ports.write().await.remove(app_id);
    }

    pub async fn get(&self, app_id: &str) -> Option<u16> {
        self.ports.read().await.get(app_id).copied()
    }
}

/// Build a reqwest Client for forwarding.
pub fn proxy_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("failed to build proxy client")
}

/// Forward an HTTP request to the app running at `port`, replacing the path
/// prefix `/app/{app_id}` with `/`.
///
/// Returns the upstream response as raw bytes + status code, or an error string.
pub async fn forward(
    client: &reqwest::Client,
    port: u16,
    method: reqwest::Method,
    path_suffix: &str,
    headers: reqwest::header::HeaderMap,
    body: bytes::Bytes,
) -> Result<reqwest::Response, String> {
    let url = format!("http://127.0.0.1:{port}{path_suffix}");
    let mut req = client.request(method, &url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    req.body(body)
        .send()
        .await
        .map_err(|e| format!("proxy error: {e}"))
}
