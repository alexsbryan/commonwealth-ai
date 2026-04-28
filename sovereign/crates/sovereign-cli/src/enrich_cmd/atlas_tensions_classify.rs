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
        Phase6Classification, TensionCandidate,
    },
    edges::{Edge, EdgeId, EdgeProvenance, EdgeType},
    read_atlas_atoms, read_atlas_edges, read_tension_candidates, write_atlas_edges,
    AtomEnvelope, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::pipeline::PipelineRegistry;

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

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

    if !pipeline.runs_phase6_atlas_classifier() {
        println!(
            "  · pipeline '{}' is not opted into the Phase 6 atlas Tension classifier — skipping.",
            cfg.pipeline_id
        );
        return 0;
    }

    let atlas_dir = atlas_dir_for(&cfg.corpus_id);

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
            e.id.as_str().strip_prefix("edge-").and_then(|s| s.parse::<usize>().ok())
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
                let n: usize = n_str
                    .parse()
                    .map_err(|e| format!("--max-candidates value '{n_str}' is not an integer: {e}"))?;
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
        let p =
            parse_args(&["bk".into(), "--max-candidates".into(), "5".into()]).unwrap();
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
