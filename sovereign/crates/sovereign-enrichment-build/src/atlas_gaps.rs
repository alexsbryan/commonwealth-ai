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
    analysis::{
        gaps::{detect_deterministic_gaps, GapDetectionInput, GapKind, GapsOutput},
        patterns_adapter::{to_investigation_graph, PatternFindingsOutput},
    },
    read_atlas_atoms, read_atlas_edges, read_atlas_ontology, write_atlas_gaps,
    write_atlas_pattern_findings, AtomEnvelope, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::investigation::patterns::detect_all;

use super::config::EnrichConfig;
use super::paths;

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
    /// The declared-pattern pass, or `None` when the corpus declares none.
    pub patterns: Option<PatternRun>,
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

    // Axis 5's declared `patterns`, over the SAME atlas — run before the
    // partition below consumes `atoms`. No-op (and no file) for a corpus
    // that declares none, which is every corpus built before ontology v1.
    let patterns = run_declared_patterns(&atlas_dir, &atoms)?;

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
        patterns,
        written_to,
    })
}

/// What the declared `patterns` found, or `None` when none are declared.
///
/// The detectors are `investigation::patterns::detect_all` unchanged — the
/// atlas is projected into the graph they already read
/// (`to_investigation_graph`) rather than a second detector set being
/// written for it (ARCH §19).
fn run_declared_patterns(
    atlas_dir: &std::path::Path,
    atoms: &corpus_engine::enrichment::atlas::atoms::AtomsFile,
) -> Result<Option<PatternRun>, String> {
    let Some(ontology) = read_atlas_ontology(atlas_dir) else {
        return Ok(None);
    };
    let declared = &ontology.policies.derivation.patterns;
    if declared.is_empty() {
        return Ok(None);
    }
    let graph = to_investigation_graph(atoms);
    let findings = detect_all(declared, &graph.entities, &graph.relationships);
    let out = PatternFindingsOutput::new(findings, graph.non_binary_relations);
    let findings = out.findings.len();
    let written_to = write_atlas_pattern_findings(atlas_dir, &out)
        .map_err(|e| format!("writing pattern_findings.json: {e}"))?;
    Ok(Some(PatternRun {
        declared: declared.len(),
        entities: graph.entities.len(),
        relationships: graph.relationships.len(),
        non_binary_relations: graph.non_binary_relations,
        findings,
        written_to,
    }))
}

/// What the declared-pattern pass did. Carried on the report for the same
/// reason [`GapInputs`] is: zero findings over zero projected edges and
/// zero findings over a full graph are different outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternRun {
    pub declared: usize,
    pub entities: usize,
    pub relationships: usize,
    pub non_binary_relations: usize,
    pub findings: usize,
    pub written_to: PathBuf,
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
    if let Some(p) = &report.patterns {
        println!(
            "  · {} declared pattern(s) over {} entity/{} relation projection: {} finding(s)",
            p.declared, p.entities, p.relationships, p.findings,
        );
        if p.non_binary_relations > 0 {
            println!(
                "    ⚠ {} relation atom(s) had other than two participants and were not \
                 projected — the detectors are binary-edge algorithms",
                p.non_binary_relations,
            );
        }
        println!("  ✓ wrote {}", p.written_to.display());
    }
    println!("  ✓ wrote {}", report.written_to.display());
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

pub fn parse_args(args: &[String]) -> Result<ParsedGaps, String> {
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
            patterns: None,
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
            patterns: None,
            written_to: PathBuf::from("/tmp/gaps.json"),
        };
        let s = r.summary();
        assert!(s.contains("4 gap(s)"), "{s}");
        assert!(s.contains("3 open-question"), "{s}");
        assert!(s.contains("1 ungrounded-claim"), "{s}");
    }
}
