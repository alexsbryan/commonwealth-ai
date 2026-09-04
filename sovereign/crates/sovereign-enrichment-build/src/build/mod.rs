// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas build orchestrator: the driver loop.
//!
//! `build_with_progress_with_embedder` is the whole public entry point — load
//! capabilities, shape the plan, then run each enabled step in order, emitting
//! a typed `EnrichProgress` event at every transition and stopping at the
//! first failure. What to run lives in [`plan`], how to run one in [`steps`],
//! and the one step with a retry policy in [`extract_step`].

use corpus_engine::enrichment::pipeline::{
    progress::wire, BuildStep, EnrichProgress, EnrichProgressFn, PipelineRegistry, SeedStrategy,
};
use corpus_engine::EmbedFn;
use sovereign_contracts::launch::EXIT_CANCELLED;

mod extract_step;
mod plan;
mod steps;

// The build's PUBLIC surface, and the whole of it. `sovereign-cli-llm`'s shim
// re-exports this module with a glob, and the daemon reaches
// `build_with_progress_with_embedder` + `ParsedBuild` through the crate root,
// so anything `pub` here is a promise to two other crates.
pub use plan::{parse_args, ParsedBuild, Selection, Step};

// Internal to the driver loop. Plain `use`, not `pub use`: these are
// `pub(super)` in their own modules, which is "visible inside `build`" — the
// narrowest scope that lets the loop see them. Re-exporting them `pub(crate)`
// would both widen them past their declaration (E0365) and put the plan's
// internals on a surface no caller needs.
use plan::{load_pipeline_capabilities, PipelineCapabilities, Plan};
use steps::{probe_embedder, run_step, StepFailure, StepOutcome};

/// [`build_with_progress`] with the Backfill step's embedder supplied by the
/// caller. The daemon passes its own, so an in-process build does not open an
/// HTTP session to itself (and does not boot a second `Runtime` inside the
/// daemon to get one). `None` is the CLI path: build and probe a daemon
/// session. Either way the embedder is probed with one call before the first
/// step runs.
///
/// The parameter is an [`EmbedFn`] — corpus-engine's embed closure — where it
/// was `Arc<dyn InferenceProvider>` (order ei-5a-build-cut). The trait offered
/// a dozen methods and this crate called exactly one of them, `embed_query`,
/// while dragging llama.cpp through the orchestrator's whole closure to do it.
/// Callers holding a provider adapt with
/// `sovereign_core::embed_fn::inference_to_embed_query_fn` — the QUERY-side
/// adapter, which keeps the seed table in the same vector space
/// `atlas_navigate_ann` queries it in.
///
/// `cancel` is the daemon driver's flag (`sovereign_tools::enrich::
/// CancellationFlag`). It is polled BETWEEN steps: a step already running
/// finishes (an extract can take thirty minutes), then the build emits
/// `Cancelled { at_step }` and returns [`EXIT_CANCELLED`] (the driver that
/// owns the flag also owns the code it reads back). Without this the UI's
/// cancel button would fire a flag nothing read (§18.3).
pub async fn build_with_progress_with_embedder(
    parsed: &ParsedBuild,
    progress: Option<EnrichProgressFn>,
    embedder: Option<EmbedFn>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> i32 {
    let emit = |evt: EnrichProgress| {
        if let Some(cb) = progress.as_ref() {
            cb(evt);
        }
    };

    // Load pipeline capabilities once. Failure surfaces before any
    // progress event so an invalid corpus_id doesn't emit a
    // spurious BuildStart.
    let capabilities = match load_pipeline_capabilities(&parsed.corpus_id) {
        Ok(c) => c,
        Err((code, msg)) => {
            eprintln!("error: {msg}");
            return code;
        }
    };

    let plan = Plan::new(parsed, &capabilities);
    if parsed.dry_run {
        plan.print_dry_run();
        return 0;
    }

    // Fail fast for the LAST step. Backfill needs the daemon's embed slot;
    // proving it answers costs one embed call here, and discovering it does
    // not after thirty minutes of extraction would waste the run. The same
    // session is the step's provider, so the daemon is resolved once.
    let backfill_embedder: Option<EmbedFn> = if plan.enabled.contains(&Step::Backfill) {
        let Some(e) = embedder else {
            // Fail fast rather than thirty minutes later inside the step:
            // no provider reached a build that plans Backfill is a wiring
            // error, and it is reported, never defaulted (ARCH §18.3).
            eprintln!(
                "error: backfill: no embed provider was wired for this build; run \
                     `svrn atlas backfill-ann {}`",
                parsed.corpus_id
            );
            return 1;
        };
        match probe_embedder(e).await {
            Ok(e) => Some(e),
            Err(msg) => {
                eprintln!("error: {msg}");
                return 1;
            }
        }
    } else {
        None
    };
    let embedder: Option<&EmbedFn> = backfill_embedder.as_ref();

    emit(EnrichProgress::BuildStart {
        corpus_id: parsed.corpus_id.clone(),
        pipeline_id: capabilities.pipeline_id.clone(),
        steps: plan.enabled_steps().map(Step::to_build_step).collect(),
        auto_skipped: plan
            .auto_skipped
            .iter()
            .map(|s| s.to_build_step())
            .collect(),
    });

    let total = plan.enabled_steps().count();
    for (i, step) in plan.enabled_steps().enumerate() {
        let ordinal = i + 1;
        let build_step = step.to_build_step();
        if cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst))
        {
            eprintln!(
                "cancelled build for {} before step `{}`",
                parsed.corpus_id,
                step.label()
            );
            emit(EnrichProgress::Cancelled {
                corpus_id: parsed.corpus_id.clone(),
                at_step: Some(build_step),
            });
            return EXIT_CANCELLED;
        }
        emit(EnrichProgress::StepStart {
            corpus_id: parsed.corpus_id.clone(),
            step: build_step,
            ordinal,
            total,
        });
        let outcome = match run_step(step, parsed, embedder).await {
            Ok(o) => o,
            Err(failure) => {
                let code = failure.exit_code;
                eprintln!();
                eprintln!("error: {}. Build stopped.", failure.message);
                emit(EnrichProgress::StepFailed {
                    corpus_id: parsed.corpus_id.clone(),
                    step: build_step,
                    message: failure.message,
                    exit_code: code,
                });
                emit(EnrichProgress::Aborted {
                    corpus_id: parsed.corpus_id.clone(),
                    failed_step: build_step,
                    exit_code: code,
                });
                return code;
            }
        };
        emit(EnrichProgress::StepDone {
            corpus_id: parsed.corpus_id.clone(),
            step: build_step,
            // Supplied by the step, never by this loop. A step that
            // ran, a step that was skipped because its output was
            // cached, and a step that found nothing are three
            // different outcomes; until 2026-08-26 all three put the
            // same fabricated `"<step> complete"` on the wire.
            summary: outcome.summary(),
        });
    }

    emit(EnrichProgress::Complete {
        corpus_id: parsed.corpus_id.clone(),
        steps_completed: total,
    });
    0
}
