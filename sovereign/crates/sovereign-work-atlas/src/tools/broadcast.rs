// SPDX-License-Identifier: AGPL-3.0-or-later
//! Indirection seam for the work atlas's "make this claim visible now"
//! requirement (§7 of the spec).
//!
//! No implementation is wired today. The daemon's `MeshBroadcaster` hurried
//! the two hops that carry a claim (drain the outbox onto the journal, then
//! ask the ring round to run); it was deleted in five-programs fp-83 because
//! the store it drained was no longer the one the atlas writes. A claim now
//! travels on cw-rails' 2 s pump tick plus the ring round, and the
//! [`DeferredBroadcaster`] the tools and observer hold stays unset, a no-op.
//! A dropped call costs latency and never the write.

use std::sync::Arc;

use arc_swap::ArcSwapOption;
use async_trait::async_trait;

#[async_trait]
pub trait ClaimBroadcaster: Send + Sync + std::fmt::Debug {
    /// Best-effort: make the write at `(app_id, key)` visible to peers as
    /// soon as this node can, rather than on the replication path's own
    /// clock. Must not block on slow peers, and must be safe to skip — the
    /// write is durable and travels either way.
    async fn broadcast(&self, app_id: &str, key: &str);
}

/// No-op broadcaster — used in tests, in the standalone CLI path,
/// and as a placeholder when the daemon hasn't wired the real one
/// yet. Claims still become visible to peers via the next gossip
/// round; the only thing lost is sub-10s latency.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullBroadcaster;

#[async_trait]
impl ClaimBroadcaster for NullBroadcaster {
    async fn broadcast(&self, _app_id: &str, _key: &str) {}
}

/// Holds an `Arc<dyn ClaimBroadcaster>` that the daemon can swap in
/// later — exactly once or many times. Calls before `set()` are
/// silently no-op'd (the next gossip round still propagates).
pub struct DeferredBroadcaster {
    inner: ArcSwapOption<Box<dyn ClaimBroadcaster>>,
}

impl DeferredBroadcaster {
    pub fn new() -> Self {
        Self {
            inner: ArcSwapOption::empty(),
        }
    }

    /// Install the real broadcaster. Safe to call multiple times.
    pub fn set(&self, b: Box<dyn ClaimBroadcaster>) {
        self.inner.store(Some(Arc::new(b)));
    }
}

impl Default for DeferredBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DeferredBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self.inner.load_full().is_some();
        f.debug_struct("DeferredBroadcaster")
            .field("set", &set)
            .finish()
    }
}

#[async_trait]
impl ClaimBroadcaster for DeferredBroadcaster {
    async fn broadcast(&self, app_id: &str, key: &str) {
        if let Some(inner) = self.inner.load_full() {
            inner.as_ref().broadcast(app_id, key).await;
        } else {
            tracing::debug!(
                app_id,
                key,
                "work_atlas:broadcast deferred (real broadcaster not yet wired); \
                 next gossip round will catch up"
            );
        }
    }
}
