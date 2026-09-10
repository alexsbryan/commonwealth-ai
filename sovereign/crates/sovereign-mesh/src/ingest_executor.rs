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

use std::collections::BTreeSet;

use commonwealth_core::ids::HandoffId;
use commonwealth_core::knowledge::{HandoffPhase, UnitId, WorkUnit, LEASE_MS, MAX_UNIT_ATTEMPTS};
use commonwealth_work::actor::ActorKey;
use commonwealth_work::executor::{subject_of, ExecuteFuture, JobContext, JobError, JobExecutor};
use commonwealth_work::projection::{WorkHandoff, WorkProjection, WorkUnitStatus};
use commonwealth_work::refusal::WorkRefusal;
use corpus_engine::{CorpusEngine, IngestProgress, ProgressCallback};
use kernel_types::quality::VerdictSource;
use kernel_types::{Judgement, Reason};
use kernel_types::{NodeId, Server};
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
            // The variant names are PascalCase because
            // `commonwealth_core::knowledge::WorkUnit` is
            // `#[serde(tag = "kind", content = "value")]` with no
            // `rename_all` (`knowledge.rs:318`). This text said `hf-file` /
            // `jsonl-shard` / `jsonl-range` until 2026-09-09 — three spellings
            // that cannot parse, taught on the FAILURE path, which is the one
            // place a reader is already stuck (cw-lift 5g D1 found it live).
            format!(
                "an `{INGEST_KIND}` payload is \
                 {{\"corpus_id\": …, \"recipe_id\": …, \"unit_id\": …, \"unit\": \
                 {{\"kind\": \"HfFile\"|\"JsonlShard\"|\"JsonlRange\", \"value\": …}}}} — {e}"
            )
        })?;
        if parsed.corpus_id.trim().is_empty() {
            return Err("`corpus_id` is empty — a slice with no corpus names no \
                        partition directory to write into"
                .to_string());
        }
        if parsed.recipe_id.trim().is_empty() {
            return Err(
                "`recipe_id` is empty — a slice with no recipe cannot say how to \
                        acquire, extract or embed anything"
                    .to_string(),
            );
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

// -----------------------------------------------------------------
// Reading the fold for coverage — cw-lift 5g part 2
// -----------------------------------------------------------------

/// What the `work` fold knows about one corpus that local disk does not.
///
/// # Why this exists at all
///
/// `auto_ingest`'s tick loop already merges stranded partitions: it walks
/// [`CorpusEngine::corpora_with_stranded_partitions`], gates against
/// `active_ingests`, prefers a peer's healthier canonical via
/// `find_best_peer_canonical`, and otherwise calls
/// [`commonwealth_api::auto_recover::try_recover_stranded_partitions`] under a
/// per-corpus cooldown. All of that is reused unchanged.
///
/// The ONE thing that loop cannot know is who else worked on the corpus. It
/// derives participants from what is on local disk and from gossip, so a
/// donor's partition that was never pulled here is not merged and is not even
/// missed — which is how a two-donor ingest produced a canonical holding half
/// the data while reporting `Recovered`
/// (`tests/main/fold_ingest_cross_node_merge_e2e.rs`).
///
/// The fold knows. This is the read that tells it.
///
/// # The identity rule, which is the subtle part
///
/// Two fields on a completed unit name the donor and they are NOT
/// interchangeable. [`WorkUnitStatus::Complete::lessee`] is an [`ActorKey`] —
/// the Ed25519 key ADMISSION VERIFIED. `provenance.host` is a [`Server`]
/// carrying a [`NodeId`] and is SELF-REPORTED by whoever ran the unit.
/// `work_donor.rs:941-944` already draws exactly this line.
///
/// So: **the verified `lessee` decides whether a contribution counts, and the
/// self-reported `host` only says where to look for it.** [`Self::expected`]
/// counts distinct VERIFIED actors, never hosts — counting hosts would let one
/// donor inflate coverage by naming extra nodes, which is the §18.1 smell "a
/// guard asserting on a field the subject supplies".
#[derive(Debug, PartialEq, Eq)]
pub struct FoldCoverage {
    /// The handoff this coverage is about. Carried because the merge is keyed
    /// on it and re-deriving it at the call site would be a second lookup of
    /// something already decided here.
    pub handoff_id: HandoffId,
    /// Distinct nodes to pull a partition from, in a stable order. Derived
    /// from the self-reported half, so this locates work — it never decides
    /// that work happened.
    pub nodes: Vec<NodeId>,
    /// Distinct VERIFIED contributors. The coverage denominator, and the
    /// number `MergePlan::expected_partitions` is armed from.
    pub expected: usize,
    /// Units whose attempts are spent — nobody ingested that slice and nobody
    /// will. Reported rather than defaulted (ARCH §18.3): a merge may still
    /// run, but this corpus can never be called complete.
    pub abandoned: Vec<String>,
}

impl FoldCoverage {
    /// Whether some slice will never exist. A merge may proceed; the corpus
    /// may not be called complete.
    pub fn is_partial(&self) -> bool {
        !self.abandoned.is_empty()
    }
}

/// The fold's coverage for `corpus_id`, if this node LEADS a terminal
/// `ingest:v1` handoff for it.
///
/// `None` means "the fold has nothing to say about this corpus" and the caller
/// must fall through to its existing disk-and-gossip behaviour unchanged — a
/// corpus ingested the legacy way, or one whose handoff another node leads,
/// must not be affected by this read.
///
/// # Why the submitter is the leader
///
/// [`WorkHandoff::submitter`] is retained from ADMISSION and never from the
/// payload (`projection.rs:274`), so every node folding the same journal
/// derives the same leader and exactly one acts. That is the property the
/// legacy path bought with `handoff.merge_leader`, for free and without a
/// second decider (ARCH §10.6).
///
/// Pure: no I/O and no clock beyond the `now_ms` it is handed, so the whole of
/// the "who counts" decision is testable without a rail or a corpus.
pub fn fold_coverage_for(
    proj: &WorkProjection,
    self_key: &ActorKey,
    corpus_id: &str,
    now_ms: u64,
) -> Option<FoldCoverage> {
    // Parsed once per call, not per handoff. Fallible only if the literal this
    // module owns stops parsing, which `the_kind_literal_this_module_owns_parses`
    // goes red on first — the same reasoning, and the same call, as
    // `IngestExecutor::new`.
    #[allow(clippy::expect_used)]
    let want = JobKind::parse(INGEST_KIND).expect("`ingest:v1` is a valid JobKind by construction");

    for (handoff_id, handoff) in &proj.handoffs {
        if handoff.kind != want {
            continue;
        }
        // The leader decision, made visible. Every node folds this same
        // journal and all but one of them take this branch, so a silent
        // `continue` here is the single most load-bearing invisible decision
        // in the collector (ARCH §9.1) — and "two nodes both merged" and
        // "no node merged" are indistinguishable after the fact without it.
        // Pre-registered as B3's instrument in
        // `quality/campaigns/cw-lift-5g-part2-prereg.md`: it names the
        // decision AND the submitter it compared against.
        if &handoff.submitter != self_key {
            tracing::debug!(
                handoff = %handoff_id,
                submitter = %handoff.submitter,
                self_key = %self_key,
                "fold_coverage_for: declining — this node is not the submitter of this \
                 ingest handoff, so another node leads its merge"
            );
            continue;
        }
        if !matches!(handoff.phase_at(now_ms), HandoffPhase::Complete) {
            continue;
        }
        if corpus_of(handoff).as_deref() != Some(corpus_id) {
            continue;
        }

        let mut actors: BTreeSet<ActorKey> = BTreeSet::new();
        let mut nodes: BTreeSet<NodeId> = BTreeSet::new();
        let mut abandoned: Vec<String> = Vec::new();
        for (unit_hash, unit) in &handoff.units {
            match unit.status_at(now_ms) {
                WorkUnitStatus::Complete {
                    lessee, provenance, ..
                } => {
                    actors.insert(lessee);
                    // `Server::Local` is the donor describing ITS machine, not
                    // ours, so the id is not resolvable from here. It still
                    // counts toward `expected` — dropping it would let an
                    // unlocatable contribution satisfy the coverage guard.
                    if let Server::Peer { node, .. } = provenance.host {
                        nodes.insert(node);
                    }
                }
                WorkUnitStatus::Failed { .. } => abandoned.push(unit_hash.clone()),
                // Queued and Leased cannot occur: the phase read `Complete`,
                // which is exactly "none of either".
                _ => {}
            }
        }
        return Some(FoldCoverage {
            handoff_id: *handoff_id,
            nodes: nodes.into_iter().collect(),
            expected: actors.len(),
            abandoned,
        });
    }
    None
}

/// The corpus every unit in `handoff` belongs to.
///
/// Read off the first unit's payload rather than carried on the handoff,
/// because [`IngestPayload::corpus_id`] is already the one speller of it
/// (ARCH §10.6). `None` when the handoff has no units or the first will not
/// parse — reported by the caller, never defaulted to a corpus name that would
/// then be merged into.
fn corpus_of(handoff: &WorkHandoff) -> Option<String> {
    let unit = handoff.units.values().next()?;
    IngestPayload::parse(&unit.unit.payload)
        .ok()
        .map(|p| p.corpus_id)
}

#[cfg(test)]
mod tests;
