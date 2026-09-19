// SPDX-License-Identifier: AGPL-3.0-or-later
//! The peer knowledge-read budget (seat A23).
//!
//! A corpus read (`/internal/knowledge/search`, ~ms of I/O) is a different
//! resource from an inference, and the member owns the two separately: the
//! 4B ring-room run had one offloaded gate judge take Bo's single
//! `max_peer_inflight` slot, and Bo then 503'd every other member's fan-out to
//! its corpora. This counter is the read's own ceiling
//! (`[daemon] max_peer_knowledge_reads`). The decision itself stays in
//! `AppState::admit_peer_request`; this only holds the count.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

#[derive(Debug)]
pub struct KnowledgeReadBudget {
    inflight: AtomicUsize,
    ceiling: AtomicUsize,
}

impl KnowledgeReadBudget {
    /// Unbounded until the daemon applies the configured ceiling at boot —
    /// the same pre-configuration rule as the peer-inflight ceiling.
    pub fn new() -> Self {
        Self {
            inflight: AtomicUsize::new(0),
            ceiling: AtomicUsize::new(usize::MAX),
        }
    }

    pub fn set_ceiling(&self, max: usize) {
        self.ceiling.store(max, Relaxed);
    }

    pub fn ceiling(&self) -> usize {
        self.ceiling.load(Relaxed)
    }

    /// Take one read slot if under the ceiling; `false` means refuse.
    pub fn try_take(&self) -> bool {
        let max = self.ceiling();
        self.inflight
            .fetch_update(Relaxed, Relaxed, |n| (n < max).then_some(n + 1))
            .is_ok()
    }

    pub fn release(&self) {
        let _ = self
            .inflight
            .fetch_update(Relaxed, Relaxed, |n| n.checked_sub(1));
    }
}

impl Default for KnowledgeReadBudget {
    fn default() -> Self {
        Self::new()
    }
}
