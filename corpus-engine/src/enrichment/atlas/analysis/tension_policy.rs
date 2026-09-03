// SPDX-License-Identifier: AGPL-3.0-or-later
//! Declared-ontology filters over the Phase-6 candidate set — axis 5's
//! `tension.between` and axis 3's `tension.same`
//! (`sovereign/docs/specs/ONTOLOGY_PRIMITIVES.md`).
//!
//! Two passes, both no-ops for a corpus that declares nothing:
//!
//! - [`restrict_claims_to_types`] cuts the claim pool down to the declared
//!   claim types named in `tension.between` BEFORE selection, so the
//!   selector never spends an embedding on a claim that cannot be one half
//!   of a tension.
//! - [`drop_non_comparable_pairs`] runs AFTER selection and removes pairs
//!   the author's own `same` criterion says are not about the same thing —
//!   the Maple House "guest parking versus guest nights" decoy, and the
//!   numismatic "two attributions about different coins" decoy.
//!
//! The third thing axis 5 contributes to Phase 6 — the DERIVED selector —
//! is [`super::tension_shape`]; it reads the corpus, not the declaration,
//! so it is a different subject and a different file.
//!
//! Why a sibling module rather than two more functions in `tensions.rs`:
//! that file is already past the 1200-line split line (ARCH §3.1) and these
//! two read only the DECLARED policy, where everything in `tensions.rs`
//! reads only the graph. One seam, two readers.
//!
//! The rule that decides whether one `same` field rules a pair out, and
//! how a field's value is read off a claim, is [`super::tension_fields`].

use std::collections::BTreeMap;

use super::super::atoms::{AtomId, Claim, Entity};
use super::tension_fields::{field_value, fields_agree, FieldValue, FieldVerdict};
use super::tensions::TensionCandidate;
use crate::enrichment::ontology::OntologyPolicies;
use crate::enrichment::pipeline::atlas::EntityType;

/// The `same` fields a tension block that declares none is read as having
/// declared. Axis 3: "defaults to subject plus the type's clock".
pub const DEFAULT_SAME_FIELDS: [&str; 2] = ["subject", "clock"];

/// The reserved `same` field naming the claim's referent. Resolves to
/// `Claim::subject`, falling back to `Claim::attributed_to` ONLY when that
/// points at something other than a named speaker.
///
/// The fallback exists because `attributed_to` is overloaded across
/// pipelines: in governance it points at the TOPIC a rule governs, which
/// is exactly the referent `same` wants. In a scholarly corpus it points at
/// the SCHOLAR, and there the fallback inverts the axis — two attributions
/// of one coin by two different scholars are the tension, and treating the
/// scholar as the subject makes them non-comparable. Same discriminator
/// [`super::tensions::drop_same_named_speaker_pairs`] already uses: a
/// Person or Institution is a voice, anything else may stand in for the
/// referent.
pub const SAME_FIELD_SUBJECT: &str = "subject";

/// The reserved `same` field naming the type's clock. Resolves through
/// `change.supersedes` to a declared time attribute, or to
/// [`DOCUMENT_DATE_ATTR`] under the default document-date clock.
pub const SAME_FIELD_CLOCK: &str = "clock";

/// The attribute key the document-date clock reads. Stamped by the
/// resolution step from the section title (`ontology::clock::section_date`);
/// absent on a corpus whose sections carry no date, which under the rule
/// above makes the clock vacuous rather than fatal.
pub const DOCUMENT_DATE_ATTR: &str = "document_date";

/// What one comparability pass removed, and on what evidence.
///
/// `by_field` and `field_coverage` exist because the drop count alone
/// cannot distinguish "the author's criterion did its job" from "the field
/// is empty on every claim and the filter deleted the corpus". The caller
/// prints both (ARCH §9.1, §18.1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComparabilityReport {
    /// Candidate pairs removed.
    pub dropped: usize,
    /// `same` field → how many pairs that field was the FIRST to rule out.
    pub by_field: BTreeMap<String, usize>,
    /// `same` field → how many of the claims in scope carried a value for
    /// it. A zero here means the field contributed nothing.
    pub field_coverage: BTreeMap<String, usize>,
    /// Pairs where some field was on the record for exactly one side. The
    /// filter could not judge these and kept them; the count is what makes
    /// that visible instead of silent.
    pub one_sided: usize,
    /// The `same` fields actually applied, in order.
    pub fields: Vec<String>,
    /// Claims the pass indexed.
    pub claims_considered: usize,
}

impl ComparabilityReport {
    /// One line for the operator, or `None` when the pass did not run.
    /// Names every field, its coverage, and what it cost — so a field that
    /// is uniformly absent reads as "contributed nothing" rather than
    /// disappearing into a drop count.
    pub fn summary(&self) -> Option<String> {
        if self.fields.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .fields
            .iter()
            .map(|f| {
                format!(
                    "{f} (on {}/{} claim(s), ruled out {})",
                    self.field_coverage.get(f).copied().unwrap_or(0),
                    self.claims_considered,
                    self.by_field.get(f).copied().unwrap_or(0),
                )
            })
            .collect();
        let mut line = format!(
            "same = [{}]: dropped {} non-comparable pair(s) — {}",
            self.fields.join(", "),
            self.dropped,
            parts.join("; "),
        );
        if self.one_sided > 0 {
            line.push_str(&format!(
                "; {} pair(s) had a field on one side only and could not be judged",
                self.one_sided
            ));
        }
        Some(line)
    }

    /// Fields present on no claim at all. The caller warns on these: the
    /// author declared a criterion the extraction never filled, so it is
    /// silently doing nothing (ARCH §18.1 — a gate you have not watched
    /// fail is not a gate).
    pub fn inert_fields(&self) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|f| self.field_coverage.get(*f).copied().unwrap_or(0) == 0)
            .map(String::as_str)
            .collect()
    }
}

/// What [`restrict_claims_to_types`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BetweenOutcome {
    /// No `tension.between` declared — the pool is untouched.
    NotDeclared,
    /// Declared, but NO claim in the corpus carries a `claim_kind` at all,
    /// so the allow-list has nothing to select on. Reported, not enforced:
    /// enforcing it would empty the pool and turn the whole axis off on a
    /// corpus whose author asked for it (ARCH §18.3 — absence is reported,
    /// never defaulted, and "the extractor never typed a claim" is not
    /// "the author declared the wrong type").
    Inert,
    /// Applied: `dropped` claims were not of a listed type.
    Applied { dropped: usize, kept: usize },
}

/// Cut the claim pool to the declared claim types named in
/// `tension.between`.
///
/// A claim whose `claim_kind` is absent is NOT of a declared type and goes
/// with the rest — `between` is an allow-list, and an unlabelled claim
/// cannot be shown to be on it. The ONE exception is the case where NO
/// claim carries a kind: see [`BetweenOutcome::Inert`].
///
/// No-op when `between` is empty (every undeclared corpus, and every
/// declared one that named no types): the pool is untouched, which is what
/// keeps invariant I1 structural rather than remembered.
pub fn restrict_claims_to_types(claims: &mut Vec<Claim>, between: &[String]) -> BetweenOutcome {
    if between.is_empty() {
        return BetweenOutcome::NotDeclared;
    }
    if claims.iter().all(|c| c.claim_kind.is_none()) {
        tracing::warn!(
            target: "atlas.tensions",
            between = ?between,
            claims = claims.len(),
            "restrict_claims_to_types: no claim carries a claim_kind — allow-list inert"
        );
        return BetweenOutcome::Inert;
    }
    let before = claims.len();
    claims.retain(|c| {
        c.claim_kind
            .as_deref()
            .is_some_and(|k| between.iter().any(|b| b == k))
    });
    let dropped = before - claims.len();
    tracing::debug!(
        target: "atlas.tensions",
        between = ?between,
        before,
        after = claims.len(),
        dropped,
        "restrict_claims_to_types: tension.between allow-list applied"
    );
    BetweenOutcome::Applied {
        dropped,
        kept: claims.len(),
    }
}

/// Remove candidate pairs the declared `same` criterion says are not about
/// the same thing.
///
/// Runs only when the corpus declares types — an undeclared corpus has no
/// `subject` on any claim and no author criterion to apply, so the pass
/// would be comparing two absent fields on every pair and dropping nothing
/// while pretending to. Returning an empty report there is the honest
/// answer and it is also invariant I1.
pub fn drop_non_comparable_pairs(
    candidates: &mut Vec<TensionCandidate>,
    claims: &[Claim],
    entities: &[Entity],
    policies: &OntologyPolicies,
) -> ComparabilityReport {
    let mut report = ComparabilityReport::default();
    if !policies.has_declarations() {
        return report;
    }
    let fields = same_fields(policies);
    report.fields = fields.clone();
    report.claims_considered = claims.len();

    // Which `attributed_to` targets are named speakers, and therefore may
    // NOT stand in for the subject. Same index the same-speaker filter
    // builds, for the same reason.
    let speakers: std::collections::HashSet<&AtomId> = entities
        .iter()
        .filter(|e| matches!(e.entity_type, EntityType::Person | EntityType::Institution))
        .map(|e| &e.id)
        .collect();

    // One pass to resolve every field on every claim, so the retain below
    // is a map lookup rather than an attribute walk per candidate.
    let by_id: BTreeMap<&str, Vec<Option<FieldValue>>> = claims
        .iter()
        .map(|c| {
            let vals = fields
                .iter()
                .map(|f| field_value(c, f, policies, &speakers))
                .collect::<Vec<_>>();
            (c.id.as_str(), vals)
        })
        .collect();

    for (i, f) in fields.iter().enumerate() {
        let covered = by_id.values().filter(|v| v[i].is_some()).count();
        report.field_coverage.insert(f.clone(), covered);
    }

    let mut by_field: BTreeMap<String, usize> = BTreeMap::new();
    let mut one_sided = 0usize;
    let before = candidates.len();
    candidates.retain(|cand| {
        let (Some(a), Some(b)) = (
            by_id.get(cand.source_atom.as_str()),
            by_id.get(cand.target_atom.as_str()),
        ) else {
            // One endpoint is a State, or an atom outside the restricted
            // pool. `same` speaks about claims; leave the pair to the
            // classifier rather than dropping it on a criterion that does
            // not apply to it.
            return true;
        };
        let mut one_sided_here = false;
        for (i, f) in fields.iter().enumerate() {
            match fields_agree(a[i].as_ref(), b[i].as_ref()) {
                FieldVerdict::Agrees => {}
                FieldVerdict::OneSided => one_sided_here = true,
                FieldVerdict::Differs => {
                    *by_field.entry(f.clone()).or_default() += 1;
                    tracing::trace!(
                        target: "atlas.tensions",
                        candidate = %cand.id,
                        field = %f,
                        "drop_non_comparable_pairs: pair ruled out"
                    );
                    return false;
                }
            }
        }
        if one_sided_here {
            one_sided += 1;
        }
        true
    });
    report.by_field = by_field;
    report.one_sided = one_sided;
    report.dropped = before - candidates.len();
    tracing::debug!(
        target: "atlas.tensions",
        fields = ?report.fields,
        before,
        after = candidates.len(),
        dropped = report.dropped,
        coverage = ?report.field_coverage,
        "drop_non_comparable_pairs: declared comparability applied"
    );
    report
}

/// The `same` fields to apply: the author's list, or
/// [`DEFAULT_SAME_FIELDS`] when they declared none.
fn same_fields(policies: &OntologyPolicies) -> Vec<String> {
    let declared = &policies.derivation.tension.same;
    if declared.is_empty() {
        DEFAULT_SAME_FIELDS.iter().map(|s| s.to_string()).collect()
    } else {
        declared.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::analysis::tensions::CandidateSource;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity};
    use crate::enrichment::ontology::{AttrDecl, AttrFamily, OntologyTypeDecl, TypeKind};
    use crate::enrichment::pipeline::atlas::EntityType;
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
    };

    fn claim(id: usize, kind: Option<&str>, subject: Option<usize>) -> Claim {
        Claim {
            id: AtomId::claim(id),
            content: format!("claim {id}"),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: Vec::new(),
            quotable_excerpt: None,
            attributed_to: None,
            subject: subject.map(AtomId::entity),
            attributes: Default::default(),
            confidence: None,
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            claim_kind: kind.map(str::to_string),
            concession_outcome: None,
            evidence_kind: None,
        }
    }

    /// A named speaker (Person) — an `attributed_to` pointing here may NOT
    /// stand in for the subject.
    fn scholar(id: usize, name: &str) -> Entity {
        Entity {
            id: AtomId::entity(id),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec-0001", None),
            description: String::new(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: Default::default(),
            concept_kind: None,
        }
    }

    fn pair(id: &str, a: usize, b: usize) -> TensionCandidate {
        TensionCandidate {
            id: id.to_string(),
            source_atom: AtomId::claim(a),
            target_atom: AtomId::claim(b),
            discovery: CandidateSource::EmbeddingTopK,
            cluster_id: None,
            shared_entity: None,
        }
    }

    /// The shipped numismatics declaration: one claim type (`attribution`)
    /// with a `proposed_date` time attribute, `between = ["attribution"]`,
    /// no `same`.
    use crate::recipe_templates::numismatics_policies as numismatics;

    #[test]
    fn drop_non_comparable_pairs_keeps_same_subject_and_clock() {
        let policies = numismatics();
        // c1 and c2 are about coin 1; c3 is about coin 2.
        let claims = vec![
            claim(1, Some("attribution"), Some(1)),
            claim(2, Some("attribution"), Some(1)),
            claim(3, Some("attribution"), Some(2)),
        ];
        let mut candidates = vec![pair("cand-0001", 1, 2), pair("cand-0002", 1, 3)];
        let report = drop_non_comparable_pairs(&mut candidates, &claims, &[], &policies);

        assert_eq!(
            candidates.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["cand-0001"],
            "the same-subject pair survives; the different-subject pair does not"
        );
        assert_eq!(report.dropped, 1);
        assert_eq!(report.by_field.get("subject").copied(), Some(1));
        assert_eq!(report.field_coverage.get("subject").copied(), Some(3));
        // No claim carries a document date, so the clock distinguished
        // nothing — and the report says so rather than hiding it.
        assert_eq!(report.field_coverage.get("clock").copied(), Some(0));
        assert_eq!(report.inert_fields(), vec!["clock"]);
    }

    #[test]
    fn undeclared_ontology_drops_nothing() {
        let claims = vec![claim(1, None, None), claim(2, None, Some(9))];
        let mut candidates = vec![pair("cand-0001", 1, 2)];
        let report =
            drop_non_comparable_pairs(&mut candidates, &claims, &[], &OntologyPolicies::default());
        assert_eq!(
            candidates.len(),
            1,
            "invariant I1: no declaration, no filter"
        );
        assert_eq!(report, ComparabilityReport::default());
        assert!(report.summary().is_none());
    }

    #[test]
    fn absent_on_both_sides_does_not_rule_a_pair_out() {
        let policies = numismatics();
        // Neither claim carries a subject: the field cannot distinguish
        // them, so the pair survives for the classifier to judge.
        let claims = vec![
            claim(1, Some("attribution"), None),
            claim(2, Some("attribution"), None),
        ];
        let mut candidates = vec![pair("cand-0001", 1, 2)];
        let report = drop_non_comparable_pairs(&mut candidates, &claims, &[], &policies);
        assert_eq!(candidates.len(), 1);
        assert_eq!(report.dropped, 0);
        assert_eq!(
            report.inert_fields(),
            vec!["subject", "clock"],
            "reported in `same` order, so a reader sees which criterion went inert"
        );
    }

    /// The measured correction. Excluding on one-sided absence drops 40 of
    /// the 158 real `wessex-hoard` candidates and removes NONE of its 3
    /// known false positives (all three are blind on both sides), so
    /// ignorance keeps the pair and is COUNTED instead.
    #[test]
    fn present_on_one_side_only_keeps_the_pair_and_is_counted() {
        let policies = numismatics();
        let claims = vec![
            claim(1, Some("attribution"), Some(1)),
            claim(2, Some("attribution"), None),
        ];
        let mut candidates = vec![pair("cand-0001", 1, 2)];
        let report = drop_non_comparable_pairs(&mut candidates, &claims, &[], &policies);
        assert_eq!(candidates.len(), 1, "not known to differ is not a mismatch");
        assert_eq!(report.dropped, 0);
        assert_eq!(report.by_field.get("subject").copied(), None);
        assert_eq!(
            report.one_sided, 1,
            "the blind spot is reported, not hidden"
        );
        assert!(report.summary().unwrap().contains("could not be judged"));
    }

    /// The `subject` fallback reads `attributed_to` only when it is not a
    /// named speaker. Two attributions of one coin BY DIFFERENT SCHOLARS
    /// are the tension the numismatics corpus plants; treating the scholar
    /// as the subject would make them non-comparable and invert the axis.
    #[test]
    fn a_named_speaker_never_stands_in_for_the_subject() {
        let policies = numismatics();
        let entities = vec![scholar(1, "Halstead"), scholar(2, "Ferreira")];
        let mut halstead = claim(1, Some("attribution"), None);
        halstead.attributed_to = Some(AtomId::entity(1));
        let mut ferreira = claim(2, Some("attribution"), None);
        ferreira.attributed_to = Some(AtomId::entity(2));

        let claims = vec![halstead, ferreira];
        let mut candidates = vec![pair("cand-0001", 1, 2)];
        let report = drop_non_comparable_pairs(&mut candidates, &claims, &entities, &policies);
        assert_eq!(
            candidates.len(),
            1,
            "two scholars disagreeing about one coin stay comparable"
        );
        assert_eq!(report.field_coverage.get("subject").copied(), Some(0));

        // …and a non-speaker attribution (a governance TOPIC) does stand in.
        let mut topic = scholar(3, "quiet hours");
        topic.entity_type = EntityType::Concept;
        let mut other_topic = scholar(4, "parking");
        other_topic.entity_type = EntityType::Concept;
        let mut a = claim(1, Some("attribution"), None);
        a.attributed_to = Some(AtomId::entity(3));
        let mut b = claim(2, Some("attribution"), None);
        b.attributed_to = Some(AtomId::entity(4));
        let mut candidates = vec![pair("cand-0001", 1, 2)];
        let report =
            drop_non_comparable_pairs(&mut candidates, &[a, b], &[topic, other_topic], &policies);
        assert!(candidates.is_empty(), "different topics are not comparable");
        assert_eq!(report.by_field.get("subject").copied(), Some(1));
    }

    #[test]
    fn declared_time_field_compares_by_overlap_not_equality() {
        // The governance shape: `same = ["subject", "valid"]`.
        let mut policies = OntologyPolicies::default();
        policies.shape.types.push(OntologyTypeDecl {
            name: "rule".into(),
            kind: TypeKind::Claim,
            attributes: vec![AttrDecl {
                name: "valid".into(),
                family: AttrFamily::Time { range: true },
                description: String::new(),
            }],
            ..Default::default()
        });
        policies.derivation.tension.same = vec!["subject".into(), "valid".into()];

        let mut overlapping = claim(1, Some("rule"), Some(1));
        overlapping
            .attributes
            .insert("valid".into(), "2024-01/2025-06".into());
        let mut later = claim(2, Some("rule"), Some(1));
        later.attributes.insert("valid".into(), "2025-03/".into());
        let mut disjoint = claim(3, Some("rule"), Some(1));
        disjoint
            .attributes
            .insert("valid".into(), "2026-01/2026-12".into());

        let claims = vec![overlapping, later, disjoint];
        let mut candidates = vec![pair("cand-0001", 1, 2), pair("cand-0002", 1, 3)];
        let report = drop_non_comparable_pairs(&mut candidates, &claims, &[], &policies);

        assert_eq!(
            candidates.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["cand-0001"],
            "overlapping validity stays comparable; disjoint validity does not"
        );
        assert_eq!(report.by_field.get("valid").copied(), Some(1));
        assert!(report.inert_fields().is_empty());
    }

    #[test]
    fn between_restricts_to_declared_types() {
        let mut claims = vec![
            claim(1, Some("attribution"), None),
            claim(2, Some("provenance_note"), None),
            claim(3, None, None),
        ];
        let outcome = restrict_claims_to_types(&mut claims, &["attribution".to_string()]);
        assert_eq!(
            outcome,
            BetweenOutcome::Applied {
                dropped: 2,
                kept: 1
            },
            "the other type and the unlabelled claim both go"
        );
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].claim_kind.as_deref(), Some("attribution"));
    }

    #[test]
    fn between_empty_leaves_every_claim() {
        let mut claims = vec![claim(1, Some("attribution"), None), claim(2, None, None)];
        assert_eq!(
            restrict_claims_to_types(&mut claims, &[]),
            BetweenOutcome::NotDeclared
        );
        assert_eq!(
            claims.len(),
            2,
            "invariant I1: no `between`, no restriction"
        );
    }

    /// The `wessex-hoard` case, measured 2026-09-02: 48 claim atoms, not
    /// one carrying a `claim_kind` (`setup-numismatics-corpus.sh
    /// --assert-only` prints `attribution 0`). Enforcing the allow-list
    /// there empties the pool and turns the axis OFF on a corpus whose
    /// author asked for it. Report it, do not enforce it.
    #[test]
    fn between_is_inert_when_no_claim_carries_a_kind() {
        let mut claims = vec![claim(1, None, None), claim(2, None, None)];
        assert_eq!(
            restrict_claims_to_types(&mut claims, &["attribution".to_string()]),
            BetweenOutcome::Inert
        );
        assert_eq!(claims.len(), 2, "the pool is NOT emptied");
    }
}
