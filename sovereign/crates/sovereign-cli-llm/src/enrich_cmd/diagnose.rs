//! `sovereign enrich diagnose <corpus-id>` — read-only atlas
//! inspection.
//!
//! Where `enrich show` reads the legacy phase-cache outputs, this
//! subcommand reads the **resolved atlas** (atoms.json, edges.json,
//! gaps.json, configurations.json, tension_candidates.json) and
//! field_skeleton.json. It is the glassbox companion to `enrich
//! eval`: when an eval F1 number drops, `diagnose` is what tells the
//! operator *why* — what was extracted, in what shape, with what
//! evidence.
//!
//! Output is plain text (Unicode bullet/check symbols), one section
//! per phase. With `--phase <id>`, only that phase's section prints.

use std::path::Path;

use corpus_engine::enrichment::atlas::analysis::configuration::ConfigurationsOutput;
use corpus_engine::enrichment::atlas::analysis::gaps::{Gap, GapKind, GapsOutput};
use corpus_engine::enrichment::atlas::analysis::tensions::TensionCandidatesOutput;
use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile, ResolutionStatus};
use corpus_engine::enrichment::atlas::edges::{EdgeType, EdgesFile};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use corpus_engine::enrichment::skeleton::FieldSkeleton;
use serde::Deserialize;

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich diagnose",
    summary: "Read-only inspection of the resolved philosophy atlas. Reports per-phase atom/edge counts, position lists, fault lines, gaps, and configurations.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich diagnose <corpus-id> [--phase positions|atoms|fault-lines|gaps|configurations|all] [--limit <n>]",
        ),
        HelpSection::Flags(&[
            (
                "--phase <id>",
                "Restrict inspection to one phase. Default: all.",
            ),
            (
                "--limit <n>",
                "Cap the number of items printed per list (positions, atoms, edges). Default: 20.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich diagnose fwd",
                "Full atlas summary for the corpus.",
            ),
            (
                "sovereign enrich diagnose fwd --phase fault-lines --limit 50",
                "List up to 50 detected Tension edges with their crux text.",
            ),
        ]),
        HelpSection::Notes(
            "Reads only — no LLM calls, no file writes. Pair with `enrich eval` to score the same artefacts against a golden set.",
        ),
    ],
};

pub async fn cmd_diagnose(args: &[String]) -> i32 {
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

    if let Err(e) = EnrichConfig::require(&parsed.corpus_id) {
        eprintln!("error: {e}");
        return 1;
    }

    let atlas_dir = paths::index_root(&parsed.corpus_id).join(ATLAS_DIRNAME);
    let skeleton_path = paths::index_root(&parsed.corpus_id).join("field_skeleton.json");

    let snap = match Snapshot::load(&atlas_dir, &skeleton_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    println!();
    println!("  Corpus: {}", parsed.corpus_id);
    println!("  Atlas dir: {}", atlas_dir.display());
    println!();

    if parsed.phase.includes(Phase::Positions) {
        print_positions(&snap, parsed.limit);
    }
    if parsed.phase.includes(Phase::Atoms) {
        print_atoms(&snap, parsed.limit);
    }
    if parsed.phase.includes(Phase::FaultLines) {
        print_fault_lines(&snap, parsed.limit);
    }
    if parsed.phase.includes(Phase::Gaps) {
        print_gaps(&snap, parsed.limit);
    }
    if parsed.phase.includes(Phase::Configurations) {
        print_configurations(&snap, parsed.limit);
    }

    0
}

// ── Argument parsing ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    All,
    Positions,
    Atoms,
    FaultLines,
    Gaps,
    Configurations,
}

impl Phase {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "all" => Ok(Self::All),
            "positions" | "skeleton" => Ok(Self::Positions),
            "atoms" => Ok(Self::Atoms),
            "fault-lines" | "fault_lines" | "tensions" => Ok(Self::FaultLines),
            "gaps" | "open-questions" | "open_questions" => Ok(Self::Gaps),
            "configurations" | "config" => Ok(Self::Configurations),
            other => Err(format!("unknown --phase: {other:?}")),
        }
    }

    fn includes(self, other: Phase) -> bool {
        self == Self::All || self == other
    }
}

#[derive(Debug)]
struct ParsedDiagnose {
    corpus_id: String,
    phase: Phase,
    limit: usize,
}

fn parse_args(args: &[String]) -> Result<ParsedDiagnose, String> {
    let mut corpus_id: Option<String> = None;
    let mut phase = Phase::All;
    let mut limit: usize = 20;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--phase" => {
                phase = Phase::parse(
                    args.get(i + 1)
                        .ok_or("--phase requires a value".to_string())?,
                )?;
                i += 2;
            }
            "--limit" => {
                let raw = args
                    .get(i + 1)
                    .ok_or("--limit requires a value".to_string())?;
                limit = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--limit must be a non-negative integer: {e}"))?;
                i += 2;
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
    Ok(ParsedDiagnose {
        corpus_id: corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?,
        phase,
        limit,
    })
}

// ── Snapshot ───────────────────────────────────────────────────────

struct Snapshot {
    skeleton: Option<FieldSkeleton>,
    atoms: Option<AtomsFile>,
    edges: Option<EdgesFile>,
    gaps: Option<GapsOutput>,
    configurations: Option<ConfigurationsOutput>,
    tension_candidates: Option<TensionCandidatesOutput>,
}

impl Snapshot {
    fn load(atlas_dir: &Path, skeleton_path: &Path) -> Result<Self, String> {
        let skeleton = if skeleton_path.exists() {
            Some(read_json::<FieldSkeleton>(skeleton_path)?)
        } else {
            None
        };
        let atoms = read_optional(atlas_dir, "atoms.json")?;
        let edges = read_optional(atlas_dir, "edges.json")?;
        let gaps = read_optional(atlas_dir, "gaps.json")?;
        let configurations = read_optional(atlas_dir, "configurations.json")?;
        let tension_candidates = read_optional(atlas_dir, "tension_candidates.json")?;
        Ok(Self {
            skeleton,
            atoms,
            edges,
            gaps,
            configurations,
            tension_candidates,
        })
    }

    fn entity_name_by_id(&self, id: &AtomId) -> Option<String> {
        let file = self.atoms.as_ref()?;
        file.atoms.iter().find_map(|a| match a {
            AtomEnvelope::Entity(e) if e.id == *id => Some(e.canonical_name.clone()),
            _ => None,
        })
    }
}

fn read_optional<T: for<'de> Deserialize<'de>>(
    dir: &Path,
    name: &str,
) -> Result<Option<T>, String> {
    let path = dir.join(name);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_json::<T>(&path)?))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

// ── Per-phase printers ─────────────────────────────────────────────

fn print_positions(snap: &Snapshot, limit: usize) {
    println!("  Positions (Phase 1)");
    println!("  ─────────────────────────────────────────────────────────────");
    let Some(sk) = &snap.skeleton else {
        println!("  · field_skeleton.json not present — Phase 1 may not have run.");
        println!();
        return;
    };
    let total: usize = sk
        .canonical_questions
        .iter()
        .map(|q| q.positions.len())
        .sum();
    println!(
        "  Domain: {}    Questions: {}    Positions: {}",
        sk.domain_id,
        sk.canonical_questions.len(),
        total
    );
    println!();
    let mut printed = 0;
    'outer: for q in &sk.canonical_questions {
        println!("  Q: {}  [{}]", trim_to(&q.question, 80), q.status);
        for p in &q.positions {
            let proponents = if p.proponents.is_empty() {
                "—".to_string()
            } else {
                p.proponents.join(", ")
            };
            println!(
                "    · {name}  [{status}]  proponents: {props}",
                name = p.name,
                status = p.status,
                props = trim_to(&proponents, 60),
            );
            printed += 1;
            if printed >= limit {
                println!("    … (limit {limit} reached)");
                break 'outer;
            }
        }
    }
    println!();
}

fn print_atoms(snap: &Snapshot, limit: usize) {
    println!("  Atoms (Phase 3a/3b)");
    println!("  ─────────────────────────────────────────────────────────────");
    let Some(atoms_file) = &snap.atoms else {
        println!("  · atoms.json not present.");
        println!();
        return;
    };

    use std::collections::BTreeMap;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut entity_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for a in &atoms_file.atoms {
        let key = match a {
            AtomEnvelope::Entity(e) => {
                *entity_kinds
                    .entry(e.entity_type.as_str_repr().to_string())
                    .or_insert(0) += 1;
                "Entity"
            }
            AtomEnvelope::Event(_) => "Event",
            AtomEnvelope::State(_) => "State",
            AtomEnvelope::Relation(_) => "Relation",
            AtomEnvelope::Claim(_) => "Claim",
            AtomEnvelope::Question(_) => "Question",
            AtomEnvelope::Configuration(_) => "Configuration",
            AtomEnvelope::ArgumentReconstruction(_) => "ArgumentReconstruction",
            AtomEnvelope::Position(_) | AtomEnvelope::Opposition(_) => {
                unreachable!("typed atoms wired in Gap B Stage 4")
            }
            AtomEnvelope::Asset(_) => "Asset",
        };
        *counts.entry(key).or_insert(0) += 1;
    }
    println!("  Total atoms: {}", atoms_file.atoms.len());
    for (k, v) in &counts {
        println!("    {k:<15} {v}");
    }
    if !entity_kinds.is_empty() {
        println!();
        println!("  Entity kinds:");
        for (k, v) in &entity_kinds {
            println!("    {k:<15} {v}");
        }
    }

    // Sample person atoms.
    println!();
    println!("  Persons (sample, salience-ordered):");
    let mut persons: Vec<_> = atoms_file
        .atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Entity(e) if e.entity_type == EntityType::Person => Some(e),
            _ => None,
        })
        .collect();
    persons.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for p in persons.iter().take(limit) {
        println!(
            "    · {name}  (salience {sal:.2})  {desc}",
            name = p.canonical_name,
            sal = p.salience,
            desc = trim_to(&p.description, 60),
        );
    }
    if persons.len() > limit {
        println!("    … (+{} more)", persons.len() - limit);
    }

    // Sample concept atoms.
    let concepts: Vec<_> = atoms_file
        .atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Entity(e) if e.entity_type == EntityType::Concept => Some(e),
            _ => None,
        })
        .collect();
    if !concepts.is_empty() {
        println!();
        println!("  Concepts (sample):");
        for c in concepts.iter().take(limit) {
            println!(
                "    · {name}  {desc}",
                name = c.canonical_name,
                desc = trim_to(&c.description, 70),
            );
        }
        if concepts.len() > limit {
            println!("    … (+{} more)", concepts.len() - limit);
        }
    }

    // Sample question atoms with resolution status.
    let questions: Vec<_> = atoms_file
        .atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Question(q) => Some(q),
            _ => None,
        })
        .collect();
    if !questions.is_empty() {
        println!();
        println!("  Questions:");
        for q in questions.iter().take(limit) {
            let status = match q.resolution_status {
                ResolutionStatus::Resolved { .. } => "resolved",
                ResolutionStatus::Contested { .. } => "contested",
                ResolutionStatus::Open => "open",
                ResolutionStatus::Dissolved => "dissolved",
            };
            println!(
                "    · [{status}]  {content}",
                content = trim_to(&q.content, 90)
            );
        }
        if questions.len() > limit {
            println!("    … (+{} more)", questions.len() - limit);
        }
    }

    // Sample claims with discourse_act.
    let claims: Vec<_> = atoms_file
        .atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Claim(c) => Some(c),
            _ => None,
        })
        .collect();
    if !claims.is_empty() {
        println!();
        println!("  Claims (with discourse act + epistemic status):");
        for c in claims.iter().take(limit) {
            println!(
                "    · [{act}/{eps}]  {content}",
                act = c.discourse_act.as_str_repr(),
                eps = c.epistemic_status.as_str_repr(),
                content = trim_to(&c.content, 80),
            );
        }
        if claims.len() > limit {
            println!("    … (+{} more)", claims.len() - limit);
        }
    }
    println!();
}

fn print_fault_lines(snap: &Snapshot, limit: usize) {
    println!("  Fault lines (Phase 6)");
    println!("  ─────────────────────────────────────────────────────────────");

    if let Some(cands) = &snap.tension_candidates {
        println!(
            "  Tension candidates (deterministic pre-LLM): {}",
            cands.candidates.len()
        );
    }

    let Some(edges_file) = &snap.edges else {
        println!("  · edges.json not present — Phase 6 has not produced Tension edges.");
        println!();
        return;
    };
    let tension_edges: Vec<_> = edges_file
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Tension)
        .collect();
    println!("  Tension edges (LLM-classified): {}", tension_edges.len());
    if tension_edges.is_empty() {
        println!("  · No Tension edges in edges.json.");
        println!();
        return;
    }
    println!();
    for e in tension_edges.iter().take(limit) {
        let a = snap
            .entity_name_by_id(&e.source)
            .unwrap_or_else(|| e.source.as_str().to_string());
        let b = snap
            .entity_name_by_id(&e.target)
            .unwrap_or_else(|| e.target.as_str().to_string());
        let crux = e.sub_question.as_deref().unwrap_or("(no crux)");
        println!("    · {a} ⟷ {b}    conf {conf:.2}", conf = e.confidence);
        println!("        crux: {}", trim_to(crux, 100));
    }
    if tension_edges.len() > limit {
        println!("    … (+{} more)", tension_edges.len() - limit);
    }
    println!();
}

fn print_gaps(snap: &Snapshot, limit: usize) {
    println!("  Gaps + open questions (Phase 7)");
    println!("  ─────────────────────────────────────────────────────────────");
    let Some(gaps_file) = &snap.gaps else {
        println!("  · gaps.json not present.");
        println!();
        return;
    };

    use std::collections::BTreeMap;
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for g in &gaps_file.gaps {
        let key = match g.kind {
            GapKind::TransitionWithoutTrigger => "transition_without_trigger",
            GapKind::UngroundedClaim => "ungrounded_claim",
            GapKind::OpenQuestion => "open_question",
        };
        *by_kind.entry(key).or_insert(0) += 1;
    }
    println!("  Total gaps: {}", gaps_file.gaps.len());
    for (k, v) in &by_kind {
        println!("    {k:<32} {v}");
    }
    println!();

    let printable: Vec<&Gap> = gaps_file.gaps.iter().take(limit).collect();
    for g in printable {
        let kind_label = match g.kind {
            GapKind::TransitionWithoutTrigger => "trans-no-trigger",
            GapKind::UngroundedClaim => "ungrounded-claim",
            GapKind::OpenQuestion => "open-question",
        };
        println!(
            "    · [{kind_label}] (sig {sig:.2}) {desc}",
            sig = g.significance,
            desc = trim_to(&g.description, 100)
        );
    }
    if gaps_file.gaps.len() > limit {
        println!("    … (+{} more)", gaps_file.gaps.len() - limit);
    }
    println!();
}

fn print_configurations(snap: &Snapshot, limit: usize) {
    println!("  Configurations (Phase 8)");
    println!("  ─────────────────────────────────────────────────────────────");

    let mut all = Vec::new();
    if let Some(file) = &snap.atoms {
        for a in &file.atoms {
            if let AtomEnvelope::Configuration(c) = a {
                all.push(c.clone());
            }
        }
    }
    if let Some(out) = &snap.configurations {
        for c in &out.configurations {
            // Avoid double-counting configurations that appear in both
            // atoms.json and configurations.json.
            if !all.iter().any(|x| x.id == c.id) {
                all.push(c.clone());
            }
        }
    }

    if all.is_empty() {
        println!("  · No configuration atoms found (atoms.json/configurations.json).");
        println!();
        return;
    }
    println!("  Total configurations: {}", all.len());
    println!();
    for c in all.iter().take(limit) {
        println!(
            "    · {label}  (conf {conf:.2}, {n} constituents)",
            label = trim_to(&c.label, 80),
            conf = c.confidence,
            n = c.constituent_atoms.len()
        );
        if !c.description.is_empty() {
            println!("        {}", trim_to(&c.description, 100));
        }
        if !c.interpretive_note.is_empty() {
            println!("        note: {}", trim_to(&c.interpretive_note, 100));
        }
    }
    if all.len() > limit {
        println!("    … (+{} more)", all.len() - limit);
    }
    println!();
}

fn trim_to(s: &str, max: usize) -> String {
    let one_line: String = s.lines().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let head: String = one_line.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_minimal_form() {
        let p = parse_args(&["fwd".into()]).unwrap();
        assert_eq!(p.corpus_id, "fwd");
        assert_eq!(p.phase, Phase::All);
        assert_eq!(p.limit, 20);
    }

    #[test]
    fn parse_args_phase_and_limit() {
        let args: Vec<String> = ["fwd", "--phase", "fault-lines", "--limit", "50"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.phase, Phase::FaultLines);
        assert_eq!(p.limit, 50);
    }

    #[test]
    fn parse_args_requires_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
    }

    #[test]
    fn parse_args_rejects_unknown_phase() {
        let err = parse_args(&["fwd".into(), "--phase".into(), "junk".into()]).unwrap_err();
        assert!(err.contains("unknown --phase"));
    }

    #[test]
    fn trim_to_truncates_with_ellipsis() {
        let s = "a".repeat(100);
        let t = trim_to(&s, 20);
        assert_eq!(t.chars().count(), 20);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn trim_to_collapses_newlines() {
        assert_eq!(trim_to("first\n\nsecond", 100), "first  second");
    }
}
