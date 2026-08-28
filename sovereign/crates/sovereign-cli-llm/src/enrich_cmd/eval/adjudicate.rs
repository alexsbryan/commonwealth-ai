// SPDX-License-Identifier: AGPL-3.0-or-later
//! The P0.2 adjudication surface — where a disputed score is recorded and
//! resolved by hand rather than by the matcher.

// The eval surface is ONE cooperating unit split for size, not a set of
// independent modules: the golden schema, the snapshot, the match primitives
// and the scorers all name each other's types. `use super::*` keeps that one
// import surface in `mod.rs` rather than duplicating it eight ways.
use super::*;

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

pub(super) fn chunkref_evidence(refs: &[ChunkRef]) -> (Vec<String>, Vec<String>) {
    let ids = refs.iter().map(|c| c.chunk_id.clone()).collect();
    let previews = refs
        .iter()
        .filter_map(|c| c.passage_preview.clone())
        .collect();
    (ids, previews)
}

pub(super) fn axis_candidate_id(c: &AxisCandidate<'_>) -> String {
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
