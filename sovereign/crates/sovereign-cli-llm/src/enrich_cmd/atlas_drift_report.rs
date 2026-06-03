//! `sovereign enrich atlas-drift-report` — narrative-vs-structural
//! drift detector.
//!
//! Compares N narrative atlases (markdown-derived: ARCH_PRINCIPLES,
//! SYSTEM_OVERVIEW, CHARTER, ADRs, …) against one structural atlas
//! (code-derived) and emits a severity-tiered markdown digest plus a
//! JSON sidecar of every finding.
//!
//! Generic by design: the command is not specialised to any
//! particular project. Severity rules key on atom shape (a
//! `Claim` with `epistemic_status = "normative"` is critical
//! regardless of which doc it lives in), not filename. This
//! ensures the same machinery works for any team's stable
//! narrative artifacts + any indexed codebase.
//!
//! ## Inputs (read-only)
//!
//! For each narrative atlas:
//!   - `~/.sovereign/indexes/<id>/atlas/atoms.json`
//!   - `~/.sovereign/indexes/<id>/atlas/cross_corpus_edges.json`
//!     (must have been produced by `atlas-cross-corpus <id> <structural>`)
//!
//! For the structural atlas:
//!   - `~/.sovereign/indexes/<id>/atlas/atoms.json`
//!   - `~/.sovereign/indexes/<id>/atlas/edges.json` (for in-degree salience)
//!
//! ## Outputs
//!
//! - `<output>` (default `./drift_report.md`) — markdown digest
//! - `<output>.json` — full structured findings sidecar

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
use serde::Serialize;
use serde_json::Value;

use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-drift-report",
    summary: "Generate a severity-ranked drift report comparing narrative atlases to a structural atlas.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich atlas-drift-report --narrative <id> [--narrative <id>...] \
             --structural <id> [--output <path>] [--max-findings <N>]",
        ),
        HelpSection::Flags(&[
            (
                "--narrative <id>",
                "Atlas id for a narrative source (markdown-derived). Repeat for multiple narrative streams.",
            ),
            (
                "--structural <id>",
                "Atlas id for the code-derived structural atlas to compare against. Required.",
            ),
            (
                "--output <path>",
                "Output path for the markdown report. Default: ./drift_report.md. The JSON sidecar lands at <output>.json.",
            ),
            (
                "--max-findings <N>",
                "Cap the rendered markdown report at the top N findings (sorted by severity then salience). Default: 50. The JSON sidecar always carries the full set.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich atlas-drift-report --narrative arch-principles-atlas --narrative system-overview-atlas --structural commonwealth-ai-self-atlas",
                "Compare two narrative streams against the unified monorepo structural atlas.",
            ),
        ]),
        HelpSection::Notes(
            "Each narrative atlas must already have a cross_corpus_edges.json produced by \
             `sovereign enrich atlas-cross-corpus <narrative-id> <structural-id>`. The drift \
             report consumes that file as the matching layer; it does not re-run name matching.",
        ),
    ],
};

pub async fn cmd_atlas_drift_report(args: &[String]) -> i32 {
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

    if parsed.narrative_ids.is_empty() {
        eprintln!("error: at least one --narrative <id> is required");
        return 2;
    }
    let Some(structural_id) = parsed.structural_id.as_deref() else {
        eprintln!("error: --structural <id> is required");
        return 2;
    };

    // ── Load structural ───────────────────────────────────
    let structural_dir = paths::index_root(structural_id).join("atlas");
    let structural_atoms = match read_atlas_atoms(&structural_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: reading structural atlas at {}: {e}",
                structural_dir.display()
            );
            return 1;
        }
    };
    let edges_path = structural_dir.join("edges.json");
    let structural_in_degree = compute_in_degree(&edges_path);
    let structural = StructuralIndex::build(&structural_atoms, &structural_in_degree);

    // ── Load each narrative ───────────────────────────────
    let mut narratives: Vec<NarrativeIndex> = Vec::new();
    for nid in &parsed.narrative_ids {
        let dir = paths::index_root(nid).join("atlas");
        let atoms = match read_atlas_atoms(&dir) {
            Ok(a) => a,
            Err(e) => {
                eprintln!(
                    "error: reading narrative atlas {} at {}: {e}",
                    nid,
                    dir.display()
                );
                return 1;
            }
        };
        let cross_path = dir.join("cross_corpus_edges.json");
        let cross = load_cross_corpus(&cross_path).unwrap_or_default();
        narratives.push(NarrativeIndex::build(nid.clone(), &atoms, &cross));
    }

    // ── Build findings ────────────────────────────────────
    let findings = compute_findings(&narratives, &structural);

    // ── Render outputs ────────────────────────────────────
    let output_md = parsed
        .output
        .unwrap_or_else(|| PathBuf::from("./drift_report.md"));
    let output_json = output_md.with_extension("md.json");
    let max_findings = parsed.max_findings.unwrap_or(50);

    // Optional rough-edges sidecar — adds an "Internal" section to
    // the digest and feeds the header counter when present.
    let rough_edges = parsed
        .rough_edges_json
        .as_deref()
        .and_then(load_rough_edges);

    // Optional git-archaeology sidecar — adds a "Provenance &
    // Evolution" section between "Confirmed" and "Investigation queue".
    let git_archaeology = parsed
        .git_archaeology_json
        .as_deref()
        .and_then(load_git_archaeology);

    let md = render_markdown(
        &findings,
        &narratives,
        structural_id,
        max_findings,
        rough_edges.as_ref(),
        git_archaeology.as_ref(),
    );
    if let Err(e) = fs::write(&output_md, md) {
        eprintln!("error: writing {}: {e}", output_md.display());
        return 1;
    }
    let json = serde_json::to_string_pretty(&findings).unwrap_or_default();
    if let Err(e) = fs::write(&output_json, json) {
        eprintln!("error: writing {}: {e}", output_json.display());
        return 1;
    }

    println!();
    println!("  ✓ wrote {} ({} findings rendered)", output_md.display(), findings.rendered_count(max_findings));
    println!("  ✓ wrote {} (full sidecar)", output_json.display());
    println!("  · {} dual-attested  ·  {} drift candidates  ·  {} notes",
        findings.dual_attested.len(),
        findings.critical.len() + findings.likely.len(),
        findings.notes.len(),
    );

    0
}

// ── Argument parsing ─────────────────────────────────────────

#[derive(Debug, Default)]
struct ParsedArgs {
    narrative_ids: Vec<String>,
    structural_id: Option<String>,
    output: Option<PathBuf>,
    max_findings: Option<usize>,
    /// Optional path to a `rough-edges` JSON sidecar. When present,
    /// the renderer adds an "Internal" section summarising marker +
    /// doc-drift findings alongside the narrative-vs-code drift.
    rough_edges_json: Option<PathBuf>,
    /// Optional path to a `git-archaeology` JSON sidecar. When present,
    /// the renderer adds a "Provenance & Evolution" section with
    /// stability highlights, recent volatility, co-evolution clusters,
    /// and a staleness queue.
    git_archaeology_json: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut out = ParsedArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--narrative" => {
                let v = args.get(i + 1).ok_or("--narrative requires a value")?;
                out.narrative_ids.push(v.clone());
                i += 2;
            }
            "--structural" => {
                let v = args.get(i + 1).ok_or("--structural requires a value")?;
                out.structural_id = Some(v.clone());
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("--output requires a value")?;
                out.output = Some(PathBuf::from(v));
                i += 2;
            }
            "--max-findings" => {
                let v = args.get(i + 1).ok_or("--max-findings requires a value")?;
                let n: usize = v
                    .parse()
                    .map_err(|e| format!("--max-findings must be an integer: {e}"))?;
                out.max_findings = Some(n);
                i += 2;
            }
            "--git-archaeology" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--git-archaeology requires a value")?;
                out.git_archaeology_json = Some(PathBuf::from(v));
                i += 2;
            }
            "--rough-edges" => {
                let v = args.get(i + 1).ok_or("--rough-edges requires a value")?;
                out.rough_edges_json = Some(PathBuf::from(v));
                i += 2;
            }
            other => return Err(format!("unknown flag or unexpected positional: {other}")),
        }
    }
    Ok(out)
}

// ── Index types ──────────────────────────────────────────────

#[derive(Debug, Clone)]
struct StructuralAtomView {
    id: String,
    canonical_name: String,
    aliases: Vec<String>,
    entity_type: String,
    description: String,
    salience: f32,
    in_degree: usize,
}

struct StructuralIndex {
    by_id: HashMap<String, StructuralAtomView>,
    by_name: BTreeMap<String, String>, // normalized name → atom id
    /// Top-quartile in-degree threshold for "high salience".
    high_salience_threshold: usize,
}

impl StructuralIndex {
    fn build(atoms: &corpus_engine::enrichment::atlas::AtomsFile, in_degree: &HashMap<String, usize>) -> Self {
        let mut by_id = HashMap::new();
        let mut by_name = BTreeMap::new();
        let mut degrees: Vec<usize> = Vec::new();
        for a in &atoms.atoms {
            if let AtomEnvelope::Entity(ent) = a {
                let id = ent.id.as_str().to_string();
                let deg = in_degree.get(&id).copied().unwrap_or(0);
                degrees.push(deg);
                let view = StructuralAtomView {
                    id: id.clone(),
                    canonical_name: ent.canonical_name.clone(),
                    aliases: ent.aliases.clone(),
                    entity_type: entity_type_string(&ent.entity_type),
                    description: ent.description.clone(),
                    salience: ent.salience,
                    in_degree: deg,
                };
                by_name.insert(normalise_name(&ent.canonical_name), id.clone());
                for alias in &ent.aliases {
                    by_name.entry(normalise_name(alias)).or_insert_with(|| id.clone());
                }
                by_id.insert(id, view);
            }
        }
        degrees.sort_unstable();
        let high_salience_threshold = degrees
            .get(degrees.len().saturating_sub(degrees.len() / 4).max(1) - 1)
            .copied()
            .unwrap_or(0)
            .max(2);
        Self {
            by_id,
            by_name,
            high_salience_threshold,
        }
    }

    fn lookup_fuzzy(&self, name: &str) -> Option<&StructuralAtomView> {
        let key = normalise_name(name);
        if key.is_empty() {
            return None;
        }
        // Exact normalised match.
        if let Some(id) = self.by_name.get(&key) {
            return self.by_id.get(id);
        }
        // Suffix match on the full name: `corpusengine` ↔
        // `corpus_engine__engine__corpusengine` (last-segment exact).
        let suffix_target = format!("::{}", key);
        for (k, id) in &self.by_name {
            if k.ends_with(&key) && (k.len() == key.len() || k.ends_with(&suffix_target)) {
                return self.by_id.get(id);
            }
        }
        // Last-segment prefix match: narrative `KnowledgeView` matches
        // structural last-segment `KnowledgeViewManager` /
        // `KnowledgeViewSection` — a fanout / refinement signal.
        for (k, id) in &self.by_name {
            let last = k.rsplit("::").next().unwrap_or(k);
            if last != k && last.starts_with(&key) {
                return self.by_id.get(id);
            }
            // Single-segment names (no `::`) — substring match.
            if last == k && k.contains(&key) && k != &key {
                return self.by_id.get(id);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
struct NarrativeAtomView {
    atlas_id: String,
    id: String,
    atom_type: String,
    canonical_name: String,
    aliases: Vec<String>,
    /// For Entity atoms; empty for Claim/Configuration.
    entity_type: String,
    /// For Claim atoms: discourse_act + epistemic_status.
    discourse_act: Option<String>,
    epistemic_status: Option<String>,
    /// `content` for Claims, `description`/`label` for others.
    description: String,
    /// `quotable_excerpt` if present (for Claims).
    quotable_excerpt: Option<String>,
    /// Origin chunk for source attribution.
    chunk_id: Option<String>,
    /// The original `Claim.anchor` value, if any — distinct from
    /// `canonical_name` which falls back to the prose first
    /// sentence when no anchor exists. Lets the renderer report
    /// "anchor exists but didn't match the structural atlas"
    /// (an actionable state: the operator can grep the anchor
    /// themselves) vs "no anchor" (model couldn't extract one).
    /// `None` for non-Claim atoms.
    anchor: Option<String>,
}

struct NarrativeIndex {
    atlas_id: String,
    atoms: Vec<NarrativeAtomView>,
    /// Set of narrative atom_ids that have a cross_corpus Grounding edge
    /// to a structural atom. Maps narrative_atom_id → structural_atom_id.
    matched: BTreeMap<String, String>,
}

impl NarrativeIndex {
    fn build(
        atlas_id: String,
        atoms: &corpus_engine::enrichment::atlas::AtomsFile,
        cross: &Vec<Value>,
    ) -> Self {
        let mut views: Vec<NarrativeAtomView> = Vec::new();
        for a in &atoms.atoms {
            views.push(narrative_view(&atlas_id, a));
        }

        // Index Grounding edges: source = narrative, target = structural.
        let mut matched: BTreeMap<String, String> = BTreeMap::new();
        for edge_obj in cross {
            // Each entry is { edge: { source, target, ... }, peer: { ... } }
            let Some(edge) = edge_obj.get("edge") else { continue };
            let Some(source) = edge.get("source").and_then(|v| v.as_str()) else { continue };
            let Some(target) = edge.get("target").and_then(|v| v.as_str()) else { continue };
            matched.insert(source.to_string(), target.to_string());
        }

        Self { atlas_id, atoms: views, matched }
    }
}

// ── Findings types ───────────────────────────────────────────

#[derive(Debug, Serialize, Default)]
struct FindingSet {
    critical: Vec<Finding>,
    likely: Vec<Finding>,
    notes: Vec<Finding>,
    dual_attested: Vec<DualAttested>,
}

impl FindingSet {
    fn rendered_count(&self, cap: usize) -> usize {
        let total = self.critical.len() + self.likely.len() + self.notes.len();
        total.min(cap)
    }
}

#[derive(Debug, Serialize, Clone)]
struct Finding {
    severity: Severity,
    kind: FindingKind,
    headline: String,
    /// Narrative atlas + atom (None when the finding is reality-only).
    narrative: Option<NarrativeRef>,
    /// Structural atom (None when the finding is narrative-only without
    /// any fuzzy match).
    structural: Option<StructuralRef>,
    action: String,
    /// For ranking inside a severity tier.
    rank_hint: f64,
}

#[derive(Debug, Serialize, Clone, Copy)]
enum Severity {
    Critical,
    Likely,
    Note,
}

#[derive(Debug, Serialize, Clone, Copy)]
enum FindingKind {
    NormativeClaimWithoutAnchor,
    ConfigurationWithoutMembers,
    EntityNarrativeOnly,
    EntityRealityOnly,
    PartialMatchCompression,
}

#[derive(Debug, Serialize, Clone)]
struct NarrativeRef {
    atlas_id: String,
    atom_id: String,
    atom_type: String,
    canonical_name: String,
    chunk_id: Option<String>,
    quotable: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct StructuralRef {
    atlas_id: String,
    atom_id: String,
    canonical_name: String,
    entity_type: String,
    in_degree: usize,
}

#[derive(Debug, Serialize, Clone)]
struct DualAttested {
    narrative_atlas_id: String,
    narrative_atom_id: String,
    structural_atom_id: String,
    canonical_name: String,
}

// ── Findings computation ─────────────────────────────────────

fn compute_findings(
    narratives: &[NarrativeIndex],
    structural: &StructuralIndex,
) -> FindingSet {
    let mut set = FindingSet::default();

    // Track which structural atoms got at least one narrative match.
    let mut covered_structural: BTreeSet<String> = BTreeSet::new();

    for narrative in narratives {
        for atom in &narrative.atoms {
            if let Some(struct_id) = narrative.matched.get(&atom.id) {
                covered_structural.insert(struct_id.clone());
                let canonical = atom.canonical_name.clone();
                set.dual_attested.push(DualAttested {
                    narrative_atlas_id: narrative.atlas_id.clone(),
                    narrative_atom_id: atom.id.clone(),
                    structural_atom_id: struct_id.clone(),
                    canonical_name: canonical,
                });
                continue;
            }

            // Skip narrative noise — entities the literary_atlas
            // pipeline extracts that aren't actually code components:
            //   - document path references (`sovereign/docs/X.md`)
            //   - abstract concepts in lowercase prose (`contract`,
            //     `mechanism`, `invariant`) — these are discussion
            //     terms, not named structural surface.
            // A normative claim atom carries the same content via
            // its quotable_excerpt, so we don't lose signal — we
            // just stop double-listing the prose noun as drift.
            if is_narrative_noise(&atom.atom_type, &atom.canonical_name) {
                continue;
            }

            // No exact match. Try fuzzy.
            let fuzzy = structural.lookup_fuzzy(&atom.canonical_name);
            classify_unmatched_narrative(atom, fuzzy, &mut set);
        }
    }

    // Reality-only: high-salience structural atoms that no narrative
    // mentioned. Filter to entity types that represent architectural
    // surface (trait, struct, module, crate).
    for view in structural.by_id.values() {
        if covered_structural.contains(&view.id) {
            continue;
        }
        if view.in_degree < structural.high_salience_threshold {
            continue;
        }
        if !matches!(
            view.entity_type.as_str(),
            "trait" | "struct" | "module" | "crate" | "enum"
        ) {
            continue;
        }
        let severity = if view.entity_type == "trait" || view.entity_type == "crate" {
            Severity::Likely
        } else {
            Severity::Note
        };
        let finding = Finding {
            severity,
            kind: FindingKind::EntityRealityOnly,
            headline: format!(
                "`{}` (high-salience {}) — no narrative coverage",
                view.canonical_name, view.entity_type
            ),
            narrative: None,
            structural: Some(StructuralRef {
                atlas_id: String::new(),
                atom_id: view.id.clone(),
                canonical_name: view.canonical_name.clone(),
                entity_type: view.entity_type.clone(),
                in_degree: view.in_degree,
            }),
            action: format!(
                "Document `{}` in the team's narrative artifacts, or confirm it's intentionally internal.",
                view.canonical_name
            ),
            rank_hint: view.in_degree as f64,
        };
        match severity {
            Severity::Critical => set.critical.push(finding),
            Severity::Likely => set.likely.push(finding),
            Severity::Note => set.notes.push(finding),
        }
    }

    // Sort within tiers by rank_hint descending.
    let by_rank = |a: &Finding, b: &Finding| {
        b.rank_hint
            .partial_cmp(&a.rank_hint)
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    set.critical.sort_by(by_rank);
    set.likely.sort_by(by_rank);
    set.notes.sort_by(by_rank);

    set
}

fn classify_unmatched_narrative(
    atom: &NarrativeAtomView,
    fuzzy: Option<&StructuralAtomView>,
    set: &mut FindingSet,
) {
    // Critical: a Claim with normative epistemic_status and no anchor.
    if atom.atom_type == "Claim" {
        let normative = atom
            .epistemic_status
            .as_deref()
            .map(is_normative_status)
            .unwrap_or(false)
            || atom_text_is_normative(&atom.description)
            || atom
                .quotable_excerpt
                .as_deref()
                .map(atom_text_is_normative)
                .unwrap_or(false);
        if normative && fuzzy.is_none() {
            // Two distinct cases, both rendered as critical but with
            // different actionability:
            //   (a) Claim never carried an anchor — the model
            //       extracted prose only. The operator has nothing to
            //       grep; the rule needs implementation OR revision.
            //   (b) Claim DID carry an anchor (`Claim.anchor`
            //       Some(...)), but the structural atlas didn't have
            //       a matching entity. This happens regularly when
            //       the structural atlas only indexes crate-level
            //       atoms while the anchor names a function. The
            //       operator can `grep <anchor>` directly — making
            //       the headline carry the anchor turns the finding
            //       from "I have nothing to act on" into "grep for X".
            let (headline, action) = match atom.anchor.as_deref() {
                Some(a) => (
                    format!(
                        "Normative claim — anchor `{}` not found in code atlas — `{}`",
                        a,
                        truncate_first_sentence(&atom.description, 120)
                    ),
                    format!(
                        "Search the codebase for `{a}`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement."
                    ),
                ),
                None => (
                    format!(
                        "Normative claim without code evidence — `{}`",
                        truncate_first_sentence(&atom.description, 120)
                    ),
                    "Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.".to_string(),
                ),
            };
            set.critical.push(Finding {
                severity: Severity::Critical,
                kind: FindingKind::NormativeClaimWithoutAnchor,
                headline,
                narrative: Some(narrative_ref(atom)),
                structural: None,
                action,
                rank_hint: 1000.0,
            });
            return;
        }
    }

    // Critical: Configuration whose constituent members aren't found.
    if atom.atom_type == "Configuration" && fuzzy.is_none() {
        set.critical.push(Finding {
            severity: Severity::Critical,
            kind: FindingKind::ConfigurationWithoutMembers,
            headline: format!(
                "Configuration not anchored — `{}`",
                truncate_first_sentence(&atom.description, 120)
            ),
            narrative: Some(narrative_ref(atom)),
            structural: None,
            action: "Verify the named structural components exist in the code, or revise the configuration if it's been refactored.".to_string(),
            rank_hint: 900.0,
        });
        return;
    }

    // Entity narrative-only.
    if atom.atom_type == "Entity" {
        if let Some(f) = fuzzy {
            // Partial: fuzzy match found, but no Grounding edge accepted.
            // Likely a paraphrase / compression / fanout.
            set.notes.push(Finding {
                severity: Severity::Note,
                kind: FindingKind::PartialMatchCompression,
                headline: format!(
                    "Naming compression — narrative `{}` ↔ structural `{}`",
                    atom.canonical_name, f.canonical_name
                ),
                narrative: Some(narrative_ref(atom)),
                structural: Some(StructuralRef {
                    atlas_id: String::new(),
                    atom_id: f.id.clone(),
                    canonical_name: f.canonical_name.clone(),
                    entity_type: f.entity_type.clone(),
                    in_degree: f.in_degree,
                }),
                action: format!(
                    "Align names: either expand the narrative to enumerate `{}`'s structural fanout, or rename for consistency.",
                    atom.canonical_name
                ),
                rank_hint: f.in_degree as f64,
            });
        } else {
            set.likely.push(Finding {
                severity: Severity::Likely,
                kind: FindingKind::EntityNarrativeOnly,
                headline: format!(
                    "Component named in narrative, absent from code — `{}`",
                    atom.canonical_name
                ),
                narrative: Some(narrative_ref(atom)),
                structural: None,
                action: format!(
                    "Confirm whether `{}` was renamed, removed, or is pending implementation.",
                    atom.canonical_name
                ),
                rank_hint: 100.0,
            });
        }
    }
}

// ── Markdown rendering ───────────────────────────────────────

/// Render a one-page-scannable digest. Three sections, brutally
/// trimmed:
///
///   1. **Act on** — Critical findings only, hand-rendered with the
///      verbatim narrative quote and a concrete next-step.
///   2. **Confirmed** — comma-separated paragraph of dual-attested
///      canonical names. Cap at 30 with "+N more".
///   3. **Investigation queue** — bucketed counts of unmatched
///      narrative entities (file paths / methods / abstracts /
///      externals / self-refs). No per-entry rendering — most of
///      these are matcher-coverage gaps, not real drift, and
///      enumerating each adds noise without action.
///
/// Full per-entry detail goes to the JSON sidecar. The markdown is
/// the audit-skim surface; the sidecar is the navigation surface for
/// downstream tools.
fn render_markdown(
    set: &FindingSet,
    narratives: &[NarrativeIndex],
    structural_id: &str,
    _max_findings: usize,
    rough_edges: Option<&RoughEdgesReport>,
    git_archaeology: Option<&GitArchaeologyReport>,
) -> String {
    let queue = bucket_investigation_queue(&set.likely, &set.notes);
    let actionable = set.critical.len();
    let confirmed = set.dual_attested.len();

    let mut md = String::new();
    md.push_str(&format!(
        "# Drift Report — {} actionable · {} confirmed · {} queued\n\n",
        actionable,
        confirmed,
        queue.total,
    ));
    md.push_str(&format!(
        "**Code**: `{}`  ·  **Narrative**: {}\n\n",
        structural_id,
        narratives
            .iter()
            .map(|n| format!("`{}`", n.atlas_id))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // ── Act on ──────────────────────────────────────────────
    if set.critical.is_empty() {
        md.push_str("## Act on\n\n_No normative claims without code anchors. Either nothing's drifting at the principle level, or the matcher missed the anchor — confirm via Investigation queue below._\n\n");
    } else {
        md.push_str("## Act on\n\n");
        for (i, f) in set.critical.iter().enumerate() {
            render_critical_finding(&mut md, i + 1, f);
        }
    }

    // ── Confirmed ───────────────────────────────────────────
    if !set.dual_attested.is_empty() {
        md.push_str(&format!("## Confirmed ({})\n\n", confirmed));
        let mut names: Vec<String> = set
            .dual_attested
            .iter()
            .map(|d| d.canonical_name.clone())
            .collect();
        names.sort();
        names.dedup();
        let cap = 30;
        let show: Vec<&String> = names.iter().take(cap).collect();
        md.push_str(&show.iter().map(|s| format!("`{}`", s)).collect::<Vec<_>>().join(", "));
        if names.len() > cap {
            md.push_str(&format!(", _+{} more_", names.len() - cap));
        }
        md.push_str(".\n\n");
    }

    // ── Provenance & Evolution (git archaeology) ───────────
    if let Some(arch) = git_archaeology {
        render_git_archaeology_section(&mut md, arch);
    }

    // ── Investigation queue ─────────────────────────────────
    if queue.total > 0 {
        md.push_str(&format!(
            "## Investigation queue ({})\n\nMost are matcher-coverage gaps, not real drift. Promote any to Act-on if you disagree:\n\n",
            queue.total
        ));
        for bucket in &queue.buckets {
            md.push_str(&format!(
                "- **{}** ({}): {}\n",
                bucket.label,
                bucket.count,
                bucket.examples.iter().map(|e| format!("`{}`", e)).collect::<Vec<_>>().join(", "),
            ));
        }
        md.push('\n');
    }

    // ── Internal: rough edges (markers + doc drift) ──────────
    if let Some(rough) = rough_edges {
        if !rough.findings.is_empty() {
            render_rough_edges_section(&mut md, rough);
        }
    }

    md.push_str("---\n_Per-finding detail in the JSON sidecar._\n");
    md
}

// ── Rough-edges integration ─────────────────────────────────────
//
// We re-deserialize the JSON sidecar that `sovereign rough-edges`
// emits. Keeping a separate type here (rather than depending on
// corpus_engine_archaeology::rough_edges directly) decouples the renderer from
// the producer's internals — the JSON shape is the contract.

#[derive(Debug, serde::Deserialize)]
struct RoughEdgesReport {
    #[serde(default)]
    corpus_id: String,
    #[serde(default)]
    findings: Vec<RoughEdgeFinding>,
}

#[derive(Debug, serde::Deserialize)]
struct RoughEdgeFinding {
    kind: RoughEdgeKind,
    severity: RoughEdgeSeverity,
    file: PathBuf,
    line: u32,
    #[serde(default)]
    message: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind")]
enum RoughEdgeKind {
    Marker(MarkerKind),
    DocDrift(DocDriftKind),
}

#[derive(Debug, serde::Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
enum MarkerKind {
    Todo,
    Fixme,
    Hack,
    Xxx,
}

#[derive(Debug, serde::Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum DocDriftKind {
    SectionMismatch,
    MissingParam,
    UnknownIdent,
}

#[derive(Debug, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum RoughEdgeSeverity {
    Note,
    Likely,
    Critical,
}

fn load_rough_edges(path: &std::path::Path) -> Option<RoughEdgesReport> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Render the "Internal" digest section. Group by marker kind +
/// severity; cap each group at 5 examples to keep the digest
/// scannable. Full detail lives in the rough-edges JSON sidecar
/// (which the orchestrator preserves at a stable path).
fn render_rough_edges_section(md: &mut String, rough: &RoughEdgesReport) {
    let total = rough.findings.len();
    let critical = rough
        .findings
        .iter()
        .filter(|f| f.severity == RoughEdgeSeverity::Critical)
        .count();
    let likely = rough
        .findings
        .iter()
        .filter(|f| f.severity == RoughEdgeSeverity::Likely)
        .count();

    md.push_str(&format!(
        "## Internal — rough edges ({total})\n\n_{critical} critical · {likely} likely · {} note. \
         The codebase's own marked-up rough edges and any rustdoc-vs-signature drift._\n\n",
        total - critical - likely
    ));

    // Group by marker kind, ordered by severity-desc.
    use std::collections::BTreeMap;
    let mut by_marker: BTreeMap<&'static str, Vec<&RoughEdgeFinding>> = BTreeMap::new();
    for f in &rough.findings {
        let label = marker_label(&f.kind);
        by_marker.entry(label).or_default().push(f);
    }

    let order = ["XXX", "FIXME", "HACK", "TODO", "doc-drift"];
    for label in order {
        let Some(group) = by_marker.get(label) else {
            continue;
        };
        if group.is_empty() {
            continue;
        }
        md.push_str(&format!(
            "- **{label}** ({}): ",
            group.len()
        ));
        let cap = 5;
        let examples: Vec<String> = group
            .iter()
            .take(cap)
            .map(|f| format!("`{}:{}`", short_path(&f.file, &rough.corpus_id), f.line))
            .collect();
        md.push_str(&examples.join(", "));
        if group.len() > cap {
            md.push_str(&format!(", _+{} more_", group.len() - cap));
        }
        md.push('\n');
    }
    md.push('\n');
}

fn marker_label(k: &RoughEdgeKind) -> &'static str {
    match k {
        RoughEdgeKind::Marker(MarkerKind::Xxx) => "XXX",
        RoughEdgeKind::Marker(MarkerKind::Fixme) => "FIXME",
        RoughEdgeKind::Marker(MarkerKind::Hack) => "HACK",
        RoughEdgeKind::Marker(MarkerKind::Todo) => "TODO",
        RoughEdgeKind::DocDrift(_) => "doc-drift",
    }
}

/// Trim the path to the project-relative form when possible. The
/// rough-edges JSON has absolute paths; the digest is more readable
/// with project-relative ones.
fn short_path(p: &std::path::Path, corpus_id: &str) -> String {
    let s = p.to_string_lossy();
    // Strip everything up to and including the corpus_id directory
    // marker if present — works for both `<root>/<corpus>/...` and
    // `<root>/.../<corpus>/...` shapes.
    if let Some(idx) = s.find(&format!("/{corpus_id}/")) {
        return s[idx + corpus_id.len() + 2..].to_string();
    }
    s.to_string()
}

// ── Git-archaeology integration ─────────────────────────────────
//
// Same pattern as RoughEdgesReport above: the renderer re-deserialises
// only the fields it needs from the `git-archaeology` JSON sidecar so
// the producer's internal struct shape can evolve independently. The
// JSON shape itself is the contract.

#[derive(Debug, serde::Deserialize)]
struct GitArchaeologyReport {
    #[serde(default)]
    repo_root: PathBuf,
    #[serde(default)]
    atlas_built_at: i64,
    #[serde(default)]
    atom_count: usize,
    #[serde(default)]
    atoms_with_history: usize,
    #[serde(default)]
    follows_renames: bool,
    #[serde(default)]
    provenance: Vec<GitArchProvenance>,
    #[serde(default)]
    co_evolution: Vec<GitArchCoEvolution>,
    #[serde(default)]
    staleness_summary: GitArchStaleness,
}

#[derive(Debug, serde::Deserialize)]
struct GitArchProvenance {
    #[serde(default)]
    atom_id: String,
    #[serde(default)]
    file_path: PathBuf,
    #[serde(default)]
    first_seen: GitArchCommitRef,
    #[serde(default)]
    last_modified: GitArchCommitRef,
    #[serde(default)]
    stability_days: u32,
    #[serde(default)]
    modification_count: u32,
    #[serde(default)]
    primary_authors: Vec<String>,
    #[serde(default)]
    staleness: GitArchStalenessKind,
}

#[derive(Debug, Default, serde::Deserialize)]
struct GitArchCommitRef {
    #[serde(default)]
    date_iso: String,
    #[serde(default)]
    author_email: String,
    #[serde(default)]
    subject: String,
}

#[derive(Debug, serde::Deserialize)]
struct GitArchCoEvolution {
    #[serde(default)]
    file_a: PathBuf,
    #[serde(default)]
    file_b: PathBuf,
    #[serde(default)]
    joint_commits: u32,
    #[serde(default)]
    a_only: u32,
    #[serde(default)]
    b_only: u32,
    #[serde(default)]
    correlation: f32,
}

#[derive(Debug, Default, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GitArchStalenessKind {
    #[default]
    Fresh,
    Moved,
}

#[derive(Debug, Default, serde::Deserialize)]
struct GitArchStaleness {
    #[serde(default)]
    fresh: usize,
    #[serde(default)]
    moved: usize,
}

fn load_git_archaeology(path: &std::path::Path) -> Option<GitArchaeologyReport> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Render the "Provenance & Evolution" digest section. Four
/// subsections: stability highlights, recent volatility, co-evolution
/// clusters, and staleness queue. Cap each at 5 examples; full per-
/// atom and per-pair detail lives in the JSON sidecar.
fn render_git_archaeology_section(md: &mut String, arch: &GitArchaeologyReport) {
    const N: usize = 5;
    md.push_str(&format!(
        "## Provenance & Evolution ({} of {} atoms enriched)\n\n",
        arch.atoms_with_history, arch.atom_count,
    ));
    md.push_str(&format!(
        "_Repo `{}` · {} co-evolution pairs · {} fresh / {} moved",
        arch.repo_root.display(),
        arch.co_evolution.len(),
        arch.staleness_summary.fresh,
        arch.staleness_summary.moved,
    ));
    if !arch.follows_renames {
        md.push_str(" · renames not followed in v1");
    }
    md.push_str("._\n\n");

    // Stability highlights — fresh atoms with the longest history.
    let mut fresh: Vec<&GitArchProvenance> = arch
        .provenance
        .iter()
        .filter(|p| p.staleness == GitArchStalenessKind::Fresh)
        .collect();
    fresh.sort_by(|a, b| b.stability_days.cmp(&a.stability_days));
    if !fresh.is_empty() {
        md.push_str("**Stability highlights** _(load-bearing — held longest unchanged)_\n\n");
        for p in fresh.iter().take(N) {
            md.push_str(&format!(
                "- `{}` · {} days · {} commits · {}\n",
                p.file_path.display(),
                p.stability_days,
                p.modification_count,
                p.primary_authors.join(", "),
            ));
        }
        md.push('\n');
    }

    // Recent volatility — sort all atoms by last-modified date desc.
    let mut recent: Vec<&GitArchProvenance> = arch.provenance.iter().collect();
    recent.sort_by(|a, b| b.last_modified.date_iso.cmp(&a.last_modified.date_iso));
    if !recent.is_empty() {
        md.push_str("**Recent volatility** _(currently active surfaces)_\n\n");
        for p in recent.iter().take(N) {
            md.push_str(&format!(
                "- `{}` · last touched {} by {} — \"{}\"\n",
                p.file_path.display(),
                p.last_modified.date_iso,
                p.last_modified.author_email,
                truncate_subject(&p.last_modified.subject, 60),
            ));
        }
        md.push('\n');
    }

    // Co-evolution clusters.
    if !arch.co_evolution.is_empty() {
        md.push_str("**Co-evolution clusters** _(implicit coupling)_\n\n");
        for pair in arch.co_evolution.iter().take(N) {
            md.push_str(&format!(
                "- `{}` ↔ `{}` · {:.0}% ({} of {})\n",
                pair.file_a.display(),
                pair.file_b.display(),
                pair.correlation * 100.0,
                pair.joint_commits,
                pair.joint_commits + pair.a_only + pair.b_only,
            ));
        }
        md.push('\n');
    }

    // Staleness queue.
    if arch.staleness_summary.moved > 0 {
        md.push_str(&format!(
            "**Staleness queue** ({}) _candidates for re-extraction — code touched since atlas built_\n\n",
            arch.staleness_summary.moved
        ));
        let moved: Vec<&GitArchProvenance> = arch
            .provenance
            .iter()
            .filter(|p| p.staleness == GitArchStalenessKind::Moved)
            .collect();
        for p in moved.iter().take(N) {
            md.push_str(&format!(
                "- `{}` · last touched {}\n",
                p.file_path.display(),
                p.last_modified.date_iso,
            ));
        }
        if moved.len() > N {
            md.push_str(&format!(
                "- _+{} more (see git_archaeology.json)_\n",
                moved.len() - N
            ));
        }
        md.push('\n');
    }
}

fn truncate_subject(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

fn render_critical_finding(md: &mut String, n: usize, f: &Finding) {
    let location = f
        .narrative
        .as_ref()
        .map(|nref| {
            let chunk = nref.chunk_id.as_deref().unwrap_or("?");
            format!("{} {}", nref.atlas_id, chunk)
        })
        .unwrap_or_default();
    let headline = headline_short(f);
    md.push_str(&format!("**{}. {}** _({})_  \n", n, headline, location));
    if let Some(nref) = &f.narrative {
        if let Some(quote) = &nref.quotable {
            md.push_str(&format!("> {}\n\n", quote.trim()));
        }
    }
    md.push_str(&format!("_Next step:_ {}\n\n", f.action));
}

fn headline_short(f: &Finding) -> String {
    // Strip the "Normative claim without code evidence — `...`"
    // prefix from headlines; the section title already says "Act
    // on" so the prefix is redundant.
    //
    // Two headline shapes:
    //   - "Normative claim — anchor `X` not found in code atlas — `<prose>`"
    //     (Claim had an anchor; atlas didn't match it.) Render the
    //     `anchor X not found` as the parenthetical so the operator
    //     sees the searchable token.
    //   - "Normative claim without code evidence — `<prose>`"
    //     (Claim never had an anchor.) Render `(no anchor)`.
    let h = &f.headline;
    if let Some(rest) = h.strip_prefix("Normative claim — anchor `") {
        // Pull out the anchor identifier between the first pair of
        // backticks, then everything after the next ` — ` is prose.
        if let Some(end) = rest.find('`') {
            let anchor = &rest[..end];
            let tail = &rest[end..];
            // tail starts with `` ` not found in code atlas — `<prose>` ``
            if let Some(prose_part) = tail.find(" — ") {
                let prose = tail[prose_part + " — ".len()..]
                    .trim_matches('`')
                    .to_string();
                return format!(
                    "normative claim _(anchor `{anchor}` not in atlas)_ — {prose}"
                );
            }
        }
    }
    if let Some(idx) = h.find(" — ") {
        let (kind, rest) = h.split_at(idx);
        let rest = rest.trim_start_matches(" — ");
        format!("{} _(no anchor)_ — {}", short_kind(kind), rest.trim_matches('`'))
    } else {
        h.clone()
    }
}

fn short_kind(s: &str) -> &str {
    if s.contains("Normative claim") {
        "normative claim"
    } else if s.contains("Configuration") {
        "configuration"
    } else {
        "claim"
    }
}

// ── Investigation-queue bucketing ────────────────────────────

#[derive(Debug, Default)]
struct InvestigationQueue {
    total: usize,
    buckets: Vec<InvestigationBucket>,
}

#[derive(Debug)]
struct InvestigationBucket {
    label: &'static str,
    count: usize,
    examples: Vec<String>,
}

/// Bucket every Likely + Note finding by what KIND of mismatch it
/// represents. Most are matcher-coverage gaps (file paths, method
/// names, constants the atlas didn't index), not real architectural
/// drift. Show counts + 2-3 examples per bucket; full detail in the
/// JSON sidecar.
fn bucket_investigation_queue(likely: &[Finding], notes: &[Finding]) -> InvestigationQueue {
    let mut by_label: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut total = 0usize;
    for f in likely.iter().chain(notes.iter()) {
        let name = f
            .narrative
            .as_ref()
            .map(|n| n.canonical_name.as_str())
            .or_else(|| f.structural.as_ref().map(|s| s.canonical_name.as_str()))
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let label = bucket_for(name);
        by_label.entry(label).or_default().push(name.to_string());
        total += 1;
    }
    let order = [
        "file path",
        "method/function",
        "constant/identifier",
        "external library",
        "abstract principle",
        "self/config reference",
        "module re-export",
        "worth a closer look",
    ];
    let mut buckets: Vec<InvestigationBucket> = Vec::new();
    for label in order.iter() {
        if let Some(names) = by_label.remove(label) {
            let mut ex: Vec<String> = names.iter().take(3).cloned().collect();
            ex.dedup();
            buckets.push(InvestigationBucket {
                label,
                count: names.len(),
                examples: ex,
            });
        }
    }
    // Catch any unrecognised labels (defensive — shouldn't fire).
    for (label, names) in by_label {
        buckets.push(InvestigationBucket {
            label,
            count: names.len(),
            examples: names.into_iter().take(3).collect(),
        });
    }
    InvestigationQueue { total, buckets }
}

/// Classify an entity name into one of the investigation-queue
/// buckets. Order matters: more specific patterns first.
fn bucket_for(name: &str) -> &'static str {
    let lower = name.to_lowercase();

    // File paths: ends in a source extension, or contains a slash.
    let exts = [".rs", ".toml", ".json", ".md", ".yaml", ".yml", ".scm", ".sh"];
    if exts.iter().any(|e| lower.ends_with(e)) || name.contains('/') {
        return "file path";
    }

    // Self/config references: known top-level docs and dotfiles.
    let self_refs = [
        "ARCH_PRINCIPLES", "SYSTEM_OVERVIEW", "CHARTER", "CLAUDE",
        "_corpus_meta", "models.toml", "sovereign-server.toml",
        "registry.toml",
    ];
    if self_refs.iter().any(|s| name.contains(s)) {
        return "self/config reference";
    }

    // Model identifiers (Qwen3.5-9B.Q8_0, Llama-3-8B-Instruct, etc.):
    // contain a model-family stem plus dotted version/quant suffix.
    let model_stems = [
        "Qwen", "qwen", "Llama", "llama", "Gemma", "gemma", "Mistral",
        "mistral", "Phi", "phi", "Claude", "claude", "GPT-", "gpt-",
        "Bonsai", "bonsai", "FINAL-Bench",
    ];
    if model_stems.iter().any(|s| name.starts_with(s)) {
        return "external library";
    }

    // Method/function/identifier names with `::` or `.` separators.
    // Distinguish by case of the LAST segment:
    //   `MdnsDiscovery::browse` → method (lowercase last)
    //   `Error::IncompatibleEmbedding` → constant/identifier
    //     (uppercase last — enum variant, associated const)
    //   `sovereign_core::oicp` → module re-export
    //     (all-lowercase path, no PascalCase anywhere)
    if name.contains("::") || name.contains('.') {
        let last = name
            .rsplit("::")
            .next()
            .and_then(|s| s.rsplit('.').next())
            .unwrap_or("");
        let first_upper = last
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false);
        let first_lower = last
            .chars()
            .next()
            .map(|c| c.is_lowercase())
            .unwrap_or(false);
        if first_upper {
            // PascalCase or ALL_CAPS — variant / type / const.
            return "constant/identifier";
        }
        if first_lower {
            return "method/function";
        }
        return "module re-export";
    }

    // Constants: SCREAMING_SNAKE_CASE.
    if name.chars().all(|c| !c.is_lowercase()) && name.contains('_') {
        return "constant/identifier";
    }

    // Known external libraries / vocabulary.
    let externals = [
        "Tantivy", "tantivy", "LanceDB", "lance", "tokio", "serde", "reqwest",
        "rustfmt", "rustdoc", "cargo", "rust-analyzer", "tree-sitter",
        "llama-cpp-2", "llama.cpp", "Ollama", "ollama", "vLLM", "TGI",
        "Tailscale", "WireGuard", "mDNS", "DNS-SD", "IVF-PQ",
        "SOLID", "SICP", "OpenAlex",
    ];
    if externals.contains(&name) {
        return "external library";
    }

    // Abstract principles: lowercase single word, hyphenated, or
    // common-noun discussion vocabulary.
    if name.contains('-') && name.chars().any(|c| c.is_lowercase()) {
        return "abstract principle";
    }
    if name == lower && !name.contains('_') {
        return "abstract principle";
    }

    "worth a closer look"
}

// ── Helpers ──────────────────────────────────────────────────

fn narrative_view(atlas_id: &str, atom: &AtomEnvelope) -> NarrativeAtomView {
    match atom {
        AtomEnvelope::Entity(e) => NarrativeAtomView {
            atlas_id: atlas_id.to_string(),
            id: e.id.as_str().to_string(),
            atom_type: "Entity".to_string(),
            canonical_name: e.canonical_name.clone(),
            aliases: e.aliases.clone(),
            entity_type: entity_type_string(&e.entity_type),
            discourse_act: None,
            epistemic_status: None,
            description: e.description.clone(),
            quotable_excerpt: e.defining_quote.clone(),
            chunk_id: Some(e.first_appearance.chunk_id.clone()),
            anchor: None,
        },
        AtomEnvelope::Claim(c) => NarrativeAtomView {
            atlas_id: atlas_id.to_string(),
            id: c.id.as_str().to_string(),
            atom_type: "Claim".to_string(),
            // canonical_name is the string the cross-corpus fuzzy
            // matcher consults to look up a structural-atlas symbol.
            // For a Claim with a code anchor (engineering-atlas
            // pipeline emits `code_anchors[0]` → `Claim.anchor`),
            // prefer that — it's already a verbatim source-span
            // snap and reads as a function or file name. Falling
            // back to the prose first sentence is what we had
            // before the anchor field existed; it always missed
            // because the matcher's threshold is too tight for
            // 80-character paraphrases of normative rules.
            //
            // The headline rendering reads `description` (full
            // prose) separately, so swapping canonical_name here
            // doesn't affect what the operator sees — only what
            // the matcher tries to ground.
            canonical_name: c
                .anchor
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| truncate_first_sentence(&c.content, 80)),
            aliases: Vec::new(),
            entity_type: String::new(),
            discourse_act: Some(format!("{:?}", c.discourse_act)),
            epistemic_status: Some(format!("{:?}", c.epistemic_status)),
            description: c.content.clone(),
            quotable_excerpt: c.quotable_excerpt.clone(),
            chunk_id: c.evidence.first().map(|r| r.chunk_id.clone()),
            anchor: c.anchor.clone().filter(|s| !s.trim().is_empty()),
        },
        AtomEnvelope::Configuration(c) => NarrativeAtomView {
            atlas_id: atlas_id.to_string(),
            id: c.id.as_str().to_string(),
            atom_type: "Configuration".to_string(),
            canonical_name: c.label.clone(),
            aliases: Vec::new(),
            entity_type: String::new(),
            discourse_act: None,
            epistemic_status: None,
            description: c.description.clone(),
            quotable_excerpt: None,
            chunk_id: c.evidence.first().map(|r| r.chunk_id.clone()),
            anchor: None,
        },
        // Other atom types (Event, State, Relation, ArgumentReconstruction,
        // Question) aren't load-bearing for v1 drift; render as Notes only.
        other => {
            let id = match other {
                AtomEnvelope::Event(e) => e.id.as_str().to_string(),
                AtomEnvelope::State(s) => s.id.as_str().to_string(),
                AtomEnvelope::Relation(r) => r.id.as_str().to_string(),
                AtomEnvelope::ArgumentReconstruction(a) => a.id.as_str().to_string(),
                AtomEnvelope::Question(q) => q.id.as_str().to_string(),
                _ => "unknown".to_string(),
            };
            NarrativeAtomView {
                atlas_id: atlas_id.to_string(),
                id,
                atom_type: format!("{:?}", other).split('(').next().unwrap_or("Other").to_string(),
                canonical_name: String::new(),
                aliases: Vec::new(),
                entity_type: String::new(),
                discourse_act: None,
                epistemic_status: None,
                description: String::new(),
                quotable_excerpt: None,
                chunk_id: None,
                anchor: None,
            }
        }
    }
}

fn narrative_ref(atom: &NarrativeAtomView) -> NarrativeRef {
    NarrativeRef {
        atlas_id: atom.atlas_id.clone(),
        atom_id: atom.id.clone(),
        atom_type: atom.atom_type.clone(),
        canonical_name: atom.canonical_name.clone(),
        chunk_id: atom.chunk_id.clone(),
        quotable: atom
            .quotable_excerpt
            .clone()
            .or_else(|| Some(truncate_first_sentence(&atom.description, 200))),
    }
}

fn entity_type_string(t: &corpus_engine::enrichment::pipeline::atlas::EntityType) -> String {
    use corpus_engine::enrichment::pipeline::atlas::EntityType;
    match t {
        EntityType::Other(s) => s.clone(),
        other => format!("{:?}", other).to_lowercase(),
    }
}

/// Heuristic noise filter for narrative entities that aren't
/// architectural surface area. Three families:
///
///   1. Document references — anything that looks like a markdown
///      filename or a path-shaped entity. The team's docs talk
///      ABOUT each other; those mentions aren't code.
///   2. Lowercase abstract terms — `contract`, `assertions`,
///      `invariant`, `mechanism`. The literary pipeline extracts
///      these as Entities because they're prose nouns, but the
///      drift signal we care about is named code components, not
///      vocabulary.
///   3. Self-references to the source doc — entities whose
///      canonical name is a doc filename or section heading slug.
///
/// Conservative: only skip when the heuristic is high-confidence.
/// When in doubt, include — false-positive findings are
/// recoverable; false negatives silently drop drift signal.
fn is_narrative_noise(atom_type: &str, canonical: &str) -> bool {
    if atom_type != "Entity" {
        return false;
    }
    let lower = canonical.to_lowercase();
    if lower.ends_with(".md") || lower.contains("/docs/") || lower.contains(".md/") {
        return true;
    }
    if lower.starts_with(".sovereign/") || lower.starts_with("sovereign/docs/") {
        return true;
    }
    // Lowercase single-word common nouns the pipeline picks up.
    if !canonical.contains(' ') && !canonical.contains(':') && canonical == lower {
        const COMMON: &[&str] = &[
            "contract", "assertions", "invariant", "mechanism", "principle",
            "principles", "policy", "design", "constraint", "constraints",
            "convention", "conventions", "test", "tests", "doc", "docs",
            "documentation", "feature", "features", "component", "components",
            "module", "modules", "system", "systems", "service", "services",
            "interface", "interfaces", "spec", "specs", "specification",
            "rule", "rules", "guideline", "guidelines", "principle", "code",
        ];
        if COMMON.contains(&lower.as_str()) {
            return true;
        }
    }
    // Any entity name with spaces is prose, not a code symbol —
    // Rust/TS/Go/Python identifiers can't contain spaces, so an
    // Entity with whitespace is necessarily a discussion-noun the
    // pipeline picked up. (`KnowledgeView` and `corpus_engine::engine`
    // both pass.)
    if canonical.contains(' ') && !canonical.contains("::") {
        return true;
    }
    false
}

fn is_normative_status(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("normative")
        || lower.contains("imperative")
        || lower.contains("prescriptive")
        || lower.contains("must")
}

fn atom_text_is_normative(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains(" must ")
        || lower.contains(" must.")
        || lower.contains(" shall ")
        || lower.contains(" never ")
        || lower.contains(" always ")
        || lower.contains(" required ")
        || lower.starts_with("must ")
        || lower.starts_with("shall ")
}

fn normalise_name(s: &str) -> String {
    s.to_lowercase()
        .replace('-', "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect()
}

fn truncate_first_sentence(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let end = trimmed
        .find(". ")
        .or_else(|| trimmed.find("? "))
        .or_else(|| trimmed.find("! "))
        .map(|i| i + 1)
        .unwrap_or(trimmed.len());
    let candidate = &trimmed[..end];
    if candidate.chars().count() <= max_chars {
        candidate.to_string()
    } else {
        candidate.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn compute_in_degree(edges_path: &Path) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let Ok(raw) = fs::read_to_string(edges_path) else {
        return counts;
    };
    let Ok(value): Result<Value, _> = serde_json::from_str(&raw) else {
        return counts;
    };
    let edges = value.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for e in edges {
        if let Some(target) = e.get("target").and_then(|v| v.as_str()) {
            *counts.entry(target.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

fn load_cross_corpus(path: &Path) -> Option<Vec<Value>> {
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let edges = value.get("edges")?.as_array()?.clone();
    Some(edges)
}

