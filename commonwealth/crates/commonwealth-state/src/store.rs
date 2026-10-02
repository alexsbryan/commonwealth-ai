// SPDX-License-Identifier: AGPL-3.0-or-later
//! MeshStore — the distributed key-value store for mesh apps.
//!
//! Each entry is scoped to an `app_id` + `key`. Conflict resolution is LWW
//! (last-write-wins) using a Unix-second `timestamp`. The underlying storage
//! is a pure-Rust in-memory backend, or SQLite (WAL mode) via `SqliteBackend`
//! for [`MeshStore::open`] under the `sqlite` feature.
//!
//! **What this store holds is decided by the fold, not by its writers.** Since
//! cw-lift 4 an entry is replicated by the ring rail rather than by gossip, and
//! [`MeshStore::apply_projection`] re-derives every row from the journal on
//! every round. Three consequences that catch callers out:
//!
//! - A local `delete` is an ACT ([`MeshStore::delete`] queues a tombstone). A
//!   local sweep is not, so [`MeshStore::gc_app`] and its siblings only stay
//!   swept when the fold agrees — see [`crate::retention`].
//! - `merge_entry` is the receive half and deliberately queues nothing.
//! - An excluded `app_id` never enters the outbox and is refused inbound.

#[cfg(feature = "sqlite")]
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;

use commonwealth_core::ids::NodeId;

use crate::backend::Backend;
use crate::error::{Error, Result};
use crate::rail_kv::KvOp;

/// A single entry in the mesh store.
#[derive(Debug, Clone)]
pub struct StoreEntry {
    pub app_id: String,
    pub key: String,
    pub value: Bytes,
    /// Unix seconds — last-write-wins conflict resolution.
    pub timestamp: u64,
    /// Node that originated this write.
    pub origin: NodeId,
}

/// One local write waiting to go onto the ring journal: the act itself, plus
/// where it is queued and which namespace it belongs to.
///
/// The act is a [`KvOp`] — the SAME type
/// [`rail_kv::to_payload`](crate::rail_kv::to_payload) puts on the journal and
/// [`rail_kv::from_payload`](crate::rail_kv::from_payload) reads back — so a
/// queued write and a journal line have ONE spelling between them
/// (ARCH §10.6). A tombstone is `op.value == None`, here and on the wire and
/// in the fold; the `deleted` column `rail_outbox` still carries is written
/// but never read back, because `value IS NULL` already says it — see the
/// schema note in [`crate::backend`].
#[derive(Debug, Clone)]
pub struct Outboxed {
    /// The `rail_outbox` row id, and what [`MeshStore::outbox_ack`] takes.
    pub id: i64,
    /// The namespace this write is scoped to. NOT part of the act: the rail
    /// carries it as the journal's name, never inside the payload.
    pub app_id: String,
    /// The write itself, in the rail's own vocabulary.
    pub op: KvOp,
}

/// What one [`MeshStore::apply_projection`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    /// Rows whose value the store took. A row the store already held at an
    /// equal-or-newer timestamp is not one of these — LWW rejected it, which
    /// is the mechanism working rather than an absence.
    pub merged: usize,
    /// Tombstones that removed a row this node held.
    pub deleted: usize,
    /// Rows dropped because their actor resolves to no node. REPORTED, never
    /// defaulted to some other node's id (ARCH §18.3): a non-zero count here
    /// means the roster and the journal disagree about who is in the ring, and
    /// the caller is the only one who can say whether that is a peer who just
    /// left or a bug.
    pub unattributed: usize,
    /// Rows RETIRED by a sealed actor's live set: this node held them on that
    /// actor's behalf and the actor's own closed snapshot does not name them.
    /// Counted apart from `deleted` because the two are different acts — a
    /// tombstone is an op that says "delete this", and a reconciliation is the
    /// absence of an op in a set an actor has vouched is whole.
    pub reconciled: usize,
    /// Rows this call REMOVED because they are older than the namespace's
    /// declared retention window (`crate::retention`). A third kind of removal
    /// and its own count: a tombstone is an author's decision, a reconciliation
    /// is an author's silence, and an expiry is neither — it is a fact about
    /// `t` that every node in the ring derives identically and nobody publishes.
    pub expired: usize,
    /// Rows the fold offered that this call REFUSED to (re-)insert, for the
    /// same window. Counted apart from `expired` because it is the half that
    /// proves the loop is closed: a non-zero `expired` says the sweep ran, and
    /// a non-zero `withheld` says the journal tried to undo it and could not.
    pub withheld: usize,
}

/// The distributed KV store. Thread-safe; clone freely (backed by `Arc`).
#[derive(Clone)]
pub struct MeshStore {
    backend: Arc<Backend>,
}

impl MeshStore {
    /// Open (or create) the store at `path`. The one door to SQLite, so the
    /// only one behind the `sqlite` feature.
    #[cfg(feature = "sqlite")]
    pub fn open(path: &Path) -> Result<Self> {
        let backend = crate::backend::SqliteBackend::open(path)?;
        Ok(Self {
            backend: Arc::new(Backend::Sqlite(backend)),
        })
    }

    /// Create an in-memory store — the pure-Rust backend, whatever features
    /// are on.
    #[cfg(not(all(test, feature = "sqlite")))]
    pub fn in_memory() -> Result<Self> {
        Ok(Self {
            backend: Arc::new(Backend::Memory(Default::default())),
        })
    }

    /// This crate's OWN tests with `sqlite` on: the same suite over SQLite's
    /// in-memory database, so `cargo test -p commonwealth-state` with and
    /// without the feature runs every test against both backends.
    #[cfg(all(test, feature = "sqlite"))]
    pub fn in_memory() -> Result<Self> {
        use rusqlite::Connection;
        use std::sync::Mutex;
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Backend(format!("in-memory open failed: {e}")))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| Error::Backend(format!("in-memory pragma failed: {e}")))?;
        // The SAME DDL the file store runs. This used to be a second copy,
        // which is how a table added to one and not the other passes every
        // test (ARCH §10.6).
        conn.execute_batch(crate::backend::SCHEMA)
            .map_err(|e| Error::Backend(format!("in-memory init failed: {e}")))?;
        Ok(Self {
            backend: Arc::new(Backend::Sqlite(crate::backend::SqliteBackend {
                conn: Mutex::new(conn),
            })),
        })
    }

    /// Get an entry. Returns `None` if not found.
    pub fn get(&self, app_id: &str, key: &str) -> Result<Option<StoreEntry>> {
        match self.backend.get(app_id, key)? {
            None => Ok(None),
            Some(raw) => {
                let origin = node_id_from_bytes(&raw.origin)?;
                Ok(Some(StoreEntry {
                    app_id: app_id.to_string(),
                    key: key.to_string(),
                    value: Bytes::from(raw.value),
                    timestamp: raw.timestamp,
                    origin,
                }))
            }
        }
    }

    /// Write an entry with the current time as timestamp. Returns true if written (LWW).
    ///
    /// This is a LOCAL write, so it also queues the act for the rail — see
    /// [`MeshStore::outbox_take`]. `merge_entry` deliberately does not: that
    /// is the receive side, and a row learned from a peer that re-entered the
    /// outbox would echo around the mesh forever.
    pub fn set(&self, app_id: &str, key: &str, value: Bytes, origin: NodeId) -> Result<bool> {
        let timestamp = now_secs();
        let origin_bytes = origin.as_bytes().to_vec();
        let written = self.backend.upsert_if_newer_and_enqueue(
            app_id,
            key,
            &value,
            timestamp,
            &origin_bytes,
        )?;
        tracing::debug!(app_id, key, timestamp, written, "mesh_store.set");
        Ok(written)
    }

    /// Delete an entry. Returns true if something was deleted.
    ///
    /// A local delete, so it queues a TOMBSTONE for the rail. A key this node
    /// does not hold queues nothing: it is not a fact about the mesh — the key
    /// may be live on a peer that has simply not reached us yet, and a
    /// tombstone stamped `now` would take it.
    pub fn delete(&self, app_id: &str, key: &str) -> Result<bool> {
        let t = now_secs();
        let deleted = self.backend.delete_and_enqueue(app_id, key, t)?;
        tracing::debug!(app_id, key, t, deleted, "mesh_store.delete");
        Ok(deleted)
    }

    /// Return all entries whose key starts with `prefix` for the given app.
    pub fn scan(&self, app_id: &str, prefix: &str) -> Result<Vec<StoreEntry>> {
        let rows = self.backend.scan_with_prefix(app_id, prefix)?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let origin = node_id_from_bytes(&row.origin)?;
            entries.push(StoreEntry {
                app_id: row.app_id,
                key: row.key,
                value: Bytes::from(row.value),
                timestamp: row.timestamp,
                origin,
            });
        }
        Ok(entries)
    }

    /// Merge one row a peer authored (LWW). Returns true if the entry was
    /// accepted — a row this node already holds at an equal-or-newer timestamp
    /// is rejected, which is the mechanism working rather than a failure.
    ///
    /// The receive half, and deliberately NOT an enqueue: a row that re-entered
    /// the outbox would echo around the mesh forever. Its one production caller
    /// is [`MeshStore::apply_projection`] — until cw-lift rung 2e it was also
    /// `/internal/app/state`'s handler, and that route and its sender are gone.
    pub fn merge_entry(&self, entry: StoreEntry) -> Result<bool> {
        let origin_bytes = entry.origin.as_bytes().to_vec();
        self.backend.upsert_if_newer(
            &entry.app_id,
            &entry.key,
            &entry.value,
            entry.timestamp,
            &origin_bytes,
        )
    }

    // ── The rail: what leaves, and what arrives ──────────────

    /// Take up to `limit` queued writes for the pump, oldest first.
    ///
    /// Rows stay queued until [`MeshStore::outbox_ack`], so a crash between
    /// the append and the ack re-sends rather than loses — the rail's op id is
    /// content-derived, so a duplicate append is the same op.
    pub fn outbox_take(&self, limit: usize) -> Result<Vec<Outboxed>> {
        let rows = self.backend.outbox_take(limit)?;
        Ok(rows
            .into_iter()
            .map(|r| Outboxed {
                id: r.id,
                app_id: r.app_id,
                op: KvOp {
                    key: r.key,
                    value: r.value.map(Bytes::from),
                    t: r.t,
                },
            })
            .collect())
    }

    /// Drop queued writes the pump has put on the rail. Returns how many rows
    /// went away.
    pub fn outbox_ack(&self, ids: &[i64]) -> Result<usize> {
        let removed = self.backend.outbox_ack(ids)?;
        tracing::debug!(asked = ids.len(), removed, "mesh_store.outbox_ack");
        Ok(removed)
    }

    /// How many writes are waiting for the pump.
    pub fn outbox_len(&self) -> Result<usize> {
        self.backend.outbox_len()
    }

    /// Apply one namespace's projection — what the fold of the ring journal
    /// says this node should hold.
    ///
    /// `origin_of` resolves a rail actor (a signing public key) to the
    /// [`NodeId`] a `StoreEntry` records. It is a parameter rather than
    /// something this crate reads, because the roster that answers it lives in
    /// the mesh and this crate has no mesh.
    ///
    /// Three things it refuses or reports rather than guesses (ARCH §18.3):
    ///
    /// - **An excluded `app_id` is refused outright** — the RECEIVER-side
    ///   privacy guard. A peer that puts a private namespace on the ring gets
    ///   it projected NOWHERE, and the refusal is an `Err` naming the
    ///   namespace, not a quiet no-op that reads as "nothing to do".
    /// - **An actor with no `NodeId` is counted `unattributed` and skipped.**
    ///   `StoreEntry.origin` has to name a node, and inventing one — a zero
    ///   id, our own — would attribute a peer's write to somebody who did not
    ///   make it.
    /// - **A tombstone deletes only what is not newer than it.** A delete at
    ///   `t` must not take a `set` at `t+1` that arrived first.
    ///
    /// # The seal reconciliation
    ///
    /// A seal retires everything below it and the snapshot re-appends only the
    /// LIVE rows, so a tombstone stops travelling the moment its seal lands —
    /// and a peer that never received it would keep the stale value forever.
    /// That is closed here rather than remembered (ARCH §7.1). For every actor
    /// [`rail_kv::project`](crate::rail_kv::project) reports in
    /// [`Projection::sealed_actors`](crate::rail_kv::Projection::sealed_actors)
    /// — those whose seal AND whole snapshot this node holds — the rows this
    /// store holds on that actor's behalf are reconciled to the set the actor
    /// asserts: a row whose origin is that actor and whose key the actor does
    /// not name is RETIRED. An actor with no entry is untouched, because it has
    /// made no claim about its whole set.
    ///
    /// **The projection is taken whole, not as its rows.** The live sets and
    /// the rows are two readings of one fold, and a caller that could pass a
    /// fresh set of rows with a stale live set would be able to retire a key on
    /// the strength of a claim made about a different journal (ARCH §10.6).
    ///
    /// # The retention floor
    ///
    /// A namespace may declare a window in [`crate::retention`], and this is
    /// the one place it can be enforced. The store is a projection: a row a
    /// sweep deleted has no incumbent, so `merge_entry` re-inserts it from the
    /// journal on the very next round, and `RetentionGc`'s thirty days on the
    /// contributions ledger were undone within a minute forever
    /// (`ring_sync::tests::a_retention_sweep_is_not_undone_by_the_next_projection`).
    /// So retention is part of the fold's decision, from the SAME table the
    /// sweep reads — two cutoffs would spend every round undoing each other
    /// (ARCH §10.6).
    ///
    /// Both directions, because either alone leaves a hole: a projected row
    /// below the floor is not merged (`withheld`), and a held row below it is
    /// retired (`expired`) whether it aged in place or a peer's fold put it
    /// there. The sweep is therefore a byproduct of the round on any node that
    /// projects, and `RetentionGc` remains the bound on a node that does not —
    /// `run_one_round` returns before projecting anything when the mesh has no
    /// online peer.
    ///
    /// **An expiry is not `self_id`-exempt, and that is not the same hazard the
    /// reconciliation has.** Reconciling self reads our own outbox lag as
    /// another actor's retirement; the floor reads nothing of anyone's — it is
    /// `now` minus a constant, compared against a `t` this node stamped. A row
    /// of ours old enough to expire while still queued has been queued for the
    /// whole window, and the queued act still travels: peers withhold it on
    /// arrival by the same arithmetic.
    ///
    /// **`self_id` is skipped, and that is the direction of truth rather than a
    /// special case.** For every other actor the journal is upstream of this
    /// store: what we hold on their behalf is a fold of what they signed. For
    /// THIS node it is the other way round — `set` writes the row and queues
    /// the act, so the store leads the journal by an outbox drain, and a write
    /// still queued (or one the rail refused) is a row this node asserts and no
    /// journal knows about yet. Reconciling self would read our own lag as a
    /// retirement. Excluding the outbox instead would patch that lag with a
    /// second source and still lose the refused row.
    pub fn apply_projection(
        &self,
        app_id: &str,
        projection: &crate::rail_kv::Projection,
        origin_of: impl Fn(&str) -> Option<NodeId>,
        self_id: NodeId,
    ) -> Result<Applied> {
        let rows = &projection.rows;
        if crate::peer_preferences::is_gossip_excluded(app_id) {
            tracing::debug!(app_id, "mesh_store.projection_refused_excluded");
            return Err(Error::Backend(format!(
                "'{app_id}' never leaves a machine, so nothing a peer sent may be \
                 projected into it"
            )));
        }
        let mut applied = Applied::default();
        // Read once for the whole call: every row is judged against ONE floor,
        // so a fold cannot keep a row and retire its neighbour because the
        // clock ticked between them.
        let floor = crate::retention::floor_now(app_id);
        for row in rows {
            if floor.is_some_and(|f| row.t < f) {
                // Before the origin lookup on purpose. An expired row needs no
                // author: attributing it would only file it under
                // `unattributed` when the roster cannot place it, which reads
                // as a roster/journal disagreement and is not one. A TOMBSTONE
                // below the floor is withheld too, and loses nothing — every
                // row it could delete is at or below its own `t` and so is
                // expired by the same floor.
                applied.withheld += 1;
                tracing::debug!(
                    app_id,
                    key = %row.key,
                    t = row.t,
                    floor,
                    "mesh_store.projection_withheld_expired"
                );
                continue;
            }
            let Some(origin) = origin_of(&row.actor) else {
                applied.unattributed += 1;
                tracing::debug!(
                    app_id,
                    key = %row.key,
                    actor = %row.actor,
                    "mesh_store.projection_unattributed"
                );
                continue;
            };
            self.apply_projected_row(app_id, row, origin, &mut applied)?;
        }
        // ── What the seals say is no longer asserted.
        //
        // AFTER the rows, so a key a tombstone in this same projection already
        // took is not counted twice, and so the origins matched below are the
        // ones this projection just wrote.
        for (actor, live) in &projection.sealed_actors {
            let Some(origin) = origin_of(actor) else {
                // Already counted `unattributed` above for any row this actor
                // won; a live set for a node we cannot place matches no row's
                // origin either, so there is nothing to reconcile against.
                continue;
            };
            if origin == self_id {
                tracing::debug!(
                    app_id,
                    actor = %actor,
                    "mesh_store.projection_reconcile_skipped_self"
                );
                continue;
            }
            let held = self.backend.keys_with_origin(app_id, origin.as_bytes())?;
            for key in held {
                if live.contains(&key) {
                    continue;
                }
                if self
                    .backend
                    .delete_of_origin(app_id, &key, origin.as_bytes())?
                {
                    applied.reconciled += 1;
                    tracing::debug!(
                        app_id,
                        key = %key,
                        actor = %actor,
                        live_keys = live.len(),
                        "mesh_store.projection_reconciled"
                    );
                }
            }
        }

        // ── What the retention window no longer holds.
        //
        // LAST, so a row this round merged is judged on the same floor as one
        // that was already here — the fold cannot leave a row above the floor
        // in the store and the sweep cannot take one it just let in.
        if let Some(f) = floor {
            applied.expired = self.backend.delete_older_than_in_app(app_id, f)?;
            if applied.expired > 0 {
                tracing::debug!(
                    app_id,
                    floor = f,
                    expired = applied.expired,
                    "mesh_store.projection_expired"
                );
            }
        }

        tracing::debug!(
            app_id,
            rows = rows.len(),
            sealed_actors = projection.sealed_actors.len(),
            merged = applied.merged,
            deleted = applied.deleted,
            reconciled = applied.reconciled,
            expired = applied.expired,
            withheld = applied.withheld,
            unattributed = applied.unattributed,
            "mesh_store.projection_applied"
        );
        Ok(applied)
    }

    /// Rebuild a LOCAL-ONLY namespace from this node's own journal of it — the
    /// self-actor rehydrate door (five-programs fp-108).
    ///
    /// [`MeshStore::apply_projection`] refuses these namespaces because what it
    /// folds is what peers signed. Here the journal is this node's own record
    /// of its own writes, so only rows signed by `self_actor` are merged, with
    /// origin `self_id`. Any other actor's row is counted in `unattributed` and
    /// skipped — a local-only journal is never offered, so a foreign row in one
    /// is not a write this node may take on anyone's word. Any namespace that
    /// is not local-only is refused with a named `Err`: those belong to
    /// `apply_projection`.
    ///
    /// No reconciliation: `apply_projection` skips self there too (the store
    /// leads its own journal), and self is the only actor this door merges.
    pub fn apply_own_projection(
        &self,
        app_id: &str,
        projection: &crate::rail_kv::Projection,
        self_actor: &str,
        self_id: NodeId,
    ) -> Result<Applied> {
        if !commonwealth_rail_core::is_local_only(app_id) {
            tracing::debug!(app_id, "mesh_store.own_projection_refused_not_local_only");
            return Err(Error::Backend(format!(
                "'{app_id}' is not a local-only namespace, so its journal is not this \
                 node's alone and cannot be rehydrated through the own-journal door"
            )));
        }
        let mut applied = Applied::default();
        let floor = crate::retention::floor_now(app_id);
        for row in &projection.rows {
            if row.actor != self_actor {
                applied.unattributed += 1;
                tracing::debug!(
                    app_id,
                    key = %row.key,
                    actor = %row.actor,
                    "mesh_store.own_projection_skipped_foreign_actor"
                );
                continue;
            }
            if floor.is_some_and(|f| row.t < f) {
                applied.withheld += 1;
                tracing::debug!(app_id, key = %row.key, t = row.t, floor, "mesh_store.own_projection_withheld_expired");
                continue;
            }
            self.apply_projected_row(app_id, row, self_id, &mut applied)?;
        }
        tracing::debug!(
            app_id,
            rows = projection.rows.len(),
            merged = applied.merged,
            deleted = applied.deleted,
            withheld = applied.withheld,
            skipped_foreign = applied.unattributed,
            "mesh_store.own_projection_applied"
        );
        Ok(applied)
    }

    /// One attributed row into the store: a value merges under LWW, a
    /// tombstone deletes what is not newer. Both projection doors fold rows
    /// through here; each decides the row's `origin` and its gates first.
    fn apply_projected_row(
        &self,
        app_id: &str,
        row: &crate::rail_kv::Projected,
        origin: NodeId,
        applied: &mut Applied,
    ) -> Result<()> {
        match &row.value {
            Some(value) => {
                let accepted = self.merge_entry(StoreEntry {
                    app_id: app_id.to_string(),
                    key: row.key.clone(),
                    value: value.clone(),
                    timestamp: row.t,
                    origin,
                })?;
                if accepted {
                    applied.merged += 1;
                }
                tracing::debug!(
                    app_id,
                    key = %row.key,
                    t = row.t,
                    accepted,
                    "mesh_store.projection_merge"
                );
            }
            None => {
                let removed = self.backend.delete_if_not_newer(app_id, &row.key, row.t)?;
                if removed {
                    applied.deleted += 1;
                }
                tracing::debug!(
                    app_id,
                    key = %row.key,
                    t = row.t,
                    removed,
                    "mesh_store.projection_tombstone"
                );
            }
        }
        Ok(())
    }

    /// Delete entries older than `ttl_seconds`. Returns count deleted.
    ///
    /// UNSCOPED — every app in the store is subject to the same cutoff.
    /// Correct only where every app's entries are refreshed or dead;
    /// prefer [`MeshStore::gc_app`] when you mean to bound one
    /// namespace.
    ///
    /// **On a rail-backed namespace the cutoff is not yours to choose.** These
    /// four are a sweep of a PROJECTION, and a row deleted at a cutoff the fold
    /// does not share is re-inserted on the next round. Take the cutoff from
    /// [`crate::retention`], which is what [`crate::RetentionGc`] does and what
    /// [`MeshStore::apply_projection`] reads.
    pub fn gc(&self, ttl_seconds: u64) -> Result<usize> {
        self.gc_before(now_secs().saturating_sub(ttl_seconds))
    }

    /// `gc` with the cutoff supplied rather than read from the clock.
    ///
    /// The TTL forms above read `now_secs()` INSIDE the call, so a caller that
    /// planted a row relative to its own earlier `now_secs()` is racing the
    /// wall clock: one tick between the two reads moves the cutoff a second
    /// later and takes a row the caller placed exactly on the boundary. That
    /// is not hypothetical — it is the flake this seam exists to remove, and
    /// it was found by a package lift rather than by the suite (2026-09-04).
    /// Anything asserting where the boundary IS must use this form, so the
    /// cutoff is one value both sides agree on (§10.6).
    pub fn gc_before(&self, cutoff: u64) -> Result<usize> {
        self.backend.delete_older_than(cutoff)
    }

    /// Delete entries older than `ttl_seconds` within a single
    /// `app_id`. Returns count deleted.
    pub fn gc_app(&self, app_id: &str, ttl_seconds: u64) -> Result<usize> {
        self.gc_app_before(app_id, now_secs().saturating_sub(ttl_seconds))
    }

    /// `gc_app` with the cutoff supplied rather than read from the clock.
    /// See [`MeshStore::gc_before`] for why a boundary assertion needs it.
    pub fn gc_app_before(&self, app_id: &str, cutoff: u64) -> Result<usize> {
        self.backend.delete_older_than_in_app(app_id, cutoff)
    }
}

use commonwealth_core::clock::unix_now_secs as now_secs;

fn node_id_from_bytes(bytes: &[u8]) -> Result<NodeId> {
    if bytes.len() != 16 {
        return Err(Error::NodeId(format!(
            "expected 16 bytes for NodeId, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(bytes);
    // Writers persist `origin.as_bytes().to_vec()` — a verbatim copy of
    // NodeId's internal byte array. NodeId stores its u128 big-endian
    // (`from_u128` calls `to_be_bytes`), so reading must invert with
    // `from_be_bytes`. Using `from_le_bytes` here silently reversed the
    // round-trip, leaving every `StoreEntry.origin` mis-identified.
    Ok(NodeId::from_u128(u128::from_be_bytes(arr)))
}

#[cfg(test)]
#[path = "store/tests.rs"]
mod tests;

// The work atlas's namespaces through the outbox and the projection, over the
// contract constants both sides name (pb-mesh-dissolve, phase-b-92).
#[cfg(test)]
#[path = "store/work_atlas_tests.rs"]
mod work_atlas_tests;
