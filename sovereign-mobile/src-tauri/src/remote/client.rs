// SPDX-License-Identifier: AGPL-3.0-or-later
//! The phone's REST client — the tenant-authed half.
//!
//! # What this is, now that it is not the wire
//!
//! Conversation CRUD is NOT here any more. It is
//! [`sovereign_turn_client::TurnClient`], and the two inline `#[derive(
//! Deserialize)] struct Wrap { conversations }` / `struct R { id }`
//! envelopes that used to parse it by hand are gone with it — they were the
//! REST half of the same hand-rolled consumer `remote::dto`'s `ServerEvent`
//! was the streaming half (sv-surface R6, `sv-one-client`).
//!
//! What remains is the tenant-front's OWN surface — `GET /v1/corpora` and
//! the reading window — plus the two things the turn client deliberately
//! does not do: inject the tenant bearer token, and surface `503 +
//! Retry-After` as [`Error::HostBusy`] so the busy state stays distinct
//! from a hard failure. See `Self::turn` for where that boundary bites.

use reqwest::{Client, StatusCode};
use sovereign_turn_client::TurnClient;

use crate::error::{Error, Result};
use crate::remote::dto::{
    Citation, ConversationDto, CorpusListDto, CorpusRefDto, MessageDto, Provenance,
    ReadingWindowDto,
};

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
        // Bound the dial so an unreachable/misdialed host fails fast (→ a quick
        // `HostDown` for the connectivity banner) instead of hanging on the OS
        // TCP timeout. The request cap is generous — REST calls are small, and
        // the long-running chat stream rides a separate WS path, not this client.
        let http = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(6))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            http,
            base_url,
            token,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// The host root this client dials — what [`TurnClient`] is built on.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// THE client family, pointed at this host.
    ///
    /// # The gap this method does not close
    ///
    /// `TurnClient` builds its own bare `reqwest::Client` and dials the
    /// WebSocket with `connect_async(url)` — it has no way to carry a
    /// bearer token on either transport. So this reaches a DAEMON (which
    /// authenticates nobody) and not the tenant-front `sovereign-server`
    /// with `[auth] mode = "api_key"` set, which is the campaign's
    /// daemon-first order and not an oversight. The tenant path needs a
    /// token seam on the family; until it has one, the token is injected
    /// only by the REST calls below. Said here rather than defaulted
    /// silently (ARCH §18.3).
    pub fn turn(&self) -> TurnClient {
        TurnClient::new(self.base_url.clone())
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
        let rows = self
            .turn()
            .list_conversations(None, None)
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|c| ConversationDto {
                id: c.id,
                title: c.title,
                // The list projection carries no bodies; the phone's list
                // screen renders titles and reconciles per-conversation on
                // open. Empty because the host sent none, which is what the
                // cache upsert must not mistake for "the conversation was
                // emptied" — `upsert_conversation` writes the row only.
                messages: Vec::new(),
                created_at: c.created_at,
                updated_at: c.updated_at,
                synced_version: None,
                indexed_in_corpus: false,
            })
            .collect())
    }

    pub async fn get_conversation(&self, id: &str) -> Result<ConversationDto> {
        let h = self
            .turn()
            .get_conversation(id)
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        Ok(ConversationDto {
            id: h.id,
            title: h.title,
            messages: h.messages.into_iter().map(message_dto).collect(),
            created_at: h.created_at,
            updated_at: h.updated_at,
            synced_version: None,
            indexed_in_corpus: false,
        })
    }

    pub async fn create_conversation(&self) -> Result<String> {
        Ok(self
            .turn()
            .create_conversation(None, None)
            .await
            .map_err(|e| Error::Http(e.to_string()))?
            .id)
    }

    pub async fn delete_conversation(&self, id: &str) -> Result<()> {
        self.turn()
            .delete_conversation(id)
            .await
            .map_err(|e| Error::Http(e.to_string()))
    }

    pub async fn list_corpora(&self) -> Result<Vec<CorpusRefDto>> {
        let c: CorpusListDto = self.get_json("/v1/corpora").await?;
        Ok(c.corpora)
    }

    /// Fetch the full cited passage + a window of surrounding chunks for
    /// the reader. `chunk_id` is the opaque string handle the client holds;
    /// the host parses it as a numeric corpus chunk id (a non-numeric id
    /// simply 404s and the reader degrades to the cached snippet).
    pub async fn read_chunk(&self, corpus_id: &str, chunk_id: &str) -> Result<ReadingWindowDto> {
        self.get_json(&format!(
            "/v1/corpora/{corpus_id}/chunks/{chunk_id}?radius=1"
        ))
        .await
    }
}

/// One history message, as the phone's cache and WebView want it.
///
/// The projections (`Provenance`, `Citation`) are the CONTRACT's, carried
/// straight through — this maps the envelope, never the payload, which is
/// the difference between an adapter and the mirror R6 deleted.
fn message_dto(m: sovereign_turn_client::ConversationMessage) -> MessageDto {
    let provenance: Option<Provenance> = m.provenance;
    let citations: Vec<Citation> = m.citations;
    MessageDto {
        id: m.id,
        conversation_id: String::new(),
        role: m.role,
        content: m.content,
        status: Some("complete".into()),
        created_at: m.created_at,
        server_version: None,
        provenance,
        citations,
        metadata: None,
    }
}
