//! Deterministic second-order re-fold of a finished investigation graph.
//!
//! The Phase-1 LLM extraction is the expensive part (~hours of 35B) and its
//! output is fixed once written. Coalescing, by contrast, is a deterministic
//! fold over surface forms — and improvable offline. When the fold rules grow
//! (a new state-suffix stripper, an adjudication-by-category rule), we want to
//! tighten the graph WITHOUT re-running inference.
//!
//! [`recoalesce_graph`] takes the persisted `entities.json` + `relationships.json`
//! and:
//!   1. re-derives each entity's canonical id under the *current* fold rules
//!      (so straggler OCR/location variants that escaped the first pass now
//!      collapse) — plus a type-specific rule for `adjudication`, whose real
//!      identity is its `category` attribute, not its (often date- or
//!      synthetic-id-shaped) name;
//!   2. merges colliding entities (longest clean canonical, unioned aliases,
//!      first-non-null attributes);
//!   3. rewrites relationship endpoints through the id remap, drops merge-
//!      artifact self-loops, and dedupes identical edges;
//!   4. re-runs the deterministic count-aggregation + pattern detection so
//!      hotspot counts and findings reflect the merged graph.
//!
//! It is idempotent: running it twice yields the same graph (a re-fold of an
//! already-folded graph is a no-op).

use std::collections::BTreeMap;

use serde_json::Value;

use super::aggregate;
use super::graph::{Entity, PatternFinding, Relationship};
use super::normalize::Normalizer;
use super::patterns;
use crate::recipe::PatternDecl;

/// Outcome of a re-fold, with before/after counts for glassbox reporting.
#[derive(Debug, Clone)]
pub struct RecoalesceResult {
    pub entities: Vec<Entity>,
    pub relationships: Vec<Relationship>,
    pub findings: Vec<PatternFinding>,
    pub entities_before: usize,
    pub entities_after: usize,
    pub relationships_before: usize,
    pub relationships_after: usize,
}

/// The id an entity folds to under the current rules. A type with a declared
/// identity attribute keys on that attribute's value (e.g. a disposition
/// `category`, collapsing date-/synthetic-id-named nodes that share it);
/// everything else re-derives from its canonical name via the shared fold so
/// it agrees with relationship endpoints built by [`Normalizer::entity_id`].
fn merged_id(normalizer: &Normalizer, e: &Entity) -> String {
    if let Some(attr) = normalizer.identity_attribute(&e.entity_type) {
        if let Some(v) = e.attributes.get(attr).and_then(Value::as_str) {
            if !v.trim().is_empty() {
                return normalizer.entity_id(&e.entity_type, v);
            }
        }
    }
    normalizer.entity_id(&e.entity_type, &e.canonical_name)
}

/// Re-fold a finished graph. See module docs.
pub fn recoalesce_graph(
    normalizer: &Normalizer,
    entities: Vec<Entity>,
    relationships: Vec<Relationship>,
    patterns_decl: &[PatternDecl],
) -> RecoalesceResult {
    let entities_before = entities.len();
    let relationships_before = relationships.len();

    // ── 1+2. Group entities by their folded id; accumulate every surface
    // form as an alias. Canonical selection is deferred to a finalize pass so
    // it can choose across ALL merged surface forms (incl. the seed's own
    // prior aliases), not just incrementally.
    let mut remap: BTreeMap<String, String> = BTreeMap::new(); // old id → new id
    let mut merged: BTreeMap<String, Entity> = BTreeMap::new();
    for e in entities {
        let new_id = merged_id(normalizer, &e);
        remap.insert(e.id.clone(), new_id.clone());
        match merged.get_mut(&new_id) {
            None => {
                let mut seed = e;
                seed.id = new_id.clone();
                merged.insert(new_id, seed);
            }
            Some(acc) => {
                if e.canonical_name != acc.canonical_name
                    && !acc.aliases.contains(&e.canonical_name)
                {
                    acc.aliases.push(e.canonical_name.clone());
                }
                for a in e.aliases {
                    if a != acc.canonical_name && !acc.aliases.contains(&a) {
                        acc.aliases.push(a);
                    }
                }
                // First-non-null attribute wins (never clobber a present value).
                for (k, v) in e.attributes {
                    if v.is_null() {
                        continue;
                    }
                    acc.attributes.entry(k).or_insert(v);
                }
            }
        }
    }

    // Finalize canonical per merged entity. A type with an identity attribute
    // takes that attribute's value as canonical (e.g. the disposition
    // category); everything else picks the cleanest surface form across
    // canonical ∪ aliases. The previous canonical is demoted to an alias.
    for e in merged.values_mut() {
        let chosen = if let Some(attr) = normalizer.identity_attribute(&e.entity_type) {
            e.attributes
                .get(attr)
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            let candidates: Vec<&str> = std::iter::once(e.canonical_name.as_str())
                .chain(e.aliases.iter().map(String::as_str))
                .collect();
            normalizer.best_canonical(&e.entity_type, candidates)
        };
        if let Some(best) = chosen {
            if best != e.canonical_name {
                e.aliases.retain(|a| a != &best);
                if !e.aliases.contains(&e.canonical_name) {
                    e.aliases.push(e.canonical_name.clone());
                }
                e.canonical_name = best;
            }
        }
    }

    let mut entities: Vec<Entity> = merged.into_values().collect();
    entities.sort_by(|a, b| a.id.cmp(&b.id));

    // ── 3. Rewrite + dedupe relationships. ──
    let mut seen: std::collections::HashSet<(String, String, String, String)> =
        std::collections::HashSet::new();
    let mut rewritten: Vec<Relationship> = Vec::with_capacity(relationships.len());
    for mut r in relationships {
        if let Some(nid) = remap.get(&r.from_entity_id) {
            r.from_entity_id = nid.clone();
        }
        if let Some(nid) = remap.get(&r.to_entity_id) {
            r.to_entity_id = nid.clone();
        }
        // A self-loop can only arise from merging two endpoints into one node;
        // it carries no information for these patterns. Drop it.
        if r.from_entity_id == r.to_entity_id {
            continue;
        }
        let dedupe_key = (
            r.from_entity_id.clone(),
            r.to_entity_id.clone(),
            r.relationship_type.clone(),
            r.evidence.excerpt.clone(),
        );
        if seen.insert(dedupe_key) {
            rewritten.push(r);
        }
    }
    // Re-id contiguously so ids stay stable + dense after dedupe.
    for (i, r) in rewritten.iter_mut().enumerate() {
        r.id = format!("r-{i}");
    }
    let relationships = rewritten;

    // ── 4. Re-aggregate (Phase 2.5) + re-detect (Phase 3) on the merged graph.
    // Mirror run_investigation: a count-based Threshold (its attribute is NOT
    // present on any edge of edge_type) is a per-entity edge count. Clear the
    // stale stamped value first so a shrunk neighbourhood can't keep an old
    // (higher) count, then re-stamp from the merged edges.
    for pattern in patterns_decl {
        if let PatternDecl::Threshold {
            edge_type,
            attribute,
            ..
        } = pattern
        {
            let attr_on_edges = relationships
                .iter()
                .any(|r| r.relationship_type == *edge_type && r.attributes.contains_key(attribute));
            if !attr_on_edges {
                for e in entities.iter_mut() {
                    e.attributes.remove(attribute);
                }
                aggregate::stamp_edge_counts(&mut entities, &relationships, edge_type, attribute);
            }
        }
    }
    let findings = patterns::detect_all(patterns_decl, &entities, &relationships);

    RecoalesceResult {
        entities_after: entities.len(),
        relationships_after: relationships.len(),
        entities_before,
        relationships_before,
        entities,
        relationships,
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::investigation::graph::Evidence;

    fn inst(id: &str, name: &str, sighting_count: Option<u64>) -> Entity {
        let mut attributes = serde_json::Map::new();
        if let Some(c) = sighting_count {
            attributes.insert("sighting_count".into(), serde_json::json!(c));
        }
        Entity {
            id: id.into(),
            canonical_name: name.into(),
            entity_type: "installation".into(),
            attributes,
            aliases: vec![],
        }
    }

    fn sighting(id: &str) -> Entity {
        Entity {
            id: id.into(),
            canonical_name: id.into(),
            entity_type: "sighting".into(),
            attributes: Default::default(),
            aliases: vec![],
        }
    }

    fn near(id: &str, from: &str, to: &str) -> Relationship {
        Relationship {
            id: id.into(),
            from_entity_id: from.into(),
            to_entity_id: to.into(),
            relationship_type: "occurred_near".into(),
            attributes: Default::default(),
            evidence: Evidence {
                chunk_id: "c".into(),
                excerpt: format!("{from}->{to}"),
            },
            confidence: 1.0,
        }
    }

    fn threshold() -> Vec<PatternDecl> {
        vec![PatternDecl::Threshold {
            name: "sighting_hotspots".into(),
            description: "installations with many nearby sightings".into(),
            edge_type: "occurred_near".into(),
            attribute: "sighting_count".into(),
            threshold: 2.0,
            comparison: crate::recipe::Comparison::GreaterThan,
        }]
    }

    /// A Normalizer mirroring the UAP facility + adjudication rules, so the
    /// re-fold tests drive the same transforms the recipe configures.
    fn norm() -> Normalizer {
        use crate::recipe::{FoldRule, NormalizationConfig};
        Normalizer::new(NormalizationConfig {
            identity_attribute: [("adjudication".to_string(), "category".to_string())]
                .into_iter()
                .collect(),
            fold: vec![FoldRule {
                types: vec!["installation".into()],
                aliases: vec![],
                leading_prefixes: vec![],
                trailing_qualifiers: ["ohio", "texas", "new mexico"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                trailing_suffixes: ["air", "force", "base", "afb"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            }],
        })
    }

    #[test]
    fn merges_state_suffix_straggler_and_recounts_hotspot() {
        // Two surface forms of the same base that the first pass left apart:
        // "Wright-Patterson Air Force Base" (folds to "wright patterson") and
        // "Wright-Patterson AFB, Ohio" (now also folds, via state strip).
        let entities = vec![
            inst("e-installation-wright-patterson", "Wright-Patterson Air Force Base", Some(2)),
            inst("e-installation-wright-patterson-afb-ohio", "Wright-Patterson AFB, Ohio", Some(2)),
            sighting("e-sighting-s1"),
            sighting("e-sighting-s2"),
            sighting("e-sighting-s3"),
        ];
        let rels = vec![
            near("r-0", "e-sighting-s1", "e-installation-wright-patterson"),
            near("r-1", "e-sighting-s2", "e-installation-wright-patterson"),
            near("r-2", "e-sighting-s3", "e-installation-wright-patterson-afb-ohio"),
        ];

        let out = recoalesce_graph(&norm(), entities, rels, &threshold());

        // The two WP nodes merged into one (plus the 3 sightings = 4 total).
        let installs: Vec<&Entity> =
            out.entities.iter().filter(|e| e.entity_type == "installation").collect();
        assert_eq!(installs.len(), 1, "WP variants must merge to one node");
        let wp = installs[0];
        assert_eq!(wp.id, "e-installation-wright-patterson");
        // Re-counted across the merged neighbourhood: 3 distinct sightings.
        assert_eq!(wp.attributes.get("sighting_count").unwrap(), &serde_json::json!(3));
        // The other surface form is preserved as an alias.
        assert!(wp.aliases.iter().any(|a| a.contains("Ohio")));
        // One hotspot finding now fires at the merged count.
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.entities_before, 5);
        assert_eq!(out.entities_after, 4);
    }

    #[test]
    fn collapses_adjudication_on_category() {
        let mk = |id: &str, name: &str, cat: &str| {
            let mut attributes = serde_json::Map::new();
            attributes.insert("category".into(), serde_json::json!(cat));
            Entity {
                id: id.into(),
                canonical_name: name.into(),
                entity_type: "adjudication".into(),
                attributes,
                aliases: vec![],
            }
        };
        let entities = vec![
            mk("e-adjudication-1-may-1952", "1 MAY 1952", "Possibly Balloon"),
            mk("e-adjudication-adjudication-1313", "Adjudication_1313", "Possibly Balloon"),
            mk("e-adjudication-other", "7 JUNE 1955", "Aircraft"),
        ];
        let out = recoalesce_graph(&norm(), entities, vec![], &[]);
        let adj: Vec<&Entity> =
            out.entities.iter().filter(|e| e.entity_type == "adjudication").collect();
        // Two distinct categories → two nodes (the two "Possibly Balloon" merged).
        assert_eq!(adj.len(), 2);
        let balloon = adj.iter().find(|e| e.id.contains("balloon")).unwrap();
        // Canonical promoted to the category, the dates kept as aliases.
        assert_eq!(balloon.canonical_name, "Possibly Balloon");
        assert!(balloon.aliases.iter().any(|a| a.contains("1952")));
    }

    #[test]
    fn is_idempotent() {
        let entities = vec![
            inst("e-installation-wright-patterson", "Wright-Patterson Air Force Base", Some(1)),
            inst("e-installation-wright-patterson-afb-ohio", "Wright-Patterson AFB, Ohio", Some(1)),
            sighting("e-sighting-s1"),
        ];
        let rels = vec![near("r-0", "e-sighting-s1", "e-installation-wright-patterson-afb-ohio")];
        let first = recoalesce_graph(&norm(), entities, rels, &threshold());
        let n1 = first.entities.len();
        let r1 = first.relationships.len();
        let second = recoalesce_graph(&norm(), first.entities, first.relationships, &threshold());
        assert_eq!(second.entities.len(), n1, "re-fold must be a no-op the 2nd time");
        assert_eq!(second.relationships.len(), r1);
        assert_eq!(second.entities_before, second.entities_after);
    }

    #[test]
    fn drops_self_loops_from_merge() {
        // Two nodes that merge, with an edge between them → becomes a self-loop
        // and must be dropped.
        let entities = vec![
            inst("e-installation-wright-patterson", "Wright-Patterson Air Force Base", None),
            inst("e-installation-wright-patterson-afb-ohio", "Wright-Patterson AFB, Ohio", None),
        ];
        let rels = vec![near(
            "r-0",
            "e-installation-wright-patterson",
            "e-installation-wright-patterson-afb-ohio",
        )];
        let out = recoalesce_graph(&norm(), entities, rels, &[]);
        assert_eq!(out.entities.len(), 1);
        assert_eq!(out.relationships.len(), 0, "merge-artifact self-loop dropped");
    }
}
