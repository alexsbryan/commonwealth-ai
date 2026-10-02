// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ProcessExecutor`] — `process:v1`, an argv run as a child process.
//!
//! The simplest useful unit of work and the one the CI pilot rides on: a list
//! of arguments, a relative working directory, an environment, a wall cap, and
//! a rule for where the verdict is read from.
//!
//! # This is a SEVENTH spawn-with-timeout, and that is a named deviation
//!
//! ARCH §10.6 says one implementation per decider, and this workspace already
//! has several: `kill_on_drop` appears in sixteen first-party files, six of
//! them a spawn-wait-timeout-kill loop of exactly this shape. Reusing one is
//! not available here and the reason is the whole point of this crate:
//! **every one of them lives in a `sovereign-*` crate**, and
//! `commonwealth-work`'s manifest and its `[[forbid]]` row in
//! `quality/ARCH_LAYERS.toml` exist so that cw-lift 5f can lift this package
//! out of the monorepo and build it in a sandbox. A dependency on any of the
//! six would end that. So the body is COPIED, and this paragraph is the
//! deviation named rather than an oversight found later in review (§15: an
//! unnamed duplicate is the smell; a named one is a decision).
//!
//! **The two implementations copied, line-cited:**
//!
//! 1. `sovereign-agent-tools/src/executor.rs:1100 run_shell` — `current_dir`,
//!    stdin from a `Stdio`, `kill_on_drop(true)`, `process_group(0)`, the
//!    `kill -KILL -- -<pgid>` on timeout, and the 16 KiB tail cap. Its
//!    process-group comment records the failure that earned it (observed
//!    2026-05-23: pytest at 100% CPU for thirty minutes after the bench killed
//!    its parent `sh`, because `kill_on_drop` reaches the child and not the
//!    grandchild).
//! 2. `sovereign-tdd/src/shared/test_runner.rs:67-72` — the colour-normalizing
//!    environment block (`NO_COLOR`, `PYTHON_COLORS`, `CARGO_TERM_COLOR`, and
//!    the removal of `FORCE_COLOR`/`CLICOLOR_FORCE`), which exists because of
//!    the 2026-08-25 incident where a colourized test runner's `1 error in
//!    0.06s` arrived wrapped in ANSI escapes, matched no parser, and was
//!    reported as `0p/0f` — an absence rendered as a result (§18.3) — plus its
//!    `wait_with_output()` shape.
//!
//! A third RULE, not a body, is copied from
//! `sovereign-cli-shared/src/lane_verdict.rs:135 from_stdout`: a verdict line
//! is **the last non-empty line of stdout**, and only that one. See
//! [`verdict_from_stdout`].
//!
//! **What was changed, and why:**
//!
//! - **argv, not a shell string.** `run_shell` runs `sh -c <cmd>` so its
//!   operator-written commands can use pipes. A unit here arrives over a rail
//!   from another machine; `argv` means there is no shell grammar between what
//!   the submitter wrote and what runs. A unit that wants a pipe says so by
//!   naming `sh` as `argv[0]`, visibly.
//! - **`wait_with_output()` instead of `wait()` then read.** `run_shell` waits
//!   for the child and only then drains its pipes, which deadlocks if the
//!   child fills the 64 KiB pipe buffer before exiting. `test_runner`'s form
//!   drains concurrently and is the one copied.
//! - **Split stdout and stderr**, each tail-capped on its own. `run_shell`
//!   concatenates them, which is fine for a model reading a summary and wrong
//!   for [`ResultSource::VerdictLine`]: interleaved stderr could BECOME the
//!   last line and be adopted as a verdict.
//! - **`exit_code` and `duration_ms` in the result**, because a `Complete` act
//!   travels to a machine that did not run it and `status.success()` alone
//!   answers nothing there.
//! - **An env map and stdin from the payload.** The payload's `env` is applied
//!   FIRST and the colour block LAST, so a unit cannot re-introduce the
//!   `0p/0f` incident by setting `FORCE_COLOR` on itself.
//! - **Cancellation.** `run_shell` has none. Here a lost lease must stop the
//!   work, so [`JobContext::cancel_requested`] is polled and takes the same
//!   process-group kill the timeout does.
//!
//! # Isolation and trust, said out loud
//!
//! [`Isolation::Subprocess`], and the trust level is **trusted-native**. That
//! is stated in the descriptor itself (see [`ProcessExecutor::descriptor`]),
//! not only here, because a peer reads the descriptor and not this file.
//!
//! There is no sandbox. This repository contains no `bwrap`, no `firejail`, no
//! `nsjail` and no `--network=none` anywhere — cw-lift 5a's audit row is what
//! established that, and `sovereign-tools/src/compute.rs:8` describing a bare
//! `python3` spawn as "sandboxed" is exactly the prose-over-no-mechanism this
//! descriptor must not repeat. A `process:v1` unit runs with the donor's user,
//! the donor's filesystem and the donor's network. The mechanism that keeps
//! that safe is consent — `Submit.allowed` ∩ `Offer.accept_from` — and consent
//! is not isolation. `oci:v1` is named in the plan as the H2 that would change
//! this answer.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant, UNIX_EPOCH};

use kernel_types::quality::VerdictSource;
use kernel_types::{Judgement, Reason, Verdict};
use oicp_types::tool::{Idempotency, ToolExample};
use oicp_types::{Isolation, JobExecutorDescriptor, JobKind, JobUnit};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::act::{MAX_TTL_SECS, MIN_TTL_SECS};
use crate::executor::{ExecuteFuture, JobContext, JobError, JobExecutor};
use crate::refusal::WorkRefusal;

// The payload and its constants live in `oicp_types::work` since
// pb-work-doors; re-exported here at their historical path. The executor stays.
pub use oicp_types::work::process::{
    resolve_workdir_path, ProcessPayload, ResultSource, COULD_NOT_JUDGE_EXITS,
    DEFAULT_TIMEOUT_SECS, LEASE_INTERVAL_MS, PROCESS_KIND, TAIL_CAP_BYTES,
};

/// How often the cancellation flag is read while a unit runs. Fast enough that
/// a lost lease stops the work within a heartbeat, slow enough to be free.
const CANCEL_POLL: Duration = Duration::from_millis(100);

// -----------------------------------------------------------------
// The verdict line rule
// -----------------------------------------------------------------

/// Read a [`Judgement`] from the LAST NON-EMPTY LINE of stdout, and only that
/// one.
///
/// The rule — not the body — is `sovereign-cli-shared/src/lane_verdict.rs:135
/// from_stdout`, and its reason is copied with it: deliberately not "the last
/// line that happens to parse", because a lane that prints a JSON report and
/// then crashes before its verdict would otherwise have its report adopted as
/// a verdict, which is a green with nothing behind it. Trailing blank lines
/// are skipped because a `println!` at the end of a run is not a statement
/// about anything.
///
/// `sovereign-cli-shared` is a `sovereign-*` crate and therefore outside this
/// package's closure; see the module doc.
pub fn verdict_from_stdout(stdout: &str) -> Result<Judgement, String> {
    let last = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| {
            "the unit printed nothing on stdout, so there is no verdict line".to_string()
        })?;
    let value: Value = serde_json::from_str(last.trim())
        .map_err(|_| "the unit's last stdout line is not a JSON object".to_string())?;
    let obj = value
        .as_object()
        .ok_or_else(|| "the unit's last stdout line is not a JSON object".to_string())?;
    let field = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("the unit's verdict line has no `{k}` string"))
    };
    let subject = field("subject")?;
    let verdict_raw = field("verdict")?;
    let reason_raw = field("reason")?;
    let verdict = Verdict::parse_wire(&verdict_raw).ok_or_else(|| {
        format!(
            "the unit reported verdict `{verdict_raw}`, which is not one of \
             passed/failed/could-not-judge/never-ran"
        )
    })?;
    let reason = Reason::new(reason_raw.clone())
        .ok_or_else(|| format!("the unit's verdict reason is a placeholder: `{reason_raw}`"))?;
    let judgement = match verdict {
        Verdict::Passed => Judgement::passed(subject, reason),
        Verdict::Failed => Judgement::failed(subject, reason),
        Verdict::CouldNotJudge => Judgement::could_not_judge(subject, reason),
        Verdict::NeverRan => Judgement::never_ran(subject, reason),
    };
    Ok(match obj.get("as_of").and_then(Value::as_u64) {
        Some(secs) => judgement.as_of(UNIX_EPOCH + Duration::from_secs(secs)),
        None => judgement,
    })
}

/// Keep the last `limit` bytes, on a character boundary, saying what was cut.
///
/// Copied from `sovereign-agent-tools/src/executor.rs:1478 cap_tail`, walking
/// FORWARD to the next boundary so a multi-byte codepoint is never sliced in
/// half.
fn cap_tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut cut = s.len() - limit;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    format!("... (truncated {cut} leading bytes) ...\n{}", &s[cut..])
}

// -----------------------------------------------------------------
// The executor
// -----------------------------------------------------------------

/// What one child process did.
#[derive(Debug, Clone)]
struct RunOutcome {
    /// `None` when the process was killed by a signal and left no code at all.
    exit_code: Option<i32>,
    /// The signal that ended it, when one did — read off the wait status
    /// under [`Sandbox::Direct`], and decoded from the runtime's `128 + n`
    /// relay under [`Sandbox::Container`], where the child the donor waits on
    /// is the runtime client and the unit's own death arrives as a code.
    signal: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u128,
}

/// Runs a unit's `argv` inside the boundary this build has, in its own
/// process group.
pub struct ProcessExecutor {
    kind: JobKind,
    /// How a unit is actually run. [`Sandbox::Direct`] by default, because a
    /// default that assumed a container would be a claim on behalf of every
    /// host that constructs one — and `new()` is called from boot paths that
    /// have not probed anything.
    ///
    /// A donor builds this with [`ProcessExecutor::with_sandbox`] from its own
    /// [`Sandbox::probe`], and the SAME value answers what the build provides.
    /// One derivation, so a node cannot run units one way and describe itself
    /// another (ARCH §10.6).
    sandbox: crate::sandbox::Sandbox,
}

impl Default for ProcessExecutor {
    fn default() -> Self {
        ProcessExecutor::new()
    }
}

impl ProcessExecutor {
    // `JobKind`'s constructors are fallible because they exist to refuse what
    // arrives off a WIRE. `PROCESS_KIND` does not arrive off a wire: it is a
    // literal this crate owns, so the only edit that could reach this arm is
    // an edit to that literal — and `the_kind_literal_this_crate_owns_parses`
    // goes red before this ever runs. Returning a `Result` here instead would
    // put an unreachable error arm on every call site, including the boot
    // path a donor registers from.
    #[allow(clippy::expect_used)]
    pub fn new() -> ProcessExecutor {
        ProcessExecutor::with_sandbox(crate::sandbox::Sandbox::Direct)
    }

    /// The executor a donor registers, carrying the boundary it probed.
    #[allow(clippy::expect_used)]
    pub fn with_sandbox(sandbox: crate::sandbox::Sandbox) -> ProcessExecutor {
        ProcessExecutor {
            kind: JobKind::parse(PROCESS_KIND)
                .expect("`process:v1` is a valid JobKind by construction"),
            sandbox,
        }
    }

    /// What this executor's boundary PROVIDES — the value a donor compares
    /// against `descriptor().isolation`, which is what it REQUIRES. Two
    /// different questions that were one field until 2026-09-10.
    pub fn provides(&self) -> Isolation {
        self.sandbox.provides()
    }

    /// The kind this executor is registered under.
    pub fn kind(&self) -> &JobKind {
        &self.kind
    }

    async fn run(&self, unit: &JobUnit, ctx: &JobContext) -> Result<(Judgement, Value), JobError> {
        // The validate/writable split: the writable path RUNS the validator
        // rather than trusting that somebody upstream did.
        self.validate(unit)?;
        let payload = ProcessPayload::parse(&unit.payload)
            .map_err(|reason| JobError::NoVerdict { reason })?;

        let cwd = match &payload.cwd {
            Some(rel) => resolve_workdir_path(ctx.workdir(), rel)
                .map_err(|reason| JobError::NoVerdict { reason })?,
            None => ctx.workdir().to_path_buf(),
        };

        let outcome = self.spawn_and_wait(&payload, &cwd, ctx).await?;
        judge(unit, &payload, outcome)
    }

    /// Spawn, wait with a wall cap and a cancellation poll, and kill the whole
    /// process GROUP if either fires.
    async fn spawn_and_wait(
        &self,
        payload: &ProcessPayload,
        cwd: &Path,
        ctx: &JobContext,
    ) -> Result<RunOutcome, JobError> {
        // The unit's own environment FIRST, and the colour normalization
        // LAST, so a unit cannot set FORCE_COLOR on itself and re-run the
        // 2026-08-25 `0p/0f` incident. Built as a list rather than applied
        // straight onto the command because the container path has to pass
        // the same pairs, in the same order, as `--env` flags — one ordering,
        // two spawn shapes (ARCH §10.6).
        let mut env: Vec<(String, String)> = payload
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in [
            ("NO_COLOR", "1"),
            ("PYTHON_COLORS", "0"),
            ("CARGO_TERM_COLOR", "never"),
        ] {
            env.retain(|(have, _)| have != k);
            env.push((k.to_string(), v.to_string()));
        }

        // THE BOUNDARY. `Direct` hands back the unit's own argv unchanged, so
        // a build with no container behaves exactly as it did — and offers
        // nothing, because `resolve_offer` refuses the kind on what this
        // sandbox `provides`. A `Container` wraps it with the hardening in
        // `sandbox::Sandbox::command_line`, which is asserted there rather
        // than trusted to a reading of this function.
        let line = self.sandbox.command_line(
            &payload.argv,
            cwd,
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        );
        let program = line[0].clone();
        let mut command = Command::new(&program);
        command
            .args(&line[1..])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Applied to the child only on the direct path. Inside a container
        // the pairs already went in as `--env`, and setting them on the
        // RUNTIME's process would leak the unit's environment into podman
        // rather than into the unit.
        if matches!(self.sandbox, crate::sandbox::Sandbox::Direct) {
            for (k, v) in &env {
                command.env(k, v);
            }
        }
        command
            .env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE");

        match &payload.stdin {
            Some(_) => command.stdin(Stdio::piped()),
            None => command.stdin(Stdio::null()),
        };

        // Its OWN process group, so the whole tree can be killed. Without
        // this, `kill_on_drop` reaches the direct child and a grandchild is
        // reparented to init and keeps running — observed 2026-05-23 in
        // `run_shell`'s own comment, and the reason
        // `a_timeout_kills_the_grandchild_not_just_the_child` exists below.
        #[cfg(unix)]
        command.process_group(0);

        let started = Instant::now();
        let mut child = command.spawn().map_err(|e| JobError::Spawn {
            program: program.clone(),
            reason: e.to_string(),
        })?;

        // Captured BEFORE `wait_with_output` consumes the child — it is the
        // PGID we kill on timeout, and after the move it is gone.
        let pid = child.id();
        ctx.progress(&format!("started `{program}`"));

        let stdin_pipe = child.stdin.take();
        let stdin_data = payload.stdin.clone();
        let feed = async move {
            if let Some(mut w) = stdin_pipe {
                if let Some(data) = stdin_data {
                    let _ = w.write_all(data.as_bytes()).await;
                }
                // Close it: a child reading to EOF blocks forever otherwise.
                let _ = w.shutdown().await;
            }
        };

        let wall_cap = Duration::from_secs(payload.timeout_secs);
        let run = async {
            let (_, output) = tokio::join!(feed, child.wait_with_output());
            output
        };

        let stop = Stop::race(run, wall_cap, ctx).await;

        match stop {
            Stop::Finished(Ok(output)) => Ok(RunOutcome {
                exit_code: output.status.code(),
                signal: signal_of(&output.status, &self.sandbox),
                stdout: cap_tail(&String::from_utf8_lossy(&output.stdout), TAIL_CAP_BYTES),
                stderr: cap_tail(&String::from_utf8_lossy(&output.stderr), TAIL_CAP_BYTES),
                duration_ms: started.elapsed().as_millis(),
            }),
            Stop::Finished(Err(e)) => Err(JobError::NoVerdict {
                reason: format!("waiting on `{program}` failed: {e}"),
            }),
            Stop::TimedOut => {
                kill_group(pid, "timeout", wall_cap.as_secs());
                Err(JobError::Timeout {
                    secs: wall_cap.as_secs(),
                })
            }
            Stop::Cancelled => {
                kill_group(pid, "cancelled", started.elapsed().as_secs());
                Err(JobError::Cancelled)
            }
        }
    }
}

/// How the wait ended. A three-armed enum rather than nested `Result`s because
/// the two abort arms take the same teardown and a reader should see that.
enum Stop<T> {
    Finished(T),
    TimedOut,
    Cancelled,
}

impl<T> Stop<T> {
    /// Race the run against the wall cap and the cancellation flag.
    async fn race<F: std::future::Future<Output = T>>(
        run: F,
        wall_cap: Duration,
        ctx: &JobContext,
    ) -> Stop<T> {
        tokio::select! {
            // Biased so a process that finished in the same tick the cap
            // expired is reported as finished, not as a timeout. Without it
            // `select!` picks at random and the test for the boundary is
            // flaky by construction.
            biased;
            out = run => Stop::Finished(out),
            _ = watch_cancel(ctx) => Stop::Cancelled,
            _ = tokio::time::sleep(wall_cap) => Stop::TimedOut,
        }
    }
}

/// Resolves only when somebody has asked the unit to stop. Polls rather than
/// waits on a channel because [`JobContext`] carries an `AtomicBool` — see its
/// doc for why that shape and not a runtime's token type.
async fn watch_cancel(ctx: &JobContext) {
    loop {
        if ctx.cancel_requested() {
            return;
        }
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

/// SIGKILL the whole process group.
///
/// Copied from `run_shell`'s timeout arm. `kill_on_drop` SIGKILLs the direct
/// child as the `Child` handle drops; grandchildren the child spawned would be
/// reparented to init and keep running, so the group gets its own `kill -KILL
/// -- -<pgid>`. Best-effort: the child may already have exited and been
/// reaped, in which case the PGID names nothing and `kill` returns ESRCH.
#[cfg_attr(not(unix), allow(unused_variables))]
fn kill_group(pid: Option<u32>, why: &str, after_secs: u64) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let pgid_arg = format!("-{pid}");
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &pgid_arg])
            .status();
        tracing::warn!(
            target: crate::TRACE_TARGET,
            pgid = pid,
            why,
            after_secs,
            "process:v1 unit stopped — killed the process group"
        );
    }
}

/// Turn what the process did into a verdict, or into the named absence of one.
///
/// **The `Complete`/`Fail` boundary in one function.** Everything that returns
/// `Ok` is a `Complete`; everything that returns `Err` is a `Fail`. A non-zero
/// exit is an `Ok` carrying `Verdict::Failed` — the unit ran and the answer is
/// no.
fn judge(
    unit: &JobUnit,
    payload: &ProcessPayload,
    outcome: RunOutcome,
) -> Result<(Judgement, Value), JobError> {
    let subject = crate::executor::subject_of(unit);

    // Ended by a signal we did not send — `podman stop`, an OOM kill, an
    // operator's ^C on the donor. It ran and we cannot tell what it decided,
    // and that holds whether the signal reached us as no code (a direct
    // child) or as the runtime's `128 + n` (a container). Watched 2026-09-11:
    // a `dst-scenarios` unit stopped from outside landed as `failed — the
    // unit exited 137`, a red on code that never reached a verdict (§18.3).
    if let Some(sig) = outcome.signal {
        return Err(JobError::NoVerdict {
            reason: format!(
                "`{}` was killed by signal {sig} ({}) after {}ms{} — nothing it was asked to \
                 judge reached a verdict",
                payload.argv[0],
                signal_name(sig),
                outcome.duration_ms,
                match outcome.exit_code {
                    Some(code) => format!(", relayed by the runtime as exit {code}"),
                    None => String::new(),
                }
            ),
        });
    }

    // No exit code at all and no signal either: the wait status is one this
    // platform cannot name. It ran and we cannot tell what it decided.
    let Some(code) = outcome.exit_code else {
        return Err(JobError::NoVerdict {
            reason: format!(
                "`{}` was killed by a signal and left no exit code after {}ms",
                payload.argv[0], outcome.duration_ms
            ),
        });
    };

    // The DECLARED could-not-judge codes, before any verdict is derived: exit
    // 4 (zero tests resolved) and exit 5 (results not attributable) are
    // non-zero and are not failures.
    if COULD_NOT_JUDGE_EXITS.contains(&code) {
        return Err(JobError::DeclaredCouldNotJudge {
            exit_code: code,
            tail: last_line_or(&outcome.stderr, &outcome.stdout),
        });
    }

    let envelope = |extra: Value| -> Value {
        let mut base = json!({
            "exit_code": code,
            "duration_ms": outcome.duration_ms as u64,
        });
        if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_obj {
                base_obj.insert(k.clone(), v.clone());
            }
        }
        base
    };

    match payload.result {
        ResultSource::VerdictLine => {
            // The one arm whose verdict is NOT the exit code. A unit that
            // exits 0 and prints a could-not-judge line is could-not-judge,
            // which is the whole reason the lane protocol exists.
            let judgement =
                verdict_from_stdout(&outcome.stdout).map_err(|reason| JobError::NoVerdict {
                    reason: format!("{reason} (exit {code})"),
                })?;
            let result = envelope(json!({
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
            }));
            Ok((judgement, result))
        }
        ResultSource::StdoutJson => {
            let parsed: Value =
                serde_json::from_str(outcome.stdout.trim()).map_err(|e| JobError::NoVerdict {
                    reason: format!(
                        "the unit asked for `stdout-json` and its stdout is not JSON: {e}"
                    ),
                })?;
            let result = envelope(json!({
                "json": parsed,
                "stderr": outcome.stderr,
            }));
            Ok((exit_code_judgement(subject, code, &outcome), result))
        }
        ResultSource::Stdout => {
            let result = envelope(json!({
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
            }));
            Ok((exit_code_judgement(subject, code, &outcome), result))
        }
        ResultSource::ExitCodeOnly => Ok((
            exit_code_judgement(subject, code, &outcome),
            envelope(json!({})),
        )),
    }
}

/// The signal that ended a child, if one did.
///
/// Under [`Sandbox::Direct`] the wait status says so itself. Under
/// [`Sandbox::Container`] the child is the runtime client, which exits 0..=255
/// no matter how the unit died and relays a signal death as `128 + n` — the
/// shell convention podman and docker both follow. A direct child that exits
/// 137 on purpose keeps its 137: the relay is only decoded where a runtime
/// stood between us and the unit.
fn signal_of(status: &std::process::ExitStatus, sandbox: &crate::sandbox::Sandbox) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Some(sig);
        }
    }
    match sandbox {
        crate::sandbox::Sandbox::Direct => None,
        crate::sandbox::Sandbox::Container { .. } => status.code().and_then(relayed_signal),
    }
}

/// `128 + n` for the 31 classic signals, and nothing else: a runtime's own
/// exits (125 cannot run, 126 not executable, 127 not found) and every code a
/// unit can choose below 128 pass through untouched.
fn relayed_signal(code: i32) -> Option<i32> {
    (129..=159).contains(&code).then(|| code - 128)
}

/// The name a reader greps for. Only the ones a stopped unit actually dies
/// of; the rest are reported by number, which is still a name.
fn signal_name(sig: i32) -> &'static str {
    match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        15 => "SIGTERM",
        _ => "unnamed",
    }
}

/// Exit 0 is passed, anything else is failed — after the signal deaths and
/// the declared could-not-judge codes have already been taken out above. One
/// place, so the three exit-code arms cannot drift apart.
fn exit_code_judgement(subject: String, code: i32, outcome: &RunOutcome) -> Judgement {
    let ms = outcome.duration_ms;
    if code == 0 {
        Judgement::passed(
            subject,
            reason_or_bug(format!("the unit exited 0 after {ms}ms")),
        )
    } else {
        Judgement::failed(
            subject,
            reason_or_bug(format!(
                "the unit exited {code} after {ms}ms — {}",
                last_line_or(&outcome.stderr, &outcome.stdout)
            )),
        )
    }
}

/// `Reason::new` refuses placeholder text. Every string above is a sentence,
/// so the fallback is unreachable today — it is written as a NAMED
/// substitution rather than an `expect`, because a panic on the reporting path
/// would lose the verdict entirely (ARCH §18.3).
fn reason_or_bug(text: String) -> Reason {
    Reason::new(text).unwrap_or_else(|| {
        Reason::literal(
            "the executor rendered a placeholder where a reason belongs — that is a bug in \
             commonwealth-work, not in the unit",
        )
    })
}

/// The most useful single line to put in a reason: the tail of stderr if there
/// is one, else the tail of stdout, else a statement that both were empty —
/// never an empty string, which reads as "no reason given".
fn last_line_or(stderr: &str, stdout: &str) -> String {
    for stream in [stderr, stdout] {
        if let Some(line) = stream.lines().rev().find(|l| !l.trim().is_empty()) {
            return line.trim().to_string();
        }
    }
    "it printed nothing on stdout or stderr".to_string()
}

impl JobExecutor for ProcessExecutor {
    fn descriptor(&self) -> JobExecutorDescriptor {
        JobExecutorDescriptor {
            kind: self.kind.clone(),
            // WHAT THIS EXECUTOR REQUIRES, not what it provides. `resolve_offer`
            // reads this field as the demand and refuses to publish an offer the
            // build cannot meet (`work_donor.rs:265-273`). The two readings
            // coincided while both sides said `Subprocess`, and the comment that
            // stood here read it the other way round — which is how a floor came
            // to be written as a capability.
            //
            // Running a submitter's arbitrary argv REQUIRES a container. That is
            // the operator's posture as of 2026-09-10: isolation is the default,
            // and there is no arbitrary execution outside a well-defined
            // boundary. `DONOR_ISOLATION` stays `Subprocess` because that is what
            // the build actually provides, so a daemon now refuses this kind at
            // boot BY NAME instead of publishing an offer whose only wall is
            // consent — and consent is not isolation (module doc, and
            // `work_donor.rs:40-43`). This arm stays red until a container-backed
            // executor raises what the build provides, which is a mechanism and
            // not a config key (ARCH §18.3).
            isolation: Isolation::RootlessContainer,
            parameters: json!({
                "type": "object",
                "title": "process:v1",
                "description":
                    "Runs `argv` as a child process, in its own process group, killed as a \
                     group on timeout. REQUIRES `rootless-container` isolation, and no build \
                     in this repository provides it yet — no bwrap, no firejail, no nsjail, \
                     no network namespace — so a daemon REFUSES to offer this kind at boot \
                     and names the shortfall. That refusal is the shipped state and it is \
                     deliberate: unsandboxed, a unit runs with the donor's user, filesystem \
                     and network, which reaches the donor's own mesh private key. What would \
                     otherwise limit it is consent (`Submit.allowed` intersected with \
                     `Offer.accept_from`), and consent is not isolation. A container-backed \
                     executor is what lifts this, not a config key.",
                "required": ["argv", "timeout_secs", "result"],
                "additionalProperties": false,
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "Program and arguments. No shell: name `sh` explicitly to get one."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Directory relative to the donor's workdir. Absolute paths and `..` are refused."
                    },
                    "stdin": {
                        "type": "string",
                        "description": "Fed to the child and then closed. Absent means /dev/null."
                    },
                    "env": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Added to the donor's environment. Colour-forcing variables are overridden afterwards and cannot be set from here."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": MIN_TTL_SECS,
                        "maximum": MAX_TTL_SECS,
                        "description": "Wall cap. On expiry the whole process group is SIGKILLed and the unit is could-not-judge, never failed."
                    },
                    "result": {
                        "enum": ["stdout", "stdout-json", "verdict-line", "exit-code-only"],
                        "description": "Where the verdict is read from. Three take it from the exit code; `verdict-line` takes it from a Judgement on the last non-empty stdout line."
                    }
                }
            }),
            examples: vec![
                ToolExample {
                    situation:
                        "A CI shard: run one crate's tests and take the verdict from the exit code"
                            .to_string(),
                    call: json!({
                        "argv": ["./scripts/sovereign-test.sh", "--human", "--package", "commonwealth-work"],
                        "timeout_secs": 900,
                        "result": "exit-code-only"
                    }),
                },
                ToolExample {
                    situation: "A lane that judges itself and prints a Judgement as its last line"
                        .to_string(),
                    call: json!({
                        "argv": ["sh", "-c", "svrn quality check --lane retrieval-prod"],
                        "timeout_secs": 1800,
                        "result": "verdict-line"
                    }),
                },
            ],
            // The executor runs an argv it did not write and cannot know
            // whether running it twice duplicates an effect. Declaring
            // `Idempotent` would be a guarantee about somebody else's command
            // (ARCH §7.6 — never claim what code cannot enforce), and the
            // retry gate reads this field, so the honest answer is the safe
            // one: a lapsed lease does not silently re-run a `process:v1`
            // unit. A kind whose units ARE re-runnable says so by being its
            // own kind with its own executor.
            idempotency: Idempotency::NonIdempotent,
            lease_interval_ms: LEASE_INTERVAL_MS,
            // Nobody has timed a generic argv, and there is nothing to time.
            // `None` is that absence reported rather than defaulted to zero
            // (ARCH §18.3).
            est_secs: None,
            could_not_judge_exits: COULD_NOT_JUDGE_EXITS.to_vec(),
            // ONE value on a per-executor descriptor, and `process:v1` speaks
            // two: three of the four `result` arms read the exit code and
            // `verdict-line` reads stdout. `ExitCode` is declared because it
            // is what applies unless the payload says otherwise, and the
            // `result` property in `parameters` above is where the other is
            // named — said, not hidden.
            verdict: VerdictSource::ExitCode,
        }
    }

    fn validate(&self, unit: &JobUnit) -> Result<(), WorkRefusal> {
        if unit.kind != self.kind {
            let refusal = if unit.kind.is_skew_of(&self.kind) {
                WorkRefusal::VersionSkew {
                    wanted: unit.kind.clone(),
                    offered: self.kind.clone(),
                }
            } else {
                WorkRefusal::KindNotOffered {
                    kind: unit.kind.clone(),
                }
            };
            tracing::debug!(
                target: crate::TRACE_TARGET,
                unit_kind = %unit.kind,
                executor_kind = %self.kind,
                "process:v1 executor refused a unit of another kind"
            );
            return Err(refusal);
        }
        if let Err(reason) = ProcessPayload::parse(&unit.payload) {
            // The closed refusal vocabulary has one arm for "this payload is
            // not one this executor can run"; the specific rule is traced so a
            // reader is not left with only the variant name.
            tracing::debug!(
                target: crate::TRACE_TARGET,
                unit_hash = %unit.unit_hash,
                reason,
                "process:v1 payload refused"
            );
            return Err(WorkRefusal::PayloadNotCanonical { detail: reason });
        }
        Ok(())
    }

    /// The IMAGE's os, arch and compiler when this executor has a boundary —
    /// see the trait method. A `Direct` executor falls through to this host,
    /// which `of_sandbox` decides rather than a second `matches!` here.
    fn attribution(&self, repo_rev: &str) -> kernel_types::ComputeAttribution {
        crate::attribution::of_sandbox(repo_rev, &self.sandbox)
    }

    fn environment_satisfies(&self, unit: &JobUnit) -> Result<(), WorkRefusal> {
        // The sandbox is the environment the argv runs in; ask it, not the
        // host. Under `Sandbox::Direct` this IS the host.
        crate::refusal::environment_satisfies(unit, &self.sandbox)
    }

    fn execute<'a>(&'a self, unit: &'a JobUnit, ctx: &'a JobContext) -> ExecuteFuture<'a> {
        Box::pin(self.run(unit, ctx))
    }
}

#[cfg(test)]
mod tests;
