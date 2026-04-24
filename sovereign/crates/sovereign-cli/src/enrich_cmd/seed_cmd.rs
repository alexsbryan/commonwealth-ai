//! `sovereign enrich seed` — Stage 1a driver.
//!
//! Runs the pipeline's seed-extraction strategy against the first
//! section of a corpus and writes the seed entity list to
//! `cache/seed.json`. Subsequent `sovereign enrich extract` runs
//! read this file and thread the canonical-names block into every
//! per-chapter Phase 1 prompt.
//!
//! Idempotent: the runner caches the seed and short-circuits on
//! cache hit unless `--force` is passed.

use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{
    PhaseCache, PhaseRunner, PipelinePhase, PipelineRegistry, RunOutputWriter, SeedStrategy,
};

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich seed",
    summary: "Stage 1a: extract the seed entity list from the first section.",
    sections: &[
        HelpSection::Usage("sovereign enrich seed <corpus-id> [--force]"),
        HelpSection::Flags(&[(
            "--force",
            "Recompute even when a seed list is already cached. Useful when the opening \
             section has been edited or the pipeline's seed prompt has changed.",
        )]),
        HelpSection::Examples(&[
            (
                "sovereign enrich seed brothers_karamazov",
                "Read chapter 1, emit canonical entity list, cache to cache/seed.json.",
            ),
            (
                "sovereign enrich seed bk --force",
                "Re-run even if the seed cache is warm.",
            ),
        ]),
        HelpSection::Notes(
            "Every subsequent `sovereign enrich extract` call reads the cached seed and \
             threads the canonical-names block into every per-chapter Phase 1 prompt. \
             This is what keeps `Fyodor Pavlovich Karamazov` from fragmenting into \
             `Fyodor Karam`, `Fyo Karamzov`, and similar variants across chapters.",
        ),
    ],
};

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

    let cfg = match EnrichConfig::require(&parsed.corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config: {e}");
            return 1;
        }
    };

    let registry = PipelineRegistry::builtin();
    let Some(pipeline) = registry.get(&cfg.pipeline_id) else {
        eprintln!(
            "error: unknown pipeline `{}`; known ids: {:?}",
            cfg.pipeline_id,
            registry.pipeline_ids()
        );
        return 1;
    };

    // Check the strategy before rebuilding corpus state so we can
    // fail fast on pipelines that don't support seed extraction.
    match pipeline.seed_strategy() {
        SeedStrategy::None => {
            eprintln!(
                "error: pipeline `{}` declares SeedStrategy::None — it does not use a \
                 seed entity list. Either switch to an atlas pipeline (e.g. \
                 `literary_atlas`) that does, or skip this step.",
                cfg.pipeline_id
            );
            return 2;
        }
        SeedStrategy::Llm | SeedStrategy::Structural => {}
    }

    // Rebuild chapter inputs — we need the first section's text
    // for LLM-strategy pipelines and the full corpus context for
    // Structural-strategy ones.
    let (inputs, _manifest) = match rebuild_corpus_state(&cfg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: rebuilding corpus state: {e}");
            return 1;
        }
    };
    if inputs.is_empty() {
        eprintln!("error: corpus has no sections");
        return 1;
    }

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

    let ctx = corpus_engine::enrichment::pipeline::CorpusContext {
        chapter_titles: inputs.iter().map(|c| c.title.clone()).collect(),
        chapters: inputs.clone(),
        chunks: Vec::new(),
    };

    println!(
        "  running stage 1a (seed) on first section: {}",
        inputs[0].chapter_id
    );
    if parsed.force {
        println!("  · --force: recomputing even if cache is warm");
    }
    let result = match runner
        .phase_1a_extract_seed(&cfg.corpus_id, &ctx, parsed.force)
        .await
    {
        Ok(Some(seed)) => seed,
        Ok(None) => {
            // Shouldn't happen — we checked strategy above.
            eprintln!("error: seed extraction returned None despite non-None strategy");
            return 1;
        }
        Err(e) => {
            eprintln!("error: stage 1a failed: {e}");
            return 1;
        }
    };

    println!("  ✓ {} seed entity(ies):", result.entries.len());
    for entry in &result.entries {
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
    println!(
        "  ✓ cache: {}/seed.json",
        paths::cache_dir(&cfg.corpus_id).display()
    );
    let _ = Arc::new(()); // silence unused `Arc` import on release builds
    0
}

#[derive(Debug)]
struct ParsedSeed {
    corpus_id: String,
    force: bool,
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
