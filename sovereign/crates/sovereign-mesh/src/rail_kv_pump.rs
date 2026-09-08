// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh store's send half — local writes onto the ring journal, and the
//! seal that keeps the journal from growing forever.
//!
//! # Where this sits
//!
//! `commonwealth_state::rail_kv` is the vocabulary and the fold, and it has no
//! I/O, no clock and no journal. This is the half that has all three: a task
//! beside [`crate::ring_sync`]'s that drains
//! [`MeshStore::outbox_take`](commonwealth_state::MeshStore::outbox_take) every
//! [`RAIL_KV_PUMP_INTERVAL`] and signs each queued write onto its namespace's
//! journal. Nothing else is done with the row — the local store already holds
//! it, because `set` wrote it in the same transaction that queued it.
//!
//! The store is not replicated any more; it is PROJECTED. The journal is
//! truth, and [`project_namespace`] is what turns a namespace's admitted ops
//! back into store rows on the receiving side.
//!
//! # Sealing: for the daemon's own rings, the daemon decides
//!
//! Rung 4a recorded that WHEN to seal is the operator's call. That holds for
//! an APP's ring: sealing forgets history, and an app's history is the app's.
//! It does not hold here. The namespaces in
//! [`DAEMON_OWN_NAMESPACES`](crate::ring_roster::DAEMON_OWN_NAMESPACES) are
//! ones the daemon writes on its own behalf, on a cadence no person chose, and
//! nobody is going to run `svrn ring seal inference` every few weeks. A journal
//! nobody seals grows without bound on every node in the mesh, so leaving the
//! decision unmade is itself a decision — and the wrong one.
//!
//! It is safe to make automatically because the two halves verify themselves.
//! `RingJournal::compact` re-admits its own result and refuses to write if the
//! prune would raise a gap, and the snapshot below re-appends the live set
//! under its ORIGINAL write time, so the fold puts every row back exactly where
//! its author left it. That is what `sync.rs:43`'s refused truncation knob was
//! not: an operator-set line with nothing checking what fell below it.
//!
//! **App rings are untouched.** Their roster comes from a file, this pump never
//! drains a row for one (the outbox only carries `MeshStore` writes), and
//! `POST /v1/rail/append` remains the only way one of them seals.
//!
//! # What a seal costs, and how the cost was absorbed
//!
//! A seal + snapshot re-appends the LIVE rows this node owns, from the store.
//! A TOMBSTONE is not a live row and is not in the store, so a delete this node
//! published would stop travelling once the seal that retired it lands, and a
//! peer that never received the tombstone would keep its stale value forever.
//! ea4da7b68 recorded that as the KV shape of K7's "no history past the next
//! seal" and priced it with the threshold; it is now CLOSED instead.
//!
//! **The seal itself carries the answer.** A seal followed by its whole
//! snapshot IS the actor's live set, so a peer holding both may retire every
//! other row of that actor's. [`snapshot`] therefore ends with
//! `rail_kv::snapshot_mark(floor)` — one act naming the seal it closes,
//! appended after the last row — and holding that mark with no sequence hole
//! for its actor is holding the whole snapshot. `rail_kv::project` reports the
//! live set per closed actor and `MeshStore::apply_projection` reconciles the
//! store's rows to it in the same call. What a peer misses is now bounded by
//! what it has HELD, not by what it happened to be online for.
//!
//! The threshold stays where it is: `SEAL_AFTER_OWN_OPS` is priced on journal
//! bytes (~594 a line) and the cost it was raised for is gone, not smaller.
//!
//! # Retention rides the fold, and through the fold it reaches the journal
//!
//! A namespace may declare a retention window
//! (`commonwealth_state::retention`). It is enforced in
//! [`MeshStore::apply_projection`](commonwealth_state::MeshStore::apply_projection),
//! because that is the only place it CAN be: the store is a projection, so a
//! row a sweep deletes has no incumbent and `merge_entry` puts it back on the
//! next round. `RetentionGc`'s thirty days on the contributions ledger were
//! undone every minute until 2026-09-08, on every node with an online peer
//! ([`crate::ring_sync`]'s `a_retention_sweep_is_not_undone_by_the_next_projection`).
//!
//! An expiry publishes NOTHING. The floor is `now` minus a constant and `t` is
//! on every op, so every node derives the same answer without being told — and
//! a tombstone per retired row would add a journal line to every node in the
//! mesh for each row retention exists to remove.
//!
//! [`snapshot`] is how the store's bound becomes the journal's: it re-appends
//! this node's live set FROM THE STORE, so a row the floor keeps out is a row
//! the snapshot does not carry above the new floor, and the compaction behind
//! the seal deletes its line. Same decision, reached twice, with no second
//! number (ARCH §10.6).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_api::state::AppState;
use commonwealth_rail::{Ed25519Verifier, RailAct, RailError, RingJournal, RingRail, Roster};
use commonwealth_state::{rail_kv, MeshStore, Outboxed};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::ring_roster::MeshRoster;

/// How often the outbox is drained.
///
/// Two seconds because the outbox is the LATENCY of a local write reaching the
/// ring, and the ring's own exchange is a minute — a shorter pump would spend
/// wakeups for nothing, a longer one would show up as a write that "did not
/// take". The pump nudges the sync loop after any append, so the round that
/// carries the write starts here rather than at the next sixty-second tick.
pub const RAIL_KV_PUMP_INTERVAL: Duration = Duration::from_secs(2);

/// How many of this node's own ops may stand above its last seal before the
/// pump seals again.
///
/// ONE constant for every namespace the daemon owns, KV and measurements alike
/// (ARCH §10.6). Two thousand ops is roughly 1.2 MB of journal at the measured
/// ~594-byte line, held by every node in the mesh — small enough that a seal is
/// rare (a household writes on the order of 3,500 ops a year) and large enough
/// that the seal's own cost, one admission plus one snapshot, is amortised over
/// a long stretch of ordinary writes.
pub const SEAL_AFTER_OWN_OPS: usize = 2_000;

/// How many outbox rows one tick takes.
///
/// A bound, not a throughput knob: rows left behind are taken on the next tick
/// two seconds later, and `outbox_take` is non-destructive so nothing is lost
/// by stopping early. It exists so a burst of writes cannot make one tick hold
/// the journal's writer lock for an unbounded stretch.
const OUTBOX_DRAIN_LIMIT: usize = 256;

/// What one [`pump_once`] did. Returned rather than only logged so a test can
/// assert on the mechanism instead of on log lines.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PumpOutcome {
    /// Queued writes signed onto a journal.
    pub appended: usize,
    /// Queued writes LEFT IN THE OUTBOX because this node is not in a roster
    /// yet. Not a failure — see [`pump_once`].
    pub deferred: usize,
    /// Queued writes dropped because the rail will never accept them. Acked so
    /// they cannot block the namespace behind them, and every one of them
    /// carries a `warn` naming the refusal.
    pub refused: usize,
    /// Namespaces sealed this tick.
    pub sealed: usize,
    /// Rows re-appended as the snapshot behind those seals.
    pub snapshot_rows: usize,
}

/// Handle to the spawned pump. Aborts the task on drop, matching
/// [`RingSyncHandle`](crate::ring_sync::RingSyncHandle) so the daemon tears
/// both down the same way.
pub struct RailKvPumpHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for RailKvPumpHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// Spawn the outbox pump. Call once per daemon start.
///
/// The FIRST thing it does is rebuild the projection from every journal on
/// disk, before any write is drained. In production `MeshStore` is
/// `in_memory()`, so what this node holds after a restart is nothing at all
/// until the fold is run — the journal is the durable half and the store is
/// derived from it, which is only true if something derives it.
///
/// `nudge` is [`crate::ring_sync`]'s wake-up. It is notified after any
/// successful append, so a local write reaches peers on a round that starts now
/// rather than up to sixty seconds later. Same wire, same sender: the pump does
/// not talk to a peer, it asks the one replication path to run.
pub fn spawn_rail_kv_pump(
    app_state: AppState,
    interval: Duration,
    nudge: Arc<Notify>,
) -> RailKvPumpHandle {
    let task = tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            seal_after_own_ops = SEAL_AFTER_OWN_OPS,
            "rail kv pump: started"
        );
        project_all_on_disk(&app_state).await;
        loop {
            let out = pump_once(&app_state).await;
            if out.appended > 0 {
                nudge.notify_one();
                debug!(
                    appended = out.appended,
                    deferred = out.deferred,
                    refused = out.refused,
                    sealed = out.sealed,
                    snapshot_rows = out.snapshot_rows,
                    "rail kv pump: tick, ring sync nudged"
                );
            }
            tokio::time::sleep(interval).await;
        }
    });
    RailKvPumpHandle { _task: task }
}

/// One drain of the outbox onto the ring, plus the seal check for every
/// namespace this node writes.
///
/// Three verdicts per row, and they are kept apart (ARCH §18.2, §18.3):
///
/// - **Appended.** The row is acked.
/// - **Deferred.** [`RailError::NotInRoster`] — this node is not in a mesh yet,
///   so nothing it signed would be readable to anyone. The row STAYS in the
///   outbox and travels the moment membership exists, which is the same
///   reasoning `mesh_http`'s measurement publish already applies to the same
///   condition. Logged once per namespace per tick at `debug`, because a solo
///   daemon is a normal daemon and this would otherwise be a warning every two
///   seconds forever.
/// - **Refused.** Anything else — a payload the rail cannot carry, a journal
///   that will not open. The row is acked WITH a `warn` naming the sentence:
///   never a silent drop, and never a retry loop against a rail that has
///   already said no.
pub async fn pump_once(app_state: &AppState) -> PumpOutcome {
    let mut out = PumpOutcome::default();
    let Some(rail) = app_state.ring_rail() else {
        return out;
    };
    let store = Arc::clone(&app_state.inner.mesh_store);
    let queued = match store.outbox_take(OUTBOX_DRAIN_LIMIT) {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "rail kv pump: the outbox could not be read");
            return out;
        }
    };

    // Grouped so the roster is read once per namespace and the seal check runs
    // once per namespace, not once per row. `BTreeMap` keeps the order a
    // function of the namespace set rather than of hash iteration.
    let mut by_namespace: BTreeMap<String, Vec<Outboxed>> = BTreeMap::new();
    for row in queued {
        by_namespace
            .entry(row.app_id.clone())
            .or_default()
            .push(row);
    }

    for (namespace, rows) in by_namespace {
        let journal = match rail.journal(&namespace) {
            Ok(j) => j,
            Err(e) => {
                // The namespace itself is unusable, so every row for it is
                // refused rather than left to be retried forever.
                let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
                warn!(namespace, error = %e, queued = ids.len(),
                      "rail kv pump: this namespace has no journal, so its queued writes were dropped");
                out.refused += ids.len();
                ack(&store, &ids);
                continue;
            }
        };
        // THE door (ARCH §10.6). For these namespaces it answers from
        // membership, because `MeshRosterSource::install` declared them.
        let roster = match rail.roster(&journal).await {
            Ok(r) => r,
            Err(e) => {
                // Left queued: an unreadable roster is a condition that heals,
                // and dropping the writes would make it permanent.
                warn!(namespace, error = %e, queued = rows.len(),
                      "rail kv pump: the roster is unreadable, so these writes stayed queued");
                out.deferred += rows.len();
                continue;
            }
        };

        let mut acked: Vec<i64> = Vec::new();
        let mut appended_here = 0usize;
        let mut not_in_roster = false;
        for row in &rows {
            if not_in_roster {
                out.deferred += 1;
                continue;
            }
            let op = &row.op;
            let payload = match rail_kv::to_payload(&op.key, op.value.as_deref(), op.t) {
                Ok(p) => p,
                Err(e) => {
                    warn!(namespace, key = %op.key, error = %e,
                          "rail kv pump: this write cannot travel on the rail and was dropped");
                    out.refused += 1;
                    acked.push(row.id);
                    continue;
                }
            };
            match journal.append(RailAct::Record { payload }, rail.signer(), &roster) {
                Ok(appended) => {
                    debug!(
                        namespace,
                        key = %op.key,
                        t = op.t,
                        // The one spelling of "this is a tombstone" — the
                        // outbox row carries no second one (ARCH §10.6).
                        deleted = op.value.is_none(),
                        id = %appended.id,
                        seq = appended.kind.seq,
                        "rail kv pump: appended a local write"
                    );
                    acked.push(row.id);
                    appended_here += 1;
                }
                Err(RailError::NotInRoster { actor, .. }) => {
                    not_in_roster = true;
                    out.deferred += 1;
                    debug!(
                        namespace,
                        actor = %actor,
                        queued = rows.len(),
                        "rail kv pump: this node is in no roster for this namespace yet, so its \
                         writes stay queued and travel once membership exists"
                    );
                }
                Err(e) => {
                    warn!(namespace, key = %op.key, error = %e,
                          "rail kv pump: the rail refused this write, which was dropped");
                    out.refused += 1;
                    acked.push(row.id);
                }
            }
        }
        ack(&store, &acked);
        out.appended += appended_here;

        if appended_here > 0 {
            seal_if_due(app_state, &rail, &journal, &roster, &mut out).await;
        }
    }

    // `mesh-measurements` never enters the outbox — it is gossip-excluded, and
    // its acts are published by `POST /v1/mesh/measurements` straight onto the
    // journal. So its journal grows with nothing above draining it, and the
    // seal check has to be reached some other way. Here is that way, and the
    // constant is the same one (ARCH §10.6).
    if let Ok(journal) = rail.journal(MEASUREMENTS_NAMESPACE) {
        if let Ok(roster) = rail.roster(&journal).await {
            seal_if_due(app_state, &rail, &journal, &roster, &mut out).await;
        }
    }

    out
}

fn ack(store: &MeshStore, ids: &[i64]) {
    if ids.is_empty() {
        return;
    }
    if let Err(e) = store.outbox_ack(ids) {
        warn!(error = %e, rows = ids.len(), "rail kv pump: the outbox ack failed");
    }
}

const MEASUREMENTS_NAMESPACE: &str = sovereign_core::mesh_measurements::MEASUREMENTS_APP_ID;

/// Seal and snapshot this namespace if this node's own history above its last
/// seal has passed [`SEAL_AFTER_OWN_OPS`].
///
/// # The cheap gate is deliberate (ARCH §9.5)
///
/// The count that decides is "own ops at or above my authenticated floor",
/// which needs an admission — one Ed25519 verify per line, 38.4 µs/op measured.
/// Paying that on every tick of a two-second loop would be a probe riding the
/// resource it monitors. So the raw line count comes first: the ops on disk
/// signed by us, parsed but not verified. Admitted-above-floor can never exceed
/// it (admitted is a subset of held, above-floor a subset of that), so a cheap
/// count under the threshold is PROOF the expensive one is too. The bound is
/// one-sided, so the gate can only ever cost an extra admission — never miss a
/// seal.
async fn seal_if_due(
    app_state: &AppState,
    rail: &RingRail,
    journal: &RingJournal,
    roster: &Roster,
    out: &mut PumpOutcome,
) {
    let namespace = journal.namespace();
    let mine = rail.signer().actor();

    let held = match journal.read() {
        Ok((ops, _)) => ops,
        Err(e) => {
            warn!(namespace, error = %e, "rail kv pump: the journal could not be read for the seal check");
            return;
        }
    };
    let own_lines = held.iter().filter(|o| o.actor == mine).count();
    if own_lines < SEAL_AFTER_OWN_OPS {
        return;
    }

    let admission = match journal.admit(roster, &Ed25519Verifier) {
        Ok(a) => a,
        Err(e) => {
            warn!(namespace, error = %e, "rail kv pump: the journal could not be admitted for the seal check");
            return;
        }
    };
    let floor = admission.floors.get(&mine).copied().unwrap_or(0);
    // `ops`, not `applied()`: a seal carries no payload and a voided op is
    // still a line on disk, and what this counts is how much of OUR history a
    // prune would still be holding.
    let own_above_floor = admission
        .ops
        .iter()
        .filter(|o| o.actor == mine && o.seq >= floor)
        .count();
    if own_above_floor < SEAL_AFTER_OWN_OPS {
        debug!(
            namespace,
            own_lines,
            own_above_floor,
            floor,
            threshold = SEAL_AFTER_OWN_OPS,
            "rail kv pump: the cheap line count cleared the bar and the admitted count did not"
        );
        return;
    }

    let sealed = match journal.seal(rail.signer(), roster, &Ed25519Verifier) {
        Ok(s) => s,
        Err(e) => {
            warn!(namespace, error = %e, "rail kv pump: the seal was refused, so nothing was retired");
            return;
        }
    };
    out.sealed += 1;
    match &sealed.retired {
        Ok(done) => info!(
            namespace,
            own_above_floor,
            removed = done.removed,
            kept = done.kept,
            "rail kv pump: sealed the daemon's own namespace"
        ),
        // Already warned by `RingJournal::seal`. The snapshot still runs: the
        // seal is on disk, so a peer WILL prune to the floor it names, and the
        // live set has to be above that floor whether or not our own prune
        // happened.
        Err(_) => {}
    }

    // The seal's own seq IS the new floor, and the mark that closes the
    // snapshot names it — taken from the act that was written rather than
    // re-read from a fresh admission, which would be a second answer to "what
    // did we just seal at" (ARCH §10.6).
    out.snapshot_rows += snapshot(app_state, rail, journal, roster, sealed.op.kind.seq).await;
}

/// Re-append this node's live rows above the floor its seal just set.
///
/// **Without this a seal is a delete.** The floor retires every line below it,
/// so a key whose only write is down there stops existing on every node that
/// admits the seal. The snapshot puts the LIVE set back above the floor,
/// carrying each row's ORIGINAL write time so the fold keeps it exactly where
/// its author left it — `a_snapshot_re_append_keeps_its_lww_position`
/// (commonwealth-state) is the pin, and it is why `t` is on the payload rather
/// than read off the journal line.
///
/// Only rows this node ORIGINATED. A peer's row is above that peer's floor, not
/// ours; re-appending it would put a second author's name on it and make every
/// node that snapshots last the author of the whole ring.
///
/// # It ends with a mark, and that is what makes the seal say "all of it"
///
/// The last act appended is `rail_kv::snapshot_mark(floor)`. A reader holding
/// it, with no hole in this actor's run above the floor, holds every row of the
/// snapshot — and may then retire every row of ours it holds that the snapshot
/// does not name, which is how a tombstone we published keeps travelling past
/// the seal that retired it (module docs). The mark is written LAST for exactly
/// that reason, and a snapshot whose mark could not be appended claims nothing
/// rather than claiming a truncated set: the failure direction is always "do
/// not retire" (ARCH §18.3).
///
/// It is one act on the journal per seal — 2,000 ops apart — and it is NOT
/// counted in `snapshot_rows`, which stays what it says: live rows re-appended.
async fn snapshot(
    app_state: &AppState,
    rail: &RingRail,
    journal: &RingJournal,
    roster: &Roster,
    floor: u64,
) -> usize {
    let namespace = journal.namespace();

    if namespace == MEASUREMENTS_NAMESPACE {
        // Not KV-shaped, and its live set is not in the store — it is the
        // local file, which is the authoritative copy. `republish` is already
        // idempotent by content, so it re-appends exactly what the seal
        // retired and nothing else. This is the fix for the seal/republish
        // interaction found 2026-09-08: republish AT the seal, not at the next
        // boot, or the window between them is a ring with no measurements in
        // it.
        let file = sovereign_core::mesh_measurements::load();
        let done =
            crate::measurements_rail::republish(journal, rail.signer(), roster, file.records());
        info!(
            namespace,
            appended = done.appended,
            already_held = done.already_held,
            withheld = done.withheld,
            "rail kv pump: snapshotted the local measurement history above the new floor"
        );
        return done.appended;
    }

    let self_id = app_state.self_node_id();
    let rows = match app_state.inner.mesh_store.scan(namespace, "") {
        Ok(r) => r,
        Err(e) => {
            warn!(namespace, error = %e,
                  "rail kv pump: the live set could not be read, so the seal retired rows nothing replaced");
            return 0;
        }
    };
    let mut appended = 0usize;
    let mut skipped = 0usize;
    for row in rows {
        if row.origin != self_id {
            skipped += 1;
            continue;
        }
        let payload = match rail_kv::to_payload(&row.key, Some(&row.value), row.timestamp) {
            Ok(p) => p,
            Err(e) => {
                warn!(namespace, key = %row.key, error = %e,
                      "rail kv pump: a live row could not be snapshotted and is now below the floor");
                continue;
            }
        };
        match journal.append(RailAct::Record { payload }, rail.signer(), roster) {
            Ok(_) => appended += 1,
            Err(e) => warn!(namespace, key = %row.key, error = %e,
                            "rail kv pump: a live row could not be snapshotted and is now below the floor"),
        }
    }
    match rail_kv::snapshot_mark(floor)
        .map_err(|e| e.to_string())
        .and_then(|payload| {
            journal
                .append(RailAct::Record { payload }, rail.signer(), roster)
                .map_err(|e| e.to_string())
        }) {
        Ok(_) => info!(
            namespace,
            appended,
            peers_rows_skipped = skipped,
            floor,
            "rail kv pump: snapshotted this node's live rows above the new floor, and closed it"
        ),
        // The rows are on the journal and every one of them still projects.
        // What is lost is the CLAIM that they are all of them, so no peer
        // retires anything of ours until the next seal marks one.
        Err(e) => warn!(
            namespace,
            appended,
            floor,
            error = %e,
            "rail kv pump: the snapshot could not be closed, so peers will keep \
             whatever of ours they already hold"
        ),
    }
    appended
}

// ── The receive half: journal → store ────────────────────────

/// Whether a namespace's acts are store writes.
///
/// The structural predicate, and it is a NAME check on purpose. The KV fold's
/// own parse failure would work as a predicate — a measurement is `unreadable`
/// to it — but "unreadable" means "this build could not read a line", which is
/// a thing worth reporting, and `mesh-measurements` would report twenty-eight
/// of them a round for a namespace that is behaving perfectly. A count that
/// fires when nothing is wrong stops being read. So the one namespace on this
/// rail with a different vocabulary is named, and what the fold counts stays a
/// real fact (ARCH §18.3).
///
/// It is also the honest direction of the dependency: this module knows both
/// vocabularies, and `rail_kv` knows only its own.
pub fn is_kv_namespace(namespace: &str) -> bool {
    namespace != MEASUREMENTS_NAMESPACE
}

/// Fold one namespace's journal and apply it to the store.
///
/// The receive side of the whole mechanism. `MeshStore` holds what the fold of
/// the admitted ops says it should hold — not what a peer sent it — so a row
/// arrives with the origin the ROSTER places its signature at, never one the
/// sender supplied (ARCH §18.1).
///
/// Returns the number of store rows this fold moved — merged, tombstoned,
/// retired by a sealed actor's live set, or expired past the namespace's
/// retention window — or `None` when the namespace was not projected at all —
/// a measurements ring, a journal that would not admit, or a namespace
/// `apply_projection` refuses on privacy grounds. The two are different facts
/// and a `0` for both would hide the second (ARCH §18.2).
pub async fn project_namespace(
    app_state: &AppState,
    rail: &RingRail,
    journal: &RingJournal,
) -> Option<usize> {
    let namespace = journal.namespace().to_string();
    if !is_kv_namespace(&namespace) {
        debug!(
            namespace = %namespace,
            "rail kv pump: not a store namespace, skipped rather than counted unreadable"
        );
        return None;
    }
    let roster = match rail.roster(journal).await {
        Ok(r) => r,
        Err(e) => {
            warn!(namespace = %namespace, error = %e, "rail kv pump: the roster is unreadable, nothing projected");
            return None;
        }
    };
    let admission = match journal.admit(&roster, &Ed25519Verifier) {
        Ok(a) => a,
        Err(e) => {
            warn!(namespace = %namespace, error = %e, "rail kv pump: the journal would not admit, nothing projected");
            return None;
        }
    };
    let projection = rail_kv::project(&admission);
    if projection.unreadable > 0 {
        warn!(
            namespace = %namespace,
            unreadable = projection.unreadable,
            "rail kv pump: acts on a store namespace that this build cannot read as store writes"
        );
    }
    // The actor → node mapping comes from the SAME membership derivation the
    // roster above came from, so a key that admitted an op is a key that can
    // name its node (ARCH §10.6). An actor it cannot place is counted
    // `unattributed` by `apply_projection`, never given an invented origin.
    //
    // The whole `projection` goes in, not its rows: the sealed actors' live
    // sets are the same fold's second answer, and the store's own node id is
    // what keeps the reconciliation off rows this node has not put on the rail
    // yet. This module does no second pass — one call, one decision.
    let mesh_roster = MeshRoster::from_app_state(app_state).await;
    match app_state.inner.mesh_store.apply_projection(
        &namespace,
        &projection,
        |actor| mesh_roster.node_id_of(actor),
        app_state.self_node_id(),
    ) {
        Ok(applied) => {
            debug!(
                namespace = %namespace,
                rows = projection.rows.len(),
                sealed_actors = projection.sealed_actors.len(),
                merged = applied.merged,
                deleted = applied.deleted,
                reconciled = applied.reconciled,
                expired = applied.expired,
                withheld = applied.withheld,
                unattributed = applied.unattributed,
                gaps = admission.gaps.len(),
                "rail kv pump: projected a namespace into the store"
            );
            if applied.unattributed > 0 {
                warn!(
                    namespace = %namespace,
                    unattributed = applied.unattributed,
                    "rail kv pump: the roster and the journal disagree about who is in this ring"
                );
            }
            Some(applied.merged + applied.deleted + applied.reconciled + applied.expired)
        }
        Err(e) => {
            // The receiver-side privacy guard firing is not a bug in this
            // module — it is a peer having put a namespace on the ring that
            // never leaves a machine. Loud, and nothing is written.
            warn!(namespace = %namespace, error = %e, "rail kv pump: the store refused this namespace");
            None
        }
    }
}

/// Rebuild the projection for every namespace this node holds a journal for.
///
/// Run once at pump start. In production `MeshStore` is `in_memory()`, so
/// without this a restart loses every row the mesh ever agreed on and the node
/// re-learns them only as peers happen to re-send — which, on a digest
/// exchange that ships only what a peer LACKS, is never.
pub async fn project_all_on_disk(app_state: &AppState) -> usize {
    let Some(rail) = app_state.ring_rail() else {
        return 0;
    };
    let namespaces = match rail.namespaces() {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "rail kv pump: cannot enumerate namespaces, nothing projected at boot");
            return 0;
        }
    };
    let mut projected = 0usize;
    for namespace in &namespaces {
        let Ok(journal) = rail.journal(namespace) else {
            continue;
        };
        if project_namespace(app_state, &rail, &journal)
            .await
            .is_some()
        {
            projected += 1;
        }
    }
    if projected > 0 {
        info!(
            namespaces = namespaces.len(),
            projected, "rail kv pump: rebuilt the store from the journals on disk"
        );
    }
    projected
}

#[cfg(test)]
mod tests {
    //! The pump's own decisions. The two-node path — a write reaching a peer's
    //! store, a delete, a seal, an excluded namespace — is driven end to end
    //! against the real router in `crate::ring_sync::tests`.

    use super::*;
    use crate::ring_roster::tests::{member, mesh_of, pubkey_of};
    use crate::ring_roster::DAEMON_OWN_NAMESPACES;
    use commonwealth_core::ids::NodeId;
    use commonwealth_rail::SigningKey;

    /// **The declaration is checkable, and this is the check.**
    ///
    /// Two properties, both of which have already failed once in this
    /// workspace. The charset one is `wikipedia-newsworthy:tracked`: a colon is
    /// legal in an `app_id` and not in a ring namespace, and the mismatch was
    /// silent until something tried to open the directory. The exclusion one is
    /// the split between the six namespaces that reach the ring through the
    /// OUTBOX and the one that does not — `mesh-measurements` is
    /// gossip-excluded, publishes straight onto its journal from
    /// `POST /v1/mesh/measurements`, and would be refused by both the outbox
    /// and `apply_projection` if anything tried to route it through the store.
    #[tokio::test]
    async fn every_declared_namespace_is_one_the_rail_and_the_store_agree_about() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let me = NodeId::from_u128(1);
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(me, mesh_of(vec![member(me, "me", Some(pubkey_of(&key)))]));
        let rail = RingRail::new(dir.path(), Arc::new(key));

        // The charset check lives in `derive_roster`, so a namespace the rail
        // would refuse to open cannot be installed either.
        crate::ring_roster::MeshRosterSource::install(&rail, &state).unwrap();

        let mut seen = std::collections::BTreeSet::new();
        for ns in DAEMON_OWN_NAMESPACES {
            assert!(seen.insert(*ns), "{ns} is declared twice");
            assert_eq!(
                rail.roster_origin(ns),
                commonwealth_rail::RosterOrigin::Derived,
                "{ns} still reads a roster file"
            );
            assert_eq!(
                commonwealth_state::is_gossip_excluded(ns),
                ns == &MEASUREMENTS_NAMESPACE,
                "{ns}: a namespace on this list either rides the outbox or is \
                 the one that publishes straight onto its journal, and which \
                 one it is decides whether the store will carry it at all"
            );
            assert_eq!(
                is_kv_namespace(ns),
                ns != &MEASUREMENTS_NAMESPACE,
                "{ns}: the KV fold and the measurement vocabulary are the two \
                 shapes on this rail"
            );
        }
    }

    /// **A node in no mesh queues its writes; it does not lose them.**
    ///
    /// `NotInRoster` is the one refusal that is not a refusal — a solo daemon
    /// is a normal daemon, and its writes travel the moment it joins. Dropping
    /// them would make a legitimate condition permanent, and retrying an
    /// append the rail will never accept would be the other failure. The
    /// second half is the control: the same row, the same pump, one member
    /// added.
    #[tokio::test]
    async fn a_node_in_no_mesh_keeps_its_writes_queued_until_membership_exists() {
        const KV: &str = commonwealth_inference::INFERENCE_APP_ID;
        let key = SigningKey::from_bytes(&[2u8; 32]);
        let me = NodeId::from_u128(7);
        let dir = tempfile::tempdir().unwrap();
        // A mesh with nobody in it: this node cannot place its own key.
        let state = AppState::new(me, mesh_of(vec![]));
        let rail = Arc::new(RingRail::new(dir.path(), Arc::new(key.clone())));
        crate::ring_roster::MeshRosterSource::install(&rail, &state).unwrap();
        state.install_ring_rail(rail.clone());

        assert!(state
            .inner
            .mesh_store
            .set(KV, "plan", bytes::Bytes::from_static(b"v1"), me)
            .unwrap());

        let out = pump_once(&state).await;
        assert_eq!(
            (out.appended, out.deferred, out.refused),
            (0, 1, 0),
            "{out:?}"
        );
        assert_eq!(
            state.inner.mesh_store.outbox_len().unwrap(),
            1,
            "a deferred write stays queued"
        );

        // Membership arrives, and the same row goes out.
        state
            .inner
            .mesh
            .write()
            .await
            .members
            .insert(me, member(me, "me", Some(pubkey_of(&key))));
        let out = pump_once(&state).await;
        assert_eq!(
            (out.appended, out.deferred, out.refused),
            (1, 0, 0),
            "{out:?}"
        );
        assert_eq!(state.inner.mesh_store.outbox_len().unwrap(), 0);
    }
}
