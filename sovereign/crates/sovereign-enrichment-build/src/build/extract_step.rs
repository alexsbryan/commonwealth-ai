// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Extract step, which is the one step with a retry policy.
//!
//! Phase 1 extraction costs ~30 minutes of model time, and its two common
//! failure kinds (ThinkTruncated, ParseDrift) are transient. So this step —
//! alone among the ten — re-runs itself once against the failed sections
//! before reporting, and promotes a subset run into cache so the downstream
//! phases have inputs.

use super::plan::{ParsedBuild, Selection};
use super::steps::{StepFailure, StepOutcome};
use crate::{
    atlas_configuration, atlas_gaps, atlas_phase_cmd, atlas_resolve, atlas_tensions,
    atlas_tensions_classify, config::EnrichConfig, extract, paths, schema_review, seed_cmd,
};

/// Run the Extract step with an auto-retry pass on transient
/// failure kinds (ThinkTruncated + ParseDrift). The pattern:
///
/// 1. Run `cmd_extract --full` (or `--chapters …`). If clean,
/// done.
/// 2. If non-zero AND the run file shows ONLY retry-eligible
/// failures, invoke `--retry-failed --terse` once. A
/// different model seed + bumped output budget recovers
/// ~80-90% of drift cases (measured on the Dopesick Jesus
/// smoke before structured retries were wired).
/// 3. After retry, re-check the final failure set. If it's
/// empty or has only non-retriable kinds (ChatError,
/// Skipped), promote whatever we have and return 0; the
/// caller can still `enrich errors <corpus>` to surface
/// what's left.
/// 4. If retry-eligible failures remain, return 1 — the
/// operator gets the same glassbox output as before.
///
/// An operator can opt out of the auto-retry by setting
/// `SOVEREIGN_ENRICH_AUTO_RETRY=0`. Defaults to on. The
/// env-var (rather than a CLI flag) keeps the common case —
/// CI invocations + the desktop UI — from having to thread a
/// boolean through every orchestrator.
pub(super) async fn run_extract_step(parsed: &ParsedBuild) -> Result<StepOutcome, StepFailure> {
    use corpus_engine::enrichment::pipeline::PhaseFailureKind;

    let corpus = parsed.corpus_id.as_str();

    // First pass — the standard extract run shaped by the
    // orchestrator's selection.
    let mut first_args: Vec<String> = vec![corpus.into()];
    match &parsed.selection {
        Selection::Full => first_args.push("--full".into()),
        Selection::Chapters(ids) => {
            first_args.push("--chapters".into());
            first_args.push(ids.join(","));
        }
    }
    let first_code = extract::run_extract(&first_args).await;

    // Whatever the first pass did, we still need to promote a
    // subset run's output to cache so downstream phases have
    // input. Do this now so the retry (if any) sees the same
    // state the promote would produce.
    if matches!(&parsed.selection, Selection::Chapters(_)) && first_code == 0 {
        if let Err(e) = promote_subset_to_cache(corpus) {
            return Err(StepFailure::new(
                format!("promoting subset run to cache: {e}"),
                1,
            ));
        }
        println!("  · promoted subset run → cache/questions.json");
    }

    if first_code == 0 {
        // `cmd_extract` is still on the `-> i32` contract, so the count
        // is not in its return value — but it IS in the run file it just
        // wrote. Reading it beats inventing one, and beats a summary
        // that says nothing.
        let summary = match extract::read_latest_run(&paths::runs_dir(corpus)) {
            Ok(Some(run)) => format!("{} chapter(s) extracted", run.extracted),
            Ok(None) => "extract reported success but wrote no run file".to_string(),
            Err(e) => format!("extract succeeded; its run file was unreadable ({e})"),
        };
        return Ok(StepOutcome::did(summary));
    }

    // The first pass failed. Decide whether to auto-retry.
    let auto_retry_enabled = std::env::var("SOVEREIGN_ENRICH_AUTO_RETRY")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true);
    if !auto_retry_enabled {
        return Err(StepFailure::new(
            format!(
                "extract failed (exit {first_code}); auto-retry is off (SOVEREIGN_ENRICH_AUTO_RETRY)"
            ),
            first_code,
        ));
    }

    let runs_dir = paths::runs_dir(corpus);
    let failures = match extract::read_latest_run(&runs_dir) {
        Ok(Some(run)) => run.failures,
        Ok(None) => {
            // No run file means extract never produced one —
            // probably a config error. Nothing to retry.
            return Err(StepFailure::new(
                format!(
                    "extract failed (exit {first_code}) and wrote no run file — nothing to auto-retry, which usually means a config error"
                ),
                first_code,
            ));
        }
        Err(msg) => {
            return Err(StepFailure::new(
                format!(
                    "extract failed (exit {first_code}); auto-retry skipped — reading latest run file: {msg}"
                ),
                first_code,
            ));
        }
    };

    let retriable_count = failures
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                PhaseFailureKind::ThinkTruncated
                    | PhaseFailureKind::ParseDrift
                    | PhaseFailureKind::DeadlineExceeded
            )
        })
        .count();

    if retriable_count == 0 {
        // Nothing the terse retry can help with (chat errors,
        // empty extractions, etc.). Let the operator handle.
        return Err(StepFailure::new(
            format!(
                "extract failed (exit {first_code}) with {} failure(s), none of them retriable — see `svrn enrich errors {corpus}`",
                failures.len()
            ),
            first_code,
        ));
    }

    println!();
    println!(
        "  · auto-retry: {} retriable failure(s) (parse_drift / think_truncated / deadline_exceeded) — \
         running `--retry-failed --terse` once",
        retriable_count
    );
    println!();

    let retry_args: Vec<String> = vec![corpus.into(), "--retry-failed".into(), "--terse".into()];
    let retry_code = extract::run_extract(&retry_args).await;

    // After the retry, re-read the latest run file to decide
    // whether the orchestration can continue. If only
    // unrecoverable kinds remain (ChatError, Skipped,
    // EmptyExtraction), surface them to the operator but allow
    // the orchestrator to continue — those chapters just won't
    // be in the cache for downstream phases. If retriable kinds
    // are still present, treat the retry as having not resolved
    // the issue and return the original failure exit.
    let remaining = match extract::read_latest_run(&runs_dir) {
        Ok(Some(run)) => run.failures,
        Ok(None) => Vec::new(),
        Err(_) => Vec::new(),
    };
    let remaining_retriable = remaining
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                PhaseFailureKind::ThinkTruncated
                    | PhaseFailureKind::ParseDrift
                    | PhaseFailureKind::DeadlineExceeded
            )
        })
        .count();

    if remaining_retriable == 0 {
        println!();
        println!(
            "  ✓ auto-retry resolved {} chapter(s). {} non-retriable failure(s) remain; \
             continuing build.",
            retriable_count - remaining_retriable,
            remaining.len()
        );
        // Promote the (now-complete) subset extract to cache so the
        // downstream phases have input. The first-pass promote above is
        // gated on `first_code == 0`; when auto-retry ran, that gate was
        // false, so the full subset never landed — and the terse-retry
        // path only merges the *retried* chapter's output, not the whole
        // subset. Promote the full subset explicitly here. (Observed
        // 2026-06-01: a single parse_drift triggered auto-retry, the
        // first-pass promote was skipped, and cluster then failed
        // "questions cache is missing".)
        if matches!(&parsed.selection, Selection::Chapters(_)) {
            if let Err(e) = promote_subset_to_cache(corpus) {
                return Err(StepFailure::new(
                    format!("promoting subset run to cache after retry: {e}"),
                    1,
                ));
            }
            println!("  · promoted subset run → cache/questions.json");
        }
        return Ok(StepOutcome::did(format!(
            "auto-retry recovered {} chapter(s); {} non-retriable failure(s) remain",
            retriable_count - remaining_retriable,
            remaining.len()
        )));
    }

    // Retry didn't recover everything. Surface the original
    // failure exit and let the operator intervene via
    // `enrich errors <corpus>`.
    Err(StepFailure::new(
        format!(
            "auto-retry left {remaining_retriable} retriable failure(s) unresolved — see `svrn enrich errors {corpus}`"
        ),
        retry_code,
    ))
}

/// Copy the most recent subset run into cache/questions.json so
/// cluster/name/resolve can proceed against a consistent input.
/// Mirrors what operators do by hand today.
fn promote_subset_to_cache(corpus_id: &str) -> std::io::Result<()> {
    let runs_dir = paths::runs_dir(corpus_id);
    let cache_dir = paths::cache_dir(corpus_id);
    let latest = find_latest_run(&runs_dir)?;
    let cache_path = cache_dir.join("questions.json");
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::copy(&latest, &cache_path)?;
    Ok(())
}

fn find_latest_run(runs_dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(runs_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.starts_with("questions-") && s.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    if entries.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no questions-*.json run files in {} — did extract succeed?",
                runs_dir.display()
            ),
        ));
    }
    entries.sort_by_key(|e| e.file_name());
    Ok(entries.last().unwrap().path())
}
