//! `sovereign enrich atlas-tensions` — Phase A Step 4 (Landing 3,
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
//! Note: intra-cluster candidates (claim pairs within a Phase 2
//! cluster) are implemented in `analysis::tensions` but not wired
//! here yet — the runtime doesn't have a stable sketch → atom
//! mapping to hand the selector. That's a follow-up; today's
//! ship is entity-overlap-only, which is the highest-quality
//! signal anyway.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    analysis::tensions::{
        select_candidates, CandidateSelectionInput, TensionCandidatesOutput,
    },
    read_atlas_atoms, write_tension_candidates, AtomEnvelope, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-tensions",
    summary: "Select tension candidates from the resolved atlas (deterministic).",
    sections: &[
        HelpSection::Usage("sovereign enrich atlas-tensions <corpus-id>"),
        HelpSection::Examples(&[(
            "sovereign enrich atlas-tensions brothers_karamazov",
            "Scan atoms.json, enumerate entity-overlap candidate pairs, write tension_candidates.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `sovereign enrich atlas-resolve <corpus> --phase all` so the \
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
                "error: reading {}/atoms.json: {e}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };

    // Partition atoms by kind. Only Claim + State drive entity-overlap
    // candidates today; the other atom types pass through untouched.
    let mut claims = Vec::new();
    let mut states = Vec::new();
    for a in atoms.atoms {
        match a {
            AtomEnvelope::Claim(c) => claims.push(c),
            AtomEnvelope::State(s) => states.push(s),
            _ => {}
        }
    }

    println!(
        "  loaded {} claim atom(s) + {} state atom(s) from atlas",
        claims.len(),
        states.len(),
    );

    let candidates = select_candidates(CandidateSelectionInput {
        claims: &claims,
        states: &states,
        // Intra-cluster candidates not wired in Landing 3 — see
        // module-level comment above.
        claim_clusters: &[],
    });

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
