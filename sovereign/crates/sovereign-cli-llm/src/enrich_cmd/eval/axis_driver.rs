// SPDX-License-Identifier: AGPL-3.0-or-later
//! The catalog-driven axis scorer — ONE driver in place of five hand-coded
//! `score_{mechanism,named_position,evidence,opposition,concession}_atoms`.
//!
//! Adding a typed axis to the bench is a catalog entry plus a golden block,
//! not a new Rust file: `gating_fields` / `atom_shape` from
//! `corpus_engine::…::axis_catalog` drive both the candidate pool
//! (`collect_axis_atoms`) and the gating predicate (`matches_axis`).

// The eval surface is ONE cooperating unit split for size, not a set of
// independent modules: the golden schema, the snapshot, the match primitives
// and the scorers all name each other's types. `use super::*` keeps that one
// import surface in `mod.rs` rather than duplicating it eight ways.
use super::*;

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
// `atom_shape` declaration drives `collect_axis_atoms` (candidate
// pool) and `matches_axis` (gating predicate).

/// Per-expectation row in the uniform view the driver consumes.
/// Built lazily from `GoldenSet.expected_*_atoms` named fields at
/// score time so the on-disk TOML schema doesn't change.
pub(super) struct AxisExpectation<'a> {
    /// Primary-name needle list. Used by `GatingField::Name`.
    pub(super) name_contains_any: &'a [String],
    /// Position stance gate (`endorse` / `rebut` / ...). None = skip.
    pub(super) stance: Option<&'a str>,
    /// Kind-discriminator gate. `EntityWithConceptKind` /
    /// `ClaimWithKind` axes filter candidates by qualifier in
    /// `collect_axis_atoms`; the field is populated for future
    /// catalog axes whose collector cannot pre-filter (e.g. cross-
    /// kind matching), and stays here so the uniform view shape
    /// doesn't grow another variant later.
    #[allow(dead_code)]
    pub(super) kind: Option<&'a str>,
    /// Opposition left/right gates (order-independent).
    pub(super) left_contains_any: &'a [String],
    pub(super) right_contains_any: &'a [String],

    // ─── Informational fields (mismatch → PhaseScore.note, not miss)
    pub(super) description_keywords_any: &'a [String],
    pub(super) domain_contains_any: &'a [String],
    pub(super) content_contains_any: &'a [String],
    pub(super) proponent_contains_any: &'a [String],
    pub(super) supports_contains_any: &'a [String],
    pub(super) axis_contains_any: &'a [String],
    pub(super) addresses_contains_any: &'a [String],
    pub(super) outcome: Option<&'a str>,
}

impl<'a> AxisExpectation<'a> {
    pub(super) fn empty() -> Self {
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
    pub(super) fn miss_label(&self) -> String {
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
pub(super) struct AxisForbidden<'a> {
    pub(super) name_contains_any: &'a [String],
}

impl<'a> AxisForbidden<'a> {
    pub(super) fn label(&self) -> String {
        self.name_contains_any.first().cloned().unwrap_or_default()
    }
}

/// Candidate atom enum — uniform over Entity / Claim / Position /
/// Opposition so the matcher doesn't need a per-axis branch on
/// candidate shape.
pub(super) enum AxisCandidate<'a> {
    Entity(&'a Entity),
    Claim(&'a corpus_engine::enrichment::atlas::atoms::Claim),
    Position(&'a Position),
    Opposition(&'a Opposition),
}

impl<'a> AxisCandidate<'a> {
    pub(super) fn primary_text(&self) -> &str {
        match self {
            AxisCandidate::Entity(e) => &e.canonical_name,
            AxisCandidate::Claim(c) => &c.content,
            AxisCandidate::Position(p) => &p.canonical_name,
            // Opposition primary text used only for forbidden-block
            // name matching, which we don't expose for Opposition v1.
            AxisCandidate::Opposition(o) => &o.axis,
        }
    }

    pub(super) fn stance(&self) -> Option<&str> {
        if let AxisCandidate::Position(p) = self {
            Some(&p.stance)
        } else {
            None
        }
    }

    pub(super) fn opposition_labels(&self) -> Option<(&str, &str)> {
        if let AxisCandidate::Opposition(o) = self {
            Some((&o.left_label, &o.right_label))
        } else {
            None
        }
    }

    pub(super) fn description(&self) -> Option<&str> {
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
    pub(super) fn proponent_name(&self, snap: &AtlasSnapshot) -> Option<String> {
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
/// the catalog's `AxisAtomShape` is `EntityWithConceptKind` /
/// `ClaimWithKind`. Returns an empty Vec when atoms.json is absent.
pub(super) fn collect_axis_atoms<'a>(
    axis: &TypedAxis,
    snap: &'a AtlasSnapshot,
) -> Vec<AxisCandidate<'a>> {
    let Some(file) = snap.atoms.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for atom in &file.atoms {
        let candidate = match (axis.atom_shape, atom) {
            (AxisAtomShape::EntityWithConceptKind(tag), AtomEnvelope::Entity(e))
                if e.concept_kind.as_deref() == Some(tag) =>
            {
                Some(AxisCandidate::Entity(e))
            }
            (AxisAtomShape::ClaimWithKind(tag), AtomEnvelope::Claim(c))
                if c.claim_kind.as_deref() == Some(tag) =>
            {
                Some(AxisCandidate::Claim(c))
            }
            (AxisAtomShape::Entity, AtomEnvelope::Entity(e)) => Some(AxisCandidate::Entity(e)),
            (AxisAtomShape::Claim, AtomEnvelope::Claim(c)) => Some(AxisCandidate::Claim(c)),
            (AxisAtomShape::Position, AtomEnvelope::Position(p)) => {
                Some(AxisCandidate::Position(p))
            }
            (AxisAtomShape::Opposition, AtomEnvelope::Opposition(o)) => {
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
pub(super) fn axis_expectations<'a>(
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
pub(super) fn matches_axis(
    axis: &TypedAxis,
    candidate: &AxisCandidate,
    expect: &AxisExpectation,
) -> bool {
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
pub(super) fn emit_informational_notes(
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
pub(super) fn score_axis(
    axis: &TypedAxis,
    golden: &GoldenSet,
    snap: &AtlasSnapshot,
) -> Option<PhaseScore> {
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
