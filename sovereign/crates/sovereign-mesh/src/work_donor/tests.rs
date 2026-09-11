// SPDX-License-Identifier: AGPL-3.0-or-later
//! `work_donor`'s tests. A sibling file only so `work_donor.rs` stays
//! under ARCH §3.1's 1200-line ceiling — moved verbatim, nothing renamed.
use super::*;

fn section(kinds: &[&str]) -> WorkOfferSection {
    WorkOfferSection {
        kinds: kinds.iter().map(|k| k.to_string()).collect(),
        max_concurrent: 1,
        ..Default::default()
    }
}

/// **THE STARTUP GATE (ARCH §18.1, §18.3).**
///
/// The failing input is a config that offers `ingest:v1` on a daemon whose
/// registry holds only `process:v1`. Before the check existed this booted
/// happily and the donor advertised a kind it could not run: every unit of
/// it would be leased, fail with `NoExecutor`, burn an attempt, and come
/// back to the submitter as a verdict about this node.
///
/// The assertion is on the SENTENCE, not on `is_err()`. A refusal that
/// names nothing is not a refusal — an operator reading "invalid work
/// offer" has to guess which of their kinds is wrong, and with one kind
/// per line in a config file that is the whole of the information they
/// needed.
#[test]
fn startup_refuses_offer_of_unregistered_kind() {
    let registry = donor_registry(None, Sandbox::Direct);
    let err = resolve_offer(
        &section(&["ingest:v1"]),
        &registry,
        "linux",
        "x86_64",
        DONOR_ISOLATION,
    )
    .expect_err("a kind with no executor must refuse the boot");
    let msg = err.to_string();
    assert!(
        msg.contains("ingest:v1"),
        "the refusal must NAME the kind it refuses, got: {msg}"
    );
    assert!(
        msg.contains("no executor is registered") || msg.contains("no executor"),
        "the refusal must say what is missing, got: {msg}"
    );
}

/// An executor that asks only for what this build provides. It exists so the
/// control below has a subject: `process:v1` now requires
/// `RootlessContainer` and is refused here (see the watched red under it), so
/// a control written against it would assert the very thing being gated.
struct StubExecutor(JobKind);

impl commonwealth_work::executor::JobExecutor for StubExecutor {
    fn descriptor(&self) -> oicp_types::JobExecutorDescriptor {
        oicp_types::JobExecutorDescriptor {
            kind: self.0.clone(),
            isolation: DONOR_ISOLATION,
            parameters: serde_json::json!({"type": "object"}),
            examples: Vec::new(),
            idempotency: oicp_types::tool::Idempotency::Idempotent,
            lease_interval_ms: 1_000,
            est_secs: None,
            could_not_judge_exits: Vec::new(),
            verdict: kernel_types::quality::VerdictSource::ExitCode,
        }
    }
    fn validate(&self, _unit: &JobUnit) -> Result<(), commonwealth_work::refusal::WorkRefusal> {
        Ok(())
    }
    fn execute<'a>(
        &'a self,
        _unit: &'a JobUnit,
        _ctx: &'a commonwealth_work::executor::JobContext,
    ) -> commonwealth_work::executor::ExecuteFuture<'a> {
        unreachable!("the boot decision never runs a unit")
    }
}

/// The control. Without it the gate above passes the day `resolve_offer`
/// starts refusing everything — "no unregistered kind is offered" is
/// trivially true of a daemon that offers nothing. That day arrived when
/// `process:v1` was raised to require a container, which is exactly why this
/// control now stands on an executor whose demand the build MEETS.
#[test]
fn the_control_a_registered_kind_this_build_can_isolate_resolves_to_an_offer() {
    let kind = JobKind::parse("stub:v1").expect("kind");
    let mut registry = JobExecutorRegistry::new();
    registry
        .register(Arc::new(StubExecutor(kind)))
        .expect("one registration");
    let offer = resolve_offer(
        &section(&["stub:v1"]),
        &registry,
        "linux",
        "x86_64",
        DONOR_ISOLATION,
    )
    .expect("stub:v1 asks for exactly what this build provides")
    .expect("kinds are set, so there is an offer");
    assert_eq!(offer.kinds.len(), 1);
    assert_eq!(offer.os, "linux");
    assert_eq!(offer.isolation, DONOR_ISOLATION);
}

/// **THE ISOLATION FLOOR (ARCH §18.1, §18.3), operator decision 2026-09-10:
/// isolation is the default and there is no arbitrary execution outside a
/// well-defined boundary.**
///
/// The failing input is the SHIPPED config — a daemon that offers
/// `process:v1`. Running a submitter's argv unsandboxed reaches the donor's
/// user, filesystem, network and, through them, `~/.svrnmesh/node_key`: the
/// donor's own mesh identity is readable by the work it accepts. Consent was
/// the only wall (`Offer.accept_from`), and consent is not isolation
/// (`work_donor.rs:40-43`).
///
/// So `ProcessExecutor` declares it REQUIRES `RootlessContainer`, this build
/// provides `Subprocess`, and the boot refuses by name through the check that
/// already existed. This test must stay RED-on-removal: flip the descriptor
/// back to `Subprocess` and it is the only thing that turns, which is what
/// makes the floor structural rather than remembered (ARCH §7).
///
/// It goes green the day a container-backed executor raises `DONOR_ISOLATION`
/// — a mechanism, never a config key. Until then a daemon offering this kind
/// does not boot into donating, and that is deliberate.
#[test]
fn the_isolation_floor_drops_process_v1_and_publishes_no_offer() {
    let registry = donor_registry(None, Sandbox::Direct);
    let resolved = resolve_offer(
        &section(&["process:v1"]),
        &registry,
        "linux",
        "x86_64",
        DONOR_ISOLATION,
    )
    .expect("the floor drops the kind; it must NOT take the daemon down");
    assert_eq!(
        resolved, None,
        "with every configured kind dropped there is nothing to publish, and \
         an offer naming no kind would advertise a donor that refuses \
         everything"
    );
}

/// The other half, and the one that would catch a floor implemented as "give
/// up on the whole offer": a config naming BOTH kinds keeps the one this
/// build can isolate and loses only the one it cannot.
///
/// The failing input is a `continue` written as a `return Ok(None)` — a node
/// with a corpus engine would then quietly stop donating ingest work because
/// of a `process:v1` line it never used.
#[test]
fn the_floor_drops_only_the_kind_it_names_and_keeps_the_rest() {
    let kind = JobKind::parse("stub:v1").expect("kind");
    let mut registry = donor_registry(None, Sandbox::Direct);
    registry
        .register(Arc::new(StubExecutor(kind)))
        .expect("one registration");
    let offer = resolve_offer(
        &section(&["process:v1", "stub:v1"]),
        &registry,
        "linux",
        "x86_64",
        DONOR_ISOLATION,
    )
    .expect("a mixed config still resolves")
    .expect("one kind survives, so there is an offer");
    let kinds: Vec<String> = offer.kinds.iter().map(|k| k.to_string()).collect();
    assert_eq!(
        kinds,
        vec!["stub:v1".to_string()],
        "the floor must drop `process:v1` and keep what this build can \
         isolate, got: {kinds:?}"
    );
}

/// The zero value donates nothing and is not an error — the shipped
/// posture, and what makes the section safe to write into every config.
#[test]
fn an_empty_section_is_no_offer_rather_than_an_empty_offer() {
    let registry = donor_registry(None, Sandbox::Direct);
    assert_eq!(
        resolve_offer(
            &WorkOfferSection::default(),
            &registry,
            "linux",
            "x86_64",
            DONOR_ISOLATION
        )
        .expect("inert is not an error"),
        None
    );
}

/// A mis-spelled actor key is a set-membership test that silently never
/// matches — a donor that takes nothing with nothing red anywhere. Named
/// at boot instead.
#[test]
fn an_accept_key_that_is_not_a_key_refuses_the_boot_naming_it() {
    let registry = donor_registry(None, Sandbox::Direct);
    let mut s = section(&["process:v1"]);
    s.accept = sovereign_contracts::setup_config::WorkAcceptFrom::Listed;
    s.accept_from = vec!["BEEFYMAC".to_string()];
    let err = resolve_offer(&s, &registry, "linux", "x86_64", DONOR_ISOLATION)
        .expect_err("an unparseable accept key must refuse the boot");
    assert!(
        err.to_string().contains("BEEFYMAC"),
        "the refusal must name the entry, got: {err}"
    );
}

fn a_key(seed: char) -> ActorKey {
    ActorKey::parse(seed.to_string().repeat(64)).expect("64 hex chars")
}

/// A projection holding ONE unit in `status`. Built as a value rather
/// than folded from signed acts: the question here is what the donor DOES
/// with a fold, and `commonwealth-work`'s own tests already prove the
/// fold produces these states from the journal.
fn folded(status: WorkUnitStatus) -> (WorkProjection, UnitRef) {
    use commonwealth_work::projection::{ProjectedUnit, WorkHandoff};
    use sovereign_contracts::oicp::JobRequirements;

    let handoff = commonwealth_core::HandoffId::from_u128(7);
    let unit_hash = "a".repeat(64);
    let unit = JobUnit {
        kind: JobKind::parse("process:v1").expect("kind"),
        unit_hash: unit_hash.clone(),
        payload: serde_json::json!({}),
        requirements: JobRequirements::any(),
        tenant: None,
    };
    let mut units = std::collections::BTreeMap::new();
    units.insert(unit_hash.clone(), ProjectedUnit { unit, status });
    let mut proj = WorkProjection::default();
    proj.handoffs.insert(
        handoff,
        WorkHandoff {
            submitter: a_key('c'),
            kind: JobKind::parse("process:v1").expect("kind"),
            allowed: None,
            submitted_at_ms: 0,
            expires_at_ms: u64::MAX,
            revoked: None,
            units,
        },
    );
    (proj, UnitRef { handoff, unit_hash })
}

fn leased_to(lessee: ActorKey, expires_at_ms: u64) -> WorkUnitStatus {
    WorkUnitStatus::Leased {
        lessee,
        leased_at_ms: 0,
        last_renewed_ms: 0,
        expires_at_ms,
        attempts: 1,
    }
}

/// **The lost-lease decision.** Three ways a running unit stops being
/// ours, each of which must cancel it: another donor won the race, the
/// lease lapsed while we were running, and somebody already reported it.
///
/// The failing input that matters is the FIRST: two donors reading one
/// journal both append a `Lease` before either sees the other's, and the
/// loser is the one with a process group running. Reading "leased" as
/// "leased by me" is the defect — it is the same shape as the HTTP path's
/// `HeartbeatResult::Reclaimed`, which the peer loop dropped into a debug
/// catch-all for months.
#[test]
fn a_lease_taken_by_somebody_else_reads_as_lost_and_not_as_held() {
    let me = a_key('a');
    let them = a_key('b');

    let (proj, r) = folded(leased_to(me.clone(), 10_000));
    assert!(
        matches!(lease_state(&proj, &r, &me, 5_000), LeaseState::Held),
        "the control: my own live lease is held"
    );

    let (proj, r) = folded(leased_to(them.clone(), 10_000));
    match lease_state(&proj, &r, &me, 5_000) {
        LeaseState::Lost(why) => assert!(
            why.contains(them.as_str()),
            "the reason must name who took it, got: {why}"
        ),
        other => panic!("a lease held by another donor must read Lost, got {other:?}"),
    }

    // Lapsed while we ran. `status_at` reads it back as Queued, so it is
    // takeable by anybody — including us again — and the unit we still
    // have running is no longer covered by a lease.
    let (proj, r) = folded(leased_to(me.clone(), 1_000));
    assert!(
        matches!(lease_state(&proj, &r, &me, 5_000), LeaseState::Lost(_)),
        "a lease past its deadline is not held, even by the donor that took it"
    );

    // Already reported by us on a previous attempt, or by the winner.
    let (proj, r) = folded(WorkUnitStatus::Failed {
        last_lessee: me.clone(),
        reason: "spent".to_string(),
        attempts: 3,
        outcome: None,
    });
    assert!(matches!(
        lease_state(&proj, &r, &me, 5_000),
        LeaseState::Lost(_)
    ));

    // A unit that is no longer in the fold at all — a compacted journal, a
    // handoff revoked and swept. Lost, never Held, and never Unknown:
    // Unknown is reserved for "the journal could not be read", which is a
    // different fact and is why the enum has three arms (ARCH §18.2).
    let (proj, r) = folded(leased_to(me.clone(), 10_000));
    let gone = UnitRef {
        handoff: r.handoff,
        unit_hash: "f".repeat(64),
    };
    assert!(matches!(
        lease_state(&proj, &gone, &me, 5_000),
        LeaseState::Lost(_)
    ));
}

/// An UNPINNED unit reports the rev this donor actually ran at, not the
/// empty string.
///
/// This is what makes the plan's 5e bar (iii) possible: a stale donor's
/// unpinned verdict has to be FLAGGABLE by
/// `ComputeAttribution::comparable_to`, and two empty revs compare equal.
/// A workdir that is not a checkout reports a named absence instead —
/// `kernel_types::is_absent_marker` reads it as one.
#[test]
fn an_unpinned_unit_reports_the_rev_the_donor_actually_ran_at() {
    let unit = JobUnit {
        kind: JobKind::parse("process:v1").expect("kind"),
        unit_hash: "0".repeat(64),
        payload: serde_json::json!({}),
        requirements: Default::default(),
        tenant: None,
    };
    // This test runs inside the repo's own checkout.
    let here = repo_rev_of(&unit, Path::new(env!("CARGO_MANIFEST_DIR")));
    assert_eq!(here.len(), 40, "a resolved sha, got {here:?}");
    assert!(!kernel_types::is_absent_marker(&here));

    let empty = tempfile::tempdir().expect("tempdir");
    let nowhere = repo_rev_of(&unit, empty.path());
    assert!(
        kernel_types::is_absent_marker(&nowhere),
        "a workdir that is not a checkout must NAME the absence, got {nowhere:?}"
    );

    // **THE CASE PRODUCTION ACTUALLY PRODUCES, and the one this test was
    // missing.** The assertion above passes for an accidental reason — a
    // `tempdir` lands in `/tmp`, outside any checkout — while an unpinned unit
    // runs in `donor_root/scratch`, which is under a checkout exactly when the
    // daemon's data dir is. `git rev-parse` WALKS UP, so before the tracked
    // content check this returned the repo's HEAD for a directory holding
    // nothing of it: a fabricated rev that compares EQUAL to a submitter at
    // that rev. Empty on purpose — git does not track empty directories, so
    // this leaves `git status` clean, which the distributed path now requires.
    let nested = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir in the repo");
    let fabricated = repo_rev_of(&unit, nested.path());
    assert!(
        kernel_types::is_absent_marker(&fabricated),
        "a scratch dir that merely SITS under a checkout holds none of it — got {fabricated:?}, \
         which is this repo's HEAD attributed to work that never touched it"
    );

    let mut pinned = unit.clone();
    pinned.requirements.repo_rev = Some("deadbeef".to_string());
    assert_eq!(
        repo_rev_of(&pinned, empty.path()),
        "deadbeef",
        "a pinned unit ran in a worktree checked forward to its pin"
    );
}

/// One worktree per repo, named from the repo's URL rather than from its
/// position in the offer — so re-ordering `[[compute.work_offer.repos]]`
/// does not silently re-point two repos at each other's checkout.
#[test]
fn a_repos_worktree_is_keyed_by_its_url_not_its_position() {
    assert_eq!(
        stable_repo_key("https://github.com/x/commonwealth-ai.git"),
        "commonwealth-ai"
    );
    assert_eq!(
        stable_repo_key("git@host:x/commonwealth-ai/"),
        "commonwealth-ai"
    );
    assert_ne!(
        stable_repo_key("https://h/a/one.git"),
        stable_repo_key("https://h/a/two.git")
    );
}

/// A precondition this build cannot evaluate is REFUSED, never assumed
/// met. The failing input is a `SlotDecodes`, which the mesh crate has no
/// way to check: reading it as satisfied would return a verdict from a
/// host that did not meet the unit's terms.
#[test]
fn a_precondition_this_build_cannot_check_is_refused_not_assumed() {
    let mut unit = JobUnit {
        kind: JobKind::parse("process:v1").expect("kind"),
        unit_hash: "0".repeat(64),
        payload: serde_json::json!({}),
        requirements: Default::default(),
        tenant: None,
    };
    unit.requirements.preconditions = vec![Precondition::SlotDecodes("primary".to_string())];
    let err = host_satisfies(&unit).expect_err("an uncheckable precondition is not met");
    assert_eq!(err.id(), "requirement-unmet", "got: {err}");

    unit.requirements.preconditions = vec![Precondition::Binary("git".to_string())];
    assert!(
        host_satisfies(&unit).is_ok(),
        "the control: `git` is on PATH in every environment this test runs in"
    );
}

// -----------------------------------------------------------------
// The credit (cw-lift 5h)
// -----------------------------------------------------------------

fn a_unit() -> JobUnit {
    JobUnit {
        kind: JobKind::parse("process:v1").expect("kind"),
        unit_hash: "a".repeat(64),
        payload: serde_json::json!({}),
        requirements: Default::default(),
        tenant: None,
    }
}

fn a_provenance() -> ComputeAttribution {
    ComputeAttribution {
        repo_rev: "0".repeat(40),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        toolchain: "rustc 1.0.0".to_string(),
        host: Server::Local,
    }
}

/// **THE CREDIT, and the failing input is the whole of 5d and 5e.** The work
/// plane ran other members' compute and its contribution ledger recorded
/// nothing about who paid for it: every gate was green, `svrn job status`
/// showed the verdicts, and `commonwealth balance` showed a donor that had
/// donated nothing.
///
/// The assertions are on the FIELDS, not on `is_some()`. A credit that names
/// no unit is not auditable against the journal it claims to describe, and
/// "some event was emitted" is exactly the shape of green that ARCH §18.1
/// says is not a check.
#[test]
fn a_completed_unit_credits_this_node_with_the_time_it_actually_spent() {
    use commonwealth_core::contributions::LedgerEventKind;

    let me = a_key('a');
    let unit = a_unit();
    let handoff = commonwealth_core::HandoffId::from_u128(7);
    let act = WorkAct::Complete(Completion {
        handoff,
        unit_hash: unit.unit_hash.clone(),
        outcome: kernel_types::Judgement::passed(
            "unit",
            kernel_types::Reason::literal("8412 passed, 0 failed"),
        ),
        result: serde_json::json!({ "exit_code": 0 }),
        provenance: a_provenance(),
    });

    match credit_for(&act, &unit, &me, 42.5) {
        Some(LedgerEventKind::JobUnitCompleted {
            handoff: h,
            unit_hash,
            donor_actor,
            kind,
            wall_seconds,
        }) => {
            assert_eq!(h, handoff, "the credit must point at the handoff it ran");
            assert_eq!(unit_hash, unit.unit_hash, "and at the unit");
            assert_eq!(
                donor_actor,
                me.as_str(),
                "the donor is the key that SIGNED the report — ARCH §7.5, never \
                 a self-reported id"
            );
            assert_eq!(kind, unit.kind);
            assert!((wall_seconds - 42.5).abs() < 1e-9);
        }
        other => panic!("a completion must credit this node, got {other:?}"),
    }
}

/// The negative half, and the one that keeps the test above from passing
/// vacuously on a `credit_for` that credits everything.
///
/// A `Fail` is the plane failing to run the work, not the work coming back
/// red — `WorkUnitStatus` keeps those apart and so does this. Crediting a
/// failure would pay a donor for burning the submitter's attempts.
#[test]
fn a_unit_that_never_reached_a_verdict_credits_nothing() {
    let me = a_key('a');
    let unit = a_unit();
    let act = WorkAct::Fail(Failure {
        handoff: commonwealth_core::HandoffId::from_u128(7),
        unit_hash: unit.unit_hash.clone(),
        outcome: kernel_types::Judgement::could_not_judge(
            "unit",
            kernel_types::Reason::literal("killed on timeout"),
        ),
        provenance: a_provenance(),
    });
    assert!(
        credit_for(&act, &unit, &me, 42.5).is_none(),
        "a unit the plane could not run is not a donation"
    );
}

/// A RED shard is a completed unit and the donor is paid for it. The verdict
/// belongs to the submitter's tree, the compute belongs to the donor, and
/// conflating them would make every donor prefer submitters whose tests are
/// green.
#[test]
fn a_red_shard_is_still_a_donation() {
    let me = a_key('a');
    let unit = a_unit();
    let act = WorkAct::Complete(Completion {
        handoff: commonwealth_core::HandoffId::from_u128(7),
        unit_hash: unit.unit_hash.clone(),
        outcome: kernel_types::Judgement::failed("unit", kernel_types::Reason::literal("3 failed")),
        result: serde_json::json!({ "exit_code": 101 }),
        provenance: a_provenance(),
    });
    assert!(
        credit_for(&act, &unit, &me, 1.0).is_some(),
        "the unit did its job; the verdict is about the submitter's tree"
    );
}

/// Acts that are not reports credit nothing. `run_unit` builds only
/// `Complete` and `Fail`, so this pins the wildcard arm rather than a
/// reachable path — a future act that starts arriving here must not be
/// silently paid.
#[test]
fn an_act_that_is_not_a_report_credits_nothing() {
    let me = a_key('a');
    let unit = a_unit();
    let r = UnitRef {
        handoff: commonwealth_core::HandoffId::from_u128(7),
        unit_hash: unit.unit_hash.clone(),
    };
    assert!(credit_for(&WorkAct::Lease(r.clone()), &unit, &me, 9.0).is_none());
    assert!(credit_for(&WorkAct::Renew(r), &unit, &me, 9.0).is_none());
}

// ── cw-lift 5g: the ingest executor's half of the boot invariant ──────────

fn an_engine() -> (tempfile::TempDir, Arc<corpus_engine::CorpusEngine>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let recipes = dir.path().join("recipes");
    let indexes = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes).expect("recipes dir");
    std::fs::create_dir_all(&indexes).expect("indexes dir");
    let embed: corpus_engine::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }));
    (
        dir,
        Arc::new(corpus_engine::CorpusEngine::new(recipes, indexes, embed)),
    )
}

/// The control for `startup_refuses_offer_of_unregistered_kind`, which passes
/// `None` and therefore now asserts something sharper than it used to: a node
/// with NO corpus engine refuses a config offering `ingest:v1`, naming it.
///
/// Without this control that gate is trivially satisfiable by a
/// `donor_registry` that registers nothing at all. The failing input here is a
/// registration that forgets the engine arm — the daemon would boot, refuse
/// the operator's `ingest:v1` line, and the refusal would be indistinguishable
/// from a genuine misconfiguration.
#[test]
fn a_node_with_a_corpus_engine_registers_the_ingest_kind_and_can_offer_it() {
    let (_dir, engine) = an_engine();
    let registry = donor_registry(Some(engine), Sandbox::Direct);
    let kinds: Vec<String> = registry.kinds().iter().map(|k| k.to_string()).collect();
    assert!(
        kinds
            .iter()
            .any(|k| k == crate::ingest_executor::INGEST_KIND),
        "a node with an engine must register `ingest:v1`, got {kinds:?}"
    );
    let offer = resolve_offer(
        &section(&["ingest:v1"]),
        &registry,
        "linux",
        "x86_64",
        DONOR_ISOLATION,
    )
    .expect("ingest:v1 is registered on a node with an engine")
    .expect("kinds are set, so there is an offer");
    assert_eq!(offer.kinds.len(), 1);
    assert_eq!(offer.isolation, DONOR_ISOLATION);
}
