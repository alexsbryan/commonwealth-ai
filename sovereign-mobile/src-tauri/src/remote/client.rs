//! HTTP client for the host's `sovereign-server`. Injects the tenant
//! token, only ever dials the configured tailnet address (no fallback
//! route — fail-closed off-tailnet is enforced upstream by the monitor),
//! and surfaces `503 + Retry-After` as [`Error::HostBusy`] so the busy
//! state stays distinct from a hard failure.

use reqwest::{Client, StatusCode};

use crate::error::{Error, Result};
use crate::remote::dto::{ConversationDto, CorpusListDto, CorpusRefDto, ReadingWindowDto};

#[derive(Clone)]
pub struct ApiClient {
    http: Client,
    /// `http://<tailnet_address>` — the ONLY origin this client dials.
    base_url: String,
    token: String,
}

impl ApiClient {
    pub fn new(tailnet_address: &str, token: String) -> Self {
        let trimmed = tailnet_address.trim_end_matches('/');
        let base_url = if trimmed.starts_with("http") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        };
        Self {
            http: Client::new(),
            base_url,
            token,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// `ws(s)://<host>/v1/conversations/<id>/stream`.
    pub fn ws_url(&self, conversation_id: &str) -> String {
        let ws_base = self.base_url.replacen("http", "ws", 1);
        format!("{ws_base}/v1/conversations/{conversation_id}/stream")
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    async fn parse<T: serde::de::DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        match resp.status() {
            StatusCode::SERVICE_UNAVAILABLE => {
                let retry_after_secs = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(2);
                Err(Error::HostBusy { retry_after_secs })
            }
            StatusCode::UNAUTHORIZED => Err(Error::Unauthenticated),
            s if s.is_success() => Ok(resp.json::<T>().await?),
            s => Err(Error::Http(s.to_string())),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await?;
        self.parse(resp).await
    }

    pub async fn list_conversations(&self) -> Result<Vec<ConversationDto>> {
        #[derive(serde::Deserialize)]
        struct Wrap {
            #[serde(default)]
            conversations: Vec<ConversationDto>,
        }
        let w: Wrap = self.get_json("/v1/conversations").await?;
        Ok(w.conversations)
    }

    pub async fn get_conversation(&self, id: &str) -> Result<ConversationDto> {
        self.get_json(&format!("/v1/conversations/{id}")).await
    }

    pub async fn create_conversation(&self) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct R {
            id: String,
        }
        let resp = self
            .http
            .post(self.url("/v1/conversations"))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let r: R = self.parse(resp).await?;
        Ok(r.id)
    }

    pub async fn delete_conversation(&self, id: &str) -> Result<()> {
        let resp = self
            .http
            .delete(self.url(&format!("/v1/conversations/{id}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(Error::Http(resp.status().to_string()))
        }
    }

    pub async fn list_corpora(&self) -> Result<Vec<CorpusRefDto>> {
        let c: CorpusListDto = self.get_json("/v1/corpora").await?;
        Ok(c.corpora)
    }

    /// Fetch the full cited passage + a window of surrounding chunks for
    /// the reader. `chunk_id` is the opaque string handle the client holds;
    /// the host parses it as a numeric corpus chunk id (a non-numeric id
    /// simply 404s and the reader degrades to the cached snippet).
    pub async fn read_chunk(
        &self,
        corpus_id: &str,
        chunk_id: &str,
    ) -> Result<ReadingWindowDto> {
        self.get_json(&format!(
            "/v1/corpora/{corpus_id}/chunks/{chunk_id}?radius=1"
        ))
        .await
    }
}
