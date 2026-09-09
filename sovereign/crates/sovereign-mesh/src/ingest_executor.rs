// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ingest:v1` — one corpus partition as a unit on the `work` fold
//! (cw-lift 5g).
//!
//! # What moves here, and what does not
//!
//! **The lease moves; the ingest does not.** Everything this module runs is
//! the same call the pull loop already made —
//! [`CorpusEngine::ingest_with_overrides`], with the same
//! `(file_indices, article_range)` pair derived by the same
//! [`WorkUnit::to_ingest_args`]. What is REPLACED around it is the second
//! lease decider: `next_unit`/`heartbeat`/`complete_unit` over HTTP, the
//! coordinator's `WorkQueueManager`, its reaper, and its own expiry and
//! attempt arithmetic. Who holds a unit becomes `commonwealth_work`'s fold —
//! the same one `process:v1` rides — and this file is only the executor seam
//! it hands units to.
//!
//! **Replaced, not yet deleted, and the tense is load-bearing.** cw-lift 5g
//! part 1 is the proof: this executor and the legacy pull path both exist,
//! and the legacy one is still what `auto_ingest::discover_and_spawn_pull_
//! loops` drives for a collaborate handoff. Part 2 is the deletion and is a
//! separate, explicitly-authorised step — a replacement that has not been
//! watched carrying real traffic is not a replacement, and the campaign's
//! `cw-work-one-decider` bar rides on the arithmetic of what actually goes.
//!
//! `commonwealth-work` learns nothing about ingest and must not: it carries a
//! `[[forbid]] -> sovereign-*` row in `quality/ARCH_LAYERS.toml` and cw-lift
//! 5f builds it in a sandbox outside this monorepo. The trait is the whole of
//! what the package owns; the kind, the payload and the body live here, on
//! the sovereign side of that line, and reach the fold through
//! [`crate::work_donor::donor_registry`].
//!
//! # Why this executor declares `InProcess`
//!
//! It is the truth about the mechanism.
//! [`ProcessExecutor`](commonwealth_work::process::ProcessExecutor) spawns a
//! child in its own process group; this one calls into `corpus-engine` on the
//! daemon's own threads, so a panic or a leak is the daemon's. Declaring
//! anything stronger would be a claim with no mechanism behind it.
//! `Isolation::covers` is an "at least as strong" comparison, so a donor
//! offering [`crate::work_donor::DONOR_ISOLATION`] (`Subprocess`) covers this
//! requirement and boot passes — the weaker requirement is the one that is
//! satisfiable, not the one that is refused.
//!
//! # Why an ingest failure is a `Fail` act and never a failed verdict
//!
//! The `Complete`/`Fail` split is a real distinction here and it is not the
//! one a reader expects. `WorkProjection::complete` puts a `Fail` back on the
//! queue with its attempt counted, and terminal only once
//! [`MAX_UNIT_ATTEMPTS`] are spent — which is precisely what
//! `HandoffQueue::ack_failure` did with `CompleteOutcome::Failed`. A
//! `Complete` carrying a `Verdict::Failed` is terminal and never retried.
//!
//! So the mapping is:
//!
//! | the engine returned | this executor returns | the fold does |
//! |---|---|---|
//! | `Ok(IngestResult)` | `Ok((passed, result))` | terminal `Complete` |
//! | `Err(_)` | `Err(JobError::NoVerdict)` | requeue, then `Failed` at the cap |
//! | `Err(Cancelled)` **and the donor asked** | `Err(JobError::Cancelled)` | nothing is appended; the lease lapses |
//!
//! There is deliberately no arm that produces `Judgement::failed`. A slice
//! either produced its shard or it did not, and "it did not" is always worth
//! another attempt — that is the old path's own rule, kept. Claiming a failed
//! VERDICT would be saying the corpus is un-ingestable, which nothing here
//! measured.
//!
//! # At-least-once is safe on this kind, and that is a fact not a hope
//!
//! [`Idempotency::Idempotent`] is declared because the merge step dedupes:
//! `merge_shards` keys on `content_hash` and on `(unit_id, source_doc_id)`
//! (`corpus-engine/src/sharding.rs:780-784`), which is exactly why the
//! `unit_id` is threaded through `ingest_with_overrides` at all. Two donors
//! that both ran one unit after a lease lapse cost disk and time; they do not
//! corrupt the merged corpus.

use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::knowledge::{UnitId, WorkUnit, LEASE_MS, MAX_UNIT_ATTEMPTS};
use commonwealth_work::executor::{
    subject_of, ExecuteFuture, JobContext, JobError, JobExecutor,
};
use commonwealth_work::refusal::WorkRefusal;
use corpus_engine::{CorpusEngine, IngestProgress, ProgressCallback};
use kernel_types::quality::VerdictSource;
use kernel_types::{Judgement, Reason};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
// Through `sovereign_contracts`' re-export, not a direct dep on `oicp-types`
// (ARCH §8.3) — the rule `work_donor` already follows for the same types.
use sovereign_contracts::oicp::{
    Idempotency, Isolation, JobExecutorDescriptor, JobKind, JobUnit, ToolExample,
};
use tracing::debug;

/// The kind this module owns, and the one spelling of it.
///
/// A `const` here rather than a literal at each site, for the reason
/// `PROCESS_KIND` is one: `svrn job submit --kind ingest:v1` and the boot-time
/// offer check have to be comparing the same string, and a second literal is a
/// second answer (ARCH §10.6). Sovereign-side because `commonwealth-work` has
/// no business knowing this kind exists.
pub const INGEST_KIND: &str = "ingest:v1";

/// The tracing target for everything this module decides.
///
/// `commonwealth_work`'s own, the way [`crate::work_donor`] does it: a reader
/// debugging "why did this ingest unit not run" turns on ONE filter
/// (`RUST_LOG=commonwealth_work=debug`) and sees the fold's refusals and this
/// executor's in the same stream (ARCH §9.1).
pub const TRACE_TARGET: &str = commonwealth_work::TRACE_TARGET;

/// How often the running unit is asked whether the donor has cancelled it.
///
/// The old pull loop spent a whole `tokio::spawn`ed task on this bridge
/// (`auto_ingest.rs:995-1004`) polling at the same cadence. It is a `select!`
/// arm here instead: one future rather than two tasks, and the flag is read on
/// the same loop that drains progress.
const CANCEL_POLL: Duration = Duration::from_millis(500);

// -----------------------------------------------------------------
// The payload
// -----------------------------------------------------------------

/// An `ingest:v1` unit's payload — which slice of which corpus.
///
/// The whole of it is hashed into the unit's identity by
/// `commonwealth_work::seal`, so two units that differ in one article index
/// are two units. That is what makes the fold's idempotency-per-`unit_hash`
/// line up with the merge step's dedupe-per-`unit_id`.
///
/// **`unit` is `commonwealth_core::knowledge::WorkUnit`, reused rather than
/// re-spelled.** That enum and its [`WorkUnit::to_ingest_args`] are already
/// the one decider for "which shards / which article range does this slice
/// mean"; writing `file_indices` and `article_range` into this payload
/// directly would be a second speller of a mapping that has to agree with the
/// first exactly when a partition is at stake (ARCH §10.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestPayload {
    /// The corpus this slice belongs to. Names the partition directory and
    /// the cancellation flag.
    pub corpus_id: String,
    /// The recipe that says how to acquire, extract, chunk and embed it.
    pub recipe_id: String,
    /// The slice's position in the handoff's unit list. Stamped into every
    /// chunk's `unit_id` column so the merge can dedupe two donors' output
    /// for one unit.
    pub unit_id: UnitId,
    /// Which shards or which article range.
    pub unit: WorkUnit,
}

impl IngestPayload {
    /// Build a payload for one slice. Exists so no caller spells this as a
    /// JSON literal — the mistake that shipped `process:v1` units with no
    /// `timeout_secs` and had every donor refuse them silently
    /// (`process.rs:230-256`). Adding a field here is a compile error at every
    /// caller rather than a refusal at every donor.
    pub fn slice(
        corpus_id: impl Into<String>,
        recipe_id: impl Into<String>,
        unit_id: UnitId,
        unit: WorkUnit,
    ) -> IngestPayload {
        IngestPayload {
            corpus_id: corpus_id.into(),
            recipe_id: recipe_id.into(),
            unit_id,
            unit,
        }
    }

    /// Read a payload out of a unit, or say which rule it broke.
    ///
    /// The `Err` is a sentence a submitter can act on, not a code — the shape
    /// `ProcessPayload::parse` uses, and the text a
    /// [`WorkRefusal::PayloadNotCanonical`] carries.
    pub fn parse(payload: &Value) -> Result<IngestPayload, String> {
        let parsed: IngestPayload = serde_json::from_value(payload.clone()).map_err(|e| {
            format!(
                "an `{INGEST_KIND}` payload is \
                 {{\"corpus_id\": …, \"recipe_id\": …, \"unit_id\": …, \"unit\": \
                 {{\"kind\": \"hf-file\"|\"jsonl-shard\"|\"jsonl-range\", \"value\": …}}}} — {e}"
            )
        })?;
        if parsed.corpus_id.trim().is_empty() {
            return Err("`corpus_id` is empty — a slice with no corpus names no \
                        partition directory to write into"
                .to_string());
        }
        if parsed.recipe_id.trim().is_empty() {
            return Err("`recipe_id` is empty — a slice with no recipe cannot say how to \
                        acquire, extract or embed anything"
                .to_string());
        }
        Ok(parsed)
    }
}

// -----------------------------------------------------------------
// The executor
// -----------------------------------------------------------------

/// Runs one corpus slice through this node's own [`CorpusEngine`].
pub struct IngestExecutor {
    kind: JobKind,
    engine: Arc<CorpusEngine>,
}

impl IngestExecutor {
    // `JobKind::parse` is fallible because it exists to refuse what arrives
    // off a WIRE. `INGEST_KIND` does not arrive off a wire — it is a literal
    // this module owns — so the only edit that could reach the error arm is an
    // edit to that literal, and `the_kind_literal_this_module_owns_parses`
    // goes red before this ever runs. Returning a `Result` here would put an
    // unreachable arm on the boot path instead. Same call, same reason, as
    // `ProcessExecutor::new`.
    #[allow(clippy::expect_used)]
    pub fn new(engine: Arc<CorpusEngine>) -> IngestExecutor {
        IngestExecutor {
            kind: JobKind::parse(INGEST_KIND)
                .expect("`ingest:v1` is a valid JobKind by construction"),
        engine,
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
        let payload = IngestPayload::parse(&unit.payload)
            .map_err(|detail| JobError::Refused(WorkRefusal::PayloadNotCanonical { detail }))?;

        let (file_indices, article_range) = payload.unit.to_ingest_args();
        let output_path = self.engine.partition_path(&payload.corpus_id);

        // The engine's own cooperative cancellation, registered for the
        // duration of this unit exactly as the pull loop did it. `register`
        // returns an existing flag when one is already there, so a desktop
        // "cancel this corpus" and a lost lease reach the same ingest loop.
        let engine_cancel = self.engine.cancel_registry().register(&payload.corpus_id);

        debug!(
            target: TRACE_TARGET,
            unit_hash = %unit.unit_hash,
            corpus = %payload.corpus_id,
            recipe = %payload.recipe_id,
            unit_id = payload.unit_id,
            output = %output_path.display(),
            "ingest:v1 starting a slice"
        );

        // Progress arrives on a channel rather than through a captured `&ctx`:
        // `ProgressCallback` is `Box<dyn Fn(..) + Send + Sync>` and therefore
        // `'static`, and the context is borrowed for this call only. The
        // channel is what lets the SAME loop that awaits the ingest also
        // report progress and poll for cancellation.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<IngestProgress>();
        let progress: ProgressCallback = Box::new(move |p| {
            // A closed receiver means the ingest already returned; dropping is
            // the whole of the correct behaviour.
            let _ = tx.send(p);
        });

        let ingest = self.engine.ingest_with_overrides(
            &payload.recipe_id,
            file_indices,
            article_range,
            &output_path,
            Some(progress),
            Some(payload.unit_id),
        );
        tokio::pin!(ingest);
        let mut ticker = tokio::time::interval(CANCEL_POLL);
        // `Delay`, not `Burst`: a poll that ran late must not then fire the
        // ticks it missed back-to-back — the reason `run_unit`'s heartbeat
        // ticker sets the same behaviour.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let outcome = loop {
            tokio::select! {
                result = &mut ingest => break result,
                Some(p) = rx.recv() => {
                    // Serialised rather than matched: `IngestProgress` has
                    // eight variants and a match here would be a second
                    // renderer of them, drifting from the desktop's the first
                    // time a variant is added (ARCH §10.6). `progress` traces
                    // at debug whether or not a sink is attached.
                    match serde_json::to_string(&p) {
                        Ok(note) => ctx.progress(&note),
                        Err(e) => debug!(
                            target: TRACE_TARGET, error = %e,
                            "ingest:v1 progress could not be rendered"
                        ),
                    }
                }
                _ = ticker.tick() => {
                    if ctx.cancel_requested() && !engine_cancel.is_cancelled() {
                        debug!(
                            target: TRACE_TARGET,
                            unit_hash = %unit.unit_hash,
                            corpus = %payload.corpus_id,
                            "ingest:v1 donor cancelled the unit — signalling the ingest loop"
                        );
                        engine_cancel.cancel();
                    }
                }
            }
        };
        self.engine.cancel_registry().unregister(&payload.corpus_id);

        match outcome {
            Ok(result) => {
                // `chunks_created` is NOT this unit's delta and must not be
                // published as one. `ingest_inner` seeds its counter from
                // `index.chunk_count()` before it writes anything
                // (`corpus-engine/src/engine/ingest.rs:989`), so on the second
                // unit into one partition it is the partition's running total
                // — measured here as 813 after a unit that added 391. Every
                // unit on a node shares one `partition_path`, so this is the
                // normal case, not an edge one. Named for what it is (ARCH
                // §18.3): a subject line reading "unit 0 ingested 813 chunks"
                // would be a number the rail carries forever and nobody can
                // reconcile against the merge.
                let text = format!(
                    "unit {} of `{}` ran in {}s, {} document(s) skipped; \
                     its partition {} now holds {} chunk(s) in total",
                    payload.unit_id,
                    payload.corpus_id,
                    result.duration_secs,
                    result.docs_skipped,
                    output_path.display(),
                    result.chunks_created,
                );
                debug!(
                    target: TRACE_TARGET,
                    unit_hash = %unit.unit_hash,
                    partition_chunks_total = result.chunks_created,
                    docs_skipped = result.docs_skipped,
                    duration_secs = result.duration_secs,
                    "ingest:v1 slice complete"
                );
                let reason = Reason::new(text).unwrap_or_else(|| {
                    // `Reason::new` refuses placeholder text. The sentence
                    // above always names a corpus and a count, so this arm is
                    // unreachable today — written as a NAMED substitution
                    // rather than an `expect`, because the alternative is a
                    // panic on the success path (ARCH §18.3).
                    Reason::literal(
                        "the ingest reported a result whose own summary was placeholder text — \
                         that is a bug in sovereign-mesh, not in the corpus",
                    )
                });
                let result_json = json!({
                    "corpus_id": payload.corpus_id,
                    "unit_id": payload.unit_id,
                    // See the comment above `text`: the partition's running
                    // total, which is what corpus-engine measures, not this
                    // unit's contribution — which nothing measures.
                    "partition_chunks_total": result.chunks_created,
                    "docs_skipped": result.docs_skipped,
                    "index_size_bytes": result.index_size_bytes,
                    "duration_secs": result.duration_secs,
                    "partition_path": output_path.display().to_string(),
                });
                Ok((Judgement::passed(subject_of(unit), reason), result_json))
            }
            // The donor asked, so this node no longer holds the lease.
            // `run_unit` publishes nothing for a `Cancelled` — a report from a
            // non-lessee is what the fold counts `unreadable`.
            Err(corpus_engine::Error::Cancelled(_)) if ctx.cancel_requested() => {
                debug!(
                    target: TRACE_TARGET,
                    unit_hash = %unit.unit_hash,
                    corpus = %payload.corpus_id,
                    "ingest:v1 stopped because the donor cancelled it"
                );
                Err(JobError::Cancelled)
            }
            Err(e) => {
                // Every other error, cancellation from somewhere ELSE
                // included. `NoVerdict` is `CouldNotJudge`, which the fold
                // requeues with the attempt counted and makes terminal at
                // MAX_UNIT_ATTEMPTS — `ack_failure`'s rule, unchanged.
                //
                // It is the closest arm in a vocabulary this module may not
                // extend: `JobError` lives in `commonwealth-work`, and adding
                // an ingest-shaped variant there would put ingest inside the
                // package closure cw-lift 5f lifts. Named here rather than
                // hidden (ARCH §18.3).
                debug!(
                    target: TRACE_TARGET,
                    unit_hash = %unit.unit_hash,
                    corpus = %payload.corpus_id,
                    unit_id = payload.unit_id,
                    error = %e,
                    max_attempts = MAX_UNIT_ATTEMPTS,
                    "ingest:v1 slice reached no verdict — the fold will requeue it"
                );
                Err(JobError::NoVerdict {
                    reason: format!(
                        "unit {} of `{}` did not finish ingesting: {e}",
                        payload.unit_id, payload.corpus_id
                    ),
                })
            }
        }
    }
}

impl JobExecutor for IngestExecutor {
    fn descriptor(&self) -> JobExecutorDescriptor {
        JobExecutorDescriptor {
            kind: self.kind.clone(),
            // See the module doc: this runs on the daemon's own threads.
            isolation: Isolation::InProcess,
            parameters: json!({
                "type": "object",
                "title": INGEST_KIND,
                "description":
                    "Ingests one slice of a corpus into this node's partition directory for \
                     that corpus, using the node's own CorpusEngine and embedding model. It \
                     runs IN the daemon process — there is no subprocess and no sandbox — and \
                     it writes only under the engine's partition path. What limits it is \
                     consent (`Submit.allowed` intersected with `Offer.accept_from`), and for \
                     a corpus that is not mesh-shared the submitter ALSO needs an ephemeral \
                     ingest grant before the slice's source text may leave the owner's node.",
                "required": ["corpus_id", "recipe_id", "unit_id", "unit"],
                "additionalProperties": false,
                "properties": {
                    "corpus_id": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Names the partition directory written and the cancellation flag registered."
                    },
                    "recipe_id": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The recipe that says how to acquire, extract, chunk and embed. Must resolve on the donor."
                    },
                    "unit_id": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Stamped into every chunk's `unit_id` column so the merge can dedupe two donors' output for one slice."
                    },
                    "unit": {
                        "type": "object",
                        "description": "Which slice. `commonwealth_core::knowledge::WorkUnit`, externally tagged.",
                        "required": ["kind", "value"],
                        "properties": {
                            "kind": {"enum": ["HfFile", "JsonlShard", "JsonlRange"]},
                            "value": {}
                        }
                    }
                }
            }),
            examples: vec![
                ToolExample {
                    situation: "One HuggingFace parquet shard of a dataset corpus".to_string(),
                    call: json!({
                        "corpus_id": "sep",
                        "recipe_id": "sep",
                        "unit_id": 0,
                        "unit": {"kind": "HfFile", "value": 0}
                    }),
                },
                ToolExample {
                    situation: "An article range of a single-file JSONL corpus".to_string(),
                    call: json!({
                        "corpus_id": "wikipedia",
                        "recipe_id": "wikipedia",
                        "unit_id": 7,
                        "unit": {"kind": "JsonlRange", "value": {"start": 7000, "end": 8000}}
                    }),
                },
            ],
            // Not a promise about somebody else's command — a fact about this
            // one. `merge_shards` dedupes on `content_hash` and on
            // `(unit_id, source_doc_id)` (`corpus-engine/src/sharding.rs:780`),
            // which is the entire reason `unit_id` is threaded through
            // `ingest_with_overrides`. Re-running a unit after a lapsed lease
            // costs disk and time; it does not double a chunk.
            idempotency: Idempotency::Idempotent,
            // The cadence `auto_ingest::HEARTBEAT_INTERVAL` already derives,
            // from the same constant, for the same lease: one third of it, so
            // two heartbeats may be lost before the lease lapses. Derived here
            // rather than restated, so there is one number (ARCH §10.6).
            lease_interval_ms: LEASE_MS / 3,
            // Nobody has timed a generic slice — a Wikipedia range and an SEP
            // shard are minutes apart — and `None` is that absence reported
            // rather than defaulted to a number somebody would plan against
            // (ARCH §18.3).
            est_secs: None,
            // There are no exit codes: nothing is spawned. An empty list is
            // "not applicable", which is what it means.
            could_not_judge_exits: Vec::new(),
            // NEITHER arm names "the return value of an in-process call", and
            // this is the honest pick of the two rather than a silent one.
            // `ExitCode`'s SEMANTICS are this executor's exactly — success is
            // passed, non-success reaches no verdict — while `JudgementLine`
            // would claim a stdout protocol this executor does not have.
            // Widening the enum is a `kernel-types` change and a separate
            // decision.
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
            debug!(
                target: TRACE_TARGET,
                unit_kind = %unit.kind,
                executor_kind = %self.kind,
                "ingest:v1 executor refused a unit of another kind"
            );
            return Err(refusal);
        }
        if let Err(reason) = IngestPayload::parse(&unit.payload) {
            debug!(
                target: TRACE_TARGET,
                unit_hash = %unit.unit_hash,
                reason,
                "ingest:v1 payload refused"
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
