// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-tensions-classify` — Phase 6 LLM Tension
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
        classification_to_edge, classification_to_same_as_claim, merge_same_as_claims,
        next_claim_ordinal, resolve_candidate_content, AtomIndex, CandidateContent,
        Phase6Classification, Phase6Verdict, TensionCandidate,
    },
    atoms::AtomId,
    edges::{Edge, EdgeId, EdgeProvenance, EdgeType},
    read_atlas_atoms, read_atlas_edges, read_tension_candidates, write_atlas_atoms,
    write_atlas_edges, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;

/// What the Phase 6 classifier did — or why it did nothing.
///
/// FIVE different things used to return exit `0` from this command: the
/// holistic pass, the per-pair pass, a pipeline that opts into neither,
/// an empty candidate file, and `--dry-run`. The orchestrator saw the
/// same `0` for all five and reported `"Tensions complete"`. Four of
/// them classified nothing at all (ARCH §18.3: absence is reported,
/// never defaulted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifyOutcome {
    /// The corpus-level naturalistic pass ran.
    Holistic {
        /// Fault-line edges written. Zero on a dry run.
        edges: usize,
        runs_succeeded: usize,
        runs_attempted: usize,
        dry_run: bool,
    },
    /// The per-pair classifier ran over the candidate file.
    PerPair {
        accepted: usize,
        rejected: usize,
        /// Pairs the classifier called `equivalent` — one statement in two
        /// surface forms. Each becomes a `same_as` Claim on atoms.json,
        /// NOT a Tension edge (ONTOLOGY_MIGRATION §P4). Always zero for a
        /// corpus that declares no ontology: only a declared corpus is
        /// given the response schema that carries `relation`.
        equivalent: usize,
        chat_failures: usize,
        parse_failures: usize,
        /// Candidates pointing at atoms missing from atoms.json.
        stale_dropped: usize,
    },
    /// The pipeline opts into neither Phase 6 classifier. Nothing ran,
    /// and that is the expected outcome for this corpus — not a failure,
    /// and not a classification either.
    NotOptedIn { pipeline_id: String },
    /// `tension_candidates.json` held no candidates.
    NoCandidates,
    /// `--dry-run`: prompts composed and printed, model never called.
    DryRun { candidates: usize },
}

impl ClassifyOutcome {
    /// One line naming what happened, for the build orchestrator's
    /// `StepDone` event.
    pub fn summary(&self) -> String {
        match self {
            Self::Holistic {
                edges,
                runs_succeeded,
                runs_attempted,
                dry_run,
            } => {
                if *dry_run {
                    "holistic dry run — prompt composed, model not called".to_string()
                } else {
                    let mut s = format!(
                        "holistic: {edges} fault-line edge(s) from {runs_succeeded}/{runs_attempted} run(s)"
                    );
                    if runs_succeeded < runs_attempted {
                        s.push_str(" (some runs failed)");
                    }
                    s
                }
            }
            Self::PerPair {
                accepted,
                rejected,
                equivalent,
                chat_failures,
                parse_failures,
                stale_dropped,
            } => {
                let mut s = format!("{accepted} tension(s), {rejected} rejected");
                if *equivalent > 0 {
                    s.push_str(&format!("; {equivalent} equivalent pair(s) → same_as claim(s)"));
                }
                if chat_failures + parse_failures > 0 {
                    s.push_str(&format!(
                        "; {chat_failures} chat + {parse_failures} parse failure(s) — recall degraded"
                    ));
                }
                if *stale_dropped > 0 {
                    s.push_str(&format!("; {stale_dropped} stale candidate(s) dropped"));
                }
                s
            }
            Self::NotOptedIn { pipeline_id } => {
                format!(
                    "nothing classified — pipeline `{pipeline_id}` opts into no Phase 6 classifier"
                )
            }
            Self::NoCandidates => {
                "nothing classified — tension_candidates.json held 0 candidates".to_string()
            }
            Self::DryRun { candidates } => {
                format!("dry run — {candidates} prompt(s) printed, model not called")
            }
        }
    }
}

/// Classify tension candidates. Keeps its per-candidate progress
/// printing: each candidate is a chat call.
pub async fn run(parsed: &ParsedClassify) -> Result<ClassifyOutcome, String> {
    let cfg = EnrichConfig::require(&parsed.corpus_id)
        .map_err(|e| format!("loading enrichment config: {e}"))?;

    let pipeline = super::pipeline_resolve::resolve_pipeline(&cfg).ok_or_else(|| {
        format!(
            "pipeline '{}' not registered (corpus references unknown pipeline)",
            cfg.pipeline_id
        )
    })?;

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
        return super::atlas_tensions_holistic::run(&cfg, pipeline.as_ref(), &atlas_dir, parsed.dry_run)
            .await;
    }

    if !pipeline.runs_phase6_atlas_classifier() {
        println!(
            "  · pipeline '{}' is not opted into the Phase 6 atlas Tension classifier — skipping.",
            cfg.pipeline_id
        );
        return Ok(ClassifyOutcome::NotOptedIn {
            pipeline_id: cfg.pipeline_id.clone(),
        });
    }

    let candidates = read_tension_candidates(&atlas_dir).map_err(|e| {
        format!(
            "reading {}/tension_candidates.json: {e}. Run `svrn enrich atlas-tensions {}` first.",
            atlas_dir.display(),
            cfg.corpus_id
        )
    })?;

    if candidates.candidates.is_empty() {
        println!("  · 0 candidates in tension_candidates.json — nothing to classify.");
        return Ok(ClassifyOutcome::NoCandidates);
    }

    let atoms = read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("reading {}/atoms.json: {e}", atlas_dir.display()))?;

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
        return Ok(ClassifyOutcome::DryRun {
            candidates: resolved.len(),
        });
    }

    // Build the chat closure once; reuse across candidates.
    let client = DaemonInferenceClient::from_enrich_config(&cfg)
        .map_err(|e| format!("building daemon client: {e}"))?;
    let (_embed, chat) = client.into_closures();

    // Walk the existing edges file. We preserve every non-Tension
    // edge and every Tension edge whose provenance is *not*
    // LlmPairwise (i.e., a hand-authored or future-non-LLM Tension
    // edge stays). LlmPairwise Tension edges from a prior classifier
    // run are dropped and replaced with this run's output.
    let prior_edges = read_atlas_edges(&atlas_dir).map_err(|e| {
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

    let next_edge_ordinal = next_edge_ordinal(&prior_edges.edges);

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    let mut chat_failures = 0usize;
    let mut parse_failures = 0usize;
    // `equivalent` verdicts, reified as `same_as` Claim atoms. Minted from
    // the ordinal after the highest claim already in atoms.json, the same
    // way `next_edge_ordinal` mints edge ids.
    let next_claim_ordinal = next_claim_ordinal(&atoms);
    let mut same_as_claims: Vec<corpus_engine::enrichment::atlas::atoms::Claim> = Vec::new();

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
        // ONE reconciliation of `is_tension` and `relation`
        // (`Phase6Classification::verdict`), so this loop cannot disagree
        // with any other reader about what the model said.
        match cls.verdict() {
            Phase6Verdict::Tension => {
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
            }
            Phase6Verdict::SameAs => {
                let claim_id = AtomId::claim(next_claim_ordinal + same_as_claims.len() + 1);
                match classification_to_same_as_claim(&cls, content, claim_id) {
                    Some(claim) => {
                        println!(" = equivalent → same_as: {}", cls.rationale.trim());
                        same_as_claims.push(claim);
                    }
                    None => {
                        println!(" — verdict said equivalent but claim build returned None");
                        parse_failures += 1;
                    }
                }
            }
            Phase6Verdict::Neither => {
                println!(" · not a tension: {}", cls.rationale.trim());
                rejected += 1;
            }
        }
    }

    println!();
    println!(
        "  Phase 6 classifier summary: {accepted} tension(s), {rejected} rejected, \
         {equivalent} equivalent, {chat_failures} chat failure(s), \
         {parse_failures} parse failure(s)",
        equivalent = same_as_claims.len(),
    );

    let next = corpus_engine::enrichment::atlas::edges::EdgesFile::new(next_edges);
    let path =
        write_atlas_edges(&atlas_dir, &next).map_err(|e| format!("writing edges.json: {e}"))?;
    println!("  ✓ wrote {}", path.display());

    // Reified merges land on atoms.json. `merge_same_as_claims` also drops
    // what a PRIOR run wrote, so a re-run that now finds no equivalence
    // does not leave stale merges standing — the same idempotence contract
    // the Tension edges above have.
    let equivalent = same_as_claims.len();
    let (next_atoms, replaced) = merge_same_as_claims(atoms, same_as_claims);
    if equivalent > 0 || replaced > 0 {
        let path = write_atlas_atoms(&atlas_dir, &next_atoms)
            .map_err(|e| format!("writing atoms.json: {e}"))?;
        println!(
            "  ✓ wrote {} ({equivalent} same_as claim(s); {replaced} from a prior run replaced)",
            path.display(),
        );
    }

    // Chat and parse failures are not a hard error — they degrade recall
    // while the accepted edges still land, so this stays a success. They
    // now RIDE ON the outcome instead of vanishing into exit 0, which is
    // what let a half-classified run read as a clean one.
    Ok(ClassifyOutcome::PerPair {
        accepted,
        rejected,
        equivalent,
        chat_failures,
        parse_failures,
        stale_dropped: stale_drops,
    })
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

/// Highest existing edge ordinal across `edges`. New edges issue
/// ordinals starting from `next_edge_ordinal(&prior) + 1` so we
/// don't clobber existing ids.
pub(super) fn next_edge_ordinal(edges: &[Edge]) -> usize {
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

/// A parsed `atlas-tensions-classify` invocation. Public so the `enrich
/// build` orchestrator constructs one directly instead of round-tripping
/// through argv.
#[derive(Debug, Clone)]
pub struct ParsedClassify {
    pub corpus_id: String,
    pub max_candidates: Option<usize>,
    pub dry_run: bool,
}

pub fn parse_args(args: &[String]) -> Result<ParsedClassify, String> {
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
