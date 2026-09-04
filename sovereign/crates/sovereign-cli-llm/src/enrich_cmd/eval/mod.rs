// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich eval <corpus-id> <golden-set>` — score the
//! resolved atlas against a hand-authored golden set.
//!
//! The eval surface is the measurement half of the philosophy tuning
//! loop: `enrich init --from-template <name>` scaffolds a corpus,
//! `enrich build` runs the pipeline against it, and this subcommand
//! reports per-phase precision / recall / F1 against
//! `bench/philosophy/<name>.toml`. The same golden set lives next to
//! the template, so a prompt-tuning iteration is a tight loop:
//!
//! ```sh
//!     enrich init <id> --from-template free-will-debate --force
//!     enrich build <id>
//!     enrich eval <id> bench/philosophy/free-will-debate.toml
//! ```
//!
//! Match semantics (TOML keys):
//!
//! - `name_contains_any` / `canonical_name_contains_any` —
//!   case-insensitive substring of the candidate's display name; ANY
//!   listed substring satisfies the match.
//! - `description_keywords_any` — case-insensitive substring of the
//!   candidate's description / claim / crux text; ANY satisfies.
//! - `proponents_any` — for Phase 1 positions only; ANY listed name
//!   appears in the position's proponent list.
//! - `epistemic_status` (positions only) — exact match against the
//!   position's status string ("majority" | "minority" | "contested").
//! - `forbidden_*` blocks — anti-tests. A matching extraction counts
//!   as a false positive; a non-match is correct silence.
//!
//! Scoring: precision counts only `forbidden_*` matches as FPs (the
//! pipeline can produce many reasonable atoms beyond the listed
//! goldens — penalising those would punish correct breadth). Recall
//! is per-`expected_*` block: how many of the listed expectations the
//! atlas covered.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::analysis::configuration::ConfigurationsOutput;
use corpus_engine::enrichment::atlas::analysis::gaps::{Gap, GapKind, GapsOutput};
use corpus_engine::enrichment::atlas::atoms::{
    AtomEnvelope, AtomId, AtomsFile, ChunkRef, Configuration, Entity, Event, Opposition, Position,
    Question, Relation, State,
};
use corpus_engine::enrichment::atlas::axis_catalog::{
    all_axes, AxisAtomShape, GatingField, TypedAxis,
};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeType, EdgesFile};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use corpus_engine::enrichment::skeleton::{FieldSkeleton, SkeletonPosition};
use serde::{Deserialize, Serialize};

use super::config::EnrichConfig;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich eval",
    summary: "Score the resolved atlas against a golden-set TOML; report per-phase precision/recall/F1.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich eval <corpus-id> <golden-set-path> \\\n  [--phase positions|atoms|fault-lines|gaps|configurations|all] \\\n  [--report <json-path>]",
        ),
        HelpSection::Flags(&[
            (
                "--phase <id>",
                "Restrict scoring to one phase. Default: all. Phases: positions (Phase 1 skeleton), atoms (Phase 3a/3b entities + concepts + questions + claims), edges (Phase 3b typed edges, directed), fault-lines (Phase 6 Tension edges between positions), gaps (Phase 7 open questions), configurations (Phase 8).",
            ),
            (
                "--report <path>",
                "Write structured JSON output to this path (in addition to printing the text table to stdout). Useful for tracking F1 across prompt iterations.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich eval fwd bench/philosophy/free-will-debate.toml",
                "Full per-phase scoreboard against a corpus initialised from --from-template free-will-debate.",
            ),
            (
                "svrn enrich eval fwd bench/philosophy/free-will-debate.toml --phase fault-lines --report /tmp/fault-lines.json",
                "Score only the Phase 6 fault-line detector and persist the result for later diff.",
            ),
        ]),
        HelpSection::Notes(
            "Reads ~/.svrnmesh/indexes/<corpus>/atlas/{atoms,edges,gaps,configurations,tension_candidates}.json and ~/.svrnmesh/indexes/<corpus>/field_skeleton.json. Phases whose artefacts are absent are skipped with a note rather than scored as zero — the table column shows '—' so a partial pipeline run does not look like a regression.",
        ),
    ],
};

pub async fn cmd_eval(args: &[String]) -> i32 {
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

    let report = match score_corpus(&parsed.corpus_id, &parsed.golden_path, parsed.phase) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    print_text_report(&report);

    if let Some(path) = parsed.report_path.as_ref() {
        match write_json_report(path, &report) {
            Ok(_) => println!("\n  ✓ wrote {}", path.display()),
            Err(e) => {
                eprintln!("error: writing report {}: {e}", path.display());
                return 1;
            }
        }
    }

    0
}

/// Run the eval scorer against an existing atlas and return the
/// `EvalReport`. Used by both `cmd_eval` (which prints + optionally
/// persists JSON) and `cmd_eval_median` (which calls this N times
/// and aggregates).
pub(crate) fn score_corpus(
    corpus_id: &str,
    golden_path: &Path,
    phase: PhaseFilter,
) -> Result<EvalReport, String> {
    EnrichConfig::require(corpus_id).map_err(|e| e.to_string())?;
    let golden = GoldenSet::load(golden_path)?;
    let atlas_dir = paths::index_root(corpus_id).join(ATLAS_DIRNAME);
    let skeleton_path = paths::index_root(corpus_id).join("field_skeleton.json");
    let snapshot = AtlasSnapshot::load(&atlas_dir, &skeleton_path)?;
    let mut report = score(&golden, &snapshot, phase);
    report.corpus_id = corpus_id.to_string();
    report.golden_path = golden_path.display().to_string();
    Ok(report)
}

mod adjudicate;
mod args;
mod axis_driver;
mod golden;
mod matching;
mod scorers;
mod snapshot;

// Glob re-exports, deliberately: the seven modules above are one unit, and a
// sibling reaching a sibling goes through here. Names cannot collide — they
// were all top-level items of the single file this replaced. `pub(crate)`
// because this module's surface was never just `cmd_eval`: bench_cmd::all,
// bench_cmd::adjudicate and eval_median import score_corpus / EvalReport /
// PhaseFilter / PhaseScore / UnmatchedAtom / collect_unmatched_atoms /
// load_golden_and_snapshot from here, and the split must not narrow that.
pub(crate) use adjudicate::*;
pub(crate) use args::*;
pub(crate) use axis_driver::*;
pub(crate) use golden::*;
pub(crate) use matching::*;
// `scorers` exports nothing wider than `pub(super)` — every one of its
// entry points is reached from inside this module, so a `pub(crate)`
// re-export here would claim a surface that does not exist.
use scorers::*;
pub(crate) use snapshot::*;

// ── Reporting ──────────────────────────────────────────────────────

fn fmt_pct(v: Option<f32>) -> String {
    match v {
        None => "  —  ".to_string(),
        Some(x) => format!("{:>5.1}%", x * 100.0),
    }
}

fn print_phase_row(label: &str, score: Option<&PhaseScore>) {
    let Some(s) = score else {
        return;
    };
    let p = fmt_pct(s.precision());
    let r = fmt_pct(s.recall());
    let f = fmt_pct(s.f1());
    let u = fmt_pct(s.unmatched_rate());
    println!(
        "  {label:<22}  {matched:>3}/{exp:<3}    P {p}   R {r}   F1 {f}    FP {fp}/{ft}    U {un}/{cand} {u}",
        matched = s.matched,
        exp = s.expected,
        fp = s.forbidden_hit,
        ft = s.forbidden_total,
        un = s.unmatched_count,
        cand = s.candidates,
    );
    if !s.unmatched_samples.is_empty() {
        let preview: Vec<String> = s.unmatched_samples.iter().take(4).cloned().collect();
        let suffix = if s.unmatched_count > preview.len() {
            format!(" (+{} more)", s.unmatched_count - preview.len())
        } else {
            String::new()
        };
        println!(
            "                          unmatched: {}{suffix}",
            preview.join(", ")
        );
    }
    for note in &s.notes {
        println!("                          note: {note}");
    }
    if !s.misses.is_empty() {
        let preview: Vec<String> = s.misses.iter().take(4).cloned().collect();
        let suffix = if s.misses.len() > preview.len() {
            format!(" (+{} more)", s.misses.len() - preview.len())
        } else {
            String::new()
        };
        println!(
            "                          misses: {}{suffix}",
            preview.join(", ")
        );
    }
    if !s.forbidden_hits.is_empty() {
        println!(
            "                          forbidden hits: {}",
            s.forbidden_hits.join(", ")
        );
    }
}

fn print_text_report(r: &EvalReport) {
    println!();
    println!("  Phase scoreboard");
    println!("  ─────────────────────────────────────────────────────────────");
    print_phase_row("positions (Phase 1)", r.positions.as_ref());
    print_phase_row("person atoms", r.person_atoms.as_ref());
    print_phase_row("concept atoms", r.concept_atoms.as_ref());
    print_phase_row("work atoms", r.work_atoms.as_ref());
    print_phase_row("event atoms", r.event_atoms.as_ref());
    print_phase_row("state atoms", r.state_atoms.as_ref());
    print_phase_row("relation atoms", r.relation_atoms.as_ref());
    print_phase_row("question atoms", r.question_atoms.as_ref());
    print_phase_row("claim atoms", r.claim_atoms.as_ref());
    print_phase_row("mechanism atoms (typed)", r.mechanism_atoms.as_ref());
    print_phase_row("named-position atoms", r.named_position_atoms.as_ref());
    print_phase_row("evidence atoms (typed)", r.evidence_atoms.as_ref());
    print_phase_row("opposition atoms", r.opposition_atoms.as_ref());
    print_phase_row("concession atoms", r.concession_atoms.as_ref());
    print_phase_row("edges (Phase 3b)", r.edges.as_ref());
    print_phase_row("fault lines (Phase 6)", r.fault_lines.as_ref());
    print_phase_row("open questions (P7)", r.open_questions.as_ref());
    print_phase_row("configurations (P8)", r.configurations.as_ref());

    if let Some(d) = &r.discourse_act_distribution {
        println!();
        println!("  Discourse-act distribution ({} claims)", d.total_claims);
        for (act, count) in &d.act_counts {
            println!("    {act:<14}  {count}");
        }
        if !d.required_satisfied {
            println!("    ⚠ no claim carries any of the required acts");
        }
        if let Some(act) = &d.uniform_violation {
            println!(
                "    ⚠ all claims tagged as {act:?} — classifier may have collapsed onto one act"
            );
        }
    }

    // Aggregate F1: average of phase F1s where defined.
    let phase_f1s: Vec<f32> = [
        r.positions.as_ref().and_then(|s| s.f1()),
        r.person_atoms.as_ref().and_then(|s| s.f1()),
        r.concept_atoms.as_ref().and_then(|s| s.f1()),
        r.work_atoms.as_ref().and_then(|s| s.f1()),
        r.event_atoms.as_ref().and_then(|s| s.f1()),
        r.state_atoms.as_ref().and_then(|s| s.f1()),
        r.relation_atoms.as_ref().and_then(|s| s.f1()),
        r.question_atoms.as_ref().and_then(|s| s.f1()),
        r.claim_atoms.as_ref().and_then(|s| s.f1()),
        r.fault_lines.as_ref().and_then(|s| s.f1()),
        r.open_questions.as_ref().and_then(|s| s.f1()),
        r.configurations.as_ref().and_then(|s| s.f1()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !phase_f1s.is_empty() {
        let avg = phase_f1s.iter().sum::<f32>() / phase_f1s.len() as f32;
        println!();
        println!(
            "  Aggregate F1 (mean of {} scored phases): {:>5.1}%",
            phase_f1s.len(),
            avg * 100.0
        );
    }
}

fn write_json_report(path: &Path, report: &EvalReport) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::edges::{EdgeId, EdgeProvenance};

    #[test]
    fn unmatched_rate_is_none_on_empty_pool_and_fraction_otherwise() {
        let empty = PhaseScore::default();
        assert_eq!(empty.unmatched_rate(), None);

        let s = PhaseScore {
            candidates: 8,
            unmatched_count: 2,
            ..PhaseScore::default()
        };
        assert_eq!(s.unmatched_rate(), Some(0.25));
    }

    #[test]
    fn tally_unmatched_counts_and_caps_samples() {
        let mut s = PhaseScore::default();
        let candidates: Vec<String> = (0..30).map(|i| format!("atom-{i}")).collect();
        // "explained" = even indices; 15 odd candidates go unmatched.
        tally_unmatched(
            &mut s,
            &candidates,
            |c| c.clone(),
            |c| {
                let n: usize = c.trim_start_matches("atom-").parse().unwrap();
                n % 2 == 0
            },
        );
        assert_eq!(s.candidates, 30);
        assert_eq!(s.unmatched_count, 15);
        assert_eq!(s.unmatched_samples.len(), UNMATCHED_SAMPLE_CAP);
        assert_eq!(s.unmatched_samples[0], "atom-1");
    }

    #[test]
    fn tally_unmatched_serde_roundtrip_defaults_for_old_baselines() {
        // Pre-P0.2 baseline JSON has none of the volume fields — it
        // must still deserialize, reading as "no volume signal".
        let old = r#"{"expected":3,"matched":2,"forbidden_total":1,"forbidden_hit":0,
                      "misses":["x"],"forbidden_hits":[],"notes":[]}"#;
        let s: PhaseScore = serde_json::from_str(old).unwrap();
        assert_eq!(s.candidates, 0);
        assert_eq!(s.unmatched_count, 0);
        assert_eq!(s.unmatched_rate(), None);
    }

    #[test]
    fn truncate_label_is_char_boundary_safe() {
        let short = "plain label";
        assert_eq!(truncate_label(short), short);
        let long: String = "é".repeat(200);
        let t = truncate_label(&long);
        assert!(t.chars().count() <= 81); // 80 + ellipsis
        assert!(t.ends_with('…'));
    }

    #[test]
    fn matches_any_is_case_insensitive_substring() {
        let needles = vec!["compatibilism".to_string(), "hard det".to_string()];
        assert!(matches_any("Compatibilism", &needles));
        assert!(matches_any("HARD DETERMINISM", &needles));
        assert!(!matches_any("libertarianism", &needles));
    }

    #[test]
    fn matches_any_with_empty_needles_is_trivially_true() {
        assert!(matches_any("anything", &[]));
    }

    #[test]
    fn matches_any_token_presence_handles_paren_reorder() {
        // Golden phrase "hard incompatibilism" must match corpus
        // canonical "incompatibilism (hard)" — substring fails
        // (parens reorder), but token-presence catches both.
        let needles = vec!["hard incompatibilism".to_string()];
        assert!(matches_any("incompatibilism (hard)", &needles));
        // Disjoint tokens still don't match.
        assert!(!matches_any("compatibilism alone", &needles));
    }

    #[test]
    fn morphology_bridges_proper_noun_to_school_adjective() {
        // The headline case the morphology rule was added for.
        let needles = vec!["aristotelian".to_string()];
        assert!(matches_any_with_morphology("Aristotle", &needles));
    }

    #[test]
    fn morphology_bridges_ism_needle_to_underlying_stem() {
        // golden writes "situationism", corpus has "situational variables".
        let needles = vec!["situationism".to_string()];
        assert!(matches_any_with_morphology(
            "situational variables",
            &needles
        ));
        // -ist variant shares the same stem.
        let needles = vec!["situationist".to_string()];
        assert!(matches_any_with_morphology(
            "situational variables",
            &needles
        ));
    }

    #[test]
    fn morphology_holds_short_prefix_below_threshold() {
        // `polis` and `police` share 4 chars — far below 7-char
        // threshold. Must not match.
        let needles = vec!["polis".to_string()];
        assert!(!matches_any_with_morphology("police state", &needles));
        // `aristotle` and `aristocracy` share 6 chars — still below
        // 7. Must not match.
        let needles = vec!["aristotelian".to_string()];
        assert!(!matches_any_with_morphology("aristocracy", &needles));
    }

    #[test]
    fn morphology_inherits_substring_match() {
        // Substring already wins; morphology layer doesn't break it.
        let needles = vec!["compatibilism".to_string()];
        assert!(matches_any_with_morphology("compatibilism", &needles));
        assert!(matches_any_with_morphology("Compatibilism", &needles));
    }

    #[test]
    fn morphology_skips_multi_token_needles() {
        // Multi-token needles route through token-presence; morphology
        // path doesn't try to prefix-match across spaces.
        let needles = vec!["hard incompatibilism".to_string()];
        assert!(matches_any_with_morphology(
            "incompatibilism (hard)",
            &needles
        ));
        // But a multi-token needle that isn't substring-matchable and
        // doesn't have all tokens present must not slip through via
        // morphology of one token.
        let needles = vec!["hard incompatibilism".to_string()];
        assert!(!matches_any_with_morphology("hard libertarian", &needles));
    }

    #[test]
    fn matches_any_token_presence_requires_multitoken_needle() {
        // Single-token needles MUST not benefit from the fallback —
        // it would over-match (e.g. needle "polis" matching haystack
        // "polished" because the only token "polis" is searched as
        // substring, not as a free-standing token).
        let needles = vec!["polis".to_string()];
        assert!(matches_any("city polis", &needles));
        // Substring still wins on partial words (existing behavior).
        assert!(matches_any("polished mirror", &needles));
    }

    #[test]
    fn phase_filter_parsing_accepts_aliases() {
        assert_eq!(PhaseFilter::parse("all").unwrap(), PhaseFilter::All);
        assert_eq!(
            PhaseFilter::parse("skeleton").unwrap(),
            PhaseFilter::Positions
        );
        assert_eq!(
            PhaseFilter::parse("fault_lines").unwrap(),
            PhaseFilter::FaultLines
        );
        assert_eq!(
            PhaseFilter::parse("config").unwrap(),
            PhaseFilter::Configurations
        );
        assert!(PhaseFilter::parse("bogus").is_err());
    }

    #[test]
    fn parse_args_requires_corpus_and_golden() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
        let err = parse_args(&["fwd".into()]).unwrap_err();
        assert!(err.contains("golden-set-path"));
    }

    #[test]
    fn parse_args_minimal_form() {
        let args: Vec<String> = ["fwd", "/tmp/g.toml"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "fwd");
        assert_eq!(p.golden_path, PathBuf::from("/tmp/g.toml"));
        assert_eq!(p.phase, PhaseFilter::All);
        assert!(p.report_path.is_none());
    }

    #[test]
    fn parse_args_phase_and_report() {
        let args: Vec<String> = [
            "fwd",
            "/tmp/g.toml",
            "--phase",
            "fault-lines",
            "--report",
            "/tmp/r.json",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.phase, PhaseFilter::FaultLines);
        assert_eq!(p.report_path, Some(PathBuf::from("/tmp/r.json")));
    }

    #[test]
    fn phase_score_precision_recall_f1() {
        let s = PhaseScore {
            expected: 4,
            matched: 3,
            forbidden_total: 2,
            forbidden_hit: 1,
            ..Default::default()
        };
        // matched/(matched+forbidden_hit) = 3/4 = 0.75
        assert!((s.precision().unwrap() - 0.75).abs() < 1e-4);
        // matched/expected = 3/4 = 0.75
        assert!((s.recall().unwrap() - 0.75).abs() < 1e-4);
        // F1 = 0.75 (P == R)
        assert!((s.f1().unwrap() - 0.75).abs() < 1e-4);
    }

    #[test]
    fn phase_score_undefined_when_no_signal() {
        let s = PhaseScore::default();
        assert!(s.precision().is_none());
        assert!(s.recall().is_none());
        assert!(s.f1().is_none());
    }

    #[test]
    fn golden_set_parses_real_fixture() {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../bench/philosophy/free-will-debate.toml"
        ));
        let g = GoldenSet::load(path).expect("free-will-debate golden should parse");
        // v2 atlas goldens have dropped `expected_positions`
        // (legacy v1 artifact — concept-atom + claim-attribution
        // scoring covers the same ground). The load itself round-tripping
        // is the load-bearing assertion; fault-lines and forbidden edges
        // are populated regardless.
        assert!(!g.expected_fault_lines.is_empty());
        assert!(!g.forbidden_edges.is_empty());
    }

    #[test]
    fn golden_set_parses_all_three_fixtures() {
        for name in &[
            "free-will-debate",
            "virtue-ethics-fragments",
            "stoicism-mini",
        ] {
            let path = std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../bench/philosophy/"
            ))
            .join(format!("{name}.toml"));
            GoldenSet::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    // ── P0.5 edge-F1 ────────────────────────────────────────────────
    //
    // Before this axis landed, `expected_edges`/`forbidden_edges` were
    // `Vec<toml::Value>` behind `#[allow(dead_code)]`: the goldens
    // carried the data and the scorer never read it. These tests fail
    // against that state — the first because the fields had no typed
    // shape to assert on, the rest because `score_edges` didn't exist.

    fn edge(id: usize, ty: EdgeType, source: &str, target: &str) -> Edge {
        Edge {
            id: EdgeId::new(id),
            edge_type: ty,
            source: AtomId::from_raw(source),
            target: AtomId::from_raw(target),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 0.9,
            provenance: EdgeProvenance::LlmPairwise,
        }
    }

    /// Endpoints are raw ids that resolve to themselves (no atoms
    /// file), so these tests exercise the MATCHING rules; endpoint
    /// name resolution is covered through the fault-line path.
    fn snap_with_edges(edges: Vec<Edge>) -> AtlasSnapshot {
        AtlasSnapshot {
            skeleton: None,
            atoms: None,
            edges: Some(EdgesFile::new(edges)),
            gaps: None,
            configurations: None,
        }
    }

    fn golden_with_edges(expected: Vec<ExpectedEdge>, forbidden: Vec<ForbiddenEdge>) -> GoldenSet {
        let mut g: GoldenSet = toml::from_str("").expect("empty golden is valid");
        g.expected_edges = expected;
        g.forbidden_edges = forbidden;
        g
    }

    fn expect_edge(ty: &str, source: &str, target: &str) -> ExpectedEdge {
        ExpectedEdge {
            edge_type: ty.to_string(),
            source_contains_any: vec![source.to_string()],
            target_contains_any: vec![target.to_string()],
            note: None,
        }
    }

    #[test]
    fn committed_golden_carries_typed_edges() {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../bench/philosophy/free-will-debate.toml"
        ));
        let g = GoldenSet::load(path).expect("free-will-debate golden should parse");
        assert_eq!(g.expected_edges.len(), 2, "goldens already author the data");
        let grounds = g
            .expected_edges
            .iter()
            .find(|e| e.edge_type == "Grounds")
            .expect("the Grounds edge is authored");
        assert_eq!(grounds.source_contains_any, vec!["frankfurt case"]);
        assert_eq!(grounds.target_contains_any, vec!["compatibilism"]);
        // The anti-test carries a wildcard type and an intent tag the
        // edge model cannot express — see `relation_kind`.
        let fb = &g.forbidden_edges[0];
        assert_eq!(fb.edge_type, "*");
        assert_eq!(fb.relation_kind.as_deref(), Some("proponent_of"));
    }

    #[test]
    fn edge_scoring_is_directed_unlike_fault_lines() {
        // Fault lines match a position PAIR either way round. An edge
        // asserts an arrow: `Grounds(a → b)` is a different claim from
        // `Grounds(b → a)`, and scoring must not credit the reverse.
        let snap = snap_with_edges(vec![edge(
            1,
            EdgeType::Grounds,
            "compatibilism",
            "frankfurt",
        )]);
        let g = golden_with_edges(
            vec![expect_edge("Grounds", "frankfurt", "compatibilism")],
            vec![],
        );
        let s = score_edges(&g, &snap);
        assert_eq!(s.expected, 1);
        assert_eq!(s.matched, 0, "the reverse arrow must not count as a hit");
        assert_eq!(s.misses, vec!["Grounds(frankfurt → compatibilism)"]);
        // The reverse edge is real output that no golden entry
        // explains — it belongs in the unmatched tally, not nowhere.
        assert_eq!(s.candidates, 1);
        assert_eq!(s.unmatched_count, 1);
    }

    #[test]
    fn edge_scoring_respects_edge_type_and_wildcard() {
        let snap = snap_with_edges(vec![edge(1, EdgeType::Causes, "a", "b")]);

        let typed = score_edges(
            &golden_with_edges(vec![expect_edge("Grounds", "a", "b")], vec![]),
            &snap,
        );
        assert_eq!(typed.matched, 0, "endpoints match but the type does not");

        let wild = score_edges(
            &golden_with_edges(vec![expect_edge("*", "a", "b")], vec![]),
            &snap,
        );
        assert_eq!(wild.matched, 1, "`*` matches any edge type");
    }

    #[test]
    fn forbidden_edge_that_exists_is_a_hit() {
        let snap = snap_with_edges(vec![edge(
            1,
            EdgeType::Grounds,
            "frankfurt",
            "hard incompatibilism",
        )]);
        let g = golden_with_edges(
            vec![],
            vec![ForbiddenEdge {
                edge_type: "*".to_string(),
                source_contains_any: vec!["frankfurt".to_string()],
                target_contains_any: vec!["hard incompatibilism".to_string()],
                relation_kind: None,
                reason: None,
            }],
        );
        let s = score_edges(&g, &snap);
        assert_eq!(s.forbidden_total, 1);
        assert_eq!(s.forbidden_hit, 1);
        assert_eq!(
            s.forbidden_hits,
            vec!["*(frankfurt → hard incompatibilism)"]
        );
    }

    #[test]
    fn unknown_edge_type_is_reported_not_charged_to_the_model() {
        // A golden naming an edge type that doesn't exist is an
        // authoring bug. Scoring it as a silent recall miss blames the
        // extractor for the golden's typo (ARCH_PRINCIPLES §18.3).
        let snap = snap_with_edges(vec![edge(1, EdgeType::Grounds, "a", "b")]);
        let g = golden_with_edges(vec![expect_edge("Groundz", "a", "b")], vec![]);
        let s = score_edges(&g, &snap);
        assert_eq!(s.matched, 1, "falls back to any-type rather than missing");
        assert!(
            s.notes
                .iter()
                .any(|n| n.contains("Groundz") && n.contains("not model misses")),
            "the golden's bad type must be named in the notes, got {:?}",
            s.notes
        );
    }

    #[test]
    fn unevaluable_relation_kind_is_declared_not_assumed() {
        // The golden constrains `relation_kind`; the edge model has no
        // such field. Matching on the remaining criteria and reporting
        // a clean verdict would assert a check that never ran.
        let snap = snap_with_edges(vec![edge(
            1,
            EdgeType::Grounds,
            "frankfurt",
            "hard incompatibilism",
        )]);
        let g = golden_with_edges(
            vec![],
            vec![ForbiddenEdge {
                edge_type: "*".to_string(),
                source_contains_any: vec!["frankfurt".to_string()],
                target_contains_any: vec!["hard incompatibilism".to_string()],
                relation_kind: Some("proponent_of".to_string()),
                reason: None,
            }],
        );
        let s = score_edges(&g, &snap);
        assert_eq!(s.forbidden_hit, 1);
        assert!(
            s.notes
                .iter()
                .any(|n| n.contains("relation_kind") && n.contains("NOT checked")),
            "the unevaluated constraint must be declared, got {:?}",
            s.notes
        );
    }

    #[test]
    fn absent_edges_file_is_skipped_not_scored_zero() {
        let snap = AtlasSnapshot {
            skeleton: None,
            atoms: None,
            edges: None,
            gaps: None,
            configurations: None,
        };
        let g = golden_with_edges(vec![expect_edge("Grounds", "a", "b")], vec![]);
        let s = score_edges(&g, &snap);
        assert_eq!(s.matched, 0);
        assert_eq!(s.candidates, 0);
        assert!(s.notes.iter().any(|n| n.contains("edges.json not present")));
    }

    #[test]
    fn golden_without_edges_axis_is_not_scored_at_all() {
        // Absence of the axis means "no signal here", not "expected
        // zero" — a golden that omits edges must not read as 0% recall.
        let snap = snap_with_edges(vec![edge(1, EdgeType::Grounds, "a", "b")]);
        let g = golden_with_edges(vec![], vec![]);
        let report = score(&g, &snap, PhaseFilter::All);
        assert!(report.edges.is_none());
    }

    #[test]
    fn edges_phase_filter_parses() {
        assert_eq!(PhaseFilter::parse("edges").unwrap(), PhaseFilter::Edges);
    }
}
