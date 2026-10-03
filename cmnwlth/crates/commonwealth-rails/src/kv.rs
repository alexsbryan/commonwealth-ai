// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh store this daemon hosts, its pump, and the `/v1/mesh/kv/*` doors
//! over it (five-programs fp-77, decision five-programs-40).
//!
//! **The store is a projection of THIS process's journals.** One
//! `MeshStore::in_memory()`, rebuilt at start by folding every KV-shaped
//! namespace under the rail root through `commonwealth_state::rail_kv` — the
//! journal is the durable half, so a restart loses nothing that reached it.
//! A door that changed the store drains the outbox onto the journal BEFORE
//! it answers ([`KvHost::journal_outbox`]), so an acknowledged write survives
//! a kill: a solo node has no peer copy (pc-solo-durable, five-programs-66).
//! The pump is the send half lifted from `sovereign_mesh::rail_kv_pump`:
//! every [`PUMP_INTERVAL`] it drains what is still queued (a write deferred
//! for want of a roster) the same way, signed with this node's key, and
//! seals + snapshots a KV namespace either drain fed once this node's own
//! ops above its last seal pass `rail_kv::SEAL_AFTER_OWN_OPS`.
//!
//! **One sealer per journal.** A snapshot's mark retires every row of its
//! actor it does not name, so two stores sealing one journal would retire
//! each other's rows. Since the daemon pump's KV half was deleted (fp-83) this
//! is the only store that seals a store namespace, local-only ones included.
//! It seals only the namespaces its OWN outbox fed on the same tick, and never
//! the `mesh-measurements` or `work` journals — their writers are the daemon's.
//!
//! The doors' bodies are the daemon's (`routes_mesh_kv.rs`), field for field,
//! so the one client (sovereign-turn-client's `rails_kv`) reads either. The wire structs
//! themselves are `sovereign_contracts::peer`'s, which this binary may not
//! name (the lift boundary), so the request shapes are mirrored here the way
//! [`crate::api::ClaimRequest`] mirrors the publish body.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{
    Admission, Ed25519Verifier, RailAct, RailError, RingJournal, RingRail, Roster,
};
use commonwealth_state::rail_kv::{self, SEAL_AFTER_OWN_OPS};
use commonwealth_state::{MeshStore, Outboxed};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

mod doors;
pub use doors::{router, KvLookup, KvScanQuery, KvSetBody};

/// How often the pump drains the outbox and runs the seal check. A door
/// write does not wait for it: the door journals before it answers.
pub const PUMP_INTERVAL: Duration = Duration::from_secs(2);

/// How many outbox rows one tick takes. A bound on how long one tick holds a
/// journal's writer, not a throughput knob: what is left is taken next tick.
const OUTBOX_DRAIN_LIMIT: usize = 256;

/// The store, and what projecting and pumping it needs.
pub struct KvHost {
    pub store: MeshStore,
    rail: Arc<RingRail>,
    mesh: Arc<RwLock<Mesh>>,
    self_id: NodeId,
    self_pubkey: Option<NodePubkey>,
    /// Namespaces the ingest door took new peer ops for since the last tick;
    /// [`KvHost::project_dirty`] folds each once (fp-109).
    dirty: Mutex<BTreeSet<String>>,
    /// Per namespace, the seq of this node's admitted snapshot mark for its
    /// current floor, which the seal bar counts above, as
    /// [`own_floor_and_mark`] last read it off an admission — never from the
    /// mark this pump wrote, so that function stays the one reader. The cheap
    /// line count starts there too and stays an upper bound; absent, it takes
    /// every own line.
    snapshot_base: Mutex<HashMap<String, u64>>,
    /// One drainer at a time: a door and the tick both take the outbox, and
    /// a row is acked only after its append, so two drains would append it
    /// twice. Held across the tick's seal too, so a door append never lands
    /// between a seal and its snapshot.
    drain: tokio::sync::Mutex<()>,
    /// Namespaces a drain appended to since the last tick's seal check.
    seal_due: Mutex<BTreeSet<String>>,
    /// The plane seal's own snapshot ends, held across ticks for
    /// [`crate::plane_seal::seal_once`].
    plane_snapshot_ends: crate::plane_seal::SnapshotEnds,
}

/// What one [`KvHost::pump_once`] did — returned so a test asserts on the
/// mechanism rather than on log lines. Same verdicts as the daemon pump's.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PumpOutcome {
    pub appended: usize,
    /// Left queued: the roster was unreadable, or this node is in no roster.
    pub deferred: usize,
    /// Dropped with a `warn`: the rail will never accept them.
    pub refused: usize,
    pub sealed: usize,
    /// Live rows re-appended behind those seals (the mark not counted).
    pub snapshot_rows: usize,
}

/// Whether a namespace's journal speaks `rail_kv`. The work plane folds its
/// own journal, and a gossip-excluded namespace is one the store refuses
/// inbound (`MeshStore::apply_projection`) — skipped here rather than warned.
fn is_kv_namespace(namespace: &str) -> bool {
    namespace != commonwealth_work::WORK_NAMESPACE
        && !commonwealth_state::peer_preferences::is_gossip_excluded(namespace)
}

impl KvHost {
    pub fn new(
        rail: Arc<RingRail>,
        mesh: Arc<RwLock<Mesh>>,
        self_id: NodeId,
        self_pubkey: Option<NodePubkey>,
    ) -> Result<Self, commonwealth_state::Error> {
        Ok(Self {
            store: MeshStore::in_memory()?,
            rail,
            mesh,
            self_id,
            self_pubkey,
            dirty: Mutex::new(BTreeSet::new()),
            snapshot_base: Mutex::new(HashMap::new()),
            drain: tokio::sync::Mutex::new(()),
            seal_due: Mutex::new(BTreeSet::new()),
            plane_snapshot_ends: Mutex::new(HashMap::new()),
        })
    }

    /// Queue `namespace` for the next tick's fold — called by the ingest door
    /// when it admitted new ops, so a peer's write reaches the store without
    /// a restart.
    pub fn mark_dirty(&self, namespace: &str) {
        self.dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(namespace.to_string());
    }

    /// Fold every namespace marked since the last call, once each — batched
    /// per tick, never per ingested chunk. A non-store name is skipped by
    /// [`KvHost::project_namespace`]'s own debug line. Returns how many
    /// projected.
    pub async fn project_dirty(&self) -> usize {
        let dirty = std::mem::take(&mut *self.dirty.lock().unwrap_or_else(|p| p.into_inner()));
        if dirty.is_empty() {
            return 0;
        }
        let mut projected = 0usize;
        for namespace in &dirty {
            if self.project_namespace(namespace).await.is_some() {
                projected += 1;
            }
        }
        debug!(target: "rails", dirty = dirty.len(), projected,
               "kv: folded the namespaces the ingest door fed since the last tick");
        projected
    }

    /// Rebuild the store from every journal on disk. Run once at start,
    /// before the first drain; returns how many namespaces projected, the
    /// local-only ones rehydrated from this node's own rows included.
    pub async fn project_all_on_disk(&self) -> usize {
        let started = std::time::Instant::now();
        let namespaces = match self.rail.namespaces() {
            Ok(n) => n,
            Err(e) => {
                warn!(target: "rails", error = %e, "kv: cannot enumerate namespaces, nothing projected at start");
                return 0;
            }
        };
        let mut projected = 0usize;
        for namespace in &namespaces {
            if self.project_namespace(namespace).await.is_some() {
                projected += 1;
            }
        }
        // `RingRail::namespaces` omits local-only journals by design (never
        // offered), so they are read from the unfiltered disk list and folded
        // through the own-journal door (fp-108).
        let local_only: Vec<String> = match commonwealth_rail::namespaces_in(self.rail.root()) {
            Ok(all) => all
                .into_iter()
                .filter(|ns| commonwealth_rail::is_local_only(ns))
                .collect(),
            Err(e) => {
                warn!(target: "rails", error = %e, "kv: cannot enumerate local-only journals, none rehydrated");
                Vec::new()
            }
        };
        let mut rehydrated = 0usize;
        for namespace in &local_only {
            if self.project_own_namespace(namespace).await.is_some() {
                rehydrated += 1;
            }
        }
        info!(
            target: "rails",
            namespaces = namespaces.len(),
            projected,
            local_only = local_only.len(),
            rehydrated,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "kv: rebuilt the store from the journals on disk"
        );
        projected + rehydrated
    }

    /// Fold one local-only namespace's journal back into the store, taking
    /// only this node's own signed rows (`MeshStore::apply_own_projection`).
    async fn project_own_namespace(&self, namespace: &str) -> Option<usize> {
        let (journal, roster) = match self.journal_and_roster(namespace).await {
            Ok(jr) => jr,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: a local-only journal or its roster is unreadable, nothing rehydrated");
                return None;
            }
        };
        let admission = match journal.admit(&roster, &Ed25519Verifier) {
            Ok(a) => a,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: a local-only journal would not admit, nothing rehydrated");
                return None;
            }
        };
        let projection = rail_kv::project(&admission);
        match self.store.apply_own_projection(
            namespace,
            &projection,
            &self.rail.signer().actor(),
            self.self_id,
        ) {
            Ok(applied) => {
                debug!(
                    target: "rails",
                    namespace,
                    rows = projection.rows.len(),
                    merged = applied.merged,
                    deleted = applied.deleted,
                    skipped_foreign = applied.unattributed,
                    "kv: rehydrated a local-only namespace from its own journal"
                );
                Some(applied.merged + applied.deleted)
            }
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the store refused this local-only namespace");
                None
            }
        }
    }

    /// Fold one namespace's admitted ops into the store.
    pub async fn project_namespace(&self, namespace: &str) -> Option<usize> {
        if !is_kv_namespace(namespace) {
            debug!(target: "rails", namespace, "kv: not a store namespace, skipped");
            return None;
        }
        let (journal, roster) = match self.journal_and_roster(namespace).await {
            Ok(jr) => jr,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the journal or its roster is unreadable, nothing projected");
                return None;
            }
        };
        let admission = match journal.admit(&roster, &Ed25519Verifier) {
            Ok(a) => a,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the journal would not admit, nothing projected");
                return None;
            }
        };
        let projection = rail_kv::project(&admission);
        if projection.unreadable > 0 {
            warn!(target: "rails", namespace, unreadable = projection.unreadable,
                  "kv: acts on a store namespace this build cannot read as store writes");
        }
        let node_ids = self.node_ids().await;
        match self.store.apply_projection(
            namespace,
            &projection,
            |actor| node_ids.get(actor).copied(),
            self.self_id,
        ) {
            Ok(applied) => {
                debug!(
                    target: "rails",
                    namespace,
                    rows = projection.rows.len(),
                    merged = applied.merged,
                    deleted = applied.deleted,
                    reconciled = applied.reconciled,
                    expired = applied.expired,
                    unattributed = applied.unattributed,
                    "kv: projected a namespace into the store"
                );
                Some(applied.merged + applied.deleted + applied.reconciled + applied.expired)
            }
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the store refused this namespace");
                None
            }
        }
    }

    /// Actor key → node id, from the same membership the roster derives from
    /// (`crate::rail::derive_roster`'s rules: self's key is passed in, a row
    /// with no key places nobody).
    async fn node_ids(&self) -> HashMap<String, NodeId> {
        let mesh = self.mesh.read().await;
        let mut out = HashMap::new();
        for record in mesh.members.values() {
            let key = if record.node_id == self.self_id {
                self.self_pubkey.or(record.node_pubkey)
            } else {
                record.node_pubkey
            };
            if let Some(key) = key {
                out.insert(key.to_string(), record.node_id);
            }
        }
        out
    }

    /// One drain of the outbox onto the journals, plus the seal check for
    /// every namespace a drain appended to since the last one — a door's
    /// included, so journaling before the ack never skips a seal.
    pub async fn pump_once(&self) -> PumpOutcome {
        let _drain = self.drain.lock().await;
        let mut out = self.drain_outbox().await;
        let due = std::mem::take(&mut *self.seal_due.lock().unwrap_or_else(|p| p.into_inner()));
        for namespace in due {
            match self.journal_and_roster(&namespace).await {
                Ok((journal, roster)) => self.seal_if_due(&journal, &roster, &mut out).await,
                Err(e) => {
                    warn!(target: "rails", namespace, error = %e,
                          "kv pump: the roster is unreadable, so the seal check waits a tick");
                    self.seal_due
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(namespace);
                }
            }
        }
        out
    }

    /// Drain the outbox onto the journals now, before a door answers the
    /// write it just made — the store is in memory, so until this append a
    /// kill loses a write the caller was told had landed. Returns what the
    /// drain did; the seal check stays the tick's.
    pub async fn journal_outbox(&self) -> PumpOutcome {
        let _drain = self.drain.lock().await;
        self.drain_outbox().await
    }

    /// One drain of the outbox onto the journals; the caller holds `drain`.
    /// Appended rows are acked; `NotInRoster` and an unreadable roster leave
    /// rows queued; any other refusal acks the row with a `warn` naming it.
    async fn drain_outbox(&self) -> PumpOutcome {
        let mut out = PumpOutcome::default();
        let queued = match self.store.outbox_take(OUTBOX_DRAIN_LIMIT) {
            Ok(rows) => rows,
            Err(e) => {
                warn!(target: "rails", error = %e, "kv pump: the outbox could not be read");
                return out;
            }
        };
        let mut by_namespace: BTreeMap<String, Vec<Outboxed>> = BTreeMap::new();
        for row in queued {
            by_namespace
                .entry(row.app_id.clone())
                .or_default()
                .push(row);
        }

        for (namespace, rows) in by_namespace {
            let (journal, roster) = match self.journal_and_roster(&namespace).await {
                Ok(jr) => jr,
                Err(e) => {
                    warn!(target: "rails", namespace, error = %e, queued = rows.len(),
                          "kv pump: the roster is unreadable, so these writes stayed queued");
                    out.deferred += rows.len();
                    continue;
                }
            };
            // One `append_all` per namespace: one write and one fsync for the
            // rows rather than one of each per row (pc-rails-journal-linear).
            // The per-row answers are the ones the row-at-a-time loop gave:
            // a row that cannot be a payload is dropped, except that one after
            // the first carried row stays queued when the roster refuses (the
            // loop stopped dropping once an append had learnt that).
            let mut acked: Vec<i64> = Vec::new();
            let mut acts: Vec<RailAct> = Vec::new();
            let mut carried: Vec<&Outboxed> = Vec::new();
            let mut unfit: Vec<(&Outboxed, String)> = Vec::new();
            let mut unfit_before_first = 0usize;
            for row in &rows {
                let op = &row.op;
                match rail_kv::to_payload(&op.key, op.value.as_deref(), op.t) {
                    Ok(payload) => {
                        acts.push(RailAct::Record { payload });
                        carried.push(row);
                    }
                    Err(e) => {
                        if carried.is_empty() {
                            unfit_before_first += 1;
                        }
                        unfit.push((row, e.to_string()));
                    }
                }
            }
            let appended = if acts.is_empty() {
                Ok(Vec::new())
            } else {
                journal.append_all(
                    acts,
                    self.rail.signer(),
                    &roster,
                    None,
                    &commonwealth_rail::Ed25519Verifier,
                )
            };
            let not_in_roster = matches!(appended, Err(RailError::NotInRoster { .. }));
            for (i, (row, error)) in unfit.iter().enumerate() {
                if not_in_roster && i >= unfit_before_first {
                    out.deferred += 1;
                    continue;
                }
                warn!(target: "rails", namespace, key = %row.op.key, error = %error,
                      "kv pump: this write cannot travel on the rail and was dropped");
                out.refused += 1;
                acked.push(row.id);
            }
            let mut appended_here = 0usize;
            match appended {
                Ok(ops) => {
                    for (row, appended) in carried.iter().zip(&ops) {
                        let op = &row.op;
                        debug!(target: "rails", namespace, key = %op.key, t = op.t,
                               deleted = op.value.is_none(), seq = appended.kind.seq,
                               "kv pump: appended a local write");
                        acked.push(row.id);
                    }
                    appended_here = ops.len();
                }
                Err(RailError::NotInRoster { actor, .. }) => {
                    out.deferred += carried.len();
                    debug!(target: "rails", namespace, actor = %actor, queued = rows.len(),
                           "kv pump: this node is in no roster for this namespace yet, so its \
                            writes stay queued");
                }
                Err(e) => {
                    for row in &carried {
                        warn!(target: "rails", namespace, key = %row.op.key, error = %e,
                              "kv pump: the rail refused this write, which was dropped");
                        out.refused += 1;
                        acked.push(row.id);
                    }
                }
            }
            if !acked.is_empty() {
                if let Err(e) = self.store.outbox_ack(&acked) {
                    warn!(target: "rails", error = %e, rows = acked.len(), "kv pump: the outbox ack failed");
                }
            }
            out.appended += appended_here;
            if appended_here > 0 {
                self.seal_due
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(namespace);
            }
        }
        out
    }

    async fn journal_and_roster(
        &self,
        namespace: &str,
    ) -> Result<(Arc<RingJournal>, Roster), RailError> {
        let journal = self.rail.journal(namespace)?;
        let roster = self.rail.roster(&journal).await?;
        Ok((journal, roster))
    }

    /// Seal and snapshot `journal` if this node's own ops since its last
    /// snapshot have passed `SEAL_AFTER_OWN_OPS` — the snapshot's rows are its
    /// live set restated and do not count, or a live set over the bar
    /// re-seals on every write (F13, pc-rails-reseal-loop). The raw line
    /// count above the cached snapshot base gates the admission (the admitted
    /// count can never exceed it), so the verify cost is paid only when a
    /// seal may be due.
    async fn seal_if_due(&self, journal: &RingJournal, roster: &Roster, out: &mut PumpOutcome) {
        let namespace = journal.namespace();
        // Local-only journals are sealed too: since the daemon pump's KV half
        // went (fp-83) this store is their one writer, so no twin sealer can
        // retire its rows. `is_kv_namespace` still gates projection.
        if !is_kv_namespace(namespace) && !commonwealth_rail::is_local_only(namespace) {
            return;
        }
        let mine = self.rail.signer().actor();
        let base = self.snapshot_base(namespace);
        let own_lines = match journal.read() {
            Ok((ops, _)) => ops
                .iter()
                .filter(|o| o.actor == mine && base.is_none_or(|b| o.kind.seq > b))
                .count(),
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv pump: the journal could not be read for the seal check");
                return;
            }
        };
        if own_lines < SEAL_AFTER_OWN_OPS {
            return;
        }
        let admission = match journal.admit(roster, &Ed25519Verifier) {
            Ok(a) => a,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv pump: the journal could not be admitted for the seal check");
                return;
            }
        };
        let (floor, mark) = own_floor_and_mark(&admission, &mine);
        self.set_snapshot_base(namespace, mark);
        let own_since_snapshot = admission
            .ops
            .iter()
            .filter(|o| o.actor == mine && mark.map_or(o.seq >= floor, |m| o.seq > m))
            .count();
        if own_since_snapshot < SEAL_AFTER_OWN_OPS {
            debug!(target: "rails", namespace, own_lines, own_since_snapshot, floor, snapshot_mark = ?mark,
                   "kv pump: the cheap line count cleared the bar and the admitted count did not");
            return;
        }
        // The seal moves the floor, so the base above is stale whatever
        // happens next; the next admission reads the new one.
        self.set_snapshot_base(namespace, None);
        let sealed = match journal.seal(self.rail.signer(), roster, &Ed25519Verifier) {
            Ok(s) => s,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv pump: the seal was refused, so nothing was retired");
                return;
            }
        };
        out.sealed += 1;
        if let Ok(done) = &sealed.retired {
            info!(target: "rails", namespace, own_since_snapshot, snapshot_mark = ?mark,
                  removed = done.removed, kept = done.kept, "kv pump: sealed a store namespace");
        }
        out.snapshot_rows += self.snapshot(journal, roster, sealed.op.kind.seq);
    }

    fn snapshot_base(&self, namespace: &str) -> Option<u64> {
        let bases = self.snapshot_base.lock().unwrap_or_else(|p| p.into_inner());
        bases.get(namespace).copied()
    }

    fn set_snapshot_base(&self, namespace: &str, mark: Option<u64>) {
        let mut bases = self.snapshot_base.lock().unwrap_or_else(|p| p.into_inner());
        match mark {
            Some(m) => bases.insert(namespace.to_string(), m),
            None => bases.remove(namespace),
        };
    }

    /// Re-append this node's live rows above the floor its seal just set,
    /// then the mark that closes the snapshot — without it a seal is a delete.
    /// Only rows this node ORIGINATED; the mark is written last, and one that
    /// could not be written claims nothing (the failure direction is "do not
    /// retire").
    fn snapshot(&self, journal: &RingJournal, roster: &Roster, floor: u64) -> usize {
        let namespace = journal.namespace();
        let rows = match self.store.scan(namespace, "") {
            Ok(r) => r,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e,
                      "kv pump: the live set could not be read, so the seal retired rows nothing replaced");
                return 0;
            }
        };
        // One batch, so the log is read once for the whole snapshot rather
        // than once per row (`RingJournal::append_all`).
        let mut acts = Vec::new();
        let mut skipped = 0usize;
        for row in rows {
            if row.origin != self.self_id {
                skipped += 1;
                continue;
            }
            match rail_kv::to_payload(&row.key, Some(&row.value), row.timestamp) {
                Ok(payload) => acts.push(RailAct::Record { payload }),
                Err(e) => warn!(target: "rails", namespace, key = %row.key, error = %e,
                                "kv pump: a live row could not be snapshotted and is now below the floor"),
            }
        }
        let wanted = acts.len();
        let appended = match journal.append_all(
            acts,
            self.rail.signer(),
            roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        ) {
            Ok(ops) => ops.len(),
            Err(e) => {
                warn!(target: "rails", namespace, rows = wanted, error = %e,
                      "kv pump: the live rows could not be snapshotted and are now below the floor");
                0
            }
        };
        let mark = rail_kv::snapshot_mark(floor)
            .map_err(|e| e.to_string())
            .and_then(|payload| {
                journal
                    .append(
                        RailAct::Record { payload },
                        self.rail.signer(),
                        roster,
                        None,
                        &commonwealth_rail::Ed25519Verifier,
                    )
                    .map_err(|e| e.to_string())
            });
        match mark {
            Ok(op) => {
                info!(target: "rails", namespace, appended, peers_rows_skipped = skipped, floor,
                      mark_seq = op.kind.seq,
                      "kv pump: snapshotted this node's live rows above the new floor, and closed it");
            }
            Err(e) => {
                warn!(target: "rails", namespace, appended, floor, error = %e,
                      "kv pump: the snapshot could not be closed, so peers will keep \
                       whatever of ours they already hold");
            }
        }
        appended
    }
}

/// This node's floor in `admission`, and the seq of its own admitted snapshot
/// mark that closes that floor, if one is held.
///
/// The ONE reading of what the seal bar counts: own ops above that mark, the
/// writes since the last snapshot — or, with no mark (never sealed, or a
/// snapshot that could not be closed), own ops from the floor. Between the
/// floor and the mark lies the snapshot, the live set restated: counting it
/// let a live set over `SEAL_AFTER_OWN_OPS` clear the bar by itself, so every
/// write-bearing tick re-sealed (F13: the deployed node's rails.log showed 64
/// seals of activity-private since 14:47Z, one for every other namespace).
fn own_floor_and_mark(admission: &Admission, mine: &str) -> (u64, Option<u64>) {
    let floor = admission.floors.get(mine).copied().unwrap_or(0);
    let mark = admission
        .ops
        .iter()
        .filter(|o| o.actor == mine && o.seq > floor && o.applies())
        .filter(|o| o.payload.as_ref().and_then(rail_kv::read_snapshot_mark) == Some(floor))
        .map(|o| o.seq)
        .max();
    (floor, mark)
}

/// Drain the store forever, and run the two plane seal arms
/// ([`crate::plane_seal::seal_once`]) on the same tick. Spawned by
/// [`crate::RailsDaemon::run`] after it has projected the store; aborted with
/// it.
///
/// Each tick runs on the blocking pool: a journal append is synchronous file
/// I/O and an fsync, and a seal admits the whole journal (a seal's snapshot
/// was tens of seconds of it before `RingJournal::append_all`), and on an async worker it
/// held the API's requests queued behind it (F13: a sandbox
/// `/v1/mesh/status` p95 of 5.3 s through a 2,500-row cycle).
pub async fn run_forever(host: Arc<KvHost>) {
    info!(target: "rails", interval_secs = PUMP_INTERVAL.as_secs(),
          seal_after_own_ops = SEAL_AFTER_OWN_OPS, "kv pump: started");
    let runtime = tokio::runtime::Handle::current();
    loop {
        let started = std::time::Instant::now();
        let tick = {
            let host = Arc::clone(&host);
            let runtime = runtime.clone();
            tokio::task::spawn_blocking(move || runtime.block_on(tick_once(&host)))
        };
        let out = match tick.await {
            Ok(out) => out,
            Err(e) => {
                warn!(target: "rails", error = %e, "kv pump: a tick did not finish; the next one retries");
                PumpOutcome::default()
            }
        };
        if out != PumpOutcome::default() {
            debug!(target: "rails", appended = out.appended, deferred = out.deferred,
                   refused = out.refused, sealed = out.sealed, snapshot_rows = out.snapshot_rows,
                   elapsed_ms = started.elapsed().as_millis() as u64,
                   "kv pump: tick (blocking pool)");
        }
        tokio::time::sleep(PUMP_INTERVAL).await;
    }
}

/// One tick: fold what peers sent, drain the outbox, run the plane seals.
async fn tick_once(host: &KvHost) -> PumpOutcome {
    host.project_dirty().await;
    let mut out = host.pump_once().await;
    let planes = crate::plane_seal::seal_once(&host.rail, &host.plane_snapshot_ends).await;
    out.sealed += planes.sealed;
    out.snapshot_rows += planes.snapshot_rows;
    out
}

#[cfg(test)]
#[path = "kv/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "kv/projection_tests.rs"]
mod projection_tests;

#[cfg(test)]
#[path = "kv/snapshot_tests.rs"]
mod snapshot_tests;
