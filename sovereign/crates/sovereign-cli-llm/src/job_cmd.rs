// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn job` — hand work to the ring, and read one fold of what happened.
//!
//! # What this verb is for
//!
//! `ring` deploys an app to a trust ring. `job` hands that same ring a unit of
//! *compute*: a command, at a pinned revision, that any node in the ring which
//! has offered that kind may take, run, and report on. There is no queue
//! server, no lease table and no HTTP route of its own — a submission is a
//! signed act on the `work` namespace, and the queue is a fold over the total
//! order every node already agrees on (`commonwealth_work`).
//!
//! ```text
//! svrn ring roster add alex --self --ring work   # my key may write here
//! svrn job submit --kind process:v1 -- uname -a  # work exists
//! svrn job status                                # what the ring did with it
//! ```
//!
//! # Two verbs, and why neither of them is a queue client
//!
//! [`run_submit`] appends ONE act through [`ring_cmd::rail_append`] — the one
//! append client in this crate (ARCH §10.6) — and stops. It does not wait, poll
//! or place: which node takes a unit is decided by every node reading the same
//! journal, and a submitter that also chose a donor would be a second decider
//! for a lease.
//!
//! [`run_status`] reads `GET /v1/rail/log` and folds it with the SAME
//! `WorkProjection::fold` the daemon's donor loop uses. That is the point of
//! the fold living in a package crate: the terminal, the daemon and a lifted
//! third-party peer are three readers of one function, so they cannot disagree
//! about who holds a lease.
//!
//! # Why `status` goes over HTTP and never opens the journal
//!
//! The same reason [`ring_cmd::rail_log`] does, written at `ring_cmd`'s
//! module docs: **the roster the DAEMON loaded is the one that decides what is
//! readable.** An act signed by a key the roster does not carry is an
//! `UnknownSigner` gap, not an act — and the on-disk journal has no roster
//! beside it. Folding the file directly would produce a projection that is
//! confidently wrong on exactly the ring where membership is the question, and
//! it would be wrong SILENTLY, because a refused act and an absent act look
//! identical once the roster is gone.
//!
//! So gaps are printed beside the acts, always, and the sentence comes off the
//! wire rather than being composed here — `ring log`'s two rules, applied to
//! the same wire shape (`ring_cmd/mod.rs`, `run_log`).

use std::collections::BTreeMap;

use commonwealth_core::HandoffId;
use commonwealth_rail::{Admission, AdmittedOp, Payload, RailAct, RailGap};
use commonwealth_work::act::{Submission, WorkAct};
use commonwealth_work::process::ProcessPayload;
use commonwealth_work::projection::{WorkProjection, WorkUnitStatus};
use commonwealth_work::{seal, ActorKey, WORK_NAMESPACE};
use oicp_types::{JobKind, JobRequirements, JobUnit};

use crate::ring_cmd::{rail_append, rail_log, short_stamp};

pub(crate) async fn run(args: &[String]) -> i32 {
    // `--help` is answered BEFORE anything is dispatched, and only when it is
    // asked of this verb rather than of the unit's own command — everything
    // after a bare `--` is the payload's argv, and a `job submit … -- svrn
    // chat --help` that printed this page instead of submitting would be the
    // verb eating its own cargo.
    //
    // It is answered before the daemon is reached, too. `job status --help`
    // used to fall through to the fold, so on a machine with a daemon it
    // printed a journal and on a machine without one it printed a connection
    // error — a help probe whose answer depends on whether a service is up
    // (ARCH §18.1: the assertion must not be about the lane).
    let (head, _) = split_at_double_dash(args);
    if head.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    match args.first().map(String::as_str) {
        Some("submit") => run_submit(&args[1..]).await,
        Some("status") => run_status(&args[1..]).await,
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

/// The whole surface, as data (ARCH §6). One text, so the `--help` page and
/// the refusal a bare `svrn job` prints cannot drift.
const USAGE: &str = concat!(
    "usage:\n",
    "  svrn job submit --kind <id:vN> (--units <file.json> | -- <argv…>)",
    " [--allow <actor-key>]… [--ttl <secs>] [--timeout <secs>]\n",
    "  svrn job status [<handoff>] [--json]\n",
    "\n",
    "submit  put work on the `work` ring as one signed act. Which node runs it\n",
    "        is not yours to choose: every node folds the same journal.\n",
    "status  the fold — handoffs, their units, who holds what, and every line\n",
    "        this node could not account for.\n",
    "\n",
    "--ttl is how long the ring keeps OFFERING the work; --timeout is how long\n",
    "one run of it may take, and applies to the `-- <argv…>` form only (a units\n",
    "file states `timeout_secs` per unit). Submit prints the cap it used.\n",
    "\n",
    "A kind is spelled `id:vN` (`process:v1`). `ingest@1` is refused: one\n",
    "spelling, so two nodes cannot disagree about whether they offer the same\n",
    "thing.\n",
    "\n",
    "Your key must be in the `work` ring's roster first, or the act you sign is\n",
    "a gap rather than a submission:\n",
    "  svrn ring roster add <you> --self --ring work",
);

// ── submit ───────────────────────────────────────────────────

/// `svrn job submit` — one `Submit` act, through the one append client.
async fn run_submit(args: &[String]) -> i32 {
    let (head, argv) = split_at_double_dash(args);

    let Some(raw_kind) = flag(head, "--kind") else {
        eprintln!("job submit: which kind of work? pass --kind <id:vN>, e.g. --kind process:v1");
        return 2;
    };
    let kind = match JobKind::parse(raw_kind) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("job submit: {e}");
            return 2;
        }
    };

    // The unit's own wall cap, which is NOT the submission's TTL: the TTL is
    // how long the ring may keep offering this work, and this is how long one
    // run of it may take. Only the argv form takes it — a units file states
    // the whole payload, `timeout_secs` included, and a flag that silently
    // overrode what the file said would be a second answer to the same
    // question.
    let timeout_secs = match flag(head, "--timeout").map(str::parse::<u64>) {
        Some(Ok(t)) => Some(t),
        Some(Err(_)) => {
            eprintln!("job submit: --timeout takes whole seconds");
            return 2;
        }
        None => None,
    };
    if timeout_secs.is_some() && flag(head, "--units").is_some() {
        eprintln!(
            "job submit: --timeout applies to the trailing `-- <argv>` form; a units file \
             states `timeout_secs` per unit, and a flag that overrode it would be a second \
             place the wall cap is decided"
        );
        return 2;
    }

    let units = match resolve_units(&kind, flag(head, "--units"), argv, timeout_secs) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("job submit: {e}");
            return 2;
        }
    };

    // Tri-state, and all three states are different (`Submission.allowed`):
    // no `--allow` is open to the ring, `--allow k…` is those actors, and
    // `--allow` with an empty list is unreachable from here on purpose — a
    // self-only submission is written by passing your own key.
    let allowed = match collect_allowed(head) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("job submit: {e}");
            return 2;
        }
    };

    let ttl_secs = match flag(head, "--ttl").map(str::parse::<u64>) {
        Some(Ok(t)) => Some(t),
        Some(Err(_)) => {
            eprintln!("job submit: --ttl takes whole seconds");
            return 2;
        }
        None => None,
    };

    let handoff = HandoffId::generate();
    // `Submission::new` takes the units by value and the report below reads
    // the cap off them, so the clone is the price of reporting a FACT about
    // what was signed rather than re-deriving it from the flag.
    let submitted_units = units.clone();
    let submission = Submission::new(handoff, kind.clone(), units, allowed.clone(), ttl_secs);
    let unit_count = submission.units.len();
    let clamped_ttl = submission.ttl_secs;

    // `to_payload` is where the seal of every unit is verified and where a
    // unit disagreeing with the handoff's kind is refused — the one door onto
    // the rail. Its refusals are sentences; they are printed as they are.
    let payload = match commonwealth_work::to_payload(&WorkAct::Submit(submission)) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("job submit: {why}");
            return 2;
        }
    };

    let v = match rail_append(WORK_NAMESPACE, &RailAct::Record { payload }).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("job submit: {e}");
            return 1;
        }
    };
    // `seq 0` would be a position on the journal, and the append route states
    // one — so printing 0 for an answer that carried none reports a place this
    // act was not put (ARCH §18.3). The submission still happened; what is
    // unknown is where.
    let seq = match v.get("seq").and_then(|s| s.as_u64()) {
        Some(n) => format!("at seq {n}"),
        None => "at a seq the daemon did not state".to_string(),
    };

    println!("submitted {unit_count} unit(s) of `{kind}` to the `{WORK_NAMESPACE}` ring {seq}.");
    println!("  handoff : {}", handoff.to_hex());
    match &allowed {
        None => println!("  allowed : anyone in the `{WORK_NAMESPACE}` roster who offers `{kind}`"),
        Some(keys) => {
            println!("  allowed : {} actor(s) —", keys.len());
            for k in keys {
                println!("            {k}");
            }
        }
    }
    println!("  ttl     : {clamped_ttl}s");
    // Printed for the argv form only, and printed rather than documented: a
    // default nobody sees is a default nobody can correct. The units-file form
    // states its own cap per unit and this line would have to pick one.
    //
    // Read OFF THE UNIT, not recomputed from the flag. `timeout_secs
    // .unwrap_or(DEFAULT_TIMEOUT_SECS)` would be a second place the rule
    // "absent means the default" is written, and the two would disagree the
    // day the constructor picks the cap differently (ARCH §10.6). What the
    // submitter needs to see is the cap that is ON THE ACT.
    if argv.is_some() {
        match wall_cap_of(&submitted_units) {
            Some(secs) => println!("  wall cap: {secs}s per unit"),
            // Unreachable for `process:v1` — `ProcessPayload` makes the field
            // required — and said rather than defaulted to
            // `DEFAULT_TIMEOUT_SECS`, which would print a cap the act does not
            // carry (ARCH §18.3).
            None => println!("  wall cap: not stated by this unit's payload"),
        }
    }
    println!();
    // The submitter is now irrelevant to the work: this process can exit, the
    // laptop can close, and the act stands on every node that holds the ring.
    println!("  Nothing is placed yet — a donor takes it by appending its own Lease.");
    println!("  svrn job status {}", handoff.to_hex());
    0
}

/// `--units <file>` or a trailing `-- <argv…>`, never both and never neither.
///
/// The argv form exists because the shortest true statement of this plane is
/// `--kind process:v1 -- uname -a`, and making a person write a JSON file to
/// say it would put a fixture between them and the first thing they try. It is
/// scoped to kinds whose payload shape this CLI can honestly claim to know:
/// `process:v1`, whose executor reads `argv`. For anything else the payload is
/// the executor's business and the file is how you say it.
fn resolve_units(
    kind: &JobKind,
    units_file: Option<&str>,
    argv: Option<&[String]>,
    timeout_secs: Option<u64>,
) -> Result<Vec<JobUnit>, String> {
    match (units_file, argv) {
        (Some(_), Some(_)) => Err("--units and a trailing `-- <argv>` say the same thing two \
                                   ways; pass one of them"
            .to_string()),
        (None, None) => Err(format!(
            "no units. Either `--units <file.json>` (a JSON array of \
             {{\"payload\": {{…}}, \"requirements\": {{…}}?}}), or a trailing \
             `-- <argv…>` when --kind is `{PROCESS_V1}`"
        )),
        (Some(path), None) => units_from_file(kind, path),
        (None, Some(argv)) => {
            if kind.to_string() != PROCESS_V1 {
                return Err(format!(
                    "a trailing `-- <argv>` is the payload shape of `{PROCESS_V1}` and this \
                     submission is `{kind}` — this CLI does not know what `{kind}` units look \
                     like, and guessing would put a second speller of that payload here. Write \
                     the payload with --units <file.json>"
                ));
            }
            if argv.is_empty() {
                return Err("`--` with no command after it submits nothing".to_string());
            }
            // `ProcessPayload`, not a JSON literal. Spelling it by hand here
            // is what shipped `{"argv": […]}` with no `timeout_secs` and no
            // `result`, which every donor refused as `payload-not-canonical`
            // — silently, five seconds at a time, while `job status` went on
            // reporting the unit `queued`. Building the executor's own type
            // makes a missing field a compile error here instead (ARCH §10.6,
            // §7).
            let mut payload = ProcessPayload::command(argv.to_vec());
            if let Some(secs) = timeout_secs {
                payload.timeout_secs = secs;
            }
            let body = serde_json::to_value(&payload)
                .map_err(|e| format!("the payload could not be encoded: {e}"))?;
            let unit = seal::seal(kind.clone(), body, JobRequirements::any(), None)
                .map_err(|e| e.to_string())?;
            Ok(vec![unit])
        }
    }
}

/// The wall cap the submitted units actually carry, when they agree on one.
///
/// `None` when a unit's payload states none — which `process:v1` cannot, and
/// which is reported rather than filled in.
fn wall_cap_of(units: &[JobUnit]) -> Option<u64> {
    let mut caps = units
        .iter()
        .map(|u| u.payload.get("timeout_secs").and_then(|v| v.as_u64()));
    let first = caps.next().flatten()?;
    caps.all(|c| c == Some(first)).then_some(first)
}

/// The one kind whose payload shape this CLI spells — the literal the crate
/// that owns the executor exports, not a copy of it (ARCH §10.6).
const PROCESS_V1: &str = commonwealth_work::process::PROCESS_KIND;

fn units_from_file(kind: &JobKind, path: &str) -> Result<Vec<JobUnit>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let rows: Vec<UnitSpec> = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "{path} is not a units file: {e}\n\
             It is a JSON ARRAY of objects, each `{{\"payload\": {{…}}}}` with an \
             optional `\"requirements\": {{\"os\": …, \"arch\": …, \"repo_rev\": …}}`."
        )
    })?;
    if rows.is_empty() {
        return Err(format!(
            "{path} declares no units — a submission with none offers nothing to take"
        ));
    }
    rows.into_iter()
        .map(|row| {
            seal::seal(
                kind.clone(),
                row.payload,
                row.requirements.unwrap_or_else(JobRequirements::any),
                None,
            )
            .map_err(|e| e.to_string())
        })
        .collect()
}

/// One row of a `--units` file. The unit's identity is NOT read from the file:
/// `seal` derives it from the payload, so a hand-written `unit_hash` cannot
/// name work other than the payload beside it (ARCH §7.5).
#[derive(serde::Deserialize)]
struct UnitSpec {
    payload: serde_json::Value,
    #[serde(default)]
    requirements: Option<JobRequirements>,
}

fn collect_allowed(args: &[String]) -> Result<Option<Vec<ActorKey>>, String> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--allow" {
            let Some(raw) = args.get(i + 1) else {
                return Err(
                    "--allow takes an actor key (64 lowercase hex characters, as \
                            `svrn ring roster list --ring work` prints it)"
                        .to_string(),
                );
            };
            keys.push(ActorKey::parse(raw).map_err(|e| e.to_string())?);
            i += 2;
            continue;
        }
        i += 1;
    }
    Ok((!keys.is_empty()).then_some(keys))
}

// ── status ───────────────────────────────────────────────────

/// `svrn job status` — the fold, and everything the fold could not account for.
async fn run_status(args: &[String]) -> i32 {
    let needle = args
        .first()
        .map(String::as_str)
        .filter(|a| !a.starts_with("--"));

    let v = match rail_log(WORK_NAMESPACE).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("job status: {e}");
            return 1;
        }
    };
    if args.iter().any(|a| a == "--json") {
        println!("{v}");
        return 0;
    }

    let admission = match admission_from_wire(&v) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("job status: {e}");
            return 1;
        }
    };
    let proj = WorkProjection::fold(&admission);
    let now_ms = now_ms();

    let matching: Vec<_> = proj
        .handoffs
        .iter()
        .filter(|(id, _)| needle.is_none_or(|n| names_handoff(id, n)))
        .collect();

    println!(
        "{WORK_NAMESPACE} — {} handoff(s), {} offer(s) from donors",
        matching.len(),
        proj.offers.len()
    );
    println!();
    if matching.is_empty() {
        match needle {
            Some(n) => println!("  no handoff on this journal starts with `{n}`."),
            None => println!("  nothing submitted yet."),
        }
    }
    for (id, handoff) in matching {
        println!(
            "  {}  {}  {}",
            id.to_hex(),
            handoff.kind,
            phase_label(&handoff.phase_at(now_ms))
        );
        println!(
            "    submitted {} by {}",
            short_stamp((handoff.submitted_at_ms / 1000) as i64),
            short_key(handoff.submitter.as_str())
        );
        for (hash, unit) in &handoff.units {
            println!("    {}  {}", short_key(hash), unit_line(unit, now_ms));
        }
    }

    // Everything the fold refused to apply, and everything admission refused
    // to admit. Both are printed unconditionally: an empty projection beside a
    // non-zero gap count is a completely different fact from an empty
    // projection on a quiet ring, and only one of them means "your key is not
    // in this roster" (ARCH §18.3).
    println!();
    if proj.unreadable > 0 {
        println!(
            "  {} admitted line(s) this build could not USE — a peer on a newer act \
             vocabulary, or an act about a unit nobody submitted.",
            proj.unreadable
        );
    }
    if !proj.lost_leases.is_empty() {
        println!(
            "  {} lost lease(s) — a second donor took a unit somebody already held \
             and must cancel it.",
            proj.lost_leases.len()
        );
    }
    if proj.double_deliveries > 0 {
        println!(
            "  {} repeated report(s) — delivery is at-least-once and idempotent per unit.",
            proj.double_deliveries
        );
    }

    // `admission_from_wire` already refused an answer with no `gaps` key, so
    // an empty list here is the daemon saying "none" rather than this build
    // failing to find them — which is what makes the sentence below a fact.
    let empty = Vec::new();
    let gaps = v.get("gaps").and_then(|g| g.as_array()).unwrap_or(&empty);
    if gaps.is_empty() {
        println!("  complete — every op this node holds is accounted for.");
        return 0;
    }
    println!("  INCOMPLETE — this is what could be read, and:");
    for gap in gaps {
        // The sentence is the RAIL's, on the wire. `UnknownSigner` here is the
        // whole answer to "why is my submission not in the fold" — a key the
        // `work` roster does not carry — so composing prose for it in the
        // terminal would be a second wording of one condition (ARCH §10.6),
        // and dropping it would turn a consent failure into silence.
        match gap.get("message").and_then(|m| m.as_str()) {
            Some(sentence) => println!("    {sentence}"),
            None => println!("    {gap}"),
        }
    }
    println!();
    println!("  A gap does not make the fold above wrong, it makes it PARTIAL.");
    0
}

/// Rebuild the `Admission` the daemon already computed, so the fold here is
/// the SAME function the donor loop runs.
///
/// The alternative was walking `ops` in this file and deciding what an act
/// means, which is a second fold and therefore a second answer to who holds a
/// lease (ARCH §10.6). `AdmittedOp` gained `Deserialize` for exactly this.
///
/// `floors` is empty and that is correct rather than lossy: it is an INPUT to
/// admission — the sealed floor below which a missing op is absent by
/// agreement rather than a hole — and the daemon has already applied it to the
/// `ops` and `gaps` on the wire. The fold reads neither.
fn admission_from_wire(v: &serde_json::Value) -> Result<Admission, String> {
    // THE KEYS ARE REQUIRED, the contents are not, and the asymmetry is the
    // point (ARCH §18.3). An answer with no `ops` at all folds to an empty
    // projection, and this verb would then print "nothing submitted yet" — a
    // confident claim about the ring composed out of a shape this build could
    // not read. Absence of the key is a daemon/CLI mismatch and says so;
    // absence of any op is a quiet ring and is `[]` on the wire.
    let missing = |k: &str| {
        format!("the daemon's log answer carried no `{k}` — this build and that daemon do not agree on the shape of `/v1/rail/log`")
    };
    let ops: Vec<AdmittedOp> = serde_json::from_value(
        v.get("ops").cloned().ok_or_else(|| missing("ops"))?,
    )
    .map_err(|e| format!("the daemon's log answer carried ops this build cannot read: {e}"))?;
    // Gaps are decoded for their COUNT, which the fold reports; the sentences
    // are rendered from the wire value itself, above. A gap kind a newer
    // daemon added is not a reason to refuse the whole answer — but a missing
    // key still is, because "no gaps" is what this verb reports as complete.
    let gaps: Vec<RailGap> =
        serde_json::from_value(v.get("gaps").cloned().ok_or_else(|| missing("gaps"))?)
            .unwrap_or_default();
    let held = v
        .get("held")
        .and_then(|h| h.as_u64())
        .ok_or_else(|| missing("held"))? as usize;
    Ok(Admission {
        ops,
        gaps,
        held,
        floors: BTreeMap::new(),
    })
}

/// Does this handoff answer to what the operator typed? Prefix over the full
/// hex, so both the id `job submit` printed and a short prefix of it work.
fn names_handoff(id: &HandoffId, needle: &str) -> bool {
    let needle = needle.trim().trim_start_matches("handoff-");
    !needle.is_empty() && id.to_hex().starts_with(needle)
}

fn unit_line(unit: &commonwealth_work::projection::ProjectedUnit, now_ms: u64) -> String {
    match unit.status_at(now_ms) {
        WorkUnitStatus::Queued { prior_attempts } if prior_attempts == 0 => "queued".to_string(),
        WorkUnitStatus::Queued { prior_attempts } => {
            format!("queued (after {prior_attempts} attempt(s))")
        }
        WorkUnitStatus::Leased {
            lessee,
            expires_at_ms,
            attempts,
            ..
        } => format!(
            "leased by {} until {} (attempt {attempts})",
            short_key(lessee.as_str()),
            short_stamp((expires_at_ms / 1000) as i64)
        ),
        WorkUnitStatus::Complete {
            lessee,
            outcome,
            attempts,
            ..
        } => format!(
            "{} by {} on attempt {attempts} — {}",
            outcome.verdict().as_str(),
            short_key(lessee.as_str()),
            outcome.reason()
        ),
        WorkUnitStatus::Failed {
            last_lessee,
            reason,
            attempts,
            outcome,
        } => {
            // Two different facts and they are kept apart: a unit that finished
            // badly reported an outcome, and a unit nobody ever finished did
            // not. Collapsing them would lose which one this is.
            let verdict = outcome
                .as_ref()
                .map(|j| j.verdict().as_str())
                .unwrap_or("never-reported");
            format!(
                "terminal ({verdict}) after {attempts} attempt(s), last {} — {reason}",
                short_key(last_lessee.as_str())
            )
        }
    }
}

fn phase_label(phase: &commonwealth_core::knowledge::HandoffPhase) -> String {
    use commonwealth_core::knowledge::HandoffPhase as P;
    match phase {
        P::Open => "open".to_string(),
        P::Draining => "draining".to_string(),
        P::Merging => "merging".to_string(),
        P::Complete => "complete".to_string(),
        P::Failed { reason } => format!("failed — {reason}"),
    }
}

/// A 64-hex actor key or a 64-hex unit hash, shortened for a terminal. Never
/// used where the value is going to be typed back in.
fn short_key(raw: &str) -> String {
    if raw.len() > 12 {
        format!("{}…", &raw[..12])
    } else {
        raw.to_string()
    }
}

fn now_ms() -> u64 {
    sovereign_core::time::unix_millis()
}

// ── argv ─────────────────────────────────────────────────────

/// Split at the FIRST bare `--`. Everything after it is the unit's command and
/// is never read as a flag of this verb — which is what lets a unit carry
/// `--json` without this CLI answering it.
fn split_at_double_dash(args: &[String]) -> (&[String], Option<&[String]>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], Some(&args[i + 1..])),
        None => (args, None),
    }
}

/// The value after `name`, if it is there. Same shape as `ring_cmd`'s, kept
/// local because that one is private to its module and this one stops at the
/// `--` boundary by virtue of the slice it is handed.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// The usage text spells the namespace, and `concat!` cannot interpolate a
    /// const. Failing input: `WORK_NAMESPACE` renamed — the help page would go
    /// on telling people to add a roster entry to a ring that no longer exists.
    #[test]
    fn the_usage_page_names_the_namespace_this_verb_writes_to() {
        assert!(
            USAGE.contains(&format!("--ring {WORK_NAMESPACE}")),
            "usage names a different ring than the verb writes to:\n{USAGE}"
        );
    }

    /// `--help` is this verb's, and the unit's command is not. Failing input:
    /// `job submit --kind process:v1 -- svrn chat --help` — answered as help,
    /// it would print a page and submit nothing.
    #[test]
    fn help_after_the_double_dash_belongs_to_the_unit() {
        let mine = s(&["submit", "--help"]);
        let (head, _) = split_at_double_dash(&mine);
        assert!(head.iter().any(|a| a == "--help"));

        let theirs = s(&[
            "submit",
            "--kind",
            "process:v1",
            "--",
            "svrn",
            "chat",
            "--help",
        ]);
        let (head, argv) = split_at_double_dash(&theirs);
        assert!(!head.iter().any(|a| a == "--help"));
        assert!(argv.expect("argv").contains(&"--help".to_string()));
    }

    /// Failing input: `--kind process:v1 -- svrn chat --json`. Before the
    /// split, `--json` after `--` would be read as a flag of `job submit`.
    #[test]
    fn the_units_command_is_not_read_as_this_verbs_flags() {
        let args = s(&["--kind", "process:v1", "--", "svrn", "chat", "--json"]);
        let (head, argv) = split_at_double_dash(&args);
        assert_eq!(flag(head, "--kind"), Some("process:v1"));
        assert_eq!(argv.unwrap(), &s(&["svrn", "chat", "--json"])[..]);
        assert!(flag(head, "--json").is_none());
    }

    /// The argv form is scoped to the one kind whose payload this CLI spells.
    /// Failing input: `--kind ingest:v1 -- anything` — accepted, it would seal
    /// an `argv` payload no `ingest` executor reads and the unit would be
    /// leased and then fail on every donor.
    #[test]
    fn a_trailing_command_is_refused_for_a_kind_this_cli_cannot_spell() {
        let kind = JobKind::parse("ingest:v1").expect("kind");
        let argv = s(&["uname"]);
        let err = resolve_units(&kind, None, Some(&argv), None).expect_err("refused");
        assert!(err.contains("ingest:v1"), "{err}");
        assert!(err.contains("--units"), "{err}");
    }

    /// `process:v1 -- uname -a` is the shortest true submission, and the seal
    /// must cover the payload the argv produced.
    #[test]
    fn a_trailing_command_seals_one_process_unit() {
        let kind = JobKind::parse(PROCESS_V1).expect("kind");
        let argv = s(&["uname", "-a"]);
        let units = resolve_units(&kind, None, Some(&argv), None).expect("sealed");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].payload["argv"], serde_json::json!(["uname", "-a"]));
        seal::verify(&units[0]).expect("the seal covers the payload");
    }

    /// Saying it twice is a refusal, not a precedence rule: a units file and a
    /// command line that disagree have no right answer.
    #[test]
    fn units_file_and_trailing_command_together_are_refused() {
        let kind = JobKind::parse(PROCESS_V1).expect("kind");
        let argv = s(&["uname"]);
        let err = resolve_units(&kind, Some("units.json"), Some(&argv), None).expect_err("refused");
        assert!(err.contains("--units"), "{err}");
    }

    /// A prefix names a handoff; `handoff-` in front of it is the Display
    /// spelling and is accepted. An empty needle names nothing — otherwise
    /// `job status ""` would silently mean "everything".
    #[test]
    fn a_handoff_is_named_by_a_prefix_of_its_hex() {
        let id = HandoffId::from_u128(0x1234_5678_9abc_def0_1122_3344_5566_7788);
        let hex = id.to_hex();
        assert!(names_handoff(&id, &hex));
        assert!(names_handoff(&id, &hex[..8]));
        assert!(names_handoff(&id, &format!("handoff-{}", &hex[..16])));
        assert!(!names_handoff(&id, "ffff"));
        assert!(!names_handoff(&id, ""));
    }

    /// The wire shape `GET /v1/rail/log` returns must fold. Failing input: the
    /// answer for an EMPTY ring, which carries `ops: []` and no `floors` key at
    /// all — the shape that would break a naive `serde_json::from_value` into
    /// `Admission`.
    #[test]
    fn the_log_answer_folds_without_a_floors_field() {
        let wire = serde_json::json!({
            "namespace": "work",
            "ops": [],
            "gaps": [],
            "held": 0,
            "complete": true,
        });
        let admission = admission_from_wire(&wire).expect("folds");
        let proj = WorkProjection::fold(&admission);
        assert!(proj.handoffs.is_empty());
        assert_eq!(proj.gaps, 0);
    }

    /// A gap on the wire carries the rail's rendered `message` beside its
    /// tagged fields, and the extra key must not cost this reader the count.
    #[test]
    fn a_gap_with_its_rendered_sentence_still_counts() {
        let wire = serde_json::json!({
            "ops": [],
            "gaps": [{
                "gap": "unknown_signer",
                "id": "op-deadbeef",
                "actor": "aa".repeat(32),
                "message": "an op signed by aaaaaaaaaaaa… — nobody in the roster claims that key",
            }],
            "held": 1,
        });
        let admission = admission_from_wire(&wire).expect("folds");
        assert_eq!(admission.gaps.len(), 1);
        assert_eq!(WorkProjection::fold(&admission).gaps, 1);
    }

    /// An answer missing a key that carries the ANSWER is refused by name,
    /// never folded. Failing input: each of the three keys dropped in turn from
    /// an otherwise valid empty-ring answer — the shape a daemon on a different
    /// wire version would send, and the shape that would otherwise make this
    /// verb print "nothing submitted yet" about a ring it could not read
    /// (ARCH §18.3).
    #[test]
    fn a_log_answer_missing_a_key_is_refused_and_says_which() {
        for dropped in ["ops", "gaps", "held"] {
            let mut wire = serde_json::json!({"ops": [], "gaps": [], "held": 0});
            wire.as_object_mut().expect("object").remove(dropped);
            let err = admission_from_wire(&wire).expect_err("refused");
            // Named key AND the shape sentence. `contains(dropped)` alone is
            // not a gate: a lenient `unwrap_or_default()` on `ops` hands
            // serde a `null`, whose own decode error also contains the word
            // "ops" — so the weaker assertion passed under the very
            // substitution this test exists to forbid (watched, ARCH §18.1).
            assert!(
                err.contains(dropped) && err.contains("do not agree on the shape"),
                "the refusal must name the missing key as a shape mismatch; got: {err}"
            );
        }
    }

    /// THE ONE THIS VERB SHIPPED WITHOUT. The argv form's payload has to be a
    /// body the only executor registered for `process:v1` will run, and for
    /// cw-lift 5d it was not: `{"argv": […]}` with no `timeout_secs` and no
    /// `result`, which `ProcessExecutor::validate` refuses as
    /// `payload-not-canonical`. The donor refused it every five seconds, on a
    /// trace target nobody had turned on, while `job status` reported the unit
    /// `queued` — a submitter with no way to find out.
    ///
    /// Failing input: the exact command from this verb's own `--help`. Spell
    /// the payload by hand again and this goes red.
    #[test]
    fn the_argv_form_produces_a_body_the_only_executor_for_that_kind_accepts() {
        use commonwealth_work::executor::JobExecutor;
        use commonwealth_work::process::ProcessExecutor;

        let kind = JobKind::parse(PROCESS_V1).expect("kind");
        let units = resolve_units(&kind, None, Some(&s(&["uname", "-a"])), None).expect("sealed");
        ProcessExecutor::new()
            .validate(&units[0])
            .expect("the one executor for this kind must accept what this verb submits");
    }

    /// `--timeout` reaches the payload, and its absence is the ONE named
    /// default rather than a second number spelled here. Failing input: a cap
    /// the flag set that the body does not carry.
    #[test]
    fn the_wall_cap_comes_from_the_flag_or_from_the_one_default() {
        let kind = JobKind::parse(PROCESS_V1).expect("kind");
        let argv = s(&["sleep", "1"]);

        let dflt = resolve_units(&kind, None, Some(&argv), None).expect("sealed");
        assert_eq!(
            dflt[0].payload["timeout_secs"],
            commonwealth_work::process::DEFAULT_TIMEOUT_SECS
        );

        let named = resolve_units(&kind, None, Some(&argv), Some(30)).expect("sealed");
        assert_eq!(named[0].payload["timeout_secs"], 30);
        // And the seal still covers the body it was given, cap included.
        seal::verify(&named[0]).expect("the seal covers the payload");
    }

    /// `--allow` is repeatable and each value is a real key. Failing input: a
    /// display-shortened key, which is a prefix and not an identity.    /// `--allow` is repeatable and each value is a real key. Failing input: a
    /// display-shortened key, which is a prefix and not an identity.
    #[test]
    fn allow_collects_every_key_and_refuses_a_short_one() {
        let a = "aa".repeat(32);
        let b = "bb".repeat(32);
        let args = s(&["--allow", &a, "--allow", &b]);
        let got = collect_allowed(&args).expect("parsed").expect("some");
        assert_eq!(got.len(), 2);

        assert!(collect_allowed(&s(&["--allow", "aabbcc"])).is_err());
        assert!(collect_allowed(&s(&["--kind", "process:v1"]))
            .expect("parsed")
            .is_none());
    }
}
