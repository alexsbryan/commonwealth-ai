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
/// Step 2's "ONLY retry-eligible" test decides whether to retry, NOT
/// whether the build survives. A first pass whose failures are all
/// non-retriable skips the retry and lands on step 3's verdict directly:
/// the failures are reported and the build continues. Both paths go
/// through [`continue_past_non_retriable`], which is the only place that
/// answer is given. The single fatal case is nothing extracted at all —
/// the eight steps after this one would have no input.
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
    let (extracted_count, failures) = match extract::read_latest_run(&runs_dir) {
        Ok(Some(run)) => (run.extracted, run.failures),
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

    if after_extract(retriable_count, extracted_count) != AfterExtract::Retry {
        // Nothing the terse retry can help with: a chapter the pipeline
        // itself declined (Skipped), an empty extraction, a chat error.
        //
        // This used to STOP the build. The post-retry branch below faces
        // exactly the same set — `remaining_retriable == 0` — and
        // continues, printing "N non-retriable failure(s) remain;
        // continuing build." One predicate, two opposite verdicts, decided
        // by whether a retry happened to have run (ARCH §10.6). The retry
        // is not what makes those failures survivable; they are the same
        // failures either way.
        //
        // Found 2026-09-02 by running the ontology chain end to end. Five
        // sections of a twenty-section catalogue fell under the 40-word
        // floor, so phase 1 skipped them BY DESIGN — and the build stopped
        // at step 1 of 9 with `none of them retriable`. A corpus with a
        // short section could not be built at all, which is every corpus
        // a person writes by hand.
        //
        // The one case that genuinely cannot continue is nothing
        // extracted: the eight downstream steps would run over an empty
        // cache and each invent its own way of saying so.
        if after_extract(retriable_count, extracted_count) == AfterExtract::Fatal {
            return Err(StepFailure::new(
                format!(
                    "extract failed (exit {first_code}): {} failure(s), none retriable, and NOT ONE chapter extracted — the remaining steps have no input; see `svrn enrich errors {corpus}`",
                    failures.len()
                ),
                first_code,
            ));
        }
        return continue_past_non_retriable(corpus, parsed, extracted_count, failures.len());
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

    let extracted_after = extract::read_latest_run(&runs_dir)
        .ok()
        .flatten()
        .map_or(extracted_count, |r| r.extracted);
    match after_extract(remaining_retriable, extracted_after) {
        AfterExtract::Continue => {
            println!();
            println!("  ✓ auto-retry resolved {retriable_count} chapter(s).");
            return continue_past_non_retriable(corpus, parsed, extracted_after, remaining.len());
        }
        AfterExtract::Fatal => {
            return Err(StepFailure::new(
                format!(
                    "auto-retry left {} failure(s) and NOT ONE chapter extracted — the remaining steps have no input; see `svrn enrich errors {corpus}`",
                    remaining.len()
                ),
                retry_code,
            ));
        }
        AfterExtract::Retry => {}
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

/// What an extract pass that left failures should lead to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterExtract {
    /// Transient kinds are present — the terse retry is worth one run.
    Retry,
    /// No retry can help, but chapters landed. Report and continue: the
    /// eight steps after this one have real input.
    Continue,
    /// No retry can help AND nothing landed. The rest of the build would
    /// run over an empty cache.
    Fatal,
}

/// The single decider, so the first pass and the post-retry pass cannot
/// answer it differently (ARCH §10.6). Whether a retry has already run is
/// not an input: it does not change what the surviving failures are.
fn after_extract(retriable: usize, extracted: usize) -> AfterExtract {
    match (retriable, extracted) {
        (0, 0) => AfterExtract::Fatal,
        (0, _) => AfterExtract::Continue,
        _ => AfterExtract::Retry,
    }
}

/// The one decider for "extract left failures a retry cannot help — now
/// what?", shared by the first pass and the post-retry pass so the two
/// cannot answer it differently (ARCH §10.6).
///
/// Continues the build. The failures are real and are reported, but the
/// chapters that DID extract are real input for the eight steps that
/// follow, and `svrn enrich errors <corpus>` holds the detail.
fn continue_past_non_retriable(
    corpus: &str,
    parsed: &ParsedBuild,
    extracted: usize,
    failed: usize,
) -> Result<StepOutcome, StepFailure> {
    println!(
        "  · {failed} failure(s) no retry can help (skipped / empty / chat error); \
         {extracted} chapter(s) extracted — continuing the build. \
         Detail: `svrn enrich errors {corpus}`"
    );
    // Promote the subset extract to cache so the downstream phases have
    // input. The first-pass promote is gated on a clean exit; when there
    // were failures that gate was false, so the subset never landed — and
    // the terse-retry path only merges the *retried* chapter's output, not
    // the whole subset. Promote it explicitly here. (Observed 2026-06-01:
    // a single parse_drift triggered auto-retry, the first-pass promote was
    // skipped, and cluster then failed "questions cache is missing".)
    if matches!(&parsed.selection, Selection::Chapters(_)) {
        if let Err(e) = promote_subset_to_cache(corpus) {
            return Err(StepFailure::new(
                format!("promoting subset run to cache: {e}"),
                1,
            ));
        }
        println!("  · promoted subset run → cache/questions.json");
    }
    Ok(StepOutcome::did(format!(
        "{extracted} chapter(s) extracted; {failed} non-retriable failure(s) remain"
    )))
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

#[cfg(test)]
mod tests {
    use super::{after_extract, AfterExtract};

    /// The bug this file was changed for, stated as an input: five chapters
    /// the pipeline SKIPPED by its own word-count rule, fifteen extracted.
    /// Nothing here is retriable, and the build must still run.
    #[test]
    fn skipped_chapters_do_not_stop_a_build_that_extracted_something() {
        assert_eq!(after_extract(0, 15), AfterExtract::Continue);
    }

    /// The failing input for the Fatal arm — without it, `Continue` would
    /// be unfalsifiable and the eight downstream steps would each invent
    /// their own way of saying the cache is empty.
    #[test]
    fn nothing_extracted_is_the_one_case_that_cannot_continue() {
        assert_eq!(after_extract(0, 0), AfterExtract::Fatal);
    }

    #[test]
    fn a_transient_failure_still_earns_the_retry() {
        assert_eq!(after_extract(1, 15), AfterExtract::Retry);
        assert_eq!(after_extract(1, 0), AfterExtract::Retry);
    }
}
