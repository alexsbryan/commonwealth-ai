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
//!     enrich init <id> --from-template free-will-debate --force
//!     enrich build <id>
//!     enrich eval <id> bench/philosophy/free-will-debate.toml
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
use corpus_engine::enrichment::atlas::axis_catalog::{all_axes, AtomKind, GatingField, TypedAxis};
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
            "Reads ~/.sovereign/indexes/<corpus>/atlas/{atoms,edges,gaps,configurations,tension_candidates}.json and ~/.sovereign/indexes/<corpus>/field_skeleton.json. Phases whose artefacts are absent are skipped with a note rather than scored as zero — the table column shows '—' so a partial pipeline run does not look like a regression.",
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

// ── Argument parsing ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PhaseFilter {
    All,
    Positions,
    Atoms,
    Edges,
    FaultLines,
    Gaps,
    Configurations,
}

impl PhaseFilter {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "all" => Ok(Self::All),
            "positions" | "skeleton" => Ok(Self::Positions),
            "atoms" => Ok(Self::Atoms),
            "edges" => Ok(Self::Edges),
            "fault-lines" | "fault_lines" | "tensions" => Ok(Self::FaultLines),
            "gaps" | "open-questions" | "open_questions" => Ok(Self::Gaps),
            "configurations" | "config" => Ok(Self::Configurations),
            other => Err(format!(
                "unknown --phase: {other:?} (allowed: positions, atoms, edges, fault-lines, gaps, configurations, all)"
            )),
        }
    }

    fn includes(self, other: PhaseFilter) -> bool {
        self == Self::All || self == other
    }
}

#[derive(Debug)]
struct ParsedEval {
    corpus_id: String,
    golden_path: PathBuf,
    phase: PhaseFilter,
    report_path: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<ParsedEval, String> {
    let mut corpus_id: Option<String> = None;
    let mut golden_path: Option<PathBuf> = None;
    let mut phase = PhaseFilter::All;
    let mut report_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--phase" => {
                let raw = args
                    .get(i + 1)
                    .ok_or("--phase requires a value".to_string())?;
                phase = PhaseFilter::parse(raw)?;
                i += 2;
            }
            "--report" => {
                report_path = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or("--report requires a path".to_string())?,
                ));
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if golden_path.is_none() {
                    golden_path = Some(PathBuf::from(other));
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                i += 1;
            }
        }
    }
    Ok(ParsedEval {
        corpus_id: corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?,
        golden_path: golden_path.ok_or_else(|| "missing <golden-set-path>".to_string())?,
        phase,
        report_path,
    })
}

// ── Golden-set TOML schema ─────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GoldenSet {
    #[allow(dead_code)]
    #[serde(default)]
    meta: GoldenMeta,
    #[serde(default)]
    expected_positions: Vec<ExpectedPosition>,
    #[serde(default)]
    forbidden_positions: Vec<ForbiddenName>,
    #[serde(default)]
    expected_person_atoms: Vec<ExpectedAtom>,
    #[serde(default)]
    forbidden_person_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    expected_concept_atoms: Vec<ExpectedAtom>,
    #[serde(default)]
    forbidden_concept_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    expected_work_atoms: Vec<ExpectedAtom>,
    #[serde(default)]
    forbidden_work_atoms: Vec<ForbiddenName>,
    // Literary atom kinds — used by literary_atlas goldens. Philosophy
    // goldens omit these; they're optional and score `None` when absent.
    #[serde(default)]
    expected_event_atoms: Vec<ExpectedEvent>,
    #[serde(default)]
    forbidden_event_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    expected_state_atoms: Vec<ExpectedState>,
    #[serde(default)]
    expected_relation_atoms: Vec<ExpectedRelation>,
    #[serde(default)]
    forbidden_relation_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    expected_question_atoms: Vec<ExpectedQuestion>,
    #[serde(default)]
    expected_claim_atoms: Vec<ExpectedClaim>,
    #[serde(default)]
    expected_discourse_act_distribution: Vec<DiscourseActDistribution>,
    #[serde(default)]
    expected_fault_lines: Vec<ExpectedFaultLine>,
    #[serde(default)]
    forbidden_fault_lines: Vec<ForbiddenFaultLine>,
    #[serde(default)]
    expected_open_questions: Vec<ExpectedOpenQuestion>,
    #[serde(default)]
    expected_configurations: Vec<ExpectedConfiguration>,
    #[serde(default)]
    forbidden_configurations: Vec<ForbiddenName>,
    // Phase 3b edges, scored under `PhaseFilter::Edges` against
    // `edges.json` across ALL edge types. `score_fault_lines` scores
    // the same file but only its `Tension` slice against position
    // pairs; this axis is the general one (`Grounds`, `Causes`,
    // `EvidenceFor`, …) and shares one endpoint resolver with it.
    #[serde(default)]
    expected_edges: Vec<ExpectedEdge>,
    #[serde(default)]
    forbidden_edges: Vec<ForbiddenEdge>,

    // ─── v2 typed-extension axes (Argumentative discourse mode) ───
    //
    // Scored against `Phase1Output.questions_by_chapter[*].
    // section_extraction.{type_extension, type_extensions}` per
    // `AtlasSnapshot::argumentative_*`. All axes are optional; an
    // empty array means "no signal on this axis", not "expected
    // zero". Goldens for non-argumentative corpora can omit them
    // entirely.
    #[serde(default)]
    expected_mechanism_atoms: Vec<ExpectedMechanism>,
    #[serde(default)]
    forbidden_mechanism_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    expected_named_position_atoms: Vec<ExpectedNamedPosition>,
    #[serde(default)]
    forbidden_named_position_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    expected_evidence_atoms: Vec<ExpectedEvidence>,
    #[serde(default)]
    expected_opposition_atoms: Vec<ExpectedOpposition>,
    #[serde(default)]
    expected_concession_atoms: Vec<ExpectedConcession>,
}

impl GoldenSet {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let raw =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        toml::from_str::<Self>(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct GoldenMeta {
    #[serde(default)]
    #[allow(dead_code)]
    template: String,
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
    /// Corpus this golden scores against. Authoritative when present;
    /// `bench all` discovery falls back to the filename stem when
    /// absent and warn-logs the inference.
    #[serde(default)]
    pub corpus_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedPosition {
    name_contains_any: Vec<String>,
    #[serde(default)]
    epistemic_status: Option<String>,
    #[serde(default)]
    proponents_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ForbiddenName {
    #[serde(alias = "canonical_name_contains_any")]
    #[serde(alias = "label_contains_any")]
    name_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedAtom {
    #[serde(alias = "name_contains_any")]
    canonical_name_contains_any: Vec<String>,
    #[serde(default)]
    description_keywords_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedEvent {
    /// Substrings the event description must contain. Match policy is
    /// "any" so a golden can list paraphrases ("dies in the woods" /
    /// "death in the woods" / "tragic death") without requiring an
    /// exact phrasing match.
    description_contains_any: Vec<String>,
    /// When non-empty, ANY listed name must appear among the event's
    /// participant entities (resolved via entity_name_by_id). Useful
    /// for asserting "Fyodor Pavlovitch's death involves him as a
    /// participant", catching the failure mode where the event is
    /// extracted but the participants are stripped.
    #[serde(default)]
    participants_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedState {
    /// Substrings the state's `entity_id`-resolved name must contain.
    /// E.g. for "Eveline's paralysis at the dock" the entity is
    /// Eveline.
    entity_name_contains_any: Vec<String>,
    /// Substrings the state's `label` must contain.
    label_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedRelation {
    /// Each entry is the set of names (any-match) one participant must
    /// match. Two entries → asserts a pair where one matches A and the
    /// other matches B (in either order). One entry → asserts at least
    /// one participant matches that set, regardless of partner.
    participants_a_any: Vec<String>,
    #[serde(default)]
    participants_b_any: Vec<String>,
    /// Substrings the relation's `label` must contain.
    #[serde(default)]
    label_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedQuestion {
    content_contains_any: Vec<String>,
    #[serde(default)]
    status_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedClaim {
    content_contains_any: Vec<String>,
    #[serde(default)]
    attributed_proponent_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

// ─── Argumentative typed-extension expectations ───────────────────
//
// Match policy mirrors `ExpectedAtom`: `*_contains_any` is a
// case-insensitive substring against the named field; ANY listed
// substring satisfies the match. Discriminator fields (`stance`,
// `kind`, `outcome`) require exact match against the canonical
// snake_case enum literal when supplied — left `None` to skip.

#[derive(Debug, Clone, Deserialize)]
struct ExpectedMechanism {
    /// Substrings against the mechanism's `name`. Load-bearing
    /// signal: a mechanism atom is the named lever the section's
    /// argument turns on. Mirrors `canonical_name_contains_any` on
    /// `ExpectedAtom` to keep the golden's verb shape consistent.
    #[serde(alias = "canonical_name_contains_any")]
    name_contains_any: Vec<String>,
    /// Optional substrings against the mechanism's `description`.
    /// Informational — a name-only hit still counts as a match;
    /// description divergence surfaces in `name_only_hits`.
    #[serde(default)]
    description_keywords_any: Vec<String>,
    /// Optional substrings against the mechanism's `domain` tag
    /// (`economics`, `urbanism`, `music`, ...). When non-empty,
    /// ANY listed substring satisfies; empty = no domain filter.
    #[serde(default)]
    domain_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedNamedPosition {
    /// Substrings against the position's `name` ("the
    /// rent-concentration thesis"; "Hardin's tragedy framing").
    /// Distinct from `ExpectedPosition` (Phase 1 skeleton) — this
    /// scores the v2 typed-extension Position sketch.
    #[serde(alias = "canonical_name_contains_any")]
    name_contains_any: Vec<String>,
    /// Optional substrings against the position's `content` (the
    /// one-sentence statement). Informational; absence does not
    /// reduce recall.
    #[serde(default)]
    content_contains_any: Vec<String>,
    /// Optional substrings against the position's `proponent` field
    /// (entity name attribution). Empty = no proponent filter.
    #[serde(default)]
    proponent_contains_any: Vec<String>,
    /// Optional exact-match on the position's `stance`
    /// (`endorse` | `rebut` | `survey` | `mixed`). None = skip.
    #[serde(default)]
    stance: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedEvidence {
    /// Substrings against the evidence atom's `label` (e.g.
    /// `"$1.4B FTC PBM spread"`, `"Soviet Aral Sea counter-example"`).
    label_contains_any: Vec<String>,
    /// Optional substrings against the evidence atom's `content`
    /// (the one-sentence statement of what the evidence is).
    #[serde(default)]
    content_contains_any: Vec<String>,
    /// Optional exact-match on the evidence atom's `kind`
    /// (`study` | `figure` | `historical_example` | `case_study` |
    /// `personal_anecdote` | `quotation` | `other`). None = skip.
    #[serde(default)]
    kind: Option<String>,
    /// Optional substrings against the evidence atom's `supports`
    /// field (the claim/position the evidence is invoked to back).
    /// Empty = no supports filter.
    #[serde(default)]
    supports_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedOpposition {
    /// Substrings against the opposition's `left` label.
    left_contains_any: Vec<String>,
    /// Substrings against the opposition's `right` label.
    right_contains_any: Vec<String>,
    /// Optional substrings against the opposition's `axis` (the
    /// dimension along which they differ). Empty = no axis filter.
    #[serde(default)]
    axis_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedConcession {
    /// Substrings against the concession's `content` (the
    /// one-sentence statement of what the author concedes).
    content_contains_any: Vec<String>,
    /// Optional substrings against the concession's `addresses`
    /// field (the position or claim the concession addresses).
    /// Empty = no addresses filter.
    #[serde(default)]
    addresses_contains_any: Vec<String>,
    /// Optional exact-match on the concession's `outcome`
    /// (`intact` | `narrowed` | `retracted`). None = skip.
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DiscourseActDistribution {
    required_acts_any: Vec<String>,
    #[serde(default)]
    forbidden_uniform_act: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedFaultLine {
    position_a_contains_any: Vec<String>,
    position_b_contains_any: Vec<String>,
    #[serde(default)]
    crux_keywords_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ForbiddenFaultLine {
    position_a_contains_any: Vec<String>,
    position_b_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
}

/// A Phase 3b edge the golden asserts should exist. `edge_type` is the
/// PascalCase [`EdgeType`] tag (`"Tension"`, `"Grounds"`, …) or `"*"`
/// for "any type". Endpoints match by keyword against the resolved
/// endpoint NAME, not the raw `AtomId` — see
/// [`resolve_endpoint_name`].
///
/// Direction is load-bearing here and is NOT symmetric, unlike
/// [`ExpectedFaultLine`]: `Grounds(frankfurt case → compatibilism)`
/// and its reverse are different assertions about the argument.
#[derive(Debug, Clone, Deserialize)]
struct ExpectedEdge {
    #[serde(default = "any_edge_type")]
    edge_type: String,
    source_contains_any: Vec<String>,
    target_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

/// An edge the golden asserts must NOT exist — the anti-test half of
/// the axis. Same matching rules as [`ExpectedEdge`].
#[derive(Debug, Clone, Deserialize)]
struct ForbiddenEdge {
    #[serde(default = "any_edge_type")]
    edge_type: String,
    source_contains_any: Vec<String>,
    target_contains_any: Vec<String>,
    /// Author's intent tag (e.g. `"proponent_of"`). The edge model has
    /// no such field, so this is NOT evaluated — `score_edges` reports
    /// the fact in its notes rather than silently matching on the
    /// remaining criteria as though the constraint had been checked.
    #[serde(default)]
    relation_kind: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
}

/// Wildcard for a golden that constrains endpoints but not type.
fn any_edge_type() -> String {
    "*".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedOpenQuestion {
    content_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedConfiguration {
    label_contains_any: Vec<String>,
    #[serde(default)]
    description_keywords_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

// ── Atlas snapshot ─────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct AtlasSnapshot {
    skeleton: Option<FieldSkeleton>,
    atoms: Option<AtomsFile>,
    edges: Option<EdgesFile>,
    gaps: Option<GapsOutput>,
    configurations: Option<ConfigurationsOutput>,
}

impl AtlasSnapshot {
    pub(crate) fn load(atlas_dir: &Path, skeleton_path: &Path) -> Result<Self, String> {
        let skeleton = if skeleton_path.exists() {
            let raw = std::fs::read_to_string(skeleton_path)
                .map_err(|e| format!("read {}: {e}", skeleton_path.display()))?;
            Some(
                serde_json::from_str::<FieldSkeleton>(&raw)
                    .map_err(|e| format!("parse {}: {e}", skeleton_path.display()))?,
            )
        } else {
            None
        };

        let atoms_path = atlas_dir.join("atoms.json");
        let atoms = if atoms_path.exists() {
            Some(read_json(&atoms_path)?)
        } else {
            None
        };
        let edges_path = atlas_dir.join("edges.json");
        let edges = if edges_path.exists() {
            Some(read_json(&edges_path)?)
        } else {
            None
        };
        let gaps_path = atlas_dir.join("gaps.json");
        let gaps = if gaps_path.exists() {
            Some(read_json(&gaps_path)?)
        } else {
            None
        };
        let cfg_path = atlas_dir.join("configurations.json");
        let configurations = if cfg_path.exists() {
            Some(read_json(&cfg_path)?)
        } else {
            None
        };

        Ok(Self {
            skeleton,
            atoms,
            edges,
            gaps,
            configurations,
        })
    }

    // ─── Typed-extension accessors (Phase 1 cache) ────────────────
    //
    // Each accessor walks `questions_by_chapter[*].section_extraction`
    // and visits every active `TypeExtension` on that section — both
    // the v2 plural slot (`type_extensions: Vec<TypeExtension>`) and
    // the v1 legacy singular (`type_extension: Option<TypeExtension>`).
    // Returns the sketches paired with the originating section_id so
    // scorers can attribute hits and misses to specific sections in
    // the report.

    // Typed-axis candidate collection moved to
    // `collect_axis_atoms` in the catalog-driven scoring block —
    // search for "Catalog-driven axis scoring" below. Adding a new
    // typed axis no longer means adding a snapshot accessor.

    fn entities_of_type(&self, kind: EntityType) -> Vec<&Entity> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Entity(e) if e.entity_type == kind => Some(e),
                _ => None,
            })
            .collect()
    }

    /// All Entity atoms regardless of type. Used by forbidden-atom
    /// checks so that a `forbidden_person_atoms` rule for "narrator"
    /// fires even when the model evaded the type tag by emitting it as
    /// `entity_type: unspecified`. The semantic is "this concept must
    /// not be lifted to an entity at all" — not "this concept must
    /// not be a Person specifically".
    fn all_entities(&self) -> Vec<&Entity> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Entity(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    fn questions(&self) -> Vec<&Question> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Question(q) => Some(q),
                _ => None,
            })
            .collect()
    }

    fn events(&self) -> Vec<&Event> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Event(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    fn states(&self) -> Vec<&State> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::State(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    fn relations(&self) -> Vec<&Relation> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Relation(r) => Some(r),
                _ => None,
            })
            .collect()
    }

    fn claims(&self) -> Vec<&corpus_engine::enrichment::atlas::atoms::Claim> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Claim(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn configurations_inline(&self) -> Vec<&Configuration> {
        let Some(file) = &self.atoms else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Configuration(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn entity_name_by_id(&self, id: &AtomId) -> Option<&str> {
        let file = self.atoms.as_ref()?;
        file.atoms.iter().find_map(|a| match a {
            AtomEnvelope::Entity(e) if e.id == *id => Some(e.canonical_name.as_str()),
            _ => None,
        })
    }

    /// Every name the entity is known by (canonical + aliases). Used by
    /// participant-keyword matchers so a golden listing "Alyosha" still
    /// credits an event whose participant resolves to entity
    /// `Alexey Fyodorovich Karamazov` with `aliases: ["Alyosha"]`. The
    /// canonical-only version is kept for display contexts (miss
    /// labels, fault-line endpoint resolution) where one name is wanted.
    fn entity_match_strings_by_id(&self, id: &AtomId) -> Vec<&str> {
        let Some(file) = self.atoms.as_ref() else {
            return Vec::new();
        };
        file.atoms
            .iter()
            .find_map(|a| match a {
                AtomEnvelope::Entity(e) if e.id == *id => {
                    let mut names: Vec<&str> = Vec::with_capacity(1 + e.aliases.len());
                    names.push(e.canonical_name.as_str());
                    names.extend(e.aliases.iter().map(String::as_str));
                    Some(names)
                }
                _ => None,
            })
            .unwrap_or_default()
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str::<T>(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

// ── Match primitives ───────────────────────────────────────────────

fn matches_any(haystack: &str, needles: &[String]) -> bool {
    if needles.is_empty() {
        return true; // "no constraint" → trivially satisfied
    }
    let lower = normalize_for_match(haystack);
    // Fast path: case-insensitive substring.
    if needles
        .iter()
        .any(|n| lower.contains(&normalize_for_match(n)))
    {
        return true;
    }
    // Token-presence fallback for multi-token needles. Handles
    // surface-form variance the substring check can't see — e.g.
    // golden's `"hard incompatibilism"` matching corpus's
    // `"incompatibilism (hard)"` (paren reorders the tokens but
    // both tokens are present). Single-token needles fall through
    // (no improvement).
    let haystack_tokens: std::collections::HashSet<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    needles.iter().any(|n| {
        let n_norm = normalize_for_match(n);
        let n_tokens: Vec<String> = n_norm
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        n_tokens.len() >= 2 && n_tokens.iter().all(|t| haystack_tokens.contains(t))
    })
}

/// `matches_any` plus a 7-char common-prefix fallback used only
/// for fault-line position-name matching.
///
/// Rationale: golden authors write academic surface forms
/// (`aristotelian`, `situationism`, `sentimentalist`) but the
/// corpus's atom inventory often uses the proponent's name
/// (`Aristotle`) or a related concept (`situational variables`).
/// Plain substring fails — `aristotle` doesn't contain
/// `aristotelian` (the suffix `-elian` versus the proper noun's
/// `-tle` ending diverge at index 7) and `situational` doesn't
/// contain `situationism`. A 7-char common-prefix rule across
/// haystack tokens captures these without admitting the
/// false-positive cases (`polis` vs `police` share only 4 chars,
/// `stoic` vs `stoicism` share 5; both stay below threshold).
///
/// Why 7: empirically threads the needle between
/// `aristotle/aristotelian` (7) and `aristotle/aristocracy` (6).
/// At 6 we'd over-match across academic root families that share
/// a Greek prefix; at 8+ we'd lose the load-bearing
/// philosopher/school bridge. 7 is the smallest threshold
/// preserving the bridge without admitting the family confusions
/// the bench corpora actually contain.
///
/// Scoped to fault-line position matching specifically so the
/// rule's slightly looser stance doesn't propagate into entity /
/// claim / question matching where the strict substring rule has
/// served well.
fn matches_any_with_morphology(haystack: &str, needles: &[String]) -> bool {
    if matches_any(haystack, needles) {
        return true;
    }
    if needles.is_empty() {
        return false;
    }
    const MIN_PREFIX: usize = 7;
    let h_lower = normalize_for_match(haystack);
    let h_tokens: Vec<String> = h_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= MIN_PREFIX)
        .map(str::to_string)
        .collect();
    if h_tokens.is_empty() {
        return false;
    }
    needles.iter().any(|n| {
        let n_lower = normalize_for_match(n);
        // Restrict to single-token needles ≥ MIN_PREFIX chars long;
        // multi-token needles already get the token-presence path.
        if n_lower.len() < MIN_PREFIX || n_lower.chars().any(|c| !c.is_alphanumeric()) {
            return false;
        }
        h_tokens.iter().any(|t| {
            let common: usize = n_lower
                .chars()
                .zip(t.chars())
                .take_while(|(a, b)| a == b)
                .count();
            common >= MIN_PREFIX
        })
    })
}

/// Lowercase + fold the four common Unicode "smart" punctuation marks
/// to ASCII so that golden keywords like `O'Rourke` (typed in
/// straight ASCII) match the actual atom name `O'Rourke` (Project
/// Gutenberg / Word-style curly apostrophe). Keeps everything else
/// unchanged. Without this fold, every passage of curly-quoted prose
/// silently fails substring matching against ASCII goldens.
fn normalize_for_match(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            other => other,
        })
        .collect::<String>()
        .to_ascii_lowercase()
}

fn any_match_in_list<'a, I, F>(items: I, needles: &[String], extract: F) -> bool
where
    I: IntoIterator<Item = &'a String>,
    F: Fn(&str) -> bool,
    String: 'a,
{
    let _ = extract; // marker — kept for clarity
    items.into_iter().any(|s| matches_any(s, needles))
}

// ── Scoring ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PhaseScore {
    pub expected: usize,
    pub matched: usize,
    pub forbidden_total: usize,
    pub forbidden_hit: usize,
    /// Per-expected hit list — names pulled from the golden's
    /// `*_contains_any` field (first entry by convention) so the
    /// report's miss column is human-readable.
    pub misses: Vec<String>,
    pub forbidden_hits: Vec<String>,
    pub notes: Vec<String>,
    /// Total candidate artefacts the scorer saw for this axis — the
    /// extraction VOLUME. `#[serde(default)]` keeps pre-P0.2 baselines
    /// deserializable (they read as 0 candidates → rate `None`).
    #[serde(default)]
    pub candidates: usize,
    /// Candidates explained by NO expected entry and NO forbidden
    /// entry: extraction volume that earns zero credit and, before
    /// P0.2, carried zero cost. The adjudication sampler
    /// (`bench enrichment-adjudicate`) prices how much of it is junk.
    #[serde(default)]
    pub unmatched_count: usize,
    /// Up to [`UNMATCHED_SAMPLE_CAP`] labels of unmatched candidates,
    /// for the human report. The full set is recomputed on demand by
    /// the adjudicator; this is a preview, not the record.
    #[serde(default)]
    pub unmatched_samples: Vec<String>,
}

impl PhaseScore {
    /// Precision = TP / (TP + FP). When the model emitted zero atoms
    /// AND the golden expected zero atoms, precision is undefined and
    /// the phase is genuinely silent (`None`). When the golden expected
    /// atoms but the model produced none, precision is treated as 0.0
    /// — otherwise zero-recall failures fall out of `f1()` as `None`
    /// and never enter the aggregate, hiding the regression. This bit
    /// is the difference between "no scoreable artefacts" (a silent
    /// phase) and "tried and failed" (a recall=0 phase).
    pub(crate) fn precision(&self) -> Option<f32> {
        let denom = self.matched + self.forbidden_hit;
        if denom == 0 {
            // Two sub-cases:
            //  - expected == 0 → genuinely undefined, stay silent
            //  - expected > 0  → zero-recall failure, return 0.0 so
            //    f1() lands in the aggregate
            if self.expected == 0 {
                return None;
            }
            return Some(0.0);
        }
        Some(self.matched as f32 / denom as f32)
    }

    pub(crate) fn recall(&self) -> Option<f32> {
        if self.expected == 0 {
            return None;
        }
        Some(self.matched as f32 / self.expected as f32)
    }

    pub(crate) fn f1(&self) -> Option<f32> {
        let p = self.precision()?;
        let r = self.recall()?;
        if p + r == 0.0 {
            return Some(0.0);
        }
        Some(2.0 * p * r / (p + r))
    }

    /// Fraction of the axis's candidate pool no golden entry explains.
    /// `None` when the pool is empty (nothing extracted ≠ over-
    /// extraction). Deliberately NOT folded into precision: the :30
    /// forbidden-only FP contract stays for baseline compat; this is
    /// the parallel volume signal.
    pub(crate) fn unmatched_rate(&self) -> Option<f32> {
        if self.candidates == 0 {
            return None;
        }
        Some(self.unmatched_count as f32 / self.candidates as f32)
    }
}

/// Cap on unmatched sample labels serialized per axis. Keeps report
/// JSON (and the lane baselines that embed it) bounded on corpora
/// where extraction volume dwarfs the golden.
const UNMATCHED_SAMPLE_CAP: usize = 10;

/// Char-boundary-safe label truncation for unmatched samples.
fn truncate_label(s: &str) -> String {
    const MAX: usize = 80;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let cut: String = s.chars().take(MAX).collect();
    format!("{cut}…")
}

/// Candidate-centric second pass: count candidates no golden entry
/// (expected OR forbidden) explains. Runs after the expected-centric
/// loops so it never perturbs the existing match/miss/FP accounting.
fn tally_unmatched<T>(
    s: &mut PhaseScore,
    candidates: &[T],
    label: impl Fn(&T) -> String,
    explained: impl Fn(&T) -> bool,
) {
    s.candidates = candidates.len();
    for c in candidates {
        if !explained(c) {
            s.unmatched_count += 1;
            if s.unmatched_samples.len() < UNMATCHED_SAMPLE_CAP {
                s.unmatched_samples.push(truncate_label(&label(c)));
            }
        }
    }
}

// ── Catalog-driven axis scoring ───────────────────────────────────
//
// One driver function replaces the five hand-coded
// score_{mechanism,named_position,evidence,opposition,concession}_
// atoms helpers. Adding a new typed axis to the bench is now:
//   1. add an arm to `resolve_type_extensions` that produces the
//      projected atom shape (concept_kind / claim_kind / new
//      AtomEnvelope variant),
//   2. add a `TypedAxis` const entry in
//      `corpus_engine::enrichment::atlas::axis_catalog`, and
//   3. add a golden TOML block for the corresponding axis.
//
// No new Rust file. No new scorer. The catalog's `gating_fields` /
// `atom_kind` declaration drives `collect_axis_atoms` (candidate
// pool) and `matches_axis` (gating predicate).

/// Per-expectation row in the uniform view the driver consumes.
/// Built lazily from `GoldenSet.expected_*_atoms` named fields at
/// score time so the on-disk TOML schema doesn't change.
struct AxisExpectation<'a> {
    /// Primary-name needle list. Used by `GatingField::Name`.
    name_contains_any: &'a [String],
    /// Position stance gate (`endorse` / `rebut` / ...). None = skip.
    stance: Option<&'a str>,
    /// Kind-discriminator gate. `EntityWithConceptKind` /
    /// `ClaimWithKind` axes filter candidates by qualifier in
    /// `collect_axis_atoms`; the field is populated for future
    /// catalog axes whose collector cannot pre-filter (e.g. cross-
    /// kind matching), and stays here so the uniform view shape
    /// doesn't grow another variant later.
    #[allow(dead_code)]
    kind: Option<&'a str>,
    /// Opposition left/right gates (order-independent).
    left_contains_any: &'a [String],
    right_contains_any: &'a [String],

    // ─── Informational fields (mismatch → PhaseScore.note, not miss)
    description_keywords_any: &'a [String],
    domain_contains_any: &'a [String],
    content_contains_any: &'a [String],
    proponent_contains_any: &'a [String],
    supports_contains_any: &'a [String],
    axis_contains_any: &'a [String],
    addresses_contains_any: &'a [String],
    outcome: Option<&'a str>,
}

impl<'a> AxisExpectation<'a> {
    fn empty() -> Self {
        Self {
            name_contains_any: &[],
            stance: None,
            kind: None,
            left_contains_any: &[],
            right_contains_any: &[],
            description_keywords_any: &[],
            domain_contains_any: &[],
            content_contains_any: &[],
            proponent_contains_any: &[],
            supports_contains_any: &[],
            axis_contains_any: &[],
            addresses_contains_any: &[],
            outcome: None,
        }
    }

    /// Label printed in `PhaseScore.misses` when this expectation
    /// goes unmatched. First non-empty needle wins; falls back to
    /// composed "L vs R" for Opposition.
    fn miss_label(&self) -> String {
        if let Some(s) = self.name_contains_any.first().cloned() {
            return s;
        }
        if !self.left_contains_any.is_empty() || !self.right_contains_any.is_empty() {
            return format!(
                "{} vs {}",
                self.left_contains_any.first().cloned().unwrap_or_default(),
                self.right_contains_any.first().cloned().unwrap_or_default()
            );
        }
        if let Some(c) = self.content_contains_any.first().cloned() {
            return c;
        }
        String::new()
    }
}

/// Forbidden block — name-based anti-test.
struct AxisForbidden<'a> {
    name_contains_any: &'a [String],
}

impl<'a> AxisForbidden<'a> {
    fn label(&self) -> String {
        self.name_contains_any.first().cloned().unwrap_or_default()
    }
}

/// Candidate atom enum — uniform over Entity / Claim / Position /
/// Opposition so the matcher doesn't need a per-axis branch on
/// candidate shape.
enum AxisCandidate<'a> {
    Entity(&'a Entity),
    Claim(&'a corpus_engine::enrichment::atlas::atoms::Claim),
    Position(&'a Position),
    Opposition(&'a Opposition),
}

impl<'a> AxisCandidate<'a> {
    fn primary_text(&self) -> &str {
        match self {
            AxisCandidate::Entity(e) => &e.canonical_name,
            AxisCandidate::Claim(c) => &c.content,
            AxisCandidate::Position(p) => &p.canonical_name,
            // Opposition primary text used only for forbidden-block
            // name matching, which we don't expose for Opposition v1.
            AxisCandidate::Opposition(o) => &o.axis,
        }
    }

    fn stance(&self) -> Option<&str> {
        if let AxisCandidate::Position(p) = self {
            Some(&p.stance)
        } else {
            None
        }
    }

    fn opposition_labels(&self) -> Option<(&str, &str)> {
        if let AxisCandidate::Opposition(o) = self {
            Some((&o.left_label, &o.right_label))
        } else {
            None
        }
    }

    fn description(&self) -> Option<&str> {
        match self {
            AxisCandidate::Entity(e) => Some(&e.description),
            AxisCandidate::Claim(c) => Some(&c.content),
            AxisCandidate::Position(p) => Some(&p.content),
            AxisCandidate::Opposition(o) => Some(&o.axis),
        }
    }

    /// Resolve the candidate's proponent / attributed-author name
    /// against the snapshot's entity table. Position-only; everything
    /// else returns None.
    fn proponent_name(&self, snap: &AtlasSnapshot) -> Option<String> {
        match self {
            AxisCandidate::Position(p) => p
                .proponent_id
                .as_ref()
                .and_then(|id| snap.entity_name_by_id(id))
                .map(str::to_string),
            _ => None,
        }
    }
}

/// Collect candidate atoms for an axis. Filters by qualifier when
/// the catalog's `AtomKind` is `EntityWithConceptKind` /
/// `ClaimWithKind`. Returns an empty Vec when atoms.json is absent.
fn collect_axis_atoms<'a>(axis: &TypedAxis, snap: &'a AtlasSnapshot) -> Vec<AxisCandidate<'a>> {
    let Some(file) = snap.atoms.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for atom in &file.atoms {
        let candidate = match (axis.atom_kind, atom) {
            (AtomKind::EntityWithConceptKind(tag), AtomEnvelope::Entity(e))
                if e.concept_kind.as_deref() == Some(tag) =>
            {
                Some(AxisCandidate::Entity(e))
            }
            (AtomKind::ClaimWithKind(tag), AtomEnvelope::Claim(c))
                if c.claim_kind.as_deref() == Some(tag) =>
            {
                Some(AxisCandidate::Claim(c))
            }
            (AtomKind::Entity, AtomEnvelope::Entity(e)) => Some(AxisCandidate::Entity(e)),
            (AtomKind::Claim, AtomEnvelope::Claim(c)) => Some(AxisCandidate::Claim(c)),
            (AtomKind::Position, AtomEnvelope::Position(p)) => Some(AxisCandidate::Position(p)),
            (AtomKind::Opposition, AtomEnvelope::Opposition(o)) => {
                Some(AxisCandidate::Opposition(o))
            }
            _ => None,
        };
        if let Some(c) = candidate {
            out.push(c);
        }
    }
    out
}

/// Build the per-axis expectation view from the GoldenSet's existing
/// named fields. Keeps the on-disk TOML schema unchanged — the
/// uniform shape lives only in memory.
fn axis_expectations<'a>(
    axis: &TypedAxis,
    golden: &'a GoldenSet,
) -> (Vec<AxisExpectation<'a>>, Vec<AxisForbidden<'a>>) {
    match axis.key {
        "mechanism" => (
            golden
                .expected_mechanism_atoms
                .iter()
                .map(|e| AxisExpectation {
                    name_contains_any: &e.name_contains_any,
                    description_keywords_any: &e.description_keywords_any,
                    domain_contains_any: &e.domain_contains_any,
                    ..AxisExpectation::empty()
                })
                .collect(),
            golden
                .forbidden_mechanism_atoms
                .iter()
                .map(|f| AxisForbidden {
                    name_contains_any: &f.name_contains_any,
                })
                .collect(),
        ),
        "named_position" => (
            golden
                .expected_named_position_atoms
                .iter()
                .map(|e| AxisExpectation {
                    name_contains_any: &e.name_contains_any,
                    stance: e.stance.as_deref(),
                    content_contains_any: &e.content_contains_any,
                    proponent_contains_any: &e.proponent_contains_any,
                    ..AxisExpectation::empty()
                })
                .collect(),
            golden
                .forbidden_named_position_atoms
                .iter()
                .map(|f| AxisForbidden {
                    name_contains_any: &f.name_contains_any,
                })
                .collect(),
        ),
        "evidence" => (
            golden
                .expected_evidence_atoms
                .iter()
                .map(|e| AxisExpectation {
                    name_contains_any: &e.label_contains_any,
                    kind: e.kind.as_deref(),
                    content_contains_any: &e.content_contains_any,
                    supports_contains_any: &e.supports_contains_any,
                    ..AxisExpectation::empty()
                })
                .collect(),
            Vec::new(),
        ),
        "opposition" => (
            golden
                .expected_opposition_atoms
                .iter()
                .map(|e| AxisExpectation {
                    left_contains_any: &e.left_contains_any,
                    right_contains_any: &e.right_contains_any,
                    axis_contains_any: &e.axis_contains_any,
                    ..AxisExpectation::empty()
                })
                .collect(),
            Vec::new(),
        ),
        "concession" => (
            golden
                .expected_concession_atoms
                .iter()
                .map(|e| AxisExpectation {
                    name_contains_any: &e.content_contains_any,
                    outcome: e.outcome.as_deref(),
                    addresses_contains_any: &e.addresses_contains_any,
                    ..AxisExpectation::empty()
                })
                .collect(),
            Vec::new(),
        ),
        _ => (Vec::new(), Vec::new()),
    }
}

/// Apply the catalog axis's gating-field policy. Returns true iff
/// the candidate satisfies every gate. Informational fields are
/// NOT checked here — they produce notes after a positive name hit
/// (see `emit_informational_notes`).
fn matches_axis(axis: &TypedAxis, candidate: &AxisCandidate, expect: &AxisExpectation) -> bool {
    for gate in axis.gating_fields {
        match gate {
            GatingField::Name => {
                // Empty needle list means "no name gate for this
                // expectation" — useful for Opposition where the
                // gate is left/right pairing. Don't fail on empty.
                if !expect.name_contains_any.is_empty()
                    && !matches_any(candidate.primary_text(), expect.name_contains_any)
                {
                    return false;
                }
            }
            GatingField::Stance => {
                if let Some(want) = expect.stance {
                    let actual = candidate.stance().unwrap_or("");
                    if !actual.eq_ignore_ascii_case(want) {
                        return false;
                    }
                }
            }
            GatingField::Kind => {
                // Already enforced by `collect_axis_atoms`'
                // `concept_kind` / `claim_kind` filter. Kept as a
                // gating-field variant so the catalog row is
                // self-describing: a reader sees `[Name, Kind]` and
                // knows the axis is qualified.
            }
            GatingField::Opposition => {
                let Some((left, right)) = candidate.opposition_labels() else {
                    return false;
                };
                let direct = matches_any(left, expect.left_contains_any)
                    && matches_any(right, expect.right_contains_any);
                let reversed = matches_any(left, expect.right_contains_any)
                    && matches_any(right, expect.left_contains_any);
                if !direct && !reversed {
                    return false;
                }
            }
        }
    }
    true
}

/// Post-match informational checks. Emits a `PhaseScore.note` per
/// mismatched supplementary field. Each axis's specific note shapes
/// are preserved from the legacy code so JSON consumers (and the
/// human reading the scoreboard) see identical messages.
fn emit_informational_notes(
    axis: &TypedAxis,
    candidate: &AxisCandidate,
    expect: &AxisExpectation,
    snap: &AtlasSnapshot,
    out: &mut PhaseScore,
) {
    match axis.key {
        "mechanism" => {
            let desc = candidate.description().unwrap_or("");
            let name = candidate.primary_text();
            if !expect.description_keywords_any.is_empty()
                && !matches_any(desc, expect.description_keywords_any)
            {
                out.notes.push(format!(
                    "mechanism name match for {:?} but description keywords did not hit",
                    name
                ));
            }
            if !expect.domain_contains_any.is_empty()
                && !matches_any(desc, expect.domain_contains_any)
            {
                out.notes.push(format!(
                    "mechanism name match for {:?} but domain keywords did not hit in description",
                    name
                ));
            }
        }
        "named_position" => {
            let name = candidate.primary_text();
            let content = candidate.description().unwrap_or("");
            if !expect.content_contains_any.is_empty()
                && !matches_any(content, expect.content_contains_any)
            {
                out.notes.push(format!(
                    "position name match for {:?} but content keywords did not hit",
                    name
                ));
            }
            if !expect.proponent_contains_any.is_empty() {
                let proponent = candidate.proponent_name(snap).unwrap_or_default();
                if !matches_any(&proponent, expect.proponent_contains_any) {
                    out.notes.push(format!(
                        "position name match for {:?} but proponent {:?} not in expected list",
                        name, proponent
                    ));
                }
            }
        }
        "evidence" => {
            let content = candidate.primary_text();
            let preview: String = content.chars().take(60).collect();
            if !expect.content_contains_any.is_empty()
                && !matches_any(content, expect.content_contains_any)
            {
                out.notes.push(format!(
                    "evidence content match {:?} but content keywords did not hit",
                    preview
                ));
            }
            if !expect.supports_contains_any.is_empty() {
                out.notes.push(format!(
                    "evidence supports_contains_any check deferred to Stage 4 (EvidenceFor edge walk); claim {:?} matched on label",
                    preview
                ));
            }
        }
        "opposition" => {
            let (left, right) = candidate.opposition_labels().unwrap_or(("", ""));
            let axis_text = candidate.description().unwrap_or("");
            if !expect.axis_contains_any.is_empty()
                && !matches_any(axis_text, expect.axis_contains_any)
            {
                out.notes.push(format!(
                    "opposition {:?} vs {:?} matched but axis {:?} not in expected list",
                    left, right, axis_text
                ));
            }
        }
        "concession" => {
            let content = candidate.primary_text();
            let preview: String = content.chars().take(60).collect();
            if let Some(want) = expect.outcome {
                let actual = match candidate {
                    AxisCandidate::Claim(c) => c.concession_outcome.as_deref().unwrap_or(""),
                    _ => "",
                };
                if !actual.eq_ignore_ascii_case(want) {
                    out.notes.push(format!(
                        "concession content match but outcome {:?} ≠ expected {:?}",
                        actual, want
                    ));
                }
            }
            if !expect.addresses_contains_any.is_empty() {
                out.notes.push(format!(
                    "concession addresses_contains_any check deferred to Stage 4 (Concedes edge walk); claim {:?} matched on content",
                    preview
                ));
            }
        }
        _ => {}
    }
}

/// Score a single axis. Returns None when the golden carries no
/// expected or forbidden entries for this axis — `absence ≠ zero
/// recall` is preserved from the legacy per-axis gates.
fn score_axis(axis: &TypedAxis, golden: &GoldenSet, snap: &AtlasSnapshot) -> Option<PhaseScore> {
    let (expected, forbidden) = axis_expectations(axis, golden);
    if expected.is_empty() && forbidden.is_empty() {
        return None;
    }

    let mut s = PhaseScore::default();
    s.expected = expected.len();
    s.forbidden_total = forbidden.len();

    if snap.atoms.is_none() {
        s.notes
            .push("atoms.json absent — typed-extension scoring lanes have no signal".to_string());
        return Some(s);
    }

    let candidates = collect_axis_atoms(axis, snap);

    for exp in &expected {
        let hit = candidates.iter().find(|c| matches_axis(axis, c, exp));
        match hit {
            Some(c) => {
                s.matched += 1;
                emit_informational_notes(axis, c, exp, snap, &mut s);
            }
            None => s.misses.push(exp.miss_label()),
        }
    }

    // Forbidden checks are name-only for v1 (matches the legacy
    // mechanism / named_position policy). Other forbidden shapes
    // (e.g. forbidden_opposition_pair) can be added later.
    for fexp in &forbidden {
        if candidates
            .iter()
            .any(|c| matches_any(c.primary_text(), fexp.name_contains_any))
        {
            s.forbidden_hit += 1;
            s.forbidden_hits.push(fexp.label());
        }
    }

    tally_unmatched(
        &mut s,
        &candidates,
        |c| c.primary_text().to_string(),
        |c| {
            expected.iter().any(|exp| matches_axis(axis, c, exp))
                || forbidden
                    .iter()
                    .any(|f| matches_any(c.primary_text(), f.name_contains_any))
        },
    );

    Some(s)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct EvalReport {
    pub corpus_id: String,
    pub golden_path: String,
    pub positions: Option<PhaseScore>,
    pub person_atoms: Option<PhaseScore>,
    pub concept_atoms: Option<PhaseScore>,
    pub work_atoms: Option<PhaseScore>,
    pub event_atoms: Option<PhaseScore>,
    pub state_atoms: Option<PhaseScore>,
    pub relation_atoms: Option<PhaseScore>,
    pub question_atoms: Option<PhaseScore>,
    pub claim_atoms: Option<PhaseScore>,
    pub discourse_act_distribution: Option<DiscourseActReport>,
    pub edges: Option<PhaseScore>,
    pub fault_lines: Option<PhaseScore>,
    pub open_questions: Option<PhaseScore>,
    pub configurations: Option<PhaseScore>,

    // v2 typed-extension axes (Argumentative). Each is scored under
    // `PhaseFilter::Atoms` when its golden axis is non-empty.
    //
    // `axis_scores` is the authoritative storage — keyed by
    // `TypedAxis.key`. The five named fields below mirror the
    // canonical map so existing JSON consumers / baseline diffs see
    // identical keys. New axes added to `AXIS_CATALOG` show up only
    // in `axis_scores`, not as new named fields.
    pub axis_scores: BTreeMap<String, PhaseScore>,
    pub mechanism_atoms: Option<PhaseScore>,
    pub named_position_atoms: Option<PhaseScore>,
    pub evidence_atoms: Option<PhaseScore>,
    pub opposition_atoms: Option<PhaseScore>,
    pub concession_atoms: Option<PhaseScore>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct DiscourseActReport {
    pub total_claims: usize,
    pub act_counts: Vec<(String, usize)>,
    pub required_satisfied: bool,
    pub uniform_violation: Option<String>,
    pub notes: Vec<String>,
}

fn score(golden: &GoldenSet, snap: &AtlasSnapshot, phase: PhaseFilter) -> EvalReport {
    let mut report = EvalReport {
        corpus_id: String::new(),
        golden_path: String::new(),
        ..Default::default()
    };

    // Phase 1 positions (skeleton)
    if phase.includes(PhaseFilter::Positions) {
        report.positions = Some(score_positions(golden, snap));
    }
    // Phase 3a/3b atoms
    if phase.includes(PhaseFilter::Atoms) {
        report.person_atoms = Some(score_entity_atoms(
            &golden.expected_person_atoms,
            &golden.forbidden_person_atoms,
            snap,
            EntityType::Person,
        ));
        report.concept_atoms = Some(score_entity_atoms(
            &golden.expected_concept_atoms,
            &golden.forbidden_concept_atoms,
            snap,
            EntityType::Concept,
        ));
        report.work_atoms = Some(score_entity_atoms(
            &golden.expected_work_atoms,
            &golden.forbidden_work_atoms,
            snap,
            EntityType::Work,
        ));
        if !golden.expected_event_atoms.is_empty() || !golden.forbidden_event_atoms.is_empty() {
            report.event_atoms = Some(score_event_atoms(golden, snap));
        }
        if !golden.expected_state_atoms.is_empty() {
            report.state_atoms = Some(score_state_atoms(golden, snap));
        }
        if !golden.expected_relation_atoms.is_empty() || !golden.forbidden_relation_atoms.is_empty()
        {
            report.relation_atoms = Some(score_relation_atoms(golden, snap));
        }
        report.question_atoms = Some(score_question_atoms(golden, snap));
        report.claim_atoms = Some(score_claim_atoms(golden, snap));
        if !golden.expected_discourse_act_distribution.is_empty() {
            report.discourse_act_distribution = Some(score_discourse_acts(golden, snap));
        }

        // v2 typed-extension scoring driven by `AXIS_CATALOG`. Each
        // axis is scored only when the golden surfaces it; absence ≠
        // zero recall. The named-field mirror (mechanism_atoms etc.)
        // is populated below for back-compat with existing JSON
        // consumers and baseline diffs.
        for axis in all_axes() {
            if let Some(score) = score_axis(axis, golden, snap) {
                report.axis_scores.insert(axis.key.to_string(), score);
            }
        }
        report.mechanism_atoms = report.axis_scores.get("mechanism").cloned();
        report.named_position_atoms = report.axis_scores.get("named_position").cloned();
        report.evidence_atoms = report.axis_scores.get("evidence").cloned();
        report.opposition_atoms = report.axis_scores.get("opposition").cloned();
        report.concession_atoms = report.axis_scores.get("concession").cloned();
    }
    // Phase 3b edges. Scored only when the golden authors the axis —
    // an absent axis means "no signal here", not "expected zero", so
    // a golden that omits edges must not read as 0% recall.
    if phase.includes(PhaseFilter::Edges)
        && (!golden.expected_edges.is_empty() || !golden.forbidden_edges.is_empty())
    {
        report.edges = Some(score_edges(golden, snap));
    }
    // Phase 6 fault lines
    if phase.includes(PhaseFilter::FaultLines) {
        report.fault_lines = Some(score_fault_lines(golden, snap));
    }
    // Phase 7 gaps
    if phase.includes(PhaseFilter::Gaps) {
        report.open_questions = Some(score_open_questions(golden, snap));
    }
    // Phase 8 configurations
    if phase.includes(PhaseFilter::Configurations) {
        report.configurations = Some(score_configurations(golden, snap));
    }
    report
}

fn score_positions(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_positions.len();
    s.forbidden_total = golden.forbidden_positions.len();

    let positions: Vec<&SkeletonPosition> = match &snap.skeleton {
        Some(sk) => sk
            .canonical_questions
            .iter()
            .flat_map(|q| q.positions.iter())
            .collect(),
        None => {
            s.notes
                .push("field_skeleton.json not present — skipping positions scoring".to_string());
            return s;
        }
    };

    for ep in &golden.expected_positions {
        let hit = positions.iter().find(|p| position_matches(p, ep));
        if hit.is_some() {
            s.matched += 1;
        } else {
            s.misses
                .push(ep.name_contains_any.first().cloned().unwrap_or_default());
        }
    }
    for fp in &golden.forbidden_positions {
        if positions
            .iter()
            .any(|p| matches_any(&p.name, &fp.name_contains_any))
        {
            s.forbidden_hit += 1;
            s.forbidden_hits
                .push(fp.name_contains_any.first().cloned().unwrap_or_default());
        }
    }
    tally_unmatched(
        &mut s,
        &positions,
        |p| p.name.clone(),
        |p| {
            golden
                .expected_positions
                .iter()
                .any(|ep| position_matches(p, ep))
                || golden
                    .forbidden_positions
                    .iter()
                    .any(|fp| matches_any(&p.name, &fp.name_contains_any))
        },
    );
    s
}

fn position_matches(p: &SkeletonPosition, ep: &ExpectedPosition) -> bool {
    let name_ok = matches_any(&p.name, &ep.name_contains_any);
    let status_ok = match &ep.epistemic_status {
        None => true,
        Some(want) => p.status.eq_ignore_ascii_case(want),
    };
    let prop_ok = if ep.proponents_any.is_empty() {
        true
    } else {
        any_match_in_list(&p.proponents, &ep.proponents_any, |x| !x.is_empty())
    };
    name_ok && status_ok && prop_ok
}

fn score_entity_atoms(
    expected: &[ExpectedAtom],
    forbidden: &[ForbiddenName],
    snap: &AtlasSnapshot,
    kind: EntityType,
) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = expected.len();
    s.forbidden_total = forbidden.len();

    let entities: Vec<&Entity> = entity_pool(snap, kind);
    if snap.atoms.is_none() {
        s.notes
            .push("atoms.json not present — skipping entity scoring".to_string());
        return s;
    }

    // Match policy: name_contains_any is the load-bearing signal. A
    // canonical-name match alone counts as a hit.
    // description_keywords_any, when specified, is informational — we
    // record name+description hits separately so a divergence between
    // them shows up in the notes column. Treating description as a
    // hard AND makes the matcher reject real extractions whose
    // description happens to use different vocabulary than the
    // golden specified, which inflates the false-negative rate
    // without measuring anything the pipeline can act on.
    let mut name_only_hits = 0usize;
    for ee in expected {
        let by_name = entities
            .iter()
            .find(|e| matches_any(&e.canonical_name, &ee.canonical_name_contains_any));
        match by_name {
            Some(e) => {
                s.matched += 1;
                if !ee.description_keywords_any.is_empty()
                    && !matches_any(&e.description, &ee.description_keywords_any)
                {
                    name_only_hits += 1;
                }
            }
            None => {
                s.misses.push(
                    ee.canonical_name_contains_any
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                );
            }
        }
    }
    if name_only_hits > 0 {
        s.notes.push(format!(
            "{name_only_hits} hit(s) matched on name only — golden's \
             description_keywords_any didn't appear in the extracted description"
        ));
    }
    // Forbidden checks scan entities of the SAME type plus
    // `Other(_)` (the hedge bucket). The type-scoped check is what
    // makes `forbidden_person_atoms = ["NFL"]` mean "NFL should not
    // appear AS A PERSON" rather than "NFL should not appear
    // anywhere" — without scoping, a correctly-classified NFL
    // Institution would trip the forbidden_person check, conflating
    // the right call with a regression. The `entities` list defined
    // above already covers typed-or-unspecified for `kind`; reuse it
    // so the narrator/type-evasion failure mode still gets caught
    // (a "narrator" emitted with entity_type=unspecified shows up in
    // `untyped` and remains in the forbidden scan).
    for fb in forbidden {
        if entities
            .iter()
            .any(|e| matches_any(&e.canonical_name, &fb.name_contains_any))
        {
            s.forbidden_hit += 1;
            s.forbidden_hits
                .push(fb.name_contains_any.first().cloned().unwrap_or_default());
        }
    }
    // Unmatched accounting runs over the same typed-plus-hedge pool the
    // matcher saw. The `Other(_)` hedge bucket is shared by the three
    // entity axes (person/concept/work), so an unexplained hedge atom
    // counts against each axis that could have claimed it — disclosed
    // here rather than hidden, because the hedge bucket is where
    // over-extraction most often lands.
    tally_unmatched(
        &mut s,
        &entities,
        |e| e.canonical_name.clone(),
        |e| entity_explained(e, expected, forbidden),
    );
    s
}

/// The candidate pool an entity axis scores over. Expected matches
/// accept the requested type OR an `Other` variant (the catch-all for
/// type strings the schema doesn't name — most commonly "unspecified"
/// or "unknown"). The model frequently hedges typing on borderline
/// cases (e.g. emitting "Mangan's sister" or "the narrator" with
/// entity_type: unspecified rather than Person). Penalising hedges as
/// zero recall conflates "model couldn't classify the type" with
/// "model didn't surface the entity at all". The first is a quality
/// concern; the second is a hard miss. Treating Other(_) as a
/// fallback recovers recall on the hard miss; the
/// `description_keywords_any` note still flags lower-quality hits.
fn entity_pool(snap: &AtlasSnapshot, kind: EntityType) -> Vec<&Entity> {
    let typed: Vec<&Entity> = snap.entities_of_type(kind);
    let untyped: Vec<&Entity> = snap
        .all_entities()
        .into_iter()
        .filter(|e| {
            matches!(e.entity_type, EntityType::Other(_)) && !typed.iter().any(|t| t.id == e.id)
        })
        .collect();
    typed.into_iter().chain(untyped).collect()
}

/// A candidate entity is "explained" when any expected entry's
/// load-bearing name check hits it, or a forbidden entry names it.
/// Mirrors the match policy above: name is the signal, description is
/// informational.
fn entity_explained(e: &Entity, expected: &[ExpectedAtom], forbidden: &[ForbiddenName]) -> bool {
    expected
        .iter()
        .any(|ee| matches_any(&e.canonical_name, &ee.canonical_name_contains_any))
        || forbidden
            .iter()
            .any(|fb| matches_any(&e.canonical_name, &fb.name_contains_any))
}

fn score_event_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_event_atoms.len();
    s.forbidden_total = golden.forbidden_event_atoms.len();
    let events = snap.events();
    if snap.atoms.is_none() {
        s.notes
            .push("atoms.json not present — skipping event scoring".to_string());
        return s;
    }

    for ee in &golden.expected_event_atoms {
        let hit = events.iter().find(|e| event_matches(e, ee, snap));
        if hit.is_some() {
            s.matched += 1;
        } else {
            s.misses.push(
                ee.description_contains_any
                    .first()
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    for fb in &golden.forbidden_event_atoms {
        if events
            .iter()
            .any(|e| matches_any(&e.description, &fb.name_contains_any))
        {
            s.forbidden_hit += 1;
            s.forbidden_hits
                .push(fb.name_contains_any.first().cloned().unwrap_or_default());
        }
    }
    tally_unmatched(
        &mut s,
        &events,
        |e| e.description.clone(),
        |e| event_explained(e, golden, snap),
    );
    s
}

fn event_matches(e: &Event, ee: &ExpectedEvent, snap: &AtlasSnapshot) -> bool {
    let desc_ok = matches_any(&e.description, &ee.description_contains_any);
    let part_ok = if ee.participants_any.is_empty() {
        true
    } else {
        e.participants.iter().any(|pid| {
            snap.entity_match_strings_by_id(pid)
                .iter()
                .any(|n| matches_any(n, &ee.participants_any))
        })
    };
    desc_ok && part_ok
}

fn event_explained(e: &Event, golden: &GoldenSet, snap: &AtlasSnapshot) -> bool {
    golden
        .expected_event_atoms
        .iter()
        .any(|ee| event_matches(e, ee, snap))
        || golden
            .forbidden_event_atoms
            .iter()
            .any(|fb| matches_any(&e.description, &fb.name_contains_any))
}

fn score_state_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_state_atoms.len();
    let states = snap.states();
    if snap.atoms.is_none() {
        s.notes
            .push("atoms.json not present — skipping state scoring".to_string());
        return s;
    }
    for es in &golden.expected_state_atoms {
        let hit = states.iter().find(|st| state_matches(st, es, snap));
        if hit.is_some() {
            s.matched += 1;
        } else {
            // Report as "<entity>: <label>" so a miss in the table tells
            // the reader which axis failed to land.
            let ent = es
                .entity_name_contains_any
                .first()
                .cloned()
                .unwrap_or_default();
            let lab = es.label_contains_any.first().cloned().unwrap_or_default();
            s.misses.push(format!("{ent}: {lab}"));
        }
    }
    tally_unmatched(
        &mut s,
        &states,
        |st| {
            let ent = snap
                .entity_match_strings_by_id(&st.entity_id)
                .first()
                .map(|n| n.to_string())
                .unwrap_or_else(|| st.entity_id.as_str().to_string());
            format!("{ent}: {}", st.label)
        },
        |st| {
            golden
                .expected_state_atoms
                .iter()
                .any(|es| state_matches(st, es, snap))
        },
    );
    s
}

fn state_matches(st: &State, es: &ExpectedState, snap: &AtlasSnapshot) -> bool {
    let entity_ok = snap
        .entity_match_strings_by_id(&st.entity_id)
        .iter()
        .any(|n| matches_any(n, &es.entity_name_contains_any));
    let label_ok = matches_any(&st.label, &es.label_contains_any);
    entity_ok && label_ok
}

fn score_relation_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_relation_atoms.len();
    s.forbidden_total = golden.forbidden_relation_atoms.len();
    let relations = snap.relations();
    if snap.atoms.is_none() {
        s.notes
            .push("atoms.json not present — skipping relation scoring".to_string());
        return s;
    }

    for er in &golden.expected_relation_atoms {
        let hit = relations.iter().find(|r| relation_matches(r, er, snap));
        if hit.is_some() {
            s.matched += 1;
        } else {
            let pa = er.participants_a_any.first().cloned().unwrap_or_default();
            let pb = er
                .participants_b_any
                .first()
                .cloned()
                .unwrap_or_else(|| "*".into());
            s.misses.push(format!("{pa} ↔ {pb}"));
        }
    }
    for fb in &golden.forbidden_relation_atoms {
        if relations
            .iter()
            .any(|r| relation_forbidden_hit(r, fb, snap))
        {
            s.forbidden_hit += 1;
            s.forbidden_hits
                .push(fb.name_contains_any.first().cloned().unwrap_or_default());
        }
    }
    tally_unmatched(
        &mut s,
        &relations,
        |r| {
            let names: Vec<String> = relation_name_sets(r, snap)
                .iter()
                .map(|ns| ns.first().cloned().unwrap_or_default())
                .collect();
            format!("{} [{}]", r.label, names.join(" ↔ "))
        },
        |r| {
            golden
                .expected_relation_atoms
                .iter()
                .any(|er| relation_matches(r, er, snap))
                || golden
                    .forbidden_relation_atoms
                    .iter()
                    .any(|fb| relation_forbidden_hit(r, fb, snap))
        },
    );
    s
}

/// Per-participant name set (canonical + aliases). A relation
/// pair-match accepts a hit on any of an entity's known names so a
/// golden listing "Alyosha" credits a relation involving entity
/// "Alexey Fyodorovich Karamazov".
fn relation_name_sets(r: &Relation, snap: &AtlasSnapshot) -> Vec<Vec<String>> {
    r.participants
        .iter()
        .map(|pid| {
            snap.entity_match_strings_by_id(pid)
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .collect()
}

fn relation_matches(r: &Relation, er: &ExpectedRelation, snap: &AtlasSnapshot) -> bool {
    let name_sets = relation_name_sets(r, snap);
    let any_match = |needles: &[String]| -> bool {
        name_sets
            .iter()
            .any(|names| names.iter().any(|n| matches_any(n, needles)))
    };
    // Two-side check requires the matches to come from
    // *different* participants. Same-participant double-hit
    // (one entity's name happens to fall in both keyword
    // sets) would otherwise spuriously satisfy a pair check.
    let pair_ok = if er.participants_b_any.is_empty() {
        any_match(&er.participants_a_any)
    } else {
        name_sets.iter().enumerate().any(|(i, names_i)| {
            let a_here = names_i
                .iter()
                .any(|n| matches_any(n, &er.participants_a_any));
            if !a_here {
                return false;
            }
            name_sets.iter().enumerate().any(|(j, names_j)| {
                i != j
                    && names_j
                        .iter()
                        .any(|n| matches_any(n, &er.participants_b_any))
            })
        })
    };
    let label_ok = matches_any(&r.label, &er.label_contains_any);
    pair_ok && label_ok
}

fn relation_forbidden_hit(r: &Relation, fb: &ForbiddenName, snap: &AtlasSnapshot) -> bool {
    let label_hit = matches_any(&r.label, &fb.name_contains_any);
    let name_hit = relation_name_sets(r, snap)
        .iter()
        .any(|names| names.iter().any(|n| matches_any(n, &fb.name_contains_any)));
    label_hit || name_hit
}

fn score_question_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_question_atoms.len();
    let questions = snap.questions();
    if snap.atoms.is_none() {
        s.notes
            .push("atoms.json not present — skipping question scoring".to_string());
        return s;
    }
    for eq in &golden.expected_question_atoms {
        let hit = questions.iter().find(|q| question_matches(q, eq));
        if hit.is_some() {
            s.matched += 1;
        } else {
            s.misses
                .push(eq.content_contains_any.first().cloned().unwrap_or_default());
        }
    }
    tally_unmatched(
        &mut s,
        &questions,
        |q| q.content.clone(),
        |q| {
            golden
                .expected_question_atoms
                .iter()
                .any(|eq| question_matches(q, eq))
        },
    );
    s
}

fn question_matches(q: &Question, eq: &ExpectedQuestion) -> bool {
    let content_ok = matches_any(&q.content, &eq.content_contains_any);
    let status_ok = if eq.status_any.is_empty() {
        true
    } else {
        let q_status = match &q.resolution_status {
            corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Resolved { .. } => {
                "resolved"
            }
            corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Contested { .. } => {
                "contested"
            }
            corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Open => "open",
            corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Dissolved => "dissolved",
        };
        eq.status_any
            .iter()
            .any(|s| s.eq_ignore_ascii_case(q_status))
    };
    content_ok && status_ok
}

fn score_claim_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_claim_atoms.len();
    let claims = snap.claims();
    if snap.atoms.is_none() {
        s.notes
            .push("atoms.json not present — skipping claim scoring".to_string());
        return s;
    }
    for ec in &golden.expected_claim_atoms {
        let hit = claims.iter().find(|c| claim_matches(c, ec, snap));
        if hit.is_some() {
            s.matched += 1;
        } else {
            s.misses
                .push(ec.content_contains_any.first().cloned().unwrap_or_default());
        }
    }
    tally_unmatched(
        &mut s,
        &claims,
        |c| c.content.clone(),
        |c| {
            golden
                .expected_claim_atoms
                .iter()
                .any(|ec| claim_matches(c, ec, snap))
        },
    );
    s
}

fn claim_matches(
    c: &corpus_engine::enrichment::atlas::atoms::Claim,
    ec: &ExpectedClaim,
    snap: &AtlasSnapshot,
) -> bool {
    let content_ok = matches_any(&c.content, &ec.content_contains_any);
    let prop_ok = if ec.attributed_proponent_contains_any.is_empty() {
        true
    } else {
        match &c.attributed_to {
            None => false,
            Some(id) => snap
                .entity_match_strings_by_id(id)
                .iter()
                .any(|n| matches_any(n, &ec.attributed_proponent_contains_any)),
        }
    };
    content_ok && prop_ok
}

fn score_discourse_acts(golden: &GoldenSet, snap: &AtlasSnapshot) -> DiscourseActReport {
    let mut report = DiscourseActReport::default();
    let claims = snap.claims();
    report.total_claims = claims.len();
    if claims.is_empty() {
        report
            .notes
            .push("no Claim atoms present — skipping discourse-act distribution".to_string());
        report.required_satisfied = true;
        return report;
    }

    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for c in &claims {
        let key = c.discourse_act.as_str_repr().to_string();
        *counts.entry(key).or_insert(0) += 1;
    }
    report.act_counts = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    report.act_counts.sort_by(|a, b| b.1.cmp(&a.1));

    // Take the union across all distribution rules.
    let required: Vec<String> = golden
        .expected_discourse_act_distribution
        .iter()
        .flat_map(|d| d.required_acts_any.iter().cloned())
        .collect();
    report.required_satisfied =
        required.is_empty() || required.iter().any(|act| counts.contains_key(act.as_str()));

    for d in &golden.expected_discourse_act_distribution {
        if let Some(uniform) = &d.forbidden_uniform_act {
            // Violation: claims exist AND every claim has the
            // forbidden act AND there are ≥ 2 claims (a single
            // "assert"-tagged claim is not yet a uniformity signal).
            if claims.len() >= 2
                && claims
                    .iter()
                    .all(|c| c.discourse_act.as_str_repr() == uniform.as_str())
            {
                report.uniform_violation = Some(uniform.clone());
            }
        }
    }
    report
}

/// Resolve an edge endpoint to a keyword-matchable name.
///
/// The atlas pipeline's deterministic enumerator pairs Claim and State
/// atoms — not the position-typed Concept atoms the goldens name in
/// `*_contains_any`. Chase the endpoint:
///   - Claim → its `attributed_to` entity's canonical name
///   - State → its `entity_id`'s canonical name
///   - Entity → its own canonical name
///   - other → the `AtomId` string (which won't match a golden
///     keyword, so misses get reported honestly rather than hidden)
///
/// Without this chase every edge appears to the matcher as
/// "claim-NNNN ↔ state-MMMM", which never pairs against
/// `compatibilism`/`hard incompatibilism` keywords, and the eval reads
/// as zero even when the classifier produced solid edges.
///
/// Shared by [`score_fault_lines`] and [`score_edges`] — one resolver,
/// so the two axes can never disagree about what an endpoint is named.
fn resolve_endpoint_name(snap: &AtlasSnapshot, id: &AtomId) -> String {
    if let Some(name) = snap.entity_name_by_id(id) {
        return name.to_string();
    }
    if let Some(file) = snap.atoms.as_ref() {
        for atom in &file.atoms {
            match atom {
                AtomEnvelope::Claim(c) if c.id == *id => {
                    if let Some(attr) = &c.attributed_to {
                        if let Some(name) = snap.entity_name_by_id(attr) {
                            return name.to_string();
                        }
                    }
                    return id.as_str().to_string();
                }
                AtomEnvelope::State(st) if st.id == *id => {
                    if let Some(name) = snap.entity_name_by_id(&st.entity_id) {
                        return name.to_string();
                    }
                    return id.as_str().to_string();
                }
                _ => {}
            }
        }
    }
    id.as_str().to_string()
}

/// Parse a golden's `edge_type` string into an [`EdgeType`].
///
/// Deliberately routed through serde rather than a hand-written match:
/// [`EdgeType`] already carries `#[serde(rename_all = "PascalCase")]`,
/// and a second string→enum table here would be a second decider that
/// drifts the first time an edge type is added (ARCH_PRINCIPLES §10.6).
/// Returns `None` for `"*"` and for unrecognised tags; callers
/// distinguish the two.
fn parse_edge_type(s: &str) -> Option<EdgeType> {
    serde_json::from_value::<EdgeType>(serde_json::Value::String(s.to_string())).ok()
}

/// Score Phase 3b edges (P0.5 edge-F1).
///
/// Complements [`score_fault_lines`], which scores the `Tension` slice
/// of the same `edges.json` against *position pairs* and treats the
/// pair as unordered. This axis covers every edge type and is
/// DIRECTED: `Grounds(frankfurt case → compatibilism)` asserts
/// something its reverse does not.
fn score_edges(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_edges.len();
    s.forbidden_total = golden.forbidden_edges.len();

    let edges_file = match &snap.edges {
        Some(e) => e,
        None => {
            s.notes
                .push("edges.json not present — skipping edge scoring".to_string());
            return s;
        }
    };
    let edges: Vec<&Edge> = edges_file.edges.iter().collect();
    if edges.is_empty() {
        s.notes
            .push("edges.json contains 0 edges — Phase 3b may not have run".to_string());
    }

    // An unrecognised `edge_type` is a golden-authoring error, not a
    // model failure. Report it instead of letting the entry match
    // nothing and read as a recall miss (ARCH_PRINCIPLES §18.3 — a
    // check that cannot be evaluated is never silently a failure).
    let mut unknown_types: Vec<String> = Vec::new();
    let mut type_of = |tag: &str| -> Option<EdgeType> {
        if tag == "*" {
            return None;
        }
        match parse_edge_type(tag) {
            Some(t) => Some(t),
            None => {
                if !unknown_types.iter().any(|u| u == tag) {
                    unknown_types.push(tag.to_string());
                }
                None
            }
        }
    };

    // Directed endpoint match, with the type constraint applied only
    // when the golden names a real one.
    let matches_edge = |e: &Edge, want: Option<EdgeType>, src: &[String], tgt: &[String]| -> bool {
        if let Some(t) = want {
            if e.edge_type != t {
                return false;
            }
        }
        let a = resolve_endpoint_name(snap, &e.source);
        let b = resolve_endpoint_name(snap, &e.target);
        matches_any_with_morphology(&a, src) && matches_any_with_morphology(&b, tgt)
    };

    for ee in &golden.expected_edges {
        let want = type_of(&ee.edge_type);
        let hit = edges
            .iter()
            .any(|e| matches_edge(e, want, &ee.source_contains_any, &ee.target_contains_any));
        if hit {
            s.matched += 1;
        } else {
            let src = ee.source_contains_any.first().cloned().unwrap_or_default();
            let tgt = ee.target_contains_any.first().cloned().unwrap_or_default();
            s.misses.push(format!("{}({src} → {tgt})", ee.edge_type));
        }
    }

    let mut unevaluated_relation_kinds = 0usize;
    for fb in &golden.forbidden_edges {
        if fb.relation_kind.is_some() {
            unevaluated_relation_kinds += 1;
        }
        let want = type_of(&fb.edge_type);
        if edges
            .iter()
            .any(|e| matches_edge(e, want, &fb.source_contains_any, &fb.target_contains_any))
        {
            s.forbidden_hit += 1;
            let src = fb.source_contains_any.first().cloned().unwrap_or_default();
            let tgt = fb.target_contains_any.first().cloned().unwrap_or_default();
            s.forbidden_hits
                .push(format!("{}({src} → {tgt})", fb.edge_type));
        }
    }
    if unevaluated_relation_kinds > 0 {
        s.notes.push(format!(
            "{unevaluated_relation_kinds} forbidden edge(s) declare `relation_kind`, which the \
             edge model has no field for — matched on type + endpoints only, so the \
             relation_kind constraint was NOT checked"
        ));
    }
    if !unknown_types.is_empty() {
        s.notes.push(format!(
            "golden names {} unknown edge_type(s) ({}) — treated as \"*\" (any type); \
             fix the golden, these are not model misses",
            unknown_types.len(),
            unknown_types.join(", ")
        ));
    }

    let explained = |e: &Edge| -> bool {
        golden.expected_edges.iter().any(|ee| {
            matches_edge(
                e,
                parse_edge_type(&ee.edge_type),
                &ee.source_contains_any,
                &ee.target_contains_any,
            )
        }) || golden.forbidden_edges.iter().any(|fb| {
            matches_edge(
                e,
                parse_edge_type(&fb.edge_type),
                &fb.source_contains_any,
                &fb.target_contains_any,
            )
        })
    };
    tally_unmatched(
        &mut s,
        &edges,
        |e| {
            format!(
                "{:?}({} → {})",
                e.edge_type,
                resolve_endpoint_name(snap, &e.source),
                resolve_endpoint_name(snap, &e.target)
            )
        },
        |e| explained(e),
    );
    s
}

fn score_fault_lines(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_fault_lines.len();
    s.forbidden_total = golden.forbidden_fault_lines.len();

    let edges_file = match &snap.edges {
        Some(e) => e,
        None => {
            s.notes
                .push("edges.json not present — skipping fault-line scoring".to_string());
            return s;
        }
    };
    let tension_edges: Vec<&Edge> = edges_file
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Tension)
        .collect();
    if tension_edges.is_empty() {
        s.notes
            .push(format!(
                "edges.json contains 0 Tension edges (any of {} edges total) — Phase 6 may not have run",
                edges_file.edges.len()
            ));
    }

    let lookup_name = |id: &AtomId| resolve_endpoint_name(snap, id);

    // Match policy: position pair is the load-bearing signal.
    // `crux_keywords_any`, when specified, is informational — a
    // pair-correct tension whose sub_question paraphrases the
    // expected crux without using the listed keywords still counts
    // as a hit. Treating crux as a hard AND rejected real tensions
    // whose model-authored sub_question used different vocabulary
    // than the golden author chose (e.g. Darwin emitting "Can
    // reasons be one's own if they are causally determined?" against
    // a golden expecting "alternative" / "do otherwise" /
    // "ultimate source" — the tension is structurally correct, the
    // wording isn't on the keyword list).
    let mut crux_mismatches = 0usize;
    for ef in &golden.expected_fault_lines {
        let hit = tension_edges.iter().find(|e| {
            let a = lookup_name(&e.source);
            let b = lookup_name(&e.target);

            (matches_any_with_morphology(&a, &ef.position_a_contains_any)
                && matches_any_with_morphology(&b, &ef.position_b_contains_any))
                || (matches_any_with_morphology(&a, &ef.position_b_contains_any)
                    && matches_any_with_morphology(&b, &ef.position_a_contains_any))
        });
        match hit {
            Some(edge) => {
                s.matched += 1;
                let crux_text = edge.sub_question.as_deref().unwrap_or("");
                if !ef.crux_keywords_any.is_empty()
                    && !matches_any(crux_text, &ef.crux_keywords_any)
                {
                    crux_mismatches += 1;
                }
            }
            None => {
                let pa = ef
                    .position_a_contains_any
                    .first()
                    .cloned()
                    .unwrap_or_default();
                let pb = ef
                    .position_b_contains_any
                    .first()
                    .cloned()
                    .unwrap_or_default();
                s.misses.push(format!("{pa} vs {pb}"));
            }
        }
    }
    if crux_mismatches > 0 {
        s.notes.push(format!(
            "{crux_mismatches} hit(s) matched on position pair only — \
             golden's crux_keywords_any didn't appear in the model's sub_question"
        ));
    }
    for fb in &golden.forbidden_fault_lines {
        if tension_edges.iter().any(|e| {
            let a = lookup_name(&e.source);
            let b = lookup_name(&e.target);

            (matches_any_with_morphology(&a, &fb.position_a_contains_any)
                && matches_any_with_morphology(&b, &fb.position_b_contains_any))
                || (matches_any_with_morphology(&a, &fb.position_b_contains_any)
                    && matches_any_with_morphology(&b, &fb.position_a_contains_any))
        }) {
            s.forbidden_hit += 1;
            let pa = fb
                .position_a_contains_any
                .first()
                .cloned()
                .unwrap_or_default();
            let pb = fb
                .position_b_contains_any
                .first()
                .cloned()
                .unwrap_or_default();
            s.forbidden_hits.push(format!("{pa} vs {pb}"));
        }
    }
    let pair_explained = |a: &str, b: &str| -> bool {
        let expected_hit = golden.expected_fault_lines.iter().any(|ef| {
            (matches_any_with_morphology(a, &ef.position_a_contains_any)
                && matches_any_with_morphology(b, &ef.position_b_contains_any))
                || (matches_any_with_morphology(a, &ef.position_b_contains_any)
                    && matches_any_with_morphology(b, &ef.position_a_contains_any))
        });
        let forbidden_hit = golden.forbidden_fault_lines.iter().any(|fb| {
            (matches_any_with_morphology(a, &fb.position_a_contains_any)
                && matches_any_with_morphology(b, &fb.position_b_contains_any))
                || (matches_any_with_morphology(a, &fb.position_b_contains_any)
                    && matches_any_with_morphology(b, &fb.position_a_contains_any))
        });
        expected_hit || forbidden_hit
    };
    tally_unmatched(
        &mut s,
        &tension_edges,
        |e| format!("{} ↔ {}", lookup_name(&e.source), lookup_name(&e.target)),
        |e| pair_explained(&lookup_name(&e.source), &lookup_name(&e.target)),
    );
    s
}

fn score_open_questions(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_open_questions.len();
    let gaps_file = match &snap.gaps {
        Some(g) => g,
        None => {
            s.notes
                .push("gaps.json not present — skipping open-question scoring".to_string());
            return s;
        }
    };
    let open_qs: Vec<&Gap> = gaps_file
        .gaps
        .iter()
        .filter(|g| g.kind == GapKind::OpenQuestion)
        .collect();
    if open_qs.is_empty() {
        s.notes.push(format!(
            "gaps.json contains {} total gaps but 0 OpenQuestion entries",
            gaps_file.gaps.len()
        ));
    }
    // Some pipelines may carry the open-question text on Question
    // atoms with resolution_status: Open instead of duplicating it
    // into gaps.json. Fold those in so the eval is independent of
    // which storage layer the implementation chose.
    let open_question_atoms: Vec<&Question> = snap
        .questions()
        .into_iter()
        .filter(|q| {
            matches!(
                q.resolution_status,
                corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Open
            )
        })
        .collect();

    for eq in &golden.expected_open_questions {
        let from_gaps = open_qs
            .iter()
            .any(|g| matches_any(&g.description, &eq.content_contains_any));
        let from_atoms = open_question_atoms
            .iter()
            .any(|q| matches_any(&q.content, &eq.content_contains_any));
        if from_gaps || from_atoms {
            s.matched += 1;
        } else {
            s.misses
                .push(eq.content_contains_any.first().cloned().unwrap_or_default());
        }
    }
    // Candidate pool for volume accounting is the union of both
    // storage layers the matcher accepts (gap entries + Open-status
    // Question atoms), flattened to their display texts.
    let candidate_texts: Vec<String> = open_qs
        .iter()
        .map(|g| g.description.clone())
        .chain(open_question_atoms.iter().map(|q| q.content.clone()))
        .collect();
    tally_unmatched(
        &mut s,
        &candidate_texts,
        |t| t.clone(),
        |t| {
            golden
                .expected_open_questions
                .iter()
                .any(|eq| matches_any(t, &eq.content_contains_any))
        },
    );
    s
}

fn score_configurations(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
    let mut s = PhaseScore::default();
    s.expected = golden.expected_configurations.len();
    s.forbidden_total = golden.forbidden_configurations.len();

    // Configurations may be in either `configurations.json` (the
    // dedicated file Phase 8 writes) or inline in `atoms.json` as
    // `Configuration` envelopes. Eval against the union.
    let inline = snap.configurations_inline();
    let dedicated: Vec<&Configuration> = match &snap.configurations {
        Some(o) => o.configurations.iter().collect(),
        None => Vec::new(),
    };
    let all: Vec<&Configuration> = inline.iter().copied().chain(dedicated).collect();
    if snap.atoms.is_none() && snap.configurations.is_none() {
        s.notes
            .push("no atoms.json or configurations.json — skipping".to_string());
        return s;
    }

    for ec in &golden.expected_configurations {
        let hit = all.iter().find(|c| {
            matches_any(&c.label, &ec.label_contains_any)
                && matches_any(&c.description, &ec.description_keywords_any)
        });
        if hit.is_some() {
            s.matched += 1;
        } else {
            s.misses
                .push(ec.label_contains_any.first().cloned().unwrap_or_default());
        }
    }
    for fb in &golden.forbidden_configurations {
        if all
            .iter()
            .any(|c| matches_any(&c.label, &fb.name_contains_any))
        {
            s.forbidden_hit += 1;
            s.forbidden_hits
                .push(fb.name_contains_any.first().cloned().unwrap_or_default());
        }
    }
    tally_unmatched(
        &mut s,
        &all,
        |c| c.label.clone(),
        |c| {
            golden.expected_configurations.iter().any(|ec| {
                matches_any(&c.label, &ec.label_contains_any)
                    && matches_any(&c.description, &ec.description_keywords_any)
            }) || golden
                .forbidden_configurations
                .iter()
                .any(|fb| matches_any(&c.label, &fb.name_contains_any))
        },
    );
    s
}

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

// ── P0.2 adjudication surface ──────────────────────────────────────
//
// The volume counters above say HOW MUCH extraction goes unexplained;
// `bench enrichment-adjudicate` prices WHETHER it is junk. It needs
// the actual unmatched atoms (labels, descriptions, chunk evidence),
// not the capped sample strings — recomputed here with the exact
// predicates the scorers use, so the two surfaces cannot disagree.

/// One unmatched atom, carrying enough context for a judge verdict.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct UnmatchedAtom {
    /// Axis that considered the atom (most specific pool wins when an
    /// atom is a candidate in several — typed axes before generic).
    pub axis: String,
    /// Atom envelope kind (`Entity` / `Event` / `State` / ...).
    pub kind: String,
    pub label: String,
    /// Secondary text (description / framing); empty when the family
    /// has none.
    pub detail: String,
    pub evidence_chunk_ids: Vec<String>,
    pub evidence_previews: Vec<String>,
}

/// Resolve golden + atlas snapshot for a corpus the same way
/// `score_corpus` does — shared by `enrich eval` and the adjudicator.
pub(crate) fn load_golden_and_snapshot(
    corpus_id: &str,
    golden_path: &Path,
) -> Result<(GoldenSet, AtlasSnapshot), String> {
    EnrichConfig::require(corpus_id).map_err(|e| e.to_string())?;
    let golden = GoldenSet::load(golden_path)?;
    let atlas_dir = paths::index_root(corpus_id).join(ATLAS_DIRNAME);
    let skeleton_path = paths::index_root(corpus_id).join("field_skeleton.json");
    let snapshot = AtlasSnapshot::load(&atlas_dir, &skeleton_path)?;
    Ok((golden, snapshot))
}

fn chunkref_evidence(refs: &[ChunkRef]) -> (Vec<String>, Vec<String>) {
    let ids = refs.iter().map(|c| c.chunk_id.clone()).collect();
    let previews = refs
        .iter()
        .filter_map(|c| c.passage_preview.clone())
        .collect();
    (ids, previews)
}

fn axis_candidate_id(c: &AxisCandidate<'_>) -> String {
    match c {
        AxisCandidate::Entity(e) => e.id.as_str().to_string(),
        AxisCandidate::Claim(cl) => cl.id.as_str().to_string(),
        AxisCandidate::Position(p) => p.id.as_str().to_string(),
        AxisCandidate::Opposition(o) => o.id.as_str().to_string(),
    }
}

/// All atoms that (a) belong to at least one pool the golden scores
/// and (b) are explained by NO expected or forbidden entry in ANY
/// pool that considered them. "Explained anywhere = not junk-suspect"
/// is deliberate: adjudication prices junk, not per-axis bookkeeping,
/// so an atom the generic claim axis credits is excluded even when a
/// typed axis it also belongs to did not match it. Pool gating
/// mirrors `score()`: events/states/relations only when the golden
/// surfaces them; entity/question/claim/configuration always; typed
/// axes only when their golden axis is non-empty. Skeleton positions
/// and tension edges are not atoms and are out of scope here.
pub(crate) fn collect_unmatched_atoms(
    golden: &GoldenSet,
    snap: &AtlasSnapshot,
) -> Vec<UnmatchedAtom> {
    use std::collections::HashSet;

    let entity_axes = |g: &GoldenSet| {
        [
            (
                "person",
                EntityType::Person,
                g.expected_person_atoms.clone(),
                g.forbidden_person_atoms.clone(),
            ),
            (
                "concept",
                EntityType::Concept,
                g.expected_concept_atoms.clone(),
                g.forbidden_concept_atoms.clone(),
            ),
            (
                "work",
                EntityType::Work,
                g.expected_work_atoms.clone(),
                g.forbidden_work_atoms.clone(),
            ),
        ]
    };

    // Pass 1 — the global explained set, across every pool score()
    // would compute.
    let mut explained: HashSet<String> = HashSet::new();
    for (_, kind, expected, forbidden) in entity_axes(golden) {
        for e in entity_pool(snap, kind) {
            if entity_explained(e, &expected, &forbidden) {
                explained.insert(e.id.as_str().to_string());
            }
        }
    }
    if !golden.expected_event_atoms.is_empty() || !golden.forbidden_event_atoms.is_empty() {
        for e in snap.events() {
            if event_explained(e, golden, snap) {
                explained.insert(e.id.as_str().to_string());
            }
        }
    }
    if !golden.expected_state_atoms.is_empty() {
        for st in snap.states() {
            if golden
                .expected_state_atoms
                .iter()
                .any(|es| state_matches(st, es, snap))
            {
                explained.insert(st.id.as_str().to_string());
            }
        }
    }
    if !golden.expected_relation_atoms.is_empty() || !golden.forbidden_relation_atoms.is_empty() {
        for r in snap.relations() {
            let ok = golden
                .expected_relation_atoms
                .iter()
                .any(|er| relation_matches(r, er, snap))
                || golden
                    .forbidden_relation_atoms
                    .iter()
                    .any(|fb| relation_forbidden_hit(r, fb, snap));
            if ok {
                explained.insert(r.id.as_str().to_string());
            }
        }
    }
    for q in snap.questions() {
        if golden
            .expected_question_atoms
            .iter()
            .any(|eq| question_matches(q, eq))
        {
            explained.insert(q.id.as_str().to_string());
        }
    }
    for c in snap.claims() {
        if golden
            .expected_claim_atoms
            .iter()
            .any(|ec| claim_matches(c, ec, snap))
        {
            explained.insert(c.id.as_str().to_string());
        }
    }
    {
        let inline = snap.configurations_inline();
        let dedicated: Vec<&Configuration> = match &snap.configurations {
            Some(o) => o.configurations.iter().collect(),
            None => Vec::new(),
        };
        for c in inline.iter().copied().chain(dedicated) {
            let ok = golden.expected_configurations.iter().any(|ec| {
                matches_any(&c.label, &ec.label_contains_any)
                    && matches_any(&c.description, &ec.description_keywords_any)
            }) || golden
                .forbidden_configurations
                .iter()
                .any(|fb| matches_any(&c.label, &fb.name_contains_any));
            if ok {
                explained.insert(c.id.as_str().to_string());
            }
        }
    }
    for axis in all_axes() {
        let (expected, forbidden) = axis_expectations(axis, golden);
        if expected.is_empty() && forbidden.is_empty() {
            continue;
        }
        for c in collect_axis_atoms(axis, snap) {
            let ok = expected.iter().any(|exp| matches_axis(axis, &c, exp))
                || forbidden
                    .iter()
                    .any(|f| matches_any(c.primary_text(), f.name_contains_any));
            if ok {
                explained.insert(axis_candidate_id(&c));
            }
        }
    }

    // Pass 2 — emit every considered-but-unexplained atom once, most
    // specific axis first.
    let mut out: Vec<UnmatchedAtom> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut push = |id: String, atom: UnmatchedAtom, out: &mut Vec<UnmatchedAtom>| {
        if !explained.contains(&id) && emitted.insert(id) {
            out.push(atom);
        }
    };

    for axis in all_axes() {
        let (expected, forbidden) = axis_expectations(axis, golden);
        if expected.is_empty() && forbidden.is_empty() {
            continue;
        }
        for c in collect_axis_atoms(axis, snap) {
            let id = axis_candidate_id(&c);
            let atom = match c {
                AxisCandidate::Entity(e) => UnmatchedAtom {
                    axis: axis.key.to_string(),
                    kind: "Entity".into(),
                    label: e.canonical_name.clone(),
                    detail: e.description.clone(),
                    evidence_chunk_ids: vec![e.first_appearance.chunk_id.clone()],
                    evidence_previews: e
                        .first_appearance
                        .passage_preview
                        .clone()
                        .into_iter()
                        .collect(),
                },
                AxisCandidate::Claim(cl) => {
                    let (ids, previews) = chunkref_evidence(&cl.evidence);
                    UnmatchedAtom {
                        axis: axis.key.to_string(),
                        kind: "Claim".into(),
                        label: cl.content.clone(),
                        detail: cl.claim_kind.clone().unwrap_or_default(),
                        evidence_chunk_ids: ids,
                        evidence_previews: previews,
                    }
                }
                AxisCandidate::Position(p) => UnmatchedAtom {
                    axis: axis.key.to_string(),
                    kind: "Position".into(),
                    label: p.canonical_name.clone(),
                    detail: p.content.clone(),
                    evidence_chunk_ids: vec![p.first_appearance.chunk_id.clone()],
                    evidence_previews: p
                        .first_appearance
                        .passage_preview
                        .clone()
                        .into_iter()
                        .collect(),
                },
                AxisCandidate::Opposition(o) => UnmatchedAtom {
                    axis: axis.key.to_string(),
                    kind: "Opposition".into(),
                    label: format!("{} vs {}", o.left_label, o.right_label),
                    detail: o.axis.clone(),
                    evidence_chunk_ids: vec![o.first_appearance.chunk_id.clone()],
                    evidence_previews: o
                        .first_appearance
                        .passage_preview
                        .clone()
                        .into_iter()
                        .collect(),
                },
            };
            push(id, atom, &mut out);
        }
    }

    for (axis_name, kind, _expected, _forbidden) in entity_axes(golden) {
        for e in entity_pool(snap, kind) {
            let atom = UnmatchedAtom {
                axis: axis_name.to_string(),
                kind: "Entity".into(),
                label: e.canonical_name.clone(),
                detail: e.description.clone(),
                evidence_chunk_ids: vec![e.first_appearance.chunk_id.clone()],
                evidence_previews: e
                    .first_appearance
                    .passage_preview
                    .clone()
                    .into_iter()
                    .collect(),
            };
            push(e.id.as_str().to_string(), atom, &mut out);
        }
    }
    if !golden.expected_event_atoms.is_empty() || !golden.forbidden_event_atoms.is_empty() {
        for e in snap.events() {
            let (ids, previews) = chunkref_evidence(&e.evidence);
            let atom = UnmatchedAtom {
                axis: "event".into(),
                kind: "Event".into(),
                label: e.description.clone(),
                detail: String::new(),
                evidence_chunk_ids: ids,
                evidence_previews: previews,
            };
            push(e.id.as_str().to_string(), atom, &mut out);
        }
    }
    if !golden.expected_state_atoms.is_empty() {
        for st in snap.states() {
            let (ids, previews) = chunkref_evidence(&st.evidence);
            let ent = snap
                .entity_match_strings_by_id(&st.entity_id)
                .first()
                .map(|n| n.to_string())
                .unwrap_or_else(|| st.entity_id.as_str().to_string());
            let atom = UnmatchedAtom {
                axis: "state".into(),
                kind: "State".into(),
                label: format!("{ent}: {}", st.label),
                detail: String::new(),
                evidence_chunk_ids: ids,
                evidence_previews: previews,
            };
            push(st.id.as_str().to_string(), atom, &mut out);
        }
    }
    if !golden.expected_relation_atoms.is_empty() || !golden.forbidden_relation_atoms.is_empty() {
        for r in snap.relations() {
            let (ids, previews) = chunkref_evidence(&r.evidence);
            let names: Vec<String> = relation_name_sets(r, snap)
                .iter()
                .map(|ns| ns.first().cloned().unwrap_or_default())
                .collect();
            let atom = UnmatchedAtom {
                axis: "relation".into(),
                kind: "Relation".into(),
                label: format!("{} [{}]", r.label, names.join(" ↔ ")),
                detail: String::new(),
                evidence_chunk_ids: ids,
                evidence_previews: previews,
            };
            push(r.id.as_str().to_string(), atom, &mut out);
        }
    }
    for q in snap.questions() {
        let (ids, previews) = chunkref_evidence(&q.raised_at);
        let atom = UnmatchedAtom {
            axis: "question".into(),
            kind: "Question".into(),
            label: q.content.clone(),
            detail: String::new(),
            evidence_chunk_ids: ids,
            evidence_previews: previews,
        };
        push(q.id.as_str().to_string(), atom, &mut out);
    }
    for c in snap.claims() {
        let (ids, previews) = chunkref_evidence(&c.evidence);
        let atom = UnmatchedAtom {
            axis: "claim".into(),
            kind: "Claim".into(),
            label: c.content.clone(),
            detail: c.claim_kind.clone().unwrap_or_default(),
            evidence_chunk_ids: ids,
            evidence_previews: previews,
        };
        push(c.id.as_str().to_string(), atom, &mut out);
    }
    {
        let inline = snap.configurations_inline();
        let dedicated: Vec<&Configuration> = match &snap.configurations {
            Some(o) => o.configurations.iter().collect(),
            None => Vec::new(),
        };
        for c in inline.iter().copied().chain(dedicated) {
            let (ids, previews) = chunkref_evidence(&c.evidence);
            let atom = UnmatchedAtom {
                axis: "configuration".into(),
                kind: "Configuration".into(),
                label: c.label.clone(),
                detail: c.description.clone(),
                evidence_chunk_ids: ids,
                evidence_previews: previews,
            };
            push(c.id.as_str().to_string(), atom, &mut out);
        }
    }

    out
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
