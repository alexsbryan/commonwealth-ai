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
    let registry = donor_registry();
    let err = resolve_offer(&section(&["ingest:v1"]), &registry, "linux", "x86_64")
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

/// The control. Without it the gate above passes the day `resolve_offer`
/// starts refusing everything — "no unregistered kind is offered" is
/// trivially true of a daemon that offers nothing.
#[test]
fn the_control_a_registered_kind_resolves_to_an_offer() {
    let registry = donor_registry();
    let offer = resolve_offer(&section(&["process:v1"]), &registry, "linux", "x86_64")
        .expect("process:v1 is registered")
        .expect("kinds are set, so there is an offer");
    assert_eq!(offer.kinds.len(), 1);
    assert_eq!(offer.os, "linux");
    assert_eq!(offer.isolation, DONOR_ISOLATION);
}

/// The zero value donates nothing and is not an error — the shipped
/// posture, and what makes the section safe to write into every config.
#[test]
fn an_empty_section_is_no_offer_rather_than_an_empty_offer() {
    let registry = donor_registry();
    assert_eq!(
        resolve_offer(&WorkOfferSection::default(), &registry, "linux", "x86_64")
            .expect("inert is not an error"),
        None
    );
}

/// A mis-spelled actor key is a set-membership test that silently never
/// matches — a donor that takes nothing with nothing red anywhere. Named
/// at boot instead.
#[test]
fn an_accept_key_that_is_not_a_key_refuses_the_boot_naming_it() {
    let registry = donor_registry();
    let mut s = section(&["process:v1"]);
    s.accept = sovereign_contracts::setup_config::WorkAcceptFrom::Listed;
    s.accept_from = vec!["BEEFYMAC".to_string()];
    let err = resolve_offer(&s, &registry, "linux", "x86_64")
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
    let here = attribution(&unit, Path::new(env!("CARGO_MANIFEST_DIR")));
    assert_eq!(
        here.repo_rev.len(),
        40,
        "a resolved sha, got {:?}",
        here.repo_rev
    );
    assert!(!kernel_types::is_absent_marker(&here.repo_rev));

    let empty = tempfile::tempdir().expect("tempdir");
    let nowhere = attribution(&unit, empty.path());
    assert!(
        kernel_types::is_absent_marker(&nowhere.repo_rev),
        "a workdir that is not a checkout must NAME the absence, got {:?}",
        nowhere.repo_rev
    );

    let mut pinned = unit.clone();
    pinned.requirements.repo_rev = Some("deadbeef".to_string());
    assert_eq!(
        attribution(&pinned, empty.path()).repo_rev,
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
