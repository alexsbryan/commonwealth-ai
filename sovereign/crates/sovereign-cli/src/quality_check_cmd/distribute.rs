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
//! (`quality/campaigns/closed/cw-lift.toml`, `cw-work-ci-offload`). So every selected
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
//! whether an executor is registered. Those are decided ON THE DONOR — the
//! precondition half by `commonwealth_work::refusal::host_satisfies` (which
//! moved out of `work_donor` at cw-lift 5f's last hole, so both donors ask one
//! decider), the checkout and registry halves still by
//! `sovereign-daemon/src/work_donor.rs`'s `resolve_workdir` and `resolve_offer`
//! — and a refusal there is a `continue`, not an act: nothing reaches the rail.
//! So a unit that every offer accepts on the rail and nobody leases is
//! reported as exactly that, and the three host-side checks are NAMED as the
//! ones this node cannot see. Guessing which of them fired would be a
//! substitution (ARCH §18.3).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

// Through `sovereign-cli-shared::rail`, the ONE operator-side rail client,
// dialing cw-rails' own `work` doors (pb-work-doors). This crate links no
// commonwealth-work: the seal, the signed Submit, `may_take` and the
// attribution method are each cw-rails' to run, over the one implementation.
use kernel_types::attribution::ComputeAttribution;
use kernel_types::quality::{Instrument, Overrun, Trigger, VerdictSource};
use kernel_types::{ActorKey, HandoffId, Judgement, Reason, Server, Verdict};
use oicp_types::work::{
    ProcessPayload, ResultSource, WorkProjection, WorkRefusal, WorkUnitStatus, MAX_TTL_SECS,
    MIN_TTL_SECS, PROCESS_KIND,
};
use oicp_types::{JobKind, JobRequirements, JobUnit};
use sovereign_cli_shared::rail::{
    rails_get, rails_post, WORK_ATTRIBUTION_PATH, WORK_PROJECTION_PATH, WORK_REFUSALS_PATH,
    WORK_SEAL_PATH, WORK_SUBMIT_PATH,
};

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
///
/// Sealed by cw-rails' seal door (pb-work-doors): a unit's identity
/// canonicalises through rail-core's `Payload::new`, which this crate does
/// not link, and a second canonicaliser here would be a second identity.
pub(super) async fn units_for(
    rails_base: &str,
    lanes: &[&Instrument],
    trigger: &Trigger,
    budget_secs: u64,
    repo_rev: &str,
) -> Result<Vec<JobUnit>, String> {
    let kind =
        JobKind::parse(PROCESS_KIND).map_err(|e| format!("`{PROCESS_KIND}` is not a kind: {e}"))?;
    let items = lanes
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
            Ok(serde_json::json!({
                "payload": body,
                "requirements": requirements_for(inst, repo_rev),
            }))
        })
        .collect::<Result<Vec<serde_json::Value>, String>>()?;
    let answer = rails_post(
        rails_base,
        WORK_SEAL_PATH,
        &serde_json::json!({ "kind": kind, "units": items }),
    )
    .await
    .map_err(|e| format!("cw-rails would not seal this selection: {e}"))?;
    let units: Vec<JobUnit> = answer
        .get("units")
        .cloned()
        .ok_or_else(|| format!("cw-rails' `{WORK_SEAL_PATH}` answer carried no `units`"))
        .and_then(|v| {
            serde_json::from_value(v)
                .map_err(|e| format!("cw-rails sealed units this build cannot read: {e}"))
        })?;
    if units.len() != lanes.len() {
        return Err(format!(
            "cw-rails sealed {} unit(s) for {} selected instrument(s) — every row needs its unit",
            units.len(),
            lanes.len()
        ));
    }
    tracing::debug!(
        units = units.len(),
        "quality check: cw-rails sealed the selection"
    );
    distinct_units(lanes, units)
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
        // ABSENT ON PURPOSE, and it is the one field here that is a judgement
        // rather than a fact. `isolation` is what the SUBMITTER demands of a
        // donor; the donor's own floor (`JobExecutorRegistry::offerable`) is
        // what protects the donor from us. Naming `RootlessContainer` here
        // would state the same threshold `ProcessExecutor`'s descriptor
        // already states, in a second place, where the two could drift — and
        // the protection it looks like it is buying is not ours to claim
        // (ARCH §10.6). This run has no isolation requirement of its own: it
        // asks for a rev, an os, an arch and the row's preconditions, and
        // whether a donor may run a stranger's argv at all is that donor's
        // decision, made before it ever leases.
        isolation: None,
        preconditions: inst.preconditions.clone(),
    }
}

// ── The attribution this run is judged against, and the run ─────────
//
// Sibling files only so this one stays under ARCH §3.1's approach band
// (pb-work-doors) — moved verbatim, and reached at their old paths.
#[path = "distribute/attribution.rs"]
mod attribution;
#[path = "distribute/run.rs"]
mod run;

use attribution::incomparable_fields;
pub(super) use attribution::{head_rev, local_attribution};
pub(super) use run::run_distributed;

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

/// What every donor that has published an offer says about each queued unit,
/// keyed by unit hash — cw-rails' refusals door, which runs `may_take`.
///
/// `Ok` means the RAIL's half is satisfied and only the donor's own host-side
/// half could have stopped it; see this module's docs for why that difference
/// matters and why it is not guessed at.
pub(super) type Surveys = BTreeMap<String, Vec<(ActorKey, Result<(), WorkRefusal>)>>;

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
    surveys: &Surveys,
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
                    WorkUnitStatus::Queued { .. } => match surveys.get(hash) {
                        Some(verdicts) => unplaced_row(inst, verdicts),
                        // No survey is not "nobody offered": it is a question
                        // cw-rails did not answer, and the row says so.
                        None => InstrumentRun::did_not_start(
                            Judgement::could_not_judge(
                                inst.id.clone(),
                                reason(format!(
                                    "unit `{}` was still queued and cw-rails' refusal survey \
                                     did not answer for it, so why nobody took it is not \
                                     known here",
                                    short(hash)
                                )),
                            ),
                            0,
                            Covariates::default(),
                        ),
                    },
                }
            }
        };
        out.insert(inst.id.clone(), run);
    }
    out
}

#[cfg(test)]
#[path = "distribute/tests.rs"]
mod tests;
