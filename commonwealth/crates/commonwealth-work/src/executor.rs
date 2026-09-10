// SPDX-License-Identifier: AGPL-3.0-or-later
//! The executor seam — where a [`JobUnit`] becomes a [`Judgement`].
//!
//! A donor holds a registry of [`JobExecutor`]s, one per [`JobKind`], and the
//! fold hands it units. Everything a donor does to a unit happens through
//! [`JobExecutorRegistry::run`]: resolve the kind, validate the payload, run
//! it, and come back with either a verdict or a named reason there is none.
//!
//! # The `Complete`/`Fail` split IS this signature
//!
//! [`JobExecutor::execute`] returns `Result<(Judgement, Value), JobError>`, and
//! the two arms map one-to-one onto the two acts:
//!
//! | `execute` returns | the donor appends | `outcome` |
//! |---|---|---|
//! | `Ok((judgement, result))` | [`WorkAct::Complete`](crate::act::WorkAct::Complete) | whatever the judgement says, **including [`Verdict::Failed`]** |
//! | `Err(JobError)` | [`WorkAct::Fail`](crate::act::WorkAct::Fail) | [`Verdict::CouldNotJudge`] or [`Verdict::NeverRan`], never `Failed` |
//!
//! A red test shard is an `Ok` — it ran, it reached a verdict, and the verdict
//! is `Failed`. That is a `Complete` act carrying a failed judgement, not a
//! `Fail` act. `Fail` says something quite different: *we could not tell*. The
//! difference matters because a submitter reading "the tests failed" acts on
//! it, and a submitter reading "we could not tell" has to run it again.
//!
//! [`JobError`] holds that boundary **structurally** rather than by review:
//! it has no variant whose [`JobError::verdict`] is `Passed` or `Failed`, and
//! `no_job_error_can_claim_a_verdict` is the test that says so. A `Fail` act
//! also carries no `result` — there is none, because the only door that
//! produces one is the `Ok` arm.
//!
//! # Shape copied, code not
//!
//! `descriptor()` + a run method + resolve-by-kind is the shape of
//! `sovereign-workflow`'s `Step`/`StepRegistry`
//! (`studio/crates/sovereign-workflow/src/steps.rs:29,354`) and of
//! `Tool`/`ToolRegistry`. Both live outside this package's closure — the
//! closure is the reason this crate exists (cw-lift 5f lifts it into a
//! sandbox) — so the shape is copied and the code is not. What was NOT copied
//! is `StepRegistry`'s "resolve returns a fresh `Arc` per call": an executor
//! here is registered once and shared, because a `process:v1` executor is
//! stateless and a second instance would be a second answer to `descriptor()`.
//!
//! # Why there is no `#[async_trait]`
//!
//! `execute` is async and the registry is `dyn`-dispatched, which normally
//! means `async_trait`. Adding it would be a new third-party crate in a
//! manifest whose whole point is that it has none (see `Cargo.toml`), so the
//! desugaring is written out by hand as [`ExecuteFuture`] — which is exactly
//! what the macro emits. `std::future::Future` and `Pin<Box<..>>` are `std`,
//! and this module links no tokio: the `process` feature does that, for
//! [`crate::process`] alone.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kernel_types::{Judgement, Reason, Verdict};
use oicp_types::{Isolation, JobExecutorDescriptor, JobKind, JobUnit};
use serde_json::Value;

use crate::refusal::WorkRefusal;

/// The future [`JobExecutor::execute`] returns.
///
/// The hand-written form of what `#[async_trait]` generates. See the module
/// doc for why the macro is not used.
pub type ExecuteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(Judgement, Value), JobError>> + Send + 'a>>;

/// A sink a running unit reports progress to. The donor wires this to its own
/// `Renew` heartbeat.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

// -----------------------------------------------------------------
// The trait
// -----------------------------------------------------------------

/// One kind of work, and everything a donor needs to run it.
///
/// Three methods and no more, because the seam has exactly three questions:
/// what do you promise ([`descriptor`](JobExecutor::descriptor)), can you run
/// THIS unit ([`validate`](JobExecutor::validate)), and what happened
/// ([`execute`](JobExecutor::execute)).
///
/// The `validate`/`execute` split is the one the expenses ring template
/// already ships (`ring_cmd/templates/expenses.js:84,157`): a pure predicate a
/// submitter, a donor and `svrn job status` can all run without side effects,
/// and a writable path that runs it again before doing anything. It is not an
/// optimisation — it is what lets a refusal be reported before a lease is
/// taken, which is the difference between `NeverRan` and a burnt attempt.
pub trait JobExecutor: Send + Sync {
    /// What this executor publishes about itself. Identity is
    /// [`JobExecutorDescriptor::kind`]: the registry resolves by exactly that
    /// value.
    fn descriptor(&self) -> JobExecutorDescriptor;

    /// Can this executor run this unit, and if not, which rule says no?
    ///
    /// Pure: no filesystem, no clock, no spawn. A donor runs it before
    /// appending a `Lease`, so a refusal here costs the unit nothing.
    fn validate(&self, unit: &JobUnit) -> Result<(), WorkRefusal>;

    /// Run the unit. `Ok` is a verdict; `Err` is the absence of one.
    ///
    /// Cancellation and the wall cap are the executor's own responsibility —
    /// [`JobContext::cancel_requested`] is polled by the implementation,
    /// because only the implementation knows what it has to tear down.
    fn execute<'a>(&'a self, unit: &'a JobUnit, ctx: &'a JobContext) -> ExecuteFuture<'a>;
}

// -----------------------------------------------------------------
// The context
// -----------------------------------------------------------------

/// What a running unit is given: where to work, how to notice it should stop,
/// and where to say it is still alive.
///
/// Deliberately three fields. Anything a unit needs BEYOND these belongs in
/// its own payload, where it is hashed into the unit's identity and a peer can
/// see it; a context field is invisible to the rail and would be a way for two
/// donors to run "the same" unit differently.
pub struct JobContext {
    workdir: PathBuf,
    cancel: Arc<AtomicBool>,
    progress: Option<ProgressSink>,
}

impl JobContext {
    /// A context rooted at `workdir`, with nothing cancelling it and nobody
    /// listening for progress. Both are opt-in because a test and a `svrn job`
    /// dry run legitimately have neither.
    pub fn new(workdir: impl Into<PathBuf>) -> JobContext {
        JobContext {
            workdir: workdir.into(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
        }
    }

    /// Share an existing cancellation flag — the donor holds the other end and
    /// sets it when the lease is lost or the process is shutting down.
    ///
    /// An `Arc<AtomicBool>` rather than a token type from a runtime library:
    /// this module must compile with no tokio (see the module doc), and a flag
    /// that both a sync validator and an async executor can read is the shape
    /// that works either way.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> JobContext {
        self.cancel = cancel;
        self
    }

    /// Attach a progress sink. `None` is the default and is not an error: a
    /// unit that reports nothing is a unit nobody is watching, which is
    /// different from a unit that has stalled.
    pub fn with_progress(mut self, progress: ProgressSink) -> JobContext {
        self.progress = Some(progress);
        self
    }

    /// The directory a unit's relative paths resolve against, and the only
    /// directory it is expected to touch.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// The other end of the cancellation flag, for a donor that wants to stop
    /// this unit later.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// Set the flag. Idempotent; a second cancel is not an error.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Has somebody asked this unit to stop?
    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Report progress. Always traced at debug (ARCH §9.1) whether or not a
    /// sink is attached, so a stalled unit is visible under
    /// `RUST_LOG=commonwealth_work=debug` with no wiring at all.
    pub fn progress(&self, note: &str) {
        tracing::debug!(target: crate::TRACE_TARGET, note, "unit progress");
        if let Some(sink) = &self.progress {
            sink(note);
        }
    }
}

impl std::fmt::Debug for JobContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobContext")
            .field("workdir", &self.workdir)
            .field("cancel_requested", &self.cancel_requested())
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

// -----------------------------------------------------------------
// The absence of a verdict
// -----------------------------------------------------------------

/// Why a unit reached no verdict.
///
/// **Every variant is a `CouldNotJudge` or a `NeverRan`, and there is no
/// variant that is a `Passed` or a `Failed`.** That is not a convention: the
/// enum has no arm [`JobError::verdict`] could map to either, which is what
/// makes the `Complete`/`Fail` boundary in the module doc structural rather
/// than remembered (ARCH §7).
///
/// The split inside the two is the one the repo's own vocabulary already
/// draws: `NeverRan` is "this unit was never started", `CouldNotJudge` is "it
/// started and we still cannot tell".
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// No executor is registered for the unit's kind. The unit never started.
    #[error("no executor is registered for `{kind}` — this donor offered a kind it cannot run, or the unit reached a node that never registered one")]
    NoExecutor { kind: JobKind },

    /// The predicate said no. The unit never started, and the refusal names
    /// which rule.
    #[error("the unit was refused before it ran: {0}")]
    Refused(#[from] WorkRefusal),

    /// The child process could not be started at all — a missing binary, a
    /// workdir that is not there. The unit never started.
    #[error("`{program}` could not be started: {reason}")]
    Spawn { program: String, reason: String },

    /// The wall cap fired and the process group was killed. It started; it
    /// reached no verdict.
    #[error("the unit was still running after {secs}s and its process group was killed — no verdict was reached")]
    Timeout { secs: u64 },

    /// Somebody set the cancellation flag: the lease was lost to another
    /// donor, or this node is shutting down.
    #[error("the unit was cancelled before it reached a verdict — the lease was lost, or the donor is stopping")]
    Cancelled,

    /// The unit exited with a code its executor DECLARES means could-not-judge
    /// (`JobExecutorDescriptor::could_not_judge_exits`) rather than failed.
    ///
    /// The two that matter here are the ones `scripts/sovereign-test.sh`
    /// already defines: **4** is a run that resolved zero tests, and **5** is a
    /// run whose results could not be attributed to it. Both are exit-non-zero
    /// and neither is a failure of the code under test; reporting either as
    /// `Failed` is the false red this variant exists to prevent.
    #[error("the unit exited {exit_code}, an exit code this executor declares means could-not-judge rather than failed — {tail}")]
    DeclaredCouldNotJudge { exit_code: i32, tail: String },

    /// The unit ran and produced something, and that something is not a
    /// verdict: killed by a signal with no exit code at all, stdout that was
    /// supposed to be JSON and is not, a verdict line that is missing.
    #[error("the unit ran but no verdict could be read from it: {reason}")]
    NoVerdict { reason: String },
}

impl JobError {
    /// The verdict this absence maps to. Total, and never `Passed`/`Failed`.
    pub fn verdict(&self) -> Verdict {
        match self {
            // Nothing was started, so nothing can be said about the work.
            JobError::NoExecutor { .. } | JobError::Refused(_) => Verdict::NeverRan,
            // It started (or the attempt to start it is itself the failure to
            // reach a verdict) and we still cannot tell.
            JobError::Spawn { .. }
            | JobError::Timeout { .. }
            | JobError::Cancelled
            | JobError::DeclaredCouldNotJudge { .. }
            | JobError::NoVerdict { .. } => Verdict::CouldNotJudge,
        }
    }

    /// Render this as the [`Judgement`] a `Fail` act carries.
    ///
    /// `subject` is the caller's — usually [`subject_of`] — because a
    /// `JobError` does not know which unit it is about, and inventing a
    /// subject here would put two spellings of one unit on the rail.
    pub fn judgement(&self, subject: impl Into<String>) -> Judgement {
        let text = self.to_string();
        let reason = Reason::new(text).unwrap_or_else(|| {
            // `Reason::new` refuses placeholder text ("unknown", "n/a", …).
            // Every message above is a sentence, so this arm is unreachable
            // today — and it is written as a NAMED substitution rather than an
            // `expect`, because the alternative is a panic on the failure path
            // (ARCH §18.3).
            Reason::literal(
                "the executor's own refusal text was a placeholder — that is a bug in \
                 commonwealth-work, not in the unit",
            )
        });
        match self.verdict() {
            Verdict::NeverRan => Judgement::never_ran(subject, reason),
            _ => Judgement::could_not_judge(subject, reason),
        }
    }
}

/// The one spelling of what a judgement about a unit is *about*.
///
/// `<kind> <unit_hash>`: the kind so a reader can tell a `process:v1` row from
/// an `ingest:v1` row without a join, the hash because that is the unit's
/// identity. Written once here so a `Complete` and a `Fail` about the same
/// unit carry the same subject (ARCH §10.6).
pub fn subject_of(unit: &JobUnit) -> String {
    format!("{} {}", unit.kind, unit.unit_hash)
}

// -----------------------------------------------------------------
// The registry
// -----------------------------------------------------------------

/// Two executors claimed one kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{kind}` already has an executor registered — a kind resolves to exactly one implementation, and a second one would mean two answers to what that kind DOES on this node")]
pub struct DuplicateExecutor {
    pub kind: JobKind,
}

/// Kind → executor, and the one door a unit goes through.
///
/// Keyed on [`JobKind`] and not on its string form: `process:v1` and
/// `process:v2` are different executors and the type already refuses the
/// spellings that would collide them (`ingest@1`, `v01`).
#[derive(Default)]
pub struct JobExecutorRegistry {
    by_kind: BTreeMap<JobKind, Arc<dyn JobExecutor>>,
}

/// A kind this build declines to offer, and the demand it could not meet.
///
/// Carries `provides` alongside `required` so the sentence a caller writes
/// names BOTH ends — an operator told only what was required cannot tell
/// whether to change the config or the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedKind {
    /// The kind that will not be offered.
    pub kind: JobKind,
    /// What its executor demands.
    pub required: Isolation,
    /// What this build actually provides.
    pub provides: Isolation,
}

impl std::fmt::Display for DroppedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` requires `{:?}` isolation and this build provides `{:?}`",
            self.kind, self.required, self.provides
        )
    }
}

/// The partition [`JobExecutorRegistry::offerable`] returns.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OfferableKinds {
    /// Kinds this node may publish.
    pub offerable: Vec<JobKind>,
    /// Kinds the isolation floor declined, each naming both ends.
    pub dropped: Vec<DroppedKind>,
    /// Kinds with no executor at all — the operator's typo, not a floor.
    pub unregistered: Vec<JobKind>,
}

impl JobExecutorRegistry {
    pub fn new() -> JobExecutorRegistry {
        JobExecutorRegistry {
            by_kind: BTreeMap::new(),
        }
    }

    /// Register an executor under the kind its own descriptor names.
    ///
    /// The kind comes from [`JobExecutor::descriptor`] rather than from a
    /// second argument, so there is no way to register an executor under a
    /// kind it does not claim — the mismatch that would let `resolve` hand a
    /// unit to the wrong implementation.
    ///
    /// A duplicate is REFUSED, not overwritten. Last-writer-wins here would
    /// make boot order decide which executor runs a kind, silently.
    pub fn register(&mut self, executor: Arc<dyn JobExecutor>) -> Result<(), DuplicateExecutor> {
        let kind = executor.descriptor().kind;
        if self.by_kind.contains_key(&kind) {
            tracing::debug!(target: crate::TRACE_TARGET, %kind, "duplicate executor refused");
            return Err(DuplicateExecutor { kind });
        }
        tracing::debug!(target: crate::TRACE_TARGET, %kind, "executor registered");
        self.by_kind.insert(kind, executor);
        Ok(())
    }

    /// The executor for a kind, or `None`.
    pub fn resolve(&self, kind: &JobKind) -> Option<Arc<dyn JobExecutor>> {
        self.by_kind.get(kind).map(Arc::clone)
    }

    /// Every kind this node can actually run.
    ///
    /// This is what a boot-time check compares a `WorkOffer` against: offering
    /// a kind with no executor is a node that takes work it will always fail.
    pub fn kinds(&self) -> Vec<JobKind> {
        self.by_kind.keys().cloned().collect()
    }

    /// Every descriptor, for a node publishing what it can do.
    pub fn descriptors(&self) -> Vec<JobExecutorDescriptor> {
        self.by_kind.values().map(|e| e.descriptor()).collect()
    }

    /// **THE ISOLATION FLOOR — one decider, every donor** (ARCH §10.6).
    ///
    /// Which of `wanted` this node may OFFER, given the isolation its build
    /// actually `provides`. A kind whose executor demands more is DROPPED,
    /// with what it demanded, so the caller can name the drop rather than
    /// swallow it (ARCH §18.3).
    ///
    /// It lives here, in the package both donors link, because it was in
    /// `sovereign-mesh`'s boot decision alone until 2026-09-10 and a donor
    /// built from this crate therefore had no floor at all — the lifted peer
    /// (`examples/work_peer.rs`) published `process:v1` and ran a stranger's
    /// argv with nothing in front of it. That is the same hole, in the same
    /// shape, as `lease_state` and `host_satisfies` before they came home:
    /// a decision a second donor has to re-derive is a decision a second
    /// donor gets WRONG, and this one is the security-relevant member of the
    /// set. A caller may not skip it and still be a donor.
    ///
    /// `unregistered` is kept apart from `dropped` because the two are not
    /// the same failure: an unregistered kind is the operator's typo and the
    /// daemon refuses to boot on it, while a dropped kind is this build
    /// declining to do something unsafe under a config that was valid when it
    /// was written.
    pub fn offerable(&self, wanted: &[JobKind], provides: Isolation) -> OfferableKinds {
        let mut out = OfferableKinds::default();
        for kind in wanted {
            let Some(executor) = self.resolve(kind) else {
                out.unregistered.push(kind.clone());
                continue;
            };
            let required = executor.descriptor().isolation;
            if provides.covers(required) {
                out.offerable.push(kind.clone());
            } else {
                out.dropped.push(DroppedKind {
                    kind: kind.clone(),
                    required,
                    provides,
                });
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.by_kind.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_kind.len()
    }

    /// Resolve, validate, run. **The** door: a donor that reaches
    /// `JobExecutor::execute` directly has skipped the validator, which is the
    /// step that turns a bad unit into a `NeverRan` instead of a burnt
    /// attempt.
    pub async fn run(
        &self,
        unit: &JobUnit,
        ctx: &JobContext,
    ) -> Result<(Judgement, Value), JobError> {
        let executor = self.resolve(&unit.kind).ok_or_else(|| {
            tracing::debug!(target: crate::TRACE_TARGET, kind = %unit.kind, "no executor for kind");
            JobError::NoExecutor {
                kind: unit.kind.clone(),
            }
        })?;
        executor.validate(unit)?;
        executor.execute(unit, ctx).await
    }
}

impl std::fmt::Debug for JobExecutorRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobExecutorRegistry")
            .field("kinds", &self.kinds())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::quality::VerdictSource;
    use oicp_types::{Isolation, JobRequirements};
    use serde_json::json;

    /// An executor that says yes to everything and returns a fixed verdict.
    /// Enough to test the registry without a subprocess.
    struct StubExecutor {
        kind: JobKind,
        verdict: Verdict,
    }

    impl JobExecutor for StubExecutor {
        fn descriptor(&self) -> JobExecutorDescriptor {
            JobExecutorDescriptor {
                kind: self.kind.clone(),
                isolation: Isolation::InProcess,
                parameters: json!({"type": "object"}),
                examples: Vec::new(),
                idempotency: oicp_types::tool::Idempotency::Idempotent,
                lease_interval_ms: 1_000,
                est_secs: None,
                could_not_judge_exits: Vec::new(),
                verdict: VerdictSource::ExitCode,
            }
        }

        fn validate(&self, _unit: &JobUnit) -> Result<(), WorkRefusal> {
            Ok(())
        }

        fn execute<'a>(&'a self, unit: &'a JobUnit, _ctx: &'a JobContext) -> ExecuteFuture<'a> {
            let verdict = self.verdict;
            Box::pin(async move {
                let subject = subject_of(unit);
                let reason = Reason::literal("the stub executor decided this before it ran");
                let j = match verdict {
                    Verdict::Passed => Judgement::passed(subject, reason),
                    _ => Judgement::failed(subject, reason),
                };
                Ok((j, json!({"stub": true})))
            })
        }
    }

    fn kind(raw: &str) -> JobKind {
        JobKind::parse(raw).expect("test kind")
    }

    fn unit(raw_kind: &str) -> JobUnit {
        crate::seal::seal(
            kind(raw_kind),
            json!({"argv": ["true"]}),
            JobRequirements::any(),
            None,
        )
        .expect("seal")
    }

    /// The failing input: a second executor claiming `process:v1` after one
    /// already has it. Overwriting would make boot ORDER decide which
    /// implementation runs a kind, with nothing anywhere saying so.
    #[test]
    fn a_second_executor_for_one_kind_is_refused_not_overwritten() {
        let mut reg = JobExecutorRegistry::new();
        reg.register(Arc::new(StubExecutor {
            kind: kind("process:v1"),
            verdict: Verdict::Passed,
        }))
        .expect("first registration");
        let err = reg
            .register(Arc::new(StubExecutor {
                kind: kind("process:v1"),
                verdict: Verdict::Failed,
            }))
            .expect_err("second registration must be refused");
        assert_eq!(err.kind, kind("process:v1"));
        assert_eq!(reg.len(), 1);
    }

    /// The failing input: a unit whose kind nothing registered. The tempting
    /// wrong answer is `Failed` ("we could not run it, so it failed"), which
    /// tells the submitter their code is broken.
    ///
    /// Behind `process` only because that is where this crate's only async
    /// runtime comes from — the module itself links no tokio, and adding a
    /// dev-dependency to get one would be the new third-party crate the
    /// manifest exists to refuse. Run with
    /// `cargo test -p commonwealth-work --features process`.
    #[cfg(feature = "process")]
    #[tokio::test]
    async fn an_unregistered_kind_is_never_ran_and_not_a_failure() {
        let reg = JobExecutorRegistry::new();
        let ctx = JobContext::new(std::env::temp_dir());
        let u = unit("ingest:v1");
        let err = reg.run(&u, &ctx).await.expect_err("nothing registered");
        assert_eq!(err.verdict(), Verdict::NeverRan);
        assert_eq!(err.judgement(subject_of(&u)).verdict(), Verdict::NeverRan);
    }

    /// The structural half of the `Complete`/`Fail` boundary: no `JobError`
    /// can ever claim the work passed or failed. The failing input would be a
    /// new variant added with a `Passed`/`Failed` arm in `verdict()` — this
    /// test is what turns that into a red build.
    #[test]
    fn no_job_error_can_claim_a_verdict() {
        let every: Vec<JobError> = vec![
            JobError::NoExecutor {
                kind: kind("process:v1"),
            },
            JobError::Spawn {
                program: "nope".into(),
                reason: "no such file".into(),
            },
            JobError::Timeout { secs: 1 },
            JobError::Cancelled,
            JobError::DeclaredCouldNotJudge {
                exit_code: 4,
                tail: "no tests were resolved".into(),
            },
            JobError::NoVerdict {
                reason: "stdout was not JSON".into(),
            },
        ];
        for e in &every {
            assert!(
                matches!(e.verdict(), Verdict::CouldNotJudge | Verdict::NeverRan),
                "{e} maps to {:?}, which would put it on a Complete act",
                e.verdict()
            );
            // And the rendered judgement agrees with the verdict.
            assert_eq!(e.judgement("subject under test").verdict(), e.verdict());
        }
    }

    #[test]
    fn a_context_carries_its_cancel_flag_to_whoever_holds_the_other_end() {
        let ctx = JobContext::new("/tmp");
        let handle = ctx.cancel_handle();
        assert!(!ctx.cancel_requested());
        handle.store(true, Ordering::SeqCst);
        assert!(ctx.cancel_requested());
    }

    /// See the note on `an_unregistered_kind_is_never_ran_and_not_a_failure`
    /// for why this one needs the `process` feature.
    #[cfg(feature = "process")]
    #[tokio::test]
    async fn the_registry_runs_the_executor_its_descriptor_claims() {
        let mut reg = JobExecutorRegistry::new();
        reg.register(Arc::new(StubExecutor {
            kind: kind("process:v1"),
            verdict: Verdict::Failed,
        }))
        .expect("register");
        let ctx = JobContext::new(std::env::temp_dir());
        let (j, result) = reg.run(&unit("process:v1"), &ctx).await.expect("ran");
        // A red unit is an Ok — it reached a verdict, and the verdict is
        // Failed. This is the Complete arm.
        assert_eq!(j.verdict(), Verdict::Failed);
        assert_eq!(result, json!({"stub": true}));
        assert_eq!(reg.kinds(), vec![kind("process:v1")]);
    }

    // ───── the isolation floor ─────

    /// A registry holding one executor that demands `required`.
    fn reg_demanding(k: &str, required: Isolation) -> JobExecutorRegistry {
        let mut reg = JobExecutorRegistry::new();
        reg.register(Arc::new(DemandingExecutor {
            kind: kind(k),
            required,
        }))
        .expect("register");
        reg
    }

    struct DemandingExecutor {
        kind: JobKind,
        required: Isolation,
    }

    impl JobExecutor for DemandingExecutor {
        fn descriptor(&self) -> JobExecutorDescriptor {
            JobExecutorDescriptor {
                kind: self.kind.clone(),
                isolation: self.required,
                parameters: json!({"type": "object"}),
                examples: Vec::new(),
                idempotency: oicp_types::tool::Idempotency::Idempotent,
                lease_interval_ms: 1_000,
                est_secs: None,
                could_not_judge_exits: Vec::new(),
                verdict: kernel_types::quality::VerdictSource::ExitCode,
            }
        }
        fn validate(&self, _unit: &JobUnit) -> Result<(), WorkRefusal> {
            Ok(())
        }
        fn execute<'a>(&'a self, _u: &'a JobUnit, _c: &'a JobContext) -> ExecuteFuture<'a> {
            unreachable!("the floor never runs a unit")
        }
    }

    /// **THE FLOOR (ARCH §18.1).** The failing input is the shipped shape: an
    /// executor that demands a container on a build that provides a bare
    /// subprocess. Before this decider lived here, `sovereign-mesh`'s boot
    /// path had it and a donor built from this crate alone had NOTHING —
    /// which is how the lifted peer came to publish `process:v1` and run a
    /// stranger's argv with only consent in front of it.
    #[test]
    fn a_kind_demanding_more_than_this_build_provides_is_dropped_naming_both_ends() {
        let reg = reg_demanding("risky:v1", Isolation::RootlessContainer);
        let p = reg.offerable(&[kind("risky:v1")], Isolation::Subprocess);
        assert!(p.offerable.is_empty(), "a demand unmet is not offerable");
        assert!(
            p.unregistered.is_empty(),
            "it IS registered — that is not the failure"
        );
        assert_eq!(p.dropped.len(), 1);
        let said = p.dropped[0].to_string();
        assert!(
            said.contains("risky:v1")
                && said.contains("RootlessContainer")
                && said.contains("Subprocess"),
            "the drop must name the kind, the demand AND what this build \
             provides — told only the demand, an operator cannot tell whether \
             to change the config or the build: {said}"
        );
    }

    /// The control, and the direction that must not over-refuse: a demand this
    /// build MEETS is offered. `covers` is upward-only, so a stronger build
    /// running a weaker demand is fine and must not be dropped.
    #[test]
    fn a_demand_this_build_meets_or_exceeds_is_offered() {
        let reg = reg_demanding("safe:v1", Isolation::InProcess);
        let p = reg.offerable(&[kind("safe:v1")], Isolation::Subprocess);
        assert_eq!(p.offerable, vec![kind("safe:v1")]);
        assert!(p.dropped.is_empty(), "Subprocess covers InProcess");
    }

    /// An unregistered kind is the operator's typo and stays a SEPARATE
    /// answer. Collapsing the two would let a caller refuse a boot over a
    /// safety drop, or shrug off a typo as a safety drop — the two need
    /// different sentences and different consequences.
    #[test]
    fn an_unregistered_kind_is_not_reported_as_an_isolation_drop() {
        let reg = reg_demanding("safe:v1", Isolation::InProcess);
        let p = reg.offerable(&[kind("ghost:v1")], Isolation::Subprocess);
        assert_eq!(p.unregistered, vec![kind("ghost:v1")]);
        assert!(p.dropped.is_empty());
        assert!(p.offerable.is_empty());
    }
}
