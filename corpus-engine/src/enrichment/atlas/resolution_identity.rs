// SPDX-License-Identifier: AGPL-3.0-or-later
//! Identity under a declared ontology: what may fold into what, and what a
//! declared claim is about.
//!
//! Two decisions the name rules in `resolution.rs` cannot make on their own,
//! both inert for an undeclared corpus so the generic resolver's behaviour is
//! exactly what it was. Split from `resolution_ontology.rs` (ARCH §3.1) when
//! it crossed 800 lines; it shares that module's [`ResolutionPolicy`].

use super::atoms::Entity;
use super::resolution_ontology::ResolutionPolicy;

// ── 3a: what may fold into what ──────────────────────────────

/// Whether the declared ontology permits folding one entity mention into
/// another. `Ok(())` for every undeclared corpus, so the generic resolver's
/// merge rules are exactly what they were; a declared corpus gets two vetoes
/// the name rules cannot express:
///
/// 1. A DECLARED type never merges across types. "Series Y sceattas of
///    Aldfrith" (a `coin`) shares a token with "Aldfrith of Northumbria" (a
///    `person`); it is not an alias of him. Specialisation is not a
///    mismatch — a `sceatta` may fold into a `coin`.
/// 2. Two mentions of a declared type carrying DIFFERENT values of that
///    type's declared identity key are two things by the author's own
///    definition (§7.5): "Wessex Down 1" and "Wessex Down 2" are two coins
///    however alike their names. A key present on one side only decides
///    nothing.
///
/// Measured before this existed (wessex-hoard, 2026-09-02): one `coin` atom
/// carried Series R and the Eoforwic mint as aliases, and the person atom
/// carried "Series Y sceattas of Aldfrith". Returns the reason so the caller
/// can trace exactly which veto fired.
/// How strongly the name rules matched two mentions. The generic resolver
/// merges on anything from an exact canonical-name hit down to a shared
/// first token plus one long token; the declared ontology cares which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeEvidence {
    /// The whole name or an alias matched, or one name contains the other.
    Exact,
    /// Token overlap, edit distance, or description similarity.
    Fuzzy,
}

#[allow(clippy::too_many_arguments)]
pub fn merge_permitted(
    policy: &ResolutionPolicy<'_>,
    evidence: MergeEvidence,
    a_type: &str,
    a_name: &str,
    a_attrs: &serde_json::Map<String, serde_json::Value>,
    b_type: &str,
    b_name: &str,
    b_attrs: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if !policy.is_active() {
        return Ok(());
    }
    let declared = |t: &str| policy.index().contains(t);
    // Compare RIGID types. A `role_of` type is a role its atom plays, not the
    // atom's essence (§7.5): a `ruler` sketch resolves to a `person` atom, and
    // comparing the declared names would refuse that merge — it fragmented
    // Beonna into three atoms when this rule first shipped. `rigid_type_of`
    // is the same mapping the endpoint and participant checks use (§10.6).
    let ra: &str = policy.index().rigid_type_of(a_type).unwrap_or(a_type);
    let rb: &str = policy.index().rigid_type_of(b_type).unwrap_or(b_type);
    if ra != rb
        && (declared(a_type) || declared(b_type))
        && !policy.index().is_a(ra, rb)
        && !policy.index().is_a(rb, ra)
    {
        return Err(format!(
            "`{a_name}` is a `{a_type}` and `{b_name}` is a `{b_type}` — a declared \
             type does not merge across types"
        ));
    }
    for t in [a_type, b_type] {
        if !declared(t) {
            continue;
        }
        for key in policy.index().effective_identity(t) {
            let (Some(x), Some(y)) = (identity_value(a_attrs, key), identity_value(b_attrs, key))
            else {
                continue;
            };
            if x != y {
                return Err(format!(
                    "`{a_name}` and `{b_name}` carry different `{key}` (`{x}` vs `{y}`) — \
                     the declared identity key of `{t}`"
                ));
            }
        }
    }
    // 3. A type the author identifies by an external key is not identified
    //    by the shape of its name. "Series R sceatta, runic type" shares a
    //    first token with "Series Y sceattas" and is a different coin; the
    //    key would have said so had both carried one. Fuzzy evidence merges
    //    a keyed type only when a declared key agrees on both sides — an
    //    exact name or alias still merges, and `enrich reconcile` folds by
    //    key afterwards.
    if evidence == MergeEvidence::Fuzzy && !same_name_modulo_plural(a_name, b_name) {
        for t in [a_type, b_type] {
            let keys = policy.index().effective_identity(t);
            if !declared(t) || keys.is_empty() {
                continue;
            }
            let agrees = keys.iter().any(|key| {
                matches!(
                    (identity_value(a_attrs, key), identity_value(b_attrs, key)),
                    (Some(x), Some(y)) if x == y
                )
            });
            if !agrees {
                return Err(format!(
                    "`{a_name}` and `{b_name}` match only by name shape, and `{t}` is \
                     identified by `{}` — no key agrees",
                    keys.join("`, `")
                ));
            }
        }
    }
    Ok(())
}

/// "Series Y sceatta" and "Series Y sceattas" are one name. The generic
/// substring rule wants a whole-word boundary, so a plural falls through to
/// the fuzzy rules — which rule 3 would otherwise refuse for a keyed type.
/// Token-wise, case-insensitive, one trailing `s` of slack per token.
fn same_name_modulo_plural(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| {
                let t = t.to_lowercase();
                t.strip_suffix('s').map(str::to_string).unwrap_or(t)
            })
            .collect()
    };
    let (na, nb) = (norm(a), norm(b));
    !na.is_empty() && na == nb
}

/// An identity key's value, folded for comparison. Strings and numbers only;
/// a value already snapped to an atom id says nothing about identity.
fn identity_value(attrs: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    match attrs.get(key)? {
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() || t.starts_with("entity-") {
                None
            } else {
                Some(t.to_lowercase())
            }
        }
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

// ── 3b: a declared claim is about something of a declared type ───

/// The entity type a declared claim kind is `about`, when the recipe says
/// (`subject = "coin"` on `attribution`). `None` for an undeclared kind.
pub fn declared_subject_type<'a>(
    policy: &ResolutionPolicy<'a>,
    claim_kind: Option<&str>,
) -> Option<&'a str> {
    policy.index().get(claim_kind?)?.subject.as_deref()
}

/// Whether `entity` can be what a claim of a kind declaring `subject =
/// declared` is about — the type itself or a specialisation of it.
pub fn accepts_subject(policy: &ResolutionPolicy<'_>, declared: &str, entity: &Entity) -> bool {
    policy.accepts(declared, entity.entity_type.as_str_repr())
}

#[cfg(test)]
mod tests {
    use super::super::atoms::{AtomId, ChunkRef};
    use super::*;
    use crate::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl, ShapePolicy, TypeKind};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn policies(types: Vec<OntologyTypeDecl>) -> OntologyPolicies {
        OntologyPolicies {
            shape: ShapePolicy { types },
            ..Default::default()
        }
    }

    fn entity_decl(name: &str, specializes: Option<&str>) -> OntologyTypeDecl {
        OntologyTypeDecl {
            name: name.into(),
            kind: TypeKind::Entity,
            specializes: specializes.map(str::to_string),
            ..Default::default()
        }
    }

    fn atom(id: &str, name: &str, ty: EntityType) -> Entity {
        Entity {
            id: AtomId::from_raw(id),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: ty,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: String::new(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    // ── merge veto ───────────────────────────────────────────

    fn with_identity(mut decl: OntologyTypeDecl, keys: &[&str]) -> OntologyTypeDecl {
        decl.identity = keys.iter().map(|k| k.to_string()).collect();
        decl
    }

    fn attrs(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    /// Falsifier: drop the type clause from `merge_permitted` and the coin
    /// folds into the king — which is exactly what wessex-hoard did.
    #[test]
    fn a_declared_type_never_merges_across_types() {
        let p = policies(vec![entity_decl("coin", None)]);
        let policy = ResolutionPolicy::new(&p);
        let none = serde_json::Map::new();
        let err = merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "coin",
            "Series Y sceattas of Aldfrith",
            &none,
            "person",
            "Aldfrith of Northumbria",
            &none,
        )
        .unwrap_err();
        assert!(err.contains("across types"), "{err}");
        // The other direction too: an undeclared-typed sketch into a coin.
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "place",
            "Eoforwic",
            &none,
            "coin",
            "Series Y",
            &none
        )
        .is_err());
    }

    #[test]
    fn a_specialisation_may_fold_into_its_base_type() {
        let p = policies(vec![
            entity_decl("coin", None),
            entity_decl("sceatta", Some("coin")),
        ]);
        let policy = ResolutionPolicy::new(&p);
        let none = serde_json::Map::new();
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "sceatta",
            "Series R",
            &none,
            "coin",
            "Series R sceatta",
            &none
        )
        .is_ok());
    }

    /// Falsifier: drop the identity clause and "Wessex Down 1" and
    /// "Wessex Down 2" become one coin.
    #[test]
    fn different_declared_identity_keys_are_two_things() {
        let p = policies(vec![with_identity(
            entity_decl("coin", None),
            &["catalogue_ref"],
        )]);
        let policy = ResolutionPolicy::new(&p);
        let a = attrs(&[("catalogue_ref", "Wessex Down 1")]);
        let b = attrs(&[("catalogue_ref", "Wessex Down 2")]);
        let err = merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "coin",
            "Series Y sceattas",
            &a,
            "coin",
            "Series Y sceatta",
            &b,
        )
        .unwrap_err();
        assert!(err.contains("catalogue_ref"), "{err}");
        // Same key, differently cased: one thing.
        let b2 = attrs(&[("catalogue_ref", "wessex down 1")]);
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "coin",
            "a",
            &a,
            "coin",
            "b",
            &b2
        )
        .is_ok());
        // Key on one side only decides nothing.
        let none = serde_json::Map::new();
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "coin",
            "a",
            &a,
            "coin",
            "b",
            &none
        )
        .is_ok());
    }

    #[test]
    fn an_undeclared_corpus_has_no_merge_veto() {
        let empty = OntologyPolicies::default();
        let policy = ResolutionPolicy::new(&empty);
        let none = serde_json::Map::new();
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "person",
            "x",
            &none,
            "place",
            "y",
            &none
        )
        .is_ok());
    }

    // ── declared subject ─────────────────────────────────────

    #[test]
    fn a_declared_claim_kind_names_the_type_it_is_about() {
        let mut attribution = OntologyTypeDecl {
            name: "attribution".into(),
            kind: TypeKind::Claim,
            ..Default::default()
        };
        attribution.subject = Some("coin".into());
        let p = policies(vec![
            entity_decl("coin", None),
            entity_decl("sceatta", Some("coin")),
            attribution,
        ]);
        let policy = ResolutionPolicy::new(&p);
        assert_eq!(
            declared_subject_type(&policy, Some("attribution")),
            Some("coin")
        );
        assert_eq!(declared_subject_type(&policy, Some("rumour")), None);
        assert_eq!(declared_subject_type(&policy, None), None);
        let coin = atom("entity-0001", "Series Y", EntityType::Other("coin".into()));
        let sceatta = atom(
            "entity-0002",
            "Series R",
            EntityType::Other("sceatta".into()),
        );
        let king = atom("entity-0003", "Aldfrith", EntityType::Person);
        assert!(accepts_subject(&policy, "coin", &coin));
        assert!(
            accepts_subject(&policy, "coin", &sceatta),
            "a specialisation is accepted"
        );
        assert!(!accepts_subject(&policy, "coin", &king));
    }

    /// Falsifier: drop rule 3 and "Series R sceatta, runic type" folds into
    /// "Series Y sceattas" on a shared first token before either carries a
    /// key — the wessex collapse this exists to stop.
    #[test]
    fn a_keyed_type_does_not_merge_on_name_shape_alone() {
        let p = policies(vec![
            with_identity(entity_decl("coin", None), &["catalogue_ref"]),
            entity_decl("mint", None),
        ]);
        let policy = ResolutionPolicy::new(&p);
        let none = serde_json::Map::new();
        let keyed = attrs(&[("catalogue_ref", "Wessex Down 2")]);
        // Fuzzy, no keys: refused.
        let err = merge_permitted(
            &policy,
            MergeEvidence::Fuzzy,
            "coin",
            "Series R sceatta",
            &keyed,
            "coin",
            "Series Y sceattas",
            &none,
        )
        .unwrap_err();
        assert!(err.contains("name shape"), "{err}");
        // Fuzzy, keys agree: one coin.
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Fuzzy,
            "coin",
            "a",
            &keyed,
            "coin",
            "b",
            &keyed
        )
        .is_ok());
        // Exact name evidence still merges an unkeyed mention.
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "coin",
            "Series Y sceattas of Aldfrith",
            &keyed,
            "coin",
            "Series Y sceattas",
            &none
        )
        .is_ok());
        // A declared type WITHOUT an identity key keeps the generic rules.
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Fuzzy,
            "mint",
            "Eoforwic",
            &none,
            "mint",
            "Eoforwic (York)",
            &none
        )
        .is_ok());
    }

    /// Falsifier: drop `same_name_modulo_plural` and the singular mention of
    /// a keyed coin stays a second atom beside its plural.
    #[test]
    fn a_plural_is_the_same_name_for_a_keyed_type() {
        let p = policies(vec![with_identity(
            entity_decl("coin", None),
            &["catalogue_ref"],
        )]);
        let policy = ResolutionPolicy::new(&p);
        let none = serde_json::Map::new();
        let keyed = attrs(&[("catalogue_ref", "Wessex Down 1")]);
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Fuzzy,
            "coin",
            "Series Y sceatta",
            &none,
            "coin",
            "Series Y sceattas",
            &keyed
        )
        .is_ok());
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Fuzzy,
            "coin",
            "Series R sceatta",
            &none,
            "coin",
            "Series Y sceattas",
            &keyed
        )
        .is_err());
        assert!(same_name_modulo_plural(
            "Series Y sceatta",
            "series y SCEATTAS"
        ));
        assert!(!same_name_modulo_plural(
            "Series Y sceatta",
            "Series Y sceattas of Aldfrith"
        ));
    }

    /// A role is not an essence (§7.5): a `ruler` sketch and the `person`
    /// atom its role resolved to are one thing. Falsifier: compare the
    /// declared names instead of the rigid ones and this refuses — which
    /// fragmented Beonna into three atoms on the first wessex rebuild.
    #[test]
    fn a_role_merges_into_the_atom_that_plays_it() {
        let mut ruler = entity_decl("ruler", None);
        ruler.role_of = Some("person".into());
        let p = policies(vec![ruler, entity_decl("coin", None)]);
        let policy = ResolutionPolicy::new(&p);
        let none = serde_json::Map::new();
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "ruler",
            "Beonna",
            &none,
            "person",
            "Beonna of East Anglia",
            &none
        )
        .is_ok());
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Fuzzy,
            "person",
            "Aldfrith",
            &none,
            "ruler",
            "Aldfrith of Northumbria",
            &none
        )
        .is_ok());
        // The cross-type veto still holds for two different essences.
        assert!(merge_permitted(
            &policy,
            MergeEvidence::Exact,
            "coin",
            "Series Y sceattas of Aldfrith",
            &none,
            "person",
            "Aldfrith",
            &none
        )
        .is_err());
    }
}
