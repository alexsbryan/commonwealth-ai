// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashSet;
use std::sync::Arc;

use sovereign_core::error::Result;
use sovereign_core::runtime::{Runtime, StreamHandle};
use sovereign_core::types::{CorpusVisibility, Response};

/// Wraps a Runtime with tenant-scoped conversation IDs.
/// Each tenant's conversations are prefixed to prevent cross-tenant access.
pub struct TenantRuntime {
    pub runtime: Arc<Runtime>,
    pub tenant_id: String,
}

impl TenantRuntime {
    pub fn new(runtime: Arc<Runtime>, tenant_id: String) -> Self {
        Self { runtime, tenant_id }
    }

    /// Scope a conversation ID to this tenant. Public so streaming
    /// callers can re-read the persisted message under the same key the
    /// runtime wrote it.
    pub fn scoped_id(&self, conversation_id: &str) -> String {
        format!("{}:{}", self.tenant_id, conversation_id)
    }

    /// Corpora this tenant must NOT retrieve from, list, or read: every
    /// `Private` corpus owned by a *different* principal. Everything else —
    /// shared `Org` corpora, this tenant's own `Private` uploads, and
    /// untracked/legacy corpora with no `CorpusState` row — is permitted,
    /// so single-user and pre-existing deployments are unaffected.
    ///
    /// Computed fresh from the store each call (no staleness). The caller
    /// MUST treat an `Err` as fail-closed (reject the request) rather than
    /// proceeding with an empty deny-set — a transient store error would
    /// otherwise open the gate.
    pub async fn forbidden_corpora(&self) -> Result<HashSet<String>> {
        let states = self.runtime.store.list_corpus_states().await?;
        Ok(states
            .into_iter()
            .filter(|s| s.deleted_at.is_none())
            .filter_map(|s| match s.visibility {
                CorpusVisibility::Private { owner } if owner != self.tenant_id => Some(s.corpus_id),
                _ => None,
            })
            .collect())
    }

    pub async fn handle_message(&self, message: &str, conversation_id: &str) -> Result<Response> {
        let scoped = self.scoped_id(conversation_id);
        self.runtime.handle_message(message, &scoped).await
    }

    /// Streaming variant — yields a [`StreamHandle`] whose `stream`
    /// produces token deltas and whose `message_id` identifies the
    /// assistant message the runtime persists once the stream is
    /// exhausted. Scoping mirrors [`Self::handle_message`].
    pub async fn handle_message_stream(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        let scoped = self.scoped_id(conversation_id);
        self.runtime.handle_message_stream(message, &scoped).await
    }

    /// Fetch the persisted `metadata` blob for a message in this
    /// tenant's conversation. Used after a stream completes to project
    /// provenance + citations for the terminal frame. Returns `None`
    /// when the conversation/message isn't found or carries no metadata
    /// (the projection layer treats all of these identically).
    pub async fn message_metadata(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Option<serde_json::Value> {
        let scoped = self.scoped_id(conversation_id);
        let convo = self.runtime.store.get_conversation(&scoped).await.ok()?;
        convo
            .messages
            .into_iter()
            .find(|m| m.id == message_id)
            .and_then(|m| m.metadata)
    }

    /// Non-streaming entry that routes workspace-tagged conversations
    /// (recipe-author) into the agent loop; generic ones behave like
    /// [`Self::handle_message`]. The conversation API uses this.
    pub async fn handle_message_any(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        let scoped = self.scoped_id(conversation_id);
        self.runtime.handle_message_any(message, &scoped).await
    }

    /// Seed an empty conversation row + optional skill tag before the
    /// first message (scoped to this tenant). `skill_id =
    /// "recipe-author"` makes [`Self::handle_message_any`] drive the
    /// agent loop.
    pub async fn seed_conversation(
        &self,
        conversation_id: &str,
        created_at: i64,
        skill_id: Option<&str>,
    ) -> Result<()> {
        let scoped = self.scoped_id(conversation_id);
        self.runtime
            .seed_conversation(&scoped, created_at, skill_id)
            .await
    }
}

/// Resolves the tenant (principal) that owns a conversation from its
/// `"{tenant}:{conv}"` scoped id — the inverse of [`TenantRuntime::scoped_id`].
/// Injected into the `Runtime` so corpus retrieval is scoped per principal
/// (another tenant's `Private` corpora never enter a turn's evidence) without
/// the Runtime knowing tenancy exists. A conversation id with no prefix
/// resolves to `None` (no scoping) — but server-issued ids are always
/// prefixed, so on the hub the principal is always present.
pub struct TenantPrincipalResolver;

impl sovereign_core::traits::PrincipalResolver for TenantPrincipalResolver {
    fn principal_for(&self, conversation_id: &str) -> Option<String> {
        conversation_id
            .split_once(':')
            .map(|(tenant, _)| tenant.to_string())
    }
}
