//! `sovereign enrich extract <corpus> [--chapters ...|--full]` — phase 1.
//!
//! Rebuilds chapter inputs from the pinned source file, constructs a
//! `PhaseRunner` with the daemon-backed embed + chat closures, runs
//! `phase_1_extract_questions`, merges `characters_present` back into
//! the chapter manifest, and prints a one-line summary.

use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{
    ChapterManifest, ChapterSelection, Phase1Progress, PhaseCache, PhaseRunner, PipelineRegistry,
    RunOutputWriter,
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
            "sovereign enrich extract <corpus-id> [--chapters <id1,id2,...> | --full]",
        ),
        HelpSection::Flags(&[
            ("--chapters <ids>", "Comma-separated chapter ids (e.g. sec_0001,sec_0003). Subset runs do NOT update the cache."),
            ("--full", "Run on every chapter in the manifest. Updates cache/questions.json."),
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
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, chat) = client.into_closures();

    let cache = PhaseCache::new(paths::cache_dir(&cfg.corpus_id));
    let runs = RunOutputWriter::new(paths::runs_dir(&cfg.corpus_id));
    let runner = PhaseRunner::new(
        pipeline,
        embed,
        chat,
        cache,
        runs,
        paths::exemplars_dir(&cfg.corpus_id),
    );

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
    };

    println!(
        "  running phase 1 ({}) over {} chapter(s)",
        selection.mode_label(),
        match &selection {
            ChapterSelection::Full => inputs.len(),
            ChapterSelection::Subset(ids) => ids.len(),
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
        .phase_1_extract_questions(&inputs, &selection, progress)
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
        println!("  · subset run — cache NOT updated (re-run with --full to promote)");
    }
    if !result.failures.is_empty() {
        eprintln!(
            "  ! {} chapter(s) failed — see run output at {}",
            result.failures.len(),
            result.run_path.display()
        );
        return 1;
    }
    0
}

#[derive(Debug)]
enum SelectionArg {
    Subset(Vec<String>),
    Full,
}

#[derive(Debug)]
struct ParsedExtract {
    corpus_id: String,
    selection: SelectionArg,
}

fn parse_args(args: &[String]) -> Result<ParsedExtract, String> {
    let mut corpus_id: Option<String> = None;
    let mut chapters_csv: Option<String> = None;
    let mut full = false;

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
    let selection = match (chapters_csv, full) {
        (Some(_), true) => {
            return Err("cannot combine --chapters and --full".into());
        }
        (Some(csv), false) => {
            let ids: Vec<String> = csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if ids.is_empty() {
                return Err("--chapters must list at least one id".into());
            }
            SelectionArg::Subset(ids)
        }
        (None, true) => SelectionArg::Full,
        (None, false) => {
            return Err("must provide either --chapters <ids> or --full".into());
        }
    };
    Ok(ParsedExtract { corpus_id, selection })
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
        assert!(err.contains("cannot combine"));
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
