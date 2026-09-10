// SPDX-License-Identifier: AGPL-3.0-or-later
//! `--distribute` — this venue's instrument rows, run as work on the ring.
//!
//! # What this is not
//!
//! It is not a second runner and it is not a queue client. The selection, the
//! budget, the concurrency, the four verdicts, the table and the durable
//! `summary.json` are all the ones `super` already owns; the only thing that
//! changes is WHERE a lane's process runs. A selected [`Instrument`] becomes
//! one `process:v1` unit — `argv` from the row, `preconditions` from the row,
//! and the verdict source from the row — and the results merge back through
//! the SAME roll-up, with one `node` column added, so a distributed verdict is
//! diffable against a local one at the same rev
//! (`sovereign/deploy/mesh/WORK_PLANE.md` §The pilot).
//!
//! # A unit the cohort cannot place is a ROW, never an absence
//!
//! The single rule this module exists to hold. A split run that silently drops
//! a shard reports a smaller, greener table than a local run, and a
//! share-only bar would score that as a win
//! (`quality/campaigns/cw-lift.toml`, `cw-work-ci-offload`). So every selected
//! instrument gets a row: one that nobody could take is `never-ran` carrying
//! the cohort's typed refusals, and one still queued or leased when the budget
//! expired is `could-not-judge` saying so.
//!
//! # What the submitter can and cannot see
//!
//! [`may_take`] answers the RAIL's half — kind, both sides of the grant, os,
//! arch, the queue, the donor's lease budget — from the journal every node
//! folds, so the submitter can name those refusals exactly as the donor would.
//! It does NOT answer the donor's host-side half: whether a checkout can
//! resolve the pinned `repo_rev`, whether the host meets a precondition,
//! whether an executor is registered. Those are decided donor-side
//! (`sovereign-mesh/src/work_donor.rs`, `resolve_workdir` / `host_satisfies`)
//! and a refusal there is a `continue`, not an act — nothing reaches the rail.
//! So a unit that every offer accepts on the rail and nobody leases is
//! reported as exactly that, and the three host-side checks are NAMED as the
//! ones this node cannot see. Guessing which of them fired would be a
//! substitution (ARCH §18.3).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

// Through `commonwealth-work`'s re-export, not a direct dep on the crate that
// defines it: one constructor is not worth an edge onto `commonwealth-core`
// (ARCH §8.3, and the fan-in cap `layer-gate` holds).
use commonwealth_rail::RailAct;
use commonwealth_work::act::{Submission, WorkAct, MAX_TTL_SECS, MIN_TTL_SECS};
use commonwealth_work::process::{ProcessPayload, ResultSource};
use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
use commonwealth_work::refusal::{may_take, WorkRefusal};
use commonwealth_work::HandoffId;
use commonwealth_work::{seal, ActorKey, UnitRef, WORK_NAMESPACE};
use kernel_types::attribution::ComputeAttribution;
use kernel_types::quality::{Instrument, Overrun, Trigger, VerdictSource};
use kernel_types::{Judgement, Reason, Server, Verdict};
use oicp_types::{JobKind, JobRequirements, JobUnit};
use sovereign_cli_shared::rail::{admission_from_wire, rail_append, rail_log};

use super::exec::{Covariates, InstrumentRun};

/// How often the submitter re-reads the journal.
///
/// Two seconds, not `super::POLL`'s 250 ms: that one polls a child process
/// this process owns, and this one is an HTTP round-trip to the daemon whose
/// answer only changes when a donor appends. The donor's own round is
/// `DONOR_POLL_INTERVAL` (5 s), so a faster poll here buys nothing and costs a
/// request.
const POLL: Duration = Duration::from_secs(2);

/// One `--distribute` run, as the merge left it.
///
/// `results` is keyed and shaped exactly like the local path's, so the caller
/// prints and persists ONE way (ARCH §10.6).
pub(super) struct DistributedRun {
    pub(super) results: BTreeMap<String, InstrumentRun>,
    /// The actor key the daemon signed the submission with, read off the fold
    /// rather than from this node's key file: what matters is who the RING
    /// says submitted it, and that is what a donor's `accept_from` is checked
    /// against.
    pub(super) submitted_by: Option<String>,
    pub(super) handoff: HandoffId,
    pub(super) repo_rev: String,
}

// ── Building the units ──────────────────────────────────────────────

/// The wall cap one unit gets, out of the venue's own overrun policy.
///
/// **The same decider the local path uses**, not a second one: `mod.rs` caps a
/// locally spawned lane at the budget remaining when `overrun = "kill"` and at
/// no cap at all when it is `"report"`, and this is that rule expressed in the
/// one number a payload can carry. `MAX_TTL_SECS` stands in for "no cap"
/// because `ProcessPayload::timeout_secs` is required — a unit that outlives
/// every lease it could hold is a unit nobody can complete.
///
/// The reservation is a FLOOR under it and never a ceiling, which is
/// `Instrument::reservation_secs`'s own rule: under-reserving starves an
/// instrument that would have finished.
pub(super) fn wall_cap_secs(inst: &Instrument, trigger: &Trigger, budget_secs: u64) -> u64 {
    let venue = match trigger.overrun {
        Overrun::Kill => budget_secs,
        Overrun::Report => MAX_TTL_SECS,
    };
    venue
        .max(inst.reservation_secs())
        .clamp(MIN_TTL_SECS, MAX_TTL_SECS)
}

/// Where a donor reads this instrument's verdict from.
///
/// Read OFF THE ROW, never fixed at `VerdictLine`: four of this repo's five
/// `ci:test` instruments say pass/fail with an exit code and would report
/// `no-verdict` under the lane protocol. `ResultSource::verdict_source` is the
/// inverse of this mapping and lives in `commonwealth-work`; the two are the
/// one correspondence between the registry's vocabulary and the executor's.
pub(super) fn result_source(inst: &Instrument) -> ResultSource {
    match inst.verdict {
        VerdictSource::JudgementLine => ResultSource::VerdictLine,
        // `Stdout` rather than `ExitCodeOnly`: the local path keeps the tail of
        // a failed lane's output and prints it under the row, and a
        // distributed row that lost it would be strictly less useful than the
        // local one it must be diffable against.
        VerdictSource::ExitCode => ResultSource::Stdout,
    }
}

/// One sealed `process:v1` unit per selected instrument, in selection order.
///
/// The payload is built as a [`ProcessPayload`] rather than a JSON literal for
/// the reason `svrn job submit` records: a hand-spelled body shipped without
/// `timeout_secs` and `result`, and every donor refused it as
/// `payload-not-canonical`, five seconds at a time, invisibly to the
/// submitter.
pub(super) fn units_for(
    lanes: &[&Instrument],
    trigger: &Trigger,
    budget_secs: u64,
    repo_rev: &str,
) -> Result<Vec<JobUnit>, String> {
    let kind = JobKind::parse(commonwealth_work::process::PROCESS_KIND).map_err(|e| {
        format!(
            "`{}` is not a kind: {e}",
            commonwealth_work::process::PROCESS_KIND
        )
    })?;
    lanes
        .iter()
        .map(|inst| {
            let payload = ProcessPayload {
                argv: inst.argv(),
                cwd: None,
                stdin: None,
                env: BTreeMap::new(),
                timeout_secs: wall_cap_secs(inst, trigger, budget_secs),
                result: result_source(inst),
            };
            let body = serde_json::to_value(&payload)
                .map_err(|e| format!("`{}`'s payload could not be encoded: {e}", inst.id))?;
            seal::seal(kind.clone(), body, requirements_for(inst, repo_rev), None)
                .map_err(|e| format!("`{}` could not be sealed: {e}", inst.id))
        })
        .collect::<Result<Vec<JobUnit>, String>>()
        .and_then(|units| distinct_units(lanes, units))
}

/// Refuse a selection whose instruments do not have distinct units.
///
/// **The plane deduplicates by `unit_hash`, and it is right to.** A unit's
/// identity is `ContentHash` over its kind and payload (`seal::unit_hash`),
/// so two instruments whose argv, cwd, env, wall cap and result source all
/// agree ARE one computation, and the fold leases, runs and completes it once
/// — idempotency per `unit_hash` is the property that makes at-least-once
/// delivery safe. What the runner needs is two ROWS, and it cannot have them
/// from one unit.
///
/// So the conflict is named at the door rather than absorbed. Submitting
/// anyway would put two table rows on one unit's fate, which is the same
/// smaller-truer-looking table `unplaced_row` exists to prevent, arriving by a
/// different road (ARCH §18.3).
///
/// Watched live: `demo-pass` and `demo-unplaceable` in the 5e demo registry
/// were both `/usr/bin/true`, collapsed to one unit, and the whole run
/// reported on a unit whose preconditions belonged to the other row.
fn distinct_units(lanes: &[&Instrument], units: Vec<JobUnit>) -> Result<Vec<JobUnit>, String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (inst, unit) in lanes.iter().zip(&units) {
        if let Some(first) = seen.insert(unit.unit_hash.as_str(), inst.id.as_str()) {
            return Err(format!(
                "`{first}` and `{}` submit the SAME unit — identical argv, wall cap and result                  source at this rev, so the work plane sees one computation and would give the                  two rows one fate. Give one of them a distinguishing command, or run them in                  different venues",
                inst.id
            ));
        }
    }
    Ok(units)
}

/// What a host must satisfy to run this instrument.
///
/// All four halves are pinned, and each for a reason `ComputeAttribution
/// ::comparable_to` names: the rev, the OS and the arch each change what the
/// program under test IS, so a verdict from a host that differs on any of them
/// is not evidence about this checkout. The preconditions come straight off
/// the row — `JobRequirements::preconditions` is `kernel_types::Precondition`,
/// the registry's own vocabulary, so nothing is re-spelled here (cw-lift 5b
/// built that field for exactly this).
pub(super) fn requirements_for(inst: &Instrument, repo_rev: &str) -> JobRequirements {
    JobRequirements {
        repo_rev: Some(repo_rev.to_string()),
        os: Some(std::env::consts::OS.to_string()),
        arch: Some(std::env::consts::ARCH.to_string()),
        preconditions: inst.preconditions.clone(),
    }
}

// ── The attribution this run is judged against ──────────────────────

/// The rev of `repo`'s working checkout — what a LOCAL run of these lanes
/// would be a verdict about.
pub(super) fn head_rev(repo: &Path) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", repo.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`git rev-parse HEAD` failed in {} — a distributed run pins its units to a revision, \
             and a checkout with no HEAD has none to pin",
            repo.display()
        ));
    }
    let rev = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if rev.is_empty() {
        return Err("`git rev-parse HEAD` printed nothing".to_string());
    }
    Ok(rev)
}

/// The submitter-side toolchain absence.
///
/// **This used to be a knowingly SECOND reading of `rustc --version`**, named
/// as a §10.6 deviation with its own convergence target written in the code:
/// "one `ComputeAttribution::of_this_host` ... which both would call". That
/// target is now built, in `commonwealth_work::attribution` rather than in
/// `kernel-types` — the kernel is a pure serde contract whose own manifest
/// says anything heavier belongs in a domain crate, and spawning `rustc` from
/// a leaf every lift carries would have been heavier. `sovereign-cli` already
/// links `commonwealth-work` with `features = ["process"]`, so the shared
/// reader costs no new edge.
///
/// The readings stay INDEPENDENT, which was the reason the duplicate was
/// tolerable: each side calls the shared function on its OWN machine and
/// reads its OWN `rustc`, so the guard is never asserting on a field the
/// donor supplied (ARCH §18.1). What converged is the method, not the value.
pub(super) use commonwealth_work::attribution::ABSENT_TOOLCHAIN;

/// What a LOCAL run at `repo_rev` would have been attributed to.
///
/// The reference every donor's `provenance` is checked against, so that "a
/// verdict that is not yours" is a typed question rather than a footnote.
pub(super) fn local_attribution(repo_rev: &str) -> ComputeAttribution {
    commonwealth_work::attribution::of_this_host(repo_rev)
}

/// Which fields of a donor's attribution do not match this checkout's.
///
/// `comparable_to` is the DECIDER — this only runs once it has already said no,
/// and only to name which halves differ, the same shape
/// `commonwealth_work::refusal`'s `UnmetRequirement` selection uses.
fn incomparable_fields(mine: &ComputeAttribution, theirs: &ComputeAttribution) -> Vec<String> {
    let mut out = Vec::new();
    // A field is incomparable two ways, and only the first used to be
    // reported: the values DIFFER, or they agree on a named absence. The
    // second is not a corner case — it is what every host without `rustc` on
    // `PATH` produces, on both sides at once, and before this the row refused
    // correctly and then named nothing, rendering as "not about — ." with a
    // dangling dash. A refusal that cannot say which field it refused on is
    // the absence-shaped half of ARCH §18.3.
    let mut check = |field: &str, mine: &str, theirs: &str, shorten: bool| {
        let render = |v: &str| {
            if shorten {
                short(v)
            } else {
                v.to_string()
            }
        };
        if mine != theirs {
            out.push(format!(
                "{field} `{}` here against `{}` there",
                render(mine),
                render(theirs)
            ));
        } else if kernel_types::is_absent_marker(mine) {
            out.push(format!(
                "neither host could read its {field} (`{}`), so the two are not \
                 evidence about each other",
                render(mine)
            ));
        }
    };
    check("rev", &mine.repo_rev, &theirs.repo_rev, true);
    check("os", &mine.os, &theirs.os, false);
    check("arch", &mine.arch, &theirs.arch, false);
    check("toolchain", &mine.toolchain, &theirs.toolchain, false);
    out
}

/// A 40-hex rev or a 64-hex key, shortened for a sentence. Never used where
/// the value is going to be typed back in.
fn short(raw: &str) -> String {
    if raw.len() > 12 {
        format!("{}…", &raw[..12])
    } else {
        raw.to_string()
    }
}

// ── The merge ───────────────────────────────────────────────────────

/// Re-key a donor's verdict onto the instrument the table is keyed by.
///
/// The verdict and the reason are the DONOR's, unchanged. Only the subject
/// moves: an exit-code unit's judgement names `process:v1 <hash>`, which is a
/// true statement about the unit and useless as a row id.
fn rekey(subject: &str, verdict: Verdict, reason: &Reason) -> Judgement {
    let reason = Reason::new(reason.as_str().to_string())
        .unwrap_or_else(|| Reason::literal("the donor's reason was a placeholder"));
    match verdict {
        Verdict::Passed => Judgement::passed(subject, reason),
        Verdict::Failed => Judgement::failed(subject, reason),
        Verdict::CouldNotJudge => Judgement::could_not_judge(subject, reason),
        Verdict::NeverRan => Judgement::never_ran(subject, reason),
    }
}

/// A `Reason` from a sentence that is never a placeholder by construction.
fn reason(text: String) -> Reason {
    Reason::new(text).unwrap_or_else(|| Reason::literal("no reason was stated"))
}

/// One instrument's row out of a TERMINAL unit.
///
/// Three things happen here and they are ordered on purpose:
///
/// 1. **Provenance first.** A `Complete` whose `ComputeAttribution` is not
///    [`comparable_to`](ComputeAttribution::comparable_to) this checkout's is
///    `could-not-judge` NAMING the difference, never the verdict it carries.
///    A donor one commit behind produces a perfectly well-formed pass about a
///    tree that is not yours, and adopting it is this system's characteristic
///    failure (WORK_PLANE.md's bar iii).
/// 2. **Then the subject**, for a lane that SAYS its verdict: the same rule
///    `exec::finish` applies locally — a verdict line naming a different
///    subject is an unrelated row in the operator's report.
/// 3. **Then the verdict**, re-keyed and otherwise untouched.
pub(super) fn terminal_row(
    inst: &Instrument,
    status: &WorkUnitStatus,
    mine: &ComputeAttribution,
) -> InstrumentRun {
    match status {
        WorkUnitStatus::Complete {
            lessee,
            outcome,
            result,
            provenance,
            attempts,
            ..
        } => {
            let node = Some(lessee.as_str().to_string());
            let secs = result
                .get("duration_ms")
                .and_then(serde_json::Value::as_u64)
                .map(|ms| ms / 1000)
                .unwrap_or(0);
            let exit_code = result
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .map(|c| c as i32);
            let tail = super::exec::tail_of(&result_text(result), 12);

            if !mine.comparable_to(provenance) {
                let why = incomparable_fields(mine, provenance).join("; ");
                tracing::debug!(
                    id = %inst.id, node = %lessee, %why,
                    "quality check: distributed row is not comparable to this checkout"
                );
                return InstrumentRun {
                    judgement: Judgement::could_not_judge(
                        inst.id.clone(),
                        reason(format!(
                            "`{}` answered `{}` on a machine this result is not about — {why}. \
                             A verdict computed elsewhere is not evidence about this tree \
                             (ComputeAttribution::comparable_to)",
                            short(lessee.as_str()),
                            outcome.verdict().as_str(),
                        )),
                    ),
                    secs,
                    exit_code,
                    tail,
                    before: Covariates::default(),
                    after: Covariates::default(),
                    node,
                };
            }

            let judgement =
                if inst.verdict == VerdictSource::JudgementLine && outcome.subject() != inst.id {
                    Judgement::could_not_judge(
                        inst.id.clone(),
                        reason(format!(
                            "the verdict line names subject `{}`, not `{}`",
                            outcome.subject(),
                            inst.id
                        )),
                    )
                } else {
                    rekey(&inst.id, outcome.verdict(), outcome.reason())
                };
            tracing::debug!(
                id = %inst.id, node = %lessee, attempts,
                verdict = judgement.verdict().as_str(),
                "quality check: distributed row merged"
            );
            InstrumentRun {
                judgement,
                secs,
                exit_code,
                tail,
                before: Covariates::default(),
                after: Covariates::default(),
                node,
            }
        }
        // Terminal without a verdict of its own. `outcome` is `Some` when the
        // last lessee reported a `Fail` act and `None` when the lease simply
        // lapsed its attempts — two different facts, kept apart, because one
        // of them says nobody ever finished.
        WorkUnitStatus::Failed {
            last_lessee,
            reason: why,
            attempts,
            outcome,
        } => {
            let judgement = match outcome {
                Some(j) => rekey(&inst.id, j.verdict(), j.reason()),
                None => Judgement::never_ran(
                    inst.id.clone(),
                    reason(format!("after {attempts} attempt(s): {why}")),
                ),
            };
            InstrumentRun {
                judgement,
                secs: 0,
                exit_code: None,
                tail: String::new(),
                before: Covariates::default(),
                after: Covariates::default(),
                node: Some(last_lessee.as_str().to_string()),
            }
        }
        // Not terminal. Reachable only through a caller that stopped checking
        // `is_terminal`, and answered rather than panicked: an abstention that
        // names its own cause is a result.
        other => InstrumentRun::did_not_start(
            Judgement::could_not_judge(
                inst.id.clone(),
                reason(format!(
                    "this unit is `{}` and not terminal — no verdict to merge",
                    other.id()
                )),
            ),
            0,
            Covariates::default(),
        ),
    }
}

/// The stdout and stderr a `process:v1` result carried, for the tail under a
/// red row. Absent on `ExitCodeOnly` units, which is why this is a join over
/// what is there rather than an index into what should be.
fn result_text(result: &serde_json::Value) -> String {
    ["stdout", "stderr"]
        .iter()
        .filter_map(|k| result.get(*k).and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What every donor that has published an offer says about one unit.
///
/// `Ok` means the RAIL's half is satisfied and only the donor's own host-side
/// half could have stopped it; see this module's docs for why that difference
/// matters and why it is not guessed at.
pub(super) fn survey(
    proj: &WorkProjection,
    unit: &UnitRef,
    now_ms: u64,
) -> Vec<(ActorKey, Result<(), WorkRefusal>)> {
    proj.offers
        .iter()
        .map(|(actor, offer)| (actor.clone(), may_take(proj, actor, offer, unit, now_ms)))
        .collect()
}

/// One instrument's row for a unit that is still QUEUED — nobody has it.
///
/// **The row exists.** That is the whole point: a shard that vanishes makes
/// the merged table smaller and greener than the local run it must equal.
pub(super) fn unplaced_row(
    inst: &Instrument,
    verdicts: &[(ActorKey, Result<(), WorkRefusal>)],
) -> InstrumentRun {
    if verdicts.is_empty() {
        return InstrumentRun::did_not_start(
            Judgement::never_ran(
                inst.id.clone(),
                Reason::literal(
                    "no node has published an offer on the `work` ring, so there was nobody to \
                     take this unit — `[compute.work_offer]` in a peer's config is what publishes \
                     one",
                ),
            ),
            0,
            Covariates::default(),
        );
    }
    let refusals: Vec<String> = verdicts
        .iter()
        .filter_map(|(actor, v)| {
            v.as_ref()
                .err()
                .map(|r| format!("{}: {} — {r}", short(actor.as_str()), r.id()))
        })
        .collect();
    let willing = verdicts.len() - refusals.len();
    if willing == 0 {
        // Every offer refused, and each refusal is the rail's own typed
        // sentence — `requirement-unmet`, `kind-not-offered`, `not-allowed`.
        // Rendered as they are, never reworded here (ARCH §10.6).
        return InstrumentRun::did_not_start(
            Judgement::never_ran(
                inst.id.clone(),
                reason(format!(
                    "no donor on the ring could take this unit — {}",
                    refusals.join("; ")
                )),
            ),
            0,
            Covariates::default(),
        );
    }
    // Somebody could have, on the rail's half, and did not. The three checks
    // that could have stopped it are host-side and this node cannot see them,
    // so they are NAMED rather than picked between.
    InstrumentRun::did_not_start(
        Judgement::could_not_judge(
            inst.id.clone(),
            reason(format!(
                "{willing} donor(s) could take this unit on the rail's half and none did before \
                 the budget expired — the host-side half is not visible from here: a checkout \
                 that cannot resolve the pinned rev, an unmet precondition, or no executor \
                 registered for the kind{}",
                if refusals.is_empty() {
                    String::new()
                } else {
                    format!(". The rest refused: {}", refusals.join("; "))
                }
            )),
        ),
        0,
        Covariates::default(),
    )
}

/// One instrument's row for a unit a donor is still HOLDING at the deadline.
pub(super) fn in_flight_row(inst: &Instrument, lessee: &ActorKey, attempts: u32) -> InstrumentRun {
    InstrumentRun {
        judgement: Judgement::could_not_judge(
            inst.id.clone(),
            reason(format!(
                "`{}` still held this unit on attempt {attempts} when the budget expired — it ran \
                 and did not finish, which is not a statement about the code under test",
                short(lessee.as_str())
            )),
        ),
        secs: 0,
        exit_code: None,
        tail: String::new(),
        before: Covariates::default(),
        after: Covariates::default(),
        node: Some(lessee.as_str().to_string()),
    }
}

/// The clock a refusal survey is taken on: now, or the last millisecond this
/// handoff was still being offered, whichever is earlier.
///
/// See the call site for why. `saturating_sub(1)` rather than the expiry
/// itself because `admits` and `is_live_at` are strict about the boundary, and
/// a survey taken exactly at expiry is a survey after it.
pub(super) fn survey_ms(proj: &WorkProjection, handoff: &HandoffId, now_ms: u64) -> u64 {
    match proj.handoffs.get(handoff) {
        Some(h) => now_ms.min(h.expires_at_ms.saturating_sub(1)),
        None => now_ms,
    }
}

/// Every selected instrument's row, out of one fold.
///
/// TOTAL over `lanes` by construction — the loop is over the instruments, not
/// over the units the fold happens to hold, so a unit the projection lost
/// still produces a row.
pub(super) fn merge(
    lanes: &[&Instrument],
    hashes: &[String],
    proj: &WorkProjection,
    handoff: &HandoffId,
    now_ms: u64,
    mine: &ComputeAttribution,
) -> BTreeMap<String, InstrumentRun> {
    let mut out = BTreeMap::new();
    for (inst, hash) in lanes.iter().zip(hashes) {
        let projected = proj.handoffs.get(handoff).and_then(|h| h.units.get(hash));
        let run = match projected {
            None => InstrumentRun::did_not_start(
                Judgement::never_ran(
                    inst.id.clone(),
                    reason(format!(
                        "the fold holds no unit `{}` under this handoff — the submission was \
                         signed by a key the `work` roster does not carry, or the act has not \
                         reached this node",
                        short(hash)
                    )),
                ),
                0,
                Covariates::default(),
            ),
            Some(p) => {
                let status = p.status_at(now_ms);
                match &status {
                    WorkUnitStatus::Complete { .. } | WorkUnitStatus::Failed { .. } => {
                        terminal_row(inst, &status, mine)
                    }
                    WorkUnitStatus::Leased {
                        lessee, attempts, ..
                    } => in_flight_row(inst, lessee, *attempts),
                    WorkUnitStatus::Queued { .. } => {
                        let unit = UnitRef {
                            handoff: *handoff,
                            unit_hash: hash.clone(),
                        };
                        // SURVEYED AT THE LAST INSTANT THE WORK WAS ON OFFER,
                        // not at the merge's own clock. The handoff's TTL is
                        // the venue's budget, so by the time this runs it has
                        // just lapsed and `WorkHandoff::admits` refuses every
                        // actor — every unplaced row would then read
                        // `not-allowed`, which is true about the closed window
                        // and says nothing about why nobody took the unit
                        // while it was open. Watched: the first live 5e run
                        // reported exactly that for both unplaced rows.
                        unplaced_row(inst, &survey(proj, &unit, survey_ms(proj, handoff, now_ms)))
                    }
                }
            }
        };
        out.insert(inst.id.clone(), run);
    }
    out
}

// ── The run ─────────────────────────────────────────────────────────

/// Submit this venue's selection to the `work` ring and merge what comes back.
///
/// `Err` is a refusal to run at all — no rev, no daemon, a roster that will not
/// take the act. It is never a green and never an empty table.
pub(super) async fn run_distributed(
    repo: &Path,
    lanes: &[&Instrument],
    trigger: &Trigger,
    budget_secs: u64,
) -> Result<DistributedRun, String> {
    let repo_rev = head_rev(repo)?;
    let units = units_for(lanes, trigger, budget_secs, &repo_rev)?;
    let hashes: Vec<String> = units.iter().map(|u| u.unit_hash.clone()).collect();
    let kind = units
        .first()
        .map(|u| u.kind.clone())
        .ok_or("nothing selected — a submission with no units offers nothing to take")?;

    let handoff = HandoffId::generate();
    // TTL is how long the ring keeps OFFERING the work, which is the venue's
    // budget: past it this run has stopped reading, and work nobody is waiting
    // for should not go on being taken.
    let submission = Submission::new(handoff, kind, units, None, Some(budget_secs));
    let payload = commonwealth_work::to_payload(&WorkAct::Submit(submission))
        .map_err(|why| format!("the submission could not be sealed onto the rail: {why}"))?;
    let answer = rail_append(WORK_NAMESPACE, &RailAct::Record { payload }).await?;
    tracing::debug!(
        handoff = %handoff.to_hex(),
        units = hashes.len(),
        rev = %short(&repo_rev),
        seq = ?answer.get("seq"),
        "quality check: submitted this venue to the work ring"
    );
    println!(
        "submitted {} unit(s) to the `{WORK_NAMESPACE}` ring at rev {} — handoff {}",
        hashes.len(),
        short(&repo_rev),
        handoff.to_hex()
    );
    println!(
        "  which node runs each is not this process's to choose: every node folds the same journal."
    );
    println!();

    let mine = local_attribution(&repo_rev);
    let started = Instant::now();
    let budget = Duration::from_secs(budget_secs);
    let mut submitted_by: Option<String> = None;
    let mut announced: BTreeMap<String, ()> = BTreeMap::new();

    let proj = loop {
        let wire = rail_log(WORK_NAMESPACE).await?;
        let admission = admission_from_wire(&wire)?;
        let proj = WorkProjection::fold(&admission);
        let now_ms = sovereign_core::time::unix_millis();

        if let Some(h) = proj.handoffs.get(&handoff) {
            submitted_by = Some(h.submitter.as_str().to_string());
        }
        // Announce each unit as it settles, so a run that takes minutes is
        // legible while it happens rather than only in the table (ARCH §9.1).
        for (inst, hash) in lanes.iter().zip(&hashes) {
            if announced.contains_key(&inst.id) {
                continue;
            }
            let Some(p) = proj.handoffs.get(&handoff).and_then(|h| h.units.get(hash)) else {
                continue;
            };
            let status = p.status_at(now_ms);
            if status.is_terminal() {
                announced.insert(inst.id.clone(), ());
                let run = terminal_row(inst, &status, &mine);
                super::report::report_one(inst, &run);
            }
        }

        let terminal = lanes
            .iter()
            .zip(&hashes)
            .filter(|(_, hash)| {
                proj.handoffs
                    .get(&handoff)
                    .and_then(|h| h.units.get(*hash))
                    .is_some_and(|p| p.status_at(now_ms).is_terminal())
            })
            .count();
        tracing::debug!(
            terminal,
            of = hashes.len(),
            offers = proj.offers.len(),
            elapsed = started.elapsed().as_secs(),
            "quality check: distributed poll"
        );
        if terminal == hashes.len() || started.elapsed() >= budget {
            // The fold this loop broke on is the one the merge reads. Carried
            // out of the loop rather than re-fetched: a second `rail_log`
            // here would merge a DIFFERENT journal from the one that decided
            // the run was over, and the two would disagree about exactly the
            // unit that settled in between.
            break proj;
        }
        tokio::time::sleep(POLL).await;
    };

    let now_ms = sovereign_core::time::unix_millis();
    Ok(DistributedRun {
        results: merge(lanes, &hashes, &proj, &handoff, now_ms, &mine),
        submitted_by,
        handoff,
        repo_rev,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::quality::Registry;

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
        use commonwealth_work::refusal::UnmetRequirement;
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
    /// The projection is built by hand and holds one QUEUED unit against one
    /// offer that cannot run it — a donor on another OS, which is the
    /// `RequirementUnmet` `may_take` can actually reach from a submitter
    /// (`repo_rev` is decided donor-side and never reaches the rail; see the
    /// module docs).
    ///
    /// Failing input: delete the `Queued` arm from [`merge`]. The instrument
    /// vanishes, the table gets shorter and greener than a local run at the
    /// same rev, and a share-only bar scores that as a win — the exact defect
    /// `cw-work-ci-offload`'s block was written about.
    #[test]
    fn a_queued_shard_the_cohort_refused_is_still_a_row_in_the_merged_table() {
        use commonwealth_work::projection::{ProjectedUnit, WorkHandoff};
        use oicp_types::{Isolation, WorkOffer};

        let r = reg();
        let inst = lane(&r, "docs-gate");
        let lanes: Vec<&Instrument> = vec![&inst];
        let trigger = r
            .trigger(&kernel_types::quality::RunsIn::Prepush)
            .expect("prepush");
        let rev = "aa".repeat(20);
        let units = units_for(&lanes, trigger, 60, &rev).expect("sealed");
        let hashes: Vec<String> = units.iter().map(|u| u.unit_hash.clone()).collect();
        let handoff = HandoffId::from_u128(9);

        let mut by_hash = BTreeMap::new();
        by_hash.insert(
            hashes[0].clone(),
            ProjectedUnit {
                unit: units[0].clone(),
                status: WorkUnitStatus::Queued { prior_attempts: 0 },
            },
        );
        let mut handoffs = BTreeMap::new();
        handoffs.insert(
            handoff,
            WorkHandoff {
                submitter: actor(0x33),
                kind: units[0].kind.clone(),
                allowed: None,
                submitted_at_ms: 1,
                expires_at_ms: 1_000_000,
                revoked: None,
                units: by_hash,
            },
        );
        let mut offers = BTreeMap::new();
        offers.insert(
            actor(0x44),
            WorkOffer {
                kinds: vec![units[0].kind.clone()],
                max_concurrent: 1,
                yield_to_foreground: false,
                isolation: Isolation::Subprocess,
                // Not this host. `accepts_host` refuses and `may_take` names
                // WHICH half — the typed refusal this row must carry.
                os: "plan9".to_string(),
                arch: std::env::consts::ARCH.to_string(),
                repos: Vec::new(),
                accept_from: None,
            },
        );
        let proj = WorkProjection {
            handoffs,
            offers,
            ..WorkProjection::default()
        };

        let rows = merge(&lanes, &hashes, &proj, &handoff, 100, &attribution(&rev));
        let row = rows
            .get(&inst.id)
            .expect("a refused shard is a row, never an absence");
        assert_eq!(row.judgement.verdict(), Verdict::NeverRan);
        let why = row.judgement.reason().as_str();
        assert!(why.contains("requirement-unmet"), "{why}");
        assert!(why.contains("plan9"), "{why}");
        assert!(row.node.is_none(), "nobody ran it");
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
    /// Delete `distinct_units` and this goes green while the run it describes
    /// is wrong in a way no verdict shows.
    #[test]
    fn two_instruments_that_submit_one_unit_are_refused_by_name() {
        let r = Registry::parse(COLLIDING).expect("parses");
        let a = r.instruments[0].clone();
        let b = r.instruments[1].clone();
        let lanes: Vec<&Instrument> = vec![&a, &b];
        let trigger = r
            .trigger(&kernel_types::quality::RunsIn::Precommit)
            .expect("precommit");
        let err = units_for(&lanes, trigger, 60, "deadbeef").expect_err("refused");
        assert!(err.contains("twin-a") && err.contains("twin-b"), "{err}");
        assert!(err.contains("SAME unit"), "{err}");

        // And two rows that genuinely differ are accepted, so the refusal is
        // about collision rather than about there being more than one row.
        let r2 = reg();
        let c = lane(&r2, "docs-gate");
        let d = lane(&r2, "chat-ask");
        let both: Vec<&Instrument> = vec![&c, &d];
        assert_eq!(
            units_for(&both, trigger, 60, "deadbeef")
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
        use commonwealth_work::projection::WorkHandoff;
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
}
