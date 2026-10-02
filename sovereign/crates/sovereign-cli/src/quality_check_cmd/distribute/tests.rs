// SPDX-License-Identifier: AGPL-3.0-or-later
//! `distribute`'s tests. A sibling file only so `distribute.rs` stays under
//! ARCH §3.1's ceiling (pb-work-doors) — moved verbatim.

/// **THE FALSE ACCEPT, and why the refusal cannot live in `comparable_to`.**
///
/// A submitter with uncommitted changes pins `HEAD`; a donor checks that
/// same rev out into a clean worktree; both attributions carry the identical
/// sha. The last assertion here is the important one — those two
/// attributions DO compare equal, because by the time `comparable_to` looks
/// there is nothing left to see. The apparatus cannot catch this, so the
/// only place it can be caught is where the rev is minted.
///
/// Failing input: the `uncommitted` check removed from `head_rev`, which is
/// how this stood until 2026-09-10. Watched — with it removed, `head_rev`
/// hands back the sha and the `expect_err` below fails.
///
/// The control comes FIRST on purpose: a check that refused every tree would
/// satisfy the dirty half while making the tool useless.
#[test]
fn a_dirty_checkout_has_no_revision_to_pin_and_the_comparison_cannot_tell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git runs");
        assert!(
            ok.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "t@example.invalid"]);
    git(&["config", "user.name", "t"]);
    git(&["commit", "--quiet", "--allow-empty", "-m", "one"]);

    // CONTROL: a clean checkout pins, and what it pins is a real sha.
    let clean = head_rev(repo).expect("a clean checkout pins its HEAD");
    assert_eq!(clean.len(), 40, "a resolved sha, got {clean:?}");

    // An UNTRACKED file is enough, and that is deliberate: a new
    // `#[test]` nobody committed changes the very count a distributed run
    // compares.
    std::fs::write(repo.join("new_test.rs"), b"#[test] fn t() {}").expect("write");
    let err = head_rev(repo).expect_err("a dirty checkout must refuse to pin");
    assert!(
        err.contains("uncommitted") && err.contains("new_test.rs"),
        "the refusal must name what is dirty, got {err:?}"
    );

    // A tracked modification is the same fact by another route.
    git(&["add", "new_test.rs"]);
    git(&["commit", "--quiet", "-m", "two"]);
    head_rev(repo).expect("committing it makes the tree pinnable again");
    std::fs::write(repo.join("new_test.rs"), b"#[test] fn t() { panic!() }").expect("write");
    head_rev(repo).expect_err("a modified tracked file must refuse too");

    // AND THE REASON IT HAD TO BE CAUGHT UPSTREAM: two readings of the same
    // sha compare EQUAL. If the refusal above were ever removed, this is
    // what would adopt a donor's verdict about different bytes.
    let submitter = attribution(&clean);
    let donor = attribution(&clean);
    assert!(
        submitter.comparable_to(&donor),
        "identical revs compare equal — which is exactly why a dirty tree must never mint one"
    );
}
use super::*;
use kernel_types::quality::Registry;

/// `commonwealth_work::attribution::ABSENT_TOOLCHAIN`'s spelling — the
/// reading a host without `rustc` reports. That reader runs in cw-rails
/// now (pb-work-doors); these tests need only a named absence, and assert
/// that this one is one.
const ABSENT_TOOLCHAIN: &str = "unknown (rustc is not on the PATH where this work ran)";

fn reg() -> Registry {
    Registry::parse(super::super::tests::TABLE).expect("parses")
}

fn lane(reg: &Registry, id: &str) -> Instrument {
    reg.instruments
        .iter()
        .find(|i| i.id == id)
        .expect("the fixture lane")
        .clone()
}

/// A venue whose budget is a REPORT rather than a kill, which neither
/// fixture trigger in `super::super::tests::TABLE` is — both take the
/// parser's `kill` default, so asserting the other arm against one of them
/// would assert nothing. The parser refuses a trigger no instrument
/// declares, so the row comes with it.
const REPORT_VENUE: &str = r#"
censused_surfaces = [".github/workflows"]

[[instrument]]
id = "docs-gate"
kind = "gate"
claim = "invariant"
command = "cargo xtask docs-gate"
cost_secs = 2.2
enforcement = "hard"
fidelity = "F0"
baseline = { kind = "none" }
negative_control = "none"
runs_in = ["precommit"]
doc = "ARCH §1.1"

[[trigger]]
id = "precommit"
budget_secs = 60
overrun = "report"
on_fail = "report"
"#;

fn actor(seed: u8) -> ActorKey {
    ActorKey::parse(format!("{:02x}", seed).repeat(32)).expect("canonical hex")
}

fn attribution(rev: &str) -> ComputeAttribution {
    ComputeAttribution {
        repo_rev: rev.to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        toolchain: "rustc 1.90.0 (deadbeef 2026-01-01)".to_string(),
        host: Server::Local,
    }
}

fn complete(rev: &str, outcome: Judgement) -> WorkUnitStatus {
    WorkUnitStatus::Complete {
        lessee: actor(0xab),
        completed_at_ms: 1_000,
        attempts: 1,
        outcome,
        result: serde_json::json!({
            "exit_code": 0,
            "duration_ms": 42_000u64,
            "stdout": "…",
            "stderr": "",
        }),
        provenance: attribution(rev),
    }
}

/// TWO HOSTS THAT BOTH COULD NOT READ `rustc` DO NOT THEREBY AGREE, and
/// this is the end-to-end half of the rule that lives in
/// [`ComputeAttribution::comparable_to`].
///
/// Failing input: the submitter's toolchain AND the donor's both set to
/// the same named absence — which is the state this checkout produces on
/// any machine without `rustc` on `PATH`, on both sides at once.
///
/// THIS CASE WAS UNREACHABLE UNTIL THE READERS CONVERGED, which is why it
/// is added with them. There were two `rustc --version` readers spelling
/// their absence differently ("this checkout's PATH" against "this
/// donor's PATH"), so two unreadable hosts compared unequal by accident of
/// authorship and the merge did the right thing for the wrong reason. One
/// reader means one spelling, and one spelling would have compared EQUAL —
/// adopting a donor verdict about a compiler neither host could name.
/// Revert `comparable_to` to plain field equality and this test goes red
/// with `Passed`; that sabotage was watched before this was believed.
#[test]
fn two_hosts_that_both_lost_their_toolchain_do_not_agree() {
    assert!(kernel_types::is_absent_marker(ABSENT_TOOLCHAIN));
    let r = reg();
    let inst = lane(&r, "docs-gate");
    let rev = "aa".repeat(20);

    let mut mine = attribution(&rev);
    mine.toolchain = ABSENT_TOOLCHAIN.to_string();

    let mut theirs = complete(
        &rev,
        Judgement::passed("process:v1 x", Reason::literal("exit 0")),
    );
    if let WorkUnitStatus::Complete { provenance, .. } = &mut theirs {
        provenance.toolchain = ABSENT_TOOLCHAIN.to_string();
    }
    // Byte-identical, and still not evidence about each other.
    if let WorkUnitStatus::Complete { provenance, .. } = &theirs {
        assert_eq!(mine.toolchain, provenance.toolchain);
    }

    let row = terminal_row(&inst, &theirs, &mine);
    assert_eq!(
        row.judgement.verdict(),
        Verdict::CouldNotJudge,
        "a verdict neither host could attribute must not be adopted"
    );
    assert!(
        row.judgement.reason().as_str().contains("toolchain"),
        "the row must NAME the field that could not be compared: {}",
        row.judgement.reason().as_str()
    );
}

/// **GATE (iii), WATCHED.** A donor one commit behind produces a
/// perfectly well-formed `passed` about a tree that is not this one, and
/// the merge must refuse to adopt it.
///
/// Failing input: the same `Complete` at `HEAD~1`. Delete the
/// `comparable_to` guard in [`terminal_row`] and this row reads `passed`
/// — a green nobody earned, from a machine the operator never looked at.
/// That is the bar WORK_PLANE.md §The pilot names as the one to watch
/// failing before the pin is trusted.
#[test]
fn a_donor_one_commit_behind_is_could_not_judge_and_names_both_revs() {
    let r = reg();
    let inst = lane(&r, "docs-gate");
    let mine = attribution("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let theirs = complete(
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        Judgement::passed(
            format!("process:v1 {}", "cc".repeat(32)),
            Reason::literal("exit 0"),
        ),
    );
    let row = terminal_row(&inst, &theirs, &mine);
    assert_eq!(
        row.judgement.verdict(),
        Verdict::CouldNotJudge,
        "a verdict from another rev must not be adopted: {}",
        row.judgement.reason().as_str()
    );
    let why = row.judgement.reason().as_str();
    assert!(why.contains("aaaaaaaaaaaa"), "{why}");
    assert!(why.contains("bbbbbbbbbbbb"), "{why}");
    // The row still names the node that produced it — an unusable verdict
    // is still evidence about WHO produced it.
    assert_eq!(row.node.as_deref(), Some(actor(0xab).as_str()));

    // And the same donor at the SAME rev is adopted, so the assertion
    // above is about comparability and not about the guard always firing.
    let same = complete(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Judgement::passed(
            format!("process:v1 {}", "cc".repeat(32)),
            Reason::literal("exit 0"),
        ),
    );
    assert_eq!(
        terminal_row(&inst, &same, &mine).judgement.verdict(),
        Verdict::Passed
    );
}

/// An unreadable toolchain on either side is a NAMED absence, and two
/// named absences are not a match. Failing input: `toolchain: String
/// ::new()` on both sides, which would compare equal and read as "the
/// same compiler" about two hosts neither of which could say.
#[test]
fn an_unreadable_toolchain_never_compares_equal_to_a_guess() {
    assert!(kernel_types::is_absent_marker(ABSENT_TOOLCHAIN));
    let r = reg();
    let inst = lane(&r, "docs-gate");
    let rev = "aa".repeat(20);
    let mut mine = attribution(&rev);
    mine.toolchain = ABSENT_TOOLCHAIN.to_string();
    let theirs = complete(
        &rev,
        Judgement::passed("process:v1 x", Reason::literal("exit 0")),
    );
    let row = terminal_row(&inst, &theirs, &mine);
    assert_eq!(row.judgement.verdict(), Verdict::CouldNotJudge);
    assert!(
        row.judgement.reason().as_str().contains("toolchain"),
        "{}",
        row.judgement.reason().as_str()
    );
}

/// **GATE (ii).** A shard the cohort refuses is a ROW carrying the typed
/// refusal, never an absence. Failing input: a donor whose checkout cannot
/// resolve the pinned rev — `RequirementUnmet(RepoRev)`, the refusal
/// `work_donor::resolve_workdir` constructs.
///
/// Delete the `unplaced_row` arm from [`merge`] and this instrument simply
/// stops appearing: the table gets shorter and greener than a local run at
/// the same rev, which is the exact defect `cw-work-ci-offload`'s block
/// says a share-only bar would score as a win.
#[test]
fn a_shard_refused_for_its_rev_is_a_row_that_names_the_refusal() {
    use oicp_types::work::UnmetRequirement;
    let r = reg();
    let inst = lane(&r, "docs-gate");
    let refusal = WorkRefusal::RequirementUnmet(UnmetRequirement::RepoRev {
        required: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        host: "deadbeef does not have aaaaaaaa".into(),
    });
    let row = unplaced_row(&inst, &[(actor(0x11), Err(refusal))]);
    assert_eq!(row.judgement.verdict(), Verdict::NeverRan);
    let why = row.judgement.reason().as_str();
    assert!(why.contains("requirement-unmet"), "{why}");
    assert!(why.contains("pinned to rev"), "{why}");
    assert!(row.node.is_none(), "nobody ran it, so no node produced it");
}

/// A cohort that COULD take the unit and did not is a different fact from
/// one that refused, and the two must not collapse: the first is
/// could-not-judge naming the host-side checks this node cannot see, the
/// second is never-ran naming what the rail refused.
#[test]
fn a_willing_cohort_that_ran_out_of_budget_is_could_not_judge_not_never_ran() {
    let r = reg();
    let inst = lane(&r, "docs-gate");
    let row = unplaced_row(&inst, &[(actor(0x22), Ok(()))]);
    assert_eq!(row.judgement.verdict(), Verdict::CouldNotJudge);
    let why = row.judgement.reason().as_str();
    assert!(why.contains("host-side half"), "{why}");

    // An empty ring is neither of those: nobody was asked.
    let none = unplaced_row(&inst, &[]);
    assert_eq!(none.judgement.verdict(), Verdict::NeverRan);
    assert!(
        none.judgement.reason().as_str().contains("work_offer"),
        "{}",
        none.judgement.reason().as_str()
    );
}

/// **The verdict source comes OFF THE ROW.** Failing input: an exit-code
/// instrument submitted with `ResultSource::VerdictLine` — the donor would
/// look for a JSON judgement on its last stdout line, find a cargo
/// summary, and report `no-verdict` for a gate that ran fine.
#[test]
fn an_exit_code_instrument_is_not_submitted_under_the_lane_protocol() {
    let r = reg();
    assert_eq!(
        result_source(&lane(&r, "docs-gate")),
        ResultSource::Stdout,
        "docs-gate reads its verdict from an exit code"
    );
    assert_eq!(
        result_source(&lane(&r, "chat-ask")),
        ResultSource::VerdictLine,
        "chat-ask SAYS its verdict"
    );
    // And the two agree with `commonwealth-work`'s own inverse mapping, so
    // there is one correspondence rather than two.
    for inst in [lane(&r, "docs-gate"), lane(&r, "chat-ask")] {
        assert_eq!(result_source(&inst).verdict_source(), inst.verdict);
    }
}

/// Every unit pins the rev, the os, the arch AND the row's preconditions.
/// Failing input: dropping `preconditions` — the unit would be leased by a
/// donor that cannot meet them and report a failure about the donor.
#[test]
fn a_unit_pins_the_rev_the_platform_and_the_rows_preconditions() {
    let r = reg();
    let inst = lane(&r, "chat-ask");
    assert!(!inst.preconditions.is_empty(), "the fixture lane has some");
    let req = requirements_for(&inst, "deadbeef");
    assert_eq!(req.repo_rev.as_deref(), Some("deadbeef"));
    assert_eq!(req.os.as_deref(), Some(std::env::consts::OS));
    assert_eq!(req.arch.as_deref(), Some(std::env::consts::ARCH));
    assert_eq!(req.preconditions, inst.preconditions);
}

/// The wall cap follows the VENUE's overrun policy, which is the same
/// decider the local path uses. Failing input: a `kill` venue whose units
/// carry `MAX_TTL_SECS` — the budget would stop reading while donors ran
/// on for hours.
#[test]
fn the_wall_cap_is_the_venues_own_overrun_policy() {
    let r = reg();
    let inst = lane(&r, "docs-gate");
    let kill = r
        .trigger(&kernel_types::quality::RunsIn::Check)
        .expect("check");
    assert_eq!(kill.overrun, Overrun::Kill);
    assert_eq!(wall_cap_secs(&inst, kill, 1800), 1800);

    // A `report` venue is spelled out rather than borrowed from the
    // fixture: BOTH fixture triggers take the parser's `kill` default, so
    // asserting the other arm against one of them would assert nothing.
    let reported = Registry::parse(REPORT_VENUE).expect("parses");
    let report = reported
        .trigger(&kernel_types::quality::RunsIn::Precommit)
        .expect("precommit");
    assert_eq!(report.overrun, Overrun::Report);
    assert_eq!(wall_cap_secs(&inst, report, 60), MAX_TTL_SECS);

    // The reservation is a floor, never a ceiling.
    assert!(wall_cap_secs(&inst, kill, 1) >= inst.reservation_secs());
}

/// **GATE (ii) IN THE MERGE ITSELF.** A shard the cohort refused is still
/// a ROW in the merged table, carrying the typed refusal.
///
/// The projection is built by hand and holds one QUEUED unit, and the
/// survey cw-rails' refusals door answered for it is one offer that cannot
/// run it — a donor on another OS, which is the `RequirementUnmet`
/// `may_take` can actually reach from a submitter (`repo_rev` is decided
/// donor-side and never reaches the rail; see the module docs). That the
/// door runs `may_take` is commonwealth-rails' `work_doors` test.
///
/// Failing input: delete the `Queued` arm from [`merge`]. The instrument
/// vanishes, the table gets shorter and greener than a local run at the
/// same rev, and a share-only bar scores that as a win — the exact defect
/// `cw-work-ci-offload`'s block was written about.
#[test]
fn a_queued_shard_the_cohort_refused_is_still_a_row_in_the_merged_table() {
    use oicp_types::work::{ProjectedUnit, UnmetRequirement, WorkHandoff};

    let r = reg();
    let inst = lane(&r, "docs-gate");
    let lanes: Vec<&Instrument> = vec![&inst];
    let rev = "aa".repeat(20);
    let unit = JobUnit {
        kind: JobKind::parse(PROCESS_KIND).expect("kind"),
        unit_hash: "cc".repeat(32),
        payload: serde_json::json!({}),
        requirements: requirements_for(&inst, &rev),
        tenant: None,
    };
    let hashes = vec![unit.unit_hash.clone()];
    let handoff = HandoffId::from_u128(9);

    let mut by_hash = BTreeMap::new();
    by_hash.insert(
        hashes[0].clone(),
        ProjectedUnit {
            unit: unit.clone(),
            status: WorkUnitStatus::Queued { prior_attempts: 0 },
        },
    );
    let mut handoffs = BTreeMap::new();
    handoffs.insert(
        handoff,
        WorkHandoff {
            submitter: actor(0x33),
            kind: unit.kind.clone(),
            allowed: None,
            submitted_at_ms: 1,
            expires_at_ms: 1_000_000,
            revoked: None,
            units: by_hash,
        },
    );
    let proj = WorkProjection {
        handoffs,
        ..WorkProjection::default()
    };
    let mut surveys = Surveys::new();
    surveys.insert(
        hashes[0].clone(),
        vec![(
            actor(0x44),
            Err(WorkRefusal::RequirementUnmet(UnmetRequirement::Os {
                required: std::env::consts::OS.to_string(),
                host: "plan9".to_string(),
            })),
        )],
    );

    let rows = merge(
        &lanes,
        &hashes,
        &proj,
        &handoff,
        100,
        &attribution(&rev),
        &surveys,
    );
    let row = rows
        .get(&inst.id)
        .expect("a refused shard is a row, never an absence");
    assert_eq!(row.judgement.verdict(), Verdict::NeverRan);
    let why = row.judgement.reason().as_str();
    assert!(why.contains("requirement-unmet"), "{why}");
    assert!(why.contains("plan9"), "{why}");
    assert!(row.node.is_none(), "nobody ran it");

    // A queued unit cw-rails did not survey is not "nobody offered".
    let unsurveyed = merge(
        &lanes,
        &hashes,
        &proj,
        &handoff,
        100,
        &attribution(&rev),
        &Surveys::new(),
    );
    assert_eq!(
        unsurveyed[&inst.id].judgement.verdict(),
        Verdict::CouldNotJudge
    );
}

/// **Two instruments that submit the same unit are REFUSED at the door.**
///
/// Failing input, and it is the one this cost a live run: two rows whose
/// `command` is `/usr/bin/true` and whose only difference is a
/// `precondition`. `seal::unit_hash` covers the kind and the payload and
/// NOT the requirements, so the two are one unit; the fold keeps one, its
/// preconditions are whichever row was submitted last, and both table rows
/// then report on a computation only one of them asked for.
///
/// The units here stand for what cw-rails' seal door answers for those two
/// rows: one identity for both.
///
/// Delete `distinct_units` and this goes green while the run it describes
/// is wrong in a way no verdict shows.
#[test]
fn two_instruments_that_submit_one_unit_are_refused_by_name() {
    let r = Registry::parse(COLLIDING).expect("parses");
    let a = r.instruments[0].clone();
    let b = r.instruments[1].clone();
    let lanes: Vec<&Instrument> = vec![&a, &b];
    let unit = |hash: &str, inst: &Instrument| JobUnit {
        kind: JobKind::parse(PROCESS_KIND).expect("kind"),
        unit_hash: hash.to_string(),
        payload: serde_json::json!({ "argv": inst.argv() }),
        requirements: requirements_for(inst, "deadbeef"),
        tenant: None,
    };
    let one = "11".repeat(32);
    let err = distinct_units(&lanes, vec![unit(&one, &a), unit(&one, &b)]).expect_err("refused");
    assert!(err.contains("twin-a") && err.contains("twin-b"), "{err}");
    assert!(err.contains("SAME unit"), "{err}");

    // And two rows that genuinely differ are accepted, so the refusal is
    // about collision rather than about there being more than one row.
    let two = "22".repeat(32);
    assert_eq!(
        distinct_units(&lanes, vec![unit(&one, &a), unit(&two, &b)])
            .expect("distinct")
            .len(),
        2
    );
}

/// Two rows, one command. The registry is legal; the SUBMISSION is not.
const COLLIDING: &str = r#"
censused_surfaces = [".github/workflows"]

[[instrument]]
id = "twin-a"
kind = "gate"
claim = "invariant"
command = "/usr/bin/true"
cost_secs = 1.0
enforcement = "hard"
fidelity = "F0"
baseline = { kind = "none" }
negative_control = "none"
runs_in = ["precommit"]
doc = "cw-lift 5e"

[[instrument]]
id = "twin-b"
kind = "gate"
claim = "invariant"
command = "/usr/bin/true"
cost_secs = 1.0
enforcement = "hard"
fidelity = "F0"
preconditions = ["port-listening:9741"]
baseline = { kind = "none" }
negative_control = "none"
runs_in = ["precommit"]
doc = "cw-lift 5e"

[[trigger]]
id = "precommit"
budget_secs = 60
on_fail = "report"
"#;

/// **A refusal survey is taken while the work was still on offer.**
///
/// Failing input: a handoff whose TTL is the venue budget — every one of
/// them — surveyed at the merge's own clock, one tick after the budget
/// expired. `WorkHandoff::admits` is false for everybody then, so every
/// unplaced row reads `not-allowed` and the reason nobody took the unit is
/// gone. Watched on the first live 5e run, both unplaced rows.
#[test]
fn the_refusal_survey_reads_the_window_the_work_was_offered_in() {
    use oicp_types::work::WorkHandoff;
    let handoff = HandoffId::from_u128(5);
    let mut handoffs = BTreeMap::new();
    handoffs.insert(
        handoff,
        WorkHandoff {
            submitter: actor(0x55),
            kind: JobKind::parse("process:v1").expect("kind"),
            allowed: None,
            submitted_at_ms: 1_000,
            expires_at_ms: 2_000,
            revoked: None,
            units: BTreeMap::new(),
        },
    );
    let proj = WorkProjection {
        handoffs,
        ..WorkProjection::default()
    };
    // Past the window: clamped back inside it.
    assert_eq!(survey_ms(&proj, &handoff, 9_999), 1_999);
    // Inside it: untouched.
    assert_eq!(survey_ms(&proj, &handoff, 1_500), 1_500);
    // A handoff the fold does not hold cannot be clamped, and is not.
    assert_eq!(survey_ms(&proj, &HandoffId::from_u128(6), 9_999), 9_999);
}

/// **Every selected instrument gets a row, even when the fold lost its
/// unit.** Failing input: a projection that holds no handoff at all — the
/// shape a submission signed by a key outside the `work` roster produces,
/// where the act is an `UnknownSigner` gap rather than an act.
#[test]
fn merge_is_total_over_the_selection_even_on_an_empty_fold() {
    let r = reg();
    let a = lane(&r, "docs-gate");
    let b = lane(&r, "chat-ask");
    let lanes: Vec<&Instrument> = vec![&a, &b];
    let hashes = vec!["11".repeat(32), "22".repeat(32)];
    let proj = WorkProjection::default();
    let rows = merge(
        &lanes,
        &hashes,
        &proj,
        &HandoffId::from_u128(7),
        0,
        &attribution("deadbeef"),
        &Surveys::new(),
    );
    assert_eq!(rows.len(), 2, "a lost unit is a row, never an absence");
    for inst in &lanes {
        let row = rows.get(&inst.id).expect("row");
        assert_eq!(row.judgement.verdict(), Verdict::NeverRan);
        assert!(
            row.judgement.reason().as_str().contains("roster"),
            "{}",
            row.judgement.reason().as_str()
        );
    }
}
