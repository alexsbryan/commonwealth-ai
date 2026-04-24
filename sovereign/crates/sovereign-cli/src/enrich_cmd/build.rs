//! `sovereign enrich build` — one-shot atlas pipeline driver.
//!
//! Runs the full atlas flow against a corpus in the expected order
//! with step-by-step progress:
//!
//!   1. seed               — Stage 1a entity list
//!   2. extract            — Phase 1 per-section atlas extraction
//!      (cached on a `--full` run so later phases can consume it;
//!       a subset run is promoted to cache in-place so downstream
//!       phases have inputs)
//!   3. cluster            — Phase 2 facet-typed clustering
//!   4. name               — Phase 3 per-facet cluster naming
//!   5. resolve            — Phase 3a/3b atoms + edges + trajectories
//!   6. tensions           — Phase 6 deterministic candidate selection
//!   7. gaps               — Phase 7 deterministic gap detection
//!   8. configure          — Phase 8 (LLM, opt-in per pipeline)
//!   9. report             — §12 schema validation table
//!
//! Each step invokes the same underlying `cmd_*` function used by
//! the standalone CLI verbs, so orchestrated behaviour matches a
//! manual sequence exactly. A step's failure stops the flow and
//! returns its exit code.

use super::{
    atlas_configuration, atlas_gaps, atlas_phase_cmd, atlas_resolve, atlas_tensions,
    config::EnrichConfig, extract, paths, schema_review, seed_cmd,
};
use corpus_engine::enrichment::pipeline::{PipelineRegistry, SeedStrategy};
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich build",
    summary: "Run the full atlas enrichment flow for a corpus in one command.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich build <corpus-id> [--chapters <ids> | --full] [--skip <step>...] [--dry-run]",
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
                 tensions, gaps, configure, report. Repeatable.",
            ),
            (
                "--dry-run",
                "Print the planned step sequence and exit without running anything.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich build brothers_karamazov --full",
                "Full end-to-end build on the whole corpus.",
            ),
            (
                "sovereign enrich build process_philosophy --chapters sec_0001,sec_0002,sec_0003",
                "Subset build — useful for iterating on a tiny validation slice.",
            ),
            (
                "sovereign enrich build bk --skip configure",
                "Skip the LLM Phase 8 configuration step (fastest path to resolved atlas + report).",
            ),
        ]),
        HelpSection::Notes(
            "Requires `sovereign enrich init <corpus>` first. Phase 8 (configure) is \
             skipped automatically if the pipeline hasn't opted in via \
             `runs_configuration_phase()`. Any step failure stops the flow with that \
             step's exit code.",
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

    // Load the pipeline so we can validate the corpus is atlas-
    // shaped AND drop steps the pipeline explicitly opts out of
    // (seed-less atlas variants; pipelines that don't run Phase 8).
    let capabilities = match load_pipeline_capabilities(&parsed.corpus_id) {
        Ok(c) => c,
        Err((code, msg)) => {
            eprintln!("error: {msg}");
            return code;
        }
    };

    let plan = Plan::new(&parsed, &capabilities);
    if parsed.dry_run {
        plan.print_dry_run();
        return 0;
    }

    // Banner.
    println!("=== enrich build — {} ===", parsed.corpus_id);
    if !plan.auto_skipped.is_empty() {
        println!(
            "  pipeline `{}` auto-skips: {}",
            capabilities.pipeline_id,
            plan.auto_skipped
                .iter()
                .map(|s| s.label())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let total = plan.enabled_steps().count();
    println!("  {total} step(s) planned");
    for (i, step) in plan.enabled_steps().enumerate() {
        println!("    {}. {}", i + 1, step.label());
    }
    println!();

    // Run each step.
    for (i, step) in plan.enabled_steps().enumerate() {
        println!("─── [{}/{}] {} ───", i + 1, total, step.label());
        let code = run_step(step, &parsed).await;
        if code != 0 {
            eprintln!();
            eprintln!(
                "error: step `{}` exited with code {code}. Build stopped.",
                step.label()
            );
            return code;
        }
        println!();
    }

    println!("=== build complete — {} ===", parsed.corpus_id);
    0
}

async fn run_step(step: Step, parsed: &ParsedBuild) -> i32 {
    let corpus = parsed.corpus_id.as_str();
    match step {
        Step::Seed => seed_cmd::cmd_seed(&[corpus.into()]).await,
        Step::Extract => {
            let mut args: Vec<String> = vec![corpus.into()];
            match &parsed.selection {
                Selection::Full => args.push("--full".into()),
                Selection::Chapters(ids) => {
                    args.push("--chapters".into());
                    args.push(ids.join(","));
                }
            }
            let code = extract::cmd_extract(&args).await;
            if code != 0 {
                return code;
            }
            // On a subset run, promote to cache so downstream
            // phases have input. `--full` already updates the cache.
            if matches!(&parsed.selection, Selection::Chapters(_)) {
                if let Err(e) = promote_subset_to_cache(corpus) {
                    eprintln!("error: promoting subset run to cache: {e}");
                    return 1;
                }
                println!("  · promoted subset run → cache/questions.json");
            }
            0
        }
        Step::Cluster => atlas_phase_cmd::cmd_cluster_atlas(&[corpus.into()]).await,
        Step::Name => atlas_phase_cmd::cmd_name_atlas_clusters(&[corpus.into()]).await,
        Step::Resolve => {
            atlas_resolve::cmd_atlas_resolve(&[corpus.into(), "--phase".into(), "all".into()])
                .await
        }
        Step::Tensions => atlas_tensions::cmd_atlas_tensions(&[corpus.into()]).await,
        Step::Gaps => atlas_gaps::cmd_atlas_gaps(&[corpus.into()]).await,
        Step::Configure => {
            atlas_configuration::cmd_atlas_configuration(&[corpus.into()]).await
        }
        Step::Report => schema_review::cmd_schema_report(&[corpus.into()]).await,
    }
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

// ── Plan + step enum ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Seed,
    Extract,
    Cluster,
    Name,
    Resolve,
    Tensions,
    Gaps,
    Configure,
    Report,
}

impl Step {
    fn label(&self) -> &'static str {
        match self {
            Step::Seed => "seed",
            Step::Extract => "extract",
            Step::Cluster => "cluster",
            Step::Name => "name",
            Step::Resolve => "resolve",
            Step::Tensions => "tensions",
            Step::Gaps => "gaps",
            Step::Configure => "configure",
            Step::Report => "report",
        }
    }

    fn from_label(s: &str) -> Option<Step> {
        match s {
            "seed" => Some(Step::Seed),
            "extract" => Some(Step::Extract),
            "cluster" => Some(Step::Cluster),
            "name" => Some(Step::Name),
            "resolve" => Some(Step::Resolve),
            "tensions" => Some(Step::Tensions),
            "gaps" => Some(Step::Gaps),
            "configure" => Some(Step::Configure),
            "report" => Some(Step::Report),
            _ => None,
        }
    }

    fn all() -> &'static [Step] {
        &[
            Step::Seed,
            Step::Extract,
            Step::Cluster,
            Step::Name,
            Step::Resolve,
            Step::Tensions,
            Step::Gaps,
            Step::Configure,
            Step::Report,
        ]
    }
}

/// Pipeline-level capabilities the orchestrator needs to shape
/// the plan. Loaded once at build start from the corpus's
/// enrichment config + the pipeline registry.
pub(super) struct PipelineCapabilities {
    pub pipeline_id: String,
    pub seed_strategy_none: bool,
    pub runs_configuration_phase: bool,
}

fn load_pipeline_capabilities(
    corpus_id: &str,
) -> Result<PipelineCapabilities, (i32, String)> {
    let cfg = EnrichConfig::require(corpus_id)
        .map_err(|e| (1, format!("loading enrichment config for `{corpus_id}`: {e}")))?;
    let registry = PipelineRegistry::builtin();
    let pipeline = registry.get(&cfg.pipeline_id).ok_or_else(|| {
        (
            1,
            format!(
                "unknown pipeline `{}` in this corpus's config (known: {:?})",
                cfg.pipeline_id,
                registry.pipeline_ids()
            ),
        )
    })?;
    // The atlas flow presumes an atlas-shaped pipeline — Phases
    // 2-8 require `section_extraction` payloads that the legacy
    // `literary` pipeline doesn't emit. Fail loudly at start
    // with an actionable remediation rather than crashing
    // mid-flow.
    if !cfg.pipeline_id.ends_with("_atlas") {
        return Err((
            2,
            format!(
                "pipeline `{}` is a legacy (non-atlas) pipeline; `build` only supports \
                 atlas pipelines. Re-init with `sovereign enrich reset {corpus_id} --full \
                 --yes` followed by `sovereign enrich init {corpus_id} --source <path> \
                 --pipeline literary_atlas` (or `--pipeline philosophy_atlas`), then \
                 retry.",
                cfg.pipeline_id
            ),
        ));
    }
    Ok(PipelineCapabilities {
        pipeline_id: cfg.pipeline_id.clone(),
        seed_strategy_none: matches!(pipeline.seed_strategy(), SeedStrategy::None),
        runs_configuration_phase: pipeline.runs_configuration_phase(),
    })
}

struct Plan {
    enabled: Vec<Step>,
    /// Steps dropped because the pipeline explicitly opts out —
    /// e.g. a seed-less atlas variant, or a pipeline that doesn't
    /// run Phase 8. Surfaced in the banner so an operator sees
    /// the pipeline-driven subset without thinking the
    /// orchestrator silently lost steps.
    auto_skipped: Vec<Step>,
}

impl Plan {
    fn new(parsed: &ParsedBuild, caps: &PipelineCapabilities) -> Self {
        let mut auto_skipped: Vec<Step> = Vec::new();
        if caps.seed_strategy_none {
            auto_skipped.push(Step::Seed);
        }
        if !caps.runs_configuration_phase {
            auto_skipped.push(Step::Configure);
        }
        let enabled = Step::all()
            .iter()
            .copied()
            .filter(|s| !parsed.skipped.contains(s) && !auto_skipped.contains(s))
            .collect();
        Self {
            enabled,
            auto_skipped,
        }
    }

    fn enabled_steps(&self) -> impl Iterator<Item = Step> + '_ {
        self.enabled.iter().copied()
    }

    fn print_dry_run(&self) {
        if !self.auto_skipped.is_empty() {
            println!(
                "  auto-skipped (pipeline opts out): {}",
                self.auto_skipped
                    .iter()
                    .map(|s| s.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("  planned steps ({}):", self.enabled.len());
        for (i, s) in self.enabled.iter().enumerate() {
            println!("    {}. {}", i + 1, s.label());
        }
    }
}

// ── Arg parsing ──────────────────────────────────────────────

#[derive(Debug)]
enum Selection {
    Full,
    Chapters(Vec<String>),
}

#[derive(Debug)]
struct ParsedBuild {
    corpus_id: String,
    selection: Selection,
    skipped: Vec<Step>,
    dry_run: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedBuild, String> {
    let mut corpus_id: Option<String> = None;
    let mut chapters: Option<Vec<String>> = None;
    let mut full = false;
    let mut skipped: Vec<Step> = Vec::new();
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--full" => {
                full = true;
                i += 1;
            }
            "--chapters" => {
                let raw = args.get(i + 1).ok_or_else(|| {
                    "--chapters requires a comma-separated id list".to_string()
                })?;
                chapters = Some(
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
                i += 2;
            }
            "--skip" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--skip requires a step name".to_string())?;
                let step = Step::from_label(raw).ok_or_else(|| {
                    format!(
                        "unknown step `{raw}` for --skip (valid: seed, extract, cluster, \
                         name, resolve, tensions, gaps, configure, report)"
                    )
                })?;
                if !skipped.contains(&step) {
                    skipped.push(step);
                }
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                corpus_id = Some(other.to_string());
                i += 1;
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let selection = match (full, chapters) {
        (true, Some(_)) => {
            return Err("use either --full or --chapters, not both".to_string());
        }
        (true, None) => Selection::Full,
        (false, Some(ids)) if ids.is_empty() => {
            return Err("--chapters list is empty".to_string());
        }
        (false, Some(ids)) => Selection::Chapters(ids),
        (false, None) => Selection::Full, // default
    };
    Ok(ParsedBuild {
        corpus_id,
        selection,
        skipped,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_to_full_selection() {
        let p = parse_args(&["bk".into()]).unwrap();
        assert!(matches!(p.selection, Selection::Full));
        assert!(!p.dry_run);
        assert!(p.skipped.is_empty());
    }

    #[test]
    fn parse_accepts_chapter_subset() {
        let p = parse_args(&[
            "bk".into(),
            "--chapters".into(),
            "sec_0001,sec_0002".into(),
        ])
        .unwrap();
        match p.selection {
            Selection::Chapters(ids) => {
                assert_eq!(ids, vec!["sec_0001", "sec_0002"]);
            }
            _ => panic!("expected Chapters selection"),
        }
    }

    #[test]
    fn parse_rejects_both_full_and_chapters() {
        let err = parse_args(&[
            "bk".into(),
            "--full".into(),
            "--chapters".into(),
            "sec_0001".into(),
        ])
        .unwrap_err();
        assert!(err.contains("either --full or --chapters"));
    }

    #[test]
    fn parse_accepts_repeated_skip_flag() {
        let p = parse_args(&[
            "bk".into(),
            "--skip".into(),
            "configure".into(),
            "--skip".into(),
            "tensions".into(),
        ])
        .unwrap();
        assert!(p.skipped.contains(&Step::Configure));
        assert!(p.skipped.contains(&Step::Tensions));
    }

    #[test]
    fn parse_rejects_unknown_skip_name() {
        let err =
            parse_args(&["bk".into(), "--skip".into(), "banana".into()]).unwrap_err();
        assert!(err.contains("unknown step"));
    }

    #[test]
    fn parse_dry_run_flag() {
        let p = parse_args(&["bk".into(), "--dry-run".into()]).unwrap();
        assert!(p.dry_run);
    }

    #[test]
    fn parse_rejects_missing_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    /// Every capability flag true — no steps auto-skipped. The
    /// pipeline registry is not hit in these unit tests so we
    /// build the struct directly.
    fn full_capabilities() -> PipelineCapabilities {
        PipelineCapabilities {
            pipeline_id: "literary_atlas".into(),
            seed_strategy_none: false,
            runs_configuration_phase: true,
        }
    }

    #[test]
    fn plan_respects_skip_filter() {
        let parsed = parse_args(&[
            "bk".into(),
            "--skip".into(),
            "configure".into(),
            "--skip".into(),
            "tensions".into(),
        ])
        .unwrap();
        let plan = Plan::new(&parsed, &full_capabilities());
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(!labels.contains(&"configure"));
        assert!(!labels.contains(&"tensions"));
        // And the ones that survive retain their canonical order.
        assert_eq!(labels[0], "seed");
        assert_eq!(labels[1], "extract");
    }

    #[test]
    fn plan_default_contains_every_step() {
        let parsed = parse_args(&["bk".into()]).unwrap();
        let plan = Plan::new(&parsed, &full_capabilities());
        assert_eq!(plan.enabled_steps().count(), Step::all().len());
    }

    #[test]
    fn plan_auto_skips_seed_when_pipeline_declares_none_strategy() {
        let parsed = parse_args(&["bk".into()]).unwrap();
        let caps = PipelineCapabilities {
            pipeline_id: "atlas_structural".into(),
            seed_strategy_none: true,
            runs_configuration_phase: true,
        };
        let plan = Plan::new(&parsed, &caps);
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(
            !labels.contains(&"seed"),
            "seed should be auto-skipped when seed_strategy is None"
        );
        assert!(plan.auto_skipped.contains(&Step::Seed));
    }

    #[test]
    fn plan_auto_skips_configure_when_pipeline_opts_out() {
        let parsed = parse_args(&["bk".into()]).unwrap();
        let caps = PipelineCapabilities {
            pipeline_id: "minimal_atlas".into(),
            seed_strategy_none: false,
            runs_configuration_phase: false,
        };
        let plan = Plan::new(&parsed, &caps);
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(
            !labels.contains(&"configure"),
            "configure should be auto-skipped when runs_configuration_phase is false"
        );
        assert!(plan.auto_skipped.contains(&Step::Configure));
    }

    #[test]
    fn plan_auto_skip_and_manual_skip_both_apply() {
        // Explicit --skip + capability-auto-skip compose —
        // neither hides the other, both land in their
        // respective categories.
        let parsed = parse_args(&[
            "bk".into(),
            "--skip".into(),
            "tensions".into(),
        ])
        .unwrap();
        let caps = PipelineCapabilities {
            pipeline_id: "minimal_atlas".into(),
            seed_strategy_none: true,
            runs_configuration_phase: false,
        };
        let plan = Plan::new(&parsed, &caps);
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(!labels.contains(&"seed"));
        assert!(!labels.contains(&"tensions"));
        assert!(!labels.contains(&"configure"));
        assert!(plan.auto_skipped.contains(&Step::Seed));
        assert!(plan.auto_skipped.contains(&Step::Configure));
        // --skip tensions is NOT a capability-driven skip, so it
        // doesn't show up in auto_skipped.
        assert!(!plan.auto_skipped.contains(&Step::Tensions));
    }
}
