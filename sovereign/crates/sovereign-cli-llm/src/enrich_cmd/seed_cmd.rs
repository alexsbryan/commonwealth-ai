// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich seed` — Stage 1a driver.
//!
//! Runs the pipeline's seed-extraction strategy against the first
//! section of a corpus and writes the seed entity list to
//! `cache/seed.json`. Subsequent `svrn enrich extract` runs
//! read this file and thread the canonical-names block into every
//! per-chapter Phase 1 prompt.
//!
//! Idempotent: the runner caches the seed and short-circuits on
//! cache hit unless `--force` is passed.

use corpus_engine::enrichment::pipeline::{
    PhaseRunner, PipelineRegistry, RunOutputWriter, SeedEntities, SeedStrategy,
};

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich seed",
    summary: "Stage 1a: extract the seed entity list from the first section.",
    sections: &[
        HelpSection::Usage("svrn enrich seed <corpus-id> [--force]"),
        HelpSection::Flags(&[(
            "--force",
            "Recompute even when a seed list is already cached. Useful when the opening \
             section has been edited or the pipeline's seed prompt has changed.",
        )]),
        HelpSection::Examples(&[
            (
                "svrn enrich seed brothers_karamazov",
                "Read chapter 1, emit canonical entity list, cache to cache/seed.json.",
            ),
            (
                "svrn enrich seed bk --force",
                "Re-run even if the seed cache is warm.",
            ),
        ]),
        HelpSection::Notes(
            "Every subsequent `svrn enrich extract` call reads the cached seed and \
             threads the canonical-names block into every per-chapter Phase 1 prompt. \
             This is what keeps `Fyodor Pavlovich Karamazov` from fragmenting into \
             `Fyodor Karam`, `Fyo Karamzov`, and similar variants across chapters.",
        ),
    ],
};

/// Why seeding stopped.
///
/// The `i32` return collapsed two different things into "nonzero": a
/// pipeline that has no seed step at all (a USAGE error — nothing went
/// wrong, the caller asked for something this pipeline does not have),
/// and a seed attempt that failed. They exit with different codes and
/// mean different things to an orchestrator, so they are different
/// values (ARCH §18.3).
#[derive(Debug, Clone)]
pub enum SeedError {
    /// The corpus's pipeline declares `SeedStrategy::None`.
    NoSeedStrategy { pipeline_id: String },
    /// Seeding was attempted and failed.
    Failed(String),
}

impl SeedError {
    /// The code `svrn enrich seed` exits with. 2 for usage, 1 for failure.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NoSeedStrategy { .. } => 2,
            Self::Failed(_) => 1,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NoSeedStrategy { pipeline_id } => format!(
                "pipeline `{pipeline_id}` declares SeedStrategy::None — it does not use a \
                 seed entity list. Either switch to an atlas pipeline (e.g. \
                 `literary_atlas`) that does, or skip this step."
            ),
            Self::Failed(m) => m.clone(),
        }
    }
}

/// What the seed step produced.
#[derive(Debug, Clone)]
pub struct SeedReport {
    pub seed: SeedEntities,
    /// The section stage 1a read. Named on the report because "12 entities
    /// from sec_0001" and "12 entities from sec_0093" are different runs.
    pub first_section: String,
    pub cache_path: std::path::PathBuf,
    pub forced: bool,
}

impl SeedReport {
    /// One line naming what this step found, for the build orchestrator's
    /// `StepDone` event.
    pub fn summary(&self) -> String {
        let forced = if self.forced { " (forced)" } else { "" };
        format!(
            "{} seed entity(ies) from {} (origin {:?}){forced}",
            self.seed.entries.len(),
            self.first_section,
            self.seed.origin
        )
    }
}

/// Run stage 1a and write the seed list.
///
/// Unlike the deterministic steps, this one keeps its two progress
/// `println!`s: the LLM call in the middle can take a while, and a line
/// before it is the operator's only sign the run is alive. The RESULT
/// printing is [`render`]'s.
pub async fn run(parsed: &ParsedSeed) -> Result<SeedReport, SeedError> {
    let cfg = EnrichConfig::require(&parsed.corpus_id)
        .map_err(|e| SeedError::Failed(format!("loading enrichment config: {e}")))?;

    let registry = PipelineRegistry::builtin();
    let Some(pipeline) = super::pipeline_resolve::resolve_pipeline(&cfg) else {
        return Err(SeedError::Failed(format!(
            "unknown pipeline `{}`; known ids: {:?}",
            cfg.pipeline_id,
            registry.pipeline_ids()
        )));
    };

    // Check the strategy before rebuilding corpus state so we can
    // fail fast on pipelines that don't support seed extraction.
    match pipeline.seed_strategy() {
        SeedStrategy::None => {
            return Err(SeedError::NoSeedStrategy {
                pipeline_id: cfg.pipeline_id.clone(),
            })
        }
        SeedStrategy::Llm | SeedStrategy::Structural => {}
    }

    // Rebuild chapter inputs — we need the first section's text
    // for LLM-strategy pipelines and the full corpus context for
    // Structural-strategy ones.
    let (inputs, _manifest) = rebuild_corpus_state(&cfg)
        .map_err(|e| SeedError::Failed(format!("rebuilding corpus state: {e}")))?;
    if inputs.is_empty() {
        return Err(SeedError::Failed("corpus has no sections".into()));
    }

    let client = DaemonInferenceClient::from_enrich_config(&cfg)
        .map_err(|e| SeedError::Failed(format!("building daemon client: {e}")))?;
    let (embed, chat) = client.into_closures();

    let cache = cfg.phase_cache();
    let runs = RunOutputWriter::new(paths::runs_dir(&cfg.corpus_id));
    let runner = PhaseRunner::new(
        pipeline,
        embed,
        chat,
        cache,
        runs,
        paths::exemplars_dir(&cfg.corpus_id),
    );

    let ctx = corpus_engine::enrichment::pipeline::CorpusContext {
        chapter_titles: inputs.iter().map(|c| c.title.clone()).collect(),
        chapters: inputs.clone(),
        chunks: Vec::new(),
    };

    let first_section = inputs[0].chapter_id.clone();
    println!("  running stage 1a (seed) on first section: {first_section}");
    if parsed.force {
        println!("  · --force: recomputing even if cache is warm");
    }

    let seed = match runner
        .phase_1a_extract_seed(&cfg.corpus_id, &ctx, parsed.force)
        .await
    {
        Ok(Some(seed)) => seed,
        Ok(None) => {
            // The strategy check above rules this out; if it happens the
            // pipeline and the runner disagree, which is worth saying.
            return Err(SeedError::Failed(
                "seed extraction returned None despite a non-None strategy — the \
                 pipeline's declared strategy and the runner disagree"
                    .into(),
            ));
        }
        Err(e) => return Err(SeedError::Failed(format!("stage 1a failed: {e}"))),
    };

    Ok(SeedReport {
        seed,
        first_section,
        cache_path: paths::cache_dir(&cfg.corpus_id).join("seed.json"),
        forced: parsed.force,
    })
}

/// Print the seed list the way `svrn enrich seed` always has.
pub fn render(report: &SeedReport) {
    println!("  ✓ {} seed entity(ies):", report.seed.entries.len());
    for entry in &report.seed.entries {
        let aliases = if entry.aliases.is_empty() {
            String::new()
        } else {
            format!("  (aka: {})", entry.aliases.join(", "))
        };
        println!(
            "    - {} [{}]{}",
            entry.canonical_name,
            entry.entity_type.as_str_repr(),
            aliases
        );
    }
    println!("  ✓ cache: {}", report.cache_path.display());
}

pub async fn cmd_seed(args: &[String]) -> i32 {
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

    match run(&parsed).await {
        Ok(report) => {
            render(&report);
            0
        }
        Err(e) => {
            eprintln!("error: {}", e.message());
            e.exit_code()
        }
    }
}

/// A parsed `seed` invocation. Public so the `enrich build` orchestrator
/// constructs one directly instead of round-tripping through argv.
#[derive(Debug, Clone)]
pub struct ParsedSeed {
    pub corpus_id: String,
    pub force: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedSeed, String> {
    let mut corpus_id: Option<String> = None;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--force" => {
                force = true;
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
    Ok(ParsedSeed { corpus_id, force })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_bare_corpus_id() {
        let p = parse_args(&["brothers_karamazov".into()]).unwrap();
        assert_eq!(p.corpus_id, "brothers_karamazov");
        assert!(!p.force);
    }

    #[test]
    fn parse_args_accepts_force_flag() {
        let p = parse_args(&["bk".into(), "--force".into()]).unwrap();
        assert!(p.force);
    }

    #[test]
    fn parse_args_rejects_missing_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }
}
