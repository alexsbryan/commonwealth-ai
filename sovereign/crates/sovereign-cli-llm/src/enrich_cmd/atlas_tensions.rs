// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-tensions` — Phase A Step 4 (Landing 3,
//! deterministic half).
//!
//! Reads the resolved atlas (atoms.json + edges.json) and
//! enumerates tension *candidates*: atom pairs plausibly in
//! tension based on two deterministic signals — claim ↔ claim
//! entity-overlap and claim ↔ state entity-overlap. Writes the
//! candidate list to `atlas/tension_candidates.json`.
//!
//! Landing 4 will add an LLM classification pass that consumes
//! this file and materialises real `Tension` edges on
//! `edges.json`. Until then the candidates are a reviewable
//! "what to look at next" list.
//!
//! Candidate selection is strategy-driven (`Pipeline::tension_strategy`):
//! the literary/philosophy atlas uses the deterministic graph signals
//! (entity-overlap + cross-position; intra-cluster is implemented in
//! `analysis::tensions` but still fed an empty cluster map here pending a
//! stable sketch → atom mapping). Custom-ontology (`custom_atlas`)
//! corpora — governance rule-sets, policy docs — instead use an embedding
//! top-K net: each rule is embedded via the daemon embed slot and paired
//! with its nearest neighbours, because their cross-document,
//! uniformly-worded rules defeat the entity/cluster signals. See
//! `TensionStrategy` for why.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    analysis::tensions::{
        drop_same_named_speaker_pairs, select_candidates, select_embedding_topk,
        CandidateSelectionInput, TensionCandidatesOutput, TensionStrategy,
    },
    read_atlas_atoms, write_tension_candidates, AtomEnvelope, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich atlas-tensions",
    summary: "Select tension candidates from the resolved atlas (deterministic).",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-tensions <corpus-id>"),
        HelpSection::Examples(&[(
            "svrn enrich atlas-tensions brothers_karamazov",
            "Scan atoms.json, enumerate entity-overlap candidate pairs, write tension_candidates.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `svrn enrich atlas-resolve <corpus> --phase all` so the \
             atlas directory exists. Produces \
             `~/.sovereign/indexes/<corpus>/atlas/tension_candidates.json`. Does NOT call \
             the LLM — the classifier that promotes candidates to real Tension edges lands \
             in a later step.",
        ),
    ],
};

pub async fn cmd_atlas_tensions(args: &[String]) -> i32 {
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

    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: reading {}/atoms.json: {e}. Run `svrn enrich atlas-resolve \
                 {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };

    // Partition atoms by kind. Claim + State drive entity-overlap
    // candidates; entities feed the cross-position concept-overlap
    // signal (claims attributed to different position concepts whose
    // content mentions the same entity).
    let mut claims = Vec::new();
    let mut states = Vec::new();
    let mut entities = Vec::new();
    for a in atoms.atoms {
        match a {
            AtomEnvelope::Claim(c) => claims.push(c),
            AtomEnvelope::State(s) => states.push(s),
            AtomEnvelope::Entity(e) => entities.push(e),
            _ => {}
        }
    }

    println!(
        "  loaded {} claim atom(s) + {} state atom(s) + {} entity atom(s) from atlas",
        claims.len(),
        states.len(),
        entities.len(),
    );

    // Candidate selection is strategy-driven (glassbox: the chosen
    // strategy is logged). The literary/philosophy atlas uses the
    // deterministic graph signals; custom-ontology corpora use an
    // embedding top-K net (see `TensionStrategy`). Resolve the pipeline
    // to read its strategy; fall back to the graph default if the
    // pipeline can't be resolved (preserves legacy behaviour).
    let strategy = super::pipeline_resolve::resolve_pipeline(&cfg)
        .map(|p| p.tension_strategy())
        .unwrap_or_default();

    let mut candidates = match strategy {
        TensionStrategy::Graph => {
            println!("  · candidate strategy: graph (cluster + entity-overlap + co-occurrence)");
            select_candidates(CandidateSelectionInput {
                claims: &claims,
                states: &states,
                // Intra-cluster candidates not wired here — see module
                // comment in `tensions.rs`.
                claim_clusters: &[],
                entities: &entities,
            })
        }
        TensionStrategy::EmbeddingTopK { k, floor } => {
            println!("  · candidate strategy: embedding top-K (k={k}, floor={floor})");
            // Embed each rule's text via the daemon embed slot, then pair
            // by cosine. The graph path needs no model; this path does —
            // but a custom-ontology build already has the daemon up for
            // the classifier that follows.
            let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: building inference client for embeddings: {e}");
                    return 1;
                }
            };
            let (embed, _chat) = client.into_closures();
            let mut embeddings = Vec::with_capacity(claims.len());
            for (i, c) in claims.iter().enumerate() {
                match embed(&c.content).await {
                    Ok(v) => embeddings.push(v),
                    Err(e) => {
                        eprintln!("error: embedding claim {i} ({}): {e}", c.id.as_str());
                        return 1;
                    }
                }
            }
            println!(
                "  · embedded {} claim(s) for similarity selection",
                embeddings.len()
            );
            select_embedding_topk(&claims, &embeddings, k, floor)
        }
    };

    // Strategy-agnostic de-noise: drop pairs where both claims are by the
    // same named speaker (Person/Institution). Critical for the embedding
    // top-K net, which otherwise pairs a speaker's many mutually-consistent
    // claims by topic similarity. (No-op for Concept/topic attributions —
    // see `drop_same_named_speaker_pairs`.)
    let before = candidates.len();
    drop_same_named_speaker_pairs(&mut candidates, &claims, &entities);
    if candidates.len() < before {
        println!(
            "  · same-speaker filter: {} -> {} candidates (dropped {} same-speaker pair(s))",
            before,
            candidates.len(),
            before - candidates.len()
        );
    }

    if candidates.is_empty() {
        println!(
            "  · no entity-overlap candidates found. Either no claim has an `attributed_to` \
             set, or every attribution is singular (no pair possible)."
        );
    }

    let out = TensionCandidatesOutput::new(candidates);
    let count = out.candidates.len();
    match write_tension_candidates(&atlas_dir, &out) {
        Ok(path) => {
            println!("  ✓ {count} candidate pair(s)");
            println!("  ✓ wrote {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("error: writing tension_candidates.json: {e}");
            1
        }
    }
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

#[derive(Debug)]
struct ParsedTensions {
    corpus_id: String,
}

fn parse_args(args: &[String]) -> Result<ParsedTensions, String> {
    let mut corpus_id: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    Ok(ParsedTensions { corpus_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_bare_corpus_id() {
        let p = parse_args(&["brothers_karamazov".into()]).unwrap();
        assert_eq!(p.corpus_id, "brothers_karamazov");
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
