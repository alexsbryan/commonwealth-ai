// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a declared ontology becomes for the model: the Phase-1 response
//! schema, plus the prompt-size budget. The prompt block that names the
//! types in the author's words lives next door in `ontology_prompt.rs`.
//!
//! Both are generated from the SAME [`OntologyPolicies`], through the same
//! [`TypeIndex::effective_attributes`] the parser validates against — so what
//! the grammar offers, what the prompt describes and what the reader accepts
//! cannot drift apart (§10.6). There is no second copy of the section schema
//! here: [`phase1_schema_for`] parses the shipped
//! [`phase1_section_extraction_schema`] and EDITS it, so a change to the
//! generic schema reaches declared corpora with no second edit.
//!
//! Undeclared corpora never reach this module — `CustomOntology` returns
//! `None` from its compose hooks when `has_declarations()` is false, which is
//! what keeps invariant I1 structural rather than remembered.

use serde_json::{json, Value};

use super::literary_atlas::phase1_section_extraction_schema;
use crate::enrichment::ontology::{
    AttrDecl, AttrFamily, OntologyPolicies, OntologyTypeDecl, TypeIndex, TypeKind,
};

/// Prompt characters the declarations may add to Phase 1 before the composer
/// warns. The neutral Phase-1 system prompt is ~6.4 kB and the literary one
/// ~8.2 kB, so +3,000 is the point at which a declared corpus's prompt starts
/// to look like a different weight class — and the fast-slot input gate
/// refuses over 6,000 characters, so this is a budget with teeth.
pub const MAX_ADDED_PROMPT_CHARS: usize = 3_000;

/// Free-text attribute value ceiling, matching the generic sketch strings.
const TEXT_MAX_LEN: usize = 200;
/// A time attribute is a short expression (`"c. 720–750"`), not prose.
const TIME_MAX_LEN: usize = 40;

/// The JSON Schema for one declared attribute's value.
///
/// Families map to the narrowest shape that still admits what models emit: a
/// quantity is a `number`. The grammar IS enforced (`wire.rs` sends this
/// schema as `response_format: json_schema, strict: true`; note e6067398
/// probed it), so a quantity arrives as a number. The parser's `"1.29 g"`
/// recovery in `parse_policy.rs` is insurance for an unconstrained path, not
/// a correction the enforced one needs.
pub fn attribute_schema(decl: &AttrDecl) -> Value {
    match &decl.family {
        AttrFamily::Text { values } if !values.is_empty() => json!({
            "type": "string",
            "enum": values,
        }),
        AttrFamily::Text { .. } => json!({ "type": "string", "maxLength": TEXT_MAX_LEN }),
        AttrFamily::Quantity { .. } => json!({ "type": "number" }),
        AttrFamily::Time { .. } => json!({ "type": "string", "maxLength": TIME_MAX_LEN }),
        // A ref carries the target's NAME at extraction time; P3 snaps it to
        // an atom id.
        AttrFamily::Ref { .. } => json!({ "type": "string", "maxLength": TEXT_MAX_LEN }),
    }
}

/// The Phase-1 section-extraction schema for a corpus that declares types.
///
/// Edits the shipped schema rather than rebuilding it: declared names extend
/// the `entity_type` enum, relation / event / claim sketches gain their type
/// slot plus one `attributes` object per kind, and
/// `argument_reconstructions` is dropped unless the recipe opted into
/// argument derivation.
///
/// The `attributes` object is the UNION of the kind's declared attributes,
/// not one object per type — per-type membership is enforced by the parser,
/// which keeps the grammar linear in the number of types rather than
/// quadratic. Where two types of one kind declare the same attribute name,
/// the first declaration wins; `validate` reports nothing about that because
/// the parser still validates each atom against ITS type.
pub fn phase1_schema_for(policies: &OntologyPolicies) -> Value {
    let mut schema = phase1_section_extraction_schema();
    let index = TypeIndex::from_policies(policies);
    let entities = of_kind(policies, TypeKind::Entity);
    let relations = of_kind(policies, TypeKind::Relation);
    let events = of_kind(policies, TypeKind::Event);
    let claims = of_kind(policies, TypeKind::Claim);

    // A build that lost `$defs` from the shipped const would be a
    // compile-visible edit, not a runtime surprise; the generic schema stands
    // rather than a half-edited one.
    if let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) {
        // Entities already have an enum — declared names EXTEND it, they do
        // not replace it. The generic six stay reachable so a declared corpus
        // can still say "person" about someone it has no type for.
        if let Some(slot) = enum_slot(defs, "entity_sketch", "entity_type") {
            let mut names: Vec<Value> = slot.as_array().cloned().unwrap_or_default();
            for t in &entities {
                let v = Value::String(t.name.clone());
                if !names.contains(&v) {
                    names.push(v);
                }
            }
            *slot = Value::Array(names);
        }
        attach_attributes(defs, "entity_sketch", &index, &entities);

        add_type_slot(defs, "relation_sketch", "relation_type", &relations);
        attach_attributes(defs, "relation_sketch", &index, &relations);

        add_type_slot(defs, "event_sketch", "event_type", &events);
        attach_attributes(defs, "event_sketch", &index, &events);

        add_type_slot(defs, "claim_sketch", "claim_kind", &claims);
        // `subject` goes in BEFORE the attribute bag. Property order is the
        // model's generation order under the strict grammar, and a `subject`
        // asked for after an empty `{}` was skipped (10 of 31 claims across
        // four sections, 2026-09-02); asked for first it was filled on 40 of
        // 42, and the bag filled more often too (grade 15 vs 8).
        if claims.iter().any(|t| t.subject.is_some()) {
            set_property(
                defs,
                "claim_sketch",
                "subject",
                json!({ "type": "string", "maxLength": TEXT_MAX_LEN }),
            );
        }
        attach_attributes(defs, "claim_sketch", &index, &claims);
        if !claims.is_empty() {
            // `discourse_act` leaves `required`: the declared `force` decides
            // it, so asking the model to guess is asking for a guarantee code
            // already enforces (§7.6). `claim_kind` takes its place.
            set_required(defs, "claim_sketch", &["content", "claim_kind"]);
            // `set_required` replaced the list; put the attribute bag back.
            require_attributes(defs, "claim_sketch");
            let deontic = union_of(&claims, |t| t.deontic.iter().map(wire_name).collect());
            let grades = union_of(&claims, |t| t.grades.clone());
            for (key, values) in [("deontic", deontic), ("grade", grades)] {
                if !values.is_empty() {
                    set_attribute_property(
                        defs,
                        "claim_sketch",
                        key,
                        json!({ "type": "string", "enum": values }),
                    );
                }
            }
            // A grade-only bag (no declared claim attributes) is created just
            // above, after `attach_attributes` declined to; it is required too.
            require_attributes(defs, "claim_sketch");
        }

        // Argument reconstruction is an opt-in derivation pass. Carrying its
        // sketch in the schema of a corpus that will never run it is prompt
        // budget spent on nothing.
        if !policies.derivation.arguments {
            defs.remove("argument_reconstruction_sketch");
        }
    }
    if !policies.derivation.arguments {
        if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            props.remove("argument_reconstructions");
        }
    }
    schema
}

/// Bytes the declarations add to Phase 1 — the prompt block plus the schema's
/// growth over the generic one — traced at every build and warned past
/// [`MAX_ADDED_PROMPT_CHARS`]. Returns the total so a caller can report it.
///
/// Glassbox, not a gate: an over-budget ontology still runs. The operator
/// needs the number to know why a corpus got slower or started overflowing
/// the fast slot (§9).
pub fn report_added_prompt_size(name: &str, declared_block: &str, schema: &Value) -> usize {
    let base = phase1_section_extraction_schema().to_string().len();
    let generated = schema.to_string().len();
    let schema_delta = generated.saturating_sub(base);
    let added = declared_block.len() + schema_delta;
    if added > MAX_ADDED_PROMPT_CHARS {
        tracing::warn!(
            ontology = %name,
            added_chars = added,
            prompt_block_chars = declared_block.len(),
            schema_chars = generated,
            schema_delta_chars = schema_delta,
            budget = MAX_ADDED_PROMPT_CHARS,
            "ontology schema: declarations add more than the Phase-1 prompt budget; \
             extraction will be slower and may overflow a fast-slot input gate"
        );
    } else {
        tracing::debug!(
            ontology = %name,
            added_chars = added,
            prompt_block_chars = declared_block.len(),
            schema_chars = generated,
            schema_delta_chars = schema_delta,
            "ontology schema: declared Phase-1 size"
        );
    }
    added
}

/// The `## Declared types` prompt block: what the recipe declared, in the
/// author's words, in the order they declared it. Empty string when nothing
/// is declared, so a caller can append unconditionally.
///
/// Modelled on `investigation::extract::compose_extract_prompt`, which has
/// rendered typed entity/relationship declarations into a prompt since before
/// ontology v1 — same shape, one bullet per type with its attributes.
pub(super) fn wire_name<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// The declared types of one atom kind, in declaration order.
fn of_kind(policies: &OntologyPolicies, kind: TypeKind) -> Vec<&OntologyTypeDecl> {
    policies
        .shape
        .types
        .iter()
        .filter(|t| t.kind == kind)
        .collect()
}

/// De-duplicated union of a per-type list, in declaration order.
fn union_of<F>(types: &[&OntologyTypeDecl], f: F) -> Vec<String>
where
    F: Fn(&OntologyTypeDecl) -> Vec<String>,
{
    let mut out: Vec<String> = Vec::new();
    for t in types.iter().copied() {
        for v in f(t) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

// ── `$defs` surgery ─────────────────────────────────────────────────────────

type Defs = serde_json::Map<String, Value>;

fn properties_of<'a>(defs: &'a mut Defs, sketch: &str) -> Option<&'a mut Defs> {
    defs.get_mut(sketch)?.get_mut("properties")?.as_object_mut()
}

fn enum_slot<'a>(defs: &'a mut Defs, sketch: &str, property: &str) -> Option<&'a mut Value> {
    properties_of(defs, sketch)?
        .get_mut(property)?
        .get_mut("enum")
}

fn set_property(defs: &mut Defs, sketch: &str, property: &str, value: Value) {
    if let Some(props) = properties_of(defs, sketch) {
        props.insert(property.to_string(), value);
    }
}

fn set_required(defs: &mut Defs, sketch: &str, keys: &[&str]) {
    if let Some(obj) = defs.get_mut(sketch).and_then(Value::as_object_mut) {
        obj.insert(
            "required".to_string(),
            Value::Array(keys.iter().map(|k| Value::String(k.to_string())).collect()),
        );
    }
}

/// Make the sketch's `attributes` object REQUIRED, if the sketch has one.
///
/// A strict JSON-schema grammar enforces only what `required` names; an
/// optional object is omitted at the model's discretion however the prompt
/// asks (measured 2026-09-02: declared claim attributes at 0-2 of 48 across
/// four prompt variants — note e6067398). Requiring the object obliges the
/// model to open it; its properties stay optional, so `{}` is legal and no
/// value is fabricated where the text supports none (§7.6). Idempotent, and a
/// no-op when the sketch carries no bag.
fn require_attributes(defs: &mut Defs, sketch: &str) {
    let has_bag = properties_of(defs, sketch).is_some_and(|p| p.contains_key("attributes"));
    if !has_bag {
        return;
    }
    let Some(obj) = defs.get_mut(sketch).and_then(Value::as_object_mut) else {
        return;
    };
    let required = obj
        .entry("required".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(list) = required.as_array_mut() {
        if !list.iter().any(|k| k == "attributes") {
            list.push(Value::String("attributes".to_string()));
        }
    }
}

/// Give a sketch its declared-type slot: a closed enum of the declared names.
/// No-op when the kind declares nothing, so the generic schema stands.
fn add_type_slot(defs: &mut Defs, sketch: &str, property: &str, types: &[&OntologyTypeDecl]) {
    if types.is_empty() {
        return;
    }
    let names: Vec<Value> = types
        .iter()
        .map(|t| Value::String(t.name.clone()))
        .collect();
    set_property(
        defs,
        sketch,
        property,
        json!({ "type": "string", "enum": names }),
    );
}

/// Give a sketch ONE `attributes` object holding the union of the kind's
/// declared attributes, and require it (see [`require_attributes`]). No-op
/// when nothing of that kind declares any, so a corpus that declares bare
/// types sends the generic sketch shape.
fn attach_attributes(
    defs: &mut Defs,
    sketch: &str,
    index: &TypeIndex<'_>,
    types: &[&OntologyTypeDecl],
) {
    let mut props = serde_json::Map::new();
    for t in types.iter().copied() {
        for a in index.effective_attributes(&t.name) {
            props
                .entry(a.name.clone())
                .or_insert_with(|| attribute_schema(a));
        }
    }
    if props.is_empty() {
        return;
    }
    set_property(
        defs,
        sketch,
        "attributes",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": Value::Object(props),
        }),
    );
    require_attributes(defs, sketch);
}

/// Add one key to a sketch's already-present `attributes` object. Used for
/// the reserved claim keys (`deontic`, `grade`), which are not declared
/// attributes but land in the same bag.
fn set_attribute_property(defs: &mut Defs, sketch: &str, key: &str, value: Value) {
    if let Some(props) = properties_of(defs, sketch) {
        let attrs = props.entry("attributes".to_string()).or_insert_with(|| {
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {},
            })
        });
        if let Some(inner) = attrs.get_mut("properties").and_then(Value::as_object_mut) {
            inner.insert(key.to_string(), value);
        }
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;

    use crate::recipe_templates::numismatics_policies as numismatics;

    fn sketch<'a>(schema: &'a Value, name: &str) -> &'a Value {
        &schema["$defs"][name]
    }

    fn strings(v: &Value) -> Vec<String> {
        v.as_array()
            .expect("array")
            .iter()
            .map(|x| x.as_str().unwrap_or_default().to_string())
            .collect()
    }

    // ── Phase 6 extras ───────────────────────────────────────

    #[test]
    fn declared_types_extend_the_entity_enum_and_keep_the_generic_six() {
        let schema = phase1_schema_for(&numismatics());
        let names = strings(&sketch(&schema, "entity_sketch")["properties"]["entity_type"]["enum"]);
        for generic in [
            "person",
            "concept",
            "institution",
            "work",
            "place",
            "initiative",
        ] {
            assert!(
                names.contains(&generic.to_string()),
                "{generic} still offered"
            );
        }
        for declared in ["coin", "sceatta", "ruler", "mint"] {
            assert!(names.contains(&declared.to_string()), "{declared} offered");
        }
    }

    /// A declared type may reuse a base kind's name (to give `person`
    /// attributes); the enum must offer it ONCE.
    ///
    /// This assertion used to ride on the numismatics template, which declared
    /// `person` until P7 dropped it — legal now that `validate` resolves
    /// references against the base kinds too. That silently left the check
    /// with no input that could fail it (§18.1), so it declares its own.
    #[test]
    fn a_declared_type_reusing_a_base_kind_name_appears_once() {
        use crate::enrichment::ontology::{OntologyTypeDecl, TypeKind};
        let mut p = numismatics();
        p.shape.types.push(OntologyTypeDecl {
            name: "person".into(),
            kind: TypeKind::Entity,
            description: "A named individual: a ruler, a moneyer, an author.".into(),
            ..Default::default()
        });
        let schema = phase1_schema_for(&p);
        let names = strings(&sketch(&schema, "entity_sketch")["properties"]["entity_type"]["enum"]);
        assert_eq!(
            names.iter().filter(|n| *n == "person").count(),
            1,
            "declared `person` must not double the generic one: {names:?}"
        );
    }

    #[test]
    fn each_attribute_family_maps_to_its_own_shape() {
        let schema = phase1_schema_for(&numismatics());
        let attrs = &sketch(&schema, "entity_sketch")["properties"]["attributes"]["properties"];
        assert_eq!(attrs["weight"]["type"], "number", "quantity");
        assert_eq!(attrs["struck"]["type"], "string", "time");
        assert_eq!(attrs["struck"]["maxLength"], TIME_MAX_LEN);
        assert_eq!(attrs["mint"]["type"], "string", "ref carries a name");
        assert_eq!(
            strings(&attrs["metal"]["enum"]),
            ["gold", "silver", "billon", "copper"]
        );
        assert_eq!(attrs["denomination"]["type"], "string", "free text");
        assert!(attrs["denomination"].get("enum").is_none());
        // The attribute bag is closed — an undeclared key cannot be emitted.
        assert_eq!(
            sketch(&schema, "entity_sketch")["properties"]["attributes"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn claims_require_a_kind_and_gain_subject_and_grade() {
        let schema = phase1_schema_for(&numismatics());
        let claim = sketch(&schema, "claim_sketch");
        assert_eq!(
            strings(&claim["required"]),
            ["content", "claim_kind", "attributes"]
        );
        assert_eq!(
            strings(&claim["properties"]["claim_kind"]["enum"]),
            ["attribution"]
        );
        assert_eq!(claim["properties"]["subject"]["type"], "string");
        let attrs = &claim["properties"]["attributes"]["properties"];
        assert_eq!(attrs["proposed_date"]["type"], "string");
        assert_eq!(
            strings(&attrs["grade"]["enum"]),
            ["die-link", "hoard-context", "stylistic", "metrological"]
        );
        // Numismatics declares no deontic modes, so no such key is offered.
        assert!(attrs.get("deontic").is_none());
    }

    /// The grammar enforces only `required`. An optional `attributes` object
    /// is omitted at the model's discretion (0-2 of 48 across four prompt
    /// variants, 2026-09-02), so the bag is required wherever it exists —
    /// with its properties still optional, so `{}` stays legal.
    #[test]
    fn a_declared_attribute_bag_is_required_but_its_slots_are_not() {
        let schema = phase1_schema_for(&numismatics());
        for kind in ["entity_sketch", "claim_sketch"] {
            let sk = sketch(&schema, kind);
            assert!(
                strings(&sk["required"]).contains(&"attributes".to_string()),
                "{kind} must require its attribute bag"
            );
            let bag = &sk["properties"]["attributes"];
            assert!(
                bag.get("required").is_none(),
                "{kind}: the bag's own slots stay optional so {{}} is legal"
            );
        }
        // The base keys survive alongside the bag, in their shipped order.
        assert_eq!(
            strings(&sketch(&schema, "entity_sketch")["required"]),
            ["canonical_name", "entity_type", "attributes"]
        );
        // A kind that declares no attributes keeps the generic `required`.
        let base = phase1_section_extraction_schema();
        assert_eq!(
            sketch(&schema, "relation_sketch")["required"],
            base["$defs"]["relation_sketch"]["required"]
        );
        assert!(sketch(&schema, "relation_sketch")["properties"]
            .get("attributes")
            .is_none());
    }

    /// The schema's property order is the GENERATION order under the daemon's
    /// strict grammar, so it is part of the schema's meaning. Alphabetical
    /// order puts the optional `claims` array first and the model omits it
    /// (0 of 2 replays) where the authored order yields it (2 of 2). This
    /// fails when serde_json is built without `preserve_order` — which is
    /// exactly the build that produced the alphabetical schema.
    #[test]
    fn phase1_schema_keeps_its_authored_order() {
        let schema = phase1_schema_for(&numismatics());
        let top: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(top.first().map(|s| s.as_str()), Some("section_id"));
        assert!(
            top.iter().position(|k| *k == "claims")
                > top.iter().position(|k| *k == "entities_introduced"),
            "claims must come after the entities the model has already listed: {top:?}"
        );
        let claim: Vec<&String> = sketch(&schema, "claim_sketch")["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(claim.first().map(|s| s.as_str()), Some("content"));
        let pos = |k: &str| claim.iter().position(|c| *c == k);
        assert!(pos("claim_kind") < pos("subject"), "{claim:?}");
        // `subject` before the bag: asked for after an empty `{}` it was
        // skipped (10/31); asked for first it was filled (40/42).
        assert!(pos("subject") < pos("attributes"), "{claim:?}");
    }

    /// Argument reconstruction is opt-in; carrying its sketch in a corpus
    /// that will never run the pass is prompt budget spent on nothing.
    #[test]
    fn argument_reconstructions_are_dropped_unless_derivation_asks_for_them() {
        let mut p = numismatics();
        assert!(!p.derivation.arguments, "the template does not opt in");
        let off = phase1_schema_for(&p);
        assert!(off["properties"].get("argument_reconstructions").is_none());
        assert!(off["$defs"].get("argument_reconstruction_sketch").is_none());

        p.derivation.arguments = true;
        let on = phase1_schema_for(&p);
        assert!(on["properties"]["argument_reconstructions"].is_object());
        assert!(on["$defs"]["argument_reconstruction_sketch"].is_object());
    }

    /// The generic schema is the base, not a rewrite of it: everything the
    /// shipped const carries that the declarations do not touch survives.
    #[test]
    fn the_generic_schema_survives_underneath() {
        let schema = phase1_schema_for(&numismatics());
        let base = phase1_section_extraction_schema();
        assert_eq!(
            sketch(&schema, "question_sketch"),
            &base["$defs"]["question_sketch"]
        );
        assert_eq!(schema["required"], base["required"]);
        assert_eq!(
            sketch(&schema, "entity_sketch")["properties"]["canonical_name"],
            base["$defs"]["entity_sketch"]["properties"]["canonical_name"]
        );
    }
}
