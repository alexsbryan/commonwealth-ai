// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `process:v1` payload — what a submitter builds and a donor parses.
//! `ProcessExecutor`, which runs it, stays in `commonwealth_work::process`
//! behind the `process` feature and re-exports this at its historical path
//! (pb-work-doors).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kernel_types::quality::VerdictSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::act::{MAX_TTL_SECS, MIN_TTL_SECS};

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
/// non-zero code the UNIT chose is a real failure and stays one; a code a
/// container runtime relays for a signal death (`128 + n`) is not one the
/// unit chose, and `commonwealth_work::process`'s `relayed_signal` takes it out before this list is read.
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
    /// structure. Stdout that is not JSON is `commonwealth_work::executor::JobError::NoVerdict` — never a
    /// failure, because "the command printed something we cannot read" is not
    /// a statement about the command's subject.
    StdoutJson,
    /// Verdict from a [`Judgement`](kernel_types::Judgement) on the last non-empty stdout line, which
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
/// The whole of it is hashed into the unit's identity by `commonwealth_work::seal`, so
/// two units that differ in one environment variable are two units — which is
/// what makes the fold's idempotency-per-`unit_hash` mean what it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessPayload {
    /// The program and its arguments. `argv[0]` is resolved against `PATH` by
    /// the OS; there is no shell.
    pub argv: Vec<String>,

    /// A directory RELATIVE to `commonwealth_work::executor::JobContext::workdir`, or the workdir itself.
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
    /// `ProcessExecutor::validate` and `ProcessExecutor::execute`.
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
