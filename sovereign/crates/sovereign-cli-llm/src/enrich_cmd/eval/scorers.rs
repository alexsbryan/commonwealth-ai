// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-kind scorers, and the `score` dispatcher that fans out to them.
//!
//! Distinct from `axis_driver`: that module scores anything the axis CATALOG
//! can describe, generically. These are the kinds whose matching rules are
//! still hand-written — positions, events, states, relations, edges, fault
//! lines, open questions, configurations — each with its own notion of what
//! counts as the same thing.

// The eval surface is ONE cooperating unit split for size, not a set of
// independent modules: the golden schema, the snapshot, the match primitives
// and the scorers all name each other's types. `use super::*` keeps that one
// import surface in `mod.rs` rather than duplicating it eight ways.
use super::*;

pub(super) fn score(golden: &GoldenSet, snap: &AtlasSnapshot, phase: PhaseFilter) -> EvalReport {
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

pub(super) fn score_positions(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn position_matches(p: &SkeletonPosition, ep: &ExpectedPosition) -> bool {
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

pub(super) fn score_entity_atoms(
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
pub(super) fn entity_pool(snap: &AtlasSnapshot, kind: EntityType) -> Vec<&Entity> {
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
pub(super) fn entity_explained(
    e: &Entity,
    expected: &[ExpectedAtom],
    forbidden: &[ForbiddenName],
) -> bool {
    expected
        .iter()
        .any(|ee| matches_any(&e.canonical_name, &ee.canonical_name_contains_any))
        || forbidden
            .iter()
            .any(|fb| matches_any(&e.canonical_name, &fb.name_contains_any))
}

pub(super) fn score_event_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn event_matches(e: &Event, ee: &ExpectedEvent, snap: &AtlasSnapshot) -> bool {
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

pub(super) fn event_explained(e: &Event, golden: &GoldenSet, snap: &AtlasSnapshot) -> bool {
    golden
        .expected_event_atoms
        .iter()
        .any(|ee| event_matches(e, ee, snap))
        || golden
            .forbidden_event_atoms
            .iter()
            .any(|fb| matches_any(&e.description, &fb.name_contains_any))
}

pub(super) fn score_state_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn state_matches(st: &State, es: &ExpectedState, snap: &AtlasSnapshot) -> bool {
    let entity_ok = snap
        .entity_match_strings_by_id(&st.entity_id)
        .iter()
        .any(|n| matches_any(n, &es.entity_name_contains_any));
    let label_ok = matches_any(&st.label, &es.label_contains_any);
    entity_ok && label_ok
}

pub(super) fn score_relation_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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
pub(super) fn relation_name_sets(r: &Relation, snap: &AtlasSnapshot) -> Vec<Vec<String>> {
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

pub(super) fn relation_matches(r: &Relation, er: &ExpectedRelation, snap: &AtlasSnapshot) -> bool {
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

pub(super) fn relation_forbidden_hit(
    r: &Relation,
    fb: &ForbiddenName,
    snap: &AtlasSnapshot,
) -> bool {
    let label_hit = matches_any(&r.label, &fb.name_contains_any);
    let name_hit = relation_name_sets(r, snap)
        .iter()
        .any(|names| names.iter().any(|n| matches_any(n, &fb.name_contains_any)));
    label_hit || name_hit
}

pub(super) fn score_question_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn question_matches(q: &Question, eq: &ExpectedQuestion) -> bool {
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

pub(super) fn score_claim_atoms(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn claim_matches(
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

pub(super) fn score_discourse_acts(golden: &GoldenSet, snap: &AtlasSnapshot) -> DiscourseActReport {
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
pub(super) fn resolve_endpoint_name(snap: &AtlasSnapshot, id: &AtomId) -> String {
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
pub(super) fn parse_edge_type(s: &str) -> Option<EdgeType> {
    serde_json::from_value::<EdgeType>(serde_json::Value::String(s.to_string())).ok()
}

/// Score Phase 3b edges (P0.5 edge-F1).
///
/// Complements [`score_fault_lines`], which scores the `Tension` slice
/// of the same `edges.json` against *position pairs* and treats the
/// pair as unordered. This axis covers every edge type and is
/// DIRECTED: `Grounds(frankfurt case → compatibilism)` asserts
/// something its reverse does not.
pub(super) fn score_edges(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn score_fault_lines(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn score_open_questions(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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

pub(super) fn score_configurations(golden: &GoldenSet, snap: &AtlasSnapshot) -> PhaseScore {
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
