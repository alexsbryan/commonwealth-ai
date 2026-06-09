// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use sovereign_core::error::Result;
use sovereign_core::runtime::{Runtime, StreamHandle};
use sovereign_core::types::Response;

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
