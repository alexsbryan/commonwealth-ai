// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two ports a daemon speaks to its peers through, and the N=1 answers.
//!
//! Minted 2026-09-04 for cw-lift rung 3b. Before it, `sovereign-cli-daemon`
//! named `commonwealth_state::MeshStore` and
//! `commonwealth_api::state::ConvergenceRecord` directly, and those two types
//! were the whole reason the local daemon could not link without the mesh
//! substrate. The couplings were eleven lines in two files; what they cost was
//! the entire `commonwealth-*` closure on a binary that has no mesh to speak
//! to until one is configured.
//!
//! # Mesh-of-one is kept, at runtime
//!
//! These are ports, not switches. The mesh-of-one design is the reason the
//! daemon has never needed an `if local { … }` branch: **every mesh operation
//! has a total, correct N=1 answer rather than a special case.** A roster of
//! one makes `should_host` true. A mesh of one is trivially converged. A store
//! of one makes replication the identity function. Because the degenerate case
//! is CORRECT rather than SKIPPED, the two paths cannot drift.
//!
//! [`SoloPeerStore`] and [`SoloConvergence`] are that N=1 answer written out.
//! Neither is a null object:
//!
//! - `SoloPeerStore` really stores. `set` then `get` returns what was set;
//!   `scan` enumerates; `delete` removes. Replication to the other zero peers
//!   is the identity function, which is why there is nothing to send — not
//!   because sending was skipped.
//! - `SoloConvergence` really records. A publish onto a mesh of one IS a
//!   successful publish, so the stamp is real, and `snapshot` reports `None`
//!   for a path that has genuinely never run (ARCH §18.3 — absence is
//!   reported, never defaulted).
//!
//! Neither may ever grow a "not applicable locally" arm. If a port method
//! cannot be answered honestly at N=1, the port is drawn in the wrong place.
//!
//! # Why these two are narrow, and not one `Mesh` trait
//!
//! A trait past ~8 methods with no sub-trait shape is the §5.1 smell, and
//! cw-lift rung 1e already refused a wide `Membership` seam on measured
//! evidence (roster admission 26.5 ns against a 4.10 ms append). [`PeerStore`]
//! is the four methods its two consumers actually call — measured across
//! `sovereign-work-atlas` (get/set/delete/scan) and the daemon's notes publish
//! sink and ingest poller (set/scan) — out of the fourteen `MeshStore`
//! exposes. [`Convergence`] is three.

use std::collections::BTreeMap;
use std::sync::Mutex;

use bytes::Bytes;
use kernel_types::NodeId;

/// Why a [`PeerStore`] call could not be served.
///
/// One variant. The backing store is the only thing that can fail — a
/// well-formed key is never itself a refusal, which is the property
/// [`SoloPeerStore`]'s totality tests pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerStoreError {
    /// The backing store refused or failed. Carries the store's own message.
    Backend(String),
}

impl std::fmt::Display for PeerStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(m) => write!(f, "peer store backend: {m}"),
        }
    }
}

impl std::error::Error for PeerStoreError {}

/// One record in a [`PeerStore`], as the reader sees it.
///
/// `origin` is which node wrote it — the field the daemon's ingest poller
/// filters on so it does not re-ingest its own publications. `timestamp` is
/// unix seconds and carries the last-write-wins ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEntry {
    /// Namespace the record lives in.
    pub app_id: String,
    /// Key within the namespace.
    pub key: String,
    /// The stored bytes.
    pub value: Bytes,
    /// Unix seconds of the write that produced this value.
    pub timestamp: u64,
    /// Node that originated the write.
    pub origin: NodeId,
}

/// A replicated key-value store shared with the node's peers.
///
/// Four methods, because four is what the consumers call. Implementations are
/// expected to be cheap to clone behind an `Arc` and safe to call from many
/// tasks; every method takes `&self`.
///
/// **Totality is part of the contract.** An implementation may return
/// [`PeerStoreError::Backend`] when its storage genuinely fails, and may never
/// return one because a call "does not apply" in its topology.
pub trait PeerStore: Send + Sync {
    /// Read one record, or `None` when the key is absent.
    fn get(&self, app_id: &str, key: &str) -> Result<Option<PeerEntry>, PeerStoreError>;

    /// Write one record. Returns whether the stored value CHANGED — a
    /// re-publication of identical bytes reports `false` and is still stored.
    fn set(
        &self,
        app_id: &str,
        key: &str,
        value: Bytes,
        origin: NodeId,
    ) -> Result<bool, PeerStoreError>;

    /// Remove one record. Returns whether anything was there to remove.
    fn delete(&self, app_id: &str, key: &str) -> Result<bool, PeerStoreError>;

    /// Every record in `app_id` whose key starts with `prefix`. An empty
    /// prefix enumerates the namespace.
    fn scan(&self, app_id: &str, prefix: &str) -> Result<Vec<PeerEntry>, PeerStoreError>;
}

/// The liveness stamps of a two-way convergence path.
///
/// Written by whatever publishes outbound and applies inbound; read by a
/// status surface as "when did each direction last actually work". A `None`
/// stamp means that direction has never succeeded since boot, and is reported
/// as absent rather than defaulted to a time (ARCH §18.3).
pub trait Convergence: Send + Sync {
    /// Stamp the outbound publish path as alive at `at_unix`.
    fn record_outbound_publish_success(&self, at_unix: i64);

    /// Stamp the inbound apply path as alive at `at_unix`.
    fn record_inbound_ingest_success(&self, at_unix: i64);

    /// `(last_outbound, last_inbound)`, each `None` until that path succeeds.
    fn snapshot(&self) -> (Option<i64>, Option<i64>);
}

// ── The N=1 answers ──────────────────────────────────────────────────────────

/// [`PeerStore`] for a mesh of one.
///
/// The honest N=1 implementation, not a null object: it stores, reads back,
/// enumerates and deletes exactly like a store with peers. What is absent is
/// replication, and only because replicating to zero peers is the identity
/// function — every write is already everywhere it needs to be the instant it
/// lands.
///
/// Constructs infallibly and does no I/O, which is the property that lets a
/// local daemon come up with nothing to mint and nothing that can refuse.
#[derive(Debug, Default)]
pub struct SoloPeerStore {
    // `(app_id, key)` ordered so `scan`'s prefix walk is a range and the
    // enumeration order is stable across runs.
    entries: Mutex<BTreeMap<(String, String), PeerEntry>>,
}

impl SoloPeerStore {
    /// An empty store. Infallible, allocation-only, no I/O.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many records are held, across every namespace.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the store holds nothing.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), PeerEntry>> {
        // A poisoned lock is recovered rather than propagated: a panic in some
        // other task must not turn every later store call into a refusal, which
        // would break the totality this type exists to provide.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Unix seconds, saturating at 0 before the epoch. Local to the solo store's
/// last-write-wins stamp; the mesh store has its own clock.
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl PeerStore for SoloPeerStore {
    fn get(&self, app_id: &str, key: &str) -> Result<Option<PeerEntry>, PeerStoreError> {
        Ok(self
            .lock()
            .get(&(app_id.to_string(), key.to_string()))
            .cloned())
    }

    fn set(
        &self,
        app_id: &str,
        key: &str,
        value: Bytes,
        origin: NodeId,
    ) -> Result<bool, PeerStoreError> {
        let mut entries = self.lock();
        let id = (app_id.to_string(), key.to_string());
        let changed = entries.get(&id).map(|e| e.value != value).unwrap_or(true);
        entries.insert(
            id,
            PeerEntry {
                app_id: app_id.to_string(),
                key: key.to_string(),
                value,
                timestamp: now_unix_secs(),
                origin,
            },
        );
        Ok(changed)
    }

    fn delete(&self, app_id: &str, key: &str) -> Result<bool, PeerStoreError> {
        Ok(self
            .lock()
            .remove(&(app_id.to_string(), key.to_string()))
            .is_some())
    }

    fn scan(&self, app_id: &str, prefix: &str) -> Result<Vec<PeerEntry>, PeerStoreError> {
        Ok(self
            .lock()
            .iter()
            .filter(|((a, k), _)| a == app_id && k.starts_with(prefix))
            .map(|(_, e)| e.clone())
            .collect())
    }
}

/// [`Convergence`] for a mesh of one.
///
/// Identical in kind to the mesh recorder, and deliberately so: a publish onto
/// a mesh of one succeeds, so the outbound stamp is a real success, and an
/// inbound apply that has never run reports `None` rather than "now". There is
/// no arm here that reads "converged, nothing to check" — that would be the
/// null object this type exists not to be.
#[derive(Debug, Default)]
pub struct SoloConvergence {
    stamps: Mutex<(Option<i64>, Option<i64>)>,
}

impl SoloConvergence {
    /// Both paths never-succeeded. Infallible, no I/O.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Convergence for SoloConvergence {
    fn record_outbound_publish_success(&self, at_unix: i64) {
        self.stamps.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(at_unix);
    }

    fn record_inbound_ingest_success(&self, at_unix: i64) {
        self.stamps.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(at_unix);
    }

    fn snapshot(&self) -> (Option<i64>, Option<i64>) {
        *self.stamps.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests;
