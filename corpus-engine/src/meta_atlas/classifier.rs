// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-atom articulation classifier — Phase 0 of the meta-atlas.
//!
//! Rule-based deterministic classifier. Reads atom shape + chunk
//! preview, emits an [`ArticulationVector`] per atom. O(1) per atom
//! at runtime; the only path that scans the chunk preview is the
//! [`classify_by_chunk_preview`] fallback for atoms whose
//! `entity_type` round-trips to `EntityType::Other(_)` (predominantly
//! Wikipedia's structural-first output, which tags every article as
//! `Other("article")`).
//!
//! ## Coverage targets (Stage 1 calibration)
//!
//! - Wikipedia atlas (1.6M atoms, every Entity is `Other("article")`):
//!   ≥80% should land Inventory-dominant via [`classify_by_chunk_preview`]'s
//!   markers.
//! - SEP per-article atlases (Claim atoms with `discourse_act` set):
//!   ≥70% should land Argument-dominant via the Claim arm.
//! - Obsidian-vault atlases (mixed Entity + Event + Claim shapes):
//!   should distribute non-trivially across all three axes.
//!
//! ## Rule rationale
//!
//! Each arm is justified by the atom shape spec (§2 of
//! ENRICHMENT_V2.md). The thresholds are best-guess for v1 and
//! expected to tune in response to the Stage 1 calibration
//! histogram. Future Moves (Move 6+) will add an LLM fallback for
//! atoms whose vectors come out [`ArticulationVector::is_ambiguous`].

use std::sync::LazyLock;

use regex::Regex;

use crate::enrichment::atlas::AtomEnvelope;
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::atlas::{DiscourseAct, EntityType, EventType};
use crate::stream_axes::ArticulationVector;

/// Top-level entry. Classify a single atom into an articulation
/// distribution.
///
/// `chunk_preview` is the first ~300 chars of the atom's source
/// chunk, used only by the [`classify_by_chunk_preview`] fallback
/// for `EntityType::Other(_)` atoms (where the type tag itself
/// carries no articulation signal). Pass empty string when no
/// preview is available — the fallback will return
/// [`ArticulationVector::balanced`].
pub fn classify_articulation(env: &AtomEnvelope, chunk_preview: &str) -> ArticulationVector {
    classify_articulation_with(env, chunk_preview, None)
}

/// [`classify_articulation`], with the corpus's DECLARED ontology in hand.
///
/// `vocab` changes exactly one arm: `EntityType::Other(name)`. Before ontology
/// v1 that arm meant "a type tag the six kinds do not cover", and the only
/// thing to do with it was scan the chunk preview for prose markers — the
/// Wikipedia `Other("article")` case the fallback was built for. A DECLARED
/// type is a different situation: the author said what a `coin` is, so it is
/// classified as the kind it declares rather than guessed at from prose.
///
/// `None` — and a name the vocabulary does not know — both fall through to
/// [`classify_by_chunk_preview`] unchanged. Wikipedia declares nothing, so its
/// 1.6M `Other("article")` atoms are byte-identical (I5).
pub fn classify_articulation_with(
    env: &AtomEnvelope,
    chunk_preview: &str,
    vocab: Option<&OntologyPolicies>,
) -> ArticulationVector {
    match env {
        AtomEnvelope::Entity(e) => {
            // defining_quote is the strongest single signal: the
            // article distilled a quotable definition for this
            // entity. The atom carries that articulation.
            if e.defining_quote.is_some() {
                return ArticulationVector::new(0.10, 0.85, 0.05);
            }
            match &e.entity_type {
                EntityType::Person
                | EntityType::Place
                | EntityType::Work
                | EntityType::Institution => public_noun(e.aliases.len(), e.salience),
                EntityType::Concept => concept(),
                EntityType::Initiative => initiative(),
                EntityType::Other(name) => declared_articulation(name, e, vocab)
                    .unwrap_or_else(|| classify_by_chunk_preview(chunk_preview)),
            }
        }
        AtomEnvelope::Claim(c) => match c.discourse_act {
            // Argument: the canonical articulated-claim acts.
            DiscourseAct::Argue
            | DiscourseAct::Assert
            | DiscourseAct::Object
            | DiscourseAct::Interpret
            | DiscourseAct::Warn => ArticulationVector::new(0.05, 0.90, 0.05),
            // Hypothesize / Imply — Argument with slight Trace tone
            // (they're tentative articulated moves; the tentativeness
            // bleeds toward "what the text is doing right now").
            DiscourseAct::Hypothesize | DiscourseAct::Imply => {
                ArticulationVector::new(0.10, 0.75, 0.15)
            }
            // Enact / Commit — performative speech acts. The claim
            // does something rather than just states it. Trace-leaning
            // because the claim IS the action.
            DiscourseAct::Enact | DiscourseAct::Commit => ArticulationVector::new(0.10, 0.40, 0.50),
            DiscourseAct::Other(_) => ArticulationVector::new(0.10, 0.80, 0.10),
        },
        AtomEnvelope::Event(e) => match e.event_type {
            // Realization / Decision — discrete moments of articulated
            // change. Argument-leaning but with strong Trace component
            // because they're temporally located.
            EventType::Realization | EventType::Decision => {
                ArticulationVector::new(0.10, 0.45, 0.45)
            }
            // Action / Encounter / ExternalForce — physical happenings.
            // Trace-dominant.
            EventType::Action | EventType::Encounter | EventType::ExternalForce => {
                ArticulationVector::new(0.05, 0.20, 0.75)
            }
            // Publication — both a structural event (a thing now
            // exists) and a temporal one (it happened at T). Balanced
            // with slight Trace bias.
            EventType::Publication => ArticulationVector::new(0.30, 0.30, 0.40),
            EventType::Other(_) => ArticulationVector::new(0.10, 0.30, 0.60),
        },
        // Configuration / Position / Opposition — structural
        // argument shapes. The atom IS an argued move; Argument-
        // dominant by construction.
        AtomEnvelope::Configuration(_)
        | AtomEnvelope::Position(_)
        | AtomEnvelope::Opposition(_) => ArticulationVector::new(0.10, 0.85, 0.05),
        // State / Relation — descriptive over interpretive.
        // Argument-leaning with mild Trace (states are
        // temporally-located conditions).
        AtomEnvelope::State(_) | AtomEnvelope::Relation(_) => {
            ArticulationVector::new(0.10, 0.75, 0.15)
        }
        // Question / ArgumentReconstruction — clearly articulated
        // structure-of-thought.
        AtomEnvelope::Question(_) | AtomEnvelope::ArgumentReconstruction(_) => {
            ArticulationVector::new(0.05, 0.85, 0.10)
        }
        // Asset — pure Inventory. An asset atom says "this binary
        // exists in this corpus"; it carries no articulated argument
        // or temporal trace by itself. The carrier doc's atoms supply
        // those — the Asset only inventories the thing.
        AtomEnvelope::Asset(_) => ArticulationVector::new(0.90, 0.05, 0.05),
    }
}

/// Concepts are articulated objects — defined and argued about.
/// Argument-leaning.
fn concept() -> ArticulationVector {
    ArticulationVector::new(0.25, 0.70, 0.05)
}

/// Personal/conversational domain "active project" shape. Trace-leaning —
/// projects are lived activity over time.
fn initiative() -> ArticulationVector {
    ArticulationVector::new(0.10, 0.30, 0.60)
}

/// Public-noun. Strongly-attested anchors (many aliases or high salience) are
/// Inventory-leaning; weakly-attested ones bleed toward Argument (probably
/// extracted opportunistically).
///
/// ONE decider: the six-kind arm and the declared-type arm are the same rule,
/// so they read the same function rather than repeating the four constants
/// (§10.6).
fn public_noun(aliases: usize, salience: f32) -> ArticulationVector {
    if aliases >= 2 || salience >= 0.5 {
        ArticulationVector::new(0.75, 0.20, 0.05)
    } else {
        ArticulationVector::new(0.55, 0.40, 0.05)
    }
}

/// The articulation of a DECLARED entity type, or `None` when the name is not
/// declared (Wikipedia's `Other("article")`, and every pre-ontology atlas).
///
/// A declared type is placed by the generic kind it descends from, so an
/// author who writes `specializes = "concept"` gets the concept vector without
/// having to name a vector. A declared type that specializes nothing generic
/// is a thing in its own right — the public-noun rule, the same one `person` /
/// `place` / `work` / `institution` get, because that is what a declared
/// entity type is.
fn declared_articulation(
    name: &str,
    e: &crate::enrichment::atlas::atoms::Entity,
    vocab: Option<&OntologyPolicies>,
) -> Option<ArticulationVector> {
    let index = TypeIndex::from_policies(vocab?);
    if !index.contains(name) {
        return None;
    }
    // Two legal spellings of "this type is a kind of X": the author declared a
    // type named `concept` and specialized it, or they wrote `specializes =
    // "concept"` with no such declaration — `validate_block` accepts an
    // unresolvable reference only when it names a generic entity kind, so the
    // chain's terminal undeclared parent IS that kind. `is_a` answers the
    // first, `generic_ancestor` the second; both read the one `specializes`
    // walk in `TypeIndex`.
    let descends_from =
        |kind: &str| index.is_a(name, kind) || index.generic_ancestor(name) == Some(kind);
    let v = if descends_from("concept") {
        concept()
    } else if descends_from("initiative") {
        initiative()
    } else {
        public_noun(e.aliases.len(), e.salience)
    };
    tracing::debug!(
        declared_type = name,
        inventory = v.inventory,
        argument = v.argument,
        trace = v.trace,
        "meta-atlas: declared type articulation"
    );
    Some(v)
}

/// Fallback for atoms whose `entity_type` round-trips as
/// `EntityType::Other(_)` — predominantly Wikipedia's structural-first
/// output (every article tagged `Other("article")`) and any custom
/// per-corpus entity taxonomies. Inspects the first ~300 chars of
/// the atom's source chunk for shape markers.
///
/// Markers (in priority order):
///   1. Wikipedia-shaped opener: `'''Name''' (is|was) a` → Inventory
///      with light Argument.
///   2. Strong first-person + date markers → Trace.
///   3. Argumentative connectives (therefore / however / argues that)
///      → Argument.
///   4. Markdown headers / pure bullet content → Inventory.
///   5. Nothing fires → Inventory by structural prior. We bias to
///      Inventory rather than balanced because the atom DOES exist
///      as a corpus entry — "this thing is named in the corpus" is
///      always at least a weak Inventory claim.
pub fn classify_by_chunk_preview(text: &str) -> ArticulationVector {
    if text.is_empty() {
        return ArticulationVector::new(0.65, 0.25, 0.10);
    }

    // Sample only the first ~600 bytes to keep regex work bounded,
    // floored to a char boundary so a multi-byte char straddling the cap
    // (e.g. an em-dash at bytes 598..601) doesn't panic the slice.
    let head = if text.len() > 600 {
        let mut end = 600;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };

    // 1. Wiki opener — strongest possible Inventory signal.
    if WIKI_OPENER.is_match(head) {
        return ArticulationVector::new(0.80, 0.15, 0.05);
    }

    let first_person = FIRST_PERSON.is_match(head);
    let date_marker = DATE_MARKER.is_match(head);
    let argumentative = ARGUMENTATIVE.is_match(head);
    let structural = STRUCTURAL_OPENER.is_match(head);

    // 2. Strong Trace signal — first-person AND date.
    if first_person && date_marker {
        return ArticulationVector::new(0.10, 0.15, 0.75);
    }
    // First-person without date — still Trace-leaning (journal,
    // chat).
    if first_person {
        return ArticulationVector::new(0.15, 0.30, 0.55);
    }
    // 3. Argumentative connectives without first-person — Argument.
    if argumentative {
        return ArticulationVector::new(0.20, 0.70, 0.10);
    }
    // 4. Structural-only shape (heading or bullet list) — Inventory.
    if structural {
        return ArticulationVector::new(0.70, 0.20, 0.10);
    }
    // 5. No marker — Inventory prior.
    ArticulationVector::new(0.65, 0.25, 0.10)
}

// ── compiled regexes ──────────────────────────────────────────

/// Wikipedia article opener pattern. Wiki article bodies start
/// with `'''<bolded name>''' (is|was|are|were) <article>`. Only
/// match the opening — anchored at start of string.
static WIKI_OPENER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\s*'''[^']{1,200}'''\s+(is|are|was|were)\s+(a|an|the)\s"#)
        .expect("WIKI_OPENER regex")
});

/// First-person markers — case-insensitive whole-word. Catches
/// journals, chat transcripts, codex sessions.
static FIRST_PERSON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(i|we|my|our|me|us|i've|i'm|we've|we're)\b"#).expect("FIRST_PERSON regex")
});

/// Date markers — explicit dates, relative-time markers, month
/// names. Combined with first-person markers signals trace
/// shape.
static DATE_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(yesterday|today|tomorrow|last\s+\w+|next\s+\w+|this\s+morning|this\s+evening|\d{4}-\d{2}-\d{2}|jan(uary)?\b|feb(ruary)?\b|mar(ch)?\b|apr(il)?\b|may\b|jun(e)?\b|jul(y)?\b|aug(ust)?\b|sep(tember)?\b|oct(ober)?\b|nov(ember)?\b|dec(ember)?\b)"#,
    )
    .expect("DATE_MARKER regex")
});

/// Argumentative connectives. Strong signal that the text is
/// articulating a position. Case-insensitive whole-word.
static ARGUMENTATIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(therefore|however|nevertheless|moreover|furthermore|thus|hence|argues|claims|proposes|asserts|contends|maintains|posits|because\s+of|in\s+contrast|on\s+the\s+other\s+hand)\b"#,
    )
    .expect("ARGUMENTATIVE regex")
});

/// Structural openers — markdown heading at start of line, or
/// pure bullet-list shape.
static STRUCTURAL_OPENER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\s*(#{1,6}\s|[-*]\s|\d+\.\s)"#).expect("STRUCTURAL_OPENER regex")
});

// ── tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::{
        atoms::{
            ArgumentReconstruction, Claim, Configuration, Entity, Event, Opposition, Position,
            Question, Relation, ResolutionStatus, State,
        },
        AtomEnvelope, AtomId, ChunkRef, SectionPosition, SectionRange,
    };
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, EventType,
        QuestionType, RelationType, StateType,
    };
    use crate::stream_axes::Articulation;

    fn entity(
        et: EntityType,
        salience: f32,
        aliases: Vec<&str>,
        quote: Option<&str>,
    ) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(1),
            canonical_name: "Test".into(),
            aliases: aliases.into_iter().map(String::from).collect(),
            entity_type: et,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "desc".into(),
            defining_quote: quote.map(String::from),
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    // ── ontology-v1 P5: declared types ───────────────────────

    fn declared(types: &[(&str, Option<&str>)]) -> OntologyPolicies {
        use crate::enrichment::ontology::{OntologyTypeDecl, TypeKind};
        let mut p = OntologyPolicies::default();
        p.shape.types = types
            .iter()
            .map(|(name, specializes)| OntologyTypeDecl {
                name: (*name).to_string(),
                kind: TypeKind::Entity,
                specializes: specializes.map(str::to_string),
                ..Default::default()
            })
            .collect();
        p
    }

    /// A declared entity type is a thing in its own right: it gets the same
    /// public-noun treatment `person` / `place` / `work` / `institution` get,
    /// not the Wikipedia prose fallback.
    #[test]
    fn a_declared_type_is_classified_as_a_public_noun() {
        let vocab = declared(&[("coin", None), ("sceatta", Some("coin"))]);
        let strong = entity(EntityType::Other("coin".into()), 0.9, vec![], None);
        let v = classify_articulation_with(&strong, "", Some(&vocab));
        assert_eq!(
            v,
            classify_articulation(&entity(EntityType::Person, 0.9, vec![], None), "")
        );
        assert_eq!(v.dominant(), Articulation::Inventory);

        // Weakly attested → the same bleed the six kinds get.
        let weak = entity(EntityType::Other("sceatta".into()), 0.1, vec![], None);
        assert_eq!(
            classify_articulation_with(&weak, "", Some(&vocab)),
            classify_articulation(&entity(EntityType::Person, 0.1, vec![], None), "")
        );
    }

    /// A declared type that says `specializes = "concept"` is placed by the
    /// generic kind it descends from — the author never names a vector.
    #[test]
    fn a_declared_type_inherits_its_generic_kinds_articulation() {
        let vocab = declared(&[
            ("doctrine", Some("concept")),
            ("school", Some("doctrine")),
            ("campaign", Some("initiative")),
        ]);
        for name in ["doctrine", "school"] {
            let env = entity(EntityType::Other(name.into()), 0.9, vec![], None);
            assert_eq!(
                classify_articulation_with(&env, "", Some(&vocab)),
                classify_articulation(&entity(EntityType::Concept, 0.9, vec![], None), ""),
                "{name} descends from concept"
            );
        }
        let env = entity(EntityType::Other("campaign".into()), 0.9, vec![], None);
        assert_eq!(
            classify_articulation_with(&env, "", Some(&vocab)),
            classify_articulation(&entity(EntityType::Initiative, 0.9, vec![], None), "")
        );
    }

    /// I5, at the site. Wikipedia tags every article `Other("article")` and
    /// declares nothing; with no vocabulary — and with a vocabulary that does
    /// not know the name — the chunk-preview fallback runs exactly as before.
    #[test]
    fn an_undeclared_other_type_still_falls_back_to_the_preview() {
        let vocab = declared(&[("coin", None)]);
        let env = entity(EntityType::Other("article".into()), 0.9, vec![], None);
        let preview = "'''Ceolwulf''' was a king of Mercia.";
        let baseline = classify_by_chunk_preview(preview);
        assert_eq!(classify_articulation(&env, preview), baseline);
        assert_eq!(classify_articulation_with(&env, preview, None), baseline);
        assert_eq!(
            classify_articulation_with(&env, preview, Some(&vocab)),
            baseline,
            "a name the vocabulary does not declare must not take the declared path"
        );
    }

    /// The two-argument name is the no-vocabulary case, for every atom kind.
    #[test]
    fn classify_articulation_is_the_none_vocabulary_case() {
        for env in [
            entity(EntityType::Person, 0.9, vec!["a", "b"], None),
            entity(EntityType::Other("article".into()), 0.2, vec![], None),
            claim(DiscourseAct::Argue),
            event(EventType::Action),
        ] {
            assert_eq!(
                classify_articulation(&env, "some preview"),
                classify_articulation_with(&env, "some preview", None)
            );
        }
    }

    fn claim(act: DiscourseAct) -> AtomEnvelope {
        AtomEnvelope::Claim(Claim {
            attributes: Default::default(),
            subject: None,
            id: AtomId::claim(1),
            content: "c".into(),
            discourse_act: act,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: Vec::new(),
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
            anchor: None,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    fn event(et: EventType) -> AtomEnvelope {
        AtomEnvelope::Event(Event {
            attributes: Default::default(),
            id: AtomId::event(1),
            description: "e".into(),
            event_type: et,
            participants: Vec::new(),
            evidence: Vec::new(),
            section_position: SectionPosition::section("sec_0001"),
            causal_antecedents: Vec::new(),
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    // ── Entity arm ─────────────────────────────────────────

    #[test]
    fn entity_with_defining_quote_is_argument_dominant() {
        let env = entity(
            EntityType::Person,
            0.5,
            vec![],
            Some("X is the practice of Y."),
        );
        let v = classify_articulation(&env, "");
        assert_eq!(v.dominant(), Articulation::Argument);
        assert!(v.argument >= 0.7);
    }

    #[test]
    fn entity_person_high_salience_is_inventory_dominant() {
        let env = entity(EntityType::Person, 0.9, vec![], None);
        let v = classify_articulation(&env, "");
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    #[test]
    fn entity_person_many_aliases_is_inventory_dominant() {
        let env = entity(EntityType::Person, 0.1, vec!["a", "b", "c"], None);
        let v = classify_articulation(&env, "");
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    #[test]
    fn entity_person_low_attestation_still_inventory_but_softer() {
        let env = entity(EntityType::Person, 0.1, vec![], None);
        let v = classify_articulation(&env, "");
        assert_eq!(v.dominant(), Articulation::Inventory);
        assert!(v.argument >= 0.35); // softer Argument component
    }

    #[test]
    fn entity_place_work_institution_follow_person_rule() {
        for et in [EntityType::Place, EntityType::Work, EntityType::Institution] {
            let env = entity(et, 0.9, vec![], None);
            let v = classify_articulation(&env, "");
            assert_eq!(v.dominant(), Articulation::Inventory);
        }
    }

    #[test]
    fn entity_concept_is_argument_dominant() {
        let env = entity(EntityType::Concept, 0.5, vec![], None);
        let v = classify_articulation(&env, "");
        assert_eq!(v.dominant(), Articulation::Argument);
    }

    #[test]
    fn entity_initiative_is_trace_dominant() {
        let env = entity(EntityType::Initiative, 0.7, vec![], None);
        let v = classify_articulation(&env, "");
        assert_eq!(v.dominant(), Articulation::Trace);
    }

    #[test]
    fn entity_other_falls_through_to_preview_classifier() {
        let env = entity(EntityType::Other("article".into()), 0.5, vec![], None);
        let v = classify_articulation(&env, "");
        // Empty preview → Inventory prior.
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    // ── Claim arm ──────────────────────────────────────────

    #[test]
    fn claim_argue_is_argument_dominant() {
        let v = classify_articulation(&claim(DiscourseAct::Argue), "");
        assert_eq!(v.dominant(), Articulation::Argument);
        assert!(v.argument >= 0.85);
    }

    #[test]
    fn claim_assert_object_interpret_warn_argument_dominant() {
        for act in [
            DiscourseAct::Assert,
            DiscourseAct::Object,
            DiscourseAct::Interpret,
            DiscourseAct::Warn,
        ] {
            let v = classify_articulation(&claim(act), "");
            assert_eq!(v.dominant(), Articulation::Argument);
        }
    }

    #[test]
    fn claim_hypothesize_imply_argument_with_trace_tail() {
        for act in [DiscourseAct::Hypothesize, DiscourseAct::Imply] {
            let v = classify_articulation(&claim(act), "");
            assert_eq!(v.dominant(), Articulation::Argument);
            assert!(v.trace >= 0.10);
        }
    }

    #[test]
    fn claim_enact_commit_trace_dominant() {
        for act in [DiscourseAct::Enact, DiscourseAct::Commit] {
            let v = classify_articulation(&claim(act), "");
            assert_eq!(v.dominant(), Articulation::Trace);
        }
    }

    #[test]
    fn claim_other_argument_dominant() {
        let v = classify_articulation(&claim(DiscourseAct::Other("editorialise".into())), "");
        assert_eq!(v.dominant(), Articulation::Argument);
    }

    // ── Event arm ──────────────────────────────────────────

    #[test]
    fn event_action_encounter_external_force_trace_dominant() {
        for et in [
            EventType::Action,
            EventType::Encounter,
            EventType::ExternalForce,
        ] {
            let v = classify_articulation(&event(et), "");
            assert_eq!(v.dominant(), Articulation::Trace);
            assert!(v.trace >= 0.7);
        }
    }

    #[test]
    fn event_realization_decision_argument_and_trace_balanced() {
        for et in [EventType::Realization, EventType::Decision] {
            let v = classify_articulation(&event(et), "");
            // Tied; dominant resolves Inventory→Argument→Trace, so
            // for a 0.10/0.45/0.45 vector the dominant is Argument
            // by tie-break order.
            assert!(matches!(
                v.dominant(),
                Articulation::Argument | Articulation::Trace
            ));
        }
    }

    #[test]
    fn event_publication_trace_dominant_lightly() {
        let v = classify_articulation(&event(EventType::Publication), "");
        assert_eq!(v.dominant(), Articulation::Trace);
    }

    // ── Other atom variants ────────────────────────────────

    #[test]
    fn configuration_position_opposition_are_argument_dominant() {
        let conf = AtomEnvelope::Configuration(Configuration {
            id: AtomId::configuration(1),
            label: "config".into(),
            description: "d".into(),
            constituent_atoms: Vec::new(),
            evidence: Vec::new(),
            confidence: 0.5,
            interpretive_note: "".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(
            classify_articulation(&conf, "").dominant(),
            Articulation::Argument
        );

        let pos = AtomEnvelope::Position(Position {
            id: AtomId::position(1),
            canonical_name: "name".into(),
            content: "the position says X".into(),
            stance: "endorse".into(),
            proponent_id: None,
            evidence_ids: Vec::new(),
            first_appearance: ChunkRef::new("sec_0001", None),
            anchors: Vec::new(),
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(
            classify_articulation(&pos, "").dominant(),
            Articulation::Argument
        );

        let opp = AtomEnvelope::Opposition(Opposition {
            id: AtomId::opposition(1),
            canonical_label: "X vs Y".into(),
            left_atom_id: None,
            left_label: "X".into(),
            right_atom_id: None,
            right_label: "Y".into(),
            axis: "".into(),
            framing: "".into(),
            first_appearance: ChunkRef::new("sec_0001", None),
            anchors: Vec::new(),
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(
            classify_articulation(&opp, "").dominant(),
            Articulation::Argument
        );
    }

    #[test]
    fn state_relation_argument_dominant() {
        let st = AtomEnvelope::State(State {
            id: AtomId::state(1),
            entity_id: AtomId::entity(1),
            label: "s".into(),
            state_type: StateType::Psychological,
            evidence: Vec::new(),
            section_range: SectionRange::point("sec_0001"),
            confidence: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(
            classify_articulation(&st, "").dominant(),
            Articulation::Argument
        );

        let rel = AtomEnvelope::Relation(Relation {
            attributes: Default::default(),
            id: AtomId::relation(1),
            participants: vec![AtomId::entity(1), AtomId::entity(2)],
            label: "r".into(),
            relation_type: RelationType::Interpersonal,
            evidence: Vec::new(),
            section_range: SectionRange::point("sec_0001"),
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(
            classify_articulation(&rel, "").dominant(),
            Articulation::Argument
        );
    }

    #[test]
    fn question_and_argument_reconstruction_argument_dominant() {
        let q = AtomEnvelope::Question(Question {
            id: AtomId::question(1),
            content: "?".into(),
            question_type: QuestionType::Thematic,
            addressed_by: Vec::new(),
            raised_at: Vec::new(),
            resolution_status: ResolutionStatus::Open,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(
            classify_articulation(&q, "").dominant(),
            Articulation::Argument
        );

        let arg = AtomEnvelope::ArgumentReconstruction(ArgumentReconstruction {
            id: AtomId::argument_reconstruction(1),
            name: "Cogito".into(),
            proponent: None,
            premises: Vec::new(),
            conclusion: "c".into(),
            objections: Vec::new(),
            evidence: Vec::new(),
            section_position: SectionPosition::section("sec_0001"),
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(
            classify_articulation(&arg, "").dominant(),
            Articulation::Argument
        );
    }

    // ── chunk-preview fallback ─────────────────────────────

    #[test]
    fn preview_wiki_opener_inventory_dominant() {
        let v = classify_by_chunk_preview(
            "'''Albert Einstein''' is a German-born theoretical physicist who developed the theory of relativity.",
        );
        assert_eq!(v.dominant(), Articulation::Inventory);
        assert!(v.inventory >= 0.7);
    }

    #[test]
    fn preview_first_person_with_date_trace_dominant() {
        let v = classify_by_chunk_preview(
            "Yesterday I was thinking about why we always end up rewriting the same module every quarter.",
        );
        assert_eq!(v.dominant(), Articulation::Trace);
        assert!(v.trace >= 0.7);
    }

    #[test]
    fn preview_first_person_no_date_still_trace() {
        let v = classify_by_chunk_preview(
            "I think the right move here is to stop chasing this rabbit hole and ship what we have.",
        );
        assert_eq!(v.dominant(), Articulation::Trace);
    }

    #[test]
    fn preview_argumentative_connectives_argument_dominant() {
        let v = classify_by_chunk_preview(
            "Therefore, the claim that supply-side reforms cause growth is contradicted by the 1990s data.",
        );
        assert_eq!(v.dominant(), Articulation::Argument);
    }

    #[test]
    fn preview_markdown_heading_inventory_dominant() {
        let v = classify_by_chunk_preview("## Section heading\n\nContent below.");
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    #[test]
    fn preview_bullet_list_inventory_dominant() {
        let v = classify_by_chunk_preview("- alpha\n- beta\n- gamma");
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    #[test]
    fn preview_multibyte_char_at_byte_cap_does_not_panic() {
        // An em-dash straddling byte 600 must not panic the slice — a
        // real Wikipedia chunk ("Consequentialism … —") hit this.
        let mut s = "a".repeat(598);
        s.push('—'); // occupies bytes 598..601, straddling the 600 cap
        s.push_str(" and considerably more text after the boundary point");
        let _ = classify_by_chunk_preview(&s); // must not panic
    }

    #[test]
    fn preview_empty_returns_inventory_prior() {
        let v = classify_by_chunk_preview("");
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    #[test]
    fn preview_neutral_content_inventory_prior() {
        // No markers fire; falls back to Inventory prior.
        let v = classify_by_chunk_preview(
            "The system has multiple components that interact through documented interfaces.",
        );
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    // ── integration: Other(_) entity falls through correctly ───

    #[test]
    fn wiki_article_entity_classifies_via_preview() {
        let env = entity(EntityType::Other("article".into()), 0.5, vec![], None);
        let v = classify_articulation(
            &env,
            "'''Isaac Newton''' was an English mathematician, physicist, and astronomer.",
        );
        assert_eq!(v.dominant(), Articulation::Inventory);
    }

    #[test]
    fn other_entity_with_journal_preview_classifies_trace() {
        let env = entity(EntityType::Other("note".into()), 0.5, vec![], None);
        let v = classify_articulation(
            &env,
            "Today I worked through the canonical-entity boost design and realised priority dials don't help.",
        );
        assert_eq!(v.dominant(), Articulation::Trace);
    }
}
