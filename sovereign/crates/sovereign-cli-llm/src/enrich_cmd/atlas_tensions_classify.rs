// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign enrich atlas-tensions-classify` — Phase 6 LLM Tension
//! classifier (Landing 4 of the v2 atlas pipeline).
//!
//! Reads `atlas/tension_candidates.json` (produced by the
//! deterministic `atlas-tensions` pass), resolves each candidate to
//! its source/target atom contents + shared-entity name, dispatches
//! a per-candidate prompt to the configured chat model, and merges
//! the accepted candidates into `atlas/edges.json` as
//! `EdgeType::Tension` records.
//!
//! Idempotent: re-running on the same candidate list reproduces the
//! same edges modulo model-temperature variance. New runs replace
//! prior LLM-classified Tension edges (stamped with
//! `EdgeProvenance::LlmPairwise`) but preserve every other edge in
//! `edges.json` untouched.
//!
//! Why not fold into `atlas-tensions`: the deterministic enumerator
//! is fast and operator-runnable without a model loaded; keeping the
//! LLM half as a separate verb means a dev can inspect candidates
//! before paying inference cost. The build flow chains the two so a
//! one-shot `enrich build` runs both halves transparently.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    analysis::{
        classification_to_edge, resolve_candidate_content, AtomIndex, CandidateContent,
        HolisticTension, Phase6Classification, TensionCandidate,
    },
    atoms::{AtomEnvelope, AtomId, AtomsFile, Entity},
    edges::{Edge, EdgeId, EdgeProvenance, EdgeType},
    read_atlas_atoms, read_atlas_edges, read_tension_candidates, write_atlas_edges, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::pipeline::{atlas::EntityType, PipelineRegistry};

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-tensions-classify",
    summary: "LLM-classify tension candidates and merge accepted ones into edges.json.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich atlas-tensions-classify <corpus-id> [--max-candidates <n>] [--dry-run]",
        ),
        HelpSection::Flags(&[
            (
                "--max-candidates <n>",
                "Cap the number of candidates classified this run. Useful for \
                 prompt-tuning iterations on a fixed slice. Default: classify every \
                 candidate in tension_candidates.json.",
            ),
            (
                "--dry-run",
                "Compose every prompt + print to stdout, but do not call the model. \
                 Useful for inspecting prompt content before a full run.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich atlas-tensions-classify brothers_karamazov",
                "Classify every candidate in bk's tension_candidates.json and merge \
                 the accepted Tension edges into edges.json.",
            ),
            (
                "sovereign enrich atlas-tensions-classify dubliners-test --max-candidates 5",
                "Quick prompt-tuning iteration: only classify the first 5 candidates.",
            ),
        ]),
        HelpSection::Notes(
            "Requires `sovereign enrich atlas-tensions <corpus>` to have run first \
             (so tension_candidates.json exists) and a daemon at localhost:9741. \
             Replaces prior LlmPairwise Tension edges in edges.json; preserves every \
             other edge type and every other-provenance edge untouched.",
        ),
    ],
};

pub async fn cmd_atlas_tensions_classify(args: &[String]) -> i32 {
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
    let pipeline = match registry.get(&cfg.pipeline_id) {
        Some(p) => p,
        None => {
            eprintln!(
                "error: pipeline '{}' not registered (corpus references unknown pipeline)",
                cfg.pipeline_id
            );
            return 1;
        }
    };

    let atlas_dir = atlas_dir_for(&cfg.corpus_id);

    // Branch on which Phase 6 mode the pipeline opts into.
    // - `runs_phase6_holistic()` → corpus-level naturalistic pass
    //   (philosophy). Reads atoms.json, single chat call, materializes
    //   between-position Tension edges with entity-id endpoints.
    // - `runs_phase6_atlas_classifier()` → per-pair classification
    //   (literary). Reads tension_candidates.json + atoms.json,
    //   classifies each candidate.
    // - Neither → no-op.
    if pipeline.runs_phase6_holistic() {
        return run_holistic_classifier(&cfg, pipeline.as_ref(), &atlas_dir, parsed.dry_run).await;
    }

    if !pipeline.runs_phase6_atlas_classifier() {
        println!(
            "  · pipeline '{}' is not opted into the Phase 6 atlas Tension classifier — skipping.",
            cfg.pipeline_id
        );
        return 0;
    }

    let candidates = match read_tension_candidates(&atlas_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "error: reading {}/tension_candidates.json: {e}. Run \
                 `sovereign enrich atlas-tensions {}` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };

    if candidates.candidates.is_empty() {
        println!("  · 0 candidates in tension_candidates.json — nothing to classify.");
        return 0;
    }

    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading {}/atoms.json: {e}", atlas_dir.display());
            return 1;
        }
    };

    // Resolve every candidate up-front. Candidates that point at
    // atoms missing from atoms.json (stale candidate file) are
    // silently dropped with a logged note — re-running the
    // deterministic pass would clean them up.
    let index = AtomIndex::build(&atoms);
    let mut resolved: Vec<(TensionCandidate, CandidateContent)> = Vec::new();
    let mut stale_drops = 0usize;
    for cand in &candidates.candidates {
        match resolve_candidate_content(cand, &index) {
            Some(content) => resolved.push((cand.clone(), content)),
            None => stale_drops += 1,
        }
    }
    if stale_drops > 0 {
        println!(
            "  · {stale_drops} candidate(s) reference atoms not in atoms.json (stale tension_candidates.json) — dropping",
        );
    }

    if let Some(cap) = parsed.max_candidates {
        if resolved.len() > cap {
            println!("  · capping to first {cap} candidate(s) per --max-candidates");
            resolved.truncate(cap);
        }
    }

    println!(
        "  loaded {} candidate(s) (from {} total in tension_candidates.json)",
        resolved.len(),
        candidates.candidates.len()
    );

    if parsed.dry_run {
        println!("  · --dry-run: composing prompts and printing without calling the model");
        for (i, (cand, content)) in resolved.iter().enumerate() {
            let Some(prompt) = pipeline.compose_phase6_atlas_classifier(content) else {
                eprintln!(
                    "  ⚠ candidate {} ({}): pipeline returned None for compose — opt-in regression",
                    i + 1,
                    cand.id
                );
                continue;
            };
            println!("\n──── candidate {} ({}) ────", i + 1, cand.id);
            println!("system: {} bytes", prompt.system.len());
            println!("user:\n{}", prompt.user);
        }
        return 0;
    }

    // Build the chat closure once; reuse across candidates.
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (_embed, chat) = client.into_closures();

    // Walk the existing edges file. We preserve every non-Tension
    // edge and every Tension edge whose provenance is *not*
    // LlmPairwise (i.e., a hand-authored or future-non-LLM Tension
    // edge stays). LlmPairwise Tension edges from a prior classifier
    // run are dropped and replaced with this run's output.
    let prior_edges = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "error: reading {}/edges.json: {e}. Run `sovereign enrich atlas-resolve {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };
    let mut next_edges: Vec<Edge> = prior_edges
        .edges
        .iter()
        .filter(|e| {
            !(e.edge_type == EdgeType::Tension && e.provenance == EdgeProvenance::LlmPairwise)
        })
        .cloned()
        .collect();

    let next_edge_ordinal = next_edge_ordinal(&prior_edges.edges);

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    let mut chat_failures = 0usize;
    let mut parse_failures = 0usize;

    for (i, (cand, content)) in resolved.iter().enumerate() {
        let Some(prompt) = pipeline.compose_phase6_atlas_classifier(content) else {
            eprintln!("  ⚠ pipeline returned None for compose — opt-in regression");
            chat_failures += 1;
            continue;
        };
        print!(
            "  [{i}/{n}] {id} {src}↔{tgt}",
            i = i + 1,
            n = resolved.len(),
            id = cand.id,
            src = cand.source_atom.as_str(),
            tgt = cand.target_atom.as_str(),
        );
        let response = match chat(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                println!(" — chat error: {e}");
                chat_failures += 1;
                continue;
            }
        };
        let cls: Phase6Classification = match pipeline.parse_phase6_atlas_classifier(&response) {
            Ok(c) => c,
            Err(e) => {
                println!(" — parse failed: {e}");
                parse_failures += 1;
                continue;
            }
        };
        if cls.is_tension {
            let edge_id = EdgeId::new(next_edge_ordinal + accepted + 1);
            if let Some(edge) = classification_to_edge(cand, &cls, content, edge_id) {
                println!(
                    " ✓ tension (conf {:.2}): {}",
                    edge.confidence,
                    edge.sub_question.as_deref().unwrap_or("(no sub-question)")
                );
                next_edges.push(edge);
                accepted += 1;
            } else {
                // Should never trigger since is_tension was checked,
                // but keep the path defensive.
                println!(" — classification said tension but edge build returned None");
                parse_failures += 1;
            }
        } else {
            println!(" · not a tension: {}", cls.rationale.trim());
            rejected += 1;
        }
    }

    println!();
    println!(
        "  Phase 6 classifier summary: {accepted} tension(s), {rejected} rejected, \
         {chat_failures} chat failure(s), {parse_failures} parse failure(s)"
    );

    let next = corpus_engine::enrichment::atlas::edges::EdgesFile::new(next_edges);
    match write_atlas_edges(&atlas_dir, &next) {
        Ok(path) => {
            println!("  ✓ wrote {}", path.display());
            // Failures aren't a hard error — they degrade recall but
            // the pipeline still emits the accepted edges. The
            // top-level build will see exit 0 unless a write actually
            // failed.
            0
        }
        Err(e) => {
            eprintln!("error: writing edges.json: {e}");
            1
        }
    }
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

/// Run the Phase 6 holistic classifier path.
///
/// Reads atoms.json, asks the pipeline for its holistic prompt (which
/// includes the corpus inventory in the user message), makes one
/// chat call, parses the fault-line list, resolves each side's name
/// to an entity id, and merges the results into edges.json as
/// `Tension` edges with entity-id endpoints. The eval's
/// `resolve_endpoint` reads entity ids directly, so the matcher
/// scores these without atom-chasing.
///
/// Replaces prior `LlmPairwise` Tension edges (so re-running
/// idempotently overwrites the previous holistic run's output);
/// preserves every non-Tension edge and every other-provenance edge.
async fn run_holistic_classifier(
    cfg: &EnrichConfig,
    pipeline: &dyn corpus_engine::enrichment::pipeline::Pipeline,
    atlas_dir: &std::path::Path,
    dry_run: bool,
) -> i32 {
    let atoms = match read_atlas_atoms(atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading {}/atoms.json: {e}", atlas_dir.display());
            return 1;
        }
    };

    let prompt = match pipeline.compose_phase6_holistic(&atoms) {
        Some(p) => p,
        None => {
            eprintln!(
                "  ⚠ pipeline '{}' opts into holistic but compose returned None",
                cfg.pipeline_id
            );
            return 1;
        }
    };

    println!(
        "  Phase 6 holistic: {} chars in (system: {} bytes, user: {} bytes)",
        prompt.system.len() + prompt.user.len(),
        prompt.system.len(),
        prompt.user.len()
    );

    if dry_run {
        println!("  · --dry-run: composed prompt below; not calling the model");
        println!("──── system ────\n{}", prompt.system);
        println!("──── user ────\n{}", prompt.user);
        return 0;
    }

    let client = match DaemonInferenceClient::from_enrich_config(cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (_embed, chat) = client.into_closures();

    // Run the holistic call N times and union the results, keyed by
    // unordered (resolved-entity-id, resolved-entity-id) pairs.
    //
    // Why: the model varies between runs in which lexicon names it
    // selects for the same fault line — e.g. across runs of the same
    // free-will corpus it alternates between picking
    // "incompatibilism (hard)" (entity-0010) and "incompatibilists"
    // (entity-0015) for the same position. A single run gives a
    // brittle snapshot; the union of three runs is much closer to
    // the model's full *knowledge* of the corpus's structure. The
    // crux text is taken from the first run that surfaced the pair
    // (we don't try to merge cruxes — they're paraphrases of the
    // same question and any single one is informative).
    //
    // Per-run cost is one chat call. Three runs ≈ 30s on Strix Halo
    // for the bench corpora. Tolerable for the precision gain.
    const HOLISTIC_RUNS: usize = 3;
    let mut accumulated: Vec<(HolisticTension, AtomId, AtomId)> = Vec::new();
    let mut seen_pairs: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut run_counts: Vec<usize> = Vec::with_capacity(HOLISTIC_RUNS);
    let mut chat_failures = 0usize;
    let mut parse_failures = 0usize;

    for run_ix in 0..HOLISTIC_RUNS {
        let response = match chat(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  ⚠ run {} chat failed: {e}", run_ix + 1);
                chat_failures += 1;
                run_counts.push(0);
                continue;
            }
        };
        let tensions: Vec<HolisticTension> = match pipeline.parse_phase6_holistic(&response) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("  ⚠ run {} parse failed: {e}", run_ix + 1);
                parse_failures += 1;
                run_counts.push(0);
                continue;
            }
        };
        let mut new_in_run = 0usize;
        for t in tensions {
            let Some(a_id) = resolve_position_to_entity(&t.position_a, &atoms) else {
                continue;
            };
            let Some(b_id) = resolve_position_to_entity(&t.position_b, &atoms) else {
                continue;
            };
            if a_id == b_id {
                continue;
            }
            // Unordered pair key (so reverse-direction emits dedupe).
            let mut key = [a_id.as_str().to_string(), b_id.as_str().to_string()];
            key.sort();
            let pair_key = (key[0].clone(), key[1].clone());
            if seen_pairs.insert(pair_key) {
                accumulated.push((t, a_id, b_id));
                new_in_run += 1;
            }
        }
        run_counts.push(new_in_run);
    }

    println!(
        "  Phase 6 holistic: {} run(s), {} unique fault line(s) (per-run new: {:?})",
        HOLISTIC_RUNS - chat_failures - parse_failures,
        accumulated.len(),
        run_counts,
    );

    // Read existing edges, drop prior LlmPairwise Tension edges
    // (whether from per-pair or a previous holistic run). Preserve
    // every other edge.
    let prior_edges = match read_atlas_edges(atlas_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "error: reading {}/edges.json: {e}. Run `sovereign enrich atlas-resolve {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };
    let mut next_edges: Vec<Edge> = prior_edges
        .edges
        .iter()
        .filter(|e| {
            !(e.edge_type == EdgeType::Tension && e.provenance == EdgeProvenance::LlmPairwise)
        })
        .cloned()
        .collect();
    let next_ord_base = next_edge_ordinal(&prior_edges.edges);

    let mut materialized = 0usize;
    for (t, a_id, b_id) in &accumulated {
        let edge = Edge {
            id: EdgeId::new(next_ord_base + materialized + 1),
            edge_type: EdgeType::Tension,
            source: a_id.clone(),
            target: b_id.clone(),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: Some(t.crux.clone()),
            confidence: 0.85,
            provenance: EdgeProvenance::LlmPairwise,
        };
        println!("  ✓ {} ↔ {}", t.position_a, t.position_b);
        println!("      crux: {}", t.crux);
        next_edges.push(edge);
        materialized += 1;
    }

    if chat_failures + parse_failures > 0 {
        println!(
            "  ⚠ {chat_failures} chat failure(s), {parse_failures} parse failure(s) across {} run(s)",
            HOLISTIC_RUNS
        );
    }
    println!("  Phase 6 holistic summary: {materialized} edge(s) written");

    let next = corpus_engine::enrichment::atlas::edges::EdgesFile::new(next_edges);
    match write_atlas_edges(atlas_dir, &next) {
        Ok(path) => {
            println!("  ✓ wrote {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("error: writing edges.json: {e}");
            1
        }
    }
}

/// Resolve a position name (the surface form the model emitted) to
/// an entity id in the atlas.
///
/// Strategy: try exact match → case-insensitive equality →
/// case-insensitive substring match on canonical names, prefer
/// concept-typed entities over person-typed when a tie. The model is
/// instructed to use names verbatim from the lexicon, so most calls
/// hit the equality path; the substring branch is a safety net for
/// minor surface drift (trailing whitespace, parenthetical
/// disambiguators, etc.).
///
/// Substring matches must be *whole-token-bounded* on at least one
/// side — otherwise short canonical names like `"compatibilism"`
/// would match composite haystacks like `"incompatibilisms (hard)"`
/// (the substring `"compatibilism"` lives inside `"incompatibilisms"`)
/// and silently collapse two distinct positions onto one entity. The
/// safer rule: accept a substring hit only when the canonical name
/// either equals a token in the haystack, or the haystack equals a
/// token in the canonical (so a "trailing ist/ism" doesn't sneak
/// through).
fn resolve_position_to_entity(name: &str, atoms: &AtomsFile) -> Option<AtomId> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Collect entities once.
    let mut concepts: Vec<&Entity> = Vec::new();
    let mut persons: Vec<&Entity> = Vec::new();
    for env in &atoms.atoms {
        if let AtomEnvelope::Entity(e) = env {
            match e.entity_type {
                EntityType::Concept => concepts.push(e),
                EntityType::Person => persons.push(e),
                _ => {}
            }
        }
    }
    let pool = concepts.iter().chain(persons.iter());

    // Pass 1: exact canonical-name equality.
    for e in pool.clone() {
        if e.canonical_name == trimmed {
            return Some(e.id.clone());
        }
    }
    // Pass 2: case-insensitive equality.
    let lower = trimmed.to_ascii_lowercase();
    for e in pool.clone() {
        if e.canonical_name.to_ascii_lowercase() == lower {
            return Some(e.id.clone());
        }
    }
    // Pass 3: token-set match with per-token prefix tolerance.
    //
    // Each needle token must pair with a canonical token via either:
    //   - exact equality, OR
    //   - 7+ char common prefix (handles model variance:
    //     "Libertarian (metaphysics)" → "Libertarianism (metaphysics)";
    //     "incompatibilisms (hard)" → "incompatibilism (hard)").
    //
    // We run pairing both directions (canonical ⊆ needle and
    // needle ⊆ canonical) so a model that adds disambiguators
    // ("Libertarianism (metaphysics)" → naturalistic
    // "Libertarianism") still resolves. Concepts win ties over
    // persons (philosophy fault lines are between doctrines).
    fn tokens(s: &str) -> Vec<String> {
        s.to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    }
    fn token_pair_ok(a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        const MIN_PREFIX: usize = 7;
        let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
        common >= MIN_PREFIX
    }
    fn smaller_subsumes_larger(small: &[String], large: &[String]) -> bool {
        small
            .iter()
            .all(|s| large.iter().any(|l| token_pair_ok(s, l)))
    }
    let n_tokens = tokens(trimmed);
    if n_tokens.is_empty() {
        return None;
    }
    let mut best: Option<&Entity> = None;
    let mut best_score: usize = 0;
    for e in pool {
        let h_tokens = tokens(&e.canonical_name);
        if h_tokens.is_empty() {
            continue;
        }
        let canonical_in_needle = smaller_subsumes_larger(&h_tokens, &n_tokens);
        let needle_in_canonical = smaller_subsumes_larger(&n_tokens, &h_tokens);
        if !canonical_in_needle && !needle_in_canonical {
            continue;
        }
        // Score by canonical token count (more specific match wins),
        // with concept-type preferred over person on tie.
        let score = h_tokens.len() * 2
            + match e.entity_type {
                EntityType::Concept => 1,
                _ => 0,
            };
        if score > best_score {
            best_score = score;
            best = Some(e);
        }
    }
    best.map(|e| e.id.clone())
}

/// Highest existing edge ordinal across `edges`. New edges issue
/// ordinals starting from `next_edge_ordinal(&prior) + 1` so we
/// don't clobber existing ids.
fn next_edge_ordinal(edges: &[Edge]) -> usize {
    edges
        .iter()
        .filter_map(|e| {
            // Edge id format is "edge-NNNNN" (5-digit zero-padded).
            // Parse the suffix; non-conforming ids contribute 0 so
            // we still issue a fresh max+1 ordinal.
            e.id.as_str()
                .strip_prefix("edge-")
                .and_then(|s| s.parse::<usize>().ok())
        })
        .max()
        .unwrap_or(0)
}

#[derive(Debug)]
struct ParsedClassify {
    corpus_id: String,
    max_candidates: Option<usize>,
    dry_run: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedClassify, String> {
    let mut corpus_id: Option<String> = None;
    let mut max_candidates: Option<usize> = None;
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--max-candidates" => {
                let n_str = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-candidates requires a value".to_string())?;
                let n: usize = n_str.parse().map_err(|e| {
                    format!("--max-candidates value '{n_str}' is not an integer: {e}")
                })?;
                max_candidates = Some(n);
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
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                i += 1;
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    Ok(ParsedClassify {
        corpus_id,
        max_candidates,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_bare_corpus_id() {
        let p = parse_args(&["bk".into()]).unwrap();
        assert_eq!(p.corpus_id, "bk");
        assert!(p.max_candidates.is_none());
        assert!(!p.dry_run);
    }

    #[test]
    fn parse_args_accepts_max_candidates() {
        let p = parse_args(&["bk".into(), "--max-candidates".into(), "5".into()]).unwrap();
        assert_eq!(p.max_candidates, Some(5));
    }

    #[test]
    fn parse_args_accepts_dry_run() {
        let p = parse_args(&["bk".into(), "--dry-run".into()]).unwrap();
        assert!(p.dry_run);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn next_edge_ordinal_finds_max_among_well_formed_ids() {
        use corpus_engine::enrichment::atlas::atoms::AtomId;
        let mk_edge = |id: &str| Edge {
            id: EdgeId::from_raw(id),
            edge_type: EdgeType::Involves,
            source: AtomId::entity(1),
            target: AtomId::entity(2),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };
        let edges = vec![
            mk_edge("edge-00003"),
            mk_edge("edge-00007"),
            mk_edge("edge-00001"),
        ];
        assert_eq!(next_edge_ordinal(&edges), 7);
    }

    #[test]
    fn next_edge_ordinal_zero_on_empty() {
        assert_eq!(next_edge_ordinal(&[]), 0);
    }
}
