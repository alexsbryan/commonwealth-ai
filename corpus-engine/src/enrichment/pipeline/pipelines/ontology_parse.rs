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

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use tracing::debug;

use super::super::atlas::{
    ClaimScope, ClaimSketch, DiscourseAct, EntitySketch, EntityStateSketch, EntityType,
    EpistemicStatus, EventSketch, QuestionSketch, RelationSketch, RelationStateSketch,
    SectionExtraction,
};
use super::super::text_helpers::is_placeholder_literal;
use super::super::types::Phase1ChapterResult;
use super::literary::{prepare_phase_json, sanitize_optional_string};
use super::literary_atlas::{
    first_event_description, first_text_level_claim, phase3_metadata_value_to_string,
    sanitize_phase1_object_arrays, scrub_placeholder_strings,
};
use crate::enrichment::ontology::{
    AttrDecl, AttrFamily, ClaimScopeDecl, Deontic, Force, OntologyPolicies, TypeIndex, TypeKind,
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

// ── Declared-ontology parse policy ──────────────────────────────────────────

/// What the reader enforces for one corpus's declared ontology.
///
/// Built once from [`OntologyPolicies`] and cached on the genre, then handed
/// to every `into_sketch`. [`ParsePolicy::default`] declares nothing and
/// reproduces the pre-ontology reader exactly — which is what makes invariant
/// I1 structural rather than remembered: the generic path and a `version = 1`
/// block with no declarations run the same code under the same policy.
///
/// The maps are keyed by the declared type NAME, and their attribute lists are
/// [`TypeIndex::effective_attributes`] — the same accessor the schema
/// generator reads, so what the grammar offers and what the parser accepts
/// cannot disagree (§10.6).
#[derive(Debug, Clone, Default)]
pub struct ParsePolicy {
    entity_types: BTreeMap<String, Vec<AttrDecl>>,
    relation_types: BTreeMap<String, Vec<AttrDecl>>,
    event_types: BTreeMap<String, Vec<AttrDecl>>,
    claim_types: BTreeMap<String, ClaimTypeRules>,
    /// Folded speaker roles that must never become entity atoms
    /// (`voices.not_entities`). Enforced here, not asked of the model (§7.6).
    not_entities: BTreeSet<String>,
    /// The claim kind to assume when a corpus declares exactly one, so a
    /// model that omits `claim_kind` still lands in the declared type rather
    /// than falling back to the generic claim.
    default_claim_kind: Option<String>,
}

/// The per-claim-type facets the reader enforces. Read off the
/// [`OntologyTypeDecl`] of kind `claim`; there is no second place they live.
#[derive(Debug, Clone)]
struct ClaimTypeRules {
    /// From the type's REQUIRED `force`. The declaration wins over whatever
    /// the model emitted: force is a property of the type, and a model cannot
    /// be asked to guarantee what the recipe already states (§7.6).
    discourse_act: DiscourseAct,
    /// From the type's `scope`; `None` leaves the resolver's default.
    scope: Option<ClaimScope>,
    /// Effective (inherited) declared attributes.
    attributes: Vec<AttrDecl>,
    /// Whether the type declares a `subject`. The sketch keeps the NAME; P3
    /// resolves it to an atom id the way `attributed_to` is resolved.
    has_subject: bool,
    /// Declared deontic modes. A `deontic` attribute is accepted only when it
    /// names one of these — validated, never synthesised.
    deontic: Vec<Deontic>,
    /// Declared evidence grades, strongest first. Same rule as `deontic`.
    grades: Vec<String>,
}

impl ClaimTypeRules {
    /// Wire spellings of the declared deontic modes, read back through serde
    /// so the accepted set can never disagree with what the recipe parses.
    fn deontic_names(&self) -> Vec<String> {
        self.deontic
            .iter()
            .map(|d| {
                serde_json::to_string(d)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string()
            })
            .collect()
    }
}

/// The declared type a sketch named, or `None` when it named nothing or named
/// something the ontology does not declare. An undeclared name is a drop of
/// the TYPE, never of the atom — the sketch keeps its label and stays
/// unclassified, exactly as an undeclared corpus's sketches do.
fn declared_type<V>(
    declared: &BTreeMap<String, V>,
    raw: Option<String>,
    kind: &str,
    subject: &str,
) -> Option<String> {
    let name = raw
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    if declared.contains_key(&name) {
        return Some(name);
    }
    debug!(
        atom = %kind, subject = %subject, named = %name,
        "ontology parse: leaving type unclassified — the ontology declares no such type"
    );
    None
}

/// Reserved attribute key carrying a directive claim's deontic normal form.
const ATTR_DEONTIC: &str = "deontic";
/// Reserved attribute key carrying a claim's evidence grade.
const ATTR_GRADE: &str = "grade";

impl ParsePolicy {
    /// Derive the reader's policy from a corpus's declared ontology.
    ///
    /// A policy with no declared types yields [`Self::default`], so callers do
    /// not branch — `has_declarations()` is the ONE predicate, and it is read
    /// by the composer, not here.
    pub fn from_policies(policies: &OntologyPolicies) -> Self {
        let index = TypeIndex::from_policies(policies);
        let mut out = Self::default();
        for t in &policies.shape.types {
            let attrs: Vec<AttrDecl> = index
                .effective_attributes(&t.name)
                .into_iter()
                .cloned()
                .collect();
            match t.kind {
                TypeKind::Entity => {
                    out.entity_types.insert(t.name.clone(), attrs);
                }
                TypeKind::Relation => {
                    out.relation_types.insert(t.name.clone(), attrs);
                }
                TypeKind::Event => {
                    out.event_types.insert(t.name.clone(), attrs);
                }
                TypeKind::Claim => {
                    let Some(force) = t.force else {
                        // Unreachable through `Recipe::from_toml` — the V1
                        // language refuses a claim type without `force`. A
                        // hand-built policy that skips it loses the type
                        // rather than silently guessing a force (§18.3).
                        debug!(
                            claim_type = %t.name,
                            "ontology parse: claim type declares no force; not enforced"
                        );
                        continue;
                    };
                    out.claim_types.insert(
                        t.name.clone(),
                        ClaimTypeRules {
                            discourse_act: discourse_act_for(force),
                            scope: t.scope.map(claim_scope_for),
                            attributes: attrs,
                            has_subject: t.subject.is_some(),
                            deontic: t.deontic.clone(),
                            grades: t.grades.clone(),
                        },
                    );
                }
                // States are not extracted as a declared kind in Phase 1 —
                // the section schema has no state-type slot. P3 emits them
                // from `role_of`.
                TypeKind::State => {}
            }
        }
        out.not_entities = policies
            .assertion
            .voices
            .not_entities
            .iter()
            .map(|v| fold_voice(v))
            .filter(|v| !v.is_empty())
            .collect();
        let mut claim_names = out.claim_types.keys();
        out.default_claim_kind = match (claim_names.next(), claim_names.next()) {
            (Some(only), None) => Some(only.clone()),
            _ => None,
        };
        out
    }

    /// Does this policy enforce anything? False for every undeclared corpus.
    pub fn is_empty(&self) -> bool {
        self.entity_types.is_empty()
            && self.relation_types.is_empty()
            && self.event_types.is_empty()
            && self.claim_types.is_empty()
            && self.not_entities.is_empty()
    }

    /// Is `name` a speaker role the corpus declared as not-subject-matter?
    fn is_voice(&self, name: &str) -> bool {
        !self.not_entities.is_empty() && self.not_entities.contains(&fold_voice(name))
    }
}

/// Searle's force → the atlas's discourse act. The ONE mapping; a second
/// spelling of it anywhere else is the §10.6 smell.
fn discourse_act_for(force: Force) -> DiscourseAct {
    match force {
        Force::Assertive => DiscourseAct::Assert,
        // A directive and a declaration both DO something by being said —
        // `enact` is the atlas's act for that. The atlas has no separate
        // "directive"; the deontic mode carries which one it is.
        Force::Directive | Force::Declaration => DiscourseAct::Enact,
        Force::Commissive => DiscourseAct::Commit,
    }
}

/// Declared claim scope → the atlas's `ClaimScope`. `in_work` is what the
/// literary resolver already defaults every claim to; `about_work` is scoped
/// to the work being discussed, which is `contextual`, not universal.
fn claim_scope_for(scope: ClaimScopeDecl) -> ClaimScope {
    match scope {
        ClaimScopeDecl::InWork => ClaimScope::Fictional,
        ClaimScopeDecl::AboutWork => ClaimScope::Contextual,
    }
}

/// Fold a speaker role for comparison: lowercased, trimmed, leading `the`
/// dropped. `"The Cataloguer"`, `"the cataloguer"` and `"cataloguer"` are one
/// voice; an author should not have to spell all three.
fn fold_voice(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    lower
        .strip_prefix("the ")
        .unwrap_or(lower.as_str())
        .trim()
        .to_string()
}

/// Normalise one attribute value against its declared family, or `None` when
/// the value cannot be that family. Stored normalised — a number for a
/// quantity, a string otherwise — so a downstream reader never re-parses.
fn validate_attr(family: &AttrFamily, value: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::Value;
    match family {
        AttrFamily::Text { values } => {
            let s = value.as_str()?.trim();
            if s.is_empty() {
                return None;
            }
            if values.is_empty() {
                return Some(Value::String(s.to_string()));
            }
            // A closed set answers in the DECLARED spelling, so the stored
            // value and the recipe agree however the model cased it.
            values
                .iter()
                .find(|v| v.eq_ignore_ascii_case(s))
                .map(|v| Value::String(v.clone()))
        }
        AttrFamily::Quantity { .. } => match value {
            Value::Number(n) => Some(Value::Number(n.clone())),
            // Models write the unit back into the value ("1.29 g") even when
            // the schema says number, and the grammar-constrained sampler is
            // a known no-op — so the parser is the only place this can be
            // recovered. Take the leading number; refuse anything else.
            Value::String(s) => leading_number(s)
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number),
            _ => None,
        },
        AttrFamily::Time { .. } | AttrFamily::Ref { .. } => {
            let s = value.as_str()?.trim();
            if s.is_empty() {
                None
            } else {
                Some(Value::String(s.to_string()))
            }
        }
    }
}

/// The leading decimal number of a string, ignoring a trailing unit or range
/// tail. `"1.29 g"` → 1.29; `"c. 720"` → 720.0; `"heavy"` → `None`.
fn leading_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let start = t.find(|c: char| c.is_ascii_digit() || c == '-' || c == '+')?;
    let rest = &t[start..];
    let end = rest
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0)))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok().filter(|f| f.is_finite())
}

/// Keep the declared attributes the model filled, normalised by family; drop
/// the rest with a reason. `subject` names the atom in the log so a debug run
/// reads as "which atom lost which attribute and why" (§9).
fn validated_attributes(
    decls: &[AttrDecl],
    raw: serde_json::Map<String, serde_json::Value>,
    kind: &str,
    subject: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    if decls.is_empty() && !raw.is_empty() {
        debug!(
            atom = %kind, subject = %subject, keys = raw.len(),
            "ontology parse: dropping attributes — the type declares none"
        );
        return out;
    }
    for (key, value) in raw {
        let Some(decl) = decls.iter().find(|d| d.name == key) else {
            debug!(
                atom = %kind, subject = %subject, attribute = %key,
                "ontology parse: dropping attribute — not declared on this type"
            );
            continue;
        };
        match validate_attr(&decl.family, &value) {
            Some(v) => {
                out.insert(key, v);
            }
            None => debug!(
                atom = %kind, subject = %subject, attribute = %key,
                family = decl.family.key(), value = %value,
                "ontology parse: dropping attribute — value is not of the declared family"
            ),
        }
    }
    out
}

/// Keep a reserved claim attribute (`deontic`, `grade`) only when it names one
/// of the values the claim type declared. Validated, never synthesised: an
/// undeclared mode is a drop, not a guess.
fn validated_choice(
    raw: &mut serde_json::Map<String, serde_json::Value>,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    allowed: &[String],
    subject: &str,
) {
    let Some(value) = raw.remove(key) else { return };
    let Some(s) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        debug!(subject = %subject, attribute = %key, "ontology parse: dropping reserved attribute — not a string");
        return;
    };
    match allowed.iter().find(|a| a.eq_ignore_ascii_case(s)) {
        Some(a) => {
            out.insert(key.to_string(), serde_json::Value::String(a.clone()));
        }
        None => debug!(
            subject = %subject, attribute = %key, value = %s,
            "ontology parse: dropping reserved attribute — the claim type declares no such value"
        ),
    }
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
            // entity type is an `Other` tag, and the arm below drops those.
            Some(EntityType::Other(s)) if policy.entity_types.contains_key(s.trim()) => {
                EntityType::Other(s.trim().to_string())
            }
            Some(EntityType::Other(s)) => {
                debug!(
                    "literary_atlas: dropping entity sketch '{name}' — \
                     entity_type='{s}' is neither one of the 6 named variants \
                     nor a declared type (model hedged on typing)"
                );
                return None;
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
            policy
                .entity_types
                .get(entity_type.as_str_repr())
                .map(Vec::as_slice)
                .unwrap_or_default(),
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
        let decls: &[AttrDecl] = match relation_type.as_deref() {
            Some(t) => policy
                .relation_types
                .get(t)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            None => &[],
        };
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
        let decls: &[AttrDecl] = match event_type.as_deref() {
            Some(t) => policy
                .event_types
                .get(t)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            None => &[],
        };
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
        let claim_kind = self
            .claim_kind
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .or_else(|| policy.default_claim_kind.clone())
            .filter(|k| policy.claim_types.contains_key(k));
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
    use crate::recipe::Recipe;

    /// The shipped numismatics template — the P2 fixture. Loaded through the
    /// same accessor `svrn recipe new --ontology numismatics` uses, so the
    /// test cannot pass against a declaration nobody ships.
    fn numismatics() -> OntologyPolicies {
        let toml = crate::recipe_templates::load_builtin("numismatics")
            .expect("numismatics is a shipped template");
        Recipe::from_toml(toml)
            .expect("the shipped template parses")
            .custom_atlas_spec()
            .expect("it declares an [enrichment.ontology] block")
            .policies()
    }

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

    #[test]
    fn default_policy_declares_nothing() {
        assert!(ParsePolicy::default().is_empty());
        assert!(!ParsePolicy::from_policies(&numismatics()).is_empty());
    }

    #[test]
    fn force_maps_to_the_pinned_discourse_acts() {
        assert_eq!(discourse_act_for(Force::Assertive), DiscourseAct::Assert);
        assert_eq!(discourse_act_for(Force::Directive), DiscourseAct::Enact);
        assert_eq!(discourse_act_for(Force::Declaration), DiscourseAct::Enact);
        assert_eq!(discourse_act_for(Force::Commissive), DiscourseAct::Commit);
    }

    #[test]
    fn leading_number_reads_what_models_actually_write() {
        assert_eq!(leading_number("1.29 g"), Some(1.29));
        assert_eq!(leading_number("c. 720"), Some(720.0));
        assert_eq!(leading_number("-3"), Some(-3.0));
        assert_eq!(leading_number("heavy"), None);
        assert_eq!(leading_number(""), None);
    }

    #[test]
    fn voice_folding_is_case_and_article_insensitive() {
        assert_eq!(fold_voice("  The Cataloguer "), "cataloguer");
        assert_eq!(fold_voice("cataloguer"), "cataloguer");
        assert_eq!(fold_voice("the narrator"), "narrator");
    }
}
