// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich reset <corpus>` — clear phase caches/runs so
//! the developer can re-iterate without hand-deleting files.
//!
//! Three modes, ordered by destructiveness:
//!
//! - `--from <phase>` (default: `question-clusters`): clear caches +
//!   run files for `<phase>` and every downstream phase. Phase 1
//!   output survives by default — it's the most expensive to
//!   regenerate, and usually the dev is iterating on exemplars for
//!   phases 3/5/6/7, not phase 1.
//! - `--full`: wipe the entire `~/.sovereign/enrichment/<corpus>/`
//!   tree PLUS the chapter manifest under
//!   `~/.sovereign/indexes/<corpus>/chapters.json`. Leaves the
//!   source text untouched.
//! - `--include-exemplars`: opt-in rider on either mode that ALSO
//!   clears the per-phase exemplar JSONs. These are hand-edited
//!   work so we default to preserving them.
//!
//! Always prompts before deleting unless `--yes`. Pair with
//! `--dry-run` to preview the file list first.

use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use corpus_engine::enrichment::pipeline::PipelinePhase;

use super::config::EnrichConfig;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_cli_shared::prompts::confirm;

const HELP: Help = Help {
    command: "svrn enrich reset",
    summary: "Clear phase caches and run outputs so a corpus can be re-iterated.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich reset <corpus-id> [--from <phase>] [--full] [--include-exemplars] [--dry-run] [--yes]",
        ),
        HelpSection::Flags(&[
            ("--from <phase>", "Clear this phase + every downstream. Default: question-clusters (keeps phase 1)."),
            ("--full", "Wipe the whole enrichment tree AND the chapter manifest. Source text is preserved."),
            ("--include-exemplars", "Also clear per-phase exemplar banks. Default is to preserve — they are hand-edited work."),
            ("--dry-run", "Print the file list that would be deleted; make no changes."),
            ("--yes", "Skip the interactive confirmation."),
        ]),
        HelpSection::Examples(&[
            ("svrn enrich reset ak", "Default: clear phases 2-7 caches + runs; keep phase 1, exemplars, config, manifest."),
            ("svrn enrich reset ak --from positions", "Clear phases 5, 6, 7 caches + runs."),
            ("svrn enrich reset ak --full --yes", "Tear down everything for 'ak' without prompting."),
            ("svrn enrich reset ak --dry-run", "Preview what would be deleted."),
        ]),
    ],
};

pub async fn cmd_reset(args: &[String]) -> i32 {
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

    // Config must exist — refuse to operate on unknown corpora.
    if let Err(e) = EnrichConfig::require(&parsed.corpus_id) {
        eprintln!("error: {e}");
        return 1;
    }

    let plan = build_plan(&parsed);
    render_plan(&plan, &parsed);

    if plan.paths.is_empty() {
        println!("  (nothing to delete)");
        return 0;
    }
    if parsed.dry_run {
        return 0;
    }
    if !parsed.yes {
        let confirmed = confirm(
            &format!("  Delete the {} item(s) listed above?", plan.paths.len()),
            false,
        );
        if !confirmed {
            println!("  aborted");
            return 2;
        }
    }

    let mut deleted = 0usize;
    let mut failed: Vec<(PathBuf, std::io::Error)> = Vec::new();
    for target in &plan.paths {
        let result = if target.is_dir() {
            fs::remove_dir_all(target)
        } else {
            fs::remove_file(target)
        };
        match result {
            Ok(()) => deleted += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Race / pre-deleted — count as success.
                deleted += 1;
            }
            Err(e) => failed.push((target.clone(), e)),
        }
    }

    println!("  ✓ deleted {deleted} item(s)");
    if !failed.is_empty() {
        eprintln!("  ! {} deletion(s) failed:", failed.len());
        for (p, e) in &failed {
            eprintln!("      · {}: {e}", p.display());
        }
        return 1;
    }
    0
}

// ── Plan construction ─────────────────────────────────────────

/// Ordered list of paths `cmd_reset` will delete. Directories are
/// removed recursively (`remove_dir_all`); individual files are
/// removed with `remove_file`.
#[derive(Debug)]
struct ResetPlan {
    /// Human description of each entry, aligned with `paths` by index.
    labels: Vec<String>,
    paths: Vec<PathBuf>,
}

fn build_plan(parsed: &ParsedReset) -> ResetPlan {
    let mut labels = Vec::new();
    let mut paths = Vec::new();

    if parsed.full {
        let enrich_root = paths::enrichment_root(&parsed.corpus_id);
        if enrich_root.exists() {
            labels.push(format!(
                "entire enrichment tree ({})",
                enrich_root.display()
            ));
            paths.push(enrich_root);
        }
        let manifest_path = paths::chapters_manifest_path(&parsed.corpus_id);
        if manifest_path.exists() {
            labels.push(format!("chapter manifest ({})", manifest_path.display()));
            paths.push(manifest_path);
        }
        // If the dev asked for full + include-exemplars, the whole tree
        // covers the bank files already — nothing extra to add. But
        // when --full is used without exemplars preserved elsewhere,
        // we make no attempt to rescue them (we warned via the plan
        // render).
        return ResetPlan { labels, paths };
    }

    // Partial mode: from parsed.from onward.
    let phases_to_clear = downstream_phases(parsed.from);

    // 1. Phase caches.
    let cache_dir = paths::cache_dir(&parsed.corpus_id);
    for phase in &phases_to_clear {
        let p = cache_dir.join(format!("{}.json", phase.id()));
        if p.exists() {
            labels.push(format!("cache: {}", phase.id()));
            paths.push(p);
        }
    }

    // 2. Run outputs for each phase (prefix-match).
    let runs_dir = paths::runs_dir(&parsed.corpus_id);
    if runs_dir.exists() {
        if let Ok(entries) = fs::read_dir(&runs_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                for phase in &phases_to_clear {
                    let prefix = format!("{}-", phase.id());
                    if name.starts_with(&prefix) {
                        labels.push(format!("run: {}", name));
                        paths.push(entry.path());
                        break;
                    }
                }
            }
        }
    }

    // 3. Exemplars — only when the dev explicitly opts in.
    if parsed.include_exemplars {
        let ex_dir = paths::exemplars_dir(&parsed.corpus_id);
        for phase in &phases_to_clear {
            let p = ex_dir.join(format!("{}.json", phase.id()));
            if p.exists() {
                labels.push(format!("exemplar bank: {}", phase.id()));
                paths.push(p);
            }
        }
    }

    ResetPlan { labels, paths }
}

/// Every phase from `start` forward in ordinal order, excluding
/// `Ingest` (which has no cache file anyway).
fn downstream_phases(start: PipelinePhase) -> Vec<PipelinePhase> {
    PipelinePhase::ALL
        .iter()
        .copied()
        .filter(|p| *p != PipelinePhase::Ingest && p.ordinal() >= start.ordinal())
        .collect()
}

fn render_plan(plan: &ResetPlan, parsed: &ParsedReset) {
    println!("  Corpus: {}", parsed.corpus_id);
    if parsed.full {
        println!("  Mode: --full (wipe enrichment tree + chapter manifest)");
    } else {
        println!(
            "  Mode: --from {} (downstream phases: {})",
            parsed.from.id(),
            downstream_phases(parsed.from)
                .iter()
                .map(|p| p.id())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !parsed.include_exemplars && !parsed.full {
        println!("  Preserving: per-phase exemplar banks (pass --include-exemplars to clear).");
    }
    if parsed.dry_run {
        println!("  (dry-run — no changes will be made)");
    }
    println!();
    if plan.labels.is_empty() {
        return;
    }
    println!("  Will delete:");
    for label in &plan.labels {
        println!("    - {label}");
    }
    println!();
}

// ── Argument parsing ──────────────────────────────────────────

#[derive(Debug)]
struct ParsedReset {
    corpus_id: String,
    from: PipelinePhase,
    full: bool,
    include_exemplars: bool,
    dry_run: bool,
    yes: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedReset, String> {
    let mut corpus_id: Option<String> = None;
    let mut from: Option<PipelinePhase> = None;
    let mut full = false;
    let mut include_exemplars = false;
    let mut dry_run = false;
    let mut yes = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--from" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--from requires a phase id".to_string())?;
                let p = PipelinePhase::from_str(v).map_err(|e| format!("--from: {e}"))?;
                if p == PipelinePhase::Ingest {
                    return Err(
                        "--from ingest is not a reset starting point (use --full instead)".into(),
                    );
                }
                from = Some(p);
                i += 2;
            }
            "--full" => {
                full = true;
                i += 1;
            }
            "--include-exemplars" => {
                include_exemplars = true;
                i += 1;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--yes" | "-y" => {
                yes = true;
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
                    return Err(format!("unexpected positional: {other}"));
                }
            }
        }
    }

    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    if full && from.is_some() {
        return Err("--full and --from are mutually exclusive".into());
    }
    if full && include_exemplars {
        // Not an error — --full already covers exemplars — but warn the
        // user so they know the rider is redundant.
        eprintln!(
            "note: --include-exemplars is redundant with --full (the full tree already contains exemplars)"
        );
    }
    // Default --from: clear phase 2 onward, preserving phase 1.
    let from = from.unwrap_or(PipelinePhase::QuestionClusters);

    Ok(ParsedReset {
        corpus_id,
        from,
        full,
        include_exemplars,
        dry_run,
        yes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use super::super::test_env::{scoped_home, HomeGuard};

    fn fake_home() -> HomeGuard {
        scoped_home()
    }

    /// Minimal corpus state — config.json + a few phase caches + a run.
    fn scaffold(corpus_id: &str) {
        let enrich = paths::enrichment_root(corpus_id);
        fs::create_dir_all(paths::exemplars_dir(corpus_id)).unwrap();
        fs::create_dir_all(paths::cache_dir(corpus_id)).unwrap();
        fs::create_dir_all(paths::runs_dir(corpus_id)).unwrap();
        fs::create_dir_all(paths::index_root(corpus_id)).unwrap();
        // config
        let cfg = EnrichConfig {
            schema_version: super::super::config::CONFIG_SCHEMA_VERSION,
            corpus_id: corpus_id.into(),
            pipeline_id: "literary".into(),
            source_path: PathBuf::from("/tmp/nonexistent.txt"),
            chapter_regex: "^Chapter".into(),
            chat_model: "c".into(),
            chat_models: None,
            embed_model: "e".into(),
            base_url: "http://localhost:9741".into(),
            min_section_body_words: 0,
            toc_markers: None,
            max_output_tokens: 4096,
            phase1b_max_output_tokens: None,
            phase_overrides: None,
            ontology: None,
            created_at: "t".into(),
        };
        cfg.save().unwrap();
        // phase caches
        for phase in ["questions", "question-clusters", "concerns", "positions"] {
            fs::write(
                paths::cache_dir(corpus_id).join(format!("{phase}.json")),
                b"{}",
            )
            .unwrap();
        }
        // runs
        for name in [
            "questions-full-001.json",
            "questions-subset-001.json",
            "concerns-full-001.json",
            "positions-full-001.json",
        ] {
            fs::write(paths::runs_dir(corpus_id).join(name), b"{}").unwrap();
        }
        // exemplars
        fs::write(
            paths::exemplars_dir(corpus_id).join("questions.json"),
            b"{\"phase\":\"questions\",\"schema_version\":1,\"exemplars\":[]}",
        )
        .unwrap();
        // chapter manifest
        fs::write(paths::chapters_manifest_path(corpus_id), b"{}").unwrap();
        // Prevent dead-code warnings on `enrich` in non-test builds.
        let _ = enrich;
    }

    fn path_exists(p: &Path) -> bool {
        p.exists()
    }

    #[test]
    fn parse_default_clears_from_question_clusters() {
        let p = parse_args(&["ak".into()]).unwrap();
        assert_eq!(p.from, PipelinePhase::QuestionClusters);
        assert!(!p.full);
        assert!(!p.include_exemplars);
    }

    #[test]
    fn parse_rejects_full_plus_from() {
        let err = parse_args(&[
            "ak".into(),
            "--full".into(),
            "--from".into(),
            "positions".into(),
        ])
        .unwrap_err();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn parse_rejects_from_ingest() {
        let err = parse_args(&["ak".into(), "--from".into(), "ingest".into()]).unwrap_err();
        assert!(err.contains("ingest"));
    }

    #[test]
    fn parse_accepts_all_flags() {
        let p = parse_args(
            &[
                "ak",
                "--from",
                "positions",
                "--include-exemplars",
                "--dry-run",
                "-y",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(p.from, PipelinePhase::Positions);
        assert!(p.include_exemplars);
        assert!(p.dry_run);
        assert!(p.yes);
    }

    #[test]
    fn plan_default_keeps_phase_1_and_exemplars() {
        let _home = fake_home();
        scaffold("t1");
        let parsed = parse_args(&["t1".into()]).unwrap();
        let plan = build_plan(&parsed);
        let labels: String = plan.labels.join("\n");
        // Phase 1 cache should NOT be in the plan.
        assert!(!labels.contains("cache: questions"), "plan:\n{labels}");
        // Exemplars should NOT be in the plan by default.
        assert!(!labels.contains("exemplar bank"), "plan:\n{labels}");
        // Phase 2+ caches that exist ARE in the plan.
        assert!(labels.contains("cache: question-clusters"));
        assert!(labels.contains("cache: concerns"));
        assert!(labels.contains("cache: positions"));
        // Phase 1 subset/full runs should NOT be deleted.
        assert!(!labels.contains("questions-full-001"));
        assert!(!labels.contains("questions-subset-001"));
        // Downstream runs ARE deleted.
        assert!(labels.contains("concerns-full-001"));
        assert!(labels.contains("positions-full-001"));
    }

    #[test]
    fn plan_from_positions_clears_5_6_7_only() {
        let _home = fake_home();
        scaffold("t2");
        let parsed = parse_args(&["t2".into(), "--from".into(), "positions".into()]).unwrap();
        let plan = build_plan(&parsed);
        let labels = plan.labels.join("\n");
        assert!(labels.contains("cache: positions"));
        assert!(!labels.contains("cache: concerns"));
        assert!(!labels.contains("cache: question-clusters"));
    }

    #[test]
    fn plan_include_exemplars_adds_banks() {
        let _home = fake_home();
        scaffold("t3");
        let parsed = parse_args(&[
            "t3".into(),
            "--from".into(),
            "questions".into(),
            "--include-exemplars".into(),
        ])
        .unwrap();
        let plan = build_plan(&parsed);
        let labels = plan.labels.join("\n");
        assert!(labels.contains("exemplar bank: questions"));
    }

    #[test]
    fn plan_full_lists_tree_and_manifest() {
        let _home = fake_home();
        scaffold("t4");
        let parsed = parse_args(&["t4".into(), "--full".into()]).unwrap();
        let plan = build_plan(&parsed);
        let labels = plan.labels.join("\n");
        assert!(labels.contains("entire enrichment tree"));
        assert!(labels.contains("chapter manifest"));
    }

    #[tokio::test]
    async fn dry_run_changes_nothing() {
        let _home = fake_home();
        scaffold("t5");
        let before_cache = paths::cache_dir("t5").join("concerns.json");
        assert!(path_exists(&before_cache));
        let code = cmd_reset(&["t5".into(), "--dry-run".into(), "--yes".into()]).await;
        assert_eq!(code, 0);
        assert!(path_exists(&before_cache), "dry-run must not delete files");
    }

    #[tokio::test]
    async fn default_reset_preserves_phase_1_cache() {
        let _home = fake_home();
        scaffold("t6");
        let phase1_cache = paths::cache_dir("t6").join("questions.json");
        let downstream = paths::cache_dir("t6").join("concerns.json");
        let phase1_run = paths::runs_dir("t6").join("questions-full-001.json");
        let downstream_run = paths::runs_dir("t6").join("concerns-full-001.json");
        let exemplar = paths::exemplars_dir("t6").join("questions.json");

        let code = cmd_reset(&["t6".into(), "--yes".into()]).await;
        assert_eq!(code, 0);

        assert!(path_exists(&phase1_cache));
        assert!(path_exists(&phase1_run));
        assert!(path_exists(&exemplar));
        assert!(!path_exists(&downstream));
        assert!(!path_exists(&downstream_run));
    }

    #[tokio::test]
    async fn full_wipes_tree_and_manifest() {
        let _home = fake_home();
        scaffold("t7");
        let enrich = paths::enrichment_root("t7");
        let manifest = paths::chapters_manifest_path("t7");
        let code = cmd_reset(&["t7".into(), "--full".into(), "--yes".into()]).await;
        assert_eq!(code, 0);
        assert!(!enrich.exists());
        assert!(!manifest.exists());
    }

    #[tokio::test]
    async fn reset_refuses_unknown_corpus() {
        let _home = fake_home();
        // No scaffold — config.json does not exist.
        let code = cmd_reset(&["not-a-corpus".into(), "--yes".into()]).await;
        assert_eq!(code, 1);
    }
}
