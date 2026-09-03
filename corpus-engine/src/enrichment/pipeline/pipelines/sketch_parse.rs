// SPDX-License-Identifier: AGPL-3.0-or-later
//! The lenient wire layer: one JSON object off a model → one sketch.
//!
//! Every `Raw*` shape the Phase-1 response can take, and the conversion that
//! turns it into the typed sketch the atlas stores. Models drift on schema
//! compliance — a claim drops a required field, an alias list carries a stray
//! null, a relation arrives with one participant — and hard-failing on any of
//! those loses a whole section to save one malformed atom. So the `Raw*`
//! structs accept what the sketches do not, and each conversion decides what
//! survives, logging every drop with its reason (§9).
//!
//! This is also where a declared ontology is ENFORCED: the [`ParsePolicy`]
//! threaded through each `into_sketch` is what keeps a declared type, drops a
//! declared voice, validates an attribute against its family and takes a
//! claim's discourse act from its declared force. The policy itself lives in
//! [`super::parse_policy`]; assembling a whole response lives in
//! [`super::ontology_parse`], which owns the reader's behavioural tests.
//!
//! Split from `ontology_parse.rs` on the seam that file already drew (its own
//! `── Lenient deserialisation layer ──` divider): at 1,134 lines it sat
//! inside ARCH §3.1's 800-1200 approach band, which is a no-slack aggregate
//! ratchet, so a file has to leave the band rather than merely shrink.

use serde::Deserialize;
use tracing::debug;

use super::super::atlas::{
    ClaimSketch, DiscourseAct, EntitySketch, EntityStateSketch, EntityType, EpistemicStatus,
    EventSketch, QuestionSketch, RelationSketch, RelationStateSketch, SectionExtraction,
};
use super::literary_atlas::phase3_metadata_value_to_string;
use super::parse_policy::{
    canonical_name, declared_attributes, declared_type, validated_attributes, validated_choice,
    ClaimTypeRules, ParsePolicy, ATTR_DEONTIC, ATTR_GRADE,
};

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
pub(super) struct RawSectionExtraction {
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
    pub(super) fn into_extraction(self, policy: &ParsePolicy) -> SectionExtraction {
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
pub(super) struct RawEntitySketch {
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
        // Reject the type-evasion failure mode. Under the default
        // `json_schema` structured-output mode the daemon's llguidance
        // grammar DOES enforce the schema's `enum` (probed 2026-09-02,
        // note e6067398), but a provider on `json-object` mode gets no
        // grammar, so the parser still has to enforce "must commit to
        // one of the 5 named variants" — without enforcement, models hedge
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
pub(super) struct RawEntityStateSketch {
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
pub(super) struct RawRelationSketch {
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
pub(super) struct RawRelationStateSketch {
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
pub(super) struct RawEventSketch {
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
pub(super) struct RawClaimSketch {
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
            // The prompt says to leave `attributed_to` out for a text-level
            // claim; models write the word instead. "omit" is not a scholar.
            // 22 of 40 claims on one build (2026-09-02) carried the literal.
            .filter(|s| {
                let absent = super::parse_policy::is_absent_marker(s);
                if absent {
                    debug!(
                        claim = %content, attributed_to = %s,
                        "ontology parse: clearing attribution — a placeholder, not a name"
                    );
                }
                !absent
            })
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
pub(super) struct RawQuestionSketch {
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
pub(super) struct RawArgumentReconstructionSketch {
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
