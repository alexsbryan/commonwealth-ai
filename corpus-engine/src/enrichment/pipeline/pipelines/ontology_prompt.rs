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
    let Some((t, attrs)) = policies
        .shape
        .types
        .iter()
        .map(|t| (t, index.effective_attributes(&t.name)))
        .find(|(_, a)| !a.is_empty())
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
         {identity}",
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
/// dating"). Empty unless a claim type declares a subject, so a recipe that
/// declares none pays nothing.
fn render_subject_shape(policies: &OntologyPolicies, index: &TypeIndex<'_>) -> String {
    let Some(t) = policies.claim_types().find(|t| t.subject.is_some()) else {
        return String::new();
    };
    let about = t.subject.as_deref().unwrap_or_default();
    // The claim example carries its `attributes` object for the same reason
    // the entity example does: an example without one is an instruction to
    // leave it out. `grade` rides in the same bag as the declared attributes
    // (`set_attribute_property`), so it is shown in the same bag.
    let mut pairs = attribute_pairs(&index.effective_attributes(&t.name));
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

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::numismatics_policies as numismatics;
    use super::super::ontology_schema::{
        phase1_schema_for, report_added_prompt_size, MAX_ADDED_PROMPT_CHARS,
    };

    // ── Phase 6 extras ───────────────────────────────────────

    #[test]
    fn phase6_extras_empty_when_undeclared() {
        // Invariant I1: a version-0 block (prose only) and a version-1
        // block with no types both add NOTHING to the Phase-6 classifier,
        // so the template renders the bytes it always did.
        assert_eq!(
            render_phase6_extras(&OntologyPolicies::default()),
            "",
            "no declaration, no extras"
        );
        let prose =
            OntologyPolicies::from_prose("Rules about guests and quiet hours.", Default::default());
        assert_eq!(
            render_phase6_extras(&prose),
            "",
            "a version-0 prose block declares no types"
        );
    }

    #[test]
    fn phase6_extras_render_the_relation_section_for_any_declared_corpus() {
        // Numismatics declares types and a `between`, but no
        // `not_conflicts` and no deontic: it gets the `relation` section
        // and neither of the other two.
        let extras = render_phase6_extras(&numismatics());
        assert!(extras.contains("## Relation"), "relation section present");
        assert!(extras.contains("`equivalent`"));
        assert!(
            !extras.contains("## Declared non-conflicts"),
            "numismatics names no non-conflicts"
        );
        assert!(
            !extras.contains("## Deontic reading"),
            "numismatics declares no directive claim type"
        );
        // The term placeholder survives for the caller to substitute.
        assert!(extras.contains("{tension_term}"));
    }

    #[test]
    fn phase6_extras_render_non_conflicts_and_the_deontic_reading_when_declared() {
        let toml = crate::recipe_templates::load_builtin("governance")
            .expect("governance is a shipped ontology template");
        let policies = crate::recipe::Recipe::from_toml(toml)
            .expect("the shipped template parses")
            .custom_atlas_spec()
            .expect("it declares an [enrichment.ontology] block")
            .policies();

        let extras = render_phase6_extras(&policies);
        assert!(extras.contains("## Declared non-conflicts"));
        assert!(
            extras.contains("a rule for visitors versus a rule for members"),
            "the author's own words reach the classifier"
        );
        assert!(extras.contains("## Deontic reading"));
        assert!(
            extras.contains("require not-X"),
            "the interdefinition is what makes two surface forms one rule"
        );
        assert!(extras.contains("## Relation"));
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

    /// The prose alone did not carry it: the neutral Phase-1 prompt this block
    /// is appended to shows a worked JSON example whose `coin` entity has no
    /// `attributes` object, and the wessex-hoard build filled 0 of 14. The
    /// block has to show the shape, in the author's own keys.
    ///
    /// Falsifier: drop `render_attribute_shape` from `render_declared_types`
    /// and the emitted JSON has no example of a filled `attributes` object
    /// anywhere in the Phase-1 prompt.
    #[test]
    fn the_block_shows_where_an_attribute_goes_in_the_json() {
        let block = render_declared_types(&numismatics());
        assert!(block.contains("## Where attributes go"));
        assert!(
            block.contains("\"attributes\": {"),
            "the object is shown as JSON, not only described: {block}"
        );
        assert!(
            block.contains("\"entity_type\": \"coin\""),
            "the sketch is shown whole, in the slot the type actually fills"
        );
        assert!(
            block.contains("never restated as a claim"),
            "the observed competing behaviour is named: the model emitted \
             `weight` as a claim rather than as the coin's attribute"
        );
        for key in [
            "\"metal\": <one of: gold | silver",
            "\"denomination\": <text>",
        ] {
            assert!(block.contains(key), "{key} shown in the author's own keys");
        }
        assert!(
            block.contains("\"weight\": <number in g>"),
            "a quantity is shown as a bare number — the family models get wrong"
        );
        assert!(
            !block.contains("silver") || !block.contains("\"metal\": \"silver\""),
            "the example must not carry a copyable value: a copied value is a \
             fabricated one, and it would read as a filled attribute in the \
             coverage report"
        );
    }

    /// An attribute named in `identity` decides whether two mentions merge, so
    /// it cannot read as one of seven interchangeable keys. `catalogue_ref`
    /// reached 3 of 14 coins while the corpus stated it on both the catalogue
    /// entry and the article, and the merge it exists for fired zero times.
    ///
    /// Falsifier: drop the identity sentence and `catalogue_ref` is named
    /// nowhere except in the same list as `denomination`.
    #[test]
    fn the_identity_keys_are_singled_out() {
        // The SHIPPED template declares no `identity` — the wessex-hoard probe
        // recipe is where P3 added `catalogue_ref` — so the negative case is
        // the fixture as it stands and the positive case declares a key.
        assert!(
            !render_declared_types(&numismatics()).contains("Always fill"),
            "nothing is claimed about identity when the recipe declares no key"
        );

        let mut p = numismatics();
        let coin = p
            .shape
            .types
            .iter_mut()
            .find(|t| t.name == "coin")
            .expect("the template declares a coin");
        coin.attributes.push(AttrDecl {
            name: "catalogue_ref".into(),
            family: AttrFamily::Text { values: vec![] },
            description: String::new(),
        });
        coin.identity = vec!["catalogue_ref".into()];

        let block = render_declared_types(&p);
        assert!(
            block.contains("Always fill `catalogue_ref`"),
            "the declared identity key is named as one: {block}"
        );
    }

    /// The neutral prompt's claim field list names `attributed_to` and never
    /// `subject` — and the build filled `attributed_to` on all 49 claims and
    /// `subject` on 1. Named in the prompt, filled; absent from it, empty.
    ///
    /// Falsifier: drop `render_subject_shape` and the word `subject` appears
    /// nowhere in the Phase-1 prompt for a corpus that declares one.
    #[test]
    fn the_block_says_what_a_declared_claim_is_about() {
        let block = render_declared_types(&numismatics());
        assert!(block.contains("## What a claim is about"));
        assert!(
            block.contains("\"subject\": <the canonical_name of the coin>"),
            "the slot is shown, in the declared subject's own type name: {block}"
        );
        assert!(
            block.contains("`attribution` is declared as a claim about a `coin`"),
            "and named from the declaration, not hardcoded"
        );
        assert!(
            block.contains("`subject` is what the claim is about; `attributed_to` is who makes it"),
            "`subject` is separated from `attributed_to`, which the neutral \
             prompt already asks for and the model already fills"
        );

        // A claim type declaring no subject buys none of it.
        let mut p = numismatics();
        for t in &mut p.shape.types {
            t.subject = None;
        }
        assert!(
            !render_declared_types(&p).contains("## What a claim is about"),
            "nothing is said about a link the recipe never declared"
        );
    }

    /// EVERY worked example in this block shows its `attributes` object.
    ///
    /// The block exists because an example that omits a slot teaches the model
    /// to omit it — so an example in the block that omits `attributes` undoes
    /// the block. That is not hypothetical: the claim example shipped without
    /// one for a single build and took `attribution proposed_date` from 14 of
    /// 43 claims to 0 of 41, and every `grade` with it.
    ///
    /// Falsifier: drop `attributes` from either example and the count of
    /// `"attributes": {` falls below the count of example blocks.
    #[test]
    fn no_worked_example_omits_the_attributes_object() {
        let block = render_declared_types(&numismatics());
        let examples =
            block.matches("\"claim_kind\":").count() + block.matches("\"entity_type\":").count();
        assert!(examples >= 2, "both examples render: {block}");
        assert_eq!(
            block.matches("\"attributes\": {").count(),
            examples,
            "every example carries the object it is teaching: {block}"
        );
        assert!(
            block.contains("\"proposed_date\": <date or range>"),
            "the claim example uses the claim type's OWN declared attributes"
        );
        assert!(
            block.contains("\"grade\": <one of the grades above>"),
            "and the reserved key that rides in the same bag"
        );
    }

    /// A declaration with types but no attributes pays nothing for the block.
    #[test]
    fn nothing_to_fill_renders_no_shape_section() {
        let mut p = numismatics();
        for t in &mut p.shape.types {
            t.attributes.clear();
        }
        let block = render_declared_types(&p);
        assert!(block.starts_with("## Declared types"), "types still named");
        assert!(
            !block.contains("## Where attributes go"),
            "but no attribute shape: {block}"
        );
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
    ///
    /// The SHIPPED template is not the worst case a shipped recipe reaches.
    /// `sovereign-recipes/wessex-hoard/recipe.toml` declares one more `coin`
    /// attribute (`catalogue_ref`) plus an `identity`, and that recipe went
    /// over budget on a build while this test stayed green against the
    /// template — a gate with no input that could fail it (§18.1). So the
    /// probe's shape is measured here too, and the assertion names which of
    /// the two blew the budget.
    #[test]
    fn the_shipped_fixture_fits_the_prompt_budget() {
        let measure = |label: &str, p: &OntologyPolicies| {
            let added =
                report_added_prompt_size(label, &render_declared_types(p), &phase1_schema_for(p));
            assert!(
                added <= MAX_ADDED_PROMPT_CHARS,
                "{label} adds {added} chars, budget {MAX_ADDED_PROMPT_CHARS}"
            );
            added
        };
        let template = measure("numismatics (template)", &numismatics());

        let mut probe = numismatics();
        let coin = probe
            .shape
            .types
            .iter_mut()
            .find(|t| t.name == "coin")
            .expect("the template declares a coin");
        coin.attributes.push(AttrDecl {
            name: "catalogue_ref".into(),
            family: AttrFamily::Text { values: vec![] },
            description: String::new(),
        });
        coin.identity = vec!["catalogue_ref".into()];
        let probe_size = measure("numismatics (wessex-hoard probe)", &probe);

        assert!(
            probe_size > template,
            "the probe is the larger of the two, so it is the one that binds"
        );
    }
}
