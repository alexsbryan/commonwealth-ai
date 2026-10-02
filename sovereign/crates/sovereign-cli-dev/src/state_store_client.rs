// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workbench's [`ConversationStore`] client — the dial, not the file
//! (fp-32; five-programs §4 rule 1: one data directory, one owner — a
//! second process never opens `state.db`, it dials the daemon that owns
//! it).
//!
//! Two routes, both already served by the daemon
//! (`sovereign-daemon/src/turn_http.rs`): `GET /v1/conversations` and
//! `GET /v1/conversations/{id}`. No route was minted for this client
//! (principle 11): the four other `ConversationStore` impls in the tree
//! are sqlite/postgres/memory plus a test mock — none is a client, so
//! none could serve the workbench's `audit --recover` pass.
//!
//! Absence is reported, never defaulted (principle 6): a daemon that is
//! down surfaces as `Err` naming the URL it could not reach, which the
//! caller renders as "the inferred-source pass could not run" — it never
//! reads as a quiet, empty store. The read-only shape is honest for the
//! same reason: every write and every unserved read says `NotImplemented`
//! rather than pretending success.
//!
//! The base URL is the audited accessor
//! ([`sovereign_cli_base::urls::daemon_v1_base`]), not a hardcoded
//! loopback — the `SOVEREIGN_DAEMON_URL` knob moves this client with
//! every other reader (the `rail` client is the precedent).

use async_trait::async_trait;
use sovereign_contracts::daemon_wire::ConversationListEntry;
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::ConversationStore;
use sovereign_contracts::types::{Conversation, Message, Role};

/// `GET /v1/conversations` — the envelope the daemon's
/// `ConversationListResponse` serializes. The rows are the shared wire
/// type ([`ConversationListEntry`], now read as well as written — one
/// schema, both directions).
#[derive(Debug, serde::Deserialize)]
struct ConversationListWire {
    conversations: Vec<ConversationListEntry>,
}

/// `GET /v1/conversations/{id}` — the fields this client reads out of the
/// daemon's `ConversationResponse` (a client-side READ, not a second
/// definition of the type: the daemon's envelope closes over runtime
/// types and stays put, per `daemon_wire`'s own charter).
#[derive(Debug, serde::Deserialize)]
struct ConversationWire {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    messages: Vec<MessageWire>,
    created_at: i64,
    updated_at: i64,
    #[serde(default)]
    enabled_corpora: Option<Vec<String>>,
}

/// One message row of [`ConversationWire`]. The daemon renders `role` as
/// the lowercase wire string, which is `Role`'s own serde form.
#[derive(Debug, serde::Deserialize)]
struct MessageWire {
    id: String,
    role: Role,
    content: String,
    created_at: i64,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

/// A `ConversationStore` whose backend is the local daemon's HTTP
/// surface. Read-only by construction: it serves the two routes the
/// `audit --recover` inferred-source pass needs.
pub struct DaemonConversationStore {
    http: reqwest::Client,
    /// The daemon's `/v1` root, from the audited accessor.
    v1: String,
}

impl DaemonConversationStore {
    /// Dial the local daemon. Construction resolves the base URL and
    /// builds the HTTP client; daemon PRESENCE is not checked here — a
    /// daemon that is down surfaces on the first call as a named
    /// transport error.
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| Error::Storage(format!("http client: {e}")))?,
            v1: sovereign_cli_base::urls::daemon_v1_base().map_err(Error::Storage)?,
        })
    }

    /// One GET, with the absence and refusal paths NAMED: transport
    /// failure carries the URL, a non-success carries the daemon's own
    /// wording, and an unparseable answer says the build and the daemon
    /// disagree on the shape (the `rail` client's error family).
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: String) -> Result<T> {
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Storage(format!("cannot reach the daemon at {url}: {e}")))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            Error::Storage(format!("the daemon's answer at {url} is unreadable: {e}"))
        })?;
        if !status.is_success() {
            return Err(Error::Storage(format!(
                "the daemon refused {url}: {status}: {body}"
            )));
        }
        serde_json::from_str(&body).map_err(|e| {
            Error::Storage(format!(
                "the daemon's answer at {url} is a shape this build cannot read: {e}"
            ))
        })
    }
}

/// The listing rows, as conversation shells — the list route serves the
/// conversation columns, not the message rows, exactly like the sqlite
/// listing this dial replaced.
fn convo_shell(e: ConversationListEntry) -> Conversation {
    Conversation {
        id: e.id,
        title: e.title,
        messages: Vec::new(),
        created_at: e.created_at,
        updated_at: e.updated_at,
        version: 0,
        deleted_at: None,
        skill_id: None,
        enabled_corpora: None,
        searched_sources: None,
    }
}

#[async_trait]
impl ConversationStore for DaemonConversationStore {
    async fn save_message(&self, _msg: &Message) -> Result<()> {
        Err(Error::NotImplemented(
            "the workbench's conversation client dials the daemon's read routes; \
             writes belong to the process that owns the store"
                .to_string(),
        ))
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let url = format!("{}/conversations/{id}", self.v1);
        let wire: ConversationWire = self.get_json(url).await?;
        Ok(Conversation {
            messages: wire
                .messages
                .into_iter()
                .map(|m| Message {
                    conversation_id: wire.id.clone(),
                    id: m.id,
                    role: m.role,
                    content: m.content,
                    created_at: m.created_at,
                    metadata: m.metadata,
                    version: 0,
                })
                .collect(),
            id: wire.id,
            title: wire.title,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: wire.enabled_corpora,
            searched_sources: None,
        })
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        let url = format!("{}/conversations?limit={limit}&offset={offset}", self.v1);
        let wire: ConversationListWire = self.get_json(url).await?;
        Ok(wire.conversations.into_iter().map(convo_shell).collect())
    }

    async fn list_conversations_for_surface(
        &self,
        _surface_skill_id: Option<&str>,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<Conversation>> {
        Err(Error::NotImplemented(
            "the workbench's conversation client serves the unscoped listing only".to_string(),
        ))
    }

    async fn list_conversations_for_corpus(
        &self,
        _corpus_id: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<Conversation>> {
        Err(Error::NotImplemented(
            "the workbench's conversation client serves the unscoped listing only".to_string(),
        ))
    }

    async fn search_messages(&self, _query: &str) -> Result<Vec<Message>> {
        Err(Error::NotImplemented(
            "the workbench's conversation client serves the recovery pass's two reads only"
                .to_string(),
        ))
    }

    async fn delete_conversation(&self, _id: &str) -> Result<()> {
        Err(Error::NotImplemented(
            "the workbench's conversation client dials the daemon's read routes; \
             writes belong to the process that owns the store"
                .to_string(),
        ))
    }

    async fn update_conversation_title(&self, _id: &str, _title: &str) -> Result<()> {
        Err(Error::NotImplemented(
            "the workbench's conversation client dials the daemon's read routes; \
             writes belong to the process that owns the store"
                .to_string(),
        ))
    }

    async fn insert_empty_conversation(
        &self,
        _id: &str,
        _created_at: i64,
        _surface_skill_id: Option<&str>,
    ) -> Result<()> {
        Err(Error::NotImplemented(
            "the workbench's conversation client dials the daemon's read routes; \
             writes belong to the process that owns the store"
                .to_string(),
        ))
    }
}
