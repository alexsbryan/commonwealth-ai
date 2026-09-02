// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ninth schema-validation dimension: did the DECLARED ontology reach the
//! atoms?
//!
//! The question the whole ontology-v1 program exists to answer, computed in
//! the one artefact an operator already runs after a build
//! (`svrn enrich schema-report`). It sits beside [`super::schema_validation`]
//! rather than inside it for two reasons: the other eight dimensions are about
//! the fixed atom model and this one is about the author's own nouns, and that
//! file is already past ARCH §3.1's ceiling.
//!
//! Present only when the atlas carries an `ontology.json`. Absent — not
//! zeroed — otherwise: "this corpus declares nothing" and "this corpus
//! declared types and got none" are different findings (§18.3).

use serde::{Deserialize, Serialize};

use super::atoms::AtomEnvelope;
use super::projection::{attributes_of, subtype_of};
use super::writer::AtlasOntologyFile;
use crate::enrichment::ontology::TypeIndex;

/// Did the declared ontology reach the atoms?
///
/// The question the whole program exists to answer, in the one artefact an
/// operator already runs after a build. A declared type with zero atoms is
/// the headline failure — it is what the as-built probe measured (0 of 1
/// surviving) — so it gets its own gap signature and the comparator can see
/// it recur across corpora.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyCoverage {
    /// The declaration language version the atlas was built under.
    pub ontology_version: u32,
    /// One row per declared type, in declaration order.
    pub by_type: Vec<DeclaredTypeCount>,
    /// One row per declared type, naming what makes two of them one thing.
    pub identity: Vec<IdentityCriterion>,
    /// Clusters the reconciler collapsed, from `reconciliation.json`. `None`
    /// when `svrn enrich reconcile` has not been run on this corpus — which is
    /// not the same as zero merges.
    pub merges: Option<usize>,
    /// `same_as` Claims in the atlas — the reified merges. Counted from the
    /// atoms, so this is what a reader would actually find.
    pub same_as_claims: usize,
    /// Claims of a type that declares a `subject` whose subject did not
    /// resolve. The is-about link is what a declared claim type is FOR, so a
    /// high count here means the type is present in name only.
    pub claims_missing_subject: usize,
    /// One row per (declared type, declared attribute) — how much of the
    /// author's attribute surface the build actually filled. Empty when no
    /// declared type declares an attribute.
    pub attribute_fill: Vec<AttributeFill>,
}

/// How much of one declared attribute the build actually filled.
///
/// A declared type can land perfectly BY NAME and carry nothing. That is what
/// the wessex-hoard probe measured on 2026-09-02: `coin` reached 14 atoms and
/// not one of them carried `metal`, `weight` or `catalogue_ref`, while the
/// type-count dimension above reported the type as fully covered. A count of
/// atoms is not a measurement of the declaration reaching them, so the fill
/// rate is its own dimension.
///
/// One row per attribute rather than an average per type: "the model never
/// fills `weight`" and "it fills every attribute half the time" are different
/// findings needing different fixes, and an average hides both (§18.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeFill {
    /// The declaring type — the author's noun.
    pub type_name: String,
    /// The declared attribute name. Inherited attributes appear under the
    /// type that inherits them, so `sceatta` carries `coin`'s rows too.
    pub attribute: String,
    /// Atoms whose subtype is this type or a `specializes` descendant — every
    /// atom the attribute could conceivably describe.
    pub atoms: usize,
    /// Of `atoms`, those whose atom KIND has an attributes slot at all. A
    /// `role_of` type lands as a State and States carry no attributes, so
    /// `atoms > 0 && with_slot == 0` means the declaration has nowhere to
    /// land — a declaration defect, not a model failure.
    pub with_slot: usize,
    /// Of `with_slot`, those carrying a value for this attribute.
    pub filled: usize,
}

/// Atoms carrying one declared type.
///
/// Counted by SUBTYPE across every atom kind, not within `kind` — because a
/// declared type does not always produce the kind it declares. A `role_of`
/// type is declared `kind = "entity"` and produces `State` atoms on the rigid
/// entity (`ruler role_of person`: the atoms are people, the roles are
/// states), so counting `ruler` inside the Entity bucket would report zero for
/// a role that landed perfectly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclaredTypeCount {
    /// The atom kind the type specializes (`entity`, `claim`, …).
    pub kind: String,
    /// The author's noun.
    pub name: String,
    /// Atoms whose subtype IS this name.
    pub count: usize,
    /// Atoms whose subtype is this name or any `specializes` descendant —
    /// what "how many coins are in the catalogue" means when `sceatta`
    /// specializes `coin`.
    pub count_with_subtypes: usize,
}

/// What makes two mentions of a declared type one thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityCriterion {
    pub type_name: String,
    /// `external:<keys>`, `fallback:<keys>`, or `default:canonical_name` —
    /// the last being what a type that declares neither resolves on, stated
    /// rather than left blank.
    pub criterion: String,
}

impl OntologyCoverage {
    pub(super) fn gap_signatures(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .by_type
            .iter()
            .filter(|t| t.count_with_subtypes == 0)
            .map(|t| format!("coverage:zero:{}", t.name))
            .collect();
        // A declared attribute that reached no atom, and a declared attribute
        // that had nowhere to land, are separate signatures: the first is
        // answered by the extraction prompt, the second by editing the recipe.
        for f in &self.attribute_fill {
            if f.atoms > 0 && f.with_slot == 0 {
                let sig = format!("attribute:unlandable:{}", f.type_name);
                if !out.contains(&sig) {
                    out.push(sig);
                }
            } else if f.with_slot > 0 && f.filled == 0 {
                out.push(format!("attribute:zero:{}:{}", f.type_name, f.attribute));
            }
        }
        out
    }
}

/// The ninth dimension. Pure over the atoms plus the declaration, so the same
/// atlas always reports the same coverage.
pub fn build_ontology_coverage(
    ontology: &AtlasOntologyFile,
    atoms: &[AtomEnvelope],
    merges: Option<usize>,
) -> OntologyCoverage {
    let index = TypeIndex::from_policies(&ontology.policies);
    let subtypes: Vec<String> = atoms.iter().map(subtype_of).collect();

    let by_type = ontology
        .policies
        .shape
        .types
        .iter()
        .map(|t| DeclaredTypeCount {
            kind: serde_json::to_string(&t.kind)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string(),
            name: t.name.clone(),
            count: subtypes.iter().filter(|s| *s == &t.name).count(),
            count_with_subtypes: subtypes
                .iter()
                .filter(|s| *s == &t.name || index.is_a(s, &t.name))
                .count(),
        })
        .collect();

    // Every declared type gets a row, including the ones that declare nothing:
    // "resolves on its canonical name" is the answer, and leaving the row out
    // would read as "no criterion" rather than "the default one".
    let identity = ontology
        .policies
        .shape
        .types
        .iter()
        .map(|t| {
            let external = index.effective_identity(&t.name);
            let fallback = index.effective_identity_fallback(&t.name);
            let criterion = if !external.is_empty() {
                format!("external:{}", external.join(", "))
            } else if !fallback.is_empty() {
                format!("fallback:{}", fallback.join(", "))
            } else {
                "default:canonical_name".to_string()
            };
            IdentityCriterion {
                type_name: t.name.clone(),
                criterion,
            }
        })
        .collect();

    // A claim type that declares a `subject` and produced claims without one
    // is present in name only — the is-about link is the point of declaring
    // it.
    let wants_subject: std::collections::BTreeSet<&str> = ontology
        .policies
        .claim_types()
        .filter(|t| t.subject.is_some())
        .map(|t| t.name.as_str())
        .collect();
    let claims_missing_subject = atoms
        .iter()
        .filter(|a| match a {
            AtomEnvelope::Claim(c) => {
                c.subject.is_none()
                    && c.claim_kind
                        .as_deref()
                        .is_some_and(|k| wants_subject.contains(k))
            }
            _ => false,
        })
        .count();

    let same_as_claims = atoms
        .iter()
        .filter(|a| match a {
            AtomEnvelope::Claim(c) => c.claim_kind.as_deref() == Some("same_as"),
            _ => false,
        })
        .count();

    // The fill rate walks (type × effective attribute) against the atoms that
    // landed on that type. `with_slot` is counted from the atom rather than
    // inferred from the declaration, so a type whose atoms cannot carry
    // attributes reports that fact instead of an unfilled attribute.
    let mut attribute_fill = Vec::new();
    for t in &ontology.policies.shape.types {
        let decls = index.effective_attributes(&t.name);
        if decls.is_empty() {
            continue;
        }
        let mine: Vec<&AtomEnvelope> = atoms
            .iter()
            .zip(subtypes.iter())
            .filter(|(_, s)| *s == &t.name || index.is_a(s, &t.name))
            .map(|(a, _)| a)
            .collect();
        for decl in decls {
            let with_slot = mine.iter().filter(|a| attributes_of(a).is_some()).count();
            let filled = mine
                .iter()
                .filter_map(|a| attributes_of(a))
                .filter(|m| m.contains_key(&decl.name))
                .count();
            attribute_fill.push(AttributeFill {
                type_name: t.name.clone(),
                attribute: decl.name.clone(),
                atoms: mine.len(),
                with_slot,
                filled,
            });
        }
    }

    OntologyCoverage {
        ontology_version: ontology.ontology_version,
        by_type,
        identity,
        merges,
        same_as_claims,
        claims_missing_subject,
        attribute_fill,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::ontology::OntologyPolicies;

    /// A declaration with one attributed entity type and one `role_of` type
    /// that also declares an attribute — the two cases the fill dimension has
    /// to tell apart.
    fn policies() -> OntologyPolicies {
        let toml = r#"
[corpus]
id = "t"
name = "t"
schema_version = 1

[acquire]
type = "local_file"
path = "t.md"

[extract]
type = "markdown"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
type = "atlas"

[enrichment.ontology]
version = 1
guidance = "coins"

[[enrichment.ontology.types]]
name = "coin"
kind = "entity"
attributes = [
  { name = "metal", type = "text", values = ["gold", "silver"] },
  { name = "weight", type = "quantity", unit = "g" },
]

[[enrichment.ontology.types]]
name = "ruler"
kind = "entity"
role_of = "person"
attributes = [{ name = "reigned", type = "time", range = true }]
"#;
        crate::recipe::Recipe::from_toml(toml)
            .expect("the fixture recipe parses")
            .custom_atlas_spec()
            .expect("it declares an [enrichment.ontology] block")
            .policies()
    }

    fn ontology() -> AtlasOntologyFile {
        AtlasOntologyFile {
            schema_version: "1.0".into(),
            ontology_version: 1,
            policies: policies(),
        }
    }

    fn atom(v: serde_json::Value) -> AtomEnvelope {
        serde_json::from_value(v).expect("the atom fixture parses")
    }

    /// A `coin` entity carrying `metal` but not `weight`.
    fn coin(id: &str, attributes: serde_json::Value) -> AtomEnvelope {
        atom(serde_json::json!({
            "atom_type": "Entity",
            "data": {
                "id": id,
                "canonical_name": id,
                "entity_type": "coin",
                "first_appearance": { "chunk_id": "sec_00001" },
                "description": "a coin",
                "salience": 1.0,
                "enrichment_depth": "extracted",
                "attributes": attributes,
            }
        }))
    }

    /// The atom a `role_of` type actually produces: a State on the rigid
    /// person atom, which has no attributes map at all.
    fn ruler_state(id: &str) -> AtomEnvelope {
        atom(serde_json::json!({
            "atom_type": "State",
            "data": {
                "id": id,
                "entity_id": "entity-0001",
                "label": "king of Northumbria",
                "state_type": "ruler",
                "section_range": { "start": "sec_00001", "end": "sec_00001" },
                "enrichment_depth": "extracted",
            }
        }))
    }

    fn fill<'a>(c: &'a OntologyCoverage, ty: &str, attr: &str) -> &'a AttributeFill {
        c.attribute_fill
            .iter()
            .find(|f| f.type_name == ty && f.attribute == attr)
            .unwrap_or_else(|| panic!("no fill row for {ty}.{attr}"))
    }

    /// The measurement the type-count dimension cannot make: `coin` reached
    /// two atoms and `weight` reached neither of them.
    ///
    /// Falsifier: fill `weight` on either atom and `filled` becomes 1.
    #[test]
    fn a_type_that_landed_can_still_carry_nothing() {
        let atoms = vec![
            coin("entity-0001", serde_json::json!({ "metal": "silver" })),
            coin("entity-0002", serde_json::json!({})),
        ];
        let c = build_ontology_coverage(&ontology(), &atoms, None);

        let metal = fill(&c, "coin", "metal");
        assert_eq!((metal.atoms, metal.with_slot, metal.filled), (2, 2, 1));

        let weight = fill(&c, "coin", "weight");
        assert_eq!(
            (weight.atoms, weight.with_slot, weight.filled),
            (2, 2, 0),
            "both coins could have carried a weight and neither did"
        );
        assert!(
            c.gap_signatures()
                .contains(&"attribute:zero:coin:weight".to_string()),
            "the unfilled attribute raises its own signature: {:?}",
            c.gap_signatures()
        );
        assert!(
            !c.gap_signatures()
                .contains(&"attribute:zero:coin:metal".to_string()),
            "a partially filled attribute is not a zero"
        );
    }

    /// A `role_of` type lands as a State, and States have no attributes map —
    /// so `reigned` has nowhere to go. Reporting that as "the model never
    /// filled it" would send the author to the prompt for a defect that is in
    /// the declaration (§18.3).
    ///
    /// Falsifier: count `with_slot` from the declaration rather than from the
    /// atom, and this reports 0/2 unfilled with a `attribute:zero:` signature.
    #[test]
    fn a_role_types_attributes_have_nowhere_to_land_and_say_so() {
        let atoms = vec![ruler_state("state-0001"), ruler_state("state-0002")];
        let c = build_ontology_coverage(&ontology(), &atoms, None);

        let reigned = fill(&c, "ruler", "reigned");
        assert_eq!(
            (reigned.atoms, reigned.with_slot, reigned.filled),
            (2, 0, 0),
            "two ruler atoms, no attribute slot on either"
        );

        let sigs = c.gap_signatures();
        assert!(
            sigs.contains(&"attribute:unlandable:ruler".to_string()),
            "the declaration defect is named as one: {sigs:?}"
        );
        assert!(
            !sigs.contains(&"attribute:zero:ruler:reigned".to_string()),
            "and is NOT reported as an unfilled attribute: {sigs:?}"
        );
    }

    /// A declared type with no atoms at all raises the existing zero-coverage
    /// signature and no attribute signature — there is nothing to have filled.
    #[test]
    fn a_type_with_no_atoms_raises_only_the_coverage_signature() {
        let c = build_ontology_coverage(&ontology(), &[], None);
        let sigs = c.gap_signatures();
        assert!(sigs.contains(&"coverage:zero:coin".to_string()));
        assert!(
            !sigs.iter().any(|s| s.starts_with("attribute:")),
            "no atoms means no attribute finding: {sigs:?}"
        );
    }
}
