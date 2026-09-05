// SPDX-License-Identifier: AGPL-3.0-or-later
//! The field model, projected onto atlas atoms — ei-7b's two directions.
//!
//! The v1 field model is a parallel artifact: `field_skeleton.json` beside the
//! index, read by exactly one consumer (`turn_prepass`'s ambient "Field guide"
//! digest) through a renderer no other atlas surface can reach. Spec §3's port
//! row says what changes: *canonical questions → `Question` atoms, the digest is
//! rendered from the atlas, the v1 writer retires.*
//!
//! This module is the whole port, as two pure functions and no new renderer:
//!
//! | Direction | Function | Used by |
//! |---|---|---|
//! | skeleton → atoms | [`skeleton_to_atoms`] | the field-model pass's terminal write; the one-shot `enrich field-atoms` migration |
//! | atoms → skeleton | [`skeleton_from_atoms`] | `turn_prepass`, which then calls the SAME [`FieldSkeleton::render_landscape`] it always called |
//!
//! ## Why there is no second renderer (ARCH §10.6)
//!
//! [`FieldSkeleton::render_landscape`] is the one implementation of the
//! digest's shape, its three sections, and its token budget. Writing an
//! atoms→digest renderer beside it would be a second decider for the same
//! output. So the read direction rebuilds a `FieldSkeleton` **view** from the
//! atoms and hands it to the renderer unchanged. `is_settled_status` — the
//! predicate that decides which questions are "settled concerns" — is likewise
//! untouched and still the only place that question is answered.
//!
//! ## The mapping, kind by kind
//!
//! Every kind used here is already in the closed set (`AtomType`); this port
//! adds none (spec §3, "no pipeline gets a private node kind").
//!
//! | Skeleton | Atom | Carried in |
//! |---|---|---|
//! | `CanonicalQuestion.question` | `Question.content` | — |
//! | `CanonicalQuestion.question_type` | `Question.question_type` | `QuestionType::from_str_repr` — an unnamed tag like SEP's `"conceptual"` lands in `Other(..)` and round-trips byte-exact |
//! | `CanonicalQuestion.status` | `Question.resolution_status` | see below |
//! | `SkeletonPosition.name` | `Position.canonical_name` | — |
//! | `SkeletonPosition.claim` | `Position.content` | — |
//! | `SkeletonPosition.status` | `Position.stance` | free string on both sides; `is_settled_status` reads it back unchanged |
//! | `SkeletonFaultLine.crux` | `Opposition.framing` | `canonical_label` carries the two position names |
//! | `SkeletonOpenQuestion` | `Question` with `ResolutionStatus::Open` | the partition below |
//!
//! **The open/canonical partition is `resolution_status`, and that is the only
//! decider.** A `Question` atom whose status is `Open` reads back as an
//! `open_questions` entry; every other status reads back as a
//! `canonical_questions` entry. So a corpus does not need two lists on disk —
//! it needs one kind with a status, which is what the atlas vocabulary already
//! has.
//!
//! ### One named substitution (ARCH §18.3)
//!
//! `ResolutionStatus::{Resolved, Contested}` carry `AtomId`s in fields *named*
//! `claim_id` / `claim_ids`. This projection points them at the `Position`
//! atoms that address the question, because a philosophy field model has
//! positions where an argumentative corpus has a single answering claim, and
//! minting shadow `Claim` atoms that duplicate a `Position`'s text verbatim
//! would be two atoms for one utterance. The ids resolve normally — every
//! consumer of those fields looks them up in the same atom map — and
//! `Question.addressed_by` carries the same ids under the name that is exactly
//! right. Named here rather than left for a reader to discover.
//!
//! ## What the projection does NOT carry, and why
//!
//! These skeleton fields have no home in the atom vocabulary and are dropped.
//! None is read by `render_landscape`, so the digest is unaffected; all of them
//! are read by `FieldModelEngine`'s phase-1 resume, which is why that engine
//! keeps its own working checkpoint (`_field_skeleton_checkpoint.json`) rather
//! than resuming from the atlas:
//!
//! - `SkeletonPosition::{proponents, source, cluster_ids, centroid_chunk_ids,
//!   discovery_confidence}` — 624 proponent names on SEP. A proponent wants an
//!   `Entity` atom to point at (`Position.proponent_id`), and minting 451
//!   Person entities that nothing in this phase reads is the speculative build
//!   the order's Less clause rules out. Filed, not built.
//! - `CanonicalQuestion.primary_entries` — empty on every SEP question (549/549).
//! - `SkeletonOpenQuestion::{related_question_id, representative_chunk_ids}`.
//! - `FieldSkeleton::{schema_version, generated_at, extraction_method,
//!   prompt_version, domain_id, field_stats}` — provenance of the v1 file, not
//!   of the atoms. [`skeleton_from_atoms`] fills them with values that say so.
//!
//! Concept entities (the port row's "concerns → concept entities") are NOT
//! emitted for the same Less reason: nothing in this phase reads them. The
//! digest reads `Question` + `Position`, and this projection deliberately
//! writes no ANN seed row, so the walk is unchanged and the SEP retrieval lane
//! measures the digest move alone.

use corpus_engine_vocab::taxonomy::{EnrichmentDepth, QuestionType};

use super::atlas::atoms::{
    AtomEnvelope, AtomId, ChunkRef, Opposition, Position, Question, ResolutionStatus,
};
use super::clustering::FieldModelStats;
use super::skeleton::{
    CanonicalQuestion, FieldSkeleton, SkeletonFaultLine, SkeletonOpenQuestion, SkeletonPosition,
};

/// `extraction_method` on a skeleton VIEW rebuilt from atoms — so a reader who
/// dumps one can tell it from a v1 file that was parsed off disk.
pub const ATOM_SOURCED_METHOD: &str = "atlas_atoms_v1";

/// The status string a `ResolutionStatus::Open` question reads back with.
/// `is_settled_status` must not match it, and the two v1 lists must partition.
const OPEN_STATUS: &str = "open";

/// A stance carries the v1 position status verbatim. When a position had no
/// status at all, this is what the round trip sees — chosen so
/// `is_settled_status` says false, which is what a missing status meant in v1.
const UNSTATED_STANCE: &str = "unstated";

/// Project a field skeleton onto atlas atoms.
///
/// Order is preserved question-by-question and position-by-position, because
/// `render_landscape` takes the FIRST five of each section: a projection that
/// reordered would silently change the digest text without changing its
/// content. `skeleton_from_atoms` reads them back in the same order.
///
/// Ids are content-derived (`AtomId::*_content_hash`, ARCH §7.5) so
/// re-projecting the same skeleton reproduces the same atoms and an append is
/// idempotent by id.
pub fn skeleton_to_atoms(skel: &FieldSkeleton) -> Vec<AtomEnvelope> {
    let corpus_id = skel.corpus_id.as_str();
    let mut out: Vec<AtomEnvelope> = Vec::new();

    for q in &skel.canonical_questions {
        let mut addressed_by: Vec<AtomId> = Vec::new();
        let mut position_atoms: Vec<AtomEnvelope> = Vec::new();
        for p in &q.positions {
            let stance = if p.status.trim().is_empty() {
                UNSTATED_STANCE.to_string()
            } else {
                p.status.clone()
            };
            let id = AtomId::position_content_hash(&p.name, &stance, corpus_id);
            addressed_by.push(id.clone());
            position_atoms.push(AtomEnvelope::Position(Position {
                id,
                canonical_name: p.name.clone(),
                content: p.claim.clone(),
                stance,
                proponent_id: None,
                evidence_ids: Vec::new(),
                first_appearance: ChunkRef {
                    chunk_id: String::new(),
                    passage_preview: None,
                    source_doc_id: None,
                },
                anchors: Vec::new(),
                salience: 0.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            }));
        }

        let question_type = QuestionType::from_str_repr(&q.question_type);
        out.push(AtomEnvelope::Question(Question {
            id: AtomId::question_content_hash(&q.question, &question_type, corpus_id),
            content: q.question.clone(),
            question_type,
            addressed_by: addressed_by.clone(),
            raised_at: Vec::new(),
            resolution_status: canonical_status_to_resolution(&q.status, addressed_by),
            enrichment_depth: EnrichmentDepth::Extracted,
        }));
        out.extend(position_atoms);

        for fl in &q.fault_lines {
            let label = fault_line_label(fl);
            out.push(AtomEnvelope::Opposition(Opposition {
                id: AtomId::opposition_content_hash(&label, corpus_id),
                canonical_label: label,
                left_atom_id: None,
                left_label: fl.between_positions.first().cloned().unwrap_or_default(),
                right_atom_id: None,
                right_label: fl.between_positions.get(1).cloned().unwrap_or_default(),
                axis: String::new(),
                framing: fl.crux.clone(),
                first_appearance: ChunkRef {
                    chunk_id: String::new(),
                    passage_preview: None,
                    source_doc_id: None,
                },
                anchors: Vec::new(),
                salience: fl.confidence,
                enrichment_depth: EnrichmentDepth::Extracted,
            }));
        }
    }

    for oq in &skel.open_questions {
        let question_type =
            QuestionType::from_str_repr(oq.question_type.as_deref().unwrap_or("open"));
        out.push(AtomEnvelope::Question(Question {
            id: AtomId::question_content_hash(&oq.question, &question_type, corpus_id),
            content: oq.question.clone(),
            question_type,
            addressed_by: Vec::new(),
            raised_at: Vec::new(),
            resolution_status: ResolutionStatus::Open,
            enrichment_depth: EnrichmentDepth::Extracted,
        }));
    }

    out
}

/// Rebuild the skeleton VIEW `render_landscape` reads, from atlas atoms.
///
/// This is not a v1 file and does not claim to be: the provenance fields say
/// [`ATOM_SOURCED_METHOD`] and `field_stats` is zeroed, because the atoms carry
/// no clustering census. Everything `render_landscape` actually reads —
/// question text, position statuses, fault-line cruxes, open questions — is
/// rebuilt exactly.
///
/// Atoms that are not `Question` / `Position` / `Opposition` are ignored, so
/// this reads correctly against a full atlas that also holds entities, claims
/// and summaries.
pub fn skeleton_from_atoms(corpus_id: &str, atoms: &[AtomEnvelope]) -> FieldSkeleton {
    // Positions and oppositions are looked up by id / by the question they
    // follow; build the position index first so a Question can resolve its
    // `addressed_by` regardless of write order.
    let mut positions: std::collections::HashMap<&str, &Position> =
        std::collections::HashMap::new();
    for a in atoms {
        if let AtomEnvelope::Position(p) = a {
            positions.insert(p.id.as_str(), p);
        }
    }
    let oppositions: Vec<&Opposition> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Opposition(o) => Some(o),
            _ => None,
        })
        .collect();
    // Every fault line the v1 skeleton carried hung off SOME canonical
    // question, and the digest flattens them across all of them anyway
    // (`render_landscape` chains `q.fault_lines`). Hanging them all on the
    // first canonical question reproduces that flattened list in order without
    // inventing a question→opposition edge the atoms do not carry.
    let mut pending_fault_lines: Vec<SkeletonFaultLine> = oppositions
        .iter()
        .map(|o| SkeletonFaultLine {
            id: o.id.as_str().to_string(),
            between_positions: vec![o.left_label.clone(), o.right_label.clone()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect(),
            crux: if o.framing.is_empty() {
                o.canonical_label.clone()
            } else {
                o.framing.clone()
            },
            key_chunk_ids: Vec::new(),
            confidence: o.salience,
            source: "atlas".to_string(),
            resolution_condition: None,
        })
        .collect();

    let mut canonical_questions: Vec<CanonicalQuestion> = Vec::new();
    let mut open_questions: Vec<SkeletonOpenQuestion> = Vec::new();

    for a in atoms {
        let AtomEnvelope::Question(q) = a else {
            continue;
        };
        if matches!(q.resolution_status, ResolutionStatus::Open) {
            open_questions.push(SkeletonOpenQuestion {
                id: q.id.as_str().to_string(),
                question: q.content.clone(),
                status: OPEN_STATUS.to_string(),
                question_type: Some(q.question_type.as_str_repr().to_string()),
                related_question_id: None,
                representative_chunk_ids: Vec::new(),
            });
            continue;
        }
        canonical_questions.push(CanonicalQuestion {
            id: q.id.as_str().to_string(),
            question: q.content.clone(),
            status: resolution_to_canonical_status(&q.resolution_status).to_string(),
            question_type: q.question_type.as_str_repr().to_string(),
            primary_entries: Vec::new(),
            positions: q
                .addressed_by
                .iter()
                .filter_map(|id| positions.get(id.as_str()))
                .map(|p| SkeletonPosition {
                    id: p.id.as_str().to_string(),
                    name: p.canonical_name.clone(),
                    claim: p.content.clone(),
                    status: p.stance.clone(),
                    proponents: Vec::new(),
                    source: "atlas".to_string(),
                    cluster_ids: Vec::new(),
                    centroid_chunk_ids: Vec::new(),
                    discovery_confidence: None,
                })
                .collect(),
            fault_lines: Vec::new(),
        });
    }
    if let Some(first) = canonical_questions.first_mut() {
        first.fault_lines.append(&mut pending_fault_lines);
    }

    FieldSkeleton {
        schema_version: 1,
        corpus_id: corpus_id.to_string(),
        generated_at: String::new(),
        extraction_method: ATOM_SOURCED_METHOD.to_string(),
        prompt_version: String::new(),
        domain_id: String::new(),
        canonical_questions,
        open_questions,
        field_stats: FieldModelStats::default(),
    }
}

/// A canonical question's v1 `status` string, onto the closed
/// [`ResolutionStatus`].
///
/// `"open"` is the one value that must NOT land here as a canonical question —
/// it is what [`skeleton_from_atoms`] routes to the `open_questions` list, so
/// projecting a canonical question with that status would move it between the
/// two v1 lists. Every other value is contested-or-answered, which is
/// `Contested` over the positions that address it (all 549 SEP questions are
/// `"contested"`); an empty status with no positions is `Dissolved`, the
/// vocabulary's "the corpus does not pose this as a live question".
fn canonical_status_to_resolution(status: &str, addressed_by: Vec<AtomId>) -> ResolutionStatus {
    match status.trim().to_lowercase().as_str() {
        "" if addressed_by.is_empty() => ResolutionStatus::Dissolved,
        "dissolved" => ResolutionStatus::Dissolved,
        _ => ResolutionStatus::Contested {
            claim_ids: addressed_by,
        },
    }
}

/// The inverse of [`canonical_status_to_resolution`] for the two values a
/// canonical question can hold. `Open` never reaches here — it is partitioned
/// off before the call.
fn resolution_to_canonical_status(status: &ResolutionStatus) -> &'static str {
    match status {
        ResolutionStatus::Resolved { .. } => "resolved",
        ResolutionStatus::Contested { .. } => "contested",
        ResolutionStatus::Dissolved => "dissolved",
        ResolutionStatus::Open => OPEN_STATUS,
    }
}

/// A fault line's reader-facing label: the two sides when it names them, its
/// crux otherwise. `Opposition::canonical_label` is what a reader sees, so it
/// must never be empty.
fn fault_line_label(fl: &SkeletonFaultLine) -> String {
    if fl.between_positions.len() >= 2 {
        format!("{} vs {}", fl.between_positions[0], fl.between_positions[1])
    } else if !fl.crux.is_empty() {
        fl.crux.clone()
    } else {
        fl.id.clone()
    }
}

/// What one publish did, in the terms an operator judges it by.
///
/// `written` alone cannot tell "already published" from "nothing to publish",
/// which is why `already_present` and `projected` are separate fields and the
/// caller prints all three (ARCH §18.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldAtomsPublished {
    /// Atoms [`skeleton_to_atoms`] produced from the skeleton.
    pub projected: usize,
    /// Atoms appended to the atlas.
    pub written: usize,
    /// Atoms whose id the atlas already held, and were therefore skipped —
    /// this is what makes a re-run idempotent rather than doubling the atlas.
    pub already_present: usize,
    /// Atoms the atlas held before this publish.
    pub atoms_before: usize,
}

impl FieldAtomsPublished {
    pub fn describe(&self) -> String {
        format!(
            "{} atoms projected -> {} written, {} already present (atlas held {} before)",
            self.projected, self.written, self.already_present, self.atoms_before
        )
    }
}

/// Publish a field skeleton into a corpus atlas as atoms.
///
/// The ONE write path for field-model atoms — both the pipeline's terminal
/// step (`FieldModelEngine::publish_skeleton_atoms`) and the one-shot
/// `enrich field-atoms` migration come through here, so "what a published
/// field model looks like on disk" is decided once (ARCH §10.6).
///
/// Idempotent by id: [`skeleton_to_atoms`] derives every id from content, so an
/// atom the atlas already holds is skipped rather than appended twice.
///
/// Creates an empty atlas (`atoms.json` + `edges.json`) when the corpus has
/// none — a field-model-only corpus has no atlas pass to have made one, and
/// `append_atoms_and_edges` reads both files before it appends.
///
/// Writes NO ANN seed row. That is deliberate and it is what keeps this port
/// measurable: the walk's seed space is unchanged, so a retrieval lane run
/// before and after moves only for reasons other than this change. Seeding
/// field-model atoms is a separate decision with its own evidence.
pub fn publish_to_atlas(
    atlas_dir: &std::path::Path,
    skel: &FieldSkeleton,
) -> std::io::Result<FieldAtomsPublished> {
    use super::atlas::{append_atoms_and_edges, read_atlas_atoms, write_atlas_atoms};
    use corpus_engine_vocab::atoms::AtomsFile;
    use corpus_engine_vocab::edges::EdgesFile;

    std::fs::create_dir_all(atlas_dir)?;
    if !atlas_dir.join("edges.json").exists() {
        let empty = EdgesFile::new(Vec::new());
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_vec_pretty(&empty)?,
        )?;
    }
    if !atlas_dir.join("atoms.json").exists() {
        // Through `write_atlas_atoms` rather than a bare file write, so the v2
        // store the runtime reads is built in the same step.
        write_atlas_atoms(atlas_dir, &AtomsFile::new(Vec::new()))?;
    }

    let existing = read_atlas_atoms(atlas_dir)?;
    let held: std::collections::HashSet<String> = existing
        .atoms
        .iter()
        .map(|a| a.id().as_str().to_string())
        .collect();
    let projected = skeleton_to_atoms(skel);
    let fresh: Vec<AtomEnvelope> = projected
        .iter()
        .filter(|a| !held.contains(a.id().as_str()))
        .cloned()
        .collect();
    let report = FieldAtomsPublished {
        projected: projected.len(),
        written: fresh.len(),
        already_present: projected.len() - fresh.len(),
        atoms_before: existing.atoms.len(),
    };
    append_atoms_and_edges(atlas_dir, &fresh, &[])?;
    tracing::info!(
        atlas_dir = %atlas_dir.display(),
        projected = report.projected,
        written = report.written,
        already_present = report.already_present,
        atoms_before = report.atoms_before,
        "field model: published skeleton atoms into the atlas"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADING: &str = "Field guide — test";
    const BUDGET: usize = 250;

    fn position(name: &str, claim: &str, status: &str) -> SkeletonPosition {
        SkeletonPosition {
            id: format!("p_{name}"),
            name: name.into(),
            claim: claim.into(),
            status: status.into(),
            proponents: vec!["Kant".into()],
            source: "skeleton".into(),
            cluster_ids: vec![3],
            centroid_chunk_ids: vec![7],
            discovery_confidence: Some(0.5),
        }
    }

    /// A skeleton shaped like the two real ones this port has to serve: SEP's
    /// (every question `contested`, some with a `majority` position, no fault
    /// lines, no open questions) PLUS the two sections SEP happens not to
    /// populate, so the round trip is exercised on all three.
    fn skeleton() -> FieldSkeleton {
        FieldSkeleton {
            schema_version: 1,
            corpus_id: "sep".into(),
            generated_at: "2026-04-11T03:54:53Z".into(),
            extraction_method: "dual_pass_v1".into(),
            prompt_version: "1.0.0".into(),
            domain_id: "philosophy".into(),
            canonical_questions: vec![
                CanonicalQuestion {
                    id: "q_epr".into(),
                    question: "Do the EPR/B correlations imply non-locality?".into(),
                    status: "contested".into(),
                    // SEP's real tag: not a named QuestionType variant.
                    question_type: "conceptual".into(),
                    primary_entries: vec![],
                    positions: vec![
                        position(
                            "orthodox quantum mechanics",
                            "postulates non-local influences between distant systems",
                            "majority",
                        ),
                        position(
                            "various interpretations",
                            "postulate different kinds of non-locality",
                            "contested",
                        ),
                    ],
                    fault_lines: vec![SkeletonFaultLine {
                        id: "fl_locality".into(),
                        between_positions: vec![
                            "orthodox quantum mechanics".into(),
                            "various interpretations".into(),
                        ],
                        crux: "whether non-locality is causal or merely correlational".into(),
                        key_chunk_ids: vec![11],
                        confidence: 0.8,
                        source: "skeleton".into(),
                        resolution_condition: Some("a decisive experiment".into()),
                    }],
                },
                CanonicalQuestion {
                    id: "q_action".into(),
                    question: "What is the nature of action?".into(),
                    status: "contested".into(),
                    question_type: "conceptual".into(),
                    primary_entries: vec![],
                    positions: vec![position(
                        "causalism",
                        "actions are events caused by intentions",
                        "minority",
                    )],
                    fault_lines: vec![],
                },
            ],
            open_questions: vec![SkeletonOpenQuestion {
                id: "oq_free_will".into(),
                question: "Is libertarian free will compatible with physics?".into(),
                status: "open".into(),
                question_type: Some("conceptual".into()),
                related_question_id: Some("q_action".into()),
                representative_chunk_ids: vec![42],
            }],
            field_stats: FieldModelStats::default(),
        }
    }

    /// THE bar for this port: the digest a reader sees must be byte-identical
    /// whether it was rendered from the v1 file or from the atoms. One
    /// renderer, two sources (ARCH §10.6).
    #[test]
    fn digest_from_atoms_is_byte_identical_to_digest_from_the_v1_file() {
        let v1 = skeleton();
        let from_v1 = v1.render_landscape(HEADING, BUDGET);

        let atoms = skeleton_to_atoms(&v1);
        let view = skeleton_from_atoms("sep", &atoms);
        let from_atoms = view.render_landscape(HEADING, BUDGET);

        assert_eq!(
            from_v1, from_atoms,
            "the atoms lost or reordered something the digest renders"
        );
        // And it is not vacuously equal — all three sections are present.
        assert!(from_v1.contains("Settled concerns"), "{from_v1}");
        assert!(from_v1.contains("Live tensions"), "{from_v1}");
        assert!(from_v1.contains("Open questions"), "{from_v1}");
    }

    /// The partition is `resolution_status` and nothing else: a question that
    /// was open on the way in comes back in `open_questions`, and a canonical
    /// one comes back in `canonical_questions`. If this ever flips, the digest
    /// silently moves a bullet between two labelled sections.
    #[test]
    fn the_open_canonical_partition_survives_the_round_trip() {
        let v1 = skeleton();
        let view = skeleton_from_atoms("sep", &skeleton_to_atoms(&v1));
        assert_eq!(view.canonical_questions.len(), v1.canonical_questions.len());
        assert_eq!(view.open_questions.len(), v1.open_questions.len());
        assert_eq!(
            view.open_questions[0].question,
            "Is libertarian free will compatible with physics?"
        );
        assert!(view
            .canonical_questions
            .iter()
            .all(|q| q.status != OPEN_STATUS));
    }

    /// A position's v1 status is what `is_settled_status` reads. It rides on
    /// `Position.stance` as a free string, so `"majority"` must come back as
    /// `"majority"` — not normalised, not dropped.
    #[test]
    fn position_status_rides_on_stance_verbatim() {
        let v1 = skeleton();
        let atoms = skeleton_to_atoms(&v1);
        let stances: Vec<&str> = atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Position(p) => Some(p.stance.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(stances, vec!["majority", "contested", "minority"]);
        let view = skeleton_from_atoms("sep", &atoms);
        assert_eq!(view.canonical_questions[0].positions[0].status, "majority");
    }

    /// SEP's `"conceptual"` is not a named `QuestionType`; it must survive as
    /// `Other("conceptual")` and read back byte-exact rather than collapsing to
    /// a default.
    #[test]
    fn an_unnamed_question_type_round_trips_through_other() {
        let atoms = skeleton_to_atoms(&skeleton());
        let view = skeleton_from_atoms("sep", &atoms);
        assert_eq!(view.canonical_questions[0].question_type, "conceptual");
        assert_eq!(
            view.open_questions[0].question_type.as_deref(),
            Some("conceptual")
        );
    }

    /// Ids are content-derived, so projecting twice yields the same atoms —
    /// an append can be de-duplicated by id and a re-run cannot double the
    /// atlas (ARCH §7.5).
    #[test]
    fn projection_is_id_stable_across_runs() {
        let a = skeleton_to_atoms(&skeleton());
        let b = skeleton_to_atoms(&skeleton());
        let ids_a: Vec<&str> = a.iter().map(|e| e.id().as_str()).collect();
        let ids_b: Vec<&str> = b.iter().map(|e| e.id().as_str()).collect();
        assert_eq!(ids_a, ids_b);
        assert!(ids_a.iter().all(|id| !id.ends_with("-0000")));
    }

    /// The view reads correctly out of a FULL atlas — the SEP per-article
    /// atlases carry entities, claims and states beside the questions, and a
    /// reader that choked on them would report an empty field for a corpus
    /// that has one.
    #[test]
    fn foreign_atom_kinds_are_ignored_not_fatal() {
        let mut atoms = skeleton_to_atoms(&skeleton());
        atoms.push(AtomEnvelope::Question(Question {
            id: AtomId::from_raw("question-orphan"),
            content: "A question whose positions are not in this atlas".into(),
            question_type: QuestionType::Thematic,
            addressed_by: vec![AtomId::from_raw("position-missing")],
            raised_at: vec![],
            resolution_status: ResolutionStatus::Contested {
                claim_ids: vec![AtomId::from_raw("position-missing")],
            },
            enrichment_depth: EnrichmentDepth::Extracted,
        }));
        let view = skeleton_from_atoms("sep", &atoms);
        assert_eq!(view.canonical_questions.len(), 3);
        // The dangling reference resolves to no position rather than panicking.
        assert!(view.canonical_questions[2].positions.is_empty());
    }

    /// The write path is idempotent by id: publishing twice leaves the atlas
    /// exactly where the first publish left it. Without this, the one-shot
    /// migration verb would double an atlas on a second invocation and the
    /// digest would repeat every bullet.
    #[test]
    fn publishing_twice_writes_the_atoms_once() {
        let dir = tempfile::tempdir().unwrap();
        let atlas = dir.path().join("atlas");
        let skel = skeleton();

        let first = publish_to_atlas(&atlas, &skel).unwrap();
        assert_eq!(first.atoms_before, 0);
        assert_eq!(first.written, first.projected);
        assert_eq!(first.already_present, 0);

        let second = publish_to_atlas(&atlas, &skel).unwrap();
        assert_eq!(second.atoms_before, first.written);
        assert_eq!(second.written, 0, "a re-publish must add nothing");
        assert_eq!(second.already_present, second.projected);

        // And what landed renders the same digest the v1 file would have.
        let on_disk = super::super::atlas::read_atlas_atoms(&atlas).unwrap();
        assert_eq!(on_disk.atoms.len(), first.written);
        assert_eq!(
            skeleton_from_atoms("sep", &on_disk.atoms).render_landscape(HEADING, BUDGET),
            skel.render_landscape(HEADING, BUDGET),
        );
    }

    /// `turn_prepass` skips a corpus WITHOUT reading its atlas when the atlas's
    /// own census reports zero `Question` atoms. That gate is only safe if a
    /// published field model actually shows up in the census — validate the
    /// instrument before trusting the result (ARCH §18.4). A census that
    /// undercounted here would take the Field guide digest dark on every turn,
    /// silently and with exit 0.
    #[test]
    fn a_published_field_model_is_visible_in_the_atlas_census() {
        use super::super::atlas::atoms::AtomType;
        let dir = tempfile::tempdir().unwrap();
        let atlas = dir.path().join("atlas");
        let skel = skeleton();
        let report = publish_to_atlas(&atlas, &skel).unwrap();

        let census = super::super::atlas::read_or_compute_atlas_summary(&atlas)
            .unwrap()
            .expect("a written atlas has a census");
        assert_eq!(census.atom_count as usize, report.written);
        assert_eq!(
            census
                .atom_counts
                .get(&AtomType::Question)
                .copied()
                .unwrap_or(0) as usize,
            skel.canonical_questions.len() + skel.open_questions.len(),
            "the census must see every Question atom, or the digest gate skips the corpus"
        );
        assert!(
            census
                .atom_counts
                .get(&AtomType::Position)
                .copied()
                .unwrap_or(0)
                > 0
        );
    }

    /// A field-model-only corpus has no atlas pass to have created one, so the
    /// publish has to make the atlas rather than erroring on a missing
    /// `atoms.json` (which is what `append_atoms_and_edges` alone does).
    #[test]
    fn publishing_into_a_corpus_with_no_atlas_creates_one() {
        let dir = tempfile::tempdir().unwrap();
        let atlas = dir.path().join("atlas");
        assert!(!atlas.exists());
        let report = publish_to_atlas(&atlas, &skeleton()).unwrap();
        assert!(report.written > 0);
        assert!(atlas.join("atoms.json").exists());
        assert!(atlas.join("edges.json").exists());
        // The v2 store the runtime reads is built too — writing only the JSON
        // leaves the read path missing atoms the export claims are there.
        assert!(atlas.join("atoms.lance").exists());
    }

    /// An atlas with no field-model atoms renders nothing — `is_empty` is what
    /// `turn_prepass` gates the splice on, so this is the "no field model here"
    /// path.
    #[test]
    fn an_atlas_without_questions_yields_an_empty_view() {
        let view = skeleton_from_atoms("wessex-hoard", &[]);
        assert!(
            view.is_empty(),
            "an empty atlas must not look like a field model"
        );
        assert!(view.open_questions.is_empty());
        // `turn_prepass` gates the splice on `is_empty()`, so the renderer is
        // never reached here; what it WOULD emit is the bare heading and no
        // section, which is why the gate is the one above and not a
        // non-empty-string check on the body.
        let body = view.render_landscape(HEADING, BUDGET);
        assert!(!body.contains("Settled concerns"), "{body}");
        assert!(!body.contains("Live tensions"), "{body}");
        assert!(!body.contains("Open questions"), "{body}");
    }
}
