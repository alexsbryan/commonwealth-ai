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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, UNIX_EPOCH};

use kernel_types::quality::VerdictSource;
use kernel_types::{Judgement, Reason, Verdict};
use oicp_types::tool::{Idempotency, ToolExample};
use oicp_types::{Isolation, JobExecutorDescriptor, JobKind, JobUnit};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::act::{MAX_TTL_SECS, MIN_TTL_SECS};
use crate::executor::{ExecuteFuture, JobContext, JobError, JobExecutor};
use crate::refusal::WorkRefusal;

/// The kind this executor is registered for, in its one wire spelling.
pub const PROCESS_KIND: &str = "process:v1";

/// How much of each stream survives into the result. Copied from
/// `run_shell`'s `16 * 1024`, and for the same reason: a `Complete` act rides
/// a rail with a 64 KiB payload ceiling, so an uncapped cargo log would not be
/// a big result — it would be a refused one.
pub const TAIL_CAP_BYTES: usize = 16 * 1024;

/// How often the lessee must renew while a unit runs.
///
/// Fifteen seconds against `commonwealth_core::knowledge::LEASE_MS` — several
/// heartbeats inside one lease window, so a single missed renew is not a lost
/// lease. It is a field on the descriptor rather than a constant a donor
/// remembers, which is what lets a slower executor ask for a longer interval
/// without a second policy anywhere.
pub const LEASE_INTERVAL_MS: u64 = 15_000;

/// How often the cancellation flag is read while a unit runs. Fast enough that
/// a lost lease stops the work within a heartbeat, slow enough to be free.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// The wall cap a submitter who did not name one gets.
///
/// Fifteen minutes: about four times the cold full-workspace run this
/// repository's own test gate takes (~3m30s), so the pilot's shard cannot be
/// cut by the default, and two orders under [`MAX_TTL_SECS`] so it is a
/// convenience rather than a ceiling. It lives here, beside the field it
/// fills, because a submitter that picked its own would be a second answer to
/// "how long may a unit run" (ARCH §10.6) — and `svrn job submit` prints the
/// value it used rather than leaving it to be remembered.
pub const DEFAULT_TIMEOUT_SECS: u64 = 900;

/// Exit codes this executor DECLARES mean could-not-judge rather than failed.
///
/// Not a range and not a guess: **4** and **5** are the two
/// `scripts/sovereign-test.sh` already defines — a run that resolved zero
/// tests, and a run whose results could not be attributed to it. Both exit
/// non-zero and neither is a statement about the code under test. Any other
/// non-zero code is a real failure and stays one.
pub const COULD_NOT_JUDGE_EXITS: [i32; 2] = [4, 5];

// -----------------------------------------------------------------
// The payload
// -----------------------------------------------------------------

/// Where the runner reads a `process:v1` unit's answer from.
///
/// Closed (ARCH §2.1). Three arms take the verdict from the exit code and
/// differ only in what they carry back; the fourth takes it from stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResultSource {
    /// Verdict from the exit code; stdout and stderr carried back as text.
    Stdout,
    /// Verdict from the exit code; stdout parsed as JSON and carried back as
    /// structure. Stdout that is not JSON is [`JobError::NoVerdict`] — never a
    /// failure, because "the command printed something we cannot read" is not
    /// a statement about the command's subject.
    StdoutJson,
    /// Verdict from a [`Judgement`] on the last non-empty stdout line, which
    /// is the lane protocol (`lane_verdict::from_stdout`). The one arm that
    /// can report `CouldNotJudge` from a process that exited 0, which is
    /// precisely what an exit code cannot express.
    VerdictLine,
    /// Verdict from the exit code, and nothing carried back but the code and
    /// the duration. For a unit whose output is large and whose answer is
    /// binary.
    ExitCodeOnly,
}

impl ResultSource {
    /// Which `kernel-types` verdict source this arm speaks.
    pub fn verdict_source(self) -> VerdictSource {
        match self {
            ResultSource::VerdictLine => VerdictSource::JudgementLine,
            ResultSource::Stdout | ResultSource::StdoutJson | ResultSource::ExitCodeOnly => {
                VerdictSource::ExitCode
            }
        }
    }
}

/// A `process:v1` unit's payload.
///
/// The whole of it is hashed into the unit's identity by [`crate::seal`], so
/// two units that differ in one environment variable are two units — which is
/// what makes the fold's idempotency-per-`unit_hash` mean what it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessPayload {
    /// The program and its arguments. `argv[0]` is resolved against `PATH` by
    /// the OS; there is no shell.
    pub argv: Vec<String>,

    /// A directory RELATIVE to [`JobContext::workdir`], or the workdir itself.
    ///
    /// The plan calls this a `RelPath` and it is spelled as a `String` whose
    /// one validator is [`resolve_workdir_path`] — an absolute path and a `..`
    /// component are both refused. A newtype would only be an improvement if
    /// it OWNED that rule, and this module is behind a feature flag, so the
    /// rule would then be unreachable from a build that never compiles it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Fed to the child on stdin and then closed. `None` is `/dev/null`, which
    /// is `run_shell`'s behaviour and the reason a unit never blocks waiting
    /// for input nobody will type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,

    /// Extra environment for the child, ON TOP of the donor's own. A
    /// `BTreeMap` because the payload is canonicalized by `Payload::new`
    /// (sorted keys) and a map that iterates in insertion order would
    /// serialize to different bytes for the same content — two identities for
    /// one unit.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// The wall cap, in seconds. Bounded by the same `MIN_TTL_SECS` /
    /// `MAX_TTL_SECS` a submission's TTL is (no new constants): a unit that
    /// outlives every lease it could hold is a unit nobody can complete.
    pub timeout_secs: u64,

    /// Where the verdict comes from.
    pub result: ResultSource,
}

impl ProcessPayload {
    /// The payload for "run this argv", with every other field at the value a
    /// submitter who said nothing means.
    ///
    /// This exists so that a caller holding only an argv does not spell the
    /// payload as a JSON literal. `svrn job submit -- uname -a` did exactly
    /// that and shipped `{"argv": […]}` — no `timeout_secs`, no `result` —
    /// which every donor then refused as `payload-not-canonical`, five
    /// seconds at a time, invisibly to the submitter. Two spellings of one
    /// shape, and the second one could not be kept right by anything (ARCH
    /// §10.6); this is the one, and adding a required field here is a compile
    /// error at every caller rather than a refusal at every donor.
    ///
    /// [`ResultSource::Stdout`] because a shorthand unit is a command whose
    /// answer is its exit code and whose output the submitter wants to read.
    pub fn command(argv: Vec<String>) -> ProcessPayload {
        ProcessPayload {
            argv,
            cwd: None,
            stdin: None,
            env: BTreeMap::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            result: ResultSource::Stdout,
        }
    }

    /// Read a payload out of a unit, or say which rule it broke.
    ///
    /// The refusal text is what a submitter sees; the `Err` is a sentence, not
    /// a code.
    pub fn parse(payload: &Value) -> Result<ProcessPayload, String> {
        let parsed: ProcessPayload = serde_json::from_value(payload.clone())
            .map_err(|e| format!("not a `process:v1` payload: {e}"))?;
        parsed.check()?;
        Ok(parsed)
    }

    /// The rules that serde cannot express. One place, run by both
    /// [`ProcessExecutor::validate`] and [`ProcessExecutor::execute`].
    fn check(&self) -> Result<(), String> {
        if self.argv.is_empty() {
            return Err("`argv` is empty — there is no program to run".to_string());
        }
        if self.argv[0].trim().is_empty() {
            return Err("`argv[0]` is blank — the program to run has no name".to_string());
        }
        if self.timeout_secs < MIN_TTL_SECS {
            return Err(format!(
                "`timeout_secs` is {} — a unit with no time to run is a unit that can only \
                 ever be killed",
                self.timeout_secs
            ));
        }
        if self.timeout_secs > MAX_TTL_SECS {
            return Err(format!(
                "`timeout_secs` is {}, past the {MAX_TTL_SECS}s ceiling a submission's TTL \
                 also carries — a unit cannot outlive every lease it could hold",
                self.timeout_secs
            ));
        }
        if let Some(cwd) = &self.cwd {
            // Validate against a placeholder root: the rule being checked is
            // about the RELATIVE path's shape, and it is the same rule
            // `execute` runs against the real workdir.
            resolve_workdir_path(Path::new("/"), cwd)?;
        }
        Ok(())
    }
}

/// Join a relative path onto a workdir, refusing anything that escapes it.
///
/// Copied from `sovereign-agent-tools/src/executor.rs:1071
/// resolve_workdir_path`: absolute paths refused, `..` components refused, and
/// deliberately NOT canonicalized — the check has to hold whether or not the
/// path exists yet, and `canonicalize` on a missing path fails for the wrong
/// reason.
pub fn resolve_workdir_path(workdir: &Path, rel: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(format!(
            "`cwd` is absolute (`{rel}`) — a unit names a directory relative to the donor's \
             workdir, because it does not know the donor's filesystem"
        ));
    }
    for comp in candidate.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(format!(
                "`cwd` climbs out of the workdir with `..` (`{rel}`) — refused"
            ));
        }
    }
    Ok(workdir.join(candidate))
}

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
    stdout: String,
    stderr: String,
    duration_ms: u128,
}

/// Runs a unit's `argv` as a child process in its own process group.
pub struct ProcessExecutor {
    kind: JobKind,
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
        ProcessExecutor {
            kind: JobKind::parse(PROCESS_KIND)
                .expect("`process:v1` is a valid JobKind by construction"),
        }
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
        let program = payload.argv[0].clone();
        let mut command = Command::new(&program);
        command
            .args(&payload.argv[1..])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // The unit's own environment FIRST …
        for (k, v) in &payload.env {
            command.env(k, v);
        }
        // … and the colour normalization LAST, so a unit cannot set
        // FORCE_COLOR on itself and re-run the 2026-08-25 `0p/0f` incident.
        // Copied verbatim from test_runner.rs:67-72.
        command
            .env("NO_COLOR", "1")
            .env("PYTHON_COLORS", "0")
            .env("CARGO_TERM_COLOR", "never")
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

    // No exit code at all: killed by a signal we did not send. It ran and we
    // cannot tell what it decided.
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

/// Exit 0 is passed, anything else is failed — after the declared
/// could-not-judge codes have already been taken out above. One place, so the
/// three exit-code arms cannot drift apart.
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
            // Subprocess, and no more. See the module doc: there is no sandbox
            // mechanism in this repository, so claiming RootlessContainer here
            // would be prose over nothing.
            isolation: Isolation::Subprocess,
            parameters: json!({
                "type": "object",
                "title": "process:v1",
                "description":
                    "Runs `argv` as a child process on the donor's own machine, in its own \
                     process group, killed as a group on timeout. Isolation is `subprocess` \
                     and trust is TRUSTED-NATIVE: there is no sandbox — no bwrap, no \
                     firejail, no nsjail, no network namespace — so a unit runs with the \
                     donor's user, filesystem and network. What limits it is consent \
                     (`Submit.allowed` intersected with `Offer.accept_from`), and consent is \
                     not isolation. Offer this kind only to actors you would hand a shell.",
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

    fn execute<'a>(&'a self, unit: &'a JobUnit, ctx: &'a JobContext) -> ExecuteFuture<'a> {
        Box::pin(self.run(unit, ctx))
    }
}

#[cfg(test)]
mod tests;
