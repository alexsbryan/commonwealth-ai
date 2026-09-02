// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase-1 section-extraction reader — the lenient `Raw*` layer and the
//! parse that turns one model response into a [`SectionExtraction`].
//!
//! Split out of `literary_atlas.rs` (which owned both the composer and the
//! reader) so the reader sits next to the ontology policy that parameterises
//! it. Nothing about the bytes changed in the split; the pin is
//! `tests/main/ontology_prompt_snapshots.rs` plus the parse tests that stayed
//! in `literary_atlas.rs` and still drive this code through
//! `Pipeline::parse_phase1`.

use serde::Deserialize;
use tracing::debug;

use super::super::atlas::{
    ClaimSketch, DiscourseAct, EntitySketch, EntityStateSketch, EntityType, EpistemicStatus,
    EventSketch, QuestionSketch, RelationSketch, RelationStateSketch, SectionExtraction,
};
use super::super::text_helpers::is_placeholder_literal;
use super::super::types::Phase1ChapterResult;
use super::literary::{prepare_phase_json, sanitize_optional_string};
use super::literary_atlas::{
    first_event_description, first_text_level_claim, phase3_metadata_value_to_string,
    sanitize_phase1_object_arrays, scrub_placeholder_strings,
};
use super::parse_policy::{
    canonical_name, declared_attributes, declared_type, validated_attributes, validated_choice,
    ClaimTypeRules, ParsePolicy, ATTR_DEONTIC, ATTR_GRADE,
};
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

// ── Lenient deserialisation layer ────────────────────────────
//
// Models drift on schema compliance: a claim drops a required field,
// an alias list has a stray null, a relation arrives with one
// participant. Hard-failing on any of these loses a whole chapter's
// extraction to save one malformed atom. The `Raw*` structs mirror
// the Phase 1 sketch shapes but accept optional required fields and
// drop nulls inside arrays, logging each drop so the prompt-compliance
// signal shows up in tracing without failing the run.
//
// Classification enums that moved to Phase 5 (state_type, event_type,
// relation_type, scope, question_type) are NOT present in these Raw
// structs — models are instructed to omit them, and the sketches on
// disk don't carry them.

pub(super) fn vec_of_some<T>(v: Vec<Option<T>>) -> Vec<T> {
    v.into_iter().flatten().collect()
}

/// Accept `null` as the empty value for a `Vec<…>` field. Models routinely
/// emit `"aliases": null` (rather than omitting the key) for a missing
/// optional sequence, and serde's default deserializer rejects null with
/// `invalid type: null, expected a sequence`. This helper unwraps such
/// values to `Vec::new()` so a single null-instead-of-omit doesn't kill
/// the whole section parse.
pub(super) fn null_or_empty_vec<'de, D, T>(d: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    use serde::Deserialize;
    let opt: Option<Vec<T>> = Option::deserialize(d)?;
    std::result::Result::Ok(opt.unwrap_or_default())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSectionExtraction {
    section_id: String,
    #[serde(deserialize_with = "null_or_empty_vec")]
    entities_introduced: Vec<Option<RawEntitySketch>>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    entities_developed: Vec<Option<RawEntityStateSketch>>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    relations_introduced: Vec<Option<RawRelationSketch>>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    relations_developed: Vec<Option<RawRelationStateSketch>>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    events: Vec<Option<RawEventSketch>>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    claims: Vec<Option<RawClaimSketch>>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    questions_raised: Vec<Option<RawQuestionSketch>>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    argument_reconstructions: Vec<Option<RawArgumentReconstructionSketch>>,
}

impl RawSectionExtraction {
    fn into_extraction(self, policy: &ParsePolicy) -> SectionExtraction {
        SectionExtraction {
            section_id: self.section_id,
            // Pin depth at `Extracted` — the atlas pipeline is by
            // definition the extraction-first ingestion strategy.
            // A structure-first strategy would build its
            // `SectionExtraction` records with
            // `EnrichmentDepth::Structural` instead.
            enrichment_depth: crate::enrichment::pipeline::atlas::EnrichmentDepth::Extracted,
            entities_introduced: vec_of_some(self.entities_introduced)
                .into_iter()
                .filter_map(|s| s.into_sketch(policy))
                .collect(),
            entities_developed: vec_of_some(self.entities_developed)
                .into_iter()
                .filter_map(RawEntityStateSketch::into_sketch)
                .collect(),
            relations_introduced: vec_of_some(self.relations_introduced)
                .into_iter()
                .filter_map(|s| s.into_sketch(policy))
                .collect(),
            relations_developed: vec_of_some(self.relations_developed)
                .into_iter()
                .filter_map(RawRelationStateSketch::into_sketch)
                .collect(),
            events: vec_of_some(self.events)
                .into_iter()
                .filter_map(|s| s.into_sketch(policy))
                .collect(),
            claims: vec_of_some(self.claims)
                .into_iter()
                .filter_map(|s| s.into_sketch(policy))
                .collect(),
            questions_raised: vec_of_some(self.questions_raised)
                .into_iter()
                .filter_map(RawQuestionSketch::into_sketch)
                .collect(),
            argument_reconstructions: vec_of_some(self.argument_reconstructions)
                .into_iter()
                .filter_map(RawArgumentReconstructionSketch::into_sketch)
                .collect(),
            // RawSectionExtraction is the literary-atlas parser; the
            // routed Phase 1 dispatcher (obsidian_atlas) is the only
            // path that ever populates `type_extension`. Leave it
            // None here; the dispatcher merges in the typed payload
            // post-parse when classification routes to a non-Fiction
            // type.
            type_extension: None,
            type_extensions: Vec::new(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEntitySketch {
    canonical_name: String,
    #[serde(deserialize_with = "null_or_empty_vec")]
    aliases: Vec<Option<String>>,
    entity_type: Option<EntityType>,
    description: String,
    defining_quote: Option<String>,
    anchor: String,
    /// Declared-type attributes (ontology v1). Absent for every undeclared
    /// corpus, and dropped wholesale by the parser when the type declares
    /// none — the schema only offers this object when types are declared.
    attributes: serde_json::Map<String, serde_json::Value>,
}

impl RawEntitySketch {
    fn into_sketch(self, policy: &ParsePolicy) -> Option<EntitySketch> {
        let name = self.canonical_name.trim().to_string();
        if name.is_empty() {
            debug!("literary_atlas: dropping entity sketch — canonical_name missing");
            return None;
        }
        // A declared voice is a speaker, not subject matter. Enforced here
        // because a prompt asking a model not to emit one is a request, and
        // this is an invariant (§7.6).
        if policy.is_voice(&name) {
            debug!(
                entity = %name,
                "ontology parse: dropping entity sketch — the ontology declares this a voice, \
                 not subject matter"
            );
            return None;
        }
        // Reject the type-evasion failure mode. The daemon's
        // grammar-constrained sampler is a known no-op (see
        // sovereign-inference/embedded.rs build_sampler comment), so
        // schema-side `enum` constraints don't reach the model. The
        // parser is the only place we can enforce "must commit to one
        // of the 5 named variants" — without enforcement, models hedge
        // borderline cases (the Narrator, an unnamed character, an
        // abstract "the household") with entity_type:"unspecified",
        // which evades both forbidden-rule scoring and expected-type
        // recall. Dropping the atom rather than persisting it as
        // Other(_) is the correct trade: a borderline case the model
        // can't classify is not load-bearing and shouldn't pollute
        // the atlas. If the model wants to emit such an atom, it has
        // to commit to a typing.
        let entity_type = match self.entity_type {
            // A declared type reaches the atom as `Other(<declared name>)`.
            // Without this the probe fails by construction: every declared
            // entity type is an `Other` tag, and this arm dropped those.
            // Matched through `canonical_name`, so the atom is stored under
            // the spelling the RECIPE declared however the model cased it.
            Some(EntityType::Other(s)) => {
                let Some(declared) = canonical_name(&policy.entity_types, &s) else {
                    debug!(
                        "literary_atlas: dropping entity sketch '{name}' — \
                         entity_type='{s}' is neither one of the 6 named variants \
                         nor a declared type (model hedged on typing)"
                    );
                    return None;
                };
                EntityType::Other(declared)
            }
            None => {
                debug!(
                    "literary_atlas: dropping entity sketch '{name}' — \
                     entity_type missing (model didn't commit to a typing)"
                );
                return None;
            }
            Some(et) => et,
        };
        // Retype obvious -ism/-ethics names from person → concept.
        // Models occasionally mark school names that appear repeatedly
        // ("virtue ethics", "situationism") as Person when no explicit
        // introduction line establishes them. The `-ism`/`-ianism`/
        // `ethics` suffix is structurally unambiguous in our corpora —
        // no real proper names take these endings in the texts we
        // process — so the retype is conservative and reverses a known
        // failure mode without affecting concept-typed entries the
        // model already got right.
        let entity_type = if matches!(entity_type, EntityType::Person) && is_position_suffix(&name)
        {
            EntityType::Concept
        } else {
            entity_type
        };
        // Only keep `defining_quote` on `concept` entities. Models
        // occasionally lift a person's quoted line into the field;
        // it's not what the field is for and it confuses downstream
        // retrieval. Person/work/institution/place atoms drop the
        // value silently — the description carries the gloss.
        let defining_quote = if matches!(entity_type, EntityType::Concept) {
            self.defining_quote
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };
        // Attributes validate against the EFFECTIVE declarations of the
        // type the atom actually landed on — so `sceatta` accepts `coin`'s
        // `weight`, and a generic `person` in a declared corpus accepts what
        // the recipe declared for `person`.
        let attributes = validated_attributes(
            declared_attributes(&policy.entity_types, entity_type.as_str_repr()),
            self.attributes,
            "entity",
            &name,
        );
        Some(EntitySketch {
            attributes,
            canonical_name: name,
            aliases: vec_of_some(self.aliases),
            entity_type,
            description: self.description,
            defining_quote,
            anchor: self.anchor,
        })
    }
}

fn is_position_suffix(name: &str) -> bool {
    let lc = name.trim().to_lowercase();
    // Singular -ism / -ianism (compatibilism, deontology …no, that's
    // -ology), -ology (deontology, epistemology, theology), and the
    // " ethics" / "-ethics" patterns. Plural forms catch model-emitted
    // group names: "Epicureans", "Aristotelians", "Stoicists", and
    // misspellings like "Aristotleians". We deliberately do NOT match
    // singular `-ist`, `-ian`, or `-ean`: real proper names use those
    // endings ("Christian", "Sebastian", "Epstein") and a singular
    // misclassification is far rarer than a plural-school one.
    lc.ends_with("ism")
        || lc.ends_with("ianism")
        || lc.ends_with("ology")
        || lc.ends_with("ologies")
        || lc.ends_with(" ethics")
        || lc.ends_with("-ethics")
        || lc.ends_with("ians")
        || lc.ends_with("eans")
        || lc.ends_with("ists")
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEntityStateSketch {
    entity_name: String,
    label: String,
    anchor: String,
}

impl RawEntityStateSketch {
    fn into_sketch(self) -> Option<EntityStateSketch> {
        let entity = self.entity_name.trim().to_string();
        let label = self.label.trim().to_string();
        if entity.is_empty() || label.is_empty() {
            debug!(
                "literary_atlas: dropping entity state sketch — entity_name={:?} label={:?}",
                entity, label
            );
            return None;
        }
        Some(EntityStateSketch {
            entity_name: entity,
            label,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRelationSketch {
    #[serde(deserialize_with = "null_or_empty_vec")]
    participants: Vec<Option<String>>,
    label: String,
    anchor: String,
    /// Declared relation type name (ontology v1).
    relation_type: Option<String>,
    /// Declared-type attributes; see [`RawEntitySketch::attributes`].
    attributes: serde_json::Map<String, serde_json::Value>,
}

impl RawRelationSketch {
    fn into_sketch(self, policy: &ParsePolicy) -> Option<RelationSketch> {
        let participants = vec_of_some(self.participants);
        let label = self.label.trim().to_string();
        if participants.len() < 2 || label.is_empty() {
            debug!(
                "literary_atlas: dropping relation sketch — participants={} label={:?}",
                participants.len(),
                label
            );
            return None;
        }
        let relation_type = declared_type(
            &policy.relation_types,
            self.relation_type,
            "relation",
            &label,
        );
        let decls = declared_attributes(
            &policy.relation_types,
            relation_type.as_deref().unwrap_or_default(),
        );
        let attributes = validated_attributes(decls, self.attributes, "relation", &label);
        Some(RelationSketch {
            attributes,
            relation_type,
            participants,
            label,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRelationStateSketch {
    #[serde(deserialize_with = "null_or_empty_vec")]
    participants: Vec<Option<String>>,
    label: String,
    anchor: String,
}

impl RawRelationStateSketch {
    fn into_sketch(self) -> Option<RelationStateSketch> {
        let participants = vec_of_some(self.participants);
        let label = self.label.trim().to_string();
        if participants.len() < 2 || label.is_empty() {
            debug!(
                "literary_atlas: dropping relation state sketch — participants={} label={:?}",
                participants.len(),
                label
            );
            return None;
        }
        Some(RelationStateSketch {
            participants,
            label,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEventSketch {
    description: String,
    #[serde(deserialize_with = "null_or_empty_vec")]
    participants: Vec<Option<String>>,
    anchor: String,
    /// Declared event type name (ontology v1).
    event_type: Option<String>,
    /// Declared-type attributes; see [`RawEntitySketch::attributes`].
    attributes: serde_json::Map<String, serde_json::Value>,
}

impl RawEventSketch {
    fn into_sketch(self, policy: &ParsePolicy) -> Option<EventSketch> {
        let description = self.description.trim().to_string();
        if description.is_empty() {
            debug!("literary_atlas: dropping event sketch — description missing");
            return None;
        }
        let event_type = declared_type(&policy.event_types, self.event_type, "event", &description);
        let decls = declared_attributes(
            &policy.event_types,
            event_type.as_deref().unwrap_or_default(),
        );
        let attributes = validated_attributes(decls, self.attributes, "event", &description);
        Some(EventSketch {
            attributes,
            event_type,
            description,
            participants: vec_of_some(self.participants),
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawClaimSketch {
    content: String,
    discourse_act: Option<DiscourseAct>,
    epistemic_status: Option<EpistemicStatus>,
    // Tolerant: the prompt asks for a single string, but Qwopus-27B
    // and other big-model variants sometimes emit a co-author array
    // (`["Author A", "Author B"]`). Phase 1 should not lose the
    // claim over a stylistic drift in attribution shape — flatten
    // arrays via `phase3_metadata_value_to_string` so the same
    // adapter that hardened Phase 3 metadata also works here.
    attributed_to: Option<serde_json::Value>,
    quotable_excerpt: Option<String>,
    anchor: String,
    /// Declared claim type name (ontology v1).
    claim_kind: Option<String>,
    /// Name of what the claim is ABOUT, when the type declares a `subject`.
    /// Kept as a NAME here; P3 resolves it to an atom id the way
    /// `attributed_to` is resolved.
    subject: Option<String>,
    /// Declared-type attributes plus the reserved `deontic` / `grade` keys.
    attributes: serde_json::Map<String, serde_json::Value>,
}

impl RawClaimSketch {
    fn into_sketch(self, policy: &ParsePolicy) -> Option<ClaimSketch> {
        let content = self.content.trim().to_string();
        if content.is_empty() {
            debug!("literary_atlas: dropping claim sketch — content missing");
            return None;
        }
        // Which declared claim type is this, if any? A corpus declaring
        // exactly one type answers for a model that omitted the key.
        let named = self
            .claim_kind
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let claim_kind = match named {
            // A kind the model named but nobody declared falls through to the
            // generic claim. It does NOT take the default: the model committed
            // to something specific and wrong, and substituting silently is
            // exactly what §18.3 forbids.
            Some(k) => {
                let canonical = canonical_name(&policy.claim_types, &k);
                if canonical.is_none() && !policy.claim_types.is_empty() {
                    debug!(
                        claim = %content, named = %k,
                        "ontology parse: claim_kind names no declared claim type; \
                         reading it as a generic claim"
                    );
                }
                canonical
            }
            None => policy.default_claim_kind.clone(),
        };
        // A `match` rather than `and_then`: the lookup borrows `policy`, and
        // `claim_kind` is moved into the sketch below.
        let rules: Option<&ClaimTypeRules> = match claim_kind.as_deref() {
            Some(k) => policy.claim_types.get(k),
            None => None,
        };

        let discourse_act = match rules {
            // Declared: the force on the type decides. The model's guess is
            // not consulted — a `rule` is enacted because the recipe says
            // what a rule does, not because the extractor agreed (§7.6).
            Some(r) => r.discourse_act.clone(),
            // `discourse_act` is the field we refuse to default — it carries
            // the information the atlas uses to calibrate downstream
            // language ("argued" vs "enacted" vs "implied"). Dropping
            // claims without it preserves that invariant while keeping the
            // rest of the chapter.
            None => {
                let Some(act) = self.discourse_act else {
                    debug!("literary_atlas: dropping claim sketch — discourse_act missing");
                    return None;
                };
                act
            }
        };
        // A declared claim must be anchored: it is the corpus's evidence for
        // an assertion the domain will be asked about, and an unanchored one
        // cannot be checked. Generic claims keep today's tolerance.
        if rules.is_some() && self.anchor.trim().is_empty() {
            debug!(
                claim = %content,
                "ontology parse: dropping declared claim — no anchor, so nothing grounds it"
            );
            return None;
        }
        // `epistemic_status` has a sensible narrative-prose default —
        // the text commits unless it signals otherwise. Defaulting is
        // preferable to losing the claim.
        let epistemic_status = self.epistemic_status.unwrap_or(EpistemicStatus::Confident);
        let attributed_to = self
            .attributed_to
            .and_then(phase3_metadata_value_to_string)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            // A voice is a speaker role, not an entity; there is no atom to
            // attribute to. The claim survives, the attribution does not.
            .filter(|s| {
                let voice = policy.is_voice(s);
                if voice {
                    debug!(
                        claim = %content, attributed_to = %s,
                        "ontology parse: clearing attribution — the ontology declares this a voice"
                    );
                }
                !voice
            });
        let quotable_excerpt = self
            .quotable_excerpt
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let subject = self
            .subject
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && rules.is_some_and(|r| r.has_subject));
        let attributes = match rules {
            Some(r) => {
                let mut raw = self.attributes;
                let mut out = serde_json::Map::new();
                validated_choice(
                    &mut raw,
                    &mut out,
                    ATTR_DEONTIC,
                    &r.deontic_names(),
                    &content,
                );
                validated_choice(&mut raw, &mut out, ATTR_GRADE, &r.grades, &content);
                out.extend(validated_attributes(&r.attributes, raw, "claim", &content));
                out
            }
            None => validated_attributes(&[], self.attributes, "claim", &content),
        };
        Some(ClaimSketch {
            attributes,
            claim_kind,
            subject,
            scope: rules.and_then(|r| r.scope.clone()),
            content,
            discourse_act,
            epistemic_status,
            attributed_to,
            quotable_excerpt,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawQuestionSketch {
    content: String,
    anchor: String,
}

impl RawQuestionSketch {
    fn into_sketch(self) -> Option<QuestionSketch> {
        let content = self.content.trim().to_string();
        if content.is_empty() {
            return None;
        }
        Some(QuestionSketch {
            content,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawArgumentReconstructionSketch {
    name: String,
    proponent: Option<String>,
    #[serde(deserialize_with = "null_or_empty_vec")]
    premises: Vec<Option<String>>,
    conclusion: String,
    #[serde(deserialize_with = "null_or_empty_vec")]
    objections: Vec<Option<RawObjection>>,
    anchor: String,
}

/// Phase 1 objection shape. Accepts both the legacy bare-string
/// shape (so old prompts and any cached partial output still parse)
/// and the new `{ name, content }` object — the schema instructs
/// the new shape, the parser tolerates either.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawObjection {
    Str(String),
    Obj {
        name: String,
        #[serde(default)]
        content: String,
    },
}

impl RawArgumentReconstructionSketch {
    fn into_sketch(
        self,
    ) -> Option<crate::enrichment::pipeline::atlas::ArgumentReconstructionSketch> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            debug!("literary_atlas: dropping argument sketch — name missing");
            return None;
        }
        let conclusion = self.conclusion.trim().to_string();
        let premises: Vec<String> = self
            .premises
            .into_iter()
            .flatten()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // An argument with neither premises nor a conclusion is
        // structurally empty — drop it. A premise list without a
        // conclusion (or vice versa) is preserved on the assumption
        // that the model captured part of the structure; downstream
        // consumers can decide what to render.
        if premises.is_empty() && conclusion.is_empty() {
            debug!("literary_atlas: dropping argument sketch '{name}' — no premises or conclusion");
            return None;
        }
        use crate::enrichment::atlas::atoms::Objection;
        let objections: Vec<Objection> = self
            .objections
            .into_iter()
            .flatten()
            .filter_map(|raw| {
                let (name, content) = match raw {
                    RawObjection::Str(s) => (s.trim().to_string(), String::new()),
                    RawObjection::Obj { name, content } => {
                        (name.trim().to_string(), content.trim().to_string())
                    }
                };
                if name.is_empty() {
                    None
                } else {
                    Some(Objection { name, content })
                }
            })
            .collect();
        let proponent = self
            .proponent
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        Some(
            crate::enrichment::pipeline::atlas::ArgumentReconstructionSketch {
                name,
                proponent,
                premises,
                conclusion,
                objections,
                anchor: self.anchor,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// dropped — the grammar-constrained sampler is a known no-op, so the
    /// parser is the only place this can be caught.
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
