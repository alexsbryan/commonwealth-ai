//! `sovereign enrich atlas-cross-corpus` — Phase C Step 8 driver.
//!
//! Detects cross-corpus Grounding edges between two resolved
//! atlases and writes a bidirectional
//! `atlas/cross_corpus_edges.json` into each corpus's directory.
//! **Glass-box observability is the first-class concern here**
//! — the default output prints the full
//! [`CrossCorpusReport`]: per-detector candidate/match/rejection
//! counts, rejection reasons grouped, sample rejections with the
//! exact folded forms that missed.
//!
//! `--explain <edge-id>` prints the full [`MatchTrace`] for one
//! edge — signal path, confidence, alternatives considered — so
//! an operator auditing the atlas can ask "why was this bridge
//! built?" and see the decision verbatim.
//!
//! Today's implementation ships Grounding only (deterministic,
//! zero LLM). Framing + Provenance land in follow-ups.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    detect_grounding, read_atlas_atoms, write_atlas_cross_corpus_edges, AtomEnvelope,
    CrossCorpusEdgesFile, CrossCorpusInput, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-cross-corpus",
    summary: "Detect cross-corpus Grounding edges between two resolved atlases.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich atlas-cross-corpus <local-corpus> <peer-corpus> [--explain <edge-id>]",
        ),
        HelpSection::Flags(&[(
            "--explain <edge-id>",
            "After the run, print the full MatchTrace for one edge. Use to audit \
             why a specific bridge was built.",
        )]),
        HelpSection::Examples(&[
            (
                "sovereign enrich atlas-cross-corpus brothers_karamazov sep",
                "Match entity atoms across BK and SEP; write cross_corpus_edges.json into both atlases.",
            ),
            (
                "sovereign enrich atlas-cross-corpus bk sep --explain cc-bk-0003",
                "Inspect the exact signal path that accepted edge cc-bk-0003.",
            ),
        ]),
        HelpSection::Notes(
            "Both corpora must have been resolved via `sovereign enrich atlas-resolve \
             <corpus> --phase all`. Zero LLM calls — the Grounding detector is \
             fully deterministic. Framing + Provenance detectors land in follow-ups.",
        ),
    ],
};

pub async fn cmd_atlas_cross_corpus(args: &[String]) -> i32 {
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

    // Load both enrichment configs so the corpus ids are validated
    // + printed in the report exactly as declared.
    let local_cfg = match EnrichConfig::require(&parsed.local_corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading local enrichment config: {e}");
            return 1;
        }
    };
    let peer_cfg = match EnrichConfig::require(&parsed.peer_corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading peer enrichment config: {e}");
            return 1;
        }
    };

    // Load atoms from both atlases.
    let local_dir = atlas_dir_for(&local_cfg.corpus_id);
    let peer_dir = atlas_dir_for(&peer_cfg.corpus_id);
    let local_entities = match load_entities(&local_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "error: reading local atlas at {}: {e}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                local_dir.display(),
                local_cfg.corpus_id
            );
            return 1;
        }
    };
    let peer_entities = match load_entities(&peer_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "error: reading peer atlas at {}: {e}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                peer_dir.display(),
                peer_cfg.corpus_id
            );
            return 1;
        }
    };

    println!(
        "  loaded {} local entity atom(s) + {} peer entity atom(s)",
        local_entities.len(),
        peer_entities.len(),
    );

    let report = detect_grounding(CrossCorpusInput {
        local_corpus_id: &local_cfg.corpus_id,
        local_entities: &local_entities,
        peer_corpus_id: &peer_cfg.corpus_id,
        peer_entities: &peer_entities,
    });

    // Glass-box summary.
    println!();
    println!("=== Cross-corpus report ===");
    for s in &report.detectors {
        println!(
            "  detector={} candidates={} accepted={} rejected={}",
            s.detector,
            s.candidates_scanned,
            s.matches_accepted,
            s.candidates_scanned - s.matches_accepted,
        );
        if !s.rejections_by_reason.is_empty() {
            println!("    rejections by reason:");
            let mut buckets: Vec<_> = s.rejections_by_reason.iter().collect();
            buckets.sort_by(|a, b| b.count.cmp(&a.count));
            for b in buckets {
                println!("      · {:30} {}", b.reason, b.count);
            }
        }
        if !s.sample_rejections.is_empty() {
            println!(
                "    sample rejections (cap {}):",
                s.sample_rejections.len()
            );
            for sample in &s.sample_rejections {
                println!(
                    "      · {} ({:?}) ↮ {} ({:?})  reason={}",
                    sample.local_atom_id.as_str(),
                    sample.local_form,
                    sample.peer_atom_id.as_str(),
                    sample.peer_form,
                    sample.reason,
                );
            }
        }
    }
    println!();

    // If --explain was requested, find the edge and print the full
    // trace before writing files.
    if let Some(target_id) = &parsed.explain_edge_id {
        println!("=== Explain: {} ===", target_id);
        let found = report
            .accepted_edges
            .iter()
            .find(|e| e.edge.id.as_str() == target_id.as_str());
        match found {
            Some(e) => {
                println!("  detector:   {}", e.trace.detector);
                println!("  signal:     {}", e.trace.signal);
                println!("  local_form: {:?}", e.trace.local_form);
                println!("  peer_form:  {:?}", e.trace.peer_form);
                println!("  confidence: {:.2}", e.trace.confidence);
                println!(
                    "  source:     {} (local {})",
                    e.edge.source.as_str(),
                    local_cfg.corpus_id
                );
                println!(
                    "  target:     {} (peer  {}, canonical {:?})",
                    e.edge.target.as_str(),
                    e.peer.corpus_id,
                    e.peer.canonical_name,
                );
                if !e.trace.rejected_alternatives.is_empty() {
                    println!("  rejected alternatives:");
                    for alt in &e.trace.rejected_alternatives {
                        println!("    · {alt}");
                    }
                }
            }
            None => {
                println!(
                    "  (edge not in this run's accepted set — check the id and \
                     rerun; accepted edges for this run:)"
                );
                for e in &report.accepted_edges {
                    println!("    · {}", e.edge.id.as_str());
                }
                return 2;
            }
        }
        println!();
    }

    if report.accepted_edges.is_empty() {
        println!("  (no cross-corpus edges accepted; not writing output files.)");
        return 0;
    }

    // Build local-side + peer-side views. Local side keeps the
    // edges verbatim. Peer side uses `flip_for_peer` so its file
    // reads "my atom X → local's atom Y".
    let local_canonical_by_id: std::collections::HashMap<String, String> = local_entities
        .iter()
        .map(|e| (e.id.as_str().to_string(), e.canonical_name.clone()))
        .collect();

    let local_file = CrossCorpusEdgesFile::new(
        local_cfg.corpus_id.clone(),
        report.accepted_edges.clone(),
    );
    let peer_edges: Vec<_> = report
        .accepted_edges
        .iter()
        .map(|e| {
            let local_canonical = local_canonical_by_id
                .get(e.edge.source.as_str())
                .cloned()
                .unwrap_or_default();
            e.flip_for_peer(local_canonical, local_cfg.corpus_id.clone())
        })
        .collect();
    let peer_file = CrossCorpusEdgesFile::new(peer_cfg.corpus_id.clone(), peer_edges);

    match write_atlas_cross_corpus_edges(&local_dir, &local_file) {
        Ok(path) => println!("  ✓ wrote {}", path.display()),
        Err(e) => {
            eprintln!("error: writing local cross_corpus_edges.json: {e}");
            return 1;
        }
    }
    match write_atlas_cross_corpus_edges(&peer_dir, &peer_file) {
        Ok(path) => println!("  ✓ wrote {}", path.display()),
        Err(e) => {
            eprintln!("error: writing peer cross_corpus_edges.json: {e}");
            return 1;
        }
    }

    println!("  ✓ {} edge(s) bridged", report.accepted_edges.len());
    0
}

fn load_entities(
    atlas_dir: &std::path::Path,
) -> std::io::Result<Vec<corpus_engine::enrichment::atlas::Entity>> {
    let file = read_atlas_atoms(atlas_dir)?;
    let mut entities = Vec::new();
    for a in file.atoms {
        if let AtomEnvelope::Entity(e) = a {
            entities.push(e);
        }
    }
    Ok(entities)
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

#[derive(Debug)]
struct ParsedCrossCorpus {
    local_corpus_id: String,
    peer_corpus_id: String,
    explain_edge_id: Option<String>,
}

fn parse_args(args: &[String]) -> Result<ParsedCrossCorpus, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut explain_edge_id: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--explain" => {
                let val = args.get(i + 1).ok_or_else(|| {
                    "--explain requires an edge id argument".to_string()
                })?;
                explain_edge_id = Some(val.clone());
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    if positional.len() != 2 {
        return Err(format!(
            "expected <local-corpus> <peer-corpus>, got {} positional argument(s)",
            positional.len()
        ));
    }
    Ok(ParsedCrossCorpus {
        local_corpus_id: positional[0].clone(),
        peer_corpus_id: positional[1].clone(),
        explain_edge_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_two_corpus_ids() {
        let p = parse_args(&["bk".into(), "sep".into()]).unwrap();
        assert_eq!(p.local_corpus_id, "bk");
        assert_eq!(p.peer_corpus_id, "sep");
        assert!(p.explain_edge_id.is_none());
    }

    #[test]
    fn parse_args_accepts_explain_flag() {
        let p = parse_args(&[
            "bk".into(),
            "sep".into(),
            "--explain".into(),
            "cc-bk-0003".into(),
        ])
        .unwrap();
        assert_eq!(p.explain_edge_id.as_deref(), Some("cc-bk-0003"));
    }

    #[test]
    fn parse_args_rejects_missing_peer_corpus() {
        let err = parse_args(&["bk".into()]).unwrap_err();
        assert!(err.contains("local-corpus") || err.contains("peer-corpus"));
    }

    #[test]
    fn parse_args_rejects_explain_without_value() {
        let err = parse_args(&["bk".into(), "sep".into(), "--explain".into()]).unwrap_err();
        assert!(err.contains("--explain"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "sep".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn parse_args_rejects_extra_positional() {
        let err = parse_args(&["bk".into(), "sep".into(), "extra".into()]).unwrap_err();
        assert!(err.contains("positional"));
    }
}
