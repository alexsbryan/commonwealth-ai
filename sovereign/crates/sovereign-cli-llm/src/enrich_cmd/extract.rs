// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich extract <corpus> [--chapters ...|--full]` — phase 1.
//!
//! Rebuilds chapter inputs from the pinned source file, constructs a
//! `PhaseRunner` with the daemon-backed embed + chat closures, runs
//! `phase_1_extract_questions`, merges `characters_present` back into
//! the chapter manifest, and prints a one-line summary.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{
    checkpoint_processed_ids, collapse_phase1_checkpoint, read_phase1_checkpoint, ChapterManifest,
    ChapterSelection, Phase1Output, Phase1Progress, PhaseFailureKind, PhaseRunner,
    PipelineRegistry, RetryMode, RunOutputWriter,
};

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich extract",
    summary: "Run phase 1 (per-chapter question extraction) on a subset or the full corpus.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich extract <corpus-id> [--chapters <id1,id2,...> | --full | --retry-failed] [--terse]",
        ),
        HelpSection::Flags(&[
            ("--chapters <ids>", "Comma-separated chapter ids (e.g. sec_0001,sec_0003). Subset runs do NOT update the cache."),
            ("--full", "Run on every chapter in the manifest. Updates cache/questions.json."),
            (
                "--retry-failed",
                "Re-run only the chapters that failed in the most recent run. Successful \
                 recoveries are merged into cache/questions.json (matching chapters \
                 overwritten; those failures dropped from the cached failures list). \
                 Errors if no prior run file exists.",
            ),
            (
                "--terse",
                "Use the terse Phase 1 prompt variant and double the configured \
                 max_output_tokens. Combinable with --chapters or --retry-failed. When paired \
                 with --retry-failed, auto-filters to failures the terse variant can recover \
                 (think-truncation and parse-drift — both benefit from the bumped output \
                 budget). Successful retries merge into cache/questions.json.",
            ),
            (
                "--resume",
                "Crash-resilient resume. Reads the per-chapter JSONL checkpoint at \
                 runs/_phase1_checkpoint.jsonl and skips chapter ids already recorded \
                 there (success OR failure). The runner appends to the checkpoint after \
                 every chapter completes, so a kill / crash / power loss mid-run loses \
                 at most one chapter. Combine with --full for long Wikipedia-scale Tier-2 \
                 runs.",
            ),
            (
                "--finalize",
                "Read runs/_phase1_checkpoint.jsonl, write a canonical run-file from it, \
                 and (when applicable) update cache/questions.json. Use after a long \
                 --resume sequence has covered every chapter — no LLM calls fired by this \
                 mode. Mutually exclusive with --chapters / --full / --retry-failed.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich extract ak --chapters sec_0001,sec_0011,sec_0023",
                "Fast-loop subset run (2-3 min). Output written to runs/.",
            ),
            (
                "svrn enrich extract ak --full",
                "Full-corpus run. Updates cache/questions.json — consumed by phases 2+.",
            ),
            (
                "svrn enrich extract ak --retry-failed",
                "Reprocess the chapters that failed in the last run (parse errors, transient chat failures).",
            ),
            (
                "svrn enrich extract bk --retry-failed --terse",
                "Recover chapters whose default pass failed with <think> truncation, using the terse prompt variant.",
            ),
        ]),
        HelpSection::Notes(
            "Requires `svrn enrich init` first. Daemon must be running at localhost:9741.",
        ),
    ],
};

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

pub async fn cmd_extract(args: &[String]) -> i32 {
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
            "error: daemon is not responding at {} — start it with `commonwealth daemon start` or equivalent",
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
    .with_checkpoint_path(&checkpoint_path);

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
        } => {
            println!("{question_count} q");
        }
        Phase1Progress::ChapterFailed {
            chapter_id: _,
            reason,
        } => {
            println!("FAILED: {reason}");
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
    let started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
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

#[derive(Debug)]
enum SelectionArg {
    Subset(Vec<String>),
    Full,
    /// Resolved later (needs the corpus runs/ directory) into a
    /// `Subset` over the chapter ids that failed in the latest run.
    RetryFailed,
}

#[derive(Debug)]
struct ParsedExtract {
    corpus_id: String,
    selection: SelectionArg,
    /// Set by `--terse`. Requests the terse Phase 1 prompt variant
    /// + an optional `max_output_tokens` bump. Combinable with
    /// `--chapters` or `--retry-failed` but not with `--full` — a
    /// terse pass is by design a recovery run, not a full-corpus
    /// run.
    terse: bool,
    /// Set by `--resume`. Reads the per-chapter checkpoint and skips
    /// chapter ids already recorded there. Compatible with `--full`,
    /// `--chapters`, and `--retry-failed`; the resume filter applies
    /// to whichever selection mode is in effect.
    resume: bool,
    /// Set by `--finalize`. Read-only mode: reconstructs a run-file
    /// (and updates the cache when --full-equivalent semantics
    /// apply) from the checkpoint. Mutually exclusive with the
    /// selection flags.
    finalize: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedExtract, String> {
    let mut corpus_id: Option<String> = None;
    let mut chapters_csv: Option<String> = None;
    let mut full = false;
    let mut retry_failed = false;
    let mut terse = false;
    let mut resume = false;
    let mut finalize = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--chapters" => {
                chapters_csv = Some(
                    args.get(i + 1)
                        .ok_or("--chapters requires a comma-separated list".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--full" => {
                full = true;
                i += 1;
            }
            "--retry-failed" => {
                retry_failed = true;
                i += 1;
            }
            "--terse" => {
                terse = true;
                i += 1;
            }
            "--resume" => {
                resume = true;
                i += 1;
            }
            "--finalize" => {
                finalize = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }

    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let selection_count = chapters_csv.is_some() as u8 + full as u8 + retry_failed as u8;
    if selection_count > 1 {
        return Err("--chapters, --full, and --retry-failed are mutually exclusive".into());
    }
    if finalize {
        if selection_count > 0 || terse || resume {
            return Err(
                "--finalize is a read-only checkpoint-to-runfile pass; do not pair with \
                 --chapters / --full / --retry-failed / --terse / --resume"
                    .into(),
            );
        }
        // Finalize mode short-circuits the whole flow; we use Full
        // as a placeholder selection so the rest of the parser
        // succeeds.
        return Ok(ParsedExtract {
            corpus_id,
            selection: SelectionArg::Full,
            terse: false,
            resume: false,
            finalize: true,
        });
    }
    if terse && full {
        return Err(
            "--terse is a recovery pass; pair it with --retry-failed or --chapters, \
             not --full"
                .into(),
        );
    }
    let selection = if retry_failed {
        SelectionArg::RetryFailed
    } else if full {
        SelectionArg::Full
    } else if let Some(csv) = chapters_csv {
        let ids: Vec<String> = csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if ids.is_empty() {
            return Err("--chapters must list at least one id".into());
        }
        SelectionArg::Subset(ids)
    } else {
        return Err(
            "must provide one of --chapters <ids>, --full, --retry-failed, or --finalize".into(),
        );
    };
    Ok(ParsedExtract {
        corpus_id,
        selection,
        terse,
        resume,
        finalize: false,
    })
}

/// Resolve the failure list `--retry-failed` should target. Prefers
/// the per-chapter checkpoint (live, written after every chapter)
/// over the last finalised run-file (only written by `--finalize` or
/// pre-checkpoint legacy runs). When neither has anything, returns
/// `Ok(None)`.
///
/// The first element of the returned tuple is a path the caller can
/// surface to the operator so they know where the failure list came
/// from — the checkpoint or a specific run-file.
fn read_failures_for_retry(
    runs_dir: &std::path::Path,
    checkpoint_path: &std::path::Path,
) -> Result<Option<(PathBuf, Vec<(String, PhaseFailureKind)>)>, String> {
    use corpus_engine::enrichment::pipeline::{read_phase1_checkpoint, Phase1CheckpointEntry};

    if checkpoint_path.exists() {
        let entries = read_phase1_checkpoint(checkpoint_path)
            .map_err(|e| format!("read checkpoint {}: {e}", checkpoint_path.display()))?;
        if !entries.is_empty() {
            // Last-write-wins on chapter_id: a chapter that failed
            // in an earlier entry but succeeded later is no longer a
            // candidate. Walk in order, track the latest verdict per
            // id, keep only the failures.
            let mut latest_failure: std::collections::HashMap<String, PhaseFailureKind> =
                std::collections::HashMap::new();
            for entry in entries {
                match entry {
                    Phase1CheckpointEntry::Success { chapter_id, .. } => {
                        latest_failure.remove(&chapter_id);
                    }
                    Phase1CheckpointEntry::Failure {
                        chapter_id,
                        failure,
                    } => {
                        latest_failure.insert(chapter_id, failure.failure_kind);
                    }
                }
            }
            let mut ids: Vec<(String, PhaseFailureKind)> = latest_failure.into_iter().collect();
            ids.sort_by(|a, b| a.0.cmp(&b.0));
            return Ok(Some((checkpoint_path.to_path_buf(), ids)));
        }
        // Checkpoint exists but is empty — fall through to run-file
        // scan in case a legacy `--full` (no checkpoint) ran prior.
    }
    read_latest_failures(runs_dir)
}

/// Locate the most recent `questions-*.json` run file under the
/// given `runs_dir`, read it, and return the chapter ids that failed
/// paired with their structured `PhaseFailureKind`. Callers that only
/// want the ids can ignore the kind.
///
/// Returns `Ok(None)` when there's nothing to retry; `Err` on I/O or
/// deserialization problems (which signal a broken run-output file).
///
/// `pub(super)` because the `build` orchestration reads this to
/// decide whether to auto-retry after an Extract step fails.
pub(super) fn read_latest_failures(
    runs_dir: &std::path::Path,
) -> Result<Option<(PathBuf, Vec<(String, PhaseFailureKind)>)>, String> {
    if !runs_dir.exists() {
        return Ok(None);
    }
    let mut candidates: Vec<(u64, PathBuf)> = Vec::new();
    for entry in
        fs::read_dir(runs_dir).map_err(|e| format!("reading {}: {e}", runs_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("iterating {}: {e}", runs_dir.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("questions-") || !name.ends_with(".json") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        candidates.push((mtime, entry.path()));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let Some((_, latest)) = candidates.first() else {
        return Ok(None);
    };
    let raw =
        fs::read_to_string(latest).map_err(|e| format!("reading {}: {e}", latest.display()))?;
    let parsed: Phase1Output = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "parsing {}: {e} (file may predate the failures field)",
            latest.display()
        )
    })?;
    let ids: Vec<(String, PhaseFailureKind)> = parsed
        .failures
        .into_iter()
        .map(|f| (f.chapter_id, f.failure_kind))
        .collect();
    Ok(Some((latest.clone(), ids)))
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

/// Phase D2 — persisted token-spend record at `<workspace>/_tokens.json`.
/// Schema kept stable so the corpus-status display + future
/// `/internal/atlas/status` endpoint can deserialise the same file
/// without coordination.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenSpendRecord {
    pub schema_version: u32,
    pub corpus_id: String,
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Wall-clock start of the extract run that wrote this record
    /// (Unix ms). Reset every run — this is per-run spend, not
    /// lifetime-of-corpus spend, because Phase 1 caches and
    /// `--resume` make lifetime accounting non-trivial.
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
}

const TOKEN_SPEND_SCHEMA: u32 = 1;

/// Atomically write a token-spend snapshot to `path`. Sibling `.tmp`
/// + rename so a crash mid-write can't leave a half-finished file.
pub fn write_token_snapshot(
    path: &std::path::Path,
    corpus_id: &str,
    started_at_ms: u64,
    ledger: &super::inference_client::TokenUsageLedger,
) -> std::io::Result<()> {
    let snap = ledger.snapshot();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let record = TokenSpendRecord {
        schema_version: TOKEN_SPEND_SCHEMA,
        corpus_id: corpus_id.to_string(),
        calls: snap.calls,
        prompt_tokens: snap.prompt_tokens,
        completion_tokens: snap.completion_tokens,
        total_tokens: snap.total_tokens,
        started_at_ms,
        updated_at_ms: now_ms,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Read the persisted token-spend record from `path`. Returns
/// `None` if the file is missing, malformed, or has a future
/// schema. Used by the corpus-status display + atlas status
/// endpoint.
pub fn read_token_snapshot(path: &std::path::Path) -> Option<TokenSpendRecord> {
    let raw = std::fs::read_to_string(path).ok()?;
    let record: TokenSpendRecord = serde_json::from_str(&raw).ok()?;
    if record.schema_version != TOKEN_SPEND_SCHEMA {
        return None;
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_subset() {
        let args = ["ak".into(), "--chapters".into(), "a,b , c".into()].to_vec();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "ak");
        match p.selection {
            SelectionArg::Subset(ids) => assert_eq!(ids, vec!["a", "b", "c"]),
            _ => panic!("expected Subset"),
        }
    }

    #[test]
    fn parse_args_full() {
        let args = ["ak".into(), "--full".into()].to_vec();
        let p = parse_args(&args).unwrap();
        matches!(p.selection, SelectionArg::Full);
    }

    #[test]
    fn parse_args_rejects_both_chapters_and_full() {
        let err = parse_args(&[
            "ak".into(),
            "--chapters".into(),
            "a".into(),
            "--full".into(),
        ])
        .unwrap_err();
        assert!(err.contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn parse_args_accepts_retry_failed() {
        let p = parse_args(&["ak".into(), "--retry-failed".into()]).unwrap();
        assert!(matches!(p.selection, SelectionArg::RetryFailed));
    }

    #[test]
    fn parse_args_rejects_retry_failed_with_chapters() {
        let err = parse_args(&[
            "ak".into(),
            "--retry-failed".into(),
            "--chapters".into(),
            "sec_0001".into(),
        ])
        .unwrap_err();
        assert!(err.contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn parse_args_accepts_terse_with_retry_failed() {
        let p = parse_args(&["ak".into(), "--retry-failed".into(), "--terse".into()]).unwrap();
        assert!(matches!(p.selection, SelectionArg::RetryFailed));
        assert!(p.terse);
    }

    #[test]
    fn parse_args_accepts_terse_with_chapters() {
        let p = parse_args(&[
            "ak".into(),
            "--chapters".into(),
            "sec_0001".into(),
            "--terse".into(),
        ])
        .unwrap();
        assert!(matches!(p.selection, SelectionArg::Subset(_)));
        assert!(p.terse);
    }

    #[test]
    fn parse_args_rejects_terse_with_full() {
        let err = parse_args(&["ak".into(), "--full".into(), "--terse".into()]).unwrap_err();
        assert!(err.contains("recovery pass"), "got: {err}");
    }

    #[test]
    fn parse_args_terse_defaults_to_false() {
        let p = parse_args(&["ak".into(), "--retry-failed".into()]).unwrap();
        assert!(!p.terse);
    }

    #[test]
    fn read_latest_failures_returns_none_when_runs_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let got = read_latest_failures(&dir.path().join("runs")).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn read_latest_failures_picks_newest_run_and_extracts_ids() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        fs::create_dir_all(&runs).unwrap();
        let old = r#"{"schema_version":1,"pipeline_id":"literary","questions_by_chapter":[],"failures":[{"chapter_id":"sec_0001","reason":"old"}],"written_at":"t"}"#;
        let new = r#"{"schema_version":1,"pipeline_id":"literary","questions_by_chapter":[],"failures":[{"chapter_id":"sec_0005","reason":"r"},{"chapter_id":"sec_0018","reason":"r"}],"written_at":"t"}"#;
        fs::write(runs.join("questions-full-001.json"), old).unwrap();
        // Ensure the "new" file has a strictly later mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(runs.join("questions-full-002.json"), new).unwrap();
        let (path, ids) = read_latest_failures(&runs).unwrap().unwrap();
        assert!(
            path.ends_with("questions-full-002.json"),
            "got: {}",
            path.display()
        );
        let id_only: Vec<String> = ids.into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            id_only,
            vec!["sec_0005".to_string(), "sec_0018".to_string()]
        );
    }

    #[test]
    fn read_latest_failures_returns_failure_kinds_for_terse_filtering() {
        // When the run file carries `failure_kind` per entry, the
        // caller should be able to partition failures by kind —
        // that's what `--terse --retry-failed` does to target the
        // kinds the terse variant can recover.
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        fs::create_dir_all(&runs).unwrap();
        let payload = r#"{
          "schema_version": 1,
          "pipeline_id": "literary_atlas",
          "questions_by_chapter": [],
          "failures": [
            {"chapter_id": "sec_0001", "reason": "<think> truncated", "failure_kind": "think_truncated"},
            {"chapter_id": "sec_0002", "reason": "chat: transport", "failure_kind": "chat_error"},
            {"chapter_id": "sec_0003", "reason": "missing field", "failure_kind": "parse_drift"}
          ],
          "written_at": "t"
        }"#;
        fs::write(runs.join("questions-full-001.json"), payload).unwrap();
        let (_, ids) = read_latest_failures(&runs).unwrap().unwrap();
        assert_eq!(ids.len(), 3);
        let think_truncs: Vec<&String> = ids
            .iter()
            .filter(|(_, kind)| matches!(kind, PhaseFailureKind::ThinkTruncated))
            .map(|(id, _)| id)
            .collect();
        assert_eq!(think_truncs, vec!["sec_0001"]);
    }

    #[test]
    fn terse_filter_targets_both_think_truncation_and_parse_drift() {
        // Both kinds result from the same underlying failure mode —
        // the reasoning trace crowds the output cap and the JSON body
        // truncates mid-emit. Terse variant's bumped budget helps
        // both. ChatError + other kinds need different fixes; the
        // filter must drop them so we don't burn budget on chapters
        // terse can't help.
        let ids = vec![
            ("sec_0001".to_string(), PhaseFailureKind::ThinkTruncated),
            ("sec_0002".to_string(), PhaseFailureKind::ChatError),
            ("sec_0003".to_string(), PhaseFailureKind::ParseDrift),
            ("sec_0004".to_string(), PhaseFailureKind::EmptyExtraction),
        ];
        let (targeted, filtered_out): (Vec<_>, Vec<_>) = ids.into_iter().partition(|(_, kind)| {
            matches!(
                kind,
                PhaseFailureKind::ThinkTruncated
                    | PhaseFailureKind::ParseDrift
                    | PhaseFailureKind::DeadlineExceeded
            )
        });
        let targeted_ids: Vec<&str> = targeted.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(targeted_ids, vec!["sec_0001", "sec_0003"]);
        let filtered_ids: Vec<&str> = filtered_out.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(filtered_ids, vec!["sec_0002", "sec_0004"]);
    }

    #[test]
    fn parse_args_requires_one_selection() {
        let err = parse_args(&["ak".into()]).unwrap_err();
        assert!(err.contains("--chapters"));
    }

    #[test]
    fn parse_args_rejects_empty_chapter_list() {
        let err = parse_args(&["ak".into(), "--chapters".into(), "  ,  ,".into()]).unwrap_err();
        assert!(err.contains("at least one"));
    }
}
