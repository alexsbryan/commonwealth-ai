// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-gaps` — Phase A Step 4 (Landing 3,
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
//!
//! # The verb triple
//!
//! `parse_args` → [`run`] → [`render`], with [`cmd_atlas_gaps`] the thin
//! argv adapter over the three. The split exists so `enrich build`'s
//! orchestrator can call [`run`] with a typed [`ParsedGaps`] and receive a
//! typed [`GapsReport`] — rather than serialising a corpus id into a
//! `Vec<String>` to call its own sibling and getting back an `i32` that
//! cannot say what happened (ARCH §18.3).

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    analysis::gaps::{detect_deterministic_gaps, GapDetectionInput, GapKind, GapsOutput},
    read_atlas_atoms, read_atlas_edges, write_atlas_gaps, AtomEnvelope, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich atlas-gaps",
    summary: "Detect structural gaps in the resolved atlas (deterministic).",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-gaps <corpus-id>"),
        HelpSection::Examples(&[(
            "svrn enrich atlas-gaps brothers_karamazov",
            "Scan atoms + edges, detect transitions without triggers / ungrounded claims \
             / open questions, write gaps.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `svrn enrich atlas-resolve <corpus> --phase all` so the \
             atlas directory exists. Produces \
             `~/.svrnmesh/indexes/<corpus>/atlas/gaps.json` as a flat list of Gap records \
             with `kind`, `description`, `referenced_atoms`, `evidence`, and `significance`.",
        ),
    ],
};

/// What the detectors were given. Carried on the report so the render half
/// can show the operator the inputs alongside the findings — a zero-gap run
/// over zero atoms and a zero-gap run over a rich atlas are different
/// outcomes, and only one of them is good news.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapInputs {
    pub claims: usize,
    pub states: usize,
    pub questions: usize,
    pub edges: usize,
}

/// What this step actually did. Returned by [`run`] so a caller gets the
/// counts, not a bare exit code.
#[derive(Debug, Clone)]
pub struct GapsReport {
    pub inputs: GapInputs,
    /// Gap counts by detector, sorted by kind so output is stable.
    pub by_kind: Vec<(&'static str, usize)>,
    pub total: usize,
    pub written_to: PathBuf,
}

impl GapsReport {
    /// One line naming what this step did, for the build orchestrator's
    /// `StepDone` event. Derived from the run — never a fabricated
    /// "<step> complete".
    pub fn summary(&self) -> String {
        if self.total == 0 {
            format!(
                "no gaps over {} claim(s) + {} state(s) + {} question(s)",
                self.inputs.claims, self.inputs.states, self.inputs.questions
            )
        } else {
            let kinds = self
                .by_kind
                .iter()
                .map(|(k, n)| format!("{n} {k}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} gap(s): {kinds}", self.total)
        }
    }
}

fn kind_label(kind: GapKind) -> &'static str {
    match kind {
        GapKind::TransitionWithoutTrigger => "transition-without-trigger",
        GapKind::UngroundedClaim => "ungrounded-claim",
        GapKind::OpenQuestion => "open-question",
    }
}

/// Detect gaps and write `gaps.json`. Pure of stdout: everything the
/// operator sees comes from [`render`], so the orchestrator can call this
/// and emit its own progress events instead.
pub fn run(parsed: &ParsedGaps) -> Result<GapsReport, String> {
    let cfg = EnrichConfig::require(&parsed.corpus_id)
        .map_err(|e| format!("loading enrichment config: {e}"))?;

    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    let atoms = read_atlas_atoms(&atlas_dir).map_err(|e| {
        format!(
            "reading {}/atoms.json: {e}. Run `svrn enrich atlas-resolve {} --phase all` first.",
            atlas_dir.display(),
            cfg.corpus_id
        )
    })?;
    let edges = read_atlas_edges(&atlas_dir).map_err(|e| {
        format!(
            "reading {}/edges.json: {e}. Run `svrn enrich atlas-resolve {} --phase all` first.",
            atlas_dir.display(),
            cfg.corpus_id
        )
    })?;

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

    let inputs = GapInputs {
        claims: claims.len(),
        states: states.len(),
        questions: questions.len(),
        edges: edges.edges.len(),
    };

    let gaps = detect_deterministic_gaps(GapDetectionInput {
        claims: &claims,
        states: &states,
        questions: &questions,
        edges: &edges.edges,
    });

    // Break down by kind so the operator sees which detectors fired.
    let mut by_kind: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    for g in &gaps {
        *by_kind.entry(kind_label(g.kind)).or_insert(0) += 1;
    }
    let mut by_kind: Vec<(&'static str, usize)> = by_kind.into_iter().collect();
    by_kind.sort_by(|a, b| a.0.cmp(b.0));

    let total = gaps.len();
    let out = GapsOutput::new(gaps);
    let written_to =
        write_atlas_gaps(&atlas_dir, &out).map_err(|e| format!("writing gaps.json: {e}"))?;

    Ok(GapsReport {
        inputs,
        by_kind,
        total,
        written_to,
    })
}

/// Print the report the way `svrn enrich atlas-gaps` always has.
pub fn render(report: &GapsReport) {
    println!(
        "  loaded {} claim(s) + {} state(s) + {} question(s) + {} edge(s)",
        report.inputs.claims, report.inputs.states, report.inputs.questions, report.inputs.edges,
    );
    println!("  ✓ {} gap(s) total", report.total);
    for (kind, count) in &report.by_kind {
        println!("    · {kind}: {count}");
    }
    println!("  ✓ wrote {}", report.written_to.display());
}

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

    match run(&parsed) {
        Ok(report) => {
            render(&report);
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

/// A parsed `atlas-gaps` invocation. Public so the `enrich build`
/// orchestrator can construct one directly instead of round-tripping
/// through argv.
#[derive(Debug, Clone)]
pub struct ParsedGaps {
    pub corpus_id: String,
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

    /// The summary a zero-gap run hands the orchestrator must name what it
    /// scanned. "0 gaps over an empty atlas" and "0 gaps over 400 claims"
    /// are different outcomes; a summary that cannot tell them apart is the
    /// placeholder this step's report replaced.
    #[test]
    fn zero_gap_summary_names_the_inputs_it_scanned() {
        let empty = GapsReport {
            inputs: GapInputs {
                claims: 0,
                states: 0,
                questions: 0,
                edges: 0,
            },
            by_kind: Vec::new(),
            total: 0,
            written_to: PathBuf::from("/tmp/gaps.json"),
        };
        let rich = GapsReport {
            inputs: GapInputs {
                claims: 400,
                states: 12,
                questions: 7,
                edges: 900,
            },
            ..empty.clone()
        };
        assert_ne!(empty.summary(), rich.summary());
        assert!(rich.summary().contains("400"));
    }

    #[test]
    fn nonzero_gap_summary_names_each_detector_that_fired() {
        let r = GapsReport {
            inputs: GapInputs {
                claims: 10,
                states: 2,
                questions: 3,
                edges: 20,
            },
            by_kind: vec![("open-question", 3), ("ungrounded-claim", 1)],
            total: 4,
            written_to: PathBuf::from("/tmp/gaps.json"),
        };
        let s = r.summary();
        assert!(s.contains("4 gap(s)"), "{s}");
        assert!(s.contains("3 open-question"), "{s}");
        assert!(s.contains("1 ungrounded-claim"), "{s}");
    }
}
