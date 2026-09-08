// SPDX-License-Identifier: AGPL-3.0-or-later
//! Indirection seam for the work atlas's "make this claim visible now"
//! requirement (§7 of the spec).
//!
//! The implementation lives in `sovereign-mesh::MeshBroadcaster` and needs
//! `AppState`, which would create a circular crate-dep if the work-atlas tools
//! called it directly. This trait lets the daemon wire the real one in from
//! the outside while tests and standalone callers use [`NullBroadcaster`].
//!
//! It is not a fan-out any more. Until cw-lift rung 2e the implementation
//! POSTed the entry to every online peer; now the claim write has already been
//! queued for the ring by `MeshStore::set`, and what the implementation does
//! is hurry the two hops that carry it (drain the outbox onto the journal,
//! then ask the ring round to run). A dropped call therefore costs latency and
//! never the write.
//!
//! The watcher coordinator starts before `AppState` is ready, so
//! [`DeferredBroadcaster`] lets cmd_serve construct an empty
//! broadcaster up-front, register it with the tools and observer,
//! then swap in the real `MeshBroadcaster` once the daemon's
//! `AppState` becomes available — no chicken-and-egg.

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
