// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashSet;
use std::sync::Arc;

use sovereign_core::error::Result;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::StateStore;
use sovereign_core::types::{CorpusVisibility, Response};

/// Wraps a Runtime with tenant-scoped conversation IDs.
/// Each tenant's conversations are prefixed to prevent cross-tenant access.
pub struct TenantRuntime {
    pub runtime: Arc<Runtime>,
    /// The server's own state-store handle — the same `Arc` `main.rs`
    /// hands to `Runtime::new`, layered as its own Extension.
    /// `forbidden_corpora` reads it directly instead of `runtime.store`,
    /// and `ws.rs` hands it to `serve_turn` so the turn service can read
    /// back what the turn concluded. The Runtime is named here only for
    /// the three methods that answer a turn. Daemon-convergence Phase 0;
    /// the streaming pair left in phase 5c.
    pub store: Arc<dyn StateStore>,
    pub tenant_id: String,
}

impl TenantRuntime {
    pub fn new(runtime: Arc<Runtime>, store: Arc<dyn StateStore>, tenant_id: String) -> Self {
        Self {
            runtime,
            store,
            tenant_id,
        }
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
        let states = self.store.list_corpus_states().await?;
        Ok(states
            .into_iter()
            .filter(|s| s.deleted_at.is_none())
            .filter_map(|s| match s.visibility {
                CorpusVisibility::Private { owner } if owner != self.tenant_id => Some(s.corpus_id),
                _ => None,
            })
            .collect())
    }

    // `handle_message` and `handle_message_any` used to live here: two
    // thin wrappers that scoped an id and then ran a turn. Both are gone
    // (TOPOLOGY §10 phase 6). Their only caller, the REST message route,
    // now scopes the id itself — once, visibly, exactly as `ws.rs` does —
    // and hands it to `sovereign_core::runtime::collect_turn`, the same
    // driver the WebSocket route uses.
    //
    // What this host actually owns is TENANCY, and that is what is left
    // here: `scoped_id`, the corpus visibility filter, and seeding. Running
    // a turn was never this type's job; it only looked like it because
    // there was nowhere else to put the call.

    /// Seed an empty conversation row + optional skill tag before the
    /// first message (scoped to this tenant). `skill_id =
    /// "recipe-author"` makes the turn driver dispatch into the
    /// agent loop.
    pub async fn seed_conversation(
        &self,
        conversation_id: &str,
        created_at: i64,
        skill_id: Option<&str>,
        enabled_corpora: Option<&[String]>,
    ) -> Result<()> {
        let scoped = self.scoped_id(conversation_id);
        self.runtime
            .seed_conversation(&scoped, created_at, skill_id, enabled_corpora)
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
