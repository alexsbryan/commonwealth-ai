// SPDX-License-Identifier: AGPL-3.0-or-later
//! The donor loop — this daemon taking units off the `work` fold and running
//! them (cw-lift 5d).
//!
//! # Where this sits
//!
//! `commonwealth_work` is the vocabulary, the fold and the one predicate, and
//! it has no I/O and no clock. This is the half that has both: a supervised
//! task beside [`crate::ring_sync`]'s and [`crate::rail_kv_pump`]'s that folds
//! the `work` namespace every [`DONOR_POLL_INTERVAL`], asks
//! [`may_take`](commonwealth_work::refusal::may_take) about each queued unit,
//! and for the ones it may take appends a `Lease`, runs the unit, heartbeats a
//! `Renew`, and appends a `Complete` or a `Fail`.
//!
//! # It is not a fourth sender of replicated state
//!
//! Every act it writes goes onto the LOCAL journal through
//! `RingJournal::append` — the same call `rail_kv_pump` makes for a store
//! write — and reaches peers through the one exchange that already exists
//! (`ring_sync`'s `/internal/ring/sync`). This module opens no socket, and
//! `replication_sender_census::every_sender_of_replicated_state_is_declared`
//! is what keeps that true rather than this sentence.
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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use commonwealth_rail::{Ed25519Verifier, RailAct, RingRail};
use commonwealth_work::act::{Completion, Failure, UnitRef, WorkAct};
use commonwealth_work::actor::ActorKey;
use sovereign_api::state::AppState;
// The named absence for a workdir that is not a checkout. Imported rather
// than re-spelled: this donor, the submitter and a lifted peer all have to
// name the same absence, and the comparability rule keys on it.
use commonwealth_work::attribution::ABSENT_REV;
use commonwealth_work::executor::{subject_of, JobContext, JobError, JobExecutorRegistry};
use commonwealth_work::projection::{lease_state, LeaseState, WorkProjection, WorkUnitStatus};
use commonwealth_work::refusal::{may_take, UnmetRequirement, WorkRefusal};
use commonwealth_work::sandbox::Sandbox;
use commonwealth_work::WORK_NAMESPACE;
use kernel_types::attribution::ComputeAttribution;
use kernel_types::quality::Precondition;
use kernel_types::Server;
// Through `sovereign_contracts`' re-export, not a direct dep on `oicp-types`
// (ARCH §8.3): this crate already links the contracts crate, and a second
// path to one logical type is how two versions of it end up in one binary.
use sovereign_contracts::oicp::{Isolation, JobKind, JobUnit, WorkOffer};
use sovereign_contracts::setup_config::{InvalidWorkOffer, WorkOfferSection};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::projects::{ProjectState, WatcherKind};
use crate::supervised_task::SupervisedTask;

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

// -----------------------------------------------------------------
// The boot decision
// -----------------------------------------------------------------

/// Why `[compute.work_offer]` cannot become an offer this node may publish.
///
/// Every one is a REFUSED BOOT that names the thing to fix. An offer this
/// daemon cannot honour is worse for a submitter than no offer at all: the
/// unit is leased, burns an attempt, and comes back with a verdict about this
/// node rather than about their tree (ARCH §18.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfferRefused {
    /// A kind in the config is not spelled `id:vN`.
    #[error("{0}")]
    Config(#[from] InvalidWorkOffer),
    /// A kind in the config has no executor registered for it.
    #[error("[compute.work_offer] offers `{kind}` and this daemon has no executor registered for it. Registered kinds: {}. Remove `{kind}` from `kinds`, or build the daemon with the feature that registers it.", registered_list(registered))]
    UnregisteredKind {
        kind: JobKind,
        registered: Vec<JobKind>,
    },
    // An executor demanding more isolation than this build provides was a
    // REFUSED BOOT until 2026-09-10 and is now a dropped kind with a `warn`
    // — see the floor's comment in `resolve_offer`. The variant is gone
    // rather than kept unreachable: an error nothing constructs is a claim
    // that a path exists, and the next reader would look for it.
    /// An `accept_from` entry is not an actor key.
    #[error("[compute.work_offer] accept_from contains `{raw}`, which is not an actor key: {why}")]
    AcceptKey { raw: String, why: String },
}

/// Render a kind list for a refusal sentence — `none` rather than `[]`, so an
/// operator reading it is told the registry is empty rather than shown an
/// empty bracket they have to interpret.
fn registered_list(kinds: &[JobKind]) -> String {
    if kinds.is_empty() {
        return "none".to_string();
    }
    kinds
        .iter()
        .map(|k| k.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The executor registry this daemon donates through.
///
/// One place, so the kinds a boot ACCEPTS and the kinds a running donor can
/// RESOLVE are the same set by construction rather than by two lists agreeing
/// (ARCH §10.6). `process:v1` is registered only when this crate is built
/// with `commonwealth-work/process`; a build without it offers nothing and
/// [`resolve_offer`] refuses a config that says otherwise, naming the kind.
///
/// **`corpus_engine` is why this takes an argument (cw-lift 5g).**
/// [`crate::ingest_executor::IngestExecutor`] runs a corpus slice through this
/// node's own engine, so a node that has none cannot run `ingest:v1` — and the
/// honest way to say that is to register nothing, which makes
/// [`resolve_offer`] refuse a config offering it and NAME the kind. The
/// alternative, registering an executor that fails at run time, is a donor
/// that leases units and burns the submitter's attempts, which is the exact
/// failure the boot check exists to prevent (ARCH §18.3).
pub fn donor_registry(
    corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
    sandbox: Sandbox,
) -> JobExecutorRegistry {
    let mut registry = JobExecutorRegistry::new();
    // `register` refuses a duplicate kind rather than overwriting, and each
    // kind is registered once here, so neither `Result` can be an error — both
    // are still surfaced rather than unwrapped, because a future second
    // registration must not be able to vanish (ARCH §18.3).
    //
    // The sandbox goes INTO the executor rather than being consulted beside
    // it, so the boundary a unit actually runs in and the isolation this node
    // advertises are one value read twice — a node cannot run units one way
    // and describe itself another (ARCH §10.6).
    if let Err(e) = registry.register(Arc::new(
        commonwealth_work::process::ProcessExecutor::with_sandbox(sandbox),
    )) {
        warn!(target: TRACE_TARGET, error = %e, "work donor: an executor could not be registered");
    }
    match corpus_engine {
        Some(engine) => {
            if let Err(e) = registry.register(Arc::new(
                crate::ingest_executor::IngestExecutor::new(engine),
            )) {
                warn!(target: TRACE_TARGET, error = %e, "work donor: an executor could not be registered");
            }
        }
        None => debug!(
            target: TRACE_TARGET,
            kind = crate::ingest_executor::INGEST_KIND,
            "work donor: this node has no corpus engine, so it registers no ingest executor"
        ),
    }
    registry
}

/// Resolve `[compute.work_offer]` against the executors this build actually
/// has, or refuse the boot naming what is wrong.
///
/// `Ok(None)` is the inert config — no kinds, no offer, no donor loop. That
/// is the shipped posture and it is not an error.
///
/// **The startup invariant:** every offered kind must resolve to a registered
/// executor whose isolation this build covers. `startup_refuses_offer_of_
/// unregistered_kind` is the gate, watched failing before the check existed.
///
/// `os` and `arch` are WHERE A UNIT RUNS, not who is hosting it — the caller
/// reads them off `Sandbox::platform`, so under a boundary they are the
/// image's. They were `std::env::consts` until 2026-09-10; see the field docs
/// on [`WorkOffer::os`] for the machine that failure was found on.
pub fn resolve_offer(
    section: &WorkOfferSection,
    registry: &JobExecutorRegistry,
    os: &str,
    arch: &str,
    provides: Isolation,
) -> Result<Option<WorkOffer>, OfferRefused> {
    let Some(offer) = section.to_offer(os, arch, provides)? else {
        debug!(
            target: TRACE_TARGET,
            "work donor: [compute.work_offer] names no kinds, so this node donates nothing"
        );
        return Ok(None);
    };
    for raw in section.accept_from.iter() {
        if let Err(why) = ActorKey::parse(raw) {
            return Err(OfferRefused::AcceptKey {
                raw: raw.clone(),
                why: why.to_string(),
            });
        }
    }
    // THE startup invariant: offered kinds are a SUBSET of registry kinds.
    // `registry.kinds()` is the same set `registry.resolve` answers from, so
    // a kind that passes here cannot fail to resolve in the loop — one
    // decider, not two lists that agree today (ARCH §10.6).
    // THE ISOLATION FLOOR IS A DROP, NOT A REFUSED BOOT (2026-09-10).
    //
    // The two failures below look alike and must not be treated alike. An
    // UNREGISTERED kind is the operator's typo, and refusing the boot is the
    // right answer: nothing they intended can happen and the sooner they read
    // the sentence the better. An isolation shortfall is OURS — it is this
    // build changing what a kind demands under a config that was valid when
    // it was written. Every daemon on this mesh carries
    // `kinds = ["process:v1"]` today, so refusing the boot would take those
    // daemons DOWN on their next restart to enforce a rule about work they
    // are not currently doing, which trades a security posture for an outage.
    //
    // So the kind is dropped from the published offer and the drop is NAMED
    // at `warn` — never silently, which would be the §18.3 substitution. If
    // that empties the offer, this node publishes none and donates nothing,
    // which is exactly the shipped posture for a node that offers no kind.
    // The partition is `commonwealth-work`'s, not this module's — the floor
    // has to reach a donor built from the package alone, and a second copy
    // here is the §10.6 twin this campaign has now closed three times.
    let partition = registry.offerable(&offer.kinds, provides);
    if let Some(kind) = partition.unregistered.first() {
        return Err(OfferRefused::UnregisteredKind {
            kind: kind.clone(),
            registered: registry.kinds(),
        });
    }
    for dropped in &partition.dropped {
        warn!(
            target: TRACE_TARGET,
            kind = %dropped.kind,
            required = ?dropped.required,
            provides = ?dropped.provides,
            "work donor: NOT offering this kind — {dropped}, so a unit of it \
             would run with this node's user, filesystem and network behind \
             nothing but consent. The config is left alone; the kind is \
             dropped from the offer. A container-backed executor is what \
             lifts this, not a config key"
        );
    }
    let offerable = partition.offerable;
    if offerable.is_empty() {
        info!(
            target: TRACE_TARGET,
            configured = %registered_list(&offer.kinds),
            "work donor: every configured kind was dropped by the isolation \
             floor, so this node publishes no offer and donates nothing"
        );
        return Ok(None);
    }
    let offer = WorkOffer {
        kinds: offerable,
        ..offer
    };
    if offer.max_concurrent == 0 {
        // Reported, not corrected. `may_take` will refuse every unit with
        // `Concurrency { held: 0, max: 0 }`, which is a working donor that
        // takes nothing — the exact shape of a silent misconfiguration, so it
        // is named once at boot where somebody is reading (ARCH §18.3).
        warn!(
            target: TRACE_TARGET,
            kinds = %registered_list(&offer.kinds),
            "work donor: [compute.work_offer] names kinds but max_concurrent is 0, so this \
             node advertises work it will then refuse every unit of — set max_concurrent"
        );
    }
    info!(
        target: TRACE_TARGET,
        kinds = %registered_list(&offer.kinds),
        max_concurrent = offer.max_concurrent,
        yield_to_foreground = offer.yield_to_foreground,
        repos = offer.repos.len(),
        registered = %registered_list(&registry.kinds()),
        "work donor: this node offers work"
    );
    Ok(Some(offer))
}

// -----------------------------------------------------------------
// The loop
// -----------------------------------------------------------------

/// Handle to the supervised donor loop. Aborting it stops taking new units;
/// units already in flight are aborted with the task, and their leases lapse
/// on the fold rather than being reported — which is the same fact a donor
/// that lost power publishes, and the fold already handles it.
pub struct WorkDonorHandle {
    task: SupervisedTask,
    state: Arc<ProjectState>,
}

impl WorkDonorHandle {
    /// What the supervisor last recorded about the loop. Exposed so a test
    /// can assert the loop parked after repeated panics rather than inferring
    /// it from a log.
    pub async fn status(&self) -> crate::projects::WatcherStatus {
        self.state.status(WatcherKind::WorkDonor).await
    }

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

/// Spawn the donor under [`crate::supervised_task::supervise`].
///
/// The supervisor is what keeps a panic inside a third party's unit from
/// taking the daemon with it: the loop body runs under `catch_unwind`, and
/// five crashes park it instead of pinning a restart loop.
pub fn spawn_work_donor(
    app_state: AppState,
    offer: WorkOffer,
    registry: Arc<JobExecutorRegistry>,
    donor_root: PathBuf,
    interval: Duration,
) -> WorkDonorHandle {
    let state = ProjectState::new(DONOR_DIR);
    let task = crate::supervised_task::supervise(WatcherKind::WorkDonor, Arc::clone(&state), {
        move || {
            let app_state = app_state.clone();
            let offer = offer.clone();
            let registry = Arc::clone(&registry);
            let donor_root = donor_root.clone();
            async move { donor_loop(app_state, offer, registry, donor_root, interval).await }
        }
    });
    WorkDonorHandle { task, state }
}

async fn donor_loop(
    app_state: AppState,
    offer: WorkOffer,
    registry: Arc<JobExecutorRegistry>,
    donor_root: PathBuf,
    interval: Duration,
) {
    info!(
        target: TRACE_TARGET,
        interval_ms = interval.as_millis() as u64,
        "work donor: loop started"
    );
    // In-flight units live here rather than in a shared map: the loop is one
    // future, so the set IS the count `max_concurrent` is compared against
    // locally, and a `JoinSet` aborts everything it holds when the loop is
    // dropped — which is what makes the supervisor's restart clean.
    let mut running: JoinSet<()> = JoinSet::new();
    loop {
        while running.try_join_next().is_some() {}
        let taken = take_round(&app_state, &offer, &registry, &donor_root, &mut running).await;
        if taken > 0 {
            debug!(target: TRACE_TARGET, taken, in_flight = running.len(), "work donor: leased");
        }
        tokio::time::sleep(interval).await;
    }
}

/// One pass over the fold. Returns how many units this pass leased.
async fn take_round(
    app_state: &AppState,
    offer: &WorkOffer,
    registry: &Arc<JobExecutorRegistry>,
    donor_root: &Path,
    running: &mut JoinSet<()>,
) -> usize {
    let Some(fold) = fold_now(app_state).await else {
        return 0;
    };
    let (rail, proj, self_key, now_ms) = fold;

    // Tier 1 preemption: the operator is at the keyboard, so nothing NEW is
    // taken. Units already running are not touched — see the module docs.
    if offer.yield_to_foreground && app_state.should_yield_to_foreground() {
        debug!(
            target: TRACE_TARGET,
            refusal = WorkRefusal::Yielding.id(),
            why = %WorkRefusal::Yielding,
            "work donor: not taking new units this round"
        );
        return 0;
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
        match append(app_state, &rail, &WorkAct::Offer(offer.clone())).await {
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
        if let Err(refusal) = executor.validate(&unit) {
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
        if let Err(e) = append(app_state, &rail, &WorkAct::Lease(unit_ref.clone())).await {
            warn!(target: TRACE_TARGET, unit = %unit_ref.unit_hash, error = %e,
                  "work donor: the lease could not be appended, so the unit was not started");
            continue;
        }
        let lease_interval_ms = executor.descriptor().lease_interval_ms;
        let app_state = app_state.clone();
        let self_key = self_key.clone();
        let executor = Arc::clone(&executor);
        running.spawn(async move {
            run_unit(
                app_state,
                executor,
                unit,
                unit_ref,
                self_key,
                workdir,
                lease_interval_ms,
            )
            .await;
        });
        taken += 1;
    }
    taken
}

/// Fold the `work` namespace as it stands right now.
///
/// `None` when this node has no rail, no `work` journal, an unreadable roster
/// or a journal that will not admit — each traced, and each a condition that
/// heals, so the round is skipped rather than the loop exiting.
pub(crate) async fn fold_now(
    app_state: &AppState,
) -> Option<(Arc<RingRail>, WorkProjection, ActorKey, u64)> {
    let rail = app_state.ring_rail()?;
    let journal = match rail.journal(WORK_NAMESPACE) {
        Ok(j) => j,
        Err(e) => {
            debug!(target: TRACE_TARGET, error = %e, "work donor: no `work` journal on this node yet");
            return None;
        }
    };
    let roster = match rail.roster(&journal).await {
        Ok(r) => r,
        Err(e) => {
            debug!(target: TRACE_TARGET, error = %e, "work donor: the `work` roster is unreadable, nothing folded");
            return None;
        }
    };
    let admission = match journal.admit(&roster, &Ed25519Verifier) {
        Ok(a) => a,
        Err(e) => {
            warn!(target: TRACE_TARGET, error = %e, "work donor: the `work` journal would not admit, nothing folded");
            return None;
        }
    };
    let self_key = match ActorKey::parse(rail.signer().actor()) {
        Ok(k) => k,
        Err(e) => {
            warn!(target: TRACE_TARGET, error = %e, "work donor: this node's own signing key is not an actor key");
            return None;
        }
    };
    let proj = WorkProjection::fold(&admission);
    Some((rail, proj, self_key, now_ms()))
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

// -----------------------------------------------------------------
// Checkouts
// -----------------------------------------------------------------

/// The directory a unit runs in.
///
/// A unit pinned to a `repo_rev` runs in **one checkout per repo, reused and
/// checked forward** — `scripts/evidence-verdict.py:529 ensure_worktree`'s
/// recipe, with `git clean -x` deliberately omitted so `target/` stays warm.
/// A per-unit checkout would invert that: the whole reason a dev peer is a
/// good donor is the warm target directory it already has, and a fresh
/// checkout per unit throws it away and pays minutes per unit for nothing.
///
/// The checkout is a self-contained local CLONE, not a `git worktree`. A
/// worktree's `.git` is a file pointing into the parent repo's `.git/`, which
/// is outside the one directory the boundary mounts — so inside the boundary
/// `git` answered "not a git repository" and ten tests that shell out to it
/// (conformance tags, refactor destinations, the donor's own rev
/// attribution) failed on the environment, not the code (watched
/// 2026-09-11, 12,730 / 13 in the boundary against 12,731 / 0 at the
/// reference). A local clone hardlinks the objects, so it costs no space and
/// carries its own history inside the mount.
///
/// A unit with no `repo_rev` runs in one reused scratch directory, for the
/// same reason and by the same rule.
async fn resolve_workdir(
    offer: &WorkOffer,
    unit: &JobUnit,
    donor_root: &Path,
) -> Result<PathBuf, WorkRefusal> {
    let Some(rev) = unit.requirements.repo_rev.clone() else {
        let scratch = donor_root.join("scratch");
        return std::fs::create_dir_all(&scratch)
            .map(|()| scratch.clone())
            .map_err(|e| WorkRefusal::PayloadNotCanonical {
                detail: format!("this donor could not create its scratch workdir: {e}"),
            });
    };
    let repos: Vec<(String, PathBuf)> = offer
        .repos
        .iter()
        .map(|r| (r.url.clone(), PathBuf::from(&r.path)))
        .collect();
    let root = donor_root.join("worktrees");
    let rev_for_err = rev.clone();
    let resolved = tokio::task::spawn_blocking(move || checkout_at(&repos, &root, &rev))
        .await
        .unwrap_or_else(|e| Err(format!("the checkout task panicked: {e}")));
    resolved.map_err(|host| {
        WorkRefusal::RequirementUnmet(UnmetRequirement::RepoRev {
            required: rev_for_err,
            host,
        })
    })
}

/// Find the first offered repo that can resolve `rev`, and hand back its one
/// reused worktree checked forward to it. Blocking; called from
/// `spawn_blocking`.
fn checkout_at(
    repos: &[(String, PathBuf)],
    worktree_root: &Path,
    rev: &str,
) -> Result<PathBuf, String> {
    if repos.is_empty() {
        return Err("this donor offers no repos".to_string());
    }
    let mut refused: Vec<String> = Vec::new();
    for (url, path) in repos {
        if git(path, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_err() {
            refused.push(format!("{url} does not have {rev}"));
            continue;
        }
        // ONE worktree per repo, keyed by the repo's own directory name so two
        // checkouts of different repos never share one (ARCH §7.5 — identity
        // from essence, and the URL is the essence here).
        let key = stable_repo_key(url);
        let worktree = worktree_root.join(&key);
        if let Err(e) = std::fs::create_dir_all(worktree_root) {
            return Err(format!("could not create {}: {e}", worktree_root.display()));
        }
        let source = path.display().to_string();
        let dot_git = worktree.join(".git");
        if !dot_git.exists() {
            git(
                path,
                &[
                    "clone",
                    "--quiet",
                    "--no-checkout",
                    "--",
                    &source,
                    &worktree.display().to_string(),
                ],
            )
            .map_err(|e| format!("`git clone` failed for {url}: {e}"))?;
        } else if dot_git.is_file() {
            // A worktree LINK from before the clone rule: convert it in place
            // and keep everything else (target/ above all). The link's parent
            // entry is pruned so the parent repo stops listing a worktree
            // that is now a repository of its own.
            let staging = worktree_root.join(format!("{key}.converting"));
            let _ = std::fs::remove_dir_all(&staging);
            git(
                path,
                &[
                    "clone",
                    "--quiet",
                    "--no-checkout",
                    "--",
                    &source,
                    &staging.display().to_string(),
                ],
            )
            .map_err(|e| format!("`git clone` (conversion) failed for {url}: {e}"))?;
            // Order matters: the parent's entry is pruned only while the
            // link is GONE and nothing sits at `.git` yet — `git worktree
            // prune` keeps an entry whose path still exists, a directory
            // included (watched in the test's first run).
            std::fs::remove_file(&dot_git).map_err(|e| {
                format!(
                    "removing the worktree link at {} failed: {e}",
                    dot_git.display()
                )
            })?;
            if let Err(e) = git(path, &["worktree", "prune"]) {
                // Not fatal for the unit — the clone below is complete either
                // way — but a stale entry in the parent is worth a line.
                warn!(
                    target: TRACE_TARGET,
                    parent = %path.display(),
                    error = %e,
                    "work donor: the parent repo still lists the converted checkout as a worktree — `git worktree prune` failed"
                );
            }
            std::fs::rename(staging.join(".git"), &dot_git).map_err(|e| {
                format!(
                    "moving the clone's .git into {} failed: {e}",
                    worktree.display()
                )
            })?;
            let _ = std::fs::remove_dir_all(&staging);
            info!(
                target: TRACE_TARGET,
                checkout = %worktree.display(),
                "work donor: converted a worktree link into a self-contained clone — git now works inside the boundary"
            );
        }
        // The clone may predate `rev`; a local fetch brings it in (objects
        // copied from the offered path, nothing crosses a network).
        git(&worktree, &["fetch", "--quiet", "--", &source, rev])
            .map_err(|e| format!("`git fetch {rev}` failed in {}: {e}", worktree.display()))?;
        git(&worktree, &["checkout", "-q", "-f", "--detach", rev])
            .map_err(|e| format!("`git checkout` failed in {}: {e}", worktree.display()))?;
        // `-x` is deliberately absent: target/ is ignored and warm, and the
        // point of one reused worktree is to keep it that way.
        git(&worktree, &["clean", "-fdq"])
            .map_err(|e| format!("`git clean` failed in {}: {e}", worktree.display()))?;
        return Ok(worktree);
    }
    Err(refused.join("; "))
}

/// A filesystem-safe, stable directory name for a repo URL.
///
/// Derived from the URL — the thing that identifies a repo across donors —
/// rather than from a counter or the offer's position in a list (ARCH §7.5).
fn stable_repo_key(url: &str) -> String {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let safe: String = last
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if safe.is_empty() {
        "repo".to_string()
    } else {
        safe
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// -----------------------------------------------------------------
// One unit
// -----------------------------------------------------------------

/// Run one leased unit: heartbeat while it runs, report when it stops.
async fn run_unit(
    app_state: AppState,
    executor: Arc<dyn commonwealth_work::executor::JobExecutor>,
    unit: JobUnit,
    unit_ref: UnitRef,
    self_key: ActorKey,
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
                    match still_ours(&app_state, &unit_ref, &self_key).await {
                        LeaseState::Held => {
                            if let Some(rail) = app_state.ring_rail() {
                                if let Err(e) =
                                    append(&app_state, &rail, &WorkAct::Renew(unit_ref.clone())).await
                                {
                                    warn!(target: TRACE_TARGET, unit = %unit_ref.unit_hash, error = %e,
                                          "work donor: a renew could not be appended — the lease will lapse");
                                }
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
    let Some(rail) = app_state.ring_rail() else {
        warn!(target: TRACE_TARGET, unit = %unit_ref.unit_hash,
              "work donor: the unit finished and this node has no rail to report it on");
        return;
    };
    let wall_seconds = started.elapsed().as_secs_f64();
    match append(&app_state, &rail, &act).await {
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
                        "work donor: crediting this node's contribution ledger"
                    );
                    app_state.inner.contribution_emitter.record(credit);
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

// -----------------------------------------------------------------
// The credit
// -----------------------------------------------------------------

/// What this node's contribution ledger owes itself for a unit it just
/// reported, or `None` when it owes nothing.
///
/// # Why the DONOR emits this, and not every node that folds the `Complete`
///
/// The other candidate looked like the convergent one and is written down
/// here because it is the argument, not the conclusion, that the next reader
/// needs: every node emits a credit when its own fold applies a `Complete`,
/// so the ledger becomes a function of the journal exactly the way the queue
/// is — the queue is a fold over admission, `svrn job status` folds the same
/// acts on any node, and a third replicated derivation of the same journal
/// would be in good company. It is still the wrong shape here, for two
/// reasons that are mechanism rather than taste.
///
/// **The contributions ledger is ALREADY a replicated log, and it converges
/// by "one write site, one event".** `LedgerEvent`s are stored in `MeshStore`
/// under the `contributions` app id, gossip to peers, and merge LWW on a key
/// of `origin:secs:nanos:seq` (`commonwealth_state::contributions`). N nodes
/// folding one `Complete` would therefore write N rows under N DISTINCT keys,
/// and `aggregate` sums rows: one donated shard would be credited once per
/// ring member, and the error would grow with the ring. Making that safe
/// needs the credit keyed by `unit_hash` — a second idempotence key beside
/// the one the work fold already owns, which is the two-deciders defect ARCH
/// §10.6 names. Fold-emission would not make this ledger more convergent; it
/// would multiply one fact by the membership.
///
/// **A folding third party cannot produce this event anyway.** It can see
/// THAT the unit completed — that part is on the rail, signed and totally
/// ordered, and that is precisely why the rail is where the *fact* lives. It
/// cannot see `wall_seconds`: the journal carries `leased_at_ms` and
/// `completed_at_ms`, whose difference is lease-held time including journal
/// admit latency and heartbeat scheduling, not the compute. Only the machine
/// that ran the process measured that. This is the same reason
/// `InferenceServed` is emitted by the server, `KnowledgeQueryServed` by the
/// node that served it and `StorageSnapshot` by the host: in this ledger the
/// OBSERVER emits, once. On the work plane the donor is the observer.
///
/// What the rail keeps is the AUDIT. `handoff` + `unit_hash` + `donor_actor`
/// point at the signed `Complete` that has to exist for the credit to be
/// honest, and `donor_actor` is the [`ActorKey`] admission verified rather
/// than the self-reported `node_id` the emitter stamps (ARCH §7.5) — so the
/// two halves of the record can be checked against each other, which neither
/// could alone (ARCH §18.1: a claim asserted only on a field its own subject
/// supplies is not evidence).
///
/// # Double counting, under at-least-once delivery
///
/// The rail delivers a `Complete` at least once, and the fold is idempotent
/// per `unit_hash`: `WorkProjection::complete` moves `Leased -> Complete`
/// exactly once and every repeat lands on `double_deliveries` without
/// changing state. This credit is not derived from that stream at all — it is
/// written once, by the single process that ran the unit, in the arm where
/// that process's own `append` returned `Ok`. Redelivery cannot multiply it
/// because redelivery never reaches it, and a peer replaying the journal
/// emits nothing.
///
/// A unit whose report lapsed and which is re-leased and re-run IS credited
/// twice, to two different donors. That is not a double count: two machines
/// really did spend the time, and the ledger's job is to say so.
///
/// # `Complete` is credited and `Fail` is not
///
/// `WorkUnitStatus`' own distinction, kept: `Complete` is "the unit ran and
/// reached a verdict" — a red test shard included, since the unit did its job
/// — while `Fail` is "the plane failed to run the work". Crediting the second
/// would pay a donor for burning the submitter's attempts, which is a reward
/// pointed at exactly the wrong behaviour.
fn credit_for(
    act: &WorkAct,
    unit: &JobUnit,
    self_key: &ActorKey,
    wall_seconds: f64,
) -> Option<commonwealth_core::contributions::LedgerEventKind> {
    match act {
        WorkAct::Complete(c) => Some(
            commonwealth_core::contributions::LedgerEventKind::JobUnitCompleted {
                handoff: c.handoff,
                unit_hash: c.unit_hash.clone(),
                // The key this node SIGNED with, taken from the rail signer
                // rather than from any field of the act — an actor is the one
                // thing on a journal line a writer cannot forge for somebody
                // else, and re-reading it off the payload would throw that
                // away.
                donor_actor: self_key.as_str().to_string(),
                kind: unit.kind.clone(),
                wall_seconds,
            },
        ),
        // `Fail` is the one other act `run_unit` builds, and it is uncredited
        // for the reason above. Nothing else can arrive here; a `Lease` or a
        // `Renew` is not a report, so "no credit" is the right answer for any
        // future act too rather than a hole this wildcard hides.
        _ => None,
    }
}

/// The I/O half of "do I still hold this lease".
///
/// The DECIDER moved to `commonwealth_work::projection::lease_state` — it is
/// pure over the fold, and a lifted peer needs exactly the same three-state
/// answer (cw-lift 5f found this by re-deriving it as a bool and cancelling a
/// running unit on one unreadable heartbeat). What stays here is the only part
/// that is this crate's business: obtaining the fold from an `AppState`, and
/// answering `Unknown` when that fails.
async fn still_ours(app_state: &AppState, unit_ref: &UnitRef, self_key: &ActorKey) -> LeaseState {
    match fold_now(app_state).await {
        Some((_, proj, _, now_ms)) => lease_state(&proj, unit_ref, self_key, now_ms),
        // A journal that could not be read is NOT evidence that somebody else
        // holds the lease. Kept apart from `Lost` so the caller can hold
        // rather than kill (ARCH §18.2 — could-not-judge is its own verdict).
        None => LeaseState::Unknown,
    }
}

/// What machine, rev, arch and toolchain produced a result.
///
/// **`repo_rev` is what this donor ACTUALLY RAN AT, never what it was asked
/// for.** A pinned unit ran in a worktree checked forward to its pin, so the
/// two agree; an UNPINNED unit ran at whatever this donor's checkout happens
/// to be, and reporting the empty string there would make every unpinned
/// donor's attribution compare equal to every other's — which is precisely
/// the comparison `ComputeAttribution::comparable_to` exists to fail (the
/// plan's 5e bar iii: a stale donor's unpinned verdict must be flaggable).
///
/// THE REV IS THE ONLY PART THIS FUNCTION DECIDES, and since 2026-09-10 it is
/// the only part it returns. Resolving it needs a workdir, which is a donor's
/// own business. The other three fields are "where did this run", and this
/// function answered "on my host" — true for a subprocess and false for a
/// unit inside a container image. `JobExecutor::attribution` answers it now,
/// because the executor that ran the unit is the only thing that knows which
/// of the two it was.
fn repo_rev_of(unit: &JobUnit, workdir: &Path) -> String {
    unit.requirements
        .repo_rev
        .clone()
        .or_else(|| rev_of_the_checkout_this_workdir_belongs_to(workdir))
        .filter(|rev| !rev.is_empty())
        .unwrap_or_else(|| ABSENT_REV.to_string())
}

/// The HEAD of the checkout this workdir is PART OF, or `None` when it is not
/// part of one.
///
/// **`git rev-parse HEAD` alone cannot answer this, because git WALKS UP.** An
/// unpinned unit runs in `donor_root/scratch` (`resolve_workdir`, the only
/// branch that serves it), which is not a checkout — so a bare `rev-parse` had
/// exactly two possible answers: a failure, which is honest, or the HEAD of
/// whatever checkout the donor's DATA DIRECTORY happens to sit under, which is
/// a fabricated rev for work that never touched that tree. And a fabricated rev
/// is the worst of the three, because it compares EQUAL to a submitter at that
/// rev and `ComputeAttribution::comparable_to` then adopts the verdict
/// (§18.3 — a plausible wrong value beats an absence at getting believed).
/// Which of the two you got was decided by where the daemon's data dir lives:
/// `~/.svrnmesh` gives the honest absence, `SVRNMESH_DATA_DIR` pointed inside a
/// checkout gives the fabrication.
///
/// `ls-files` is the discriminator because it lists TRACKED content under the
/// cwd: a donor's own checkout has some (160 files at this crate's root), a
/// scratch directory nested under one has none. Measured 2026-09-10. It costs a
/// full listing, which is fine — it runs only for an UNPINNED unit, once, beside
/// a unit that is about to run a whole program.
///
/// WHAT THIS DOES NOT CHANGE: a donor running unpinned work inside its own
/// checkout still reports that checkout's rev, so `WORK_PLANE.md`'s 5e bar iii
/// keeps the flaggable stale verdict it asks for. It stops getting an invented
/// one.
fn rev_of_the_checkout_this_workdir_belongs_to(workdir: &Path) -> Option<String> {
    let tracked = git(workdir, &["ls-files"]).ok()?;
    if tracked.trim().is_empty() {
        return None;
    }
    git(workdir, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
}

/// Append one act to the local `work` journal, then nudge the ring round.
///
/// THE door, and the same one `rail_kv_pump` uses for a store write: the
/// journal signs and sequences it, `ring_sync` carries it. No HTTP, so no new
/// sender of replicated state.
async fn append(app_state: &AppState, rail: &RingRail, act: &WorkAct) -> Result<(), String> {
    let payload = commonwealth_work::act::to_payload(act).map_err(|e| e.to_string())?;
    let journal = rail.journal(WORK_NAMESPACE).map_err(|e| e.to_string())?;
    let roster = rail.roster(&journal).await.map_err(|e| e.to_string())?;
    journal
        .append(RailAct::Record { payload }, rail.signer(), &roster)
        .map_err(|e| e.to_string())?;
    app_state.ring_write_nudge().notify_one();
    Ok(())
}

/// Now, in the milliseconds the fold speaks.
fn now_ms() -> u64 {
    sovereign_core::time::unix_millis()
}

#[cfg(test)]
mod tests;
