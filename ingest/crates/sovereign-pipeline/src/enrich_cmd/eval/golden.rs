// SPDX-License-Identifier: AGPL-3.0-or-later
//! The golden-set TOML schema — what a hand-authored expectation FILE says.
//!
//! Pure data (serde `Deserialize`) with no scoring in it: this module declares
//! the shape of the bench input, and `scorers` decides what a match means.
//! Fields are `pub(super)` because the scorers read them across the module
//! boundary; the type is a DTO, and that is the whole of its contract.

// The eval surface is ONE cooperating unit split for size, not a set of
// independent modules: the golden schema, the snapshot, the match primitives
// and the scorers all name each other's types. `use super::*` keeps that one
// import surface in `mod.rs` rather than duplicating it eight ways.
use super::*;

// ── Golden-set TOML schema ─────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GoldenSet {
    #[allow(dead_code)]
    #[serde(default)]
    pub(super) meta: GoldenMeta,
    #[serde(default)]
    pub(super) expected_positions: Vec<ExpectedPosition>,
    #[serde(default)]
    pub(super) forbidden_positions: Vec<ForbiddenName>,
    #[serde(default)]
    pub(super) expected_person_atoms: Vec<ExpectedAtom>,
    #[serde(default)]
    pub(super) forbidden_person_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    pub(super) expected_concept_atoms: Vec<ExpectedAtom>,
    #[serde(default)]
    pub(super) forbidden_concept_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    pub(super) expected_work_atoms: Vec<ExpectedAtom>,
    #[serde(default)]
    pub(super) forbidden_work_atoms: Vec<ForbiddenName>,
    // Literary atom kinds — used by literary_atlas goldens. Philosophy
    // goldens omit these; they're optional and score `None` when absent.
    #[serde(default)]
    pub(super) expected_event_atoms: Vec<ExpectedEvent>,
    #[serde(default)]
    pub(super) forbidden_event_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    pub(super) expected_state_atoms: Vec<ExpectedState>,
    #[serde(default)]
    pub(super) expected_relation_atoms: Vec<ExpectedRelation>,
    #[serde(default)]
    pub(super) forbidden_relation_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    pub(super) expected_question_atoms: Vec<ExpectedQuestion>,
    #[serde(default)]
    pub(super) expected_claim_atoms: Vec<ExpectedClaim>,
    #[serde(default)]
    pub(super) expected_discourse_act_distribution: Vec<DiscourseActDistribution>,
    #[serde(default)]
    pub(super) expected_fault_lines: Vec<ExpectedFaultLine>,
    #[serde(default)]
    pub(super) forbidden_fault_lines: Vec<ForbiddenFaultLine>,
    #[serde(default)]
    pub(super) expected_open_questions: Vec<ExpectedOpenQuestion>,
    #[serde(default)]
    pub(super) expected_configurations: Vec<ExpectedConfiguration>,
    #[serde(default)]
    pub(super) forbidden_configurations: Vec<ForbiddenName>,
    // Phase 3b edges, scored under `PhaseFilter::Edges` against
    // `edges.json` across ALL edge types. `score_fault_lines` scores
    // the same file but only its `Tension` slice against position
    // pairs; this axis is the general one (`Grounds`, `Causes`,
    // `EvidenceFor`, …) and shares one endpoint resolver with it.
    #[serde(default)]
    pub(super) expected_edges: Vec<ExpectedEdge>,
    #[serde(default)]
    pub(super) forbidden_edges: Vec<ForbiddenEdge>,

    // ─── v2 typed-extension axes (Argumentative discourse mode) ───
    //
    // Scored against `Phase1Output.questions_by_chapter[*].
    // section_extraction.{type_extension, type_extensions}` per
    // `AtlasSnapshot::argumentative_*`. All axes are optional; an
    // empty array means "no signal on this axis", not "expected
    // zero". Goldens for non-argumentative corpora can omit them
    // entirely.
    #[serde(default)]
    pub(super) expected_mechanism_atoms: Vec<ExpectedMechanism>,
    #[serde(default)]
    pub(super) forbidden_mechanism_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    pub(super) expected_named_position_atoms: Vec<ExpectedNamedPosition>,
    #[serde(default)]
    pub(super) forbidden_named_position_atoms: Vec<ForbiddenName>,
    #[serde(default)]
    pub(super) expected_evidence_atoms: Vec<ExpectedEvidence>,
    #[serde(default)]
    pub(super) expected_opposition_atoms: Vec<ExpectedOpposition>,
    #[serde(default)]
    pub(super) expected_concession_atoms: Vec<ExpectedConcession>,
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
    pub(super) template: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) description: String,
    /// Corpus this golden scores against. Authoritative when present;
    /// `bench all` discovery falls back to the filename stem when
    /// absent and warn-logs the inference.
    #[serde(default)]
    pub corpus_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedPosition {
    pub(super) name_contains_any: Vec<String>,
    #[serde(default)]
    pub(super) epistemic_status: Option<String>,
    #[serde(default)]
    pub(super) proponents_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ForbiddenName {
    #[serde(alias = "canonical_name_contains_any")]
    #[serde(alias = "label_contains_any")]
    pub(super) name_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedAtom {
    #[serde(alias = "name_contains_any")]
    pub(super) canonical_name_contains_any: Vec<String>,
    #[serde(default)]
    pub(super) description_keywords_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedEvent {
    /// Substrings the event description must contain. Match policy is
    /// "any" so a golden can list paraphrases ("dies in the woods" /
    /// "death in the woods" / "tragic death") without requiring an
    /// exact phrasing match.
    pub(super) description_contains_any: Vec<String>,
    /// When non-empty, ANY listed name must appear among the event's
    /// participant entities (resolved via entity_name_by_id). Useful
    /// for asserting "Fyodor Pavlovitch's death involves him as a
    /// participant", catching the failure mode where the event is
    /// extracted but the participants are stripped.
    #[serde(default)]
    pub(super) participants_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedState {
    /// Substrings the state's `entity_id`-resolved name must contain.
    /// E.g. for "Eveline's paralysis at the dock" the entity is
    /// Eveline.
    pub(super) entity_name_contains_any: Vec<String>,
    /// Substrings the state's `label` must contain.
    pub(super) label_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedRelation {
    /// Each entry is the set of names (any-match) one participant must
    /// match. Two entries → asserts a pair where one matches A and the
    /// other matches B (in either order). One entry → asserts at least
    /// one participant matches that set, regardless of partner.
    pub(super) participants_a_any: Vec<String>,
    #[serde(default)]
    pub(super) participants_b_any: Vec<String>,
    /// Substrings the relation's `label` must contain.
    #[serde(default)]
    pub(super) label_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedQuestion {
    pub(super) content_contains_any: Vec<String>,
    #[serde(default)]
    pub(super) status_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedClaim {
    pub(super) content_contains_any: Vec<String>,
    #[serde(default)]
    pub(super) attributed_proponent_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

// ─── Argumentative typed-extension expectations ───────────────────
//
// Match policy mirrors `ExpectedAtom`: `*_contains_any` is a
// case-insensitive substring against the named field; ANY listed
// substring satisfies the match. Discriminator fields (`stance`,
// `kind`, `outcome`) require exact match against the canonical
// snake_case enum literal when supplied — left `None` to skip.

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedMechanism {
    /// Substrings against the mechanism's `name`. Load-bearing
    /// signal: a mechanism atom is the named lever the section's
    /// argument turns on. Mirrors `canonical_name_contains_any` on
    /// `ExpectedAtom` to keep the golden's verb shape consistent.
    #[serde(alias = "canonical_name_contains_any")]
    pub(super) name_contains_any: Vec<String>,
    /// Optional substrings against the mechanism's `description`.
    /// Informational — a name-only hit still counts as a match;
    /// description divergence surfaces in `name_only_hits`.
    #[serde(default)]
    pub(super) description_keywords_any: Vec<String>,
    /// Optional substrings against the mechanism's `domain` tag
    /// (`economics`, `urbanism`, `music`, ...). When non-empty,
    /// ANY listed substring satisfies; empty = no domain filter.
    #[serde(default)]
    pub(super) domain_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedNamedPosition {
    /// Substrings against the position's `name` ("the
    /// rent-concentration thesis"; "Hardin's tragedy framing").
    /// Distinct from `ExpectedPosition` (Phase 1 skeleton) — this
    /// scores the v2 typed-extension Position sketch.
    #[serde(alias = "canonical_name_contains_any")]
    pub(super) name_contains_any: Vec<String>,
    /// Optional substrings against the position's `content` (the
    /// one-sentence statement). Informational; absence does not
    /// reduce recall.
    #[serde(default)]
    pub(super) content_contains_any: Vec<String>,
    /// Optional substrings against the position's `proponent` field
    /// (entity name attribution). Empty = no proponent filter.
    #[serde(default)]
    pub(super) proponent_contains_any: Vec<String>,
    /// Optional exact-match on the position's `stance`
    /// (`endorse` | `rebut` | `survey` | `mixed`). None = skip.
    #[serde(default)]
    pub(super) stance: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedEvidence {
    /// Substrings against the evidence atom's `label` (e.g.
    /// `"$1.4B FTC PBM spread"`, `"Soviet Aral Sea counter-example"`).
    pub(super) label_contains_any: Vec<String>,
    /// Optional substrings against the evidence atom's `content`
    /// (the one-sentence statement of what the evidence is).
    #[serde(default)]
    pub(super) content_contains_any: Vec<String>,
    /// Optional exact-match on the evidence atom's `kind`
    /// (`study` | `figure` | `historical_example` | `case_study` |
    /// `personal_anecdote` | `quotation` | `other`). None = skip.
    #[serde(default)]
    pub(super) kind: Option<String>,
    /// Optional substrings against the evidence atom's `supports`
    /// field (the claim/position the evidence is invoked to back).
    /// Empty = no supports filter.
    #[serde(default)]
    pub(super) supports_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedOpposition {
    /// Substrings against the opposition's `left` label.
    pub(super) left_contains_any: Vec<String>,
    /// Substrings against the opposition's `right` label.
    pub(super) right_contains_any: Vec<String>,
    /// Optional substrings against the opposition's `axis` (the
    /// dimension along which they differ). Empty = no axis filter.
    #[serde(default)]
    pub(super) axis_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedConcession {
    /// Substrings against the concession's `content` (the
    /// one-sentence statement of what the author concedes).
    pub(super) content_contains_any: Vec<String>,
    /// Optional substrings against the concession's `addresses`
    /// field (the position or claim the concession addresses).
    /// Empty = no addresses filter.
    #[serde(default)]
    pub(super) addresses_contains_any: Vec<String>,
    /// Optional exact-match on the concession's `outcome`
    /// (`intact` | `narrowed` | `retracted`). None = skip.
    #[serde(default)]
    pub(super) outcome: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct DiscourseActDistribution {
    pub(super) required_acts_any: Vec<String>,
    #[serde(default)]
    pub(super) forbidden_uniform_act: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedFaultLine {
    pub(super) position_a_contains_any: Vec<String>,
    pub(super) position_b_contains_any: Vec<String>,
    #[serde(default)]
    pub(super) crux_keywords_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ForbiddenFaultLine {
    pub(super) position_a_contains_any: Vec<String>,
    pub(super) position_b_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) reason: Option<String>,
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
pub(super) struct ExpectedEdge {
    #[serde(default = "any_edge_type")]
    pub(super) edge_type: String,
    pub(super) source_contains_any: Vec<String>,
    pub(super) target_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

/// An edge the golden asserts must NOT exist — the anti-test half of
/// the axis. Same matching rules as [`ExpectedEdge`].
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ForbiddenEdge {
    #[serde(default = "any_edge_type")]
    pub(super) edge_type: String,
    pub(super) source_contains_any: Vec<String>,
    pub(super) target_contains_any: Vec<String>,
    /// Author's intent tag (e.g. `"proponent_of"`). The edge model has
    /// no such field, so this is NOT evaluated — `score_edges` reports
    /// the fact in its notes rather than silently matching on the
    /// remaining criteria as though the constraint had been checked.
    #[serde(default)]
    pub(super) relation_kind: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) reason: Option<String>,
}

/// Wildcard for a golden that constrains endpoints but not type.
pub(super) fn any_edge_type() -> String {
    "*".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedOpenQuestion {
    pub(super) content_contains_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ExpectedConfiguration {
    pub(super) label_contains_any: Vec<String>,
    #[serde(default)]
    pub(super) description_keywords_any: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) note: Option<String>,
}
