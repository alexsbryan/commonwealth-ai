// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-tensions-classify` — the HOLISTIC half of Phase 6.
//!
//! Phase 6 has two modes and a pipeline opts into exactly one. The
//! per-pair classifier (`atlas_tensions_classify.rs`) walks
//! `tension_candidates.json` one pair at a time; this one — the philosophy
//! genre's — makes ONE corpus-level call over the atom inventory and
//! resolves each named side back to an entity id.
//!
//! Split out of the per-pair module on 2026-09-02 (ontology-v1 P4), when
//! the per-pair half grew the `equivalent` → `same_as` path and the two
//! together crossed ARCH §3.1's 800-line approach band. The seam was
//! already there: the only thing the two modes share is
//! `next_edge_ordinal` and the `ClassifyOutcome` they both return.
//!
//! ## What it does
//!
//! Reads atoms.json, asks the pipeline for its holistic prompt (which
//! includes the corpus inventory in the user message), makes one chat call,
//! parses the fault-line list, resolves each side's name to an entity id,
//! and merges the results into edges.json as `Tension` edges with entity-id
//! endpoints. The eval's `resolve_endpoint` reads entity ids directly, so
//! the matcher scores these without atom-chasing.
//!
//! Replaces prior `LlmPairwise` Tension edges (so re-running idempotently
//! overwrites the previous holistic run's output); preserves every
//! non-Tension edge and every other-provenance edge.

use corpus_engine::enrichment::atlas::{
    analysis::HolisticTension,
    atoms::{AtomEnvelope, AtomId, AtomsFile, Entity},
    edges::{Edge, EdgeId, EdgeProvenance, EdgeType},
    read_atlas_atoms, read_atlas_edges, write_atlas_edges,
};
use corpus_engine::enrichment::pipeline::atlas::EntityType;

use super::atlas_tensions_classify::{next_edge_ordinal, ClassifyOutcome};
use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;

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
pub(super) async fn run(
    cfg: &EnrichConfig,
    pipeline: &dyn corpus_engine::enrichment::pipeline::Pipeline,
    atlas_dir: &std::path::Path,
    dry_run: bool,
) -> Result<ClassifyOutcome, String> {
    let atoms = read_atlas_atoms(atlas_dir)
        .map_err(|e| format!("reading {}/atoms.json: {e}", atlas_dir.display()))?;

    let prompt = pipeline.compose_phase6_holistic(&atoms).ok_or_else(|| {
        format!(
            "pipeline '{}' opts into holistic but compose returned None",
            cfg.pipeline_id
        )
    })?;

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
        return Ok(ClassifyOutcome::Holistic {
            edges: 0,
            runs_succeeded: 0,
            runs_attempted: 0,
            dry_run: true,
        });
    }

    let client = DaemonInferenceClient::from_enrich_config(cfg)
        .map_err(|e| format!("building daemon client: {e}"))?;
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
    let prior_edges = read_atlas_edges(atlas_dir).map_err(|e| {
        format!(
            "reading {}/edges.json: {e}. Run `svrn enrich atlas-resolve {} --phase all` first.",
            atlas_dir.display(),
            cfg.corpus_id
        )
    })?;
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
    let path =
        write_atlas_edges(atlas_dir, &next).map_err(|e| format!("writing edges.json: {e}"))?;
    println!("  ✓ wrote {}", path.display());

    Ok(ClassifyOutcome::Holistic {
        edges: materialized,
        runs_succeeded: HOLISTIC_RUNS - chat_failures - parse_failures,
        runs_attempted: HOLISTIC_RUNS,
        dry_run: false,
    })
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
