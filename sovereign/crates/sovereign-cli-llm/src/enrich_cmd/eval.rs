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
    AtomEnvelope, AtomId, AtomsFile, Configuration, Entity, Event, Opposition, Position, Question,
    Relation, State,
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
                "Restrict scoring to one phase. Default: all. Phases: positions (Phase 1 skeleton), atoms (Phase 3a/3b entities + concepts + questions + claims), fault-lines (Phase 6 Tension edges), gaps (Phase 7 open questions), configurations (Phase 8).",
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
            "fault-lines" | "fault_lines" | "tensions" => Ok(Self::FaultLines),
            "gaps" | "open-questions" | "open_questions" => Ok(Self::Gaps),
            "configurations" | "config" => Ok(Self::Configurations),
            other => Err(format!(
                "unknown --phase: {other:?} (allowed: positions, atoms, fault-lines, gaps, configurations, all)"
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
struct GoldenSet {
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
    // `expected_edges` and `forbidden_edges` — accepted in the TOML
    // for forward compatibility with future scoring; not yet wired
    // into the report. The fault-line section already covers the
    // load-bearing edge case (Tension edges between positions).
    #[serde(default)]
    #[allow(dead_code)]
    expected_edges: Vec<toml::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    forbidden_edges: Vec<toml::Value>,

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
    fn load(path: &Path) -> Result<Self, String> {
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
struct AtlasSnapshot {
    skeleton: Option<FieldSkeleton>,
    atoms: Option<AtomsFile>,
    edges: Option<EdgesFile>,
    gaps: Option<GapsOutput>,
    configurations: Option<ConfigurationsOutput>,
}

impl AtlasSnapshot {
    fn load(atlas_dir: &Path, skeleton_path: &Path) -> Result<Self, String> {
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
        let hit = positions.iter().find(|p| {
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
        });
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
    s
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

    // Expected matches accept the requested type OR an `Other`
    // variant (the catch-all for type strings the schema doesn't
    // name — most commonly "unspecified" or "unknown"). The model
    // frequently hedges typing on borderline cases (e.g. emitting
    // "Mangan's sister" or "the narrator" with entity_type:
    // unspecified rather than Person). Penalising hedges as zero
    // recall conflates "model couldn't classify the type" with
    // "model didn't surface the entity at all". The first is a
    // quality concern; the second is a hard miss. Treating Other(_)
    // as a fallback recovers recall on the hard miss; the
    // `description_keywords_any` note still flags lower-quality hits.
    let typed: Vec<&Entity> = snap.entities_of_type(kind);
    let untyped: Vec<&Entity> = snap
        .all_entities()
        .into_iter()
        .filter(|e| {
            matches!(e.entity_type, EntityType::Other(_)) && !typed.iter().any(|t| t.id == e.id)
        })
        .collect();
    let entities: Vec<&Entity> = typed
        .iter()
        .copied()
        .chain(untyped.iter().copied())
        .collect();
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
    s
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
        let hit = events.iter().find(|e| {
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
        });
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
    s
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
        let hit = states.iter().find(|st| {
            let entity_ok = snap
                .entity_match_strings_by_id(&st.entity_id)
                .iter()
                .any(|n| matches_any(n, &es.entity_name_contains_any));
            let label_ok = matches_any(&st.label, &es.label_contains_any);
            entity_ok && label_ok
        });
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
    s
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

    // Per-participant name set (canonical + aliases). A relation
    // pair-match accepts a hit on any of an entity's known names so a
    // golden listing "Alyosha" credits a relation involving entity
    // "Alexey Fyodorovich Karamazov".
    let participant_name_sets = |r: &Relation| -> Vec<Vec<String>> {
        r.participants
            .iter()
            .map(|pid| {
                snap.entity_match_strings_by_id(pid)
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .collect()
    };

    for er in &golden.expected_relation_atoms {
        let hit = relations.iter().find(|r| {
            let name_sets = participant_name_sets(r);
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
        });
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
        if relations.iter().any(|r| {
            let label_hit = matches_any(&r.label, &fb.name_contains_any);
            let name_hit = participant_name_sets(r)
                .iter()
                .any(|names| names.iter().any(|n| matches_any(n, &fb.name_contains_any)));
            label_hit || name_hit
        }) {
            s.forbidden_hit += 1;
            s.forbidden_hits
                .push(fb.name_contains_any.first().cloned().unwrap_or_default());
        }
    }
    s
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
        let hit = questions.iter().find(|q| {
            let content_ok = matches_any(&q.content, &eq.content_contains_any);
            let status_ok = if eq.status_any.is_empty() {
                true
            } else {
                let q_status = match &q.resolution_status {
                    corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Resolved {
                        ..
                    } => "resolved",
                    corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Contested {
                        ..
                    } => "contested",
                    corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Open => "open",
                    corpus_engine::enrichment::atlas::atoms::ResolutionStatus::Dissolved => {
                        "dissolved"
                    }
                };
                eq.status_any
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(q_status))
            };
            content_ok && status_ok
        });
        if hit.is_some() {
            s.matched += 1;
        } else {
            s.misses
                .push(eq.content_contains_any.first().cloned().unwrap_or_default());
        }
    }
    s
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
        let hit = claims.iter().find(|c| {
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
        });
        if hit.is_some() {
            s.matched += 1;
        } else {
            s.misses
                .push(ec.content_contains_any.first().cloned().unwrap_or_default());
        }
    }
    s
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

    // Resolve a Tension-edge endpoint to a position-readable name.
    // The atlas pipeline's deterministic enumerator pairs Claim and
    // State atoms (not the position-typed Concept atoms the goldens
    // name in `position_a_contains_any`). Chase the endpoint:
    //   - Claim → its `attributed_to` entity's canonical name
    //   - State → its `entity_id`'s canonical name
    //   - Entity → its own canonical name
    //   - other → the AtomId string (which won't match a position
    //     keyword, so misses get reported honestly)
    //
    // Without this chase, every accepted Tension edge appears as
    // "claim-NNNN ↔ state-MMMM" to the matcher, which will never
    // pair against `compatibilism`/`hard incompatibilism` keywords,
    // and the eval reads as 0 fault lines even when the classifier
    // produced solid edges.
    let resolve_endpoint = |id: &AtomId| -> String {
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
    };
    let lookup_name = resolve_endpoint;

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
    println!(
        "  {label:<22}  {matched:>3}/{exp:<3}    P {p}   R {r}   F1 {f}    FP {fp}/{ft}",
        matched = s.matched,
        exp = s.expected,
        fp = s.forbidden_hit,
        ft = s.forbidden_total,
    );
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
}
