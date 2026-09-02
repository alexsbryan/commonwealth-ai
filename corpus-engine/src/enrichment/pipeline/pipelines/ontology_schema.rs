// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a declared ontology becomes for the model: the Phase-1 response
//! schema, and the prompt block that names the types in the author's words.
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
/// quantity is a `number`, and the parser separately recovers `"1.29 g"`
/// because the grammar-constrained sampler is a known no-op.
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
        attach_attributes(defs, "claim_sketch", &index, &claims);
        if !claims.is_empty() {
            // `discourse_act` leaves `required`: the declared `force` decides
            // it, so asking the model to guess is asking for a guarantee code
            // already enforces (§7.6). `claim_kind` takes its place.
            set_required(defs, "claim_sketch", &["content", "claim_kind"]);
            if claims.iter().any(|t| t.subject.is_some()) {
                set_property(
                    defs,
                    "claim_sketch",
                    "subject",
                    json!({ "type": "string", "maxLength": TEXT_MAX_LEN }),
                );
            }
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
pub fn render_declared_types(policies: &OntologyPolicies) -> String {
    if !policies.has_declarations() {
        return String::new();
    }
    let index = TypeIndex::from_policies(policies);
    let mut out = String::from(
        "## Declared types\n\n\
         This corpus declares the types below. Use the exact name in the sketch's \
         type field, and put declared attributes in that sketch's `attributes` \
         object — only the keys listed here, and only where the text supports a \
         value. Never invent one.\n",
    );
    for (kind, heading, slot) in [
        (TypeKind::Entity, "Entities", "entity_type"),
        (TypeKind::Relation, "Relations", "relation_type"),
        (TypeKind::Event, "Events", "event_type"),
        (TypeKind::Claim, "Claims", "claim_kind"),
    ] {
        let types: Vec<&OntologyTypeDecl> = policies
            .shape
            .types
            .iter()
            .filter(|t| t.kind == kind)
            .collect();
        if types.is_empty() {
            continue;
        }
        out.push_str(&format!("\n### {heading} (`{slot}`)\n\n"));
        for t in types {
            out.push_str(&format!("- **{}**", t.name));
            if !t.description.trim().is_empty() {
                out.push_str(&format!(" — {}", t.description.trim()));
            } else if let Some(parent) = t.specializes.as_deref() {
                out.push_str(&format!(" — a kind of {parent}"));
            }
            out.push('\n');
            let mut facets: Vec<String> = Vec::new();
            if let Some(subject) = t.subject.as_deref() {
                facets.push(format!("about a {subject}"));
            }
            if !t.grades.is_empty() {
                facets.push(format!("grade: {}", t.grades.join(" | ")));
            }
            if !t.deontic.is_empty() {
                facets.push(format!(
                    "deontic: {}",
                    t.deontic
                        .iter()
                        .map(wire_name)
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
            if !facets.is_empty() {
                out.push_str(&format!("  {}\n", facets.join(" · ")));
            }
            let attrs = index.effective_attributes(&t.name);
            if !attrs.is_empty() {
                out.push_str(&format!(
                    "  attributes: {}\n",
                    attrs
                        .iter()
                        .map(|a| render_attr(a))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }

    let voices = &policies.assertion.voices;
    if !voices.not_entities.is_empty() || !voices.attributed_to.is_empty() {
        out.push_str("\n## Voices\n\n");
        if !voices.not_entities.is_empty() {
            out.push_str(&format!(
                "These are speakers, not subject matter — never emit an entity for them: {}.\n",
                voices.not_entities.join(", ")
            ));
        }
        if !voices.attributed_to.is_empty() {
            out.push_str(&format!(
                "Attribute a claim only to one of: {}.\n",
                voices.attributed_to.join(", ")
            ));
        }
    }

    if !policies.assertion.must_not.is_empty() {
        out.push_str("\n## Must not\n\n");
        for m in &policies.assertion.must_not {
            out.push_str(&format!("- {m}\n"));
        }
    }
    out
}

/// One attribute, rendered for the prompt: name plus what the family admits.
fn render_attr(a: &AttrDecl) -> String {
    let shape = match &a.family {
        AttrFamily::Text { values } if !values.is_empty() => {
            format!("one of: {}", values.join(" | "))
        }
        AttrFamily::Text { .. } => "text".to_string(),
        AttrFamily::Quantity { unit: Some(u) } => format!("number in {u}"),
        AttrFamily::Quantity { unit: None } => "number".to_string(),
        AttrFamily::Time { range: true } => "date or range".to_string(),
        AttrFamily::Time { range: false } => "date".to_string(),
        AttrFamily::Ref { of } => format!("name of a {of}"),
    };
    if a.description.trim().is_empty() {
        format!("{} ({shape})", a.name)
    } else {
        format!("{} ({shape}; {})", a.name, a.description.trim())
    }
}

/// The serde wire spelling of a closed enum value, read back through serde so
/// prompt text and parser cannot disagree about the accepted spelling.
fn wire_name<T: serde::Serialize>(v: &T) -> String {
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
/// declared attributes. No-op when nothing of that kind declares any, so a
/// corpus that declares bare types sends the generic sketch shape.
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
mod tests {
    use super::*;

    use super::super::numismatics_policies as numismatics;

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
        // `person` is declared AND generic — it appears once, not twice.
        assert_eq!(names.iter().filter(|n| *n == "person").count(), 1);
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
        assert_eq!(strings(&claim["required"]), ["content", "claim_kind"]);
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

    #[test]
    fn nothing_declared_renders_no_prompt_block() {
        let p = OntologyPolicies::from_prose("Rules of a house.", Default::default());
        assert_eq!(render_declared_types(&p), "");
    }

    #[test]
    fn the_prompt_block_names_every_declared_type_and_its_attributes() {
        let block = render_declared_types(&numismatics());
        assert!(block.starts_with("## Declared types"));
        for name in ["coin", "sceatta", "ruler", "mint", "attribution"] {
            assert!(block.contains(&format!("**{name}**")), "{name} named");
        }
        assert!(block.contains("`entity_type`") && block.contains("`claim_kind`"));
        assert!(block.contains("weight (number in g)"), "unit rendered");
        assert!(block.contains("one of: gold | silver | billon | copper"));
        assert!(block.contains("name of a mint"), "ref target rendered");
        assert!(block.contains("about a coin"), "declared subject rendered");
        assert!(block.contains("grade: die-link | hoard-context"));
        // `sceatta` declares no attributes of its own but inherits coin's.
        assert!(block.contains("a kind of coin"));
    }

    #[test]
    fn voices_and_must_not_render_only_when_declared() {
        let mut p = numismatics();
        assert!(!render_declared_types(&p).contains("## Voices"));
        assert!(!render_declared_types(&p).contains("## Must not"));
        p.assertion.voices.not_entities = vec!["the cataloguer".into()];
        p.assertion.must_not = vec!["price a coin".into()];
        let block = render_declared_types(&p);
        assert!(block.contains("## Voices") && block.contains("the cataloguer"));
        assert!(block.contains("## Must not") && block.contains("- price a coin"));
    }

    /// The fixture must fit the budget; if it ever does not, the number is
    /// the thing to read, not the pass/fail.
    #[test]
    fn the_shipped_fixture_fits_the_prompt_budget() {
        let p = numismatics();
        let block = render_declared_types(&p);
        let added = report_added_prompt_size("numismatics", &block, &phase1_schema_for(&p));
        assert!(
            added <= MAX_ADDED_PROMPT_CHARS,
            "numismatics adds {added} chars, budget {MAX_ADDED_PROMPT_CHARS}"
        );
    }
}
