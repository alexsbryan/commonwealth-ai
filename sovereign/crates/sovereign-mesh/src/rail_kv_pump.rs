// SPDX-License-Identifier: AGPL-3.0-or-later
//! The seal that keeps the daemon's own non-store journals — `mesh-measurements`
//! and `work` — from growing forever.
//!
//! The store's half moved out: cw-rails drains the mesh store's outbox onto
//! the journals, seals and snapshots the store namespaces, and folds admitted
//! ops back into its store (`commonwealth_rails::kv`, five-programs fp-77,
//! fp-109, fp-83). What stays here is the two planes whose writers are this
//! daemon's (five-programs-40).
//!
//! # Sealing: for the daemon's own rings, the daemon decides
//!
//! Rung 4a recorded that WHEN to seal is the operator's call. That holds for
//! an APP's ring: sealing forgets history, and an app's history is the app's.
//! It does not hold here. The namespaces in
//! `DAEMON_OWN_NAMESPACES` (retired: membership is now every ring's default roster, `crate::ring_roster`) are
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
//! **`work` is the exception to the sentence above, and deliberately not to
//! the list** (cw-lift 5d). The work plane's roster IS an app ring's — the
//! operator's `rings/work/roster.json` — because joining
//! `DAEMON_OWN_NAMESPACES` would flip it to `Derived` and orphan that file
//! silently (`refuse_derived_roster` hardcodes measurements, so nothing would
//! warn). But its journal is written on a cadence no person chose, by the
//! donor loop renewing every lease it holds, so leaving the seal unmade has
//! exactly the unbounded-growth consequence this section is about. It is
//! sealed here, and it stays off that list: two different questions, and the
//! list answers the other one.
//!
//! The three vocabularies are named by [`projector_for`], which is what
//! [`seal_if_due`] and [`snapshot`] both read. What a `work` seal re-appends is
//! [`live_work_acts`], and its bound is stated there.

use std::sync::Arc;
use std::time::Duration;

use crate::fabric::FabricPart;
use commonwealth_rail::{RailAct, Roster};
use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
use commonwealth_work::{ActorKey, UnitRef, WorkAct, WorkActKind};
use tracing::{debug, info, warn};

use crate::rail_port::RingRailPort;

/// How often the seal arms check their journals — cw-rails' own pump
/// interval (`commonwealth_rails::kv::PUMP_INTERVAL`), kept equal so the two
/// sealers run on one cadence.
pub const RAIL_KV_PUMP_INTERVAL: Duration = Duration::from_secs(2);

/// How many of this node's own ops may stand above its last seal before the
/// pump seals again — `rail_kv`'s one constant, which cw-rails' pump reads
/// too (fp-77), re-exported at its historical path.
pub use commonwealth_state::rail_kv::SEAL_AFTER_OWN_OPS;

/// What one [`seal_once`] did. Returned rather than only logged so a test can
/// assert on the mechanism instead of on log lines.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PumpOutcome {
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

/// Spawn the two seal arms nothing drains — `mesh-measurements` and `work`.
/// The KV drain and KV seal are cw-rails' (five-programs fp-77/fp-83; the two
/// arms stay here, five-programs-40).
pub fn spawn_plane_seal(fabric: Arc<FabricPart>, interval: Duration) -> RailKvPumpHandle {
    let task = tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            "rail kv pump: started the measurements and work seal arms only"
        );
        loop {
            let out = seal_once(&fabric).await;
            if out.sealed > 0 {
                debug!(
                    sealed = out.sealed,
                    snapshot_rows = out.snapshot_rows,
                    "rail kv pump: sealed a plane"
                );
            }
            tokio::time::sleep(interval).await;
        }
    });
    RailKvPumpHandle { _task: task }
}

/// One tick of the seal arms: the seal check for `mesh-measurements` and
/// `work`. A node with no ring rail seals nothing.
pub async fn seal_once(fabric: &FabricPart) -> PumpOutcome {
    let mut out = PumpOutcome::default();
    if let Some(rail) = fabric.ring_rail() {
        seal_planes(rail.as_ref(), &mut out).await;
    }
    out
}

/// The seal checks for the planes that never enter the outbox.
async fn seal_planes(rail: &dyn RingRailPort, out: &mut PumpOutcome) {
    // `mesh-measurements` never enters the outbox — it is gossip-excluded, and
    // its acts are published by `POST /v1/mesh/measurements` straight onto the
    // journal. So its journal grows with nothing above draining it, and the
    // seal check has to be reached some other way. Here is that way, and the
    // constant is the same one (ARCH §10.6).
    if let Ok(roster) = rail.roster(MEASUREMENTS_NAMESPACE).await {
        seal_if_due(rail, MEASUREMENTS_NAMESPACE, &roster, out).await;
    }

    // `work` never enters the outbox either, and for the same structural
    // reason: its acts are not store rows. They are appended straight onto the
    // journal by `POST /v1/rail/append` (which is what `svrn job submit` and
    // the donor loop both reach), so nothing above drains it and the seal
    // check has to be reached the same way measurements is. Same constant
    // (ARCH §10.6).
    //
    // Both `Err`s are silent on purpose and neither is a failure: a daemon
    // that has never hosted the work plane has no `work` directory, and one
    // whose operator has not written `rings/work/roster.json` has no roster —
    // and a namespace this node cannot append to is one it must not seal. The
    // failure direction is always "do not retire" (ARCH §18.3).
    if let Ok(roster) = rail.roster(WORK_NAMESPACE).await {
        seal_if_due(rail, WORK_NAMESPACE, &roster, out).await;
    }
}

pub const MEASUREMENTS_NAMESPACE: &str = oicp_types::measurements::MEASUREMENTS_APP_ID;

/// The work plane's namespace, taken from the crate that owns the vocabulary
/// rather than spelled again here (ARCH §10.6) — a second literal would be a
/// second answer to what this data is called, and the symptom would be a
/// silently empty fold rather than an error.
pub const WORK_NAMESPACE: &str = commonwealth_work::WORK_NAMESPACE;

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
    rail: &dyn RingRailPort,
    namespace: &str,
    roster: &Roster,
    out: &mut PumpOutcome,
) {
    // A store namespace's live set is cw-rails' store, so cw-rails seals it
    // (fp-83); a seal here would retire rows with nothing to put them back.
    if projector_for(namespace) == Some(Projector::Kv) {
        warn!(
            namespace,
            "rail kv pump: a store namespace is sealed by cw-rails, not here; nothing sealed"
        );
        return;
    }
    let mine = match rail.actor().await {
        Ok(a) => a,
        Err(e) => {
            warn!(namespace, error = %e, "rail kv pump: the port would not name this node's actor, so no seal check ran");
            return;
        }
    };

    let held = match rail.journal_read(namespace).await {
        Ok(ops) => ops,
        Err(e) => {
            warn!(namespace, error = %e, "rail kv pump: the journal could not be read for the seal check");
            return;
        }
    };
    let own_lines = held.iter().filter(|o| o.actor == mine).count();
    if own_lines < SEAL_AFTER_OWN_OPS {
        return;
    }

    let admission = match rail.journal_admit(namespace, roster).await {
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

    // The work plane's live set is a FOLD, and this journal is its only copy:
    // the KV snapshot re-reads the store and the measurements one re-reads the
    // local file, and `work` has neither. So it is captured HERE — from the
    // admission the floor check already built, and BEFORE the prune below
    // deletes the lines it is derived from. One admission, not a second
    // (ARCH §10.6).
    let live_work = match projector_for(namespace) {
        Some(Projector::Work) => {
            let projection = commonwealth_work::projection::fold(&admission);
            debug!(
                namespace,
                handoffs = projection.handoffs.len(),
                offers = projection.offers.len(),
                unreadable = projection.unreadable,
                "rail kv pump: captured the work plane's live set before the seal prunes it"
            );
            Some(projection)
        }
        _ => None,
    };

    let sealed = match rail.journal_seal(namespace, roster).await {
        Ok(s) => s,
        Err(e) => {
            warn!(namespace, error = %e, "rail kv pump: the seal was refused, so nothing was retired");
            return;
        }
    };
    out.sealed += 1;
    let (sealed_op, retired) = sealed;
    match &retired {
        Ok(done) => info!(
            namespace,
            own_above_floor,
            removed = done.removed,
            kept = done.kept,
            "rail kv pump: sealed the daemon's own namespace"
        ),
        // Already warned by the seal. The snapshot still runs: the
        // seal is on disk, so a peer WILL prune to the floor it names, and the
        // live set has to be above that floor whether or not our own prune
        // happened.
        Err(_) => {}
    }

    // The seal's own seq IS the new floor, and the mark that closes the
    // snapshot names it — taken from the act that was written rather than
    // re-read from a fresh admission, which would be a second answer to "what
    // did we just seal at" (ARCH §10.6).
    out.snapshot_rows += snapshot(
        rail,
        namespace,
        roster,
        sealed_op.kind.seq,
        live_work.as_ref(),
    )
    .await;
}

/// Re-append this node's live set above the floor its seal just set.
///
/// **Without this a seal is a delete.** The floor retires every line below it,
/// so anything whose only line is down there stops existing on every node that
/// admits the seal. Each plane puts its LIVE set back from wherever that set
/// lives: measurements from the local file, work from the pre-seal fold.
async fn snapshot(
    rail: &dyn RingRailPort,
    namespace: &str,
    roster: &Roster,
    floor: u64,
    live_work: Option<&WorkProjection>,
) -> usize {
    // ONE decider for which vocabulary this namespace speaks (ARCH §10.6) —
    // the same selector the seal check reads, rather than a second set of
    // name comparisons that could drift from it.
    match projector_for(namespace) {
        None => {
            // Measurements. Not KV-shaped, and its live set is not in the
            // store — it is the local file, which is the authoritative copy.
            // `republish` is already idempotent by content, so it re-appends
            // exactly what the seal retired and nothing else. This is the fix
            // for the seal/republish interaction found 2026-09-08: republish
            // AT the seal, not at the next boot, or the window between them is
            // a ring with no measurements in it.
            let file = crate::mesh_measurements::load();
            let done = crate::measurements_rail::republish(rail, roster, file.records()).await;
            info!(
                namespace,
                appended = done.appended,
                already_held = done.already_held,
                withheld = done.withheld,
                "rail kv pump: snapshotted the local measurement history above the new floor"
            );
            return done.appended;
        }
        Some(Projector::Work) => snapshot_work(rail, namespace, roster, floor, live_work).await,
        // Refused before the seal, in `seal_if_due`: a store namespace's live
        // set is cw-rails' store, and cw-rails seals it.
        Some(Projector::Kv) => 0,
    }
}

/// Re-append this node's live work acts above the floor its seal just set.
///
/// **The work plane's answer to "without this a seal is a delete".** The unit
/// this node is running right now is a `Lease` on the journal and nowhere
/// else; retire it and every node — including this one — reads the unit as
/// queued again and hands it to somebody else while it is still running. The
/// offer goes back for the same reason: an offer is the latest admitted
/// `Offer` per actor, so a seal that retires ours takes this node out of the
/// cohort until it happens to publish another.
///
/// `live_work` is the fold [`seal_if_due`] captured BEFORE the prune. It is a
/// parameter rather than a re-fold here because by the time this runs the
/// lines it would be folded from are gone — the difference from the KV and
/// measurements arms, which both re-read a copy that lives somewhere else.
///
/// # What it carries, and what it deliberately does not
///
/// Only acts this node AUTHORED and still holds, which is the KV snapshot's
/// rule verbatim: a seal retires only the sealer's own lines below its own
/// floor, so a peer's `Submit` is untouched and re-appending one would put our
/// name on another actor's work.
///
/// A lapsed lease is left lapsed. `status_at` is the one place expiry is
/// decided, and a snapshot that re-appended a lease the fold had already given
/// back to the queue would be this module inventing a second answer to it.
///
/// **There is no snapshot mark.** `rail_kv::snapshot_mark` closes a KV
/// snapshot so a peer may retire every other row of that actor's, and the
/// reconciliation it authorises is `MeshStore::apply_projection`'s — a store
/// the work plane never touches. Appending one here would be a KV act on a
/// work journal: one more `unreadable` on every node, claiming nothing.
///
/// **What a seal past this point still costs**, named rather than left to be
/// discovered: this node's own `Submit`s and its own `Complete`/`Fail` reports
/// are below the floor too, and neither is re-appended. A handoff we submitted
/// ourselves loses its units, and a unit we completed reads as queued again.
/// Neither is reachable until this node writes [`SEAL_AFTER_OWN_OPS`] work acts
/// of its own, and both want the live set widened rather than this rule bent —
/// the rung that adds a donor's completion history is the rung to do it in.
async fn snapshot_work(
    rail: &dyn RingRailPort,
    namespace: &str,
    roster: &Roster,
    floor: u64,
    live_work: Option<&WorkProjection>,
) -> usize {
    let Some(projection) = live_work else {
        // Unreachable by construction — `seal_if_due` folds exactly when
        // `projector_for` says `Work` — and loud rather than a silent zero,
        // because the silent zero is a queue that forgot what it was running
        // (ARCH §18.3).
        warn!(
            namespace,
            "rail kv pump: no pre-seal fold was captured, so the seal retired this node's live \
             leases and nothing replaced them"
        );
        return 0;
    };
    let mine = match rail.actor().await {
        Ok(a) => a,
        Err(e) => {
            warn!(namespace, error = %e,
                  "rail kv pump: the port would not name this node's actor, so its live leases \
                   were not snapshotted");
            return 0;
        }
    };
    let mine = match ActorKey::parse(&mine) {
        Ok(a) => a,
        Err(e) => {
            warn!(namespace, error = %e,
                  "rail kv pump: this node's own actor key is not one the work plane can read, so \
                   its live leases were not snapshotted");
            return 0;
        }
    };
    let now_ms = commonwealth_core::clock::unix_now_millis();

    let acts = live_work_acts(projection, &mine, now_ms);
    // Counted from the acts rather than from where the offer was pushed: a
    // count that depends on a position in a vector is a count that a later
    // reorder makes wrong without failing.
    let offers = acts
        .iter()
        .filter(|a| a.kind() == WorkActKind::Offer)
        .count();

    let mut appended = 0usize;
    for act in &acts {
        if append_work_act(rail, namespace, roster, act).await {
            appended += 1;
        }
    }
    info!(
        namespace,
        appended,
        offers,
        leases = acts.len() - offers,
        floor,
        "rail kv pump: snapshotted this node's live leases and its offer above the new floor"
    );
    appended
}

/// Sign one work act onto the journal, reporting whether it landed.
///
/// A refusal is a `warn` naming the sentence and the loop keeps going, which
/// is the same failure direction the KV snapshot takes on a row it cannot
/// re-append: one act that will not travel must not cost the rest of the live
/// set the floor it was about to be lifted above (ARCH §18.3).
async fn append_work_act(
    rail: &dyn RingRailPort,
    namespace: &str,
    roster: &Roster,
    act: &WorkAct,
) -> bool {
    match commonwealth_work::to_payload(act).and_then(|payload| Ok(RailAct::Record { payload })) {
        Ok(rail_act) => match rail.journal_append(namespace, rail_act, roster).await {
            Ok(_) => true,
            Err(e) => {
                warn!(namespace, act = %act.kind(), error = %e,
                      "rail kv pump: a live work act could not be snapshotted and is now below the \
                       floor");
                false
            }
        },
        Err(e) => {
            warn!(namespace, act = %act.kind(), error = %e,
                  "rail kv pump: a live work act could not be snapshotted and is now below the \
                   floor");
            false
        }
    }
}

/// The work acts this node still holds at `now_ms` — the live set a seal must
/// put back above its floor.
///
/// Two rules, and both are borrowed rather than invented:
///
/// - **Only what this node AUTHORED.** The KV snapshot's rule verbatim. A
///   seal retires only the sealer's own lines below its own floor, so a peer's
///   `Submit` is untouched — and re-appending one would put our name on
///   another actor's work.
/// - **A lapsed lease is left lapsed.** `ProjectedUnit::status_at` is the ONE
///   place expiry is decided (`commonwealth_work::projection`), so this reads
///   it rather than comparing a deadline again; a snapshot that re-appended a
///   lease the fold had already handed back to the queue would be a second
///   answer to the same question (ARCH §10.6).
///
/// The offer comes first only for the reader's sake — the fold is a function
/// of admission's total order, not of the order these are appended in.
fn live_work_acts(projection: &WorkProjection, mine: &ActorKey, now_ms: u64) -> Vec<WorkAct> {
    let mut acts: Vec<WorkAct> = Vec::new();
    if let Some(offer) = projection.offers.get(mine) {
        acts.push(WorkAct::Offer(offer.clone()));
    }
    for (handoff, held) in &projection.handoffs {
        for (unit_hash, unit) in &held.units {
            let WorkUnitStatus::Leased { lessee, .. } = unit.status_at(now_ms) else {
                continue;
            };
            if &lessee == mine {
                acts.push(WorkAct::Lease(UnitRef {
                    handoff: *handoff,
                    unit_hash: unit_hash.clone(),
                }));
            }
        }
    }
    acts
}

/// Which vocabulary a namespace's acts are written in.
///
/// Two variants and an absence, because there are three answers and not two:
/// [`Kv`](Projector::Kv) is `commonwealth_state::rail_kv`, whose acts are
/// store writes; [`Work`](Projector::Work) is `commonwealth_work`, whose acts
/// are a queue and reach no store at all; and `None` is a namespace this
/// module folds nowhere — `mesh-measurements`, which `measurements_rail`
/// reads straight off its journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projector {
    /// `rail_kv`'s vocabulary: every act is a row in `commonwealth_state::MeshStore`.
    Kv,
    /// `commonwealth_work`'s vocabulary: every act is a move in the work
    /// plane's queue, folded by `WorkProjection::fold` and never stored.
    Work,
}

/// Which fold a namespace's acts belong to, or `None` for one this module
/// does not fold.
///
/// **A NAME check, and that is the whole design** — this is
/// `is_kv_namespace`'s reasoning, unchanged, now that the rail carries three
/// vocabularies instead of two. Each fold's own parse failure would work as a
/// predicate — a measurement is `unreadable` to `rail_kv`, and so is a lease —
/// but "unreadable" means "this build could not read a line", which is a thing
/// worth reporting, and a namespace with a different vocabulary would report
/// every line of a ring that is behaving perfectly. `mesh-measurements` would
/// contribute twenty-eight a round; `work` is worse, because a donor renews
/// every live lease on a timer, so the count would rise with how well the work
/// plane is working. A count that fires when nothing is wrong stops being read.
/// So the namespaces with a different vocabulary are NAMED, and what each fold
/// counts stays a real fact (ARCH §18.3).
///
/// It is also the honest direction of the dependency: this module knows all
/// three vocabularies, and each of `rail_kv`, `commonwealth_work` and
/// `measurements_rail` knows only its own.
///
/// **Not keyed on
/// `DAEMON_OWN_NAMESPACES` (retired: membership is now every ring's default roster, `crate::ring_roster`).**
/// That list answers a different question — whose roster the daemon derives,
/// and which journals it seals on its own cadence — and an app ring is on
/// neither side of it. Keying the projector on membership of that list would
/// hand every app ring in the mesh a different projection than it has today,
/// which is not a rung's worth of blast radius. `work` is on this selector and
/// deliberately NOT on that list: joining it would flip the namespace's roster
/// to `Derived` and orphan the operator-written `roster.json`, with
/// `refuse_derived_roster` hardcoding measurements so nothing would warn
/// (`commonwealth_work`'s own `WORK_NAMESPACE` doc records the same).
pub fn projector_for(namespace: &str) -> Option<Projector> {
    match namespace {
        MEASUREMENTS_NAMESPACE => {
            debug!(
                namespace,
                "rail kv pump: a measurements namespace, folded by measurements_rail — skipped \
                 here rather than counted unreadable"
            );
            None
        }
        WORK_NAMESPACE => {
            debug!(
                namespace,
                "rail kv pump: the work plane's namespace, folded by commonwealth_work — skipped \
                 here rather than counted unreadable"
            );
            Some(Projector::Work)
        }
        _ => {
            debug!(
                namespace,
                "rail kv pump: a store namespace, folded by rail_kv"
            );
            Some(Projector::Kv)
        }
    }
}
