// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 1 per-section atlas extraction.
//!
//! `run_extract` is `svrn enrich extract` minus its `--help` gate: the CLI
//! wraps it, and the build orchestrator calls it directly for the Extract
//! step. The invocation vocabulary lives in [`args`], the token-spend sidecar
//! in [`tokens`].

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use corpus_engine::enrichment::pipeline::{
    checkpoint_processed_ids, collapse_phase1_checkpoint, read_phase1_checkpoint, ChapterManifest,
    ChapterSelection, Phase1Output, Phase1Progress, PhaseFailureKind, PhaseRunner,
    PipelineRegistry, RetryMode, RunOutputWriter,
};
use std::sync::Arc;

mod args;
pub mod tokens;

pub(crate) use args::read_latest_run;
pub use args::{parse_args, ParsedExtract};
use args::{read_failures_for_retry, LatestRun, SelectionArg};
pub use tokens::{read_token_snapshot, write_token_snapshot, TokenSpendRecord};

/// `--finalize` mode: read the per-chapter checkpoint, collapse it
/// to a `Phase1Output`, write the canonical run-file, and update the
/// cache (so phases 2+ see a complete questions set). Zero LLM calls.
async fn cmd_finalize(cfg: &EnrichConfig, checkpoint_path: &std::path::Path) -> i32 {
    use corpus_engine::enrichment::pipeline::{types::PipelinePhase, Phase1Output};
    let entries = match read_phase1_checkpoint(checkpoint_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "error: reading checkpoint {}: {e}",
                checkpoint_path.display()
            );
            return 1;
        }
    };
    if entries.is_empty() {
        eprintln!(
            "error: checkpoint {} is empty (or missing). Run `svrn enrich extract {} --full --resume` first to populate it.",
            checkpoint_path.display(),
            cfg.corpus_id
        );
        return 1;
    }
    let entries_total = entries.len();
    let (extracted, failures) = collapse_phase1_checkpoint(entries);
    println!(
        "  · finalize: {} entries → {} successes + {} failures",
        entries_total,
        extracted.len(),
        failures.len()
    );

    let output = Phase1Output {
        schema_version: Phase1Output::SCHEMA_VERSION,
        pipeline_id: cfg.pipeline_id.clone(),
        questions_by_chapter: extracted,
        failures,
        written_at: chrono::Utc::now().to_rfc3339(),
    };

    let runs = RunOutputWriter::new(paths::runs_dir(&cfg.corpus_id));
    let run_path = match runs.write(PipelinePhase::Questions, "finalize", &output) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: writing run-file: {e}");
            return 1;
        }
    };
    println!("  ✓ wrote run-file {}", run_path.display());

    // Update the cache — finalize is the canonical "promote
    // checkpoint to authoritative state" pass, so cache semantics
    // mirror a `--full` run.
    let cache = cfg.phase_cache();
    if let Err(e) = cache.write(PipelinePhase::Questions, &output) {
        eprintln!("error: updating cache: {e}");
        return 1;
    }
    println!("  ✓ updated cache/questions.json");
    0
}

/// `svrn enrich extract` minus its `--help` gate: Phase 1 per-section atlas
/// extraction for a corpus, on the `-> i32` exit-code contract both callers
/// need. The CLI wraps this ([`super::…`] is not available here — the wrapper
/// lives in `sovereign-cli-llm`), and the build orchestrator calls it directly
/// with synthesized args for the Extract step.
///
/// Argv is re-parsed here rather than taken pre-parsed so BOTH callers keep
/// the exact contract they had: the orchestrator hands it `--full` /
/// `--chapters …` strings, and the CLI wrapper validates first only so it can
/// print `HELP` on a parse error (help text is a host concern and stayed up).
/// The duplicate parse is argv only — pure, microseconds, no side effect.
pub async fn run_extract(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 2;
        }
    };

    // Load config.
    let cfg = match EnrichConfig::require(&parsed.corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let checkpoint_path = paths::runs_dir(&cfg.corpus_id).join("_phase1_checkpoint.jsonl");

    // Finalize mode: read checkpoint, collapse to Phase1Output,
    // write the canonical run-file, update cache. Zero LLM calls,
    // no daemon required.
    if parsed.finalize {
        return cmd_finalize(&cfg, &checkpoint_path).await;
    }

    // Probe daemon — fail fast if it's down.
    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon is not responding at {} — start it with `svrn daemon start` or equivalent",
            cfg.base_url
        );
        return 2;
    }

    // Build the pipeline + runner.
    let registry = PipelineRegistry::builtin();
    let pipeline = match super::pipeline_resolve::resolve_pipeline(&cfg) {
        Some(p) => p,
        None => {
            eprintln!(
                "error: unknown pipeline id in config: {} (known: {:?})",
                cfg.pipeline_id,
                registry.pipeline_ids()
            );
            return 1;
        }
    };

    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    // Negotiate structured-output mode against the daemon's advertised
    // OICP features before the first request (OICP v0.4). No-op for a
    // Sovereign daemon (advertises constraint:json_schema); matters
    // when `base_url` points at another OICP host.
    let client = client.discover_capabilities().await;
    // Phase D2 — grab the cumulative token ledger before consuming
    // the client into closures. The Arc<TokenUsageLedger> is shared
    // with the closures, so each chat call bumps it and the flusher
    // task below sees the running totals.
    let usage_ledger = client.usage_ledger();
    let (embed, chat, chat_with_tokens) = client.into_closures_with_tokens();

    let cache = cfg.phase_cache();
    let runs = RunOutputWriter::new(paths::runs_dir(&cfg.corpus_id));
    let runner = PhaseRunner::new(
        pipeline,
        embed,
        chat,
        cache,
        runs,
        paths::exemplars_dir(&cfg.corpus_id),
    )
    .with_chat_with_tokens(chat_with_tokens)
    .with_checkpoint_path(&checkpoint_path)
    .with_dry_run(parsed.dry_run);

    // Rebuild corpus state.
    let (inputs, manifest) = match rebuild_corpus_state(&cfg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let manifest = Arc::new(std::sync::Mutex::new(manifest));

    let selection = match parsed.selection {
        SelectionArg::Subset(ids) => {
            // Validate every id exists before dispatching.
            let known: std::collections::HashSet<_> =
                inputs.iter().map(|c| c.chapter_id.as_str()).collect();
            let missing: Vec<String> = ids
                .iter()
                .filter(|id| !known.contains(id.as_str()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                eprintln!(
                    "error: chapter id(s) not in manifest: {}",
                    missing.join(", ")
                );
                return 1;
            }
            ChapterSelection::Subset(ids)
        }
        SelectionArg::Full => ChapterSelection::Full,
        SelectionArg::RetryFailed => {
            let runs_dir = paths::runs_dir(&cfg.corpus_id);
            let source = read_failures_for_retry(&runs_dir, &checkpoint_path);
            match source {
                Ok(Some((path, ids))) if ids.is_empty() => {
                    println!("  · no failures in {} — nothing to retry.", path.display());
                    return 0;
                }
                Ok(Some((path, ids))) => {
                    // When `--terse` is combined with `--retry-failed`
                    // without an explicit chapter list, target the
                    // failures the terse variant is designed to
                    // recover: think-truncations AND parse-drifts.
                    // Both manifest as truncated output (the
                    // reasoning trace overflows the cap, leaving
                    // incomplete JSON), and terse addresses both via
                    // the bumped `max_output_tokens`. Transport
                    // failures (ChatError), empty extractions, and
                    // "other" shapes need different fixes — a plain
                    // retry for chat, inspection for empties — so
                    // filtering to the terse-recoverable kinds
                    // avoids burning budget on chapters the variant
                    // can't help.
                    let (targeted, filtered_out): (Vec<_>, Vec<_>) = if parsed.terse {
                        ids.into_iter().partition(|(_, kind)| {
                            matches!(
                                kind,
                                PhaseFailureKind::ThinkTruncated
                                    | PhaseFailureKind::ParseDrift
                                    | PhaseFailureKind::DeadlineExceeded
                            )
                        })
                    } else {
                        (ids, Vec::new())
                    };
                    if parsed.terse && !filtered_out.is_empty() {
                        println!(
                            "  · --terse filter: skipping {} failure(s) not classified as \
                             think-truncation or parse-drift (use --retry-failed without \
                             --terse to target them)",
                            filtered_out.len()
                        );
                    }
                    if targeted.is_empty() {
                        println!(
                            "  · no retry-eligible failures in the most recent run ({}) — nothing to retry.",
                            path.display()
                        );
                        return 0;
                    }
                    println!(
                        "  · retrying {} failed chapter(s) from {}",
                        targeted.len(),
                        path.display()
                    );
                    // Filter to ids actually present in the manifest
                    // (a manifest edit could have renumbered them).
                    let known: std::collections::HashSet<_> =
                        inputs.iter().map(|c| c.chapter_id.as_str()).collect();
                    let (present, missing): (Vec<_>, Vec<_>) = targeted
                        .into_iter()
                        .map(|(id, _)| id)
                        .partition(|id| known.contains(id.as_str()));
                    if !missing.is_empty() {
                        eprintln!(
                            "    · skipping {} id(s) no longer in manifest: {}",
                            missing.len(),
                            missing.join(", ")
                        );
                    }
                    if present.is_empty() {
                        eprintln!(
                            "error: every failed id from the last run is missing from the current manifest"
                        );
                        return 1;
                    }
                    ChapterSelection::RetryFailed(present)
                }
                Ok(None) => {
                    eprintln!(
                        "error: no prior run files under {} — run `svrn enrich extract {} --full` first",
                        runs_dir.display(),
                        cfg.corpus_id
                    );
                    return 1;
                }
                Err(msg) => {
                    eprintln!("error: {msg}");
                    return 1;
                }
            }
        }
    };

    // Resume filter — when --resume is set, read the per-chapter
    // checkpoint and remove ids the runner has already processed
    // (success OR failure) from the selection. After the filter,
    // a Full run may demote to Subset (if any chapter ids were
    // already done), which preserves the "skip processed work"
    // semantic without changing the schema.
    // RetryFailed already targets exactly the chapters we want to
    // re-attempt — those WILL be in the checkpoint as failures, so
    // applying the resume "skip processed" filter would remove every
    // candidate and exit. Suppress resume in that case.
    let resume_active = parsed.resume && !matches!(selection, ChapterSelection::RetryFailed(_));
    let selection = if resume_active {
        let entries = match read_phase1_checkpoint(&checkpoint_path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!(
                    "error: reading checkpoint {}: {e}",
                    checkpoint_path.display()
                );
                return 1;
            }
        };
        let done = checkpoint_processed_ids(&entries);
        if done.is_empty() {
            println!(
                "  · --resume: checkpoint at {} is empty (or missing); proceeding with full selection.",
                checkpoint_path.display()
            );
            selection
        } else {
            let before = match &selection {
                ChapterSelection::Full => inputs.len(),
                ChapterSelection::Subset(ids) | ChapterSelection::RetryFailed(ids) => ids.len(),
            };
            let remaining: Vec<String> = match &selection {
                ChapterSelection::Full => inputs
                    .iter()
                    .map(|c| c.chapter_id.clone())
                    .filter(|id| !done.contains(id))
                    .collect(),
                ChapterSelection::Subset(ids) | ChapterSelection::RetryFailed(ids) => ids
                    .iter()
                    .filter(|id| !done.contains(id.as_str()))
                    .cloned()
                    .collect(),
            };
            if remaining.is_empty() {
                println!(
                    "  · --resume: every selected chapter is already in the checkpoint ({} done). \
                     Run `svrn enrich extract {} --finalize` to write the canonical run-file.",
                    done.len(),
                    cfg.corpus_id
                );
                return 0;
            }
            println!(
                "  · --resume: checkpoint covers {} chapter(s) → running {} of {} originally selected.",
                done.len(),
                remaining.len(),
                before
            );
            // Preserve the original mode (Full vs RetryFailed)
            // semantically even though we narrow the id list:
            // RetryFailed kept its merge-into-cache behaviour, Full
            // did not. Demote Full to Subset only when something is
            // already done — otherwise keep Full so the cache is
            // overwritten on completion.
            match selection {
                ChapterSelection::Full => {
                    if remaining.len() == before {
                        ChapterSelection::Full
                    } else {
                        // We've already processed some chapters in a
                        // prior invocation — the in-memory result of
                        // THIS invocation alone wouldn't be a valid
                        // "full" snapshot to overwrite the cache
                        // with. Demote to Subset; the operator
                        // promotes via --finalize.
                        ChapterSelection::Subset(remaining)
                    }
                }
                ChapterSelection::Subset(_) => ChapterSelection::Subset(remaining),
                ChapterSelection::RetryFailed(_) => ChapterSelection::RetryFailed(remaining),
            }
        }
    } else {
        selection
    };

    // Build the retry mode passed to the runner. The terse variant
    // bumps the output cap to double the config's default so a
    // chapter that starved the default pass has room to emit JSON
    // after its shorter reasoning trace.
    let retry_mode = if parsed.terse {
        Some(RetryMode::Terse {
            max_output_tokens: cfg.max_output_tokens.saturating_mul(2).max(8192),
        })
    } else {
        None
    };

    println!(
        "  running phase 1 ({}) over {} chapter(s) — checkpoint: {}",
        selection.mode_label(),
        match &selection {
            ChapterSelection::Full => inputs.len(),
            ChapterSelection::Subset(ids) | ChapterSelection::RetryFailed(ids) => ids.len(),
        },
        checkpoint_path.display()
    );

    let progress = |ev: Phase1Progress<'_>| match ev {
        Phase1Progress::Start {
            total,
            exemplars_loaded,
        } => {
            println!("    · {exemplars_loaded} exemplar(s) loaded, {total} chapter(s) to process");
        }
        Phase1Progress::ChapterStart {
            i,
            total,
            chapter_id,
        } => {
            print!("    [{i}/{total}] {chapter_id}… ");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }
        Phase1Progress::ChapterDone {
            chapter_id: _,
            question_count,
            entity_count,
            declared_types,
        } => {
            // The declared types go first because they are the answer to the
            // only question an author with a declared ontology has while this
            // runs. A corpus that declares nothing prints the counts alone.
            if declared_types.is_empty() {
                println!("{entity_count} e, {question_count} q");
            } else {
                println!(
                    "{entity_count} e [{}], {question_count} q",
                    declared_types.join(", ")
                );
            }
        }
        Phase1Progress::ChapterFailed {
            chapter_id: _,
            reason,
        } => {
            println!("FAILED: {reason}");
        }
        Phase1Progress::ChapterPrompt { chapter_id, prompt } => {
            // The whole prompt, on stdout, as the runner composed it — this
            // is the payload of `--dry-run` and the reason it exists. JSON
            // for the schema so a probe can diff it against the daemon's
            // `inference_client: request body` line without re-deriving it.
            println!("dry-run");
            println!("──── {chapter_id} · system ────\n{}", prompt.system);
            println!("──── {chapter_id} · user ────\n{}", prompt.user);
            match &prompt.response_schema {
                Some(schema) => println!(
                    "──── {chapter_id} · response schema ────\n{}",
                    serde_json::to_string_pretty(schema)
                        .unwrap_or_else(|e| format!("<unserialisable: {e}>"))
                ),
                None => println!("──── {chapter_id} · response schema ────\n<none>"),
            }
        }
        Phase1Progress::Done {
            produced,
            failed,
            run_path,
        } => {
            println!(
                "  ✓ {produced} ok, {failed} failed — {}",
                run_path.display()
            );
        }
    };

    // Phase D2 — spawn a background flusher that writes the
    // running token snapshot to `<workspace>/_tokens.json` every
    // 30 s. A snapshot is written one final time after the runner
    // returns so the on-disk ledger reflects the run-end state.
    let tokens_path = paths::enrichment_root(&cfg.corpus_id).join("_tokens.json");
    let started_at_ms = sovereign_core::time::unix_millis();
    let flusher_ledger = std::sync::Arc::clone(&usage_ledger);
    let flusher_path = tokens_path.clone();
    let flusher_corpus = cfg.corpus_id.clone();
    let flusher = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // First tick is immediate — skip it; we want the next 30s
        // checkpoint, not a write before any work happens.
        tick.tick().await;
        loop {
            tick.tick().await;
            let _ = write_token_snapshot(
                &flusher_path,
                &flusher_corpus,
                started_at_ms,
                &flusher_ledger,
            );
        }
    });

    let result = match runner
        .phase_1_extract_questions_with_retry(&inputs, &selection, retry_mode, progress)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            flusher.abort();
            // Persist the partial spend so the operator can see how
            // many tokens were burned even on a failed run.
            let _ =
                write_token_snapshot(&tokens_path, &cfg.corpus_id, started_at_ms, &usage_ledger);
            eprintln!("error: phase 1 run failed: {e}");
            return 1;
        }
    };
    flusher.abort();
    if let Err(e) = write_token_snapshot(&tokens_path, &cfg.corpus_id, started_at_ms, &usage_ledger)
    {
        tracing::warn!(
            path = %tokens_path.display(),
            error = %e,
            "extract: token snapshot write failed (non-fatal)"
        );
    } else {
        let snap = usage_ledger.snapshot();
        if snap.calls > 0 {
            println!(
                "  · token spend: {} call(s), {} prompt + {} completion = {} total",
                snap.calls, snap.prompt_tokens, snap.completion_tokens, snap.total_tokens
            );
        }
    }

    // Merge characters_present back into the manifest for every
    // chapter the run succeeded on.
    {
        let mut m = manifest.lock().unwrap();
        for q in &result.output.questions_by_chapter {
            if q.thematic_carriers.is_empty() {
                continue;
            }
            let _ = m.merge_characters_present(&q.chapter_id, &q.thematic_carriers);
        }
        let manifest_path = paths::chapters_manifest_path(&cfg.corpus_id);
        if let Err(e) = m.save(&manifest_path) {
            eprintln!(
                "warning: saving updated chapter manifest {}: {e}",
                manifest_path.display()
            );
        }
    }

    if result.cache_updated {
        println!("  ✓ cache updated (cache/questions.json)");
    } else {
        match &selection {
            ChapterSelection::RetryFailed(_) => {
                println!("  · retry produced no new successes — cache/questions.json unchanged");
            }
            _ => {
                println!("  · subset run — cache NOT updated (re-run with --full to promote)");
            }
        }
    }
    if !result.failures.is_empty() {
        // Glassbox: name the failed ids and the exact retry command
        // so the operator can act without opening the run file. The
        // run file still has the raw response heads for deeper
        // debugging.
        eprintln!();
        eprintln!(
            "  ! {} chapter(s) failed — run file: {}",
            result.failures.len(),
            result.run_path.display()
        );
        for f in &result.failures {
            // reason is already a one-line string (runner trims the
            // raw-response excerpt to 200 chars).
            eprintln!("      · {:<12} {}", f.chapter_id, f.reason);
        }
        let ids: Vec<&str> = result
            .failures
            .iter()
            .map(|f| f.chapter_id.as_str())
            .collect();
        eprintln!();
        eprintln!("    Retry just these chapters:");
        eprintln!(
            "      svrn enrich extract {} --chapters {}",
            cfg.corpus_id,
            ids.join(",")
        );
        eprintln!("    Or, from the latest run file:");
        eprintln!("      svrn enrich extract {} --retry-failed", cfg.corpus_id);
        return 1;
    }
    0
}

/// Public entry point used by the integration test so it can exercise
/// the whole wire without spawning the binary. Lets the test inject
/// its own (`embed`, `chat`) pair instead of going through
/// `DaemonInferenceClient`.
#[cfg(test)]
pub async fn run_with_closures_for_test(
    corpus_id: &str,
    selection: ChapterSelection,
    embed: corpus_engine::types::EmbedFn,
    chat: corpus_engine::enrichment::pipeline::ChatCompletionFn,
) -> Result<(usize, bool), String> {
    let cfg = EnrichConfig::require(corpus_id).map_err(|e| e.to_string())?;
    let pipeline = super::pipeline_resolve::resolve_pipeline(&cfg)
        .ok_or_else(|| format!("unknown pipeline: {}", cfg.pipeline_id))?;
    let cache = cfg.phase_cache();
    let runs = RunOutputWriter::new(paths::runs_dir(corpus_id));
    let runner = PhaseRunner::new(
        pipeline,
        embed,
        chat,
        cache,
        runs,
        paths::exemplars_dir(corpus_id),
    );
    let (inputs, _manifest) = rebuild_corpus_state(&cfg).map_err(|e| e.to_string())?;
    let result = runner
        .phase_1_extract_questions(&inputs, &selection, |_| {})
        .await
        .map_err(|e| e.to_string())?;
    let _ = _manifest; // silence unused — the CLI path does manifest merging; tests don't need to
    Ok((
        result.output.questions_by_chapter.len(),
        result.cache_updated,
    ))
}

// Suppress unused-import warnings for `ChapterManifest` in non-test
// builds (it's only touched through `rebuild_corpus_state`'s return
// tuple, which the CLI explicitly names for clarity).
#[allow(dead_code)]
fn _hold_chapter_manifest(_: &ChapterManifest) {}
