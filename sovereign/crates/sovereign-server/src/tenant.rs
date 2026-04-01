use std::sync::Arc;

use sovereign_core::error::Result;
use sovereign_core::runtime::Runtime;
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

    /// Scope a conversation ID to this tenant.
    fn scoped_id(&self, conversation_id: &str) -> String {
        format!("{}:{}", self.tenant_id, conversation_id)
    }

    pub async fn handle_message(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        let scoped = self.scoped_id(conversation_id);
        self.runtime.handle_message(message, &scoped).await
    }

    pub async fn end_conversation(&self, conversation_id: &str) -> Result<()> {
        let scoped = self.scoped_id(conversation_id);
        self.runtime.end_conversation(&scoped).await
    }
}
