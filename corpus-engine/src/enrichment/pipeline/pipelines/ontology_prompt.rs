// SPDX-License-Identifier: AGPL-3.0-or-later
//! The prompt block a declared ontology adds to Phase 1, and the Phase-6
//! extras: the types in the author's words, the worked examples that show
//! where an attribute and a subject go, and the deontic reading.
//!
//! Split from `ontology_schema.rs` (ARCH §3.2) when the required-bag change
//! took that file over 1200 lines; the two halves are generated from the SAME
//! [`OntologyPolicies`] through the same [`TypeIndex`], so what the prompt
//! describes and what the grammar offers cannot drift apart (§10.6). The
//! schema half — [`super::ontology_schema::phase1_schema_for`] — and the
//! prompt-size budget stay next door.

use crate::enrichment::ontology::{
    AttrDecl, AttrFamily, OntologyPolicies, OntologyTypeDecl, TypeIndex, TypeKind,
};

use super::ontology_schema::wire_name;
use super::parse_policy::state_is_of_relation;

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
        (TypeKind::State, "States", "state_type"),
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
            // A state type names one of the two state facets, and which one
            // is not guessable from the name — `of` decides it. Say it, or
            // the model has a `state_type` enum on two lists and no rule for
            // choosing between them.
            if t.kind == TypeKind::State {
                let facet = if state_is_of_relation(policies, t) {
                    "relations_developed"
                } else {
                    "entities_developed"
                };
                facets.push(format!(
                    "goes in {facet}, of the {}",
                    t.of.as_deref().unwrap_or("declared type")
                ));
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

    out.push_str(&render_attribute_shape(policies, &index));
    out.push_str(&render_subject_shape(policies, &index));

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

// ── Phase 6: what the declarations add to the tension classifier ────────

/// The declared ontology's contribution to the Phase-6 classifier system
/// prompt — the `{ontology_extras}` slot in
/// `literary_atlas_prompts/custom_phase6_classifier_system.md`.
///
/// **Empty for every corpus that declares nothing**, which is invariant I1
/// for the Phase-6 prompt: the template renders byte-identically for a
/// version-0 block and for a version-1 block with no types
/// (`maple_house.phase6_classifier` pins both).
///
/// Three sections, each present only when the declaration earns it:
///
/// - **Declared non-conflicts** — `tension.not_conflicts`, the author's own
///   list of pairs that look like conflicts and are not. Versioned with the
///   recipe because it is never complete (ONTOLOGY_PRIMITIVES §2 axis 5).
/// - **Deontic reading** — the interdefinition of `forbid` / `require` /
///   `permit`, emitted when any declared claim type carries a deontic mode.
///   Without it "must not host after 10pm" and "must end hosting by 10pm"
///   are two rules in conflict instead of one rule stated twice.
/// - **Relation** — asks for the third field the declared response schema
///   carries ([`crate::enrichment::atlas::analysis::phase6_classifier_response_schema_with_relation`]).
///   `equivalent` is the verdict that becomes a `same_as` Claim rather than
///   a Tension edge.
///
/// The returned text may itself contain `{tension_term}`; the caller
/// substitutes this slot BEFORE the term placeholders so it is filled.
pub fn render_phase6_extras(policies: &OntologyPolicies) -> String {
    if !policies.has_declarations() {
        return String::new();
    }
    let mut out = String::new();

    let not_conflicts = &policies.derivation.tension.not_conflicts;
    if !not_conflicts.is_empty() {
        out.push_str(
            "\n\n## Declared non-conflicts\n\n\
             The author of this corpus named the pairs below as things that LOOK like \
             {tension_term}s and are not. A pair matching any of them is NOT a \
             {tension_term}, whatever else you notice about it:\n",
        );
        for n in not_conflicts {
            out.push_str(&format!("\n- {}", n.trim()));
        }
        out.push('\n');
    }

    if declares_deontic(policies) {
        out.push_str(
            "\n\n## Deontic reading\n\n\
             The declared claim types carry a deontic mode (require, forbid, permit, \
             request). Read the modes as INTERDEFINED, not as separate vocabularies:\n\
             \n\
             - \"forbid X\" and \"require not-X\" are the same statement in two \
             surface forms. So are \"must not do X after T\" and \"must stop X by T\". \
             A restatement is never a {tension_term}.\n\
             - \"permit X\" is compatible with \"require Y\" unless honouring Y makes \
             the permitted act impossible in the same ordinary moment.\n\
             - Two statements that differ only in wording, in mode, or in which side \
             of one prohibition they state, are ONE statement said twice.\n",
        );
    }

    out.push_str(
        "\n\n## Relation\n\n\
         Alongside `is_tension`, return a `relation` naming what A and B are to \
         each other:\n\
         \n\
         - `conflict` — a genuine {tension_term} (set `is_tension: true`).\n\
         - `equivalent` — the same statement in different words: same subject, same \
         content, nothing added or narrowed by either side. Two surface forms of one \
         rule are `equivalent`, NOT a {tension_term}.\n\
         - `compatible` — anything else: both can hold at once and they are not the \
         same statement.\n\
         \n\
         `equivalent` is a strong claim. Use it only when the two would be redundant \
         if both were kept; when either adds a condition, a scope, or a number the \
         other lacks, the answer is `compatible`.\n",
    );

    out
}

/// Does any declared claim type carry a deontic mode? Only a `directive`
/// claim type can, so this is also the test for "this corpus states rules".
fn declares_deontic(policies: &OntologyPolicies) -> bool {
    policies.claim_types().any(|t| !t.deontic.is_empty())
}

/// Where a declared attribute goes in the emitted JSON, shown once.
///
/// The prose above already says to put attributes in the sketch's
/// `attributes` object. It was not enough: the wessex-hoard probe filled 0 of
/// 14 `coin` atoms across all seven declared attributes while filling the
/// claim-side `grade` 28 times. The reason is that the NEUTRAL Phase-1 prompt
/// this block is appended to carries a worked JSON example — and that example
/// happens to show a `coin` entity with no `attributes` object at all. A
/// model shown one filled example and one contradicting instruction follows
/// the example. Phase 1 cannot fall back on the grammar to force the issue:
/// the response schema is advisory here (models emit `"1.29 g"` where it says
/// `number`, which is why the parser recovers quantities itself), so the
/// prompt is the only lever there is.
///
/// The shapes are deliberately `<text>` / `<number>` rather than plausible
/// values. A filled example invites copying, and a copied value is a
/// fabricated one — it would also register as a filled attribute in the
/// coverage report, corrupting the instrument that measures this fix (§18.4).
/// `<number>` earns its place separately: a quantity is the one family whose
/// JSON shape a model routinely gets wrong.
///
/// Empty when no declared type declares an attribute, so a declaration that
/// cannot benefit does not pay for the block.
fn render_attribute_shape(policies: &OntologyPolicies, index: &TypeIndex<'_>) -> String {
    // The first declared type that has attributes, in declaration order —
    // the example uses the AUTHOR's own keys, so it needs no translation.
    // A type carrying a REF attribute is preferred when one exists: the
    // example is the only place the model sees a ref's shape filled, and
    // an example without one taught it that refs are optional (the
    // ei7-ans build filled `mint` on 247 of 362 coins — the row states
    // it — and `hoard` on 49, all from rows that repeat it; the hoard a
    // coin belongs to is SECTION CONTEXT, stated in the title, never in
    // the row, so nothing the prompt showed ever got filled from it).
    let candidates: Vec<(&OntologyTypeDecl, Vec<&AttrDecl>)> = policies
        .shape
        .types
        .iter()
        .map(|t| (t, index.effective_attributes(&t.name)))
        .filter(|(_, a)| !a.is_empty())
        .collect();
    let Some((t, attrs)) = candidates
        .iter()
        .find(|(_, a)| a.iter().any(|x| matches!(x.family, AttrFamily::Ref { .. })))
        .or_else(|| candidates.first())
        .map(|(t, a)| (*t, a.clone()))
    else {
        return String::new();
    };
    let pairs = attribute_pairs(&attrs);
    // An attribute named in `identity` is not one attribute among seven: it is
    // what tells two mentions of one thing from two things, so a mention that
    // omits it can never be matched to its other mentions. The declaration
    // already knows which those are; without this the prompt flattened them
    // into the same list as `denomination` and the wessex-hoard probe filled
    // `catalogue_ref` on 3 of 14 coins — with the article and the catalogue
    // entry both stating it, and the merge they exist for firing zero times.
    let mut keys: Vec<String> = Vec::new();
    for t in &policies.shape.types {
        for k in index.effective_identity(&t.name) {
            if !keys.iter().any(|seen| seen == k) {
                keys.push(k.to_string());
            }
        }
    }
    let identity = if keys.is_empty() {
        String::new()
    } else {
        format!(
            "\nAlways fill {} when the section states one, even in passing: \
             those keys are what make two mentions one thing, and a mention \
             without them stays separate from every other mention of itself.\n",
            keys.iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(" and "),
        )
    };
    // A ref may be SECTION CONTEXT, not row text. Measured (ei7-ans,
    // 2026-09-22): the model filled `mint` wherever the row printed it and
    // `hoard` almost never, because a coin's hoard is named by the section
    // heading ("A. The Corinth Hoard") and the instruction above says to
    // take values from "the section's own words" — read as the row. The
    // ref-link is the declared ontology's load-bearing relation (the
    // enumeration path walks exactly these edges), so this block names the
    // context source the example cannot show. Only emitted when a declared
    // type carries a ref, so a text-only declaration pays nothing.
    let ref_context = if policies.shape.types.iter().any(|t| {
        index
            .effective_attributes(&t.name)
            .iter()
            .any(|a| matches!(a.family, AttrFamily::Ref { .. }))
    }) {
        "\nA `name of a …` attribute may be stated by the row, by the \
         surrounding catalogue, or by the section heading: a sketch inside \
         a section about one thing belongs to that thing even when the row \
         does not repeat its name. Write that name — an unlinked sketch \
         cannot be found from the thing it belongs to.\n"
            .to_string()
    } else {
        String::new()
    };
    format!(
        "\n## Where attributes go\n\n\
         A declared attribute is a field of the sketch object itself. The \
         example above shows a sketch without one; a `{name}` sketch is \
         written like this instead:\n\n\
         \x20   {{ \"canonical_name\": <text>, \"{slot}\": \"{name}\",\n\
         \x20     \"description\": <text>, \"anchor\": <text>,\n\
         \x20     \"attributes\": {{ {pairs} }} }}\n\n\
         When a type declares an attribute, the value belongs in that object \
         and nowhere else — never restated as a claim, a relation, or prose \
         in the description.\n\n\
          `<text>` and `<number>` are shapes, not values: take each from the \
          section's own words. Leave out any key the section does not state. A \
          `0` or an \"unknown\" put there to fill a slot reads downstream as a \
          measurement, and only a missing key is visibly missing.\n\
          {identity}\
          {ref_context}",
        name = t.name,
        slot = match t.kind {
            TypeKind::Relation => "relation_type",
            TypeKind::Event => "event_type",
            TypeKind::Claim => "claim_kind",
            _ => "entity_type",
        },
    )
}

/// The `attributes` object's keys, rendered as shapes for a prompt example.
///
/// Shared by both worked examples so they cannot drift: an example that omits
/// this object teaches the model to omit it, which is the whole defect these
/// two sections exist to correct. Learned the hard way — the claim example
/// shipped without it for one build and took `proposed_date` from 14 of 43
/// claims to 0 of 41, along with every `grade`.
///
/// `<number>` is spelled out separately from `<text>` because a quantity is
/// the one family whose JSON shape a model routinely gets wrong.
fn attribute_pairs(attrs: &[&AttrDecl]) -> String {
    attrs
        .iter()
        .map(|a| format!("\"{}\": <{}>", a.name, family_shape(a)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What one attribute's family admits, as a placeholder a model can act on.
///
/// The same phrase [`render_attr`] puts on the type bullet, so the example and
/// the list cannot describe a family differently (§10.6). Descriptive rather
/// than a bare `<text>` because the one claim slot that filled when the rest
/// did not was `subject`, whose placeholder says what to put there
/// (`<the canonical_name of the coin>`) — the difference worth testing.
fn family_shape(a: &AttrDecl) -> String {
    match &a.family {
        AttrFamily::Text { values } if !values.is_empty() => {
            format!("one of: {}", values.join(" | "))
        }
        AttrFamily::Text { .. } => "text".to_string(),
        AttrFamily::Quantity { unit: Some(u) } => format!("number in {u}"),
        AttrFamily::Quantity { unit: None } => "number".to_string(),
        AttrFamily::Time { range: true } => "date or range".to_string(),
        AttrFamily::Time { range: false } => "date".to_string(),
        AttrFamily::Ref { of } => format!("name of a {of}"),
    }
}

/// What a declared claim type is ABOUT, shown once.
///
/// The same defect as [`render_attribute_shape`], in the neutral prompt's
/// other slot. That prompt's claim field list names `content`,
/// `discourse_act`, `epistemic_status`, `attributed_to` and `anchor` — and
/// never `subject`, which only exists once a recipe declares one. Its worked
/// example likewise shows a claim with an `attributed_to` and no `subject`.
/// The wessex-hoard build filled `attributed_to` on all 49 claims and
/// `subject` on 1: named in the prompt, filled; absent from it, empty.
///
/// The is-about link is the whole point of `subject = "coin"`. A claim
/// without it cannot be reached from the thing it discusses, which is the
/// question a declared claim type exists to answer ("who disputes this coin's
/// dating"). Empty unless a claim type declares a subject or a deontic mode,
/// so a recipe that declares neither pays nothing.
fn render_subject_shape(policies: &OntologyPolicies, index: &TypeIndex<'_>) -> String {
    // A declared `deontic` is the same defect in the same slot: the mode is
    // in the schema, absent from the only worked example, and filled on 46 of
    // 1,221 obligations (spike 3, 2026-09-19). So a claim type that declares
    // either facet earns the example — a deontic recipe that declares no
    // subject got none at all.
    let Some(t) = policies
        .claim_types()
        .find(|t| t.subject.is_some() || !t.deontic.is_empty())
    else {
        return String::new();
    };
    // The claim example carries its `attributes` object for the same reason
    // the entity example does: an example without one is an instruction to
    // leave it out. `deontic` and `grade` ride in the same bag as the
    // declared attributes (`set_attribute_property`), in the order that
    // function inserts them, so prompt and schema agree on generation order.
    let mut pairs = attribute_pairs(&index.effective_attributes(&t.name));
    if let Some(first) = t.deontic.first() {
        if !pairs.is_empty() {
            pairs.push_str(", ");
        }
        pairs.push_str(&format!("\"deontic\": \"{}\"", wire_name(first)));
    }
    if !t.grades.is_empty() {
        if !pairs.is_empty() {
            pairs.push_str(", ");
        }
        pairs.push_str("\"grade\": <one of the grades above>");
    }
    let attributes = if pairs.is_empty() {
        String::new()
    } else {
        format!("\x20     \"attributes\": {{ {pairs} }}\n")
    };
    // The heading, the lead and the closing paragraph are about the is-about
    // link and nothing else, so a type that declares no subject renders the
    // example without them rather than asserting a link it never declared.
    let Some(about) = t.subject.as_deref() else {
        return format!(
            "\n## What a claim looks like\n\n\
             A `{name}` sketch carries its declared keys:\n\n\
             \x20   {{ \"content\": <text>, \"claim_kind\": \"{name}\",\n\
             \x20     \"attributed_to\": <text>, \"anchor\": <text>,\n\
             {attributes}\
             \x20   }}\n",
            name = t.name,
        );
    };
    format!(
        "\n## What a claim is about\n\n\
         `{name}` is declared as a claim about a `{about}`, so its sketch says \
         WHICH one:\n\n\
         \x20   {{ \"content\": <text>, \"claim_kind\": \"{name}\",\n\
         \x20     \"subject\": <the canonical_name of the {about}>,\n\
         \x20     \"attributed_to\": <text>, \"anchor\": <text>,\n\
         {attributes}\
         \x20   }}\n\n\
         `subject` is what the claim is about; `attributed_to` is who makes \
         it. Several claims in one section about the same {about} each name \
         it again. Omit `subject` rather than guess it.\n",
        name = t.name,
    )
}

/// One attribute, rendered for the prompt: name plus what the family admits.
fn render_attr(a: &AttrDecl) -> String {
    let shape = family_shape(a);
    if a.description.trim().is_empty() {
        format!("{} ({shape})", a.name)
    } else {
        format!("{} ({shape}; {})", a.name, a.description.trim())
    }
}

/// The serde wire spelling of a closed enum value, read back through serde so
/// prompt text and parser cannot disagree about the accepted spelling.

// The tests live in a sibling file: this one entered arch-gate's approach
// band (ARCH §3.1) and the test module is the half that moves cleanly.
// `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "ontology_prompt/tests.rs"]
mod tests;
