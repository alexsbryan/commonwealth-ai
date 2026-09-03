// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase-1 section-extraction reader — one model response → one
//! [`Phase1ChapterResult`].
//!
//! Cleans the response, sanitises the array fields whose schema declares
//! objects, hands the rest to the lenient wire layer in
//! [`super::sketch_parse`], scrubs schema echo, refuses a section that
//! extracted nothing, and derives the legacy `questions` / `thematic_carriers`
//! / `plot` fields the older phases still read.
//!
//! The reader's behavioural contract — what survives a declared ontology and
//! what does not — is tested here, at the entry point, because that is the
//! surface every caller actually uses. What each `Raw*` shape does is in
//! `sketch_parse`; what the ontology demands is in [`super::parse_policy`].

use super::super::text_helpers::is_placeholder_literal;
use super::super::types::Phase1ChapterResult;
use super::literary::{prepare_phase_json, sanitize_optional_string};
use super::literary_atlas::{
    first_event_description, first_text_level_claim, sanitize_phase1_object_arrays,
    scrub_placeholder_strings,
};
use super::parse_policy::ParsePolicy;
use super::sketch_parse::RawSectionExtraction;
use crate::error::{Error, Result};

/// Read one Phase-1 response into a [`Phase1ChapterResult`] under `policy`.
///
/// Was the body of `LiteraryAtlasPipeline::parse_phase1`; the trait method is
/// now the genre dispatch plus a call to this with [`ParsePolicy::default`].
/// A genre whose ontology declares types passes its own policy, which is the
/// only thing that changes what survives.
pub(super) fn parse_phase1_section_extraction(
    response: &str,
    policy: &ParsePolicy,
) -> Result<Phase1ChapterResult> {
    let cleaned = prepare_phase_json(response, "phase 1 (atlas)")?;

    // Two-step deserialization: parse to `serde_json::Value`
    // first (which silently keeps the last value when the model
    // emits the same key twice, observed on Gemma-31B), then
    // sanitize array fields whose schema declares objects but
    // where the model occasionally drops in a `"//"` comment
    // string or other non-object literal. Only after this
    // cleaning pass do we deserialize into the typed Raw
    // layout. Without this pre-pass a single duplicate field or
    // hallucinated comment string costs the whole section.
    let mut value: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!("phase 1 (atlas) response is not valid JSON: {e}"))
    })?;
    sanitize_phase1_object_arrays(&mut value);

    // Deserialize through a lenient Raw layout that tolerates
    // common model-compliance drift — an individual claim missing
    // `epistemic_status`, a lone null in an array, an unknown
    // enum tag — so a single bad claim doesn't throw away the
    // rest of a chapter's extraction. Hard-failing on shape only
    // makes sense when the response as a whole is unusable.
    let raw: RawSectionExtraction = serde_json::from_value(value).map_err(|e| {
        Error::Serialization(format!("phase 1 (atlas) response is not valid JSON: {e}"))
    })?;
    let mut extraction = raw.into_extraction(policy);

    // Reject the common failure mode where the model echoes the
    // schema placeholder for section_id instead of stamping the
    // real one.
    if extraction.section_id.trim().is_empty() || is_placeholder_literal(&extraction.section_id) {
        // Section id is stamped by the runner from the chapter
        // input anyway — we don't care what the model put here
        // as long as we can see it isn't vacant for debugging.
        extraction.section_id = String::new();
    }

    // Scrub placeholder literals inside string fields. A `"..."`
    // in an `evidence_preview` or `description` slot is schema
    // echo, not an answer.
    scrub_placeholder_strings(&mut extraction);

    // A section with zero atoms from a literary chapter is almost
    // always a parse quality failure — the model skipped the
    // extraction. Surface it as an error so the run file captures
    // the raw response head for post-mortem.
    if extraction.has_no_atoms() {
        return Err(Error::Serialization(
            "phase 1 (atlas) produced no entities, states, relations, \
             events, claims, or questions — the model did not extract \
             anything usable. Check the raw response head for schema \
             echo, truncated output, or a refusal."
                .into(),
        ));
    }

    // Derive the legacy `questions` / `thematic_carriers` /
    // `setting` / `plot` fields from the atlas extraction so the
    // existing Phase 2/3/4/5 flow still functions against the
    // atlas output. These are back-compat bridges, not the
    // preferred view of the data.
    let questions: Vec<String> = extraction
        .questions_raised
        .iter()
        .map(|q| q.content.trim().to_string())
        .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
        .collect();

    if questions.is_empty() {
        return Err(Error::Serialization(
            "phase 1 (atlas) response has no questions_raised — at \
             least one thematic question is required so downstream \
             clustering has something to align on."
                .into(),
        ));
    }

    let thematic_carriers: Vec<String> = extraction
        .entities_introduced
        .iter()
        .map(|e| e.canonical_name.trim().to_string())
        .chain(
            extraction
                .entities_developed
                .iter()
                .map(|e| e.entity_name.trim().to_string()),
        )
        .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
        .fold(Vec::<String>::new(), |mut acc, name| {
            if !acc.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                acc.push(name);
            }
            acc
        });

    let plot = first_event_description(&extraction);
    let reveals = first_text_level_claim(&extraction);

    Ok(Phase1ChapterResult {
        questions,
        reveals: sanitize_optional_string(reveals),
        thematic_carriers,
        setting: None,
        plot: sanitize_optional_string(plot),
        section_extraction: Some(extraction),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::{DiscourseAct, EntityType, SectionExtraction};

    use super::super::numismatics_policies as numismatics;

    fn policy() -> ParsePolicy {
        ParsePolicy::from_policies(&numismatics())
    }

    fn parse(json: &str, policy: &ParsePolicy) -> SectionExtraction {
        parse_phase1_section_extraction(json, policy)
            .expect("canned response parses")
            .section_extraction
            .expect("the atlas parser always attaches a section extraction")
    }

    /// One coin with every attribute family the template declares, plus the
    /// question the parser requires.
    const COIN_RESPONSE: &str = r#"{
      "section_id": "sec_0001",
      "entities_introduced": [
        {
          "canonical_name": "Series R sceatta",
          "entity_type": "coin",
          "description": "A silver penny of the Hamwic mint.",
          "anchor": "Series R sceatta",
          "attributes": {
            "weight": 1.29,
            "metal": "Silver",
            "mint": "Hamwic",
            "struck": "c. 720-750",
            "denomination": "penny"
          }
        }
      ],
      "questions_raised": [{"content": "Which mint struck Series R?", "anchor": "Series R"}]
    }"#;

    /// The probe from the order. Before P2 this dropped the atom: every
    /// declared entity type arrives as `EntityType::Other`, and the reader
    /// dropped every `Other` as a model hedge.
    #[test]
    fn parse_phase1_keeps_declared_entity_type() {
        let e = parse(COIN_RESPONSE, &policy());
        assert_eq!(e.entities_introduced.len(), 1, "the coin survived");
        let coin = &e.entities_introduced[0];
        assert_eq!(coin.entity_type, EntityType::Other("coin".into()));
        assert_eq!(coin.attributes["weight"].as_f64(), Some(1.29));
        // A closed set answers in the DECLARED spelling, not the model's.
        assert_eq!(coin.attributes["metal"].as_str(), Some("silver"));
        assert_eq!(coin.attributes["mint"].as_str(), Some("Hamwic"));
        assert_eq!(coin.attributes["struck"].as_str(), Some("c. 720-750"));
    }

    /// The same response under the default policy is today's behaviour: an
    /// `Other` entity type is a hedge and the atom is dropped. This is the
    /// red half of the probe — without it, "the coin survived" could be
    /// true for a reason that has nothing to do with the declaration.
    #[test]
    fn default_policy_still_drops_an_undeclared_entity_type() {
        let e = parse(COIN_RESPONSE, &ParsePolicy::default());
        assert!(
            e.entities_introduced.is_empty(),
            "an undeclared corpus has no coin type to keep"
        );
    }

    /// Case and separator drift must not cost the atom, and the atom is
    /// stored under the spelling the RECIPE declared — so the subtype column
    /// says `coin` whatever the model wrote. `EntityType::from_str_repr`
    /// already forgives this for the six named variants; matching declared
    /// names by equality made a declared type stricter than a generic one.
    #[test]
    fn a_declared_type_survives_a_case_drifted_tag() {
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [
                {"canonical_name": "Series R", "entity_type": "Coin", "anchor": "a",
                 "attributes": {"weight": 1.29}},
                {"canonical_name": "Hamwic", "entity_type": " MINT ", "anchor": "b"}
              ],
              "claims": [{
                "content": "Series R was struck at Hamwic.",
                "claim_kind": "Attribution", "anchor": "x"
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert_eq!(e.entities_introduced.len(), 2, "neither atom was lost");
        assert_eq!(
            e.entities_introduced[0].entity_type,
            EntityType::Other("coin".into()),
            "stored under the declared spelling, not the model's"
        );
        assert_eq!(
            e.entities_introduced[0].attributes["weight"].as_f64(),
            Some(1.29),
            "and its attributes still validate against the declared type"
        );
        assert_eq!(
            e.entities_introduced[1].entity_type,
            EntityType::Other("mint".into())
        );
        assert_eq!(e.claims[0].claim_kind.as_deref(), Some("attribution"));
    }

    #[test]
    fn drops_an_attribute_the_type_does_not_declare() {
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [{
                "canonical_name": "Series R sceatta", "entity_type": "coin", "anchor": "a",
                "attributes": {"weight": 1.29, "die_axis": "6h"}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        let attrs = &e.entities_introduced[0].attributes;
        assert!(attrs.contains_key("weight"));
        assert!(!attrs.contains_key("die_axis"), "undeclared key dropped");
    }

    #[test]
    fn drops_an_attribute_whose_value_is_not_of_the_declared_family() {
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [{
                "canonical_name": "Series R sceatta", "entity_type": "coin", "anchor": "a",
                "attributes": {"weight": "heavy", "metal": "adamantium"}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        let attrs = &e.entities_introduced[0].attributes;
        assert!(!attrs.contains_key("weight"), "not a number");
        assert!(!attrs.contains_key("metal"), "not in the declared values");
    }

    /// A model that writes the unit back into a quantity is recovered, not
    /// dropped. Under the enforced `json_schema` grammar a quantity arrives as
    /// a number; this is the `json-object` provider path, where the parser is
    /// the only place it can be caught.
    #[test]
    fn quantity_recovers_a_unit_suffixed_string() {
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [{
                "canonical_name": "Series R sceatta", "entity_type": "coin", "anchor": "a",
                "attributes": {"weight": "1.29 g"}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert_eq!(
            e.entities_introduced[0].attributes["weight"].as_f64(),
            Some(1.29)
        );
    }

    /// `sceatta specializes coin`, so it accepts coin's attributes without
    /// re-declaring them.
    #[test]
    fn a_specializing_type_inherits_its_parents_attributes() {
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [{
                "canonical_name": "Series R", "entity_type": "sceatta", "anchor": "a",
                "attributes": {"weight": 1.19}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert_eq!(
            e.entities_introduced[0].entity_type,
            EntityType::Other("sceatta".into())
        );
        assert_eq!(
            e.entities_introduced[0].attributes["weight"].as_f64(),
            Some(1.19)
        );
    }

    /// A declared claim takes its discourse act from the type's `force`, and
    /// keeps `claim_kind` + `subject`. The model's own `discourse_act` is not
    /// consulted — here it says `imply` and the declaration says assertive.
    #[test]
    fn declared_claim_takes_discourse_act_from_force_and_keeps_subject() {
        let e = parse(
            r#"{
              "section_id": "s",
              "claims": [{
                "content": "Series R was struck at Hamwic.",
                "discourse_act": "imply",
                "claim_kind": "attribution",
                "subject": "Series R sceatta",
                "anchor": "struck at Hamwic",
                "attributes": {"proposed_date": "c. 720-750", "grade": "die-link"}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert_eq!(e.claims.len(), 1);
        let c = &e.claims[0];
        assert_eq!(c.discourse_act, DiscourseAct::Assert);
        assert_eq!(c.claim_kind.as_deref(), Some("attribution"));
        assert_eq!(c.subject.as_deref(), Some("Series R sceatta"));
        assert_eq!(c.attributes["proposed_date"].as_str(), Some("c. 720-750"));
        assert_eq!(c.attributes["grade"].as_str(), Some("die-link"));
    }

    /// The prompt says to leave `attributed_to` out; the model writes the
    /// word. Falsifier: remove the marker filter in `sketch_parse` and
    /// "omit" becomes a scholar the resolver then fails to find (22 of 40
    /// claims on one build).
    #[test]
    fn the_word_omit_is_not_an_attribution() {
        let e = parse(
            r#"{
              "section_id": "s",
              "claims": [
                {"content": "A.", "discourse_act": "assert", "attributed_to": "omit", "anchor": "a"},
                {"content": "B.", "discourse_act": "assert", "attributed_to": "Halstead", "anchor": "b"}
              ],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert_eq!(e.claims.len(), 2);
        assert_eq!(e.claims[0].attributed_to, None);
        assert_eq!(e.claims[1].attributed_to.as_deref(), Some("Halstead"));
    }

    /// A date slot holding "unknown" reads downstream as a date. Falsifier:
    /// drop `is_absent_marker` from the Time branch and the placeholder
    /// lands beside real dates in the fill-rate report.
    #[test]
    fn a_placeholder_date_is_an_absent_date() {
        let e = parse(
            r#"{
              "section_id": "s",
              "claims": [{
                "content": "Series X is undated.",
                "discourse_act": "assert",
                "claim_kind": "attribution",
                "anchor": "undated",
                "attributes": {"proposed_date": "unknown", "grade": "stylistic"}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        let c = &e.claims[0];
        assert!(
            !c.attributes.contains_key("proposed_date"),
            "{:?}",
            c.attributes
        );
        assert_eq!(c.attributes["grade"].as_str(), Some("stylistic"));
    }

    #[test]
    fn drops_a_grade_the_claim_type_does_not_declare() {
        let e = parse(
            r#"{
              "section_id": "s",
              "claims": [{
                "content": "Series R was struck at Hamwic.",
                "claim_kind": "attribution", "anchor": "x",
                "attributes": {"grade": "vibes"}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert!(!e.claims[0].attributes.contains_key("grade"));
    }

    /// A declared claim with no anchor is evidence of nothing; it is dropped
    /// rather than persisted unanchored.
    #[test]
    fn rejects_an_anchorless_declared_claim() {
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [{"canonical_name": "Hamwic", "entity_type": "mint", "anchor": "a"}],
              "claims": [{"content": "Series R was struck at Hamwic.", "claim_kind": "attribution"}],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert!(e.claims.is_empty(), "no anchor, nothing grounds it");
        assert_eq!(
            e.entities_introduced.len(),
            1,
            "the rest of the section survives"
        );
    }

    /// A voice is a speaker role, not subject matter: no entity for it, and
    /// no claim attributed to it. The claim itself survives.
    #[test]
    fn drops_a_declared_voice_entity_and_clears_its_attribution() {
        let mut policies = numismatics();
        policies.assertion.voices.not_entities = vec!["the cataloguer".into()];
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [
                {"canonical_name": "The Cataloguer", "entity_type": "person", "anchor": "a"},
                {"canonical_name": "Hamwic", "entity_type": "mint", "anchor": "b"}
              ],
              "claims": [{
                "content": "Series R was struck at Hamwic.",
                "claim_kind": "attribution", "attributed_to": "the cataloguer", "anchor": "x"
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &ParsePolicy::from_policies(&policies),
        );
        let names: Vec<&str> = e
            .entities_introduced
            .iter()
            .map(|x| x.canonical_name.as_str())
            .collect();
        assert_eq!(names, vec!["Hamwic"], "the voice is not an entity");
        assert_eq!(e.claims.len(), 1);
        assert_eq!(e.claims[0].attributed_to, None, "no atom to attribute to");
    }

    /// An entity type the ontology does not declare is still a hedge.
    #[test]
    fn drops_an_entity_type_the_ontology_does_not_declare() {
        let e = parse(
            r#"{
              "section_id": "s",
              "entities_introduced": [
                {"canonical_name": "Crondall hoard", "entity_type": "hoard", "anchor": "a"},
                {"canonical_name": "Hamwic", "entity_type": "mint", "anchor": "b"}
              ],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert_eq!(e.entities_introduced.len(), 1);
        assert_eq!(e.entities_introduced[0].canonical_name, "Hamwic");
    }

    /// An undeclared relation type leaves the relation UNCLASSIFIED — it
    /// does not drop the atom, and it does not invent a type.
    #[test]
    fn an_undeclared_relation_type_leaves_the_relation_untyped() {
        let e = parse(
            r#"{
              "section_id": "s",
              "relations_introduced": [{
                "participants": ["Series R sceatta", "Hamwic"],
                "label": "struck at", "anchor": "a",
                "relation_type": "struck_at", "attributes": {"certainty": "high"}
              }],
              "questions_raised": [{"content": "q"}]
            }"#,
            &policy(),
        );
        assert_eq!(e.relations_introduced.len(), 1);
        assert_eq!(e.relations_introduced[0].relation_type, None);
        assert!(e.relations_introduced[0].attributes.is_empty());
    }
}
