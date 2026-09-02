// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `enrich extract` invocation, and reading back what a run produced.
//!
//! Argv into a `ParsedExtract`, plus the two readers the retry policy needs:
//! which sections failed and with which kinds (`read_latest_run`), and which
//! of those are worth a second attempt (`read_failures_for_retry`).

use corpus_engine::enrichment::pipeline::{
    checkpoint_processed_ids, collapse_phase1_checkpoint, read_phase1_checkpoint, ChapterManifest,
    ChapterSelection, Phase1Output, Phase1Progress, PhaseFailureKind, PhaseRunner,
    PipelineRegistry, RetryMode, RunOutputWriter,
};
use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
pub(super) enum SelectionArg {
    Subset(Vec<String>),
    Full,
    /// Resolved later (needs the corpus runs/ directory) into a
    /// `Subset` over the chapter ids that failed in the latest run.
    RetryFailed,
}

#[derive(Debug)]
/// A parsed `enrich extract` invocation. `pub` because `parse_args` returns
/// one across the crate boundary; fields stay PRIVATE — a caller may hold and
/// pass one, not inspect it.
pub struct ParsedExtract {
    pub(super) corpus_id: String,
    pub(super) selection: SelectionArg,
    /// Set by `--terse`. Requests the terse Phase 1 prompt variant
    /// + an optional `max_output_tokens` bump. Combinable with
    /// `--chapters` or `--retry-failed` but not with `--full` — a
    /// terse pass is by design a recovery run, not a full-corpus
    /// run.
    pub(super) terse: bool,
    /// Set by `--resume`. Reads the per-chapter checkpoint and skips
    /// chapter ids already recorded there. Compatible with `--full`,
    /// `--chapters`, and `--retry-failed`; the resume filter applies
    /// to whichever selection mode is in effect.
    pub(super) resume: bool,
    /// Set by `--finalize`. Read-only mode: reconstructs a run-file
    /// (and updates the cache when --full-equivalent semantics
    /// apply) from the checkpoint. Mutually exclusive with the
    /// selection flags.
    pub(super) finalize: bool,
}

pub fn parse_args(args: &[String]) -> Result<ParsedExtract, String> {
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
pub(super) fn read_failures_for_retry(
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
    read_latest_run(runs_dir).map(|opt| opt.map(|r| (r.path, r.failures)))
}

/// Locate the most recent `questions-*.json` run file under the
/// given `runs_dir`, read it, and return the chapter ids that failed
/// paired with their structured `PhaseFailureKind`. Callers that only
/// want the ids can ignore the kind.
///
/// Returns `Ok(None)` when there's nothing to retry; `Err` on I/O or
/// deserialization problems (which signal a broken run-output file).
///
/// `pub(crate)` because the `build` orchestration reads this to
/// decide whether to auto-retry after an Extract step fails.
/// What the newest run file records. One reader, not two: finding "the
/// newest `questions-*.json`" is a single decision and lives in one
/// place (ARCH §10.6).
#[derive(Debug, Clone)]
pub(crate) struct LatestRun {
    pub path: PathBuf,
    /// Chapters that produced an extraction.
    pub extracted: usize,
    /// Chapters that did not, with the kind that stopped them.
    pub failures: Vec<(String, PhaseFailureKind)>,
}

pub(crate) fn read_latest_run(runs_dir: &std::path::Path) -> Result<Option<LatestRun>, String> {
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
    let extracted = parsed.questions_by_chapter.len();
    let failures: Vec<(String, PhaseFailureKind)> = parsed
        .failures
        .into_iter()
        .map(|f| (f.chapter_id, f.failure_kind))
        .collect();
    Ok(Some(LatestRun {
        path: latest.clone(),
        extracted,
        failures,
    }))
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
    fn read_latest_run_returns_none_when_runs_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let got = read_latest_run(&dir.path().join("runs")).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn read_latest_run_picks_newest_run_and_extracts_ids() {
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
        let run = read_latest_run(&runs).unwrap().unwrap();
        let (path, ids) = (run.path, run.failures);
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
    fn read_latest_run_returns_failure_kinds_for_terse_filtering() {
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
        let ids = read_latest_run(&runs).unwrap().unwrap().failures;
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
