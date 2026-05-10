//! `sovereign enrich atlas-gaps` — Phase A Step 4 (Landing 3,
//! gap detection half).
//!
//! Reads the resolved atlas (atoms.json + edges.json) and runs the
//! three deterministic gap detectors from
//! `corpus_engine::enrichment::atlas::analysis::gaps`:
//!
//! - Transitions without a trigger event
//! - Claims without grounding evidence or an inbound Grounds edge
//! - Questions still `Open` after Phase 3b
//!
//! Writes the result to `atlas/gaps.json` as a flat list with
//! sequential ids. Idempotent: running again overwrites with the
//! same ids on the same inputs.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    analysis::gaps::{detect_deterministic_gaps, GapDetectionInput, GapsOutput},
    read_atlas_atoms, read_atlas_edges, write_atlas_gaps, AtomEnvelope, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-gaps",
    summary: "Detect structural gaps in the resolved atlas (deterministic).",
    sections: &[
        HelpSection::Usage("sovereign enrich atlas-gaps <corpus-id>"),
        HelpSection::Examples(&[(
            "sovereign enrich atlas-gaps brothers_karamazov",
            "Scan atoms + edges, detect transitions without triggers / ungrounded claims \
             / open questions, write gaps.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `sovereign enrich atlas-resolve <corpus> --phase all` so the \
             atlas directory exists. Produces \
             `~/.sovereign/indexes/<corpus>/atlas/gaps.json` as a flat list of Gap records \
             with `kind`, `description`, `referenced_atoms`, `evidence`, and `significance`.",
        ),
    ],
};

pub async fn cmd_atlas_gaps(args: &[String]) -> i32 {
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
    let edges = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(err) => {
            eprintln!(
                "error: reading {}/edges.json: {err}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };

    // Partition atoms by kind. Only Claim / State / Question drive
    // detectors today; the other atom types pass through untouched.
    let mut claims = Vec::new();
    let mut states = Vec::new();
    let mut questions = Vec::new();
    for a in atoms.atoms {
        match a {
            AtomEnvelope::Claim(c) => claims.push(c),
            AtomEnvelope::State(s) => states.push(s),
            AtomEnvelope::Question(q) => questions.push(q),
            _ => {}
        }
    }

    println!(
        "  loaded {} claim(s) + {} state(s) + {} question(s) + {} edge(s)",
        claims.len(),
        states.len(),
        questions.len(),
        edges.edges.len(),
    );

    let gaps = detect_deterministic_gaps(GapDetectionInput {
        claims: &claims,
        states: &states,
        questions: &questions,
        edges: &edges.edges,
    });

    // Break down by kind so the operator sees which detectors fired.
    let mut by_kind: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for g in &gaps {
        let key = match g.kind {
            corpus_engine::enrichment::atlas::analysis::gaps::GapKind::TransitionWithoutTrigger => {
                "transition-without-trigger"
            }
            corpus_engine::enrichment::atlas::analysis::gaps::GapKind::UngroundedClaim => {
                "ungrounded-claim"
            }
            corpus_engine::enrichment::atlas::analysis::gaps::GapKind::OpenQuestion => {
                "open-question"
            }
        };
        *by_kind.entry(key).or_insert(0) += 1;
    }

    let total = gaps.len();
    let out = GapsOutput::new(gaps);
    match write_atlas_gaps(&atlas_dir, &out) {
        Ok(path) => {
            println!("  ✓ {total} gap(s) total");
            let mut sorted: Vec<_> = by_kind.into_iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            for (kind, count) in sorted {
                println!("    · {kind}: {count}");
            }
            println!("  ✓ wrote {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("error: writing gaps.json: {e}");
            1
        }
    }
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

#[derive(Debug)]
struct ParsedGaps {
    corpus_id: String,
}

fn parse_args(args: &[String]) -> Result<ParsedGaps, String> {
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
    Ok(ParsedGaps { corpus_id })
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
