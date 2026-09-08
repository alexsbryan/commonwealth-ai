// SPDX-License-Identifier: AGPL-3.0-or-later
//! MeshStore — the distributed key-value store for mesh apps.
//!
//! Each entry is scoped to an `app_id` + `key`. Conflict resolution is LWW
//! (last-write-wins) using a Unix-second `timestamp`. The underlying storage
//! is SQLite (WAL mode) via `SqliteBackend`.
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

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;

use commonwealth_core::ids::NodeId;

use crate::backend::SqliteBackend;
use crate::error::{Error, Result};

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

/// One local write waiting to go onto the ring journal.
///
/// `deleted` and `value: None` say the same thing, and both are here because
/// the column is what the pump filters on and the `Option` is what it hands
/// [`rail_kv::to_payload`](crate::rail_kv::to_payload). They cannot disagree —
/// `an_outbox_rows_two_spellings_of_a_tombstone_agree` is the pin.
#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id: i64,
    pub app_id: String,
    pub key: String,
    /// `None` is a tombstone.
    pub value: Option<Bytes>,
    /// Unix seconds — the ORIGINAL write time, and what the fold orders by.
    pub t: u64,
    pub deleted: bool,
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
    backend: Arc<SqliteBackend>,
}

impl MeshStore {
    /// Open (or create) the store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let backend = SqliteBackend::open(path)?;
        Ok(Self {
            backend: Arc::new(backend),
        })
    }

    /// Create an in-memory store (useful for tests).
    pub fn in_memory() -> Result<Self> {
        use rusqlite::Connection;
        use std::sync::Mutex;
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Backend(format!("in-memory open failed: {e}")))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| Error::Backend(format!("in-memory pragma failed: {e}")))?;
        // The SAME DDL the file store runs. This used to be a second copy,
        // which is how a table added to one and not the other passes every
        // test (ARCH §10.6) — and in production this IS the store, so the
        // copy that mattered was this one.
        conn.execute_batch(crate::backend::SCHEMA)
            .map_err(|e| Error::Backend(format!("in-memory init failed: {e}")))?;
        Ok(Self {
            backend: Arc::new(crate::backend::SqliteBackend {
                conn: Mutex::new(conn),
            }),
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

    /// Append `value` to an existing entry or create it. Values are newline-joined.
    ///
    /// Composed of `get` + `set`, so the rail carries the WHOLE combined value
    /// as one act rather than a delta. That is the right shape here: the fold
    /// is last-write-wins over whole values, and a delta would need every
    /// prior act to have arrived before it could mean anything.
    pub fn append(&self, app_id: &str, key: &str, value: Bytes, origin: NodeId) -> Result<()> {
        let existing = self.get(app_id, key)?;
        let new_value = match existing {
            Some(e) => {
                let mut combined = e.value.to_vec();
                combined.push(b'\n');
                combined.extend_from_slice(&value);
                Bytes::from(combined)
            }
            None => value,
        };
        self.set(app_id, key, new_value, origin)?;
        Ok(())
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

    /// List all keys for an app.
    pub fn list_keys(&self, app_id: &str) -> Result<Vec<String>> {
        self.backend.list_keys(app_id)
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
    pub fn outbox_take(&self, limit: usize) -> Result<Vec<OutboxRow>> {
        let rows = self.backend.outbox_take(limit)?;
        Ok(rows
            .into_iter()
            .map(|r| OutboxRow {
                id: r.id,
                app_id: r.app_id,
                key: r.key,
                value: r.value.map(Bytes::from),
                t: r.t,
                deleted: r.deleted,
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
mod tests {
    use super::*;
    use commonwealth_core::ids::NodeId;

    fn node(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    /// covers: FE-119
    ///
    /// Neither `gc` nor `gc_app` had any test. Both are silent by
    /// construction — they return a count nobody checks and delete rows
    /// nobody reads afterwards — so both failure directions ship quietly:
    /// a gc that deletes nothing lets a replicated store grow without
    /// bound, and a `gc_app` that ignores its `app_id` reaches across
    /// namespaces and takes another app's live state with it.
    ///
    /// Timestamps are planted through `merge_entry` rather than `set`,
    /// because `set` stamps `now_secs()` and a TTL test that has to sleep
    /// is a test that will be deleted the first time it flakes.
    #[test]
    fn gc_is_bounded_by_both_the_cutoff_and_the_namespace() {
        let store = MeshStore::in_memory().unwrap();
        let now = now_secs();
        let plant = |app: &str, key: &str, age: u64| {
            store
                .merge_entry(StoreEntry {
                    app_id: app.to_string(),
                    key: key.to_string(),
                    value: Bytes::from("v"),
                    timestamp: now - age,
                    origin: node(1),
                })
                .unwrap()
        };
        const TTL: u64 = 100;

        // Two namespaces. `chat` holds one stale and one live entry;
        // `presence` holds a stale entry that `gc_app("chat", ..)` must not
        // touch.
        plant("chat", "old", 500);
        plant("chat", "fresh", 5);
        plant("presence", "old", 500);
        // Exactly ON the cutoff. `delete_older_than` is `timestamp <
        // cutoff`, so this entry SURVIVES — the boundary is stated here so
        // an off-by-one in either direction is a failure rather than a
        // silent change in how much history a TTL keeps.
        plant("chat", "at-cutoff", TTL);

        // ONE cutoff, derived from the same `now` the rows were planted
        // against. The TTL forms re-read the clock, so a tick between plant
        // and sweep silently moves the boundary this test exists to pin.
        let cutoff = now - TTL;
        let deleted = store.gc_app_before("chat", cutoff).unwrap();
        assert_eq!(deleted, 1, "only `chat`'s stale entry is old enough");
        assert!(store.get("chat", "old").unwrap().is_none());
        assert!(store.get("chat", "fresh").unwrap().is_some());
        assert!(
            store.get("chat", "at-cutoff").unwrap().is_some(),
            "an entry exactly at the cutoff is not yet older than the TTL"
        );
        assert!(
            store.get("presence", "old").unwrap().is_some(),
            "gc_app must not reach outside its namespace"
        );

        // The unscoped sweep spans every app, and reports what it took.
        let deleted = store.gc_before(cutoff).unwrap();
        assert_eq!(deleted, 1, "the remaining stale entry, in the other app");
        assert!(store.get("presence", "old").unwrap().is_none());
        assert!(store.get("chat", "fresh").unwrap().is_some());

        // A sweep with nothing to take reports zero rather than erroring —
        // the caller is a periodic task and a spurious Err would be logged
        // forever.
        assert_eq!(store.gc_before(cutoff).unwrap(), 0);
    }

    #[test]
    fn set_and_get_roundtrip() {
        let store = MeshStore::in_memory().unwrap();
        store
            .set("myapp", "greeting", Bytes::from("hello"), node(1))
            .unwrap();
        let entry = store.get("myapp", "greeting").unwrap().unwrap();
        assert_eq!(entry.value.as_ref(), b"hello");
        assert_eq!(entry.app_id, "myapp");
        assert_eq!(entry.key, "greeting");
        assert_eq!(entry.origin, node(1));
    }

    #[test]
    fn origin_round_trips_for_realistic_node_id() {
        // Regression test: real NodeIds (random 16 bytes, not low-int test
        // fixtures) used to silently byte-reverse on read because writes
        // were verbatim but reads went through `u128::from_le_bytes`. The
        // `node(1)` fixtures don't catch this — `0...01` looks identical
        // reversed at the display layer because Display only shows the
        // first 8 bytes — so we use a value whose low and high halves
        // differ.
        let store = MeshStore::in_memory().unwrap();
        let id = NodeId::from_u128(0x1122_3344_5566_7788_AABB_CCDD_EEFF_0011);
        store.set("a", "k", Bytes::from("v"), id).unwrap();
        let entry = store.get("a", "k").unwrap().unwrap();
        assert_eq!(entry.origin, id);
        let scanned = store.scan("a", "").unwrap();
        assert_eq!(scanned[0].origin, id);
    }

    #[test]
    fn get_missing_returns_none() {
        let store = MeshStore::in_memory().unwrap();
        assert!(store.get("myapp", "nope").unwrap().is_none());
    }

    #[test]
    fn merge_entry_lww() {
        let store = MeshStore::in_memory().unwrap();

        let old = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("old"),
            timestamp: 100,
            origin: node(1),
        };
        let new = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("new"),
            timestamp: 200,
            origin: node(2),
        };

        assert!(store.merge_entry(old).unwrap());
        // Older entry should be rejected.
        let stale = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("stale"),
            timestamp: 50,
            origin: node(3),
        };
        assert!(!store.merge_entry(stale).unwrap());
        // Newer entry should be accepted.
        assert!(store.merge_entry(new).unwrap());

        let e = store.get("a", "k").unwrap().unwrap();
        assert_eq!(e.value.as_ref(), b"new");
        assert_eq!(e.timestamp, 200);
    }

    /// **LWW tie-break determinism (clock-skew vector).** Consumer
    /// hardware clocks skew, so two nodes can stamp the same key with
    /// the *same* second. `upsert_if_newer` accepts only `ts > existing`
    /// (a later write at an equal timestamp is rejected), so locally
    /// the INCUMBENT wins a tie — deterministic, no last-arrival thrash.
    ///
    /// KNOWN LIMITATION pinned here on purpose: this makes ties
    /// *node-local* deterministic but NOT cross-node convergent. If A
    /// holds X@100 and B holds Y@100 for the same key (equal stamp,
    /// different origins), each rejects the other's value on merge and
    /// they stay diverged — origin is not a tiebreaker. Acceptable
    /// today because every gossiped namespace keys by origin/content
    /// (so two nodes don't co-write one key at one second); if a future
    /// shared-key namespace appears, add a deterministic tiebreaker
    /// (e.g. higher origin NodeId wins) rather than relying on this.
    #[test]
    fn merge_entry_equal_timestamp_keeps_incumbent() {
        let store = MeshStore::in_memory().unwrap();
        let first = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("incumbent"),
            timestamp: 100,
            origin: node(1),
        };
        assert!(store.merge_entry(first).unwrap());

        // Same timestamp, different value+origin — must be rejected,
        // deterministically, regardless of arrival order.
        let tie = StoreEntry {
            app_id: "a".into(),
            key: "k".into(),
            value: Bytes::from("challenger"),
            timestamp: 100,
            origin: node(2),
        };
        assert!(
            !store.merge_entry(tie).unwrap(),
            "equal-timestamp write must not displace the incumbent"
        );
        assert_eq!(
            store.get("a", "k").unwrap().unwrap().value.as_ref(),
            b"incumbent"
        );
    }

    #[test]
    fn delete_removes_entry() {
        let store = MeshStore::in_memory().unwrap();
        store.set("a", "k", Bytes::from("v"), node(1)).unwrap();
        assert!(store.delete("a", "k").unwrap());
        assert!(store.get("a", "k").unwrap().is_none());
    }

    #[test]
    fn list_keys_scoped_to_app() {
        let store = MeshStore::in_memory().unwrap();
        store.set("app1", "k1", Bytes::from("v"), node(1)).unwrap();
        store.set("app1", "k2", Bytes::from("v"), node(1)).unwrap();
        store.set("app2", "k3", Bytes::from("v"), node(1)).unwrap();

        let keys = store.list_keys("app1").unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"k1".to_string()));
        assert!(keys.contains(&"k2".to_string()));
    }

    #[test]
    fn scan_filters_by_prefix() {
        let store = MeshStore::in_memory().unwrap();
        store
            .set("inf", "model:abc", Bytes::from("a"), node(1))
            .unwrap();
        store
            .set("inf", "model:def", Bytes::from("b"), node(1))
            .unwrap();
        store
            .set("inf", "ledger:xyz", Bytes::from("c"), node(1))
            .unwrap();
        store
            .set("other", "model:abc", Bytes::from("d"), node(1))
            .unwrap();

        let model_entries = store.scan("inf", "model:").unwrap();
        assert_eq!(model_entries.len(), 2);
        assert!(model_entries.iter().all(|e| e.key.starts_with("model:")));

        let ledger_entries = store.scan("inf", "ledger:").unwrap();
        assert_eq!(ledger_entries.len(), 1);
        assert_eq!(ledger_entries[0].key, "ledger:xyz");

        // Prefix not present for app returns empty.
        let empty = store.scan("inf", "nope:").unwrap();
        assert!(empty.is_empty());

        // Scan is scoped to app_id.
        let scoped = store.scan("other", "model:").unwrap();
        assert_eq!(scoped.len(), 1);
    }

    // ── The outbox: what this node wrote, for the rail ──────

    /// **THE SENDER-SIDE PRIVACY GUARD.** The outbox is now the only thing
    /// that leaves this machine, so it is the chokepoint
    /// `all_entries_for_gossip` used to be. An excluded namespace must never
    /// appear in it — not filtered later, not filtered by the pump: absent.
    ///
    /// The named failing input is any of the writes below reaching the queue.
    /// Watched red by deleting the `is_gossip_excluded` guard in
    /// `backend::enqueue_on`: eight rows queued instead of one.
    #[test]
    fn an_excluded_namespace_never_enters_the_outbox() {
        use crate::{ACTIVITY_APP_ID, CONTRIBUTIONS_APP_ID, GOSSIP_EXCLUDED_APP_IDS};

        let store = MeshStore::in_memory().unwrap();
        // Every excluded namespace there is, written through the ordinary
        // door. Driving the LIST rather than a hand-picked few means a
        // namespace added to it later is covered without editing this test.
        for app in GOSSIP_EXCLUDED_APP_IDS {
            store
                .set(app, "k", Bytes::from("private"), node(1))
                .unwrap();
        }
        store
            .set(CONTRIBUTIONS_APP_ID, "ev1", Bytes::from("public"), node(1))
            .unwrap();

        let queued = store.outbox_take(100).unwrap();
        assert_eq!(
            queued.len(),
            1,
            "only the public write is queued: {queued:?}"
        );
        assert_eq!(queued[0].app_id, CONTRIBUTIONS_APP_ID);

        // Excluded is not the same as unwritten — the rows are all here.
        for app in GOSSIP_EXCLUDED_APP_IDS {
            assert!(store.get(app, "k").unwrap().is_some(), "{app} lost its row");
        }

        // A DELETE in an excluded namespace queues no tombstone either. A
        // tombstone names a key, and the key is the private half.
        assert!(store.delete(ACTIVITY_APP_ID, "k").unwrap());
        assert_eq!(store.outbox_take(100).unwrap().len(), 1);
    }

    /// A LOCAL write queues an act; a write learned from a peer does not.
    /// `merge_entry` is the receive side, and a row that re-entered the outbox
    /// would be republished by every node that saw it, forever.
    #[test]
    fn a_local_write_queues_an_act_and_a_received_one_does_not() {
        let store = MeshStore::in_memory().unwrap();
        store.set("app", "a", Bytes::from("1"), node(1)).unwrap();
        assert!(store.delete("app", "a").unwrap());
        assert_eq!(store.outbox_len().unwrap(), 2, "set + delete");

        store.outbox_ack(&[1, 2]).unwrap();
        store
            .merge_entry(StoreEntry {
                app_id: "app".into(),
                key: "fromapeer".into(),
                value: Bytes::from("v"),
                timestamp: 9_000,
                origin: node(2),
            })
            .unwrap();
        assert_eq!(
            store.outbox_len().unwrap(),
            0,
            "a row learned from a peer must not be re-published"
        );

        // A write LWW rejected queues nothing either — there is no act.
        assert!(!store
            .merge_entry(StoreEntry {
                app_id: "app".into(),
                key: "fromapeer".into(),
                value: Bytes::from("older"),
                timestamp: 1,
                origin: node(2),
            })
            .unwrap());
        assert!(!store.delete("app", "never-existed").unwrap());
        assert_eq!(store.outbox_len().unwrap(), 0);
    }

    /// **`append` LOSES a write inside one second, and has since it was
    /// written.** This is a pin on a DEFECT, not a contract — read the name.
    ///
    /// It composes as `get` + `set`, `set` stamps `now_secs()`, and
    /// `upsert_if_newer` refuses an equal timestamp (deliberately: that is how
    /// a tie keeps the incumbent). So a second `append` in the same wall-clock
    /// second writes nothing, and `append` throws away the `bool` that would
    /// have said so — an unsuccessful write in a success-shaped return
    /// (ARCH §18.3).
    ///
    /// It ships unfixed here because `MeshStore::append` has NO production
    /// caller — the ledgers spell append-only in the KEY instead
    /// (`ContributionEmitter`, `ActivityEmitter`) — so a fix would be a
    /// second timestamp rule minted for nobody (ARCH §10.6). Whoever gives it
    /// a caller owns the fix, and this test is what turns red when they do.
    ///
    /// The rail half is right either way: when the write DOES land it queues
    /// the whole combined value rather than a delta, because the fold is
    /// last-write-wins over whole values.
    #[test]
    /// A same-second `append` used to be LOST: `set` stamps `now_secs()`, the
    /// tie rule refused an equal timestamp, and `append` threw the bool away.
    /// The store now lets one origin rewrite its own key inside a second, so
    /// the appended value lands and the WHOLE combined value is queued.
    #[test]
    fn append_within_one_second_lands_and_queues_the_whole_value() {
        let store = MeshStore::in_memory().unwrap();
        store
            .set("app", "log", Bytes::from("one"), node(1))
            .unwrap();
        store
            .append("app", "log", Bytes::from("two"), node(1))
            .unwrap();
        assert_eq!(
            store.get("app", "log").unwrap().unwrap().value.as_ref(),
            b"one\ntwo"
        );
        assert_eq!(store.outbox_len().unwrap(), 2, "both writes are queued");
    }

    /// The tie rule, both shapes: the SAME origin rewriting a key in the same
    /// second wins (program order); a DIFFERENT origin at the same timestamp
    /// does not (the incumbent keeps, deterministically — see
    /// `merge_entry_equal_timestamp_keeps_incumbent`).
    #[test]
    fn a_same_second_rewrite_by_the_same_origin_wins() {
        let store = MeshStore::in_memory().unwrap();
        assert!(store
            .set("app", "k", Bytes::from("first"), node(1))
            .unwrap());
        assert!(
            store
                .set("app", "k", Bytes::from("second"), node(1))
                .unwrap(),
            "the daemon's own second write in one second was being dropped"
        );
        assert_eq!(
            store.get("app", "k").unwrap().unwrap().value.as_ref(),
            b"second"
        );
        let queued = store.outbox_len().unwrap();
        assert!(
            !store
                .set("app", "k", Bytes::from("second"), node(1))
                .unwrap(),
            "the same bytes again is not a write"
        );
        assert_eq!(store.outbox_len().unwrap(), queued, "and queues nothing");
        let ts = store.get("app", "k").unwrap().unwrap().timestamp;
        let rival = StoreEntry {
            app_id: "app".into(),
            key: "k".into(),
            value: Bytes::from("rival"),
            timestamp: ts,
            origin: node(2),
        };
        assert!(
            !store.merge_entry(rival).unwrap(),
            "another origin at the same second is refused"
        );
        assert_eq!(
            store.get("app", "k").unwrap().unwrap().value.as_ref(),
            b"second"
        );
    }

    /// The two spellings of "this is a tombstone" cannot disagree. They are
    /// both here because the column is what a query filters on and the
    /// `Option` is what the pump hands `rail_kv::to_payload`.
    #[test]
    fn an_outbox_rows_two_spellings_of_a_tombstone_agree() {
        let store = MeshStore::in_memory().unwrap();
        store.set("app", "a", Bytes::from(""), node(1)).unwrap();
        assert!(store.delete("app", "a").unwrap());
        for row in store.outbox_take(100).unwrap() {
            assert_eq!(
                row.deleted,
                row.value.is_none(),
                "row {row:?} says two different things"
            );
        }
    }

    /// Take is non-destructive and ack is what removes: the pump appends
    /// first and acks after, so a crash between them re-sends rather than
    /// loses. The rail's op id is content-derived, so a duplicate append is
    /// the same op.
    #[test]
    fn outbox_take_leaves_the_rows_and_ack_removes_them() {
        let store = MeshStore::in_memory().unwrap();
        for i in 0..5 {
            store
                .set("app", &format!("k{i}"), Bytes::from("v"), node(1))
                .unwrap();
        }
        let first = store.outbox_take(2).unwrap();
        assert_eq!(first.len(), 2, "the limit is honoured");
        assert_eq!(
            store.outbox_take(2).unwrap().len(),
            2,
            "taking must not consume"
        );

        let ids: Vec<i64> = first.iter().map(|r| r.id).collect();
        assert_eq!(store.outbox_ack(&ids).unwrap(), 2);
        assert_eq!(store.outbox_len().unwrap(), 3);
        // Acking the same ids again reports zero rather than erroring — the
        // caller is a loop and a spurious Err would be logged forever.
        assert_eq!(store.outbox_ack(&ids).unwrap(), 0);
        assert_eq!(store.outbox_ack(&[]).unwrap(), 0);
    }

    // ── The projection: what the ring says we hold ──────────

    fn projected(
        key: &str,
        value: Option<&[u8]>,
        t: u64,
        actor: &str,
    ) -> crate::rail_kv::Projected {
        crate::rail_kv::Projected {
            key: key.to_string(),
            value: value.map(Bytes::copy_from_slice),
            t,
            actor: actor.to_string(),
        }
    }

    /// A projection of `rows` and NO claim about anyone's whole live set —
    /// what the fold returns for a namespace nobody has sealed, which is every
    /// namespace until one does.
    fn unsealed(rows: Vec<crate::rail_kv::Projected>) -> crate::rail_kv::Projection {
        crate::rail_kv::Projection {
            rows,
            ..Default::default()
        }
    }

    /// The projection a sealed actor produces: its rows, and the live set it
    /// vouches is whole.
    fn sealed(
        rows: Vec<crate::rail_kv::Projected>,
        actor: &str,
        live: &[&str],
    ) -> crate::rail_kv::Projection {
        let mut p = unsealed(rows);
        p.sealed_actors.insert(
            actor.to_string(),
            live.iter().map(|k| k.to_string()).collect(),
        );
        p
    }

    /// This node, wherever a test needs one. Deliberately not any `node(n)` an
    /// actor resolves to, so nothing is skipped as self by accident.
    fn me() -> NodeId {
        node(99)
    }

    /// **THE RECEIVER-SIDE PRIVACY GUARD.** A peer that puts a private
    /// namespace on the ring gets it projected NOWHERE, and the refusal is an
    /// `Err` naming the namespace rather than a quiet `Ok(0 rows)` that reads
    /// as "there was nothing to do" (ARCH §18.3). This is the half the old
    /// `routes_app_internal` inbound check did.
    #[test]
    fn apply_projection_refuses_an_excluded_namespace() {
        use crate::GOSSIP_EXCLUDED_APP_IDS;

        let store = MeshStore::in_memory().unwrap();
        let rows = unsealed(vec![projected("k", Some(b"leaked"), 100, "aa")]);
        for app in GOSSIP_EXCLUDED_APP_IDS {
            let err = store
                .apply_projection(app, &rows, |_| Some(node(1)), me())
                .expect_err("an excluded namespace must be refused, not applied");
            assert!(
                err.to_string().contains(app),
                "the refusal must name the namespace: {err}"
            );
            assert!(store.get(app, "k").unwrap().is_none(), "{app} was written");
        }
        // The control: a public namespace goes through the same call. Its row
        // is stamped `now` rather than reusing the fixture's `t: 100` — the
        // ledger declares a thirty-day retention window and the fold applies
        // it, so a 1970 timestamp would be withheld for a reason that has
        // nothing to do with what this test is about.
        let live = unsealed(vec![projected("k", Some(b"leaked"), now_secs(), "aa")]);
        let applied = store
            .apply_projection("contributions", &live, |_| Some(node(1)), me())
            .unwrap();
        assert_eq!(applied.merged, 1);
    }

    /// An actor the roster cannot place is COUNTED and skipped. Inventing an
    /// origin — a zero id, our own — would attribute a peer's write to
    /// somebody who did not make it, and `StoreEntry.origin` is what every
    /// per-peer reader resolves by.
    #[test]
    fn apply_projection_reports_an_unattributable_actor_rather_than_inventing_an_origin() {
        let store = MeshStore::in_memory().unwrap();
        let rows = unsealed(vec![
            projected("known", Some(b"v"), 100, "aa"),
            projected("stranger", Some(b"v"), 100, "zz"),
        ]);
        let applied = store
            .apply_projection("app", &rows, |actor| (actor == "aa").then(|| node(7)), me())
            .unwrap();
        assert_eq!(applied.merged, 1);
        assert_eq!(applied.unattributed, 1);
        assert_eq!(store.get("app", "known").unwrap().unwrap().origin, node(7));
        assert!(
            store.get("app", "stranger").unwrap().is_none(),
            "an unplaceable actor's row is not written under some other node"
        );
    }

    /// A tombstone deletes only what is not NEWER than it. The failing input
    /// is a delete at `t` racing a `set` at `t+1` that arrived first: without
    /// the bound the tombstone takes a live row and the key is gone from this
    /// node until somebody writes it again.
    #[test]
    fn a_tombstone_does_not_take_a_row_written_after_it() {
        let store = MeshStore::in_memory().unwrap();
        let plant = |key: &str, ts: u64| {
            store
                .merge_entry(StoreEntry {
                    app_id: "app".into(),
                    key: key.to_string(),
                    value: Bytes::from("live"),
                    timestamp: ts,
                    origin: node(1),
                })
                .unwrap()
        };
        plant("newer", 200);
        plant("older", 100);
        plant("equal", 150);

        let applied = store
            .apply_projection(
                "app",
                &unsealed(vec![
                    projected("newer", None, 150, "aa"),
                    projected("older", None, 150, "aa"),
                    projected("equal", None, 150, "aa"),
                    // A tombstone for a key this node never held is not a
                    // failure; there is simply nothing to take.
                    projected("absent", None, 150, "aa"),
                ]),
                |_| Some(node(1)),
                me(),
            )
            .unwrap();

        assert_eq!(applied.deleted, 2, "`older` and `equal`, not `newer`");
        assert!(
            store.get("app", "newer").unwrap().is_some(),
            "a tombstone must not take a row written after it"
        );
        assert!(store.get("app", "older").unwrap().is_none());
        assert!(
            store.get("app", "equal").unwrap().is_none(),
            "a delete at the row's own second wins — the write happened, then the delete"
        );
    }

    /// Applying a projection does NOT re-queue it. The whole point of the
    /// receive side going through `merge_entry` is that a row learned from a
    /// peer cannot echo back onto the rail — and the reconciliation is on the
    /// same side of the wire, so its delete must not queue a tombstone either.
    #[test]
    fn applying_a_projection_queues_nothing() {
        let store = MeshStore::in_memory().unwrap();
        store
            .merge_entry(StoreEntry {
                app_id: "app".into(),
                key: "retired".into(),
                value: Bytes::from("v"),
                timestamp: 50,
                origin: node(1),
            })
            .unwrap();
        let applied = store
            .apply_projection(
                "app",
                &sealed(
                    vec![
                        projected("a", Some(b"v"), 100, "aa"),
                        projected("b", None, 100, "aa"),
                    ],
                    "aa",
                    &["a"],
                ),
                |_| Some(node(1)),
                me(),
            )
            .unwrap();
        assert_eq!(applied.reconciled, 1, "the retired row went: {applied:?}");
        assert_eq!(store.outbox_len().unwrap(), 0);
    }

    /// **A closed snapshot retires what it does not name, and speaks only for
    /// its own author.** `stale` is the tombstone case: this node never
    /// received the delete, and the seal that retired it means it never will —
    /// so the actor's own live set is the only thing left that can say the row
    /// is gone (ARCH §7.1). The two controls in the same call are the whole
    /// rule: `kept` is named and stays, and `bb` never sealed, so nothing of
    /// `bb`'s is touched even though it is in the same namespace.
    ///
    /// Watched RED by dropping the reconciliation loop: `stale` survives.
    #[test]
    fn apply_projection_retires_a_sealed_actors_rows_and_only_that_actors() {
        let store = MeshStore::in_memory().unwrap();
        let plant = |key: &str, origin: NodeId| {
            store
                .merge_entry(StoreEntry {
                    app_id: "app".into(),
                    key: key.to_string(),
                    value: Bytes::from("held"),
                    timestamp: 100,
                    origin,
                })
                .unwrap()
        };
        plant("kept", node(7));
        plant("stale", node(7));
        plant("bb-row", node(8));

        let applied = store
            .apply_projection(
                "app",
                &sealed(
                    vec![projected("kept", Some(b"v"), 200, "aa")],
                    "aa",
                    &["kept"],
                ),
                |actor| match actor {
                    "aa" => Some(node(7)),
                    "bb" => Some(node(8)),
                    _ => None,
                },
                me(),
            )
            .unwrap();

        assert_eq!(applied.reconciled, 1, "{applied:?}");
        assert_eq!(applied.deleted, 0, "no tombstone was in the projection");
        assert!(
            store.get("app", "stale").unwrap().is_none(),
            "a row the actor's whole live set does not name is retired"
        );
        assert_eq!(
            store.get("app", "kept").unwrap().unwrap().value.as_ref(),
            b"v",
            "a named key keeps the value the projection gave it"
        );
        assert!(
            store.get("app", "bb-row").unwrap().is_some(),
            "bb has not sealed, so bb's rows are nobody's to retire"
        );

        // The control on the claim itself: the same rows, no live set, and
        // `stale` would have survived.
        plant("stale", node(7));
        let applied = store
            .apply_projection(
                "app",
                &unsealed(vec![projected("kept", Some(b"v"), 200, "aa")]),
                |_| Some(node(7)),
                me(),
            )
            .unwrap();
        assert_eq!(applied.reconciled, 0);
        assert!(store.get("app", "stale").unwrap().is_some());
    }

    /// **This node's own rows are never reconciled, because for them the store
    /// is upstream of the journal.** A local `set` writes the row and QUEUES
    /// the act; until the pump drains it the journal has never heard of the
    /// key, so our own snapshot — folded from the journal — cannot name it.
    /// Reconciling self would read the outbox lag as a retirement and delete a
    /// write this node just made.
    ///
    /// Watched RED by removing the `origin == self_id` skip: the second half
    /// of this test is that run, and `local` goes.
    #[test]
    fn apply_projection_never_reconciles_this_nodes_own_rows() {
        let store = MeshStore::in_memory().unwrap();
        assert!(store
            .set("app", "local", Bytes::from_static(b"just-written"), me())
            .unwrap());
        assert_eq!(
            store.outbox_len().unwrap(),
            1,
            "still on its way to the rail"
        );

        // Our own actor, our own seal, folded before the write reached it.
        let ours = sealed(vec![], "self", &[]);
        let applied = store
            .apply_projection("app", &ours, |_| Some(me()), me())
            .unwrap();
        assert_eq!(applied.reconciled, 0, "{applied:?}");
        assert!(
            store.get("app", "local").unwrap().is_some(),
            "a write still in the outbox is not a retired row"
        );

        // The control: the identical claim about somebody ELSE's node id takes
        // it, which is what the skip is holding back.
        let applied = store
            .apply_projection("app", &ours, |_| Some(me()), node(1))
            .unwrap();
        assert_eq!(applied.reconciled, 1, "{applied:?}");
        assert!(store.get("app", "local").unwrap().is_none());
    }

    /// An actor the roster cannot place has no rows to match: its live set
    /// names nothing this store holds under that name, and inventing an origin
    /// to match against would retire another node's rows (ARCH §18.3).
    #[test]
    fn a_live_set_from_an_unplaceable_actor_retires_nothing() {
        let store = MeshStore::in_memory().unwrap();
        store
            .merge_entry(StoreEntry {
                app_id: "app".into(),
                key: "k".into(),
                value: Bytes::from("v"),
                timestamp: 100,
                origin: node(7),
            })
            .unwrap();
        let applied = store
            .apply_projection("app", &sealed(vec![], "zz", &[]), |_| None, me())
            .unwrap();
        assert_eq!(applied.reconciled, 0);
        assert!(store.get("app", "k").unwrap().is_some());
    }

    // ── The retention floor: the projection's half of the window ──

    /// **The bug this closes.** The store is a projection, so a row a sweep
    /// deleted has no incumbent and the next fold re-inserts it. Both halves
    /// are here because either alone leaves the loop open: the fold must not
    /// (re-)take a row past the window (`withheld`), and it must take out one
    /// that is already here (`expired`).
    ///
    /// Watched RED by deleting the `floor.is_some_and(..)` arm from the row
    /// loop — `withheld` is 0 and the swept row is back — and again by deleting
    /// the `delete_older_than_in_app` block, which leaves `expired` 0 and the
    /// aged-in-place row in the store.
    #[test]
    fn the_projection_neither_takes_nor_keeps_a_row_past_the_retention_window() {
        const LEDGER: &str = crate::CONTRIBUTIONS_APP_ID;
        let store = MeshStore::in_memory().unwrap();
        let now = now_secs();
        let floor = crate::retention::floor_at(LEDGER, now).expect("the ledger declares a window");
        // A day either side of the boundary, so no clock tick between this
        // read and the one inside `apply_projection` can move a row across it.
        let (expired_t, fresh_t) = (floor - 86_400, now - 60);

        // The row is ALREADY HERE — planted through `merge_entry`, which is
        // how a peer's fold put it here before it aged out.
        store
            .merge_entry(StoreEntry {
                app_id: LEDGER.into(),
                key: "aged-in-place".into(),
                value: Bytes::from("v"),
                timestamp: expired_t,
                origin: node(1),
            })
            .unwrap();

        let rows = unsealed(vec![
            projected("from-the-journal", Some(b"v"), expired_t, "aa"),
            projected("in-window", Some(b"v"), fresh_t, "aa"),
        ]);
        let applied = store
            .apply_projection(LEDGER, &rows, |_| Some(node(1)), me())
            .unwrap();

        assert_eq!(
            applied.withheld, 1,
            "the fold re-took an expired row: {applied:?}"
        );
        assert_eq!(
            applied.expired, 1,
            "an aged-in-place row survived: {applied:?}"
        );
        assert_eq!(
            applied.merged, 1,
            "the in-window row must still land: {applied:?}"
        );
        assert!(store.get(LEDGER, "from-the-journal").unwrap().is_none());
        assert!(store.get(LEDGER, "aged-in-place").unwrap().is_none());
        assert!(
            store.get(LEDGER, "in-window").unwrap().is_some(),
            "the window took only what is past it"
        );
    }

    /// The control the test above needs: a namespace that declares NO window
    /// is not swept for age at all. Without it, `expired`/`withheld` firing on
    /// every namespace would look identical from inside the ledger's test — and
    /// `work-atlas` claims, `processed_shards` markers and
    /// `corpus-engine/handoff:*` records are all deliberately never rewritten.
    #[test]
    fn a_namespace_with_no_declared_window_is_never_swept_for_age() {
        const UNDECLARED: &str = "processed_shards";
        assert_eq!(crate::retention::window_days(UNDECLARED), None);
        let store = MeshStore::in_memory().unwrap();
        store
            .merge_entry(StoreEntry {
                app_id: UNDECLARED.into(),
                key: "corpus:shard-0".into(),
                value: Bytes::from("v"),
                timestamp: 100,
                origin: node(1),
            })
            .unwrap();
        let rows = unsealed(vec![projected("ancient", Some(b"v"), 100, "aa")]);
        let applied = store
            .apply_projection(UNDECLARED, &rows, |_| Some(node(1)), me())
            .unwrap();
        assert_eq!((applied.expired, applied.withheld), (0, 0), "{applied:?}");
        assert_eq!(applied.merged, 1);
        assert!(store.get(UNDECLARED, "corpus:shard-0").unwrap().is_some());
        assert!(store.get(UNDECLARED, "ancient").unwrap().is_some());
    }

    /// The sweep and the fold agree ROW FOR ROW, because they read one window
    /// (ARCH §10.6). Asserted behaviourally: the same three rows put through
    /// `RetentionGc::sweep` and through `apply_projection` leave the same two
    /// survivors. Two cutoffs that differ by an hour would pass a check on the
    /// constants and fail here on the row between them.
    #[test]
    fn the_sweep_and_the_fold_keep_exactly_the_same_rows() {
        const LEDGER: &str = crate::CONTRIBUTIONS_APP_ID;
        let now = now_secs();
        let floor = crate::retention::floor_at(LEDGER, now).unwrap();
        let ages = [
            ("old", floor - 86_400),
            ("edge", floor + 60),
            ("new", now - 60),
        ];

        let plant = || {
            let store = std::sync::Arc::new(MeshStore::in_memory().unwrap());
            for (key, t) in ages {
                store
                    .merge_entry(StoreEntry {
                        app_id: LEDGER.into(),
                        key: key.into(),
                        value: Bytes::from("v"),
                        timestamp: t,
                        origin: node(1),
                    })
                    .unwrap();
            }
            store
        };
        let survivors = |store: &MeshStore| -> Vec<String> {
            let mut k = store.list_keys(LEDGER).unwrap();
            k.sort();
            k
        };

        let swept = plant();
        crate::RetentionGc::for_namespace(
            std::sync::Arc::clone(&swept),
            LEDGER,
            std::time::Duration::from_secs(3_600),
        )
        .expect("the ledger declares a window")
        .sweep()
        .unwrap();

        let folded = plant();
        folded
            .apply_projection(LEDGER, &unsealed(vec![]), |_| Some(node(1)), me())
            .unwrap();

        assert_eq!(survivors(&swept), vec!["edge", "new"], "the sweep");
        assert_eq!(survivors(&folded), survivors(&swept), "the fold disagrees");
    }

    /// **Why the work atlas's eviction is safe and a sweep is not.**
    ///
    /// `WorkAtlasGc` drops an expired claim through `MeshStore::delete` (via
    /// `MeshPeerStore`), which queues a TOMBSTONE — so the fold carries the
    /// removal instead of undoing it. The same row taken by `gc_app_before`
    /// queues nothing, and on a namespace that declares no retention window
    /// there is no floor either, so the next fold puts it straight back.
    ///
    /// Both halves in one test, because the difference between them IS the
    /// finding: on a projection, a delete is an act and a sweep is a wish.
    /// A namespace that needs rows dropped for age declares a window
    /// (`crate::retention`); one that needs a specific row dropped calls
    /// `delete`. There is no third way, and the second assertion is what says
    /// so out loud.
    #[test]
    fn a_delete_survives_the_fold_and_an_undeclared_sweep_does_not() {
        const ATLAS: &str = "work-atlas";
        assert_eq!(
            crate::retention::window_days(ATLAS),
            None,
            "the atlas evicts by its own per-record deadline, not by row age"
        );
        let store = MeshStore::in_memory().unwrap();
        let now = now_secs();
        let claim = |k: &str| projected(k, Some(b"claim"), now - 10, "aa");

        store
            .apply_projection(
                ATLAS,
                &unsealed(vec![claim("evicted"), claim("swept")]),
                |_| Some(node(1)),
                me(),
            )
            .unwrap();

        // The atlas's door: a delete queues a tombstone.
        assert!(store.delete(ATLAS, "evicted").unwrap());
        let queued = store.outbox_take(8).unwrap();
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert!(queued[0].deleted && queued[0].value.is_none());

        // The other door: a sweep queues nothing at all.
        assert_eq!(store.gc_app_before(ATLAS, now).unwrap(), 1);
        assert_eq!(
            store.outbox_take(8).unwrap().len(),
            1,
            "a sweep is not an act — the outbox still holds only the tombstone"
        );

        // The next fold. The tombstone is on the journal by now; the sweep is
        // on no journal anywhere, so the row it took is still a winning row.
        let next = unsealed(vec![
            projected("evicted", None, queued[0].t, "aa"),
            claim("swept"),
        ]);
        store
            .apply_projection(ATLAS, &next, |_| Some(node(1)), me())
            .unwrap();
        assert!(
            store.get(ATLAS, "evicted").unwrap().is_none(),
            "a tombstone the fold carries keeps the row gone"
        );
        assert!(
            store.get(ATLAS, "swept").unwrap().is_some(),
            "and a sweep with neither a tombstone nor a declared window is \
             undone by the next round — which is why there is no third way"
        );
    }
}
