// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the build will run: the step vocabulary, the plan, and the parsed
//! invocation that produces both.
//!
//! `Step` is the closed set of phases; `Plan` is the subset this invocation
//! will actually run, after the operator's `--skip` flags and the pipeline's
//! own opt-outs; `ParsedBuild` is the request, constructible from argv
//! (`parse_args`) or from typed inputs (`ParsedBuild::from_inputs`, which the
//! daemon uses).

use crate::config::EnrichConfig;
use corpus_engine::enrichment::pipeline::{
    progress::wire, BuildStep, EnrichProgress, EnrichProgressFn, PipelineRegistry, SeedStrategy,
};

// ── Plan + step enum ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Seed,
    Extract,
    Cluster,
    Name,
    Resolve,
    Tensions,
    Gaps,
    Configure,
    Report,
    /// Last, always: it reads the atlas every step before it wrote.
    Backfill,
}

impl Step {
    /// Cross-crate representation of this step for the progress
    /// event stream (`corpus_engine::enrichment::pipeline::BuildStep`).
    /// Keep in lockstep with `Step::label` — the canonical id string
    /// comes from `BuildStep::id` to avoid two sources of truth.
    pub(super) fn to_build_step(self) -> BuildStep {
        match self {
            Step::Seed => BuildStep::Seed,
            Step::Extract => BuildStep::Extract,
            Step::Cluster => BuildStep::Cluster,
            Step::Name => BuildStep::Name,
            Step::Resolve => BuildStep::Resolve,
            Step::Tensions => BuildStep::Tensions,
            Step::Gaps => BuildStep::Gaps,
            Step::Configure => BuildStep::Configure,
            Step::Report => BuildStep::Report,
            Step::Backfill => BuildStep::Backfill,
        }
    }

    pub fn label(&self) -> &'static str {
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
            Step::Backfill => "backfill",
        }
    }

    /// The `--skip` vocabulary, rendered from `all()` so the parser error and
    /// the desktop constructor cannot list different steps than the plan runs.
    /// (`HELP` is a `const` and repeats the list as a literal; a test below
    /// holds the two together.)
    fn valid_labels() -> String {
        Self::all()
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", ")
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
            "backfill" => Some(Step::Backfill),
            _ => None,
        }
    }

    pub fn all() -> &'static [Step] {
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
            Step::Backfill,
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

pub(super) fn load_pipeline_capabilities(
    corpus_id: &str,
) -> Result<PipelineCapabilities, (i32, String)> {
    let cfg = EnrichConfig::require(corpus_id).map_err(|e| {
        (
            1,
            format!("loading enrichment config for `{corpus_id}`: {e}"),
        )
    })?;
    let registry = PipelineRegistry::builtin();
    let pipeline = crate::pipeline_resolve::resolve_pipeline(&cfg).ok_or_else(|| {
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
                 atlas pipelines. Re-init with `svrn enrich reset {corpus_id} --full \
                 --yes` followed by `svrn enrich init {corpus_id} --source <path> \
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

pub(super) struct Plan {
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

#[derive(Debug, Clone)]
pub enum Selection {
    Full,
    Chapters(Vec<String>),
}

/// Parsed `enrich build` invocation. Exposed publicly so external
/// callers (desktop app) can construct one without going through
/// argv parsing.
#[derive(Debug, Clone)]
pub struct ParsedBuild {
    pub corpus_id: String,
    pub selection: Selection,
    /// Step labels the caller explicitly asked to skip (via
    /// `--skip <label>` on the CLI, or by inserting values
    /// manually in the desktop path). Pipeline-capability auto
    /// skips land separately in `Plan::auto_skipped`.
    pub(super) skipped: Vec<Step>,
    pub dry_run: bool,
}

impl ParsedBuild {
    /// Construct a `ParsedBuild` without going through argv.
    /// Intended for the desktop Tauri layer, which receives typed
    /// inputs (a corpus id + an optional chapter list + a set of
    /// step-ids to skip).
    ///
    /// `skip_step_ids` accepts the step-id strings exposed by
    /// `BuildStep::id` (`seed`, `extract`, …). Unknown ids are
    /// rejected — silent ignore would be a footgun when a typo
    /// lets Phase 8 run on a corpus the operator meant to exclude.
    #[allow(dead_code)] // Used by the desktop enrich_commands layer once it lands.
    pub fn from_inputs(
        corpus_id: impl Into<String>,
        chapters: Option<Vec<String>>,
        skip_step_ids: &[String],
        dry_run: bool,
    ) -> Result<Self, String> {
        let selection = match chapters {
            Some(ids) if ids.is_empty() => {
                return Err("chapter list is empty".into());
            }
            Some(ids) => Selection::Chapters(ids),
            None => Selection::Full,
        };
        let mut skipped: Vec<Step> = Vec::new();
        for id in skip_step_ids {
            let step = Step::from_label(id).ok_or_else(|| {
                format!("unknown skip step `{id}` (valid: {})", Step::valid_labels())
            })?;
            if !skipped.contains(&step) {
                skipped.push(step);
            }
        }
        Ok(Self {
            corpus_id: corpus_id.into(),
            selection,
            skipped,
            dry_run,
        })
    }

    /// Does this invocation need an embed provider in hand before step 1?
    ///
    /// The ONE decider for that question (ARCH §10.6), so the CLI wrapper and
    /// the orchestrator cannot disagree about it. `false` for a dry run — no
    /// step executes — and `false` whenever Backfill is not in the plan,
    /// whether the operator skipped it (`--skip backfill`) or the pipeline
    /// opted out. Both cases must build with no daemon reachable at all.
    ///
    /// The daemon never asks: it always holds its own provider.
    pub fn needs_backfill_embedder(&self) -> Result<bool, (i32, String)> {
        if self.dry_run {
            return Ok(false);
        }
        let caps = load_pipeline_capabilities(&self.corpus_id)?;
        Ok(Plan::new(self, &caps).enabled.contains(&Step::Backfill))
    }
}

pub fn parse_args(args: &[String]) -> Result<ParsedBuild, String> {
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
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--chapters requires a comma-separated id list".to_string())?;
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
                        "unknown step `{raw}` for --skip (valid: {})",
                        Step::valid_labels()
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
        let p =
            parse_args(&["bk".into(), "--chapters".into(), "sec_0001,sec_0002".into()]).unwrap();
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
        let err = parse_args(&["bk".into(), "--skip".into(), "banana".into()]).unwrap_err();
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
    /// `--skip <label>` must accept exactly the labels the plan runs, in
    /// both directions, for every step — a variant added to `all()` without
    /// a `from_label` arm would be un-skippable and this catches it.
    #[test]
    fn from_label_round_trips_every_step() {
        for step in Step::all() {
            assert_eq!(
                Step::from_label(step.label()),
                Some(*step),
                "label `{}` must parse back to {step:?}",
                step.label()
            );
        }
        assert_eq!(Step::from_label("backfill"), Some(Step::Backfill));
    }
    /// Backfill reads the atlas every other step wrote, so it is last in
    /// `all()` and last in every plan — including one whose pipeline
    /// auto-skips Seed and Configure — and `--skip backfill` removes it.
    #[test]
    fn backfill_is_the_last_step_in_every_plan() {
        assert_eq!(Step::all().last(), Some(&Step::Backfill));
        let caps = PipelineCapabilities {
            pipeline_id: "custom_atlas".into(),
            seed_strategy_none: true,
            runs_configuration_phase: false,
        };
        let parsed = ParsedBuild::from_inputs("bk", None, &[], false).unwrap();
        let plan = Plan::new(&parsed, &caps);
        assert_eq!(plan.enabled.last(), Some(&Step::Backfill));
        assert_eq!(plan.enabled.len(), Step::all().len() - 2);

        let skipped = ParsedBuild::from_inputs("bk", None, &["backfill".into()], false).unwrap();
        let plan = Plan::new(&skipped, &caps);
        assert!(!plan.enabled.contains(&Step::Backfill));
        assert_eq!(plan.enabled.last(), Some(&Step::Report));
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
        let parsed = parse_args(&["bk".into(), "--skip".into(), "tensions".into()]).unwrap();
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
    #[test]
    fn from_inputs_matches_parse_args_for_equivalent_cli() {
        // Desktop builds ParsedBuild via `from_inputs`; CLI
        // via `parse_args`. Equivalent inputs must produce
        // identical ParsedBuild shapes — otherwise the progress
        // stream diverges between the two frontends.
        let cli = parse_args(&[
            "bk".into(),
            "--chapters".into(),
            "sec_0001,sec_0002".into(),
            "--skip".into(),
            "configure".into(),
        ])
        .unwrap();
        let desktop = ParsedBuild::from_inputs(
            "bk",
            Some(vec!["sec_0001".into(), "sec_0002".into()]),
            &["configure".into()],
            false,
        )
        .unwrap();
        assert_eq!(cli.corpus_id, desktop.corpus_id);
        assert_eq!(cli.dry_run, desktop.dry_run);
        assert_eq!(cli.skipped, desktop.skipped);
        match (cli.selection, desktop.selection) {
            (Selection::Chapters(a), Selection::Chapters(b)) => assert_eq!(a, b),
            other => panic!("expected matching chapter selections, got {other:?}"),
        }
    }
    #[test]
    fn from_inputs_rejects_unknown_skip_id() {
        // Typos in the skip list are operator errors — surfacing
        // as an Err prevents a UI dialog that silently runs a
        // phase the operator thought it had excluded.
        let err = ParsedBuild::from_inputs("bk", None, &["configure".into(), "nope".into()], false)
            .unwrap_err();
        assert!(err.contains("nope"));
    }
    #[test]
    fn from_inputs_rejects_empty_chapter_list() {
        let err = ParsedBuild::from_inputs("bk", Some(vec![]), &[], false).unwrap_err();
        assert!(err.contains("empty"));
    }
}
