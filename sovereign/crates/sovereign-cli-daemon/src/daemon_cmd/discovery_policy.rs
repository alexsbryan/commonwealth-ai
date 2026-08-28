//! What one RPC-worker discovery tick should do to the distributed-primary
//! child — as a pure function of that tick's observations.
//!
//! This decision used to live inside the async closure in [`super::bootstrap`],
//! where it could not be exercised without a mesh, a peer, a GPU and a 92GB
//! model. That is how it shipped a rule that retired a healthy compute child
//! eight seconds after spawning it, throwing away a three-minute warm cycle
//! (live capture 2026-07-28, note c5678d34): nothing could ask the loop "what
//! would you do with THIS tick?" without reproducing the whole cluster.
//!
//! So the decision is extracted here and the effects stay in `bootstrap`. The
//! precedent is [`sovereign_mesh`]'s `reaffirm_plan` / `sticky_endpoint`, split
//! out of `discover_rpc_workers` for exactly this reason — see the doc comment
//! at `sovereign-mesh/src/daemon.rs:3544`.
//!
//! Scope note: this covers the CHILD arm only (`[compute] distributed_primary`).
//! The in-process arm stays inline in `bootstrap` on purpose — its consequences
//! are different in kind (an in-place `reload_primary` against a departed worker
//! is an uncatchable `GGML_ABORT`, per DISTRIBUTED_PILOT_READINESS.md P0.4), and
//! sharing a policy function would invite treating the two as interchangeable.

use std::time::Duration;

use sovereign_compute::supervisor::SpawnVerdict;

/// How long the eligible worker set must stop changing before a pure GROW is
/// acted on. Anti-thrash: without it, a cluster settling one worker at a time
/// pays a full warm + respawn per worker.
///
/// Shrinks deliberately bypass this — see [`TickInputs::shrank`].
pub(super) const STABLE: Duration = Duration::from_secs(20);

/// Everything one discovery tick knows, as data.
///
/// One struct rather than eight arguments so the call site cannot silently
/// forget a term, and so a test can state a tick declaratively instead of
/// reconstructing loop-local mutable state.
pub(super) struct TickInputs<'a> {
    /// Whether this node won the shared-model host election this tick. Only the
    /// host assembles the split; a non-host anchor still keeps its discovery and
    /// eligibility warm so that election is followed by an immediate assemble.
    pub am_host: bool,
    /// A warm/respawn is already in flight on the detached task. Warming moves
    /// gigabytes and can take minutes; the 15s tick must not queue a second one
    /// behind it.
    pub busy: bool,
    /// This tick's ELIGIBLE worker set (post-eligibility-gate, sorted + deduped).
    pub current: &'a [String],
    /// The worker set the last warm+respawn ACTED ON — deliberately the
    /// attempted set, not the set that ended up warm. A warm may legitimately
    /// place on a subset (a worker going ineligible between discovery and
    /// planning); comparing against the subset would make `changed` true forever
    /// and respawn the child every tick.
    pub last_loaded: &'a [String],
    /// A previous warm was refused and its backoff has elapsed, so retry even
    /// though nothing about the worker set changed.
    pub retry_due: bool,
    /// How long the eligible set has been unchanged.
    pub stable_for: Duration,
    /// How long the eligible set has been EMPTY, if it is. `None` = not empty.
    pub empty_for: Option<Duration>,
    /// Age of the live child generation; `None` when the slot holds no child.
    pub child_age: Option<Duration>,
}

/// How long the eligible set must stay empty before the child is parked.
///
/// Only the shrink-to-ZERO case gets this grace. A partial shrink still fires
/// immediately (see [`TickInputs::shrank`]) because survivors exist to re-form
/// on, so acting fast buys something real. A shrink to zero has nothing to
/// re-form on — the fast path buys literally nothing and costs a warm cycle.
///
/// ~6 ticks: long enough to outlast a starved probe run on a peer that is busy
/// serving our own model warm, short enough that a genuinely dissolved cluster
/// does not leave a child dialling ghosts for minutes.
const RETIRE_GRACE: Duration = Duration::from_secs(90);

/// A child may never be retired younger than this.
///
/// On 2026-07-28 a child that had been serving for EIGHT SECONDS — the direct
/// product of a three-minute warm cycle — was retired by the very next tick,
/// because the peer that had just warmed it was still too busy to answer a
/// probe. A distributed child at that age is still walking its shards; the
/// load deadline for one is 1800s.
const MIN_CHILD_LIFETIME: Duration = Duration::from_secs(120);

impl TickInputs<'_> {
    /// The worker set differs from what we last acted on (either direction), or
    /// a refusal's backoff came due.
    fn changed(&self) -> bool {
        self.current != self.last_loaded || self.retry_due
    }

    /// A worker the child was last spawned ACROSS is no longer eligible.
    ///
    /// Shrink-fast-prune: a shrink skips the [`STABLE`] debounce, because the
    /// departed worker must be pruned before it can abort a decode, and the
    /// survivors' warm caches make re-forming cheap. A pure grow keeps the
    /// debounce.
    fn shrank(&self) -> bool {
        self.last_loaded.iter().any(|w| !self.current.contains(w))
    }
}

/// What the tick should do to the distributed-primary child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChildAction {
    /// Nothing to do: not the host, nothing changed, or a grow still inside the
    /// debounce.
    Hold,
    /// A warm/respawn is already running; this tick defers to it.
    Busy,
    /// Warm these workers and respawn the child across exactly the set that
    /// warms.
    Respawn { workers: Vec<String> },
    /// Park the slot unavailable. Deliberately NOT a fallback to a local load:
    /// loading a model this size locally is what collapsed the desktop session
    /// on 2026-07-27.
    Retire { reason: String },
    /// The eligible set is empty, but not for long enough (or the child is too
    /// young) to believe it. Leave the child alone and re-evaluate next tick.
    WaitForWorkers {
        empty_for_secs: u64,
        child_age_secs: u64,
    },
}

/// Decide this tick's action on the distributed-primary child.
///
/// Pure: no clock, no I/O, no locks. Every temporal input arrives as an elapsed
/// [`Duration`] so a test can state "the set has been empty for 8 seconds"
/// without sleeping.
pub(super) fn decide_child_action(t: &TickInputs<'_>) -> ChildAction {
    // Only the host distributes, and only when something actually moved. Note
    // the ordering: the host/changed/debounce gate is evaluated BEFORE the busy
    // check, matching the original loop — a busy tick inside a quiet window is
    // `Hold`, not `Busy`.
    if !t.am_host || !t.changed() {
        return ChildAction::Hold;
    }
    if !(t.shrank() || t.stable_for >= STABLE) {
        return ChildAction::Hold;
    }
    if t.busy {
        return ChildAction::Busy;
    }
    if t.current.is_empty() {
        // Two independent guards, both of which must clear. Either alone is
        // insufficient: a long-lived child can still be torn down by one
        // starved probe run, and a young child can still be torn down by a
        // grace that expired while it was warming.
        let empty_ok = t.empty_for.is_some_and(|d| d >= RETIRE_GRACE);
        let age_ok = t.child_age.is_none_or(|a| a >= MIN_CHILD_LIFETIME);
        if empty_ok && age_ok {
            return ChildAction::Retire {
                reason: "no eligible RPC workers".to_string(),
            };
        }
        return ChildAction::WaitForWorkers {
            empty_for_secs: t.empty_for.map(|d| d.as_secs()).unwrap_or(0),
            child_age_secs: t.child_age.map(|d| d.as_secs()).unwrap_or(0),
        };
    }
    ChildAction::Respawn {
        workers: t.current.to_vec(),
    }
}

/// May the supervisor (re)spawn the distributed-primary child right now?
///
/// - `pinned` — the endpoints the live handoff names, i.e. what the child WILL
///   dial when it re-reads that file at startup.
/// - `eligible` — this tick's eligible-worker snapshot.
/// - `env` — `SOVEREIGN_RPC_WORKERS`, the manual worker list. It never enters
///   the eligible snapshot at all, so it must never be gated away.
///
/// Holds ONLY when every pinned endpoint is known-gone, because that is the
/// only provably futile case: the supervisor respawns with identical argv and
/// the child re-reads the same handoff, so it will dial the same corpses and
/// abort again, burning crash-loop budget the discovery loop is the only thing
/// that can refresh.
///
/// FAILS OPEN everywhere else — nothing pinned yet, an empty snapshot, a
/// partial overlap. The partial case is deliberate: one surviving worker means
/// the discovery loop is a tick away from issuing a correct new handoff, and a
/// stricter all-must-be-eligible rule would hold on a single transient probe
/// miss. This gate's job is to stop futile work, not to second-guess placement.
pub(super) fn spawn_gate_verdict(
    pinned: &[String],
    eligible: &[String],
    env: &[String],
) -> SpawnVerdict {
    if pinned.is_empty() {
        return SpawnVerdict::Allow;
    }
    let survives = pinned
        .iter()
        .any(|p| eligible.iter().any(|e| e == p) || env.iter().any(|e| e == p));
    if survives {
        SpawnVerdict::Allow
    } else {
        SpawnVerdict::Hold {
            reason: format!(
                "every worker this child is pinned to is gone ({} pinned, {} eligible) — \
                 a respawn would re-dial a dead endpoint and abort",
                pinned.len(),
                eligible.len()
            ),
        }
    }
}

/// What the child will ask THIS host for, given the cut it will actually load.
///
/// `local_blocks / total_blocks` of the weights, plus the non-weight terms.
/// With `overheads` (llama.cpp's own projection, `projected_overheads`), those
/// terms are the host's real KV share plus its accelerator and scheduler
/// compute buffers — the same three-term accounting the per-device fit gate
/// uses, so the two gates cannot drift. Without it, the old `share/8 + 1 GiB`
/// proxy stands in (measured ~3–5× over on MLA models, but conservative in
/// the direction whose failure mode is an unusable machine).
///
/// `None` when the cut is unknown (`total_blocks == 0`, i.e. no block plan
/// yet) — the caller must then FAIL OPEN, because a guess here is a guess
/// about whether to run at all. `SOVEREIGN_LOCAL_FIT_RESERVE_GB` and
/// `SOVEREIGN_COMPUTE_SPAWN_GATE=0` are the escape hatches when the estimate
/// is wrong for a given host.
pub(super) fn host_share_need_bytes(
    model_bytes: u64,
    local_blocks: u32,
    total_blocks: u32,
    overheads: Option<&sovereign_inference::embedded::PlanOverheads>,
) -> Option<u64> {
    if total_blocks == 0 || model_bytes == 0 {
        return None;
    }
    const GIB: u64 = 1024 * 1024 * 1024;
    let share = (model_bytes as u128 * local_blocks as u128 / total_blocks as u128) as u64;
    let overhead = match overheads {
        // The host is the plan's LAST device by construction, so it carries
        // its KV share plus BOTH compute terms (accelerator + scheduler).
        Some(o) => {
            let ctx = (o.context_total_bytes as u128 * local_blocks as u128 / total_blocks as u128)
                as u64;
            ctx + o.compute_accel_bytes + o.compute_host_bytes
        }
        None => share / 8 + GIB,
    };
    Some(share + overhead)
}

/// Would spawning this child leave the machine with nothing?
///
/// Holds when the host's share plus the reserve exceeds available memory. A
/// `Hold` parks the supervisor's run loop and is re-polled every 2 s WITHOUT
/// burning crash-loop budget (see [`SpawnVerdict`]), so freeing memory — or a
/// re-plan that shifts blocks onto a worker — releases it with no operator
/// action.
///
/// WHY THIS EXISTS. A crashed child is restarted with identical argv, so a
/// footprint that did not fit the first time does not fit the second either —
/// and the retry runs unattended, seconds after the crash, while the operator is
/// still using the machine. On a unified-memory host the GPU allocator's own
/// ceiling can exceed system RAM, so nothing below this gate stops a load from
/// consuming the last free page; graphics drivers commonly abort a non-robust
/// client on an out-of-memory submit rather than stall it, which turns "the
/// model did not fit" into "the user lost their session". Observed twice,
/// 2026-07-27 and 2026-08-02 (notes 309c841b, 92d55ceb).
///
/// The equivalent guard on the LocalOnly path predates this one and never
/// covered the distributed door — see [`ChildAction::Retire`].
pub(super) fn memory_headroom_verdict(
    need_bytes: Option<u64>,
    available_bytes: u64,
    reserve_bytes: u64,
) -> SpawnVerdict {
    // No cut yet, or a memory sensor that returned zero: never brick a spawn on
    // a missing measurement. Same fail-open rule as the local-fit gate.
    let (Some(need), true) = (need_bytes, available_bytes > 0) else {
        return SpawnVerdict::Allow;
    };
    if need.saturating_add(reserve_bytes) <= available_bytes {
        return SpawnVerdict::Allow;
    }
    const MB: u64 = 1024 * 1024;
    SpawnVerdict::Hold {
        reason: format!(
            "host share {} MB + {} MB reserved for the OS and desktop exceeds {} MB available \
             — spawning would starve the host (2026-08-02 session-kill class). Free memory, or \
             shift blocks onto a worker. Override: SOVEREIGN_LOCAL_FIT_RESERVE_GB, or \
             SOVEREIGN_COMPUTE_SPAWN_GATE=0 to disable the gate entirely",
            need / MB,
            reserve_bytes / MB,
            available_bytes / MB,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    /// A tick that would act: host, set changed, debounce satisfied, not busy.
    fn acting_tick<'a>(current: &'a [String], last_loaded: &'a [String]) -> TickInputs<'a> {
        TickInputs {
            am_host: true,
            busy: false,
            current,
            last_loaded,
            retry_due: false,
            stable_for: STABLE,
            empty_for: None,
            child_age: None,
        }
    }

    #[test]
    fn a_non_host_never_acts() {
        let current = v(&["a:1", "b:2"]);
        let last = v(&[]);
        let mut t = acting_tick(&current, &last);
        t.am_host = false;
        assert_eq!(decide_child_action(&t), ChildAction::Hold);
    }

    #[test]
    fn an_unchanged_set_holds() {
        let current = v(&["a:1"]);
        let last = v(&["a:1"]);
        let t = acting_tick(&current, &last);
        assert_eq!(decide_child_action(&t), ChildAction::Hold);
    }

    #[test]
    fn a_grow_waits_for_the_stable_debounce() {
        let current = v(&["a:1", "b:2"]);
        let last = v(&["a:1"]);

        let mut t = acting_tick(&current, &last);
        t.stable_for = Duration::from_secs(5);
        assert_eq!(
            decide_child_action(&t),
            ChildAction::Hold,
            "a pure grow inside the debounce must not respawn"
        );

        t.stable_for = Duration::from_secs(21);
        assert_eq!(
            decide_child_action(&t),
            ChildAction::Respawn {
                workers: v(&["a:1", "b:2"])
            }
        );
    }

    #[test]
    fn a_partial_shrink_bypasses_the_debounce() {
        // Shrink-fast-prune: survivors exist to re-form on, and the departed
        // worker must be pruned before it can abort a decode.
        let current = v(&["a:1"]);
        let last = v(&["a:1", "b:2"]);
        let mut t = acting_tick(&current, &last);
        t.stable_for = Duration::ZERO;
        assert_eq!(
            decide_child_action(&t),
            ChildAction::Respawn {
                workers: v(&["a:1"])
            }
        );
    }

    #[test]
    fn a_busy_tick_defers_to_the_warm_in_flight() {
        let current = v(&["a:1", "b:2"]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.busy = true;
        assert_eq!(decide_child_action(&t), ChildAction::Busy);
    }

    #[test]
    fn a_busy_tick_inside_a_quiet_window_is_a_plain_hold() {
        // Ordering guard: the host/changed gate is evaluated before `busy`, so a
        // busy tick with nothing to do reports Hold, not Busy.
        let current = v(&["a:1"]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.busy = true;
        assert_eq!(decide_child_action(&t), ChildAction::Hold);
    }

    #[test]
    fn retry_due_respawns_against_an_unchanged_set() {
        // A refused warm must not leave the primary down until a worker happens
        // to join or leave.
        let current = v(&["a:1"]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.retry_due = true;
        assert_eq!(
            decide_child_action(&t),
            ChildAction::Respawn {
                workers: v(&["a:1"])
            }
        );
    }

    // ── spawn gate ──────────────────────────────────────────────────────

    #[test]
    fn the_gate_holds_when_every_pinned_worker_is_gone() {
        let pinned = v(&["mac:50052"]);
        let verdict = spawn_gate_verdict(&pinned, &[], &[]);
        assert!(
            matches!(verdict, SpawnVerdict::Hold { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn the_gate_allows_on_any_surviving_worker() {
        // Partial overlap ALLOWS on purpose: the discovery loop is one tick
        // from a corrected handoff, and holding on a transient miss would be
        // worse than one more respawn.
        let pinned = v(&["mac:50052", "pi:50052"]);
        let eligible = v(&["pi:50052"]);
        assert_eq!(
            spawn_gate_verdict(&pinned, &eligible, &[]),
            SpawnVerdict::Allow
        );
    }

    #[test]
    fn the_gate_never_blocks_a_manually_configured_worker() {
        // SOVEREIGN_RPC_WORKERS never enters the eligible snapshot, so gating
        // on the snapshot alone would permanently wedge a manual setup.
        let pinned = v(&["manual:50052"]);
        let env = v(&["manual:50052"]);
        assert_eq!(spawn_gate_verdict(&pinned, &[], &env), SpawnVerdict::Allow);
    }

    #[test]
    fn an_unpinned_slot_is_never_gated() {
        // Nothing pinned yet (first spawn, or a mock child) — fail open.
        assert_eq!(spawn_gate_verdict(&[], &[], &[]), SpawnVerdict::Allow);
    }

    // ── retire grace (the 2026-07-28 headline) ──────────────────────────

    /// THE FIX. This test previously asserted the opposite — that an empty
    /// eligible set retires the child immediately — which is exactly what
    /// destroyed a three-minute warm cycle on 2026-07-28: a peer that was alive
    /// the whole time went briefly unconfirmed while it was busy serving our own
    /// 21GB warm, and the very next tick retired the child that warm had just
    /// produced, eight seconds after it spawned.
    #[test]
    fn an_empty_eligible_set_does_not_retire_a_freshly_spawned_child() {
        let current = v(&[]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.empty_for = Some(Duration::from_secs(15));
        t.child_age = Some(Duration::from_secs(8));
        assert_eq!(
            decide_child_action(&t),
            ChildAction::WaitForWorkers {
                empty_for_secs: 15,
                child_age_secs: 8,
            }
        );
    }

    /// Recovery inside the grace is free — and this is the property that turns
    /// the incident's three warm cycles into one. When the worker returns,
    /// `current == last_loaded`, so the tick is a plain `Hold` and the serving
    /// child is never disturbed. It depends on `WaitForWorkers` NOT clearing the
    /// attempted set.
    #[test]
    fn a_worker_returning_inside_the_grace_leaves_the_child_alone() {
        let current = v(&["a:1"]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.child_age = Some(Duration::from_secs(20));
        assert_eq!(decide_child_action(&t), ChildAction::Hold);
    }

    #[test]
    fn the_grace_expires_and_the_child_is_finally_retired() {
        let current = v(&[]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.empty_for = Some(Duration::from_secs(120));
        t.child_age = Some(Duration::from_secs(600));
        assert_eq!(
            decide_child_action(&t),
            ChildAction::Retire {
                reason: "no eligible RPC workers".to_string()
            }
        );
    }

    /// The two guards are independent — neither alone may authorise a retire.
    #[test]
    fn a_long_lived_child_still_needs_the_empty_grace() {
        let current = v(&[]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.empty_for = Some(Duration::from_secs(10));
        t.child_age = Some(Duration::from_secs(9999));
        assert!(matches!(
            decide_child_action(&t),
            ChildAction::WaitForWorkers { .. }
        ));
    }

    #[test]
    fn a_young_child_survives_an_expired_empty_grace() {
        let current = v(&[]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.empty_for = Some(Duration::from_secs(9999));
        t.child_age = Some(Duration::from_secs(5));
        assert!(matches!(
            decide_child_action(&t),
            ChildAction::WaitForWorkers { .. }
        ));
    }

    /// With no child in the slot there is nothing to protect, so an expired
    /// grace retires (parks) immediately.
    #[test]
    fn an_empty_slot_retires_once_the_grace_expires() {
        let current = v(&[]);
        let last = v(&["a:1"]);
        let mut t = acting_tick(&current, &last);
        t.empty_for = Some(Duration::from_secs(120));
        t.child_age = None;
        assert!(matches!(
            decide_child_action(&t),
            ChildAction::Retire { .. }
        ));
    }

    // ─── memory headroom ───────────────────────────────────────
    //
    // The rule under test is a relation between three quantities, not a
    // property of any particular machine. Numbers here are chosen to make the
    // arithmetic legible; where host SIZE is the thing being tested, the case
    // sweeps a range rather than naming one box.

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A share that does not fit alongside the reserve is refused.
    #[test]
    fn a_share_that_does_not_fit_beside_the_reserve_is_held() {
        assert!(matches!(
            memory_headroom_verdict(Some(60), 70, 20),
            SpawnVerdict::Hold { .. }
        ));
    }

    /// ...and one that does fit is not.
    #[test]
    fn a_share_that_fits_beside_the_reserve_is_allowed() {
        assert_eq!(
            memory_headroom_verdict(Some(40), 70, 20),
            SpawnVerdict::Allow
        );
    }

    /// Exactly consuming the non-reserved remainder is a fit; one byte more is
    /// not. Stated because an off-by-one here either wedges a load that fits or
    /// admits one that does not.
    #[test]
    fn the_boundary_is_inclusive() {
        assert_eq!(
            memory_headroom_verdict(Some(80), 100, 20),
            SpawnVerdict::Allow
        );
        assert!(matches!(
            memory_headroom_verdict(Some(81), 100, 20),
            SpawnVerdict::Hold { .. }
        ));
    }

    /// The verdict must not depend on how big the host is. The same RATIO of
    /// need to capacity resolves the same way from a small board to a large
    /// server — the rule is scale-free, and a host is whatever shape it is.
    #[test]
    fn the_verdict_is_scale_free_across_host_sizes() {
        for total in [4u64, 16, 64, 128, 512, 2048].map(|g| g * GIB) {
            let available = total / 2;
            // Scaled with the host so the case stays meaningful at every size:
            // a fixed reserve exceeds a small host's available memory, which
            // tests the saturating subtraction rather than the rule.
            let reserve = available / 4;
            // A need that leaves room for the reserve fits at every scale.
            let fits = available.saturating_sub(reserve);
            assert_eq!(
                memory_headroom_verdict(Some(fits), available, reserve),
                SpawnVerdict::Allow,
                "total={total} should admit a need of {fits}"
            );
            // One byte past it does not, at every scale.
            assert!(
                matches!(
                    memory_headroom_verdict(Some(fits + 1), available, reserve),
                    SpawnVerdict::Hold { .. }
                ),
                "total={total} should refuse a need of {}",
                fits + 1
            );
        }
    }

    /// FAIL OPEN on a missing measurement. An unknown cut or a memory sensor
    /// that returned nothing must never be the reason a model cannot run —
    /// refusing on absent evidence would brick loads that are perfectly fine.
    #[test]
    fn a_missing_measurement_fails_open() {
        assert_eq!(host_share_need_bytes(1_000, 31, 0, None), None);
        assert_eq!(host_share_need_bytes(0, 31, 43, None), None);
        assert_eq!(memory_headroom_verdict(None, 100, 20), SpawnVerdict::Allow);
        assert_eq!(
            memory_headroom_verdict(Some(u64::MAX), 0, 20),
            SpawnVerdict::Allow
        );
    }

    /// A refusal has to be actionable on its own: what was needed, what was
    /// held back, what was there, and how to override it.
    #[test]
    fn the_hold_reason_states_the_shortfall_and_the_override() {
        let SpawnVerdict::Hold { reason } =
            memory_headroom_verdict(Some(60 * GIB), 70 * GIB, 20 * GIB)
        else {
            panic!("expected Hold");
        };
        assert!(reason.contains("host share"), "{reason}");
        assert!(reason.contains("available"), "{reason}");
        assert!(
            reason.contains("SOVEREIGN_LOCAL_FIT_RESERVE_GB"),
            "{reason}"
        );
    }

    /// The need term is the local fraction of the weights plus the shared
    /// overhead shape. Proportional to the cut, so shifting blocks onto a
    /// worker is a lever the gate can actually see.
    #[test]
    fn the_need_estimate_is_proportional_to_the_local_cut() {
        // Everything local: whole model + overhead.
        assert_eq!(host_share_need_bytes(80, 43, 43, None), Some(80 + 10 + GIB));
        // Half the blocks: half the weights, half the KV proxy.
        assert_eq!(host_share_need_bytes(80, 20, 40, None), Some(40 + 5 + GIB));
        // Everything lent out: only the scratch term remains.
        assert_eq!(host_share_need_bytes(80, 0, 43, None), Some(GIB));
        // Monotonic in the local cut.
        let mut prev = 0;
        for local in 0..=43 {
            let n = host_share_need_bytes(1_000_000, local, 43, None).unwrap();
            assert!(n >= prev, "need went down at local={local}");
            prev = n;
        }
    }

    /// With llama.cpp's projection the proxy terms are REPLACED, not added:
    /// the host's need is its weight share + its pro-rata KV + both compute
    /// buffers. This is what stops the `share/8` proxy from over-charging an
    /// MLA model ~5× and refusing a load that fits.
    #[test]
    fn the_projection_replaces_the_kv_proxy_when_present() {
        let o = sovereign_inference::embedded::PlanOverheads {
            context_total_bytes: 86,
            compute_accel_bytes: 7,
            compute_host_bytes: 11,
            model_host_bytes: 0,
        };
        // Half the blocks: half the weights (40), half the KV (43), both
        // compute terms — and NO share/8, NO flat GiB.
        assert_eq!(
            host_share_need_bytes(80, 20, 40, Some(&o)),
            Some(40 + 43 + 7 + 11)
        );
        // Everything lent out: only the compute terms remain.
        assert_eq!(host_share_need_bytes(80, 0, 40, Some(&o)), Some(7 + 11));
        // The unknown-cut guard is unchanged by the projection.
        assert_eq!(host_share_need_bytes(80, 20, 0, Some(&o)), None);
    }
}
