// SPDX-License-Identifier: AGPL-3.0-or-later
//! Probing a precondition, and running one instrument to a verdict.
//!
//! The precondition PREDICATES live here rather than in `kernel_types`
//! deliberately: that crate parses text and validates closed sets and knows
//! nothing about a checkout, a daemon or a load average. The spelling is
//! shared; the probing is the runner's.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use kernel_types::quality::{Instrument, Precondition, VerdictSource};
use kernel_types::{Judgement, Reason};
use sovereign_cli_shared::lane_verdict;

use super::fingerprint::Fingerprint;
use super::select::baseline_dir;

/// What names the container on this repo's toolbox hosts. ONE reader, because
/// the predicate and the reason it prints must agree about what they looked at
/// (ARCH §10.6).
const CONTAINER_MARKER: &str = "/run/.containerenv";

/// Probe one [`Precondition`] against this host.
///
/// The predicates live here rather than in `kernel_types` deliberately: that
/// crate parses text and validates closed sets and knows nothing about a
/// checkout, a daemon or a load average. The SPELLING is shared; the probing
/// is the runner's.
pub(super) async fn check_precondition(p: &Precondition, base: &str) -> bool {
    let ok = match p {
        Precondition::PortListening(port) => std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], *port)),
            Duration::from_secs(2),
        )
        .is_ok(),
        Precondition::SlotDecodes(slot) => slot_decodes(base, slot).await,
        Precondition::CorpusInstalled(id) => {
            use sovereign_enrichment_catalog::corpus_state::{inspect_corpus_state, CorpusState};
            inspect_corpus_state(id) != CorpusState::Unindexed
        }
        Precondition::Binary(name) => locate_binary(name).is_some(),
        // The shell harnesses' spelling, which the lane runner lacked until
        // the merge. `/run/.containerenv` names the container on this repo's
        // toolbox hosts; its ABSENCE means the host side, which is a real
        // answer and not an error. An UNREADABLE one is neither, and
        // collapsing the two would hand the abstention a reason that is
        // simply false — "you are not inside `sovereign-vulkan`" when the
        // truth is that the probe could not look (ARCH §18.2: an abstention's
        // reason is part of the abstention).
        Precondition::Container(name) => match std::fs::read_to_string(CONTAINER_MARKER) {
            Ok(t) => t.contains(name.as_str()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                tracing::warn!(error = %e, "quality check: cannot read {CONTAINER_MARKER}");
                false
            }
        },
    };
    tracing::debug!(precondition = ?p, ok, "quality check: precondition");
    ok
}

/// How an unmet precondition reads in a could-not-judge reason.
///
/// Every one of them names the SUBJECT that is missing — a port, a slot, a
/// corpus, a binary, a container — because that is the only thing a
/// precondition is allowed to assert (ARCH §18.2). None of them can say "the
/// machine is busy"; load is a covariate on the row, not a gate in front of
/// it.
pub(super) fn describe_precondition(p: &Precondition) -> String {
    match p {
        Precondition::PortListening(port) => format!("nothing is listening on 127.0.0.1:{port}"),
        Precondition::SlotDecodes(s) => {
            format!("slot `{s}` did not decode a token — is the model resident?")
        }
        Precondition::CorpusInstalled(c) => format!("corpus `{c}` is not installed"),
        Precondition::Binary(b) => format!("binary `{b}` is not on this host"),
        Precondition::Container(c) => match std::fs::metadata(CONTAINER_MARKER) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!(
                "this shell is not inside the `{c}` container — the native toolchain is there"
            ),
            Err(e) => format!(
                "cannot read {CONTAINER_MARKER} ({e}), so whether this is the `{c}` container is \
                 unknown — this is the probe failing, not an answer about the host"
            ),
            Ok(_) => {
                format!("{CONTAINER_MARKER} does not name `{c}` — this is a different container")
            }
        },
    }
}

/// Locate an executable: beside the running dispatcher first (co-built
/// target artifacts are what a developer actually means), then PATH. Same
/// discovery order as `llm_bin::locate`.
fn locate_binary(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(real) = std::fs::canonicalize(&exe) {
            if let Some(dir) = real.parent() {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    which::which(name).ok()
}

/// One token out of the named slot. The probe IS a decode.
async fn slot_decodes(base: &str, slot: &str) -> bool {
    let body = serde_json::json!({
        "model": slot,
        "messages": [{"role": "user", "content": "ok"}],
        "max_tokens": 1,
        "temperature": 0,
        "stream": false,
    });
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
    else {
        return false;
    };
    resp.status().is_success()
}

/// The conditions a row was measured under. **Recorded on every row, gating
/// nothing** — this is what replaced the `host-quiet` precondition on
/// 2026-09-08 (ARCH §18.2). That guard refused to judge a wall-clock row
/// above a load bound nobody derived, and on this host it was unmet most of
/// the time, so the rows it "protected" reported could-not-judge instead of a
/// number. When the world is inconvenient, record the world and judge
/// anyway.
///
/// Two fields because two different things make a run slow and they call for
/// opposite readings:
///
/// - `load` — the host's 1-minute average. A busy box decodes 2.8x slower
///   here (50.7 tok/s at load 3.7 against 17.8 at load 32, note `d596639c`),
///   so a slow row taken at load 30 is a caveat on the reading.
/// - `daemon_uptime_secs` — how long the resident stack has been up. A row
///   whose start uptime EXCEEDS its end uptime ran across a daemon restart,
///   and that row is not slow, it is interrupted: the models were evicted and
///   re-loaded under it. Two peer sessions share this daemon and one restarts
///   it deliberately, so the two cases are routinely confusable and only this
///   field tells them apart.
///
/// `None` on either is a NAMED absence — no load average on this platform, or
/// a daemon that did not answer — never a zero, which would read as an idle
/// host or a just-started one (ARCH §18.3).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Covariates {
    pub(super) load: Option<f64>,
    pub(super) daemon_uptime_secs: Option<u64>,
}

impl Covariates {
    /// Read both, now. ONE capture point, so a row's `before` and `after`
    /// cannot end up sampling different things (ARCH §10.6).
    pub(super) async fn capture(base: &str) -> Covariates {
        let c = Covariates {
            load: sovereign_cli_shared::host_load::load_average_1m(),
            daemon_uptime_secs: daemon_uptime_secs(base).await,
        };
        // Glassbox: the numbers a reader will later use to caveat a row are
        // visible while the run is happening, not only in the file after it
        // (ARCH §9.1). Either being `None` is itself the interesting event.
        tracing::debug!(
            load = ?c.load,
            daemon_uptime_secs = ?c.daemon_uptime_secs,
            "quality check: covariates"
        );
        c
    }
}

/// `process.uptime_seconds` off the daemon's `/status`, or `None`.
///
/// A short timeout and a swallowed error on purpose: this is a covariate, and
/// a covariate that can fail a run would be a gate wearing a different name.
/// The absence is recorded as `null`.
async fn daemon_uptime_secs(base: &str) -> Option<u64> {
    let resp = reqwest::Client::new()
        .get(format!("{base}/status"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("process")?.get("uptime_seconds")?.as_u64()
}

/// What happened to one instrument, beyond its verdict.
pub(super) struct InstrumentRun {
    pub(super) judgement: Judgement,
    pub(super) secs: u64,
    /// `None` when it never started.
    pub(super) exit_code: Option<i32>,
    /// The tail of what it printed, kept only for a red. A gate in this repo
    /// ends its output with its own fix command, so printing the tail of a
    /// failure is the whole of what the shell harnesses used to reconstruct
    /// by grepping for `✗|FAIL|^error` — a pattern no gate promised to keep.
    pub(super) tail: String,
    /// The conditions when this row STARTED, and when it finished.
    ///
    /// Two readings rather than one because a run is minutes long and a
    /// 1-minute average taken at the end does not describe the start: a row
    /// that began quiet and ended at load 30 is a different story from one
    /// that was busy throughout, and a single sample cannot tell them apart.
    /// The pair is also what makes a daemon restart visible — see
    /// [`Covariates`].
    pub(super) before: Covariates,
    pub(super) after: Covariates,
}

impl InstrumentRun {
    /// A row that never got as far as a process: out of budget, an unmet
    /// precondition, a log that could not be created, a spawn that failed.
    ///
    /// One constructor rather than five literals, so the load covariate
    /// cannot be recorded on four of them and forgotten on the fifth — the
    /// deliverable is "load on EVERY row, always" and a struct literal is
    /// how that becomes "on most rows" three commits later (ARCH §7).
    pub(super) fn did_not_start(judgement: Judgement, secs: u64, at: Covariates) -> InstrumentRun {
        InstrumentRun {
            judgement,
            secs,
            exit_code: None,
            tail: String::new(),
            before: at,
            after: at,
        }
    }
}

/// Resolve `argv[0]`. `svrn`/`sovereign` mean THIS dispatcher — never
/// whatever an operator's PATH happens to hold, which on this host is a
/// symlink into someone else's `target/debug`.
pub(super) fn resolve_program(program: &str) -> PathBuf {
    if matches!(program, "svrn" | "sovereign" | "sovereign-cli") {
        if let Ok(exe) = std::env::current_exe() {
            return exe;
        }
    }
    PathBuf::from(program)
}

/// The last `n` non-empty lines of a captured stream.
pub(super) fn tail_of(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// One instrument in flight.
pub(super) struct InFlight<'a> {
    // `pub(super)` rather than accessors: the scheduler in `mod.rs` owns WHEN
    // to poll and kill, this module owns HOW to spawn and read a verdict, and
    // a getter per field would be ceremony over a private struct in a private
    // module.
    pub(super) inst: &'a Instrument,
    pub(super) child: std::process::Child,
    pub(super) started: Instant,
    pub(super) cap_secs: u64,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    /// Read once, at spawn. Carried rather than re-read in [`finish`] so the
    /// pair on the summary row genuinely brackets the run.
    before: Covariates,
}

/// Spawn one instrument. `Err` is a judgement it earned by not starting.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_instrument<'a>(
    inst: &'a Instrument,
    argv: &[String],
    repo: &Path,
    out_dir: &Path,
    fingerprint: &Fingerprint,
    mint: bool,
    cap_secs: u64,
    before: Covariates,
) -> Result<InFlight<'a>, InstrumentRun> {
    let t0 = Instant::now();
    let stdout_path = out_dir.join(format!("lane-{}.out", inst.id));
    let stderr_path = out_dir.join(format!("lane-{}.err", inst.id));
    let (Ok(so), Ok(se)) = (
        std::fs::File::create(&stdout_path),
        std::fs::File::create(&stderr_path),
    ) else {
        return Err(InstrumentRun::did_not_start(
            Judgement::could_not_judge(
                inst.id.clone(),
                Reason::new(format!("cannot create the log under {}", out_dir.display()))
                    .expect("a path is never a placeholder"),
            ),
            0,
            before,
        ));
    };
    let program = resolve_program(&argv[0]);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&argv[1..])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::from(so))
        .stderr(Stdio::from(se))
        // The lane protocol, as environment. Env rather than appended flags
        // because a command is DATA and some instruments wrap a verb that
        // never heard of this runner.
        .env("SOVEREIGN_QUALITY_FINGERPRINT", &fingerprint.hex)
        .env("SOVEREIGN_QUALITY_OUT_DIR", out_dir)
        .env("SOVEREIGN_QUALITY_BUDGET_SECS", cap_secs.to_string())
        .env(
            "SOVEREIGN_QUALITY_BASELINE_DIR",
            baseline_dir(inst).map(|d| repo.join(d)).unwrap_or_default(),
        );
    if mint {
        cmd.env("SOVEREIGN_QUALITY_MINT", "1");
    }
    tracing::debug!(id = %inst.id, ?argv, cap_secs, "quality check: start");
    match cmd.spawn() {
        Ok(child) => Ok(InFlight {
            inst,
            child,
            started: t0,
            cap_secs,
            stdout_path,
            stderr_path,
            before,
        }),
        Err(e) => Err(InstrumentRun::did_not_start(
            Judgement::never_ran(
                inst.id.clone(),
                Reason::new(format!("cannot run `{}`: {e}", argv.join(" ")))
                    .expect("a command line is never a placeholder"),
            ),
            t0.elapsed().as_secs(),
            before,
        )),
    }
}

/// Read the verdict of a finished child.
///
/// TWO verdict sources, because this repo has two conventions and neither can
/// express the other. Every gate and script says pass/fail with an exit code;
/// the eight check lanes SAY a [`Judgement`] on their last stdout line,
/// because an exit code cannot express could-not-judge and `bench all` exits
/// 1 for regressed, stale AND missing-baseline.
pub(super) fn finish(
    f: InFlight<'_>,
    status: std::process::ExitStatus,
    after: Covariates,
) -> InstrumentRun {
    let secs = f.started.elapsed().as_secs();
    let code = status.code();
    let captured = match std::fs::read_to_string(&f.stdout_path) {
        Ok(c) => c,
        // An unreadable log is not silence. Defaulting to `""` here would
        // report "it printed nothing" about something that may have said
        // everything (ARCH §18.3).
        Err(e) => {
            return InstrumentRun {
                judgement: Judgement::could_not_judge(
                    f.inst.id.clone(),
                    Reason::new(format!("cannot read {}: {e}", f.stdout_path.display()))
                        .expect("a path is never a placeholder"),
                ),
                secs,
                exit_code: code,
                tail: String::new(),
                before: f.before,
                after,
            };
        }
    };
    let stderr = std::fs::read_to_string(&f.stderr_path).unwrap_or_default();
    tracing::debug!(id = %f.inst.id, ?code, secs, "quality check: exit");

    let judgement = match f.inst.verdict {
        VerdictSource::JudgementLine => match lane_verdict::from_stdout(&captured) {
            Ok(j) => {
                // It names its own subject; the TABLE is keyed on the id, so
                // one that answers about something else would put an
                // unrelated row in the operator's report.
                if j.subject() == f.inst.id {
                    j
                } else {
                    Judgement::could_not_judge(
                        f.inst.id.clone(),
                        Reason::new(format!(
                            "the verdict line names subject `{}`, not `{}`",
                            j.subject(),
                            f.inst.id
                        ))
                        .expect("never a placeholder"),
                    )
                }
            }
            Err(e) => Judgement::never_ran(
                f.inst.id.clone(),
                Reason::new(format!(
                    "{e} (exit {}) — see {}",
                    code.map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    f.stdout_path.display()
                ))
                .expect("never a placeholder"),
            ),
        },
        VerdictSource::ExitCode => match code {
            Some(0) => Judgement::passed(
                f.inst.id.clone(),
                Reason::new(format!("exit 0 in {secs}s")).expect("never a placeholder"),
            ),
            // Declared, never inferred — `concept-gate` exits 3 and 4 when
            // the graph cannot judge the commit in front of it, and a runner
            // guessing a range would get some other gate wrong.
            Some(c) if f.inst.could_not_judge_exits.contains(&c) => Judgement::could_not_judge(
                f.inst.id.clone(),
                Reason::new(format!(
                    "exit {c} — declared as could-not-judge; see {}",
                    f.stdout_path.display()
                ))
                .expect("never a placeholder"),
            ),
            Some(c) => Judgement::failed(
                f.inst.id.clone(),
                Reason::new(format!("exit {c} — see {}", f.stdout_path.display()))
                    .expect("never a placeholder"),
            ),
            // Killed by a signal. Not a failure it EARNED, and not a pass.
            None => Judgement::could_not_judge(
                f.inst.id.clone(),
                Reason::new(format!(
                    "killed by a signal — see {}",
                    f.stdout_path.display()
                ))
                .expect("never a placeholder"),
            ),
        },
    };
    // stdout first, then stderr: the ratchets print their findings and their
    // fix command to whichever one they chose, and the runner does not get to
    // decide which is the real output.
    let joined = format!("{captured}\n{stderr}");
    InstrumentRun {
        judgement: judgement.as_of(sovereign_core::time::system_now()),
        secs,
        exit_code: code,
        tail: tail_of(&joined, 12),
        before: f.before,
        after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `container:` is the spelling the shell harnesses had and the lane
    /// runner did not. It now has exactly one probe, like the other five.
    #[test]
    fn the_container_precondition_describes_the_toolbox_it_wants() {
        let d = describe_precondition(&Precondition::Container("sovereign-vulkan".into()));
        assert!(d.contains("sovereign-vulkan"), "{d}");
        assert!(d.contains("container"), "{d}");
    }

    /// `svrn` in a command means THIS dispatcher. A PATH lookup here would
    /// silently run whichever build an operator's symlink points at — on this
    /// host, one in a different checkout.
    #[test]
    fn svrn_in_a_command_resolves_to_the_running_dispatcher() {
        let me = std::env::current_exe().unwrap();
        for spelling in ["svrn", "sovereign", "sovereign-cli"] {
            assert_eq!(resolve_program(spelling), me, "{spelling}");
        }
        assert_eq!(resolve_program("python3"), PathBuf::from("python3"));
    }

    #[test]
    fn the_tail_is_the_last_lines_that_said_something() {
        assert_eq!(tail_of("a\n\nb\nc\n", 2), "b\nc");
        assert_eq!(tail_of("only\n", 12), "only");
        assert_eq!(tail_of("\n\n", 12), "");
    }
}
