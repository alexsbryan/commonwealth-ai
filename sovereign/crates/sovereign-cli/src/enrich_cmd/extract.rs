//! `sovereign enrich extract <corpus> [--chapters ...|--full]` — phase 1.
//!
//! Rebuilds chapter inputs from the pinned source file, constructs a
//! `PhaseRunner` with the daemon-backed embed + chat closures, runs
//! `phase_1_extract_questions`, merges `characters_present` back into
//! the chapter manifest, and prints a one-line summary.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{
    ChapterManifest, ChapterSelection, Phase1Output, Phase1Progress, PhaseCache, PhaseFailureKind,
    PhaseRunner, PipelineRegistry, RetryMode, RunOutputWriter,
};

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich extract",
    summary: "Run phase 1 (per-chapter question extraction) on a subset or the full corpus.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich extract <corpus-id> [--chapters <id1,id2,...> | --full | --retry-failed] [--terse]",
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
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich extract ak --chapters sec_0001,sec_0011,sec_0023",
                "Fast-loop subset run (2-3 min). Output written to runs/.",
            ),
            (
                "sovereign enrich extract ak --full",
                "Full-corpus run. Updates cache/questions.json — consumed by phases 2+.",
            ),
            (
                "sovereign enrich extract ak --retry-failed",
                "Reprocess the chapters that failed in the last run (parse errors, transient chat failures).",
            ),
            (
                "sovereign enrich extract bk --retry-failed --terse",
                "Recover chapters whose default pass failed with <think> truncation, using the terse prompt variant.",
            ),
        ]),
        HelpSection::Notes(
            "Requires `sovereign enrich init` first. Daemon must be running at localhost:9741.",
        ),
    ],
};

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
    let pipeline = match registry.get(&cfg.pipeline_id) {
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

    let client = match DaemonInferenceClient::new(
        cfg.base_url.clone(),
        cfg.chat_model.clone(),
        cfg.embed_model.clone(),
    ) {
        Ok(c) => c.with_max_output_tokens(cfg.max_output_tokens),
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, chat, chat_with_tokens) = client.into_closures_with_tokens();

    let cache = PhaseCache::new(paths::cache_dir(&cfg.corpus_id));
    let runs = RunOutputWriter::new(paths::runs_dir(&cfg.corpus_id));
    let runner = PhaseRunner::new(
        pipeline,
        embed,
        chat,
        cache,
        runs,
        paths::exemplars_dir(&cfg.corpus_id),
    )
    .with_chat_with_tokens(chat_with_tokens);

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
            let known: std::collections::HashSet<_> = inputs
                .iter()
                .map(|c| c.chapter_id.as_str())
                .collect();
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
            match read_latest_failures(&runs_dir) {
                Ok(Some((path, ids))) if ids.is_empty() => {
                    println!(
                        "  · no failures in the most recent run ({}) — nothing to retry.",
                        path.display()
                    );
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
                                PhaseFailureKind::ThinkTruncated | PhaseFailureKind::ParseDrift
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
                    let known: std::collections::HashSet<_> = inputs
                        .iter()
                        .map(|c| c.chapter_id.as_str())
                        .collect();
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
                        "error: no prior run files under {} — run `sovereign enrich extract {} --full` first",
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
        "  running phase 1 ({}) over {} chapter(s)",
        selection.mode_label(),
        match &selection {
            ChapterSelection::Full => inputs.len(),
            ChapterSelection::Subset(ids) | ChapterSelection::RetryFailed(ids) => ids.len(),
        }
    );

    let progress = |ev: Phase1Progress<'_>| match ev {
        Phase1Progress::Start { total, exemplars_loaded } => {
            println!("    · {exemplars_loaded} exemplar(s) loaded, {total} chapter(s) to process");
        }
        Phase1Progress::ChapterStart { i, total, chapter_id } => {
            print!("    [{i}/{total}] {chapter_id}… ");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }
        Phase1Progress::ChapterDone { chapter_id: _, question_count } => {
            println!("{question_count} q");
        }
        Phase1Progress::ChapterFailed { chapter_id: _, reason } => {
            println!("FAILED: {reason}");
        }
        Phase1Progress::Done { produced, failed, run_path } => {
            println!(
                "  ✓ {produced} ok, {failed} failed — {}",
                run_path.display()
            );
        }
    };

    let result = match runner
        .phase_1_extract_questions_with_retry(&inputs, &selection, retry_mode, progress)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: phase 1 run failed: {e}");
            return 1;
        }
    };

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
                println!(
                    "  · retry produced no new successes — cache/questions.json unchanged"
                );
            }
            _ => {
                println!(
                    "  · subset run — cache NOT updated (re-run with --full to promote)"
                );
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
            "      sovereign enrich extract {} --chapters {}",
            cfg.corpus_id,
            ids.join(",")
        );
        eprintln!("    Or, from the latest run file:");
        eprintln!("      sovereign enrich extract {} --retry-failed", cfg.corpus_id);
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
}

fn parse_args(args: &[String]) -> Result<ParsedExtract, String> {
    let mut corpus_id: Option<String> = None;
    let mut chapters_csv: Option<String> = None;
    let mut full = false;
    let mut retry_failed = false;
    let mut terse = false;

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
    if terse && full {
        return Err(
            "--terse is a recovery pass; pair it with --retry-failed or --chapters, \
             not --full".into(),
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
            "must provide one of --chapters <ids>, --full, or --retry-failed".into(),
        );
    };
    Ok(ParsedExtract { corpus_id, selection, terse })
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
    for entry in fs::read_dir(runs_dir).map_err(|e| format!("reading {}: {e}", runs_dir.display()))? {
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
    let raw = fs::read_to_string(latest)
        .map_err(|e| format!("reading {}: {e}", latest.display()))?;
    let parsed: Phase1Output = serde_json::from_str(&raw).map_err(|e| {
        format!("parsing {}: {e} (file may predate the failures field)", latest.display())
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
    let registry = PipelineRegistry::builtin();
    let pipeline = registry
        .get(&cfg.pipeline_id)
        .ok_or_else(|| format!("unknown pipeline: {}", cfg.pipeline_id))?;
    let cache = PhaseCache::new(paths::cache_dir(corpus_id));
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
    Ok((result.output.questions_by_chapter.len(), result.cache_updated))
}

// Suppress unused-import warnings for `ChapterManifest` in non-test
// builds (it's only touched through `rebuild_corpus_state`'s return
// tuple, which the CLI explicitly names for clarity).
#[allow(dead_code)]
fn _hold_chapter_manifest(_: &ChapterManifest) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_subset() {
        let args = ["ak".into(), "--chapters".into(), "a,b , c".into()]
            .to_vec();
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
        let err = parse_args(
            &["ak".into(), "--chapters".into(), "a".into(), "--full".into()],
        )
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
        let p = parse_args(&[
            "ak".into(),
            "--retry-failed".into(),
            "--terse".into(),
        ])
        .unwrap();
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
        let err = parse_args(&["ak".into(), "--full".into(), "--terse".into()])
            .unwrap_err();
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
        assert!(path.ends_with("questions-full-002.json"), "got: {}", path.display());
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
                PhaseFailureKind::ThinkTruncated | PhaseFailureKind::ParseDrift
            )
        });
        let targeted_ids: Vec<&str> =
            targeted.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(targeted_ids, vec!["sec_0001", "sec_0003"]);
        let filtered_ids: Vec<&str> =
            filtered_out.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(filtered_ids, vec!["sec_0002", "sec_0004"]);
    }

    #[test]
    fn parse_args_requires_one_selection() {
        let err = parse_args(&["ak".into()]).unwrap_err();
        assert!(err.contains("--chapters"));
    }

    #[test]
    fn parse_args_rejects_empty_chapter_list() {
        let err = parse_args(
            &["ak".into(), "--chapters".into(), "  ,  ,".into()],
        )
        .unwrap_err();
        assert!(err.contains("at least one"));
    }
}
