//! `sovereign enrich atlas-resolve` — Phase A Step 3a + Step 3b
//! driver.
//!
//! Reads the cached `Phase1Output` (section-level sketches),
//! resolves entity + event sketches into canonical atoms with
//! `Involves` edges (Step 3a), and — when `--phase 3b` or
//! `--phase all` is in effect — extends resolution with
//! state/relation/claim/question atoms + Transition/Grounds edges +
//! a populated trajectories index (Step 3b).
//!
//! Idempotent: the writer overwrites atomically. Re-running on the
//! same cache reproduces the same atoms (modulo embedding
//! non-determinism, which the daemon's embed slot does not
//! introduce). `--phase all` runs 3a and 3b in one shot and writes
//! the union; `--phase 3b` assumes a prior `--phase 3a` but
//! recomputes 3a internally so the atom ids remain consistent
//! across the two passes.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    resolve_entities_and_events, resolve_step_3b, write_atlas, write_atlas_full, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::pipeline::{
    ExtractedQuestion, Phase1Output, PhaseCache, PipelinePhase, PipelineRegistry, SectionExtraction,
};

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-resolve",
    summary: "Resolve atlas atoms + edges from Phase 1 sketches.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich atlas-resolve <corpus-id> [--phase 3a|3b|all]",
        ),
        HelpSection::Flags(&[
            (
                "--phase 3a",
                "Entity + event atoms + Involves edges only. Fast; no LLM calls. \
                 Default when --phase is omitted.",
            ),
            (
                "--phase 3b",
                "Adds state / relation / claim / question atoms + Transition + Grounds \
                 edges + populates trajectories.json. Implies 3a (entities + events \
                 are re-resolved so atom ids stay consistent).",
            ),
            (
                "--phase all",
                "Synonym for --phase 3b — runs the full structural pass. Phase 5 \
                 LLM-enriched grounding is a separate subcommand that will land in a \
                 later step.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich atlas-resolve brothers_karamazov",
                "Default (Phase 3a) — resolve entities + events from the cached sketches.",
            ),
            (
                "sovereign enrich atlas-resolve bk --phase all",
                "Full structural pass — every atom type + trajectories.json populated.",
            ),
        ]),
        HelpSection::Notes(
            "Requires a prior `sovereign enrich extract <corpus> --full` so the Phase 1 \
             cache exists. Produces `~/.sovereign/indexes/<corpus>/atlas/atoms.json`, \
             `edges.json`, and `trajectories.json`.",
        ),
    ],
};

pub async fn cmd_atlas_resolve(args: &[String]) -> i32 {
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

    // Validate the pipeline is atlas-shaped. A `literary` (v1) cache
    // doesn't carry section_extraction payloads, so Phase 3a
    // resolution would produce an empty atlas — tell the operator
    // rather than silently writing empty files.
    let registry = PipelineRegistry::builtin();
    if registry.get(&cfg.pipeline_id).is_none() {
        eprintln!(
            "error: unknown pipeline `{}` in enrichment config",
            cfg.pipeline_id
        );
        return 1;
    }
    if !cfg.pipeline_id.ends_with("_atlas") {
        eprintln!(
            "error: pipeline `{}` does not produce atlas sketches. Re-init with \
             --pipeline literary_atlas (or another *_atlas pipeline) and re-run \
             extract before resolving.",
            cfg.pipeline_id
        );
        return 1;
    }

    // Load the Phase 1 cache. No cache = nothing to resolve.
    let cache = PhaseCache::new(paths::cache_dir(&cfg.corpus_id));
    let phase1: Phase1Output = match cache.read(PipelinePhase::Questions) {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "error: no Phase 1 cache at {}. Run `sovereign enrich extract {} \
                 --full` first.",
                paths::cache_dir(&cfg.corpus_id).display(),
                cfg.corpus_id
            );
            return 1;
        }
        Err(e) => {
            eprintln!("error: reading Phase 1 cache: {e}");
            return 1;
        }
    };

    // Extract section_extraction from each chapter. Skip chapters
    // that don't carry one (the legacy pipeline, or a pre-atlas
    // run) with a warning so the operator knows coverage is partial.
    let sections = collect_section_extractions(&phase1.questions_by_chapter);
    if sections.is_empty() {
        eprintln!(
            "error: Phase 1 cache contains no `section_extraction` payloads. \
             Either re-run extract with the `literary_atlas` pipeline or \
             resolve a different corpus."
        );
        return 1;
    }
    println!(
        "  loaded {} section(s) with atlas sketches (of {} total chapter(s) in cache)",
        sections.len(),
        phase1.questions_by_chapter.len()
    );

    // Build the embed closure. Resolution needs embeddings for the
    // description-cosine rule; the daemon client is the same one
    // used by `extract`.
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
    let (embed, _chat, _chat_with_tokens) = client.into_closures_with_tokens();

    // Step 3a: always runs. Step 3b is re-resolved from 3a's
    // output so the atom ids remain consistent regardless of
    // whether the caller chose 3a-only or 3b/all.
    let step_3a = match resolve_entities_and_events(&sections, &embed).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: atlas resolution (3a) failed: {e}");
            return 1;
        }
    };

    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    let want_3b = matches!(parsed.phase, ResolvePhase::P3b | ResolvePhase::All);

    let written = if want_3b {
        let step_3b = match resolve_step_3b(&sections, &step_3a.entities, &step_3a.events) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: atlas resolution (3b) failed: {e}");
                return 1;
            }
        };

        // Merge 3a + 3b edges — they use distinct id ranges so no
        // collision, but the order matters for stable diffing
        // across runs: 3a edges first, then 3b.
        let mut edges = step_3a.edges.clone();
        edges.extend(step_3b.edges.iter().cloned());

        let result = write_atlas_full(
            &atlas_dir,
            &step_3a.entities,
            &step_3a.events,
            &step_3b.states,
            &step_3b.relations,
            &step_3b.claims,
            &step_3b.questions,
            &[], // configurations — Phase 8 territory
            &edges,
            &step_3b.trajectories,
        );
        match result {
            Ok(w) => {
                println!("  ✓ {} entity atom(s)", step_3a.entities.len());
                println!("  ✓ {} event atom(s)", step_3a.events.len());
                println!("  ✓ {} state atom(s)", step_3b.states.len());
                println!("  ✓ {} relation atom(s)", step_3b.relations.len());
                println!("  ✓ {} claim atom(s)", step_3b.claims.len());
                println!("  ✓ {} question atom(s)", step_3b.questions.len());
                println!("  ✓ {} edge(s) total", edges.len());
                println!(
                    "  ✓ {} trajectory chain(s)",
                    step_3b.trajectories.len()
                );
                w
            }
            Err(e) => {
                eprintln!("error: writing atlas files: {e}");
                return 1;
            }
        }
    } else {
        // 3a-only path — mirrors the original behaviour.
        match write_atlas(
            &atlas_dir,
            &step_3a.entities,
            &step_3a.events,
            &step_3a.edges,
        ) {
            Ok(w) => {
                println!("  ✓ {} entity atom(s)", step_3a.entities.len());
                println!("  ✓ {} event atom(s)", step_3a.events.len());
                println!("  ✓ {} involves edge(s)", step_3a.edges.len());
                w
            }
            Err(e) => {
                eprintln!("error: writing atlas files: {e}");
                return 1;
            }
        }
    };

    println!("  ✓ wrote {}", written.atoms_path.display());
    println!("  ✓ wrote {}", written.edges_path.display());
    if want_3b {
        println!("  ✓ wrote {}", written.trajectories_path.display());
    } else {
        println!(
            "  ✓ wrote {} (empty — --phase 3b or all populates it)",
            written.trajectories_path.display()
        );
    }

    0
}

fn collect_section_extractions(chapters: &[ExtractedQuestion]) -> Vec<SectionExtraction> {
    chapters
        .iter()
        .filter_map(|c| c.section_extraction.clone())
        .collect()
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvePhase {
    P3a,
    P3b,
    All,
}

#[derive(Debug)]
struct ParsedResolve {
    corpus_id: String,
    phase: ResolvePhase,
}

fn parse_args(args: &[String]) -> Result<ParsedResolve, String> {
    let mut corpus_id: Option<String> = None;
    let mut phase = ResolvePhase::P3a;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--phase" => {
                let val = args
                    .get(i + 1)
                    .ok_or("--phase requires a value (3a|3b|all)".to_string())?;
                phase = match val.as_str() {
                    "3a" => ResolvePhase::P3a,
                    "3b" => ResolvePhase::P3b,
                    "all" => ResolvePhase::All,
                    other => {
                        return Err(format!(
                            "unknown phase `{other}`; expected one of 3a, 3b, all"
                        ));
                    }
                };
                i += 2;
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
    Ok(ParsedResolve { corpus_id, phase })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_defaults_to_phase_3a() {
        let p = parse_args(&["brothers_karamazov".into()]).unwrap();
        assert_eq!(p.corpus_id, "brothers_karamazov");
        assert_eq!(p.phase, ResolvePhase::P3a);
    }

    #[test]
    fn parse_args_accepts_explicit_phase_3a() {
        let p = parse_args(&[
            "bk".into(),
            "--phase".into(),
            "3a".into(),
        ])
        .unwrap();
        assert_eq!(p.phase, ResolvePhase::P3a);
    }

    #[test]
    fn parse_args_accepts_phase_3b_and_all_at_parse_time() {
        // The command refuses to run these today (they're not
        // implemented), but the parser accepts them so the error
        // path is reachable — we want the operator to see a clear
        // "not yet implemented" message, not a parse failure.
        let p = parse_args(&["bk".into(), "--phase".into(), "3b".into()]).unwrap();
        assert_eq!(p.phase, ResolvePhase::P3b);
        let p = parse_args(&["bk".into(), "--phase".into(), "all".into()]).unwrap();
        assert_eq!(p.phase, ResolvePhase::All);
    }

    #[test]
    fn parse_args_rejects_unknown_phase() {
        let err = parse_args(&["bk".into(), "--phase".into(), "42".into()]).unwrap_err();
        assert!(err.contains("unknown phase"), "got: {err}");
    }

    #[test]
    fn parse_args_requires_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"), "got: {err}");
    }
}
