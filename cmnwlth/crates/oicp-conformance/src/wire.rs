// SPDX-License-Identifier: AGPL-3.0-or-later
//! One reply shape for the v0.5 checks: status, headers and body together,
//! so a check can judge a refusal by its status and its named reason.

use reqwest::header::HeaderMap;
use reqwest::{Method, RequestBuilder};
use serde_json::Value;

use crate::checks::Host;

/// What a host answered.
#[derive(Debug, Clone)]
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// Response headers.
    pub headers: HeaderMap,
    /// The body as JSON, or as a JSON string when it is not JSON.
    pub body: Value,
}

impl Reply {
    /// Whether the request reached the route: neither refused for who sent
    /// it (401, 403) nor sent to a route that is not there (404, 405).
    pub fn admitted(&self) -> bool {
        !matches!(self.status, 401 | 403 | 404 | 405)
    }

    /// The named reason of a refusal, `{"error": "<reason>"}`.
    pub fn error(&self) -> Option<&str> {
        self.body.get("error").and_then(Value::as_str)
    }

    /// `status` and the body, for a failure's detail.
    pub fn brief(&self) -> String {
        let body = self.body.to_string();
        let cut: String = body.chars().take(160).collect();
        format!("HTTP {} {cut}", self.status)
    }
}

/// Send and read a reply; `Err` only when no reply came.
pub async fn send(rb: RequestBuilder) -> Result<Reply, String> {
    let resp = rb.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Ok(Reply {
        status,
        headers,
        body,
    })
}

impl Host {
    /// A request carrying no credential at all, whatever `--token` says:
    /// what a web page, or a stranger, sends.
    pub(crate) fn bare(&self, method: Method, path: &str) -> RequestBuilder {
        self.client.request(method, self.url(path))
    }

    /// A request carrying exactly `bearer`.
    pub(crate) fn with_bearer(&self, method: Method, path: &str, bearer: &str) -> RequestBuilder {
        self.bare(method, path).bearer_auth(bearer)
    }

    /// GET `path` with the run's credential, as JSON.
    pub(crate) async fn get(&self, path: &str) -> Result<Reply, String> {
        send(self.req(Method::GET, &self.url(path))).await
    }

    /// POST `body` to `path` with the run's credential.
    pub(crate) async fn post(&self, path: &str, body: &Value) -> Result<Reply, String> {
        send(self.req(Method::POST, &self.url(path)).json(body)).await
    }

    /// `host:port` of the base URL, as a client would send it in `Host`.
    pub(crate) fn authority(&self) -> String {
        let rest = self
            .base
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        rest.split('/').next().unwrap_or(rest).to_string()
    }
}
