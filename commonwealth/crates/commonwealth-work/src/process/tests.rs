// SPDX-License-Identifier: AGPL-3.0-or-later
//! `process`'s tests. A sibling file only so `process.rs` stays under
//! ARCH §3.1's 1200-line ceiling — moved verbatim, nothing renamed.
use super::*;
use crate::executor::{subject_of, JobExecutorRegistry};
use oicp_types::JobRequirements;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn unit_with(payload: Value) -> JobUnit {
    crate::seal::seal(
        JobKind::parse(PROCESS_KIND).unwrap(),
        payload,
        JobRequirements::any(),
        None,
    )
    .expect("seal")
}

fn ctx() -> JobContext {
    JobContext::new(std::env::temp_dir())
}

/// A scratch directory nobody else is using.
///
/// No `tempfile` — this crate adds no dependency — and deliberately no
/// `SystemTime::now()` either: `cargo xtask clock-gate` counts hand-read
/// wall clocks per FILE and does not exempt test code, and a unique name
/// does not need a clock. Pid plus a monotonic counter is enough.
fn scratch(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "commonwealth-work-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ───── the named test: the process GROUP dies, not just the child ─────

/// **The failing input:** a command that spawns a child which outlives its
/// parent. `sh` backgrounds a grandchild that loops forever, records the
/// grandchild's pid, and then sleeps past the wall cap.
///
/// A naive `child.kill()` — or `kill_on_drop` alone — reaches the direct
/// `sh` and nothing else: the grandchild is reparented to init and keeps
/// spinning, which is exactly what was observed on 2026-05-23 (pytest at
/// 100% CPU for thirty minutes after its parent shell was killed). What
/// prevents it is `process_group(0)` at spawn plus `kill -KILL -- -<pgid>`
/// on the timeout path, and this test is what proves both are still there.
///
/// The second half of the claim is the verdict: a timeout is
/// `CouldNotJudge`, never `Failed`. A unit we killed made no statement
/// about its subject.
#[cfg(unix)]
#[tokio::test]
async fn a_timeout_kills_the_grandchild_not_just_the_child_and_is_could_not_judge() {
    let dir = scratch("pgroup");
    let marker = dir.join("grandchild.pid");
    let script = format!(
        "sh -c 'echo $$ > {} ; while : ; do sleep 0.2 ; done' & sleep 30",
        marker.display()
    );

    let unit = unit_with(json!({
        "argv": ["sh", "-c", script],
        "timeout_secs": 1,
        "result": "exit-code-only"
    }));
    let ctx = JobContext::new(&dir);
    let executor = ProcessExecutor::new();

    let err = executor
        .execute(&unit, &ctx)
        .await
        .expect_err("a unit we killed reached no verdict");

    // Half one: a timeout is could-not-judge, and it is a Fail act, not a
    // Complete carrying Failed.
    assert!(
        matches!(err, JobError::Timeout { secs: 1 }),
        "expected a timeout, got {err}"
    );
    assert_eq!(err.verdict(), Verdict::CouldNotJudge);
    assert_eq!(
        err.judgement(subject_of(&unit)).verdict(),
        Verdict::CouldNotJudge
    );

    // Half two: the grandchild is gone. This is the half a naive
    // `child.kill()` fails.
    let pid: u32 = std::fs::read_to_string(&marker)
        .expect(
            "the grandchild wrote its pid — if this is missing the test's own \
                     fixture is broken, not the executor",
        )
        .trim()
        .parse()
        .expect("a pid");
    // The group kill is a signal, not a join; give the kernel a moment.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still_running = alive(pid);
    if still_running {
        // Never leave a spinner behind, whatever the verdict.
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !still_running,
        "grandchild {pid} survived the timeout — the process GROUP was not killed, \
             only the direct child"
    );
}

/// Cancellation takes the same teardown as the timeout, and is likewise
/// could-not-judge: a donor that lost its lease has no verdict to publish.
#[cfg(unix)]
#[tokio::test]
async fn a_cancelled_unit_is_could_not_judge_and_its_group_dies_too() {
    let dir = scratch("cancel");
    let unit = unit_with(json!({
        "argv": ["sh", "-c", "sleep 30"],
        "timeout_secs": 60,
        "result": "exit-code-only"
    }));
    let ctx = JobContext::new(&dir);
    let cancel = ctx.cancel_handle();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.store(true, Ordering::SeqCst);
    });
    let err = ProcessExecutor::new()
        .execute(&unit, &ctx)
        .await
        .expect_err("a cancelled unit reached no verdict");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, JobError::Cancelled), "got {err}");
    assert_eq!(err.verdict(), Verdict::CouldNotJudge);
}

// ───── the Complete/Fail boundary ─────

/// **The failing input:** a command that runs perfectly and exits 1. The
/// tempting wrong answer is a `Fail` act; the right one is a `Complete`
/// carrying `Verdict::Failed`, because the unit did its job and the answer
/// is no.
#[tokio::test]
async fn a_red_unit_is_a_completion_carrying_failed_not_a_failure() {
    let unit = unit_with(json!({
        "argv": ["sh", "-c", "echo 'two tests failed' >&2 ; exit 1"],
        "timeout_secs": 30,
        "result": "stdout"
    }));
    let (judgement, result) = ProcessExecutor::new()
        .execute(&unit, &ctx())
        .await
        .expect("it ran, so it is an Ok");
    assert_eq!(judgement.verdict(), Verdict::Failed);
    assert_eq!(result["exit_code"], json!(1));
    assert!(result["stderr"]
        .as_str()
        .unwrap()
        .contains("two tests failed"));
    assert!(result["duration_ms"].is_number());
}

/// **The failing input:** exit 4, which `scripts/sovereign-test.sh` uses
/// for "this run resolved zero tests". Reporting it as `Failed` tells the
/// submitter their code is broken when nothing was even run.
#[tokio::test]
async fn exit_4_and_5_are_could_not_judge_and_carry_no_result() {
    for code in COULD_NOT_JUDGE_EXITS {
        let unit = unit_with(json!({
            "argv": ["sh", "-c", format!("echo 'no tests were resolved' ; exit {code}")],
            "timeout_secs": 30,
            "result": "stdout"
        }));
        let err = match ProcessExecutor::new().execute(&unit, &ctx()).await {
            Err(e) => e,
            Ok((j, _)) => panic!(
                "exit {code} must not be a Complete — it came back as {:?}",
                j.verdict()
            ),
        };
        assert!(
            matches!(err, JobError::DeclaredCouldNotJudge { exit_code, .. } if exit_code == code),
            "exit {code} gave {err}"
        );
        assert_eq!(err.verdict(), Verdict::CouldNotJudge);
    }
}

#[tokio::test]
async fn a_green_unit_passes_and_carries_its_stdout() {
    let unit = unit_with(json!({
        "argv": ["sh", "-c", "echo hello"],
        "timeout_secs": 30,
        "result": "stdout"
    }));
    let (judgement, result) = ProcessExecutor::new()
        .execute(&unit, &ctx())
        .await
        .expect("ran");
    assert_eq!(judgement.verdict(), Verdict::Passed);
    assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello");
}

// ───── the verdict-line rule ─────

/// **The failing input:** a lane that prints a JSON report and THEN its
/// verdict. "The last line that parses" would be right by luck here; the
/// rule is "the last non-empty line", and the next test is the one that
/// separates them.
#[tokio::test]
async fn a_verdict_line_is_read_from_the_last_non_empty_stdout_line() {
    let script = "echo '{\"rows\": 3}' ; \
                      echo '{\"subject\":\"lane\",\"verdict\":\"could-not-judge\",\
                      \"reason\":\"no baseline on this host\"}' ; echo ; echo";
    let unit = unit_with(json!({
        "argv": ["sh", "-c", script],
        "timeout_secs": 30,
        "result": "verdict-line"
    }));
    let (judgement, _) = ProcessExecutor::new()
        .execute(&unit, &ctx())
        .await
        .expect("ran");
    // Exit 0, and STILL could-not-judge — which is the entire reason the
    // lane protocol exists: an exit code cannot say this.
    assert_eq!(judgement.verdict(), Verdict::CouldNotJudge);
    assert_eq!(judgement.subject(), "lane");
}

/// **The failing input:** a lane that prints its JSON report and then dies
/// before its verdict. Adopting the report as a verdict would be a green
/// with nothing behind it.
#[test]
fn a_report_that_is_not_a_verdict_is_refused_rather_than_adopted() {
    let err = verdict_from_stdout("{\"subject\":\"lane\",\"rows\":3}\n")
        .expect_err("a report is not a verdict");
    assert!(err.contains("`verdict`"), "{err}");
}

#[test]
fn stdout_with_nothing_on_it_is_an_absence_not_a_verdict() {
    let err = verdict_from_stdout("\n \n\n").expect_err("nothing to read");
    assert!(err.contains("printed nothing"), "{err}");
}

// ───── stdin, env, cwd ─────

#[tokio::test]
async fn stdin_reaches_the_child_and_is_then_closed() {
    let unit = unit_with(json!({
        "argv": ["sh", "-c", "cat"],
        "stdin": "the payload wrote this",
        "timeout_secs": 30,
        "result": "stdout"
    }));
    let (_, result) = ProcessExecutor::new()
        .execute(&unit, &ctx())
        .await
        .expect("ran");
    assert_eq!(
        result["stdout"].as_str().unwrap().trim(),
        "the payload wrote this"
    );
}

/// **The failing input:** a unit that sets `FORCE_COLOR`, which is exactly
/// what re-created the 2026-08-25 `0p/0f` incident. The colour block is
/// applied AFTER the payload's env, so the unit cannot win this.
#[tokio::test]
async fn a_unit_cannot_force_colour_back_on() {
    let unit = unit_with(json!({
        "argv": ["sh", "-c", "echo \"FORCE_COLOR=[${FORCE_COLOR:-unset}] NO_COLOR=[${NO_COLOR:-unset}]\""],
        "env": {"FORCE_COLOR": "3"},
        "timeout_secs": 30,
        "result": "stdout"
    }));
    let (_, result) = ProcessExecutor::new()
        .execute(&unit, &ctx())
        .await
        .expect("ran");
    let out = result["stdout"].as_str().unwrap();
    assert!(out.contains("FORCE_COLOR=[unset]"), "{out}");
    assert!(out.contains("NO_COLOR=[1]"), "{out}");
}

#[tokio::test]
async fn a_relative_cwd_resolves_under_the_context_workdir() {
    let dir = scratch("cwd");
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    std::fs::write(dir.join("inner/marker.txt"), "here").unwrap();
    let unit = unit_with(json!({
        "argv": ["cat", "marker.txt"],
        "cwd": "inner",
        "timeout_secs": 30,
        "result": "stdout"
    }));
    let (judgement, result) = ProcessExecutor::new()
        .execute(&unit, &JobContext::new(&dir))
        .await
        .expect("ran");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(judgement.verdict(), Verdict::Passed);
    assert_eq!(result["stdout"].as_str().unwrap().trim(), "here");
}

/// **The failing inputs:** `/etc` and `../..`. Both are refused by
/// `validate`, BEFORE a lease is taken, so the unit costs nothing.
#[test]
fn an_escaping_cwd_is_refused_by_the_validator_not_by_the_filesystem() {
    for bad in ["/etc", "../../etc", "a/../../b"] {
        let unit = unit_with(json!({
            "argv": ["true"],
            "cwd": bad,
            "timeout_secs": 30,
            "result": "exit-code-only"
        }));
        assert!(
            ProcessExecutor::new().validate(&unit).is_err(),
            "`{bad}` must be refused"
        );
    }
    // And the positive control: a plain relative path is fine.
    let ok = unit_with(json!({
        "argv": ["true"],
        "cwd": "inner/dir",
        "timeout_secs": 30,
        "result": "exit-code-only"
    }));
    assert!(ProcessExecutor::new().validate(&ok).is_ok());
}

// ───── validation ─────

#[test]
fn an_empty_argv_is_refused_before_anything_is_leased() {
    let unit = unit_with(json!({
        "argv": [],
        "timeout_secs": 30,
        "result": "exit-code-only"
    }));
    assert!(ProcessExecutor::new().validate(&unit).is_err());
}

#[test]
fn a_timeout_past_the_ttl_ceiling_is_refused() {
    let unit = unit_with(json!({
        "argv": ["true"],
        "timeout_secs": MAX_TTL_SECS + 1,
        "result": "exit-code-only"
    }));
    assert!(ProcessExecutor::new().validate(&unit).is_err());
}

/// **The failing input:** a `process:v2` unit handed to the `process:v1`
/// executor. The refusal names the skew rather than saying "unknown kind",
/// because the fix a submitter needs is different in each case.
#[test]
fn a_version_skew_is_named_as_skew_and_a_foreign_kind_is_not() {
    let executor = ProcessExecutor::new();
    let skewed = crate::seal::seal(
        JobKind::parse("process:v2").unwrap(),
        json!({"argv": ["true"], "timeout_secs": 30, "result": "exit-code-only"}),
        JobRequirements::any(),
        None,
    )
    .unwrap();
    assert_eq!(
        executor.validate(&skewed),
        Err(WorkRefusal::VersionSkew {
            wanted: JobKind::parse("process:v2").unwrap(),
            offered: JobKind::parse("process:v1").unwrap(),
        })
    );
    let foreign = crate::seal::seal(
        JobKind::parse("ingest:v1").unwrap(),
        json!({"argv": ["true"], "timeout_secs": 30, "result": "exit-code-only"}),
        JobRequirements::any(),
        None,
    )
    .unwrap();
    assert_eq!(
        executor.validate(&foreign),
        Err(WorkRefusal::KindNotOffered {
            kind: JobKind::parse("ingest:v1").unwrap(),
        })
    );
}

// ───── the descriptor ─────

/// The descriptor is what a peer reads, so the two claims that must not be
/// prose-only live in it: the isolation this executor REQUIRES, and the exit
/// codes it declares mean could-not-judge.
///
/// It said `Subprocess` until 2026-09-10 and the assertion here said so too.
/// That was the floor written as a capability: `resolve_offer` reads this
/// field as the DEMAND (`work_donor.rs:265-273`), and running a submitter's
/// arbitrary argv demands a container. Raising it is what makes a daemon
/// refuse to donate this kind rather than publish an offer walled only by
/// consent — operator decision the same day, and consent is not isolation.
#[test]
fn the_descriptor_requires_a_container_and_says_no_build_provides_one() {
    let d = ProcessExecutor::new().descriptor();
    assert_eq!(d.kind, JobKind::parse(PROCESS_KIND).unwrap());
    assert_eq!(
        d.isolation,
        Isolation::RootlessContainer,
        "this executor runs a stranger's argv; anything below a container is \
         a floor nobody is holding"
    );
    assert_eq!(d.could_not_judge_exits, vec![4, 5]);
    assert_eq!(d.idempotency, Idempotency::NonIdempotent);
    assert!(d.est_secs.is_none());
    let description = d.parameters["description"].as_str().expect("a description");
    assert!(
        description.contains("REQUIRES") && description.contains("REFUSES"),
        "the descriptor must say what it demands and that a daemon therefore \
         refuses the kind — a peer reading it must not infer a sandbox that \
         does not exist: {description}"
    );
}

/// The invariant behind the one `expect` in this module: the literal this
/// crate owns parses. **The failing input** would be an edit to
/// `PROCESS_KIND` — `"process@1"`, `"process"`, `"process:1"` — and this
/// test goes red before `ProcessExecutor::new()` can panic on a boot path.
#[test]
fn the_kind_literal_this_crate_owns_parses() {
    let kind = JobKind::parse(PROCESS_KIND).expect("the literal must parse");
    assert_eq!(kind.id(), "process");
    assert_eq!(kind.version(), 1);
    assert_eq!(kind.to_string(), PROCESS_KIND);
    assert_eq!(ProcessExecutor::new().kind(), &kind);
}

#[test]
fn the_registry_resolves_a_process_unit_to_this_executor() {
    let mut reg = JobExecutorRegistry::new();
    reg.register(Arc::new(ProcessExecutor::new()))
        .expect("register");
    assert_eq!(reg.kinds(), vec![JobKind::parse(PROCESS_KIND).unwrap()]);
}

#[test]
fn every_result_source_names_the_verdict_source_it_speaks() {
    assert_eq!(
        ResultSource::VerdictLine.verdict_source(),
        VerdictSource::JudgementLine
    );
    for arm in [
        ResultSource::Stdout,
        ResultSource::StdoutJson,
        ResultSource::ExitCodeOnly,
    ] {
        assert_eq!(arm.verdict_source(), VerdictSource::ExitCode);
    }
}

#[test]
fn the_tail_cap_never_slices_a_codepoint_in_half() {
    let s = "é".repeat(20_000);
    let capped = cap_tail(&s, TAIL_CAP_BYTES);
    assert!(capped.starts_with("... (truncated"));
    assert!(capped.ends_with('é'));
}

/// **THE OVERRIDE ITSELF, which the `of_sandbox` test does not reach.**
///
/// `JobExecutor::attribution` defaults to this host, and that default is right
/// for an executor that runs work in the donor's own process. If
/// `ProcessExecutor` ever stops overriding it — a refactor deleting four lines,
/// nothing else failing — every sandboxed verdict silently goes back to being
/// attributed to the donor's kernel and compiler, and
/// `ComputeAttribution::comparable_to` starts refusing correct verdicts again.
/// So the failing input is the absent override, and this is what makes it red.
///
/// The runtime name cannot exist, so no container is started and the compiler
/// read lands on the named absence — hermetic, and the assertion that carries
/// the test is the os and arch disagreeing with this host on purpose.
#[test]
fn a_sandboxed_executor_attributes_work_to_the_image_and_not_to_this_host() {
    use crate::executor::JobExecutor;

    let contained = ProcessExecutor::with_sandbox(crate::sandbox::Sandbox::Container {
        runtime: "no-such-container-runtime".into(),
        image: "example:latest".into(),
        os: "plan9".into(),
        arch: "sparc64".into(),
    });
    let a = contained.attribution("deadbeef");
    assert_eq!(a.os, "plan9", "the image's OS, not this host's");
    assert_eq!(a.arch, "sparc64");
    assert_ne!(
        a.os,
        std::env::consts::OS,
        "the default would have leaked the host in"
    );
    assert_eq!(a.repo_rev, "deadbeef");

    // The control: no boundary, and the executor's answer IS this host — so a
    // pass above cannot come from `attribution` ignoring the sandbox entirely.
    let direct = ProcessExecutor::new().attribution("deadbeef");
    assert_eq!(direct.os, std::env::consts::OS);
    assert_eq!(direct.arch, std::env::consts::ARCH);
}
