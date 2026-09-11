// SPDX-License-Identifier: AGPL-3.0-or-later
//! A **package-only work peer** — cw-lift 5f, the campaign's second lift.
//!
//! ```text
//! work_peer --root <rail-dir> [--workdir <dir>] [--label <name>]
//!            [--image <container-image>] [-- <shard argv...>]
//! ```
//!
//! The campaign's CLAIM block asks for a second application composing on the
//! commonwealth substrate with **zero sovereign-\* and zero corpus-engine**.
//! This is it: a third party's whole program, holding a roster key, folding
//! the `work` journal, leasing, running and reporting — the same lease → run →
//! renew → report shape as `sovereign-mesh/src/work_donor.rs`, with none of
//! `sovereign-mesh` underneath it. Its entire import surface is
//! `commonwealth-work`, `commonwealth-rail`, `commonwealth-core`,
//! `oicp-types`, `kernel-types` and `tokio`.
//!
//! It is a peer and not a test because the thing being proven is that the
//! closure BUILDS AND RUNS off this monorepo. `scripts/cw-work-lift.sh
//! --sandbox` copies the closure to a scratch directory, synthesises a root
//! workspace, builds it there and runs this binary; a `cargo tree` count
//! cannot produce that fact (BOUNDARY.md §"What a green gate does not prove").
//!
//! # The three units, and why these three
//!
//! Heterogeneous in the two ways the plane can actually tell apart — the kind
//! of work, and where the verdict comes from:
//!
//! | Unit | Work | `ResultSource` | Verdict from |
//! |---|---|---|---|
//! | `shell` | a `sh -c` one-liner reporting the donor's machine | `Stdout` | the exit code, text carried back |
//! | `sim` | a Python Monte-Carlo simulation that judges its own error bound | `VerdictLine` | a `Judgement` the unit printed |
//! | `shard` | a test shard — the lifted package's own `cargo test` | `ExitCodeOnly` | the exit code, nothing carried back |
//!
//! Three copies of one command would exercise one code path three times.
//! These exercise three: `verdict_from_stdout` is reachable only from the
//! second, and only the second can report `could-not-judge` from a process
//! that exited 0.
//!
//! # Exit codes
//!
//! `0` all three units reached `Complete` with a passing verdict · `1` the
//! plane ran and a unit did not · `3` could-not-judge: a precondition of the
//! run is absent, so nothing was measured (ARCH §18.2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use commonwealth_core::clock::unix_now_millis;
use commonwealth_core::ids::HandoffId;
use commonwealth_rail::{Ed25519Verifier, Person, RailAct, RingJournal, Roster, SigningKey};
use commonwealth_work::executor::{subject_of, JobContext, JobExecutor, JobExecutorRegistry};
use commonwealth_work::process::{ProcessExecutor, ProcessPayload, ResultSource, PROCESS_KIND};
use commonwealth_work::projection::{lease_state, LeaseState, WorkProjection, WorkUnitStatus};
use commonwealth_work::refusal::{host_satisfies, may_take};
use commonwealth_work::sandbox::Sandbox;
use commonwealth_work::{
    act, seal, ActorKey, Completion, Failure, Submission, UnitRef, WorkAct, WORK_NAMESPACE,
};
use kernel_types::quality::Precondition;
use kernel_types::{ContentHash, Verdict};
use oicp_types::{Isolation, JobKind, JobRequirements, JobUnit, WorkOffer};

/// The test shard's argv when the caller does not supply one after `--`. The
/// instrument does supply one, pointing at the tree it has just built.
const SHARD: &str = "cargo test --offline -q -p commonwealth-rail-core";

/// The whole run's wall bound. Not a flag: a demo whose deadline is tunable
/// from outside has a deadline nobody can reason about.
const DEADLINE: Duration = Duration::from_secs(1800);

/// One unit's job in the demo, and the label the report prints for it.
struct PlannedUnit {
    what: &'static str,
    unit: JobUnit,
}

/// A deterministic Monte-Carlo estimate of pi that judges its OWN error bound
/// and prints the `Judgement` as its last stdout line — the `VerdictLine`
/// protocol `verdict_from_stdout` reads. An LCG rather than `random` so two
/// donors running this unit get the same answer, which is what makes a
/// donated verdict comparable at all.
const SIM: &str = r#"
s, hits, n = 12345, 0, 200000
for _ in range(n):
    s = (1103515245 * s + 12345) % (2**31); x = s / 2**31
    s = (1103515245 * s + 12345) % (2**31); y = s / 2**31
    hits += (x*x + y*y) <= 1.0
est = 4.0 * hits / n
err = abs(est - 3.141592653589793)
ok = err < 0.02
print('{"subject": "monte-carlo pi, 200k samples", "verdict": "%s", "reason": "estimated %.5f, error %.5f against a 0.02 bound"}'
      % ("passed" if ok else "failed", est, err))
"#;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cfg = match PeerArgs::parse(&args) {
        Ok(cfg) => cfg,
        Err(why) => {
            eprintln!("work_peer: {why}");
            return ExitCode::from(3);
        }
    };
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("work_peer: no tokio runtime on this host: {e}");
            return ExitCode::from(3);
        }
    };
    match rt.block_on(run(&cfg)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(why) => {
            eprintln!("work_peer: {why}");
            ExitCode::from(3)
        }
    }
}

struct PeerArgs {
    root: PathBuf,
    workdir: PathBuf,
    label: String,
    /// The image a unit runs inside, on THIS host. Absent means this peer has
    /// no boundary and therefore offers nothing — which is a working peer
    /// that declines, not a broken one. It is a flag rather than a constant
    /// because the package ships no image and must not pretend to: a third
    /// party naming their own is the whole point of the lift.
    image: Option<String>,
    shard: Vec<String>,
}

impl PeerArgs {
    fn parse(args: &[String]) -> Result<PeerArgs, String> {
        let mut cfg = PeerArgs {
            root: PathBuf::new(),
            workdir: std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?,
            label: "cw-work-lift peer".to_string(),
            image: std::env::var("CW_WORK_IMAGE").ok(),
            shard: SHARD.split_whitespace().map(str::to_string).collect(),
        };
        let mut it = args.iter().skip(1);
        while let Some(arg) = it.next() {
            let mut value = || it.next().cloned().ok_or(format!("{arg} wants a value"));
            match arg.as_str() {
                "--root" => cfg.root = PathBuf::from(value()?),
                "--workdir" => cfg.workdir = PathBuf::from(value()?),
                "--label" => cfg.label = value()?,
                "--image" => cfg.image = Some(value()?),
                // Everything past `--` is the test shard's argv, so the
                // instrument can point the shard at the tree it just built
                // without this file knowing where that is.
                "--" => {
                    cfg.shard = it.cloned().collect();
                    break;
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        if cfg.root.as_os_str().is_empty() {
            return Err("--root <rail-dir> is required".to_string());
        }
        if cfg.shard.is_empty() {
            return Err("the shard argv after `--` is empty".to_string());
        }
        Ok(cfg)
    }
}

/// Mint this peer's key from its label.
///
/// Deterministic on purpose: a second peer is `--label` and nothing else, and
/// a run that can be repeated is a run whose journal can be diffed. The seed
/// is a blake3 of the label through `kernel-types`' own hash, so there is no
/// second content-hash implementation here (ARCH §10.6).
fn key_of(label: &str) -> SigningKey {
    SigningKey::from_bytes(ContentHash::of_str(label).as_bytes())
}

// The host half of consent is `commonwealth_work::refusal::host_satisfies`
// now, not a copy here. cw-lift 5f shipped this peer with its own twelve-line
// version that checked `Binary` and refused `Container` outright — so a unit
// this machine could in fact have run was declined, silently, by a second
// decider nobody compared against the first.

fn plan(cfg: &PeerArgs) -> Result<Vec<PlannedUnit>, String> {
    let kind = JobKind::parse(PROCESS_KIND).map_err(|e| e.to_string())?;
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let shell = ProcessPayload {
        timeout_secs: 60,
        ..ProcessPayload::command(argv(&["sh", "-c", "uname -sm && echo cwd=$(pwd)"]))
    };
    let sim = ProcessPayload {
        timeout_secs: 300,
        result: ResultSource::VerdictLine,
        ..ProcessPayload::command(argv(&["python3", "-c", SIM]))
    };
    let shard = ProcessPayload {
        timeout_secs: 900,
        result: ResultSource::ExitCodeOnly,
        ..ProcessPayload::command(cfg.shard.clone())
    };

    let want = |bin: &str| JobRequirements {
        preconditions: vec![Precondition::Binary(bin.to_string())],
        ..JobRequirements::any()
    };
    let mut out = Vec::new();
    for (what, payload, reqs) in [
        ("shell one-liner", shell, want("sh")),
        ("python simulation", sim, want("python3")),
        ("test shard", shard, want(&cfg.shard[0])),
    ] {
        let body = serde_json::to_value(&payload).map_err(|e| e.to_string())?;
        let unit = seal::seal(kind.clone(), body, reqs, None).map_err(|e| e.to_string())?;
        out.push(PlannedUnit { what, unit });
    }
    Ok(out)
}

/// Sign one act onto the local `work` journal. THE door — the same one
/// `work_donor::append` uses, minus the daemon's ring-sync nudge.
fn append(
    journal: &RingJournal,
    key: &SigningKey,
    roster: &Roster,
    a: &WorkAct,
) -> Result<(), String> {
    let payload = act::to_payload(a)?;
    journal
        .append(RailAct::Record { payload }, key, roster)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn fold(journal: &RingJournal, roster: &Roster) -> Result<WorkProjection, String> {
    let admission = journal
        .admit(roster, &Ed25519Verifier)
        .map_err(|e| e.to_string())?;
    Ok(WorkProjection::fold(&admission))
}

async fn run(cfg: &PeerArgs) -> Result<bool, String> {
    let key = key_of(&cfg.label);
    let me =
        ActorKey::parse(commonwealth_rail::RingSigner::actor(&key)).map_err(|e| e.to_string())?;
    let journal = RingJournal::open(&cfg.root, WORK_NAMESPACE).map_err(|e| e.to_string())?;
    let roster = Roster::new(BTreeMap::from([(
        Person::from(cfg.label.as_str()),
        vec![me.as_str().to_string()],
    )]));
    journal.set_roster(&roster).map_err(|e| e.to_string())?;

    let planned = plan(cfg)?;
    for p in &planned {
        // Refused BEFORE anything is written. A precondition this host cannot
        // meet is not evidence that the plane is broken, so the run abstains
        // rather than reporting a failure it did not measure (ARCH §18.2).
        host_satisfies(&p.unit).map_err(|why| format!("{}: {why}", p.what))?;
    }
    let handoff = HandoffId::generate();
    let kind = JobKind::parse(PROCESS_KIND).map_err(|e| e.to_string())?;
    let units: Vec<JobUnit> = planned.iter().map(|p| p.unit.clone()).collect();
    append(
        &journal,
        &key,
        &roster,
        &WorkAct::Submit(Submission::new(
            handoff,
            kind.clone(),
            units,
            None,
            Some(3600),
        )),
    )?;

    // THE REGISTRY COMES FIRST, because the floor is asked BEFORE anything is
    // published. This peer used to build its offer, append it, and only then
    // register an executor — which is how a third-party donor came to
    // advertise `process:v1` with nothing in front of a stranger's argv. The
    // decider is `commonwealth-work`'s, so a donor built from this crate
    // cannot get it wrong by forgetting to write it.
    // PROBE, then register with what the probe found — the same order and the
    // same decider the daemon uses. What this peer PROVIDES is never asserted
    // here: it is whatever `Sandbox::probe` could actually confirm on this
    // host, which is the §18.3 rule that keeps a config from claiming a
    // boundary a machine does not have.
    let (sandbox, why) = Sandbox::probe(cfg.image.as_deref());
    if let Some(reason) = &why {
        eprintln!("work_peer: no boundary — {reason}");
    }
    let (peer_provides, platform) = (sandbox.provides(), sandbox.platform());
    let mut registry = JobExecutorRegistry::new();
    registry
        .register(Arc::new(ProcessExecutor::with_sandbox(sandbox)))
        .map_err(|e| e.to_string())?;

    let partition = registry.offerable(std::slice::from_ref(&kind), peer_provides);
    for dropped in &partition.dropped {
        eprintln!("work_peer: NOT offering {dropped}");
    }
    if partition.offerable.is_empty() {
        // ABSTAIN, and say which — never a silent exit and never a failure.
        // This peer lifting and building is one question; whether this build
        // may donate is another, and collapsing them would report a working
        // closure as a broken one (ARCH §18.3). Exit 3 is the lift
        // instrument's could-not-judge.
        eprintln!(
            "work_peer: this host provides `{peer_provides:?}` isolation and every \
             offered kind demands more, so it publishes no offer and donates \
             nothing. The package lifted and ran; it declined to execute a \
             stranger's argv without a boundary. Give it `--image <ref>` on a \
             host with a rootless runtime and it donates."
        );
        std::process::exit(3);
    }

    let offer = WorkOffer {
        kinds: partition.offerable,
        max_concurrent: 1,
        yield_to_foreground: false,
        isolation: peer_provides,
        os: platform.0,
        arch: platform.1,
        repos: Vec::new(),
        accept_from: None,
    };
    append(&journal, &key, &roster, &WorkAct::Offer(offer.clone()))?;
    let (os, arch, root) = (&offer.os, &offer.arch, cfg.root.display());
    eprintln!("work_peer: {me} offers {PROCESS_KIND} on {os}/{arch}, rail at {root}");
    let started = Instant::now();
    loop {
        let proj = fold(&journal, &roster)?;
        if proj.gaps > 0 || proj.unreadable > 0 {
            return Err(format!(
                "the `work` journal folded with {} gap(s) and {} unreadable line(s)",
                proj.gaps, proj.unreadable
            ));
        }
        let takeable = proj.takeable_at(unix_now_millis());
        if takeable.is_empty() {
            break;
        }
        let mut progressed = false;
        for unit_ref in takeable {
            if may_take(&proj, &me, &offer, &unit_ref, unix_now_millis()).is_err() {
                continue;
            }
            let Some(unit) = proj.unit(&unit_ref).map(|p| p.unit.clone()) else {
                continue;
            };
            let Some(executor) = registry.resolve(&unit.kind) else {
                eprintln!("work_peer: no executor for {}", unit.kind);
                continue;
            };
            if let Err(refusal) = executor.validate(&unit) {
                eprintln!("work_peer: refused {}: {refusal}", unit_ref.unit_hash);
                continue;
            }
            append(&journal, &key, &roster, &WorkAct::Lease(unit_ref.clone()))?;
            let act = run_unit(
                &executor,
                &journal,
                &roster,
                &key,
                &me,
                &unit,
                &unit_ref,
                &cfg.workdir,
            )
            .await;
            append(&journal, &key, &roster, &act)?;
            progressed = true;
        }
        // A round that took nothing and changed nothing would spin. The only
        // thing that can change without this peer acting is a lease lapsing,
        // which is a clock the deadline below already bounds.
        if !progressed {
            break;
        }
        if started.elapsed() > DEADLINE {
            return Err(format!("the run passed its {DEADLINE:?} deadline"));
        }
    }
    report(&fold(&journal, &roster)?, &planned, &handoff)
}

/// Run one unit, renewing the lease under it, and say what happened.
///
/// `work_donor::run_unit`'s shape: the heartbeat and the future race in one
/// `select!`, a lost lease cancels rather than being reported over, and a
/// renew that cannot be appended is warned about rather than swallowed.
async fn run_unit(
    executor: &Arc<dyn JobExecutor>,
    journal: &RingJournal,
    roster: &Roster,
    key: &SigningKey,
    me: &ActorKey,
    unit: &JobUnit,
    unit_ref: &UnitRef,
    workdir: &Path,
) -> WorkAct {
    let ctx = JobContext::new(workdir);
    let interval = executor.descriptor().lease_interval_ms.max(1);
    let outcome = {
        let fut = executor.execute(unit, &ctx);
        tokio::pin!(fut);
        let mut ticker = tokio::time::interval(Duration::from_millis(interval));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            tokio::select! {
                done = &mut fut => break done,
                _ = ticker.tick() => {
                    // THREE states, not two, and the third is why this is a
                    // `match`: a journal that could not be READ is not
                    // evidence that somebody else holds the lease, and killing
                    // a half-hour shard on it is the substitution §18.3
                    // forbids. The peer no longer draws that line itself —
                    // `commonwealth_work::lease_state` owns it now, so this
                    // program and the daemon's donor loop cannot disagree
                    // about what "lost" means. The I/O half stays here,
                    // because obtaining the fold is what differs between a
                    // peer with a journal and a daemon with an AppState.
                    let state = match fold(journal, roster) {
                        Ok(p) => lease_state(&p, unit_ref, me, unix_now_millis()),
                        Err(e) => {
                            eprintln!("work_peer: the fold was unreadable this heartbeat, holding ({e})");
                            LeaseState::Unknown
                        }
                    };
                    match state {
                        LeaseState::Held => {
                            if let Err(e) = append(journal, key, roster, &WorkAct::Renew(unit_ref.clone())) {
                                eprintln!("work_peer: a renew could not be appended: {e}");
                            }
                        }
                        LeaseState::Lost(why) => {
                            eprintln!("work_peer: lease lost on {} ({why}) — cancelling", unit_ref.unit_hash);
                            ctx.cancel();
                        }
                        LeaseState::Unknown => {}
                    }
                }
            }
        }
    };
    // THE THIRD HAND-BUILT ATTRIBUTION, and 5f named it as this crate's own
    // hole: every donor invented its own, which is precisely the value
    // `comparable_to` exists to compare. It also declared a REFUSAL to read
    // (`"a lifted peer does not read the donor's rustc"`) where the honest
    // answer is this peer's actual compiler — a lifted peer runs the unit, so
    // its rustc is the one that matters. One reader now, in the crate the peer
    // already links.
    let provenance = executor.attribution(&unit.requirements.repo_rev.clone().unwrap_or_default());
    match outcome {
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
            outcome: e.judgement(subject_of(unit)),
            provenance,
        }),
    }
}

/// The verdict table, and the one place this peer decides whether the run
/// passed: every unit reached `Complete`, and every verdict is `Passed`.
fn report(
    proj: &WorkProjection,
    planned: &[PlannedUnit],
    handoff: &HandoffId,
) -> Result<bool, String> {
    let Some(h) = proj.handoffs.get(handoff) else {
        return Err("the fold does not carry the handoff this peer submitted".to_string());
    };
    let mut all_good = true;
    println!(
        "{:<18}  {:<12}  {:<9}  {}",
        "unit", "status", "attempts", "verdict"
    );
    for p in planned {
        let (status, attempts, verdict) = match h
            .units
            .get(&p.unit.unit_hash)
            .map(|u| u.status_at(unix_now_millis()))
        {
            Some(WorkUnitStatus::Complete {
                attempts, outcome, ..
            }) => (
                outcome.verdict() == Verdict::Passed,
                attempts,
                outcome.verdict().to_string(),
            ),
            Some(WorkUnitStatus::Failed {
                attempts,
                outcome,
                reason,
                ..
            }) => (
                false,
                attempts,
                outcome
                    .map(|o| o.reason().as_str().to_string())
                    .unwrap_or(reason),
            ),
            Some(other) => (false, 0, format!("still {}", other.id())),
            None => (false, 0, "not in the fold".to_string()),
        };
        all_good &= status;
        println!(
            "{:<18}  {:<12}  {:<9}  {}",
            p.what,
            if status { "complete" } else { "not complete" },
            attempts,
            verdict
        );
    }
    println!(
        "{} unit(s), {} lost lease(s), {} double deliver(ies)",
        h.units.len(),
        proj.lost_leases.len(),
        proj.double_deliveries
    );
    Ok(all_good)
}
