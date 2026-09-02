// SPDX-License-Identifier: AGPL-3.0-or-later
//! The declared-ontology passes of Phase 3 resolution.
//!
//! P2 got the author's nouns as far as the SKETCH: a `coin` survives the
//! parse with a `weight` of 1.29 and a `mint` of `"Hamwic"`. What it could
//! not do is make any of that point at anything — `"Hamwic"` is a string, a
//! relation's ends are unchecked, and a `ruler` is a separate atom from the
//! person who is one. This module is the difference, and it holds only the
//! passes that READ a [`ResolutionPolicy`]; the id allocation, the atom
//! construction and the trajectory walk stay in
//! [`super::resolution`], which already owns them.
//!
//! Every pass here is pure and takes the already-resolved entity set, so each
//! is testable without an embedder and none of them can renumber an atom.
//!
//! **What is deliberately not checked.** A `ref` attribute snaps by NAME and
//! nothing verifies that the atom it landed on is of the declared `of` type.
//! `of` earns its keep in the prompt (P2 renders it) and in
//! `recipe validate`; adding a second, stricter gate here would refuse a
//! correct snap whenever Phase 1 typed the target as one of the generic six,
//! which is the common case on a first extraction. The resolver's own
//! ambiguity guard is the gate, and an unresolved ref is recorded, not
//! silently dropped (§18.3).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tracing::debug;

use super::atoms::{AtomId, ChunkRef, Entity, Event};
use crate::enrichment::ontology::{AttrFamily, OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::atlas::{EntityType, SectionExtraction};
use crate::enrichment::pipeline::types::{PhaseFailure, PhaseFailureKind, PipelinePhase};

/// Phase 3 rides on the Questions cache, the way every other resolution drop
/// in this file's sibling does. One spelling, so a filter on it finds all of
/// them.
const PHASE: PipelinePhase = PipelinePhase::Questions;

/// What the resolver needs to know about the declared ontology.
///
/// Borrowed from the [`OntologyPolicies`] the recipe produced, so it costs one
/// pass over `shape.types` and no allocation per declared type. The default —
/// what `resolve_step_3b` (the shim) passes and what every version-0 corpus
/// gets — declares nothing, and [`Self::is_active`] is the single predicate
/// every pass below short-circuits on.
#[derive(Debug, Clone, Default)]
pub struct ResolutionPolicy<'a> {
    index: TypeIndex<'a>,
    /// Declared `ref` attribute name → the type name it points at,
    /// corpus-wide. Attribute names are already per-type-validated by the
    /// Phase-1 reader, so a flat map is enough here and it survives the
    /// rigidification of a role-typed atom (which loses the role from the
    /// atom but not from the attribute).
    ref_attributes: BTreeMap<&'a str, &'a str>,
}

impl<'a> ResolutionPolicy<'a> {
    /// Read the policies a recipe produced. Cheap; build it per run.
    pub fn new(policies: &'a OntologyPolicies) -> Self {
        let mut ref_attributes: BTreeMap<&'a str, &'a str> = BTreeMap::new();
        for t in &policies.shape.types {
            for a in &t.attributes {
                if let AttrFamily::Ref { of } = &a.family {
                    ref_attributes.entry(a.name.as_str()).or_insert(of.as_str());
                }
            }
        }
        Self {
            index: TypeIndex::from_policies(policies),
            ref_attributes,
        }
    }

    /// Does this corpus declare any type? False for version 0 and for a
    /// version-1 block with no `[[types]]`, which is what keeps every pass
    /// below a no-op on the prebuilt corpora (I5).
    pub fn is_active(&self) -> bool {
        !self.index.is_empty()
    }

    /// The type index, for the checks that need the `specializes` chain.
    pub fn index(&self) -> &TypeIndex<'a> {
        &self.index
    }

    /// The type name to check an atom against when the recipe names `declared`
    /// at a constrained position. A ROLE resolves to its rigid type, because
    /// that is what the atom carries — `from = "ruler"` is satisfied by the
    /// person atom that holds the `ruler` State.
    fn expected_at_position<'b>(&self, declared: &'b str) -> &'b str
    where
        'a: 'b,
    {
        self.index.rigid_type_of(declared).unwrap_or(declared)
    }

    /// Is `atom_type` acceptable where `declared` was required? True on an
    /// exact match and on any `specializes` descendant, after the role→rigid
    /// normalisation above.
    fn accepts(&self, declared: &str, atom_type: &str) -> bool {
        let expected = self.expected_at_position(declared);
        atom_type == expected || self.index.is_a(atom_type, expected)
    }
}

// ── 3a: roles are not essences ───────────────────────────────

/// The entity type an atom should carry given what the sketch said.
///
/// `ruler role_of person` means a `ruler` mention IS a person; the role is
/// recorded as a State by [`role_mentions`], never as the atom's kind (§7.5,
/// identity from essence — a part played is not an essence). Rigidifying here
/// rather than at read time also makes the type independent of which mention
/// happened to come first: a corpus that says "Aldfrith (ruler)" in §3 and
/// "Aldfrith" in §1 gets one person atom either way.
///
/// Anything else — an undeclared name, a declared type that plays no role —
/// is returned unchanged.
pub fn rigid_entity_type(policy: &ResolutionPolicy<'_>, declared: &EntityType) -> EntityType {
    let EntityType::Other(name) = declared else {
        return declared.clone();
    };
    match policy.index.rigid_type_of(name) {
        Some(rigid) => {
            debug!(
                role = %name,
                rigid,
                "atlas/resolution 3a: role_of — atom takes the rigid type, role becomes a State"
            );
            EntityType::from_str_repr(rigid)
        }
        None => declared.clone(),
    }
}

/// One mention of a declared ROLE, already resolved to the rigid atom that
/// plays it. [`super::resolution`] turns each into a `State` + `Involves` +
/// `Grounds`, using its own id counters — this pass allocates no ids.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleMention {
    /// The rigid entity atom the role is a role OF.
    pub owner: AtomId,
    /// The declared role name; becomes the State's label and `state_type`.
    pub role: String,
    /// Section the mention was in, for the State's range and evidence.
    pub section_id: String,
    /// The sketch's anchor, or empty.
    pub anchor: String,
}

/// Every role mention in the corpus, in section order then sketch order.
///
/// A role is claimed by an ENTITY SKETCH naming it as its type — the atom that
/// mention produced has already been rigidified by [`rigid_entity_type`], so
/// the role name survives only here, in the sketches. One State per mention,
/// not one per atom: two sections that both call Aldfrith a ruler are two
/// points on his trajectory, which is what lets the existing Transition pass
/// chain them for free.
pub fn role_mentions(
    policy: &ResolutionPolicy<'_>,
    sections: &[SectionExtraction],
    entities: &[Entity],
    name_index: &HashMap<String, AtomId>,
    token_index: &HashMap<String, Vec<AtomId>>,
) -> (Vec<RoleMention>, Vec<PhaseFailure>) {
    let mut out = Vec::new();
    let mut failures = Vec::new();
    if !policy.is_active() {
        return (out, failures);
    }
    for section in sections {
        for (sketch_index, sketch) in section.entities_introduced.iter().enumerate() {
            let EntityType::Other(role) = &sketch.entity_type else {
                continue;
            };
            if policy.index.rigid_type_of(role).is_none() {
                continue;
            }
            let Some(owner) = super::resolution::resolve_entity_id_with_salience(
                &sketch.canonical_name,
                entities,
                name_index,
                token_index,
            ) else {
                // The atom this sketch produced cannot be found by its own
                // name — the fuzzy resolver refused on ambiguity. Recorded,
                // never guessed at (§18.3).
                failures.push(PhaseFailure {
                    phase: PHASE,
                    subject: format!("sketch:role:{}#{}", section.section_id, sketch_index),
                    kind: PhaseFailureKind::UnresolvedEntityName,
                    reason: format!(
                        "role mention `{}` (role: `{role}`) did not resolve back to an \
                         Entity atom, so the role State has no owner",
                        sketch.canonical_name.trim()
                    ),
                    raw_response_head: None,
                });
                continue;
            };
            debug!(
                entity = %sketch.canonical_name,
                role = %role,
                owner = %owner.as_str(),
                section = %section.section_id,
                "atlas/resolution 3b: role mention becomes a State on the rigid atom"
            );
            out.push(RoleMention {
                owner,
                role: role.clone(),
                section_id: section.section_id.clone(),
                anchor: sketch.anchor.clone(),
            });
        }
    }
    (out, failures)
}

// ── Endpoint and participant type checks ─────────────────────

/// Check a declared relation's ends against the atoms they resolved to.
///
/// `Ok(())` when the relation may stand. `Err(reason)` names the position, the
/// declared type and what was actually there — the caller drops the relation
/// and records [`PhaseFailureKind::EndpointTypeMismatch`] with that text.
///
/// Ends are positional: `participant_ids[0]` is `from`, `[1]` is `to`, the
/// order the sketch schema documents. Participants past the second are
/// unconstrained (a declaration has two ends; a sketch may name more), and a
/// relation with fewer than two never reaches here — the resolver drops it
/// first.
pub fn check_relation_endpoints(
    policy: &ResolutionPolicy<'_>,
    relation_type: &str,
    participant_ids: &[AtomId],
    entities: &[Entity],
) -> Result<(), String> {
    let ends = policy.index.endpoints(relation_type);
    for (position, declared) in [("from", ends[0]), ("to", ends[1])] {
        let Some(declared) = declared else {
            continue;
        };
        let idx = usize::from(position == "to");
        let Some(id) = participant_ids.get(idx) else {
            continue;
        };
        let Some(actual) = entities.iter().find(|e| &e.id == id) else {
            continue;
        };
        let actual_type = actual.entity_type.as_str_repr();
        if !policy.accepts(declared, actual_type) {
            return Err(format!(
                "relation `{relation_type}` declares {position} = `{declared}` but \
                 `{}` is a `{actual_type}`",
                actual.canonical_name
            ));
        }
    }
    Ok(())
}

/// Drop event participants that are not of a declared participant type.
///
/// The declaration is `role → type`; a sketch names participants as a bare
/// ordered list, so pairing them to roles positionally would be a guess. What
/// IS checkable without guessing is membership: an event of a declared type
/// admits atoms of the declared participant types (and their `specializes`
/// descendants), and an atom of some other type is not in the event.
///
/// The participant is dropped, never the event — the event happened, and the
/// evidence for it does not depend on getting every participant right. Returns
/// the ids dropped per event so the caller can remove the matching `Involves`
/// edges; leaving those behind would put an edge in the graph asserting exactly
/// what this pass just refused.
pub fn check_event_participants(
    policy: &ResolutionPolicy<'_>,
    events: &mut [Event],
    entities: &[Entity],
) -> (Vec<(AtomId, AtomId)>, Vec<PhaseFailure>) {
    let mut dropped: Vec<(AtomId, AtomId)> = Vec::new();
    let mut failures: Vec<PhaseFailure> = Vec::new();
    if !policy.is_active() {
        return (dropped, failures);
    }
    let by_id: HashMap<&str, &Entity> = entities.iter().map(|e| (e.id.as_str(), e)).collect();
    for event in events.iter_mut() {
        let event_type = event.event_type.as_str_repr().to_string();
        let declared = policy.index.participants(&event_type);
        if declared.is_empty() {
            continue;
        }
        let allowed: BTreeSet<&str> = declared.iter().map(|(_, ty)| *ty).collect();
        let event_id = event.id.clone();
        event.participants.retain(|pid| {
            let Some(entity) = by_id.get(pid.as_str()) else {
                return true;
            };
            let actual = entity.entity_type.as_str_repr();
            if allowed.iter().any(|d| policy.accepts(d, actual)) {
                return true;
            }
            debug!(
                event = %event_id.as_str(),
                event_type = %event_type,
                participant = %entity.canonical_name,
                actual,
                "atlas/resolution 3a: event participant is not a declared type; dropping"
            );
            failures.push(PhaseFailure {
                phase: PHASE,
                subject: format!("atom:{}", event_id.as_str()),
                kind: PhaseFailureKind::EndpointTypeMismatch,
                reason: format!(
                    "event `{event_type}` admits participants of {} but `{}` is a `{actual}`",
                    allowed
                        .iter()
                        .map(|t| format!("`{t}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    entity.canonical_name
                ),
                raw_response_head: None,
            });
            dropped.push((event_id.clone(), pid.clone()));
            false
        });
    }
    (dropped, failures)
}

// ── `ref` attributes become atom ids ─────────────────────────

/// Replace every declared `ref` attribute value with the atom id it names.
///
/// Returns the updated attribute maps keyed by entity id — the caller applies
/// them the way it applies `entity_qualifier_updates`, so this pass borrows
/// the entity set immutably and can run beside the others.
///
/// An unresolvable ref KEEPS the name the model wrote and records
/// [`PhaseFailureKind::UnresolvedAttributeRef`]. A string that cannot be
/// followed is worth strictly more than an absent field: a reader still learns
/// that the catalogue said "Hamwic", and the failure says the graph has no edge
/// for it (§18.3 — absence is reported, never defaulted).
pub fn snap_ref_attributes(
    policy: &ResolutionPolicy<'_>,
    entities: &[Entity],
    name_index: &HashMap<String, AtomId>,
    token_index: &HashMap<String, Vec<AtomId>>,
) -> (
    BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
    Vec<PhaseFailure>,
) {
    let mut updates = BTreeMap::new();
    let mut failures = Vec::new();
    if policy.ref_attributes.is_empty() {
        return (updates, failures);
    }
    for entity in entities {
        let mut changed = false;
        let mut attributes = entity.attributes.clone();
        for (attr, of) in &policy.ref_attributes {
            let Some(value) = attributes.get(*attr).and_then(|v| v.as_str()) else {
                continue;
            };
            let name = value.trim().to_string();
            if name.is_empty() {
                continue;
            }
            match super::resolution::resolve_entity_id_with_salience(
                &name,
                entities,
                name_index,
                token_index,
            ) {
                Some(target) => {
                    debug!(
                        entity = %entity.canonical_name,
                        attribute = %attr,
                        target = %target.as_str(),
                        "atlas/resolution 3b: ref attribute snapped to an atom id"
                    );
                    attributes.insert(
                        (*attr).to_string(),
                        serde_json::Value::String(target.as_str().to_string()),
                    );
                    changed = true;
                }
                None => {
                    debug!(
                        entity = %entity.canonical_name,
                        attribute = %attr,
                        value = %name,
                        "atlas/resolution 3b: ref attribute did not resolve; keeping the name"
                    );
                    failures.push(PhaseFailure {
                        phase: PHASE,
                        subject: format!("atom:{}", entity.id.as_str()),
                        kind: PhaseFailureKind::UnresolvedAttributeRef,
                        reason: format!(
                            "`{}`.{attr} = `{name}` (declared ref to `{of}`) did not resolve \
                             to an Entity atom; the attribute keeps the name",
                            entity.canonical_name
                        ),
                        raw_response_head: None,
                    });
                }
            }
        }
        if changed {
            updates.insert(entity.id.as_str().to_string(), attributes);
        }
    }
    (updates, failures)
}

/// The evidence a role State grounds on. Same shape the sibling module's
/// sketch anchors produce, so a role State is indistinguishable from an
/// extracted one downstream.
pub(super) fn role_evidence(mention: &RoleMention) -> Vec<ChunkRef> {
    if mention.anchor.trim().is_empty() {
        vec![ChunkRef::new(mention.section_id.clone(), None)]
    } else {
        vec![ChunkRef::new(
            mention.section_id.clone(),
            Some(mention.anchor.clone()),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::ontology::{AttrDecl, OntologyTypeDecl, ShapePolicy, TypeKind};
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

    #[test]
    fn an_undeclared_corpus_leaves_every_pass_inert() {
        let empty = OntologyPolicies::default();
        let policy = ResolutionPolicy::new(&empty);
        assert!(!policy.is_active());
        let entities = vec![atom("entity-0001", "Aldfrith", EntityType::Person)];
        let (mentions, failures) =
            role_mentions(&policy, &[], &entities, &HashMap::new(), &HashMap::new());
        assert!(mentions.is_empty() && failures.is_empty());
        let (updates, ref_failures) =
            snap_ref_attributes(&policy, &entities, &HashMap::new(), &HashMap::new());
        assert!(updates.is_empty() && ref_failures.is_empty());
        assert_eq!(
            ResolutionPolicy::default().is_active(),
            false,
            "the shim's policy declares nothing"
        );
    }

    #[test]
    fn a_role_endpoint_is_satisfied_by_the_rigid_atom_that_plays_it() {
        // `struck_by` runs coin → ruler, but `ruler role_of person`, so the
        // atom at the `to` end is a PERSON. A check that compared the declared
        // name to the atom's type would refuse every correct relation.
        let p = policies(vec![
            entity_decl("coin", None),
            entity_decl("sceatta", Some("coin")),
            OntologyTypeDecl {
                name: "ruler".into(),
                kind: TypeKind::Entity,
                role_of: Some("person".into()),
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "struck_by".into(),
                kind: TypeKind::Relation,
                from: Some("coin".into()),
                to: Some("ruler".into()),
                ..Default::default()
            },
        ]);
        let policy = ResolutionPolicy::new(&p);
        let entities = vec![
            atom(
                "entity-0001",
                "Series Y",
                EntityType::Other("sceatta".into()),
            ),
            atom("entity-0002", "Aldfrith", EntityType::Person),
            atom("entity-0003", "Eoforwic", EntityType::Other("mint".into())),
        ];
        let ids = |a: &str, b: &str| vec![AtomId::from_raw(a), AtomId::from_raw(b)];

        // `sceatta specializes coin` satisfies `from`; the person satisfies
        // the `ruler` end.
        assert!(check_relation_endpoints(
            &policy,
            "struck_by",
            &ids("entity-0001", "entity-0002"),
            &entities
        )
        .is_ok());

        let err = check_relation_endpoints(
            &policy,
            "struck_by",
            &ids("entity-0001", "entity-0003"),
            &entities,
        )
        .expect_err("a mint is not a ruler");
        assert!(
            err.contains("to = `ruler`") && err.contains("Eoforwic"),
            "{err}"
        );
    }

    #[test]
    fn an_event_drops_the_participant_that_is_not_a_declared_type() {
        let mut p = policies(vec![
            entity_decl("coin", None),
            entity_decl("mint", None),
            OntologyTypeDecl {
                name: "striking".into(),
                kind: TypeKind::Event,
                ..Default::default()
            },
        ]);
        if let Some(t) = p.shape.types.iter_mut().find(|t| t.name == "striking") {
            t.participants.insert("struck".into(), "coin".into());
            t.participants.insert("at".into(), "mint".into());
        }
        let policy = ResolutionPolicy::new(&p);
        let entities = vec![
            atom("entity-0001", "Series Y", EntityType::Other("coin".into())),
            atom("entity-0002", "Eoforwic", EntityType::Other("mint".into())),
            atom("entity-0003", "The Cataloguer", EntityType::Person),
        ];
        let mut events = vec![Event {
            id: AtomId::from_raw("event-0001"),
            description: "Struck at Eoforwic".into(),
            event_type: crate::enrichment::pipeline::atlas::EventType::Other("striking".into()),
            participants: vec![
                AtomId::from_raw("entity-0001"),
                AtomId::from_raw("entity-0002"),
                AtomId::from_raw("entity-0003"),
            ],
            evidence: Vec::new(),
            section_position: super::super::atoms::SectionPosition::section("sec_0001"),
            causal_antecedents: Vec::new(),
            attributes: serde_json::Map::new(),
            enrichment_depth: EnrichmentDepth::Extracted,
        }];
        let (dropped, failures) = check_event_participants(&policy, &mut events, &entities);
        assert_eq!(events[0].participants.len(), 2, "the event survives");
        assert_eq!(dropped.len(), 1, "and names the edge to remove");
        assert_eq!(dropped[0].1.as_str(), "entity-0003");
        assert_eq!(failures[0].kind, PhaseFailureKind::EndpointTypeMismatch);
        assert!(failures[0].reason.contains("The Cataloguer"));
    }

    #[test]
    fn an_unresolvable_ref_keeps_the_name_and_is_recorded() {
        let p = policies(vec![
            OntologyTypeDecl {
                name: "coin".into(),
                kind: TypeKind::Entity,
                attributes: vec![AttrDecl {
                    name: "mint".into(),
                    family: AttrFamily::Ref { of: "mint".into() },
                    description: String::new(),
                }],
                ..Default::default()
            },
            entity_decl("mint", None),
        ]);
        let policy = ResolutionPolicy::new(&p);
        let mut coin = atom("entity-0001", "Series Y", EntityType::Other("coin".into()));
        coin.attributes.insert(
            "mint".into(),
            serde_json::Value::String("Nowhere-at-all".into()),
        );
        let mint = atom("entity-0002", "Eoforwic", EntityType::Other("mint".into()));
        let entities = vec![coin, mint];
        let name_index: HashMap<String, AtomId> = entities
            .iter()
            .map(|e| {
                (
                    super::super::resolution::fold(&e.canonical_name),
                    e.id.clone(),
                )
            })
            .collect();

        let (updates, failures) =
            snap_ref_attributes(&policy, &entities, &name_index, &HashMap::new());
        assert!(updates.is_empty(), "nothing snapped, so nothing to apply");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, PhaseFailureKind::UnresolvedAttributeRef);
        assert!(failures[0].reason.contains("Nowhere-at-all"));
    }
}
