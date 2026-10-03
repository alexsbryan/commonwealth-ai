// SPDX-License-Identifier: AGPL-3.0-or-later
//! The donor loop — this node taking units off the `work` fold and running
//! them (cw-lift 5d). The one job-execution drive (FIVE_PROGRAMS §2c): it
//! moved here from the svrn daemon with pb-work-donor, because the journal
//! it folds and signs onto is cw-rails' own.
//!
//! # Where this sits
//!
//! `commonwealth_work` is the vocabulary, the fold and the one predicate, and
//! it has no I/O and no clock. This is the half that has both: a supervised
//! task beside [`crate::ring_sync`]'s round that folds the `work` journal
//! this process holds every [`DONOR_POLL_INTERVAL`], asks
//! [`may_take`](commonwealth_work::refusal::may_take) about each queued unit,
//! and for the ones it may take appends a `Lease`, runs the unit, heartbeats a
//! `Renew`, and appends a `Complete` or a `Fail`.
//!
//! `process:v1` runs here, in `commonwealth_work::process`. A kind a program
//! on this node runs in its own process (the svrn daemon's `ingest:v1`) runs
//! through that program's execute origin ([`origin`]): registered in the
//! origin table as `Admit::Local`, found by the listing, forwarded each unit.
//!
//! # It is not a fourth sender of replicated state
//!
//! Every act it writes goes onto the LOCAL journal through
//! `RingJournal::append` — the same call the append door makes — and reaches
//! peers through the one exchange that already exists (the ring round's
//! `/internal/ring/sync`), woken by `ring_nudge`. This module opens no
//! socket to a peer, and `replication_sender_census` is what keeps that true
//! rather than this sentence.
//!
//! # Consent, and what it is not
//!
//! A unit runs here only when all of these hold, and every one of them is a
//! refusal in `commonwealth_work`'s single closed vocabulary rather than a
//! silent skip:
//!
//! 1. The submitter's `Submit.allowed` names this node, and this node's own
//!    `Offer.accept_from` names the submitter — the fold decides both.
//! 2. The offered kind resolves to a registered executor. Checked at BOOT
//!    (see [`resolve_offer`]) so the failure is a daemon that refuses to
//!    start, not a donor that leases units and fails every one of them.
//! 3. The host satisfies the unit's requirements — rev, os, arch,
//!    preconditions. Only the host can answer these, so this module
//!    constructs those refusals ([`UnmetRequirement`]) in the same enum.
//! 4. The operator is not at the keyboard, when the offer says to yield.
//!
//! **Consent is not isolation, and the boundary is now a thing rather than a
//! warning.** Until 2026-09-10 `process:v1` ran the submitter's argv as this
//! node's user with this node's filesystem and network, and the only wall was
//! `accept_from`. A unit now runs inside `commonwealth_work::sandbox` —
//! rootless container, no network, no capabilities, nothing mounted but its
//! own workdir — and what this node PROVIDES is whatever
//! [`Sandbox::probe`](commonwealth_work::sandbox::Sandbox::probe) could
//! actually find. A host with no runtime, or no declared image, provides
//! [`DONOR_ISOLATION`] and therefore offers no kind that demands more, which
//! is every kind that runs a stranger's argv. `accept` still defaults to
//! `nobody`; it is no longer the only thing standing there.
//!
//! # A lost lease cancels the unit
//!
//! Two donors reading one journal can both append a `Lease` for one unit
//! before either has seen the other's; the fold's total order picks a winner
//! and records the loser in `lost_leases`. The loser is *running the unit*,
//! so it has to find out — and the heartbeat is where it does. Every
//! `lease_interval_ms` the running unit's task re-folds the journal and asks
//! whether this node is still the lessee. If it is, it appends a `Renew`. If
//! it is not — lost to another donor, expired past `expires_at_ms`, or
//! already reported — it sets the [`JobContext`] cancellation flag, the
//! executor kills its process group, and **nothing is appended**: a report
//! from a non-lessee is exactly what the fold counts `unreadable`, and adding
//! to that count is not the same as reporting a fact.
//!
//! # Preemption is tier 1 only
//!
//! `should_yield_to_foreground` gates whether a new unit is TAKEN, not
//! whether a running one is killed. Tier 2 (suspend or kill in flight) is
//! named in the plan as H2 and deliberately not built: a unit killed
//! mid-flight burns an attempt on the submitter's behalf for a reason that
//! has nothing to do with the submitter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::ids::NodeId;
use commonwealth_work::act::{Completion, Failure, UnitRef, WorkAct};
use commonwealth_work::actor::ActorKey;
use commonwealth_work::executor::{subject_of, JobContext, JobError, JobExecutorRegistry};
use commonwealth_work::projection::LeaseState;
#[cfg(test)]
use commonwealth_work::projection::{lease_state, WorkProjection};
use commonwealth_work::refusal::{may_take, WorkRefusal};
use commonwealth_work::sandbox::Sandbox;
use oicp_types::{Isolation, JobKind, JobUnit, WorkOffer};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::config::{InvalidWorkOffer, WorkOfferSection};
use crate::RailsDaemon;

pub mod origin;
use origin::{OriginExecutor, Origins};

/// The tracing target for everything this module decides.
///
/// `commonwealth_work`'s own target for the refusals it constructs, so a
/// reader debugging "why is this donor taking nothing" turns on ONE filter
/// and sees both halves of the answer (ARCH §9.1). A second target here would
/// be dark for anybody who typed the obvious one.
pub const TRACE_TARGET: &str = commonwealth_work::TRACE_TARGET;

/// What a donor provides when it has NO boundary — a child in its own process
/// group, killed on timeout, and the donor's own user, filesystem and network.
///
/// It is the floor of the ladder rather than the answer: since 2026-09-10 the
/// answer is `Sandbox::provides()`, derived from a probe that has to find a
/// rootless runtime and a locally-present image before it will report
/// anything above this. The constant remains because it is the value that
/// probe FALLS BACK to, and because `ingest:v1` — which runs this daemon's own
/// code in-process — is covered by it.
///
/// It stays a constant and not a config key for the reason
/// `oicp_types::Isolation` refuses a `Default`: a donor that does not answer
/// how it isolates has not answered, and a config that could assert an
/// isolation the host cannot perform is §18.3's substitution in the one place
/// it would be least visible.
pub const DONOR_ISOLATION: Isolation = Isolation::Subprocess;

/// How often the fold is re-read for takeable units.
///
/// Five seconds, between `rail_kv_pump`'s two (the latency of a local write
/// reaching the ring) and `ring_sync`'s sixty (the ring's own convergence).
/// The unit of work here is a CI shard measured in minutes, so a faster poll
/// buys nothing and each one costs a journal admit.
pub const DONOR_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Subdirectory of the daemon's data dir the donor works under.
pub const DONOR_DIR: &str = "work-donor";

// The boot decision (`[work_offer]` against the registry), the credit and
// the journal's reads and writes are siblings, so this file stays out of
// ARCH §3.1's approach band.
mod credit;
mod journal;
mod offer;
use credit::credit_for;
use journal::{append, fold_now, still_ours};
use offer::registered_list;
pub use offer::{donor_registry, resolve_offer, OfferRefused};

// -----------------------------------------------------------------
// The loop
// -----------------------------------------------------------------

/// Handle to the supervised donor loop. Aborting it stops taking new units;
/// units already in flight are aborted with the task, and their leases lapse
/// on the fold rather than being reported — which is the same fact a donor
/// that lost power publishes, and the fold already handles it.
pub struct WorkDonorHandle {
    task: tokio::task::JoinHandle<()>,
}

impl WorkDonorHandle {
    /// Stop taking units at the next await point.
    pub fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for WorkDonorHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What the start decided once and every round reads: the section, the
/// boundary a unit runs in, what it provides, and where units work.
struct DonorSetup {
    section: WorkOfferSection,
    sandbox: Sandbox,
    provides: Isolation,
    os: String,
    arch: String,
    donor_root: PathBuf,
    interval: Duration,
}

/// Probe the boundary, check the section against cw-rails' own executors,
/// and spawn the supervised loop. `Ok(None)` is the inert section.
///
/// THE BOUNDARY IS PROBED ONCE, HERE, before anything is published. What
/// comes back is both how a unit will be run and what this node may say about
/// itself — one value, so the two cannot disagree (ARCH §10.6). A host with
/// no runtime or no declared image gets `Direct`, which provides
/// `Subprocess`, which offers no kind that runs a stranger's argv.
pub async fn spawn(daemon: Arc<RailsDaemon>) -> Result<Option<WorkDonorHandle>, OfferRefused> {
    let section = daemon.node.config.work_offer.clone();
    if section.kinds.is_empty() {
        debug!(target: TRACE_TARGET, "work donor: [work_offer] names no kinds, so this node donates nothing");
        return Ok(None);
    }
    let image = section.image.clone();
    let (sandbox, why) = match tokio::task::spawn_blocking(move || Sandbox::probe(image.as_deref()))
        .await
    {
        Ok(probed) => probed,
        Err(e) => {
            warn!(target: TRACE_TARGET, error = %e, "work donor: the boundary probe did not finish, so no boundary");
            (Sandbox::Direct, None)
        }
    };
    // Named at `info` when it worked and `warn` when it did not, because
    // "this node donates nothing" with no reason is the shape of a
    // misconfiguration nobody finds (ARCH §18.3, §9.1).
    match &why {
        Some(reason) => warn!(target: TRACE_TARGET, provides = ?sandbox.provides(), why = %reason,
                              "work donor: no boundary on this host, so it will offer no kind that needs one"),
        None => {
            info!(target: TRACE_TARGET, provides = ?sandbox.provides(), "work donor: boundary ready")
        }
    }
    // `platform` is the IMAGE's under a boundary and this host's without one
    // — a donor advertises where a unit RUNS.
    let (provides, (os, arch)) = (sandbox.provides(), sandbox.platform());
    // A misspelled kind or accept key refuses the start now, naming it.
    resolve_offer(
        &section,
        &donor_registry(sandbox.clone(), &[]),
        &os,
        &arch,
        provides,
    )?;
    let setup = Arc::new(DonorSetup {
        donor_root: daemon.node.data_dir.join(DONOR_DIR),
        section,
        sandbox,
        provides,
        os,
        arch,
        interval: DONOR_POLL_INTERVAL,
    });
    Ok(Some(WorkDonorHandle {
        task: tokio::spawn(supervise(daemon, setup)),
    }))
}

/// Crashes before the loop is parked instead of restarted.
pub const MAX_AUTO_RESTARTS: u32 = 5;

/// Keep a panic inside a unit from taking cw-rails with it: the loop runs as
/// its own task, a panic restarts it after a doubling backoff (1 s, capped at
/// 60 s), and the fifth parks it instead of pinning a restart loop. The svrn
/// daemon's supervisor held the same budget for this loop.
async fn supervise(daemon: Arc<RailsDaemon>, setup: Arc<DonorSetup>) {
    struct AbortOnDrop(tokio::task::JoinHandle<()>);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let mut crashes = 0u32;
    let mut backoff = Duration::from_secs(1);
    loop {
        let mut run = AbortOnDrop(tokio::spawn(donor_loop(
            Arc::clone(&daemon),
            Arc::clone(&setup),
        )));
        match (&mut run.0).await {
            Err(e) if e.is_panic() => {
                crashes += 1;
                if crashes >= MAX_AUTO_RESTARTS {
                    tracing::error!(target: TRACE_TARGET, crashes,
                                    "work donor: parked after repeated crashes — restart cw-rails to retry");
                    return;
                }
                tracing::error!(target: TRACE_TARGET, crashes, backoff_secs = backoff.as_secs(),
                                "work donor: the loop panicked — restarting");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
            _ => return,
        }
    }
}

async fn donor_loop(daemon: Arc<RailsDaemon>, setup: Arc<DonorSetup>) {
    info!(
        target: TRACE_TARGET,
        interval_ms = setup.interval.as_millis() as u64,
        "work donor: loop started"
    );
    // In-flight units live here rather than in a shared map: the loop is one
    // future, so the set IS the count `max_concurrent` is compared against
    // locally, and a `JoinSet` aborts everything it holds when the loop is
    // dropped — which is what makes the supervisor's restart clean.
    let mut running: JoinSet<()> = JoinSet::new();
    let mut origins = Origins::default();
    let mut current: Option<Current> = None;
    loop {
        while running.try_join_next().is_some() {}
        let found = origins.refresh(&daemon).await;
        let shape: Vec<(String, std::net::SocketAddr)> = found
            .iter()
            .map(|o| (o.slot().to_string(), o.addr()))
            .collect();
        // Re-resolved only when the origins change, so a steady node names
        // its offer once rather than every round.
        if current.as_ref().map(|c| &c.shape) != Some(&shape) {
            current = Some(Current::resolve(&daemon, &setup, &found, shape));
        }
        let taken = match current.as_ref() {
            Some(c) => match &c.offer {
                Some(offer) => take_round(&daemon, c, offer, &setup.donor_root, &mut running).await,
                None => 0,
            },
            None => 0,
        };
        if taken > 0 {
            debug!(target: TRACE_TARGET, taken, in_flight = running.len(), "work donor: leased");
        }
        tokio::time::sleep(setup.interval).await;
    }
}

/// The offer as the origins registered right now make it, and what every
/// unit taken under it runs through and is credited to.
struct Current {
    shape: Vec<(String, std::net::SocketAddr)>,
    registry: Arc<JobExecutorRegistry>,
    origins: BTreeMap<JobKind, Arc<OriginExecutor>>,
    offer: Option<WorkOffer>,
    /// The node's ONE roster identity (phase-b-39 fork 1): the registrant's
    /// node while a program on this node registers an origin, cw-rails' own
    /// id with none.
    credit_node: NodeId,
}

impl Current {
    fn resolve(
        daemon: &RailsDaemon,
        setup: &DonorSetup,
        found: &[Arc<OriginExecutor>],
        shape: Vec<(String, std::net::SocketAddr)>,
    ) -> Current {
        let registry = Arc::new(donor_registry(setup.sandbox.clone(), found));
        let offer = match resolve_offer(
            &setup.section,
            &registry,
            &setup.os,
            &setup.arch,
            setup.provides,
        ) {
            Ok(offer) => offer,
            // `spawn` resolved this section once already, so only a
            // regression reaches here; named, and nothing is offered.
            Err(e) => {
                warn!(target: TRACE_TARGET, error = %e, "work donor: the offer no longer resolves");
                None
            }
        };
        let credit_node = found
            .iter()
            .find_map(|o| o.credit_node())
            .unwrap_or(daemon.node.self_id);
        debug!(target: TRACE_TARGET, origins = found.len(), credit_node = %credit_node,
               "work donor: executors resolved");
        Current {
            shape,
            origins: found
                .iter()
                .map(|o| (o.kind().clone(), Arc::clone(o)))
                .collect(),
            registry,
            offer,
            credit_node,
        }
    }
}

/// One pass over the fold. Returns how many units this pass leased.
async fn take_round(
    daemon: &Arc<RailsDaemon>,
    current: &Current,
    offer: &WorkOffer,
    donor_root: &Path,
    running: &mut JoinSet<()>,
) -> usize {
    let registry = &current.registry;
    let Some(fold) = fold_now(daemon).await else {
        return 0;
    };
    let (proj, self_key, now_ms) = fold;

    // Tier 1 preemption: the operator is at the keyboard, so nothing NEW is
    // taken. Units already running are not touched — see the module docs.
    // The foreground is the svrn daemon's, published to `POST /v1/work/yield`
    // (phase-b-38 fork 3); this compares against the deadline it posted.
    if offer.yield_to_foreground {
        if let Some(until_ms) = daemon.work_yield.yielding_at(now_ms) {
            debug!(
                target: TRACE_TARGET,
                refusal = WorkRefusal::Yielding.id(),
                why = %WorkRefusal::Yielding,
                until_ms,
                now_ms,
                "work donor: not taking new units this round"
            );
            return 0;
        }
    }

    // Publish the offer when the rail does not already carry THIS one.
    //
    // Not "once at boot": the fold is the state, so the condition is a
    // comparison against it rather than a flag this process remembers. That
    // makes it self-healing — a config change, a re-signed key, a journal
    // compacted below the last offer, all republish on the next round — and
    // it costs one journal line rather than one every five seconds.
    // `Offer`'s own rule is latest-per-actor-wins, so a re-append is a
    // correction, not a duplicate.
    if proj.offers.get(&self_key) != Some(offer) {
        match append(daemon, &WorkAct::Offer(offer.clone())).await {
            Ok(()) => info!(
                target: TRACE_TARGET,
                kinds = %registered_list(&offer.kinds),
                "work donor: published this node's offer onto the `work` ring"
            ),
            Err(e) => warn!(
                target: TRACE_TARGET, error = %e,
                "work donor: the offer could not be published, so submitters cannot see it"
            ),
        }
    }

    let mut taken = 0usize;
    for unit_ref in proj.takeable_at(now_ms) {
        // TWO comparisons against ONE threshold, and they are not two
        // deciders (ARCH §10.6). `may_take` below compares `offer
        // .max_concurrent` against the leases the RAIL shows this node
        // holding; this compares it against the tasks THIS PROCESS has in
        // flight. They differ for exactly one round: a lease appended earlier
        // in this loop is not in `proj`, which was folded before it, so
        // without this guard one round could take `max_concurrent` units on
        // top of everything already running.
        if running.len() >= offer.max_concurrent as usize {
            let refusal = WorkRefusal::Concurrency {
                held: running.len() as u32,
                max: offer.max_concurrent,
            };
            debug!(
                target: TRACE_TARGET,
                refusal = refusal.id(),
                why = %refusal,
                "work donor: this round is full"
            );
            break;
        }
        // The rail's own half. `may_take` traces its own refusal.
        if may_take(&proj, &self_key, offer, &unit_ref, now_ms).is_err() {
            continue;
        }
        let Some(projected) = proj.unit(&unit_ref) else {
            continue;
        };
        let unit = projected.unit.clone();

        // The host's half — the three questions no signature over the rail
        // can answer, constructed in the SAME closed vocabulary.
        let Some(executor) = registry.resolve(&unit.kind) else {
            let e = JobError::NoExecutor {
                kind: unit.kind.clone(),
            };
            debug!(target: TRACE_TARGET, unit = %unit_ref.unit_hash, why = %e, "work donor: refused");
            continue;
        };
        // An execute origin answers its executor's pure check over loopback,
        // still before any lease; cw-rails' own executors answer in-process.
        let validated = match current.origins.get(&unit.kind) {
            Some(origin) => match origin.validate_remote(&unit).await {
                Ok(verdict) => verdict,
                Err(why) => {
                    warn!(target: TRACE_TARGET, unit = %unit_ref.unit_hash, slot = %origin.slot(),
                          why = %why, "work donor: the execute origin did not answer its check; not taken this round");
                    continue;
                }
            },
            None => executor.validate(&unit),
        };
        if let Err(refusal) = validated {
            trace_refusal(&unit_ref, &refusal);
            continue;
        }
        // Asked of the environment the EXECUTOR runs units in — under a
        // container boundary that is the image, not this host. Announced at
        // info: the submitter's side can only say "the host-side half is not
        // visible from here", so this line is the one place the reason is.
        if let Err(refusal) = executor.environment_satisfies(&unit) {
            info!(
                target: TRACE_TARGET,
                handoff = %unit_ref.handoff,
                unit = %unit_ref.unit_hash,
                refusal = refusal.id(),
                why = %refusal,
                "work donor: refused — the unit's preconditions are not met by the environment this node runs units in"
            );
            continue;
        }
        let workdir = match resolve_workdir(offer, &unit, donor_root).await {
            Ok(dir) => dir,
            Err(refusal) => {
                trace_refusal(&unit_ref, &refusal);
                continue;
            }
        };

        // Consent is settled; take it. The Lease goes on the LOCAL journal
        // and reaches peers on ring_sync's next round, which the nudge starts
        // now rather than at the next sixty-second tick.
        if let Err(e) = append(daemon, &WorkAct::Lease(unit_ref.clone())).await {
            warn!(target: TRACE_TARGET, unit = %unit_ref.unit_hash, error = %e,
                  "work donor: the lease could not be appended, so the unit was not started");
            continue;
        }
        let lease_interval_ms = executor.descriptor().lease_interval_ms;
        let daemon = Arc::clone(daemon);
        let self_key = self_key.clone();
        let executor = Arc::clone(&executor);
        let credit_node = current.credit_node;
        running.spawn(async move {
            run_unit(
                daemon,
                executor,
                unit,
                unit_ref,
                self_key,
                credit_node,
                workdir,
                lease_interval_ms,
            )
            .await;
        });
        taken += 1;
    }
    taken
}

fn trace_refusal(unit_ref: &UnitRef, refusal: &WorkRefusal) {
    debug!(
        target: TRACE_TARGET,
        handoff = %unit_ref.handoff,
        unit = %unit_ref.unit_hash,
        refusal = refusal.id(),
        why = %refusal,
        "work donor: refused by the host's own half"
    );
}

// -----------------------------------------------------------------
// The host's half of the predicate
// -----------------------------------------------------------------

// The checkouts block moved to a sibling file: inline, this file crossed
// ARCH §3.2's 1200-line ceiling (ARCH §3.1). The names are unchanged and the
// callers read the same.
mod checkout;
#[cfg(test)]
use checkout::{checkout_at, git, stable_repo_key};
use checkout::{repo_rev_of, resolve_workdir};

// -----------------------------------------------------------------
// One unit
// -----------------------------------------------------------------

/// Run one leased unit: heartbeat while it runs, report when it stops.
#[allow(clippy::too_many_arguments)]
async fn run_unit(
    daemon: Arc<RailsDaemon>,
    executor: Arc<dyn commonwealth_work::executor::JobExecutor>,
    unit: JobUnit,
    unit_ref: UnitRef,
    self_key: ActorKey,
    credit_node: NodeId,
    workdir: PathBuf,
    lease_interval_ms: u64,
) {
    let ctx = JobContext::new(&workdir);
    // The donor's own metal, measured the way `routes_inference` measures the
    // server's for `InferenceServed` — one `Instant` around the work, read
    // once. It spans the heartbeat loop because the heartbeat is what holds
    // the lease this unit occupies; it is time this node could not sell to
    // anybody else.
    let started = std::time::Instant::now();
    let outcome = {
        let fut = executor.execute(&unit, &ctx);
        tokio::pin!(fut);
        // A zero interval would be a busy loop; the executor's descriptor owns
        // the number and a zero there is a bug in the executor, not a licence
        // to spin.
        let period = Duration::from_millis(lease_interval_ms.max(1));
        let mut ticker = tokio::time::interval(period);
        // `Delay`, not the default `Burst`: a heartbeat that took longer than
        // its own period (a slow journal admit) must not then fire the ticks
        // it missed back-to-back, which would append a run of `Renew`s that
        // say nothing new.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // the first tick is immediate
                             // Once the lease is gone it does not come back, so the fold is not
                             // re-admitted every period for a process that is already dying.
        let mut cancelled = false;
        loop {
            tokio::select! {
                result = &mut fut => break result,
                _ = ticker.tick(), if !cancelled => {
                    match still_ours(&daemon, &unit_ref, &self_key).await {
                        LeaseState::Held => {
                            if let Err(e) = append(&daemon, &WorkAct::Renew(unit_ref.clone())).await {
                                warn!(target: TRACE_TARGET, unit = %unit_ref.unit_hash, error = %e,
                                      "work donor: a renew could not be appended — the lease will lapse");
                            }
                        }
                        LeaseState::Lost(why) => {
                            // THE cancellation. Set the flag, keep awaiting
                            // the future: the executor kills its process
                            // group and returns `Cancelled`, which is a
                            // verdict we deliberately do not publish below.
                            debug!(target: TRACE_TARGET, handoff = %unit_ref.handoff,
                                   unit = %unit_ref.unit_hash, why = %why,
                                   "work donor: lease lost — cancelling the unit in flight");
                            ctx.cancel();
                            cancelled = true;
                        }
                        LeaseState::Unknown => {
                            // The journal could not be read this tick. Do not
                            // cancel on it: a transient read failure is not
                            // evidence that somebody else holds the lease,
                            // and killing a half-hour shard on it would be
                            // the silent substitution §18.3 forbids.
                            debug!(target: TRACE_TARGET, unit = %unit_ref.unit_hash,
                                   "work donor: the fold was unreadable this heartbeat, holding");
                        }
                    }
                }
            }
        }
    };

    // A cancelled unit is one we no longer hold. Reporting it would be a
    // report from a non-lessee, which the fold counts `unreadable` — the
    // count is a real fact about a broken peer and must not be fed by this
    // node's own normal operation.
    if matches!(outcome, Err(JobError::Cancelled)) {
        debug!(
            target: TRACE_TARGET,
            handoff = %unit_ref.handoff,
            unit = %unit_ref.unit_hash,
            "work donor: cancelled unit reported nothing — the lease is somebody else's"
        );
        return;
    }

    let provenance = executor.attribution(&repo_rev_of(&unit, &workdir));
    let act = match outcome {
        Ok((outcome, result)) => WorkAct::Complete(Completion {
            handoff: unit_ref.handoff,
            unit_hash: unit_ref.unit_hash.clone(),
            outcome,
            result,
            provenance,
        }),
        Err(e) => WorkAct::Fail(Failure {
            handoff: unit_ref.handoff,
            unit_hash: unit_ref.unit_hash.clone(),
            outcome: e.judgement(subject_of(&unit)),
            provenance,
        }),
    };
    let wall_seconds = started.elapsed().as_secs_f64();
    match append(&daemon, &act).await {
        Ok(()) => {
            info!(
                target: TRACE_TARGET,
                handoff = %unit_ref.handoff,
                unit = %unit_ref.unit_hash,
                act = %act.kind(),
                wall_seconds,
                "work donor: reported"
            );
            // THE CREDIT (cw-lift 5h). Inside the `Ok` arm and nowhere else:
            // a report this node could not append is work the ring has no
            // record of, and crediting it would be the ledger disagreeing
            // with the journal it is supposed to be auditable against.
            match credit_for(&act, &unit, &self_key, wall_seconds) {
                Some(credit) => {
                    debug!(
                        target: TRACE_TARGET,
                        handoff = %unit_ref.handoff,
                        unit = %unit_ref.unit_hash,
                        wall_seconds,
                        credit_node = %credit_node,
                        "work donor: crediting this node's contribution ledger"
                    );
                    // The store's own emitter under the node's roster
                    // identity, the one the ledger door writes through; a
                    // store failure is logged by `record` itself.
                    commonwealth_state::ContributionEmitter::new(
                        daemon.kv.store.clone(),
                        credit_node,
                    )
                    .record(credit);
                }
                None => debug!(
                    target: TRACE_TARGET,
                    handoff = %unit_ref.handoff,
                    unit = %unit_ref.unit_hash,
                    act = %act.kind(),
                    "work donor: not a completion — nothing credited"
                ),
            }
        }
        Err(e) => warn!(
            target: TRACE_TARGET,
            unit = %unit_ref.unit_hash,
            error = %e,
            "work donor: the report could not be appended — the lease will lapse and the unit re-queue"
        ),
    }
}

#[cfg(test)]
mod tests;
