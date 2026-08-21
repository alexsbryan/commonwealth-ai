// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-resolve` — Phase A Step 3a + Step 3b
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

use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::{resolve_entities_and_events, resolve_step_3b, write_atlas, write_atlas_full, ATLAS_DIRNAME};
use corpus_engine::enrichment::pipeline::{ExtractedQuestion, Phase1Output, PipelinePhase, SectionExtraction};
use corpus_engine::types::EmbedFn;

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_core::tool_manifest::DeclaredTool;
use sovereign_core::types::{StepOutput, ToolContext};
use std::sync::Arc;

const HELP: Help = Help {
    command: "svrn enrich atlas-resolve",
    summary: "Resolve atlas atoms + edges from Phase 1 sketches.",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-resolve <corpus-id> [--phase 3a|3b|all]"),
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
                "svrn enrich atlas-resolve brothers_karamazov",
                "Default (Phase 3a) — resolve entities + events from the cached sketches.",
            ),
            (
                "svrn enrich atlas-resolve bk --phase all",
                "Full structural pass — every atom type + trajectories.json populated.",
            ),
        ]),
        HelpSection::Notes(
            "Requires a prior `svrn enrich extract <corpus> --full` so the Phase 1 \
             cache exists. Produces `~/.svrnmesh/indexes/<corpus>/atlas/atoms.json`, \
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
    if super::pipeline_resolve::resolve_pipeline(&cfg).is_none() {
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
    let cache = cfg.phase_cache();
    let phase1: Phase1Output = match cache.read(PipelinePhase::Questions) {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "error: no Phase 1 cache at {}. Run `svrn enrich extract {} \
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
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, _chat, _chat_with_tokens) = client.into_closures_with_tokens();

    // Resolve into the live atlas dir. The `enrich delta` command
    // calls `resolve_into_dir` directly with a staging tempdir; this
    // wrapper preserves the original "write to the corpus's canonical
    // atlas/" behaviour byte-for-byte.
    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    match resolve_into_dir(&cfg, &sections, &embed, &atlas_dir, parsed.phase).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Resolve the section sketches into `target_atlas_dir` (Step 3a +
/// optional 3b/typed extensions), writing `atoms.json`, `edges.json`,
/// `trajectories.json`, and `resolution_failures.json`.
///
/// Extracted from `cmd_atlas_resolve` so two callers can share the
/// resolve→write body against *different* output directories:
///   - `cmd_atlas_resolve` passes the corpus's live `atlas/` dir.
///   - `enrich delta` (`delta_cmd::cmd_delta`) passes a throwaway
///     staging tempdir, then content-hashes + merges the result into
///     the live atlas via `apply_atom_delta` — never overwriting the
///     live atlas wholesale.
///
/// Behaviour for the live-dir caller is identical to the pre-refactor
/// in-lined body: 3a always runs; 3b/typed extensions run when
/// `phase` is `P3b`/`All`; the writer overwrites atomically; failures
/// are always persisted (even empty) so the aggregator can tell
/// "ran cleanly" from "never ran".
///
/// On the error path the message is a complete sentence (the caller
/// just prefixes `error: ` and returns nonzero) so this fn carries no
/// process-exit policy of its own.
pub(crate) async fn resolve_into_dir(
    cfg: &EnrichConfig,
    sections: &[SectionExtraction],
    embed: &EmbedFn,
    target_atlas_dir: &Path,
    phase: ResolvePhase,
) -> Result<(), String> {
    // Step 3a: always runs. Step 3b is re-resolved from 3a's
    // output so the atom ids remain consistent regardless of
    // whether the caller chose 3a-only or 3b/all.
    let step_3a = resolve_entities_and_events(sections, embed)
        .await
        .map_err(|e| format!("atlas resolution (3a) failed: {e}"))?;

    let atlas_dir = target_atlas_dir;
    let want_3b = matches!(phase, ResolvePhase::P3b | ResolvePhase::All);

    // Collect structured drops across both resolution phases so the
    // aggregator (`svrn enrich errors`) can surface them grouped
    // by kind. Empty in the clean-run case.
    let mut resolution_failures: Vec<corpus_engine::enrichment::pipeline::PhaseFailure> =
        Vec::new();
    resolution_failures.extend(step_3a.failures.iter().cloned());

    let written = if want_3b {
        let step_3b = resolve_step_3b(sections, &step_3a.entities, &step_3a.events)
            .map_err(|e| format!("atlas resolution (3b) failed: {e}"))?;
        resolution_failures.extend(step_3b.failures.iter().cloned());

        // Merge 3a + 3b edges — they use distinct id ranges so no
        // collision, but the order matters for stable diffing
        // across runs: 3a edges first, then 3b.
        let mut edges = step_3a.edges.clone();
        edges.extend(step_3b.edges.iter().cloned());

        // Gap B: project typed extensions into Position + Opposition
        // atoms + qualified Concept Entities + qualified Claims +
        // new edges. Runs after 3b so mechanism merge can fuzzy-match
        // against the existing Concept entities and EvidenceFor /
        // Concedes edges can target already-resolved positions /
        // claims.
        let typed = corpus_engine::enrichment::atlas::resolution::resolve_type_extensions(
            sections,
            &step_3a.entities,
            &[], // no pre-existing positions on first run
            &step_3b.claims,
            step_3a.entities.len() + 1,
            step_3b.claims.len() + 1,
            1,
            1,
            edges.len() + 1,
        );
        resolution_failures.extend(typed.failures.iter().cloned());

        // Apply qualifier updates to existing entities — set
        // `concept_kind` on Concept atoms whose name matched a
        // mechanism sketch.
        let mut entities = step_3a.entities.clone();
        for e in entities.iter_mut() {
            if let Some(kind) = typed.entity_qualifier_updates.get(&e.id) {
                e.concept_kind = Some(kind.clone());
            }
        }
        // Merge new entities (mechanism Concepts the resolver
        // didn't fuzzy-match).
        entities.extend(typed.new_entities.iter().cloned());
        // Merge new claims (evidence + concession).
        let mut claims = step_3b.claims.clone();
        claims.extend(typed.new_claims.iter().cloned());
        // Merge new edges.
        edges.extend(typed.new_edges.iter().cloned());

        let result = write_atlas_full(
            atlas_dir,
            &entities,
            &step_3a.events,
            &step_3b.states,
            &step_3b.relations,
            &claims,
            &step_3b.questions,
            &[], // configurations — Phase 8 territory
            &step_3b.argument_reconstructions,
            &typed.new_positions,
            &typed.new_oppositions,
            &edges,
            &step_3b.trajectories,
        );
        match result {
            Ok(w) => {
                println!("  ✓ {} entity atom(s)", entities.len());
                println!("  ✓ {} event atom(s)", step_3a.events.len());
                println!("  ✓ {} state atom(s)", step_3b.states.len());
                println!("  ✓ {} relation atom(s)", step_3b.relations.len());
                println!("  ✓ {} claim atom(s)", claims.len());
                println!("  ✓ {} question atom(s)", step_3b.questions.len());
                println!(
                    "  ✓ {} argument-reconstruction atom(s)",
                    step_3b.argument_reconstructions.len()
                );
                println!("  ✓ {} position atom(s)", typed.new_positions.len());
                println!("  ✓ {} opposition atom(s)", typed.new_oppositions.len());
                println!("  ✓ {} edge(s) total", edges.len());
                println!("  ✓ {} trajectory chain(s)", step_3b.trajectories.len());
                w
            }
            Err(e) => {
                return Err(format!("writing atlas files: {e}"));
            }
        }
    } else {
        // 3a-only path — mirrors the original behaviour.
        match write_atlas(
            atlas_dir,
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
                return Err(format!("writing atlas files: {e}"));
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

    // Always persist resolution failures, even empty — that way the
    // aggregator knows "ran cleanly" vs. "hasn't run yet" by whether
    // the file exists. The schema-versioned file is atomic-safe so
    // a mid-run interrupt leaves the prior state intact.
    match corpus_engine::enrichment::atlas::write_atlas_failures(atlas_dir, &resolution_failures) {
        Ok(path) => {
            if resolution_failures.is_empty() {
                println!("  ✓ {} (no resolution drops)", path.display());
            } else {
                println!(
                    "  ! {} drop(s) — see {} (run `svrn enrich errors {}` for remediation)",
                    resolution_failures.len(),
                    path.display(),
                    cfg.corpus_id
                );
            }
        }
        Err(e) => {
            eprintln!("warning: writing resolution_failures.json: {e}");
        }
    }

    Ok(())
}

/// Collect the per-section atlas sketches from a cached
/// `Phase1Output`'s chapters. Chapters that don't carry a
/// `section_extraction` (legacy / pre-atlas runs) are skipped.
/// Shared with `delta_cmd` (the `enrich delta` subcommand reuses the
/// same filter against its subset cache).
pub(crate) fn collect_section_extractions(
    chapters: &[ExtractedQuestion],
) -> Vec<SectionExtraction> {
    chapters
        .iter()
        .filter_map(|c| c.section_extraction.clone())
        .collect()
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvePhase {
    P3a,
    P3b,
    All,
}

/// `atlas_resolve` — literary-atlas **Phase 3a/3b** as a workflow leaf: resolve
/// the Phase-1 section sketches into canonical atoms + edges + trajectories.
///
/// One atomic op wrapping the *exact* bespoke `resolve_into_dir` (entity merge by
/// description cosine, event dedupe, typed-atom resolution, type extensions) — so
/// a workflow-built atlas is byte-faithful to `svrn enrich atlas-resolve`.
/// It reuses the same machinery (`EnrichConfig`, `DaemonInferenceClient`,
/// `resolve_into_dir`), which is why this leaf lives here in `enrich_cmd` rather
/// than in `sovereign-tools`: the resolver needs a daemon embed closure (the
/// description-cosine rule), and that closure + config live in this crate.
///
/// Effect `Write` (writes the canonical `atlas/` files); needs the daemon up.
pub(crate) struct AtlasResolveTool;

impl AtlasResolveTool {
    /// Bind this tool's state to its `atlas_resolve` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_resolve", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_resolve`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> sovereign_core::error::Result<StepOutput> {
        use sovereign_core::error::Error;

        let corpus = params
            .get("corpus")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("atlas_resolve: missing required `corpus`".into()))?;
        // Parse the phase up front (pure) so a bad value fails before any IO.
        let phase = match params.get("phase").and_then(|v| v.as_str()) {
            Some("3a") => ResolvePhase::P3a,
            Some("3b") => ResolvePhase::P3b,
            Some("all") | None => ResolvePhase::All,
            Some(other) => {
                return Err(Error::Execution(format!(
                    "atlas_resolve: unknown phase `{other}` (expected 3a|3b|all)"
                )))
            }
        };

        let cfg = EnrichConfig::require(corpus).map_err(|e| {
            Error::Execution(format!(
                "atlas_resolve: enrichment config for `{corpus}`: {e} — run `enrich init` first"
            ))
        })?;
        if !cfg.pipeline_id.ends_with("_atlas") {
            return Err(Error::Execution(format!(
                "atlas_resolve: pipeline `{}` does not produce atlas sketches (need an *_atlas pipeline)",
                cfg.pipeline_id
            )));
        }

        // Load the Phase-1 cache (questions.json) and pull its section sketches.
        let cache = cfg.phase_cache();
        let phase1: Phase1Output = cache
            .read(PipelinePhase::Questions)
            .map_err(|e| Error::Execution(format!("atlas_resolve: read Phase 1 cache: {e}")))?
            .ok_or_else(|| {
                Error::Execution(format!(
                    "atlas_resolve: no Phase 1 cache for `{corpus}` — run the extract phase first"
                ))
            })?;
        let sections = collect_section_extractions(&phase1.questions_by_chapter);
        if sections.is_empty() {
            return Err(Error::Execution(
                "atlas_resolve: Phase 1 cache carries no `section_extraction` sketches \
                 (re-run extract with an *_atlas pipeline)"
                    .into(),
            ));
        }

        // The description-cosine merge rule needs embeddings — build the same
        // daemon embed closure the bespoke resolver uses.
        let client = DaemonInferenceClient::from_enrich_config(&cfg)
            .map_err(|e| Error::Execution(format!("atlas_resolve: build daemon client: {e}")))?;
        let (embed, _chat, _chat_with_tokens) = client.into_closures_with_tokens();

        let atlas_dir = atlas_dir_for(&cfg.corpus_id);
        resolve_into_dir(&cfg, &sections, &embed, &atlas_dir, phase)
            .await
            .map_err(|e| Error::Execution(format!("atlas_resolve: {e}")))?;

        Ok(StepOutput::Text(format!(
            "atlas_resolve: resolved {} section(s) into {}/atoms.json + edges.json + trajectories.json",
            sections.len(),
            atlas_dir.display()
        )))
    }
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
    use sovereign_core::traits::Tool;

    #[test]
    fn parse_args_defaults_to_phase_3a() {
        let p = parse_args(&["brothers_karamazov".into()]).unwrap();
        assert_eq!(p.corpus_id, "brothers_karamazov");
        assert_eq!(p.phase, ResolvePhase::P3a);
    }

    #[test]
    fn parse_args_accepts_explicit_phase_3a() {
        let p = parse_args(&["bk".into(), "--phase".into(), "3a".into()]).unwrap();
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

    /// The `atlas_resolve` workflow leaf validates its params before any IO: a
    /// missing `corpus`, a bogus `phase`, and an unknown corpus all fail loudly.
    /// (The happy path needs the daemon + a resolved Phase-1 cache — exercised by
    /// the integration run, not a unit test.)
    #[tokio::test]
    async fn atlas_resolve_leaf_validates_params() {
        let ctx = ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        };
        assert!(AtlasResolveTool
            .declared()
            .execute(&serde_json::json!({}), &ctx)
            .await
            .is_err());
        assert!(AtlasResolveTool
            .declared()
            .execute(
                &serde_json::json!({ "corpus": "x", "phase": "bogus" }),
                &ctx
            )
            .await
            .is_err());
        assert!(AtlasResolveTool
            .declared()
            .execute(
                &serde_json::json!({ "corpus": "definitely-not-a-real-corpus-zzz" }),
                &ctx
            )
            .await
            .is_err());
    }
}
