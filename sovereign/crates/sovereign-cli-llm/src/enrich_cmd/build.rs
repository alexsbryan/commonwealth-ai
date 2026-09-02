// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich build` — the CLI surface for
//! [`sovereign_enrichment_build::build`].
//!
//! What stays here is everything about being a COMMAND LINE: the help text,
//! the `cmd_build` entry point, the two renderings of the progress stream
//! (human banners vs the `@progress` wire), and the daemon session the
//! Backfill step embeds through. The orchestrator itself — the plan, the
//! steps, the cache gates — moved down (ontology-v1 P0.5).
//!
//! The session is why the split is worth anything: resolving one needs
//! `sovereign-mesh`, `sovereign-runtime-recipe` and `sovereign-store`, and
//! doing it inside the orchestrator is what pinned the whole atlas build above
//! the `capabilities` layer. A host resolves a session; a capability is handed
//! one.

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use corpus_engine::enrichment::pipeline::{
    progress::wire, BuildStep, EnrichProgress, EnrichProgressFn, PipelineRegistry, SeedStrategy,
};
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_core::traits::InferenceProvider;
use std::sync::Arc;

pub use sovereign_enrichment_build::build::*;

const HELP: Help = Help {
    command: "svrn enrich build",
    summary: "Run the full atlas enrichment flow for a corpus in one command.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich build <corpus-id> [--chapters <ids> | --full] [--skip <step>...] [--dry-run]",
        ),
        HelpSection::Flags(&[
            (
                "--chapters <ids>",
                "Comma-separated chapter ids for Phase 1 (e.g. sec_0001,sec_0002). \
                 Subset runs promote the run output into cache so downstream steps \
                 have inputs. Default: --full.",
            ),
            (
                "--full",
                "Run Phase 1 on every section in the corpus manifest. Updates \
                 cache/questions.json directly.",
            ),
            (
                "--skip <step>",
                "Skip a step by name. Accepts: seed, extract, cluster, name, resolve, \
                 tensions, gaps, configure, report, backfill. Repeatable.",
            ),
            (
                "--dry-run",
                "Print the planned step sequence and exit without running anything.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich build brothers_karamazov --full",
                "Full end-to-end build on the whole corpus.",
            ),
            (
                "svrn enrich build process_philosophy --chapters sec_0001,sec_0002,sec_0003",
                "Subset build — useful for iterating on a tiny validation slice.",
            ),
            (
                "svrn enrich build bk --skip configure",
                "Skip the LLM Phase 8 configuration step (fastest path to resolved atlas + report).",
            ),
        ]),
        HelpSection::Notes(
            "Requires `svrn enrich init <corpus>` first. Phase 8 (configure) is \
             skipped automatically if the pipeline hasn't opted in via \
             `runs_configuration_phase()`. Any step failure stops the flow with that \
             step's exit code. The last step, `backfill`, embeds the resolved atoms \
             into `atlas/atoms_ann.lance` through the daemon's embed model so the \
             corpus grounds immediately; it is probed before the first step runs, \
             and `--skip backfill` builds without grounding (`svrn atlas backfill-ann \
             <corpus>` seeds it later).",
        ),
    ],
};
pub async fn cmd_build(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    // Two renderings of one event stream, and the parent picks.
    //
    // A human gets banners. A parent process that set
    // `SOVEREIGN_ENRICH_PROGRESS=json` gets one `@progress {…}` line per
    // event on stdout — the wire `corpus_engine::…::progress::wire` declares,
    // which is what `sovereign_tools::enrich` reads. Before 2026-08-26 there
    // was no second rendering and that runner regex-matched THESE banners, so
    // rewording one silently changed what the desktop believed about a running
    // enrichment (TOPOLOGY §9.3).
    //
    // The env var is read here, at the ONE place that decides how to render.
    let machine = std::env::var(wire::REQUEST_ENV)
        .map(|v| v == wire::REQUEST_VALUE)
        .unwrap_or(false);
    let progress: EnrichProgressFn = if machine {
        Arc::new(|evt: EnrichProgress| println!("{}", wire::encode(&evt)))
    } else {
        Arc::new(|evt: EnrichProgress| print_cli_event(&evt))
    };
    build_with_progress(&parsed, Some(progress)).await
}
/// Library entry point: run the full `enrich build` flow with an
/// optional streaming progress callback.
///
/// The callback receives typed `EnrichProgress` events in strict
/// order (`BuildStart` → step events → `Complete` or `Aborted`).
/// `None` runs silently — useful for integration tests that only
/// care about the exit code.
///
/// Returns the same exit code `cmd_build` would: 0 on success,
/// nonzero when any enabled step fails.
///
/// Shared by the CLI (`cmd_build`) and, through
/// [`build_with_progress_with_embedder`], the daemon's in-process atlas
/// build (ontology-v1 P0.4). Adding a per-step side effect means editing
/// here once rather than across frontends.
///
/// The Backfill step's embed provider is resolved the CLI way — a daemon
/// session — and ONLY when the plan actually runs that step
/// ([`ParsedBuild::needs_backfill_embedder`]). `--skip backfill` exists so a
/// build can run with no daemon at all; resolving eagerly here would take
/// that away.
pub async fn build_with_progress(parsed: &ParsedBuild, progress: Option<EnrichProgressFn>) -> i32 {
    let embedder = match parsed.needs_backfill_embedder() {
        Ok(false) => None,
        Ok(true) => match backfill_session_embedder().await {
            Ok(e) => Some(e),
            Err(msg) => {
                eprintln!("error: {msg}");
                return 1;
            }
        },
        Err((code, msg)) => {
            eprintln!("error: {msg}");
            return code;
        }
    };
    build_with_progress_with_embedder(parsed, progress, embedder, None).await
}
/// Render a single progress event on the CLI (stdout) in the same
/// banner shape operators have seen since Landing 3. Desktop
/// callers don't use this — they emit the structured event
/// straight through.
fn print_cli_event(evt: &EnrichProgress) {
    match evt {
        EnrichProgress::BuildStart {
            corpus_id,
            pipeline_id,
            steps,
            auto_skipped,
        } => {
            println!("=== enrich build — {corpus_id} ===");
            if !auto_skipped.is_empty() {
                let labels: Vec<&str> = auto_skipped.iter().map(|s| s.id()).collect();
                println!(
                    "  pipeline `{pipeline_id}` auto-skips: {}",
                    labels.join(", ")
                );
            }
            println!("  {} step(s) planned", steps.len());
            for (i, s) in steps.iter().enumerate() {
                println!("    {}. {}", i + 1, s.id());
            }
            println!();
        }
        EnrichProgress::StepStart {
            step,
            ordinal,
            total,
            ..
        } => {
            println!("─── [{ordinal}/{total}] {} ───", step.id());
        }
        EnrichProgress::StepDone { .. } => {
            println!();
        }
        EnrichProgress::ChapterProgress { .. } | EnrichProgress::ChapterFailed { .. } => {
            // The extract step prints its own per-chapter lines;
            // avoid double-printing. These events exist for
            // non-CLI consumers.
        }
        EnrichProgress::StepFailed { .. } | EnrichProgress::Aborted { .. } => {
            // The caller already prints a detailed error to
            // stderr via eprintln! above; nothing to add here.
        }
        EnrichProgress::SpawnFailed { corpus_id, message } => {
            // This variant is emitted only by the desktop's
            // subprocess-spawn path — the CLI's own in-process
            // orchestration can't hit it. Print a line anyway so
            // this branch is exhaustive and future in-process
            // spawn scenarios (e.g. a sub-step that shells out)
            // would surface correctly.
            eprintln!("error: could not start build for {corpus_id}: {message}");
        }
        EnrichProgress::Cancelled { corpus_id, at_step } => {
            // Same reason as `SpawnFailed` above: the CLI can't
            // emit this today (no cancellation channel in the
            // in-process path), but the match is exhaustive so a
            // future CLI flag like `--cancel-after-step <N>`
            // wouldn't require a parser update.
            let step_label = at_step.map(|s| s.id()).unwrap_or("none");
            eprintln!("cancelled build for {corpus_id} (was at step: {step_label})");
        }
        EnrichProgress::Complete { corpus_id, .. } => {
            println!("=== build complete — {corpus_id} ===");
        }
    }
}
/// The CLI's embed provider for the Backfill step: a daemon-backed session.
/// `parse_globals(&[])` resolves the daemon exactly as `svrn atlas
/// backfill-ann` does — the same bootstrap, not a second one (ARCH §19).
///
/// It does NOT probe. The one `embed_query("probe")` lives in
/// [`probe_embedder`], on the path both callers share, so the CLI and the
/// daemon spend exactly one probe each and neither spends two.
async fn backfill_session_embedder() -> Result<Arc<dyn InferenceProvider>, String> {
    let (globals, _) = parse_globals(&[])?;
    let session = build_session(&globals).await.map_err(|e| {
        format!(
            "backfill: could not reach the daemon ({e}); start it, or pass \
             `--skip backfill` to build without grounding"
        )
    })?;
    Ok(session.inference)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one invariant that can only be checked HERE.
    ///
    /// `HELP` is this host's `const` and repeats the `--skip` vocabulary as a
    /// literal; `Step::all()` is the capability crate's. Nothing below can see
    /// both, so when the orchestrator moved down (ontology-v1 P0.5) this test
    /// came UP with the help text rather than being dropped — a split that
    /// silently retires the test holding two halves in agreement is how the
    /// two halves start disagreeing.
    #[test]
    fn help_names_every_skippable_step() {
        let flags = HELP
            .sections
            .iter()
            .find_map(|s| match s {
                HelpSection::Flags(entries) => Some(entries),
                _ => None,
            })
            .expect("HELP has a Flags section");
        let (_, skip_text) = flags
            .iter()
            .find(|(flag, _)| flag.starts_with("--skip"))
            .expect("HELP documents --skip");
        for step in Step::all() {
            assert!(
                skip_text.contains(step.label()),
                "HELP --skip text omits `{}`: {skip_text}",
                step.label()
            );
        }
    }
}
