//! Pattern detectors for the investigation pipeline.
//!
//! Three built-in detectors cover the common investigative
//! patterns the recipe author declares in
//! `[[enrichment.patterns]]`:
//!
//! - [`detect_circular_flow`] — directed cycles over typed edges
//!   (e.g. money flows in a A→B→C→A loop). Powered by petgraph's
//!   Tarjan SCC + simple cycle enumeration.
//! - [`detect_role_overlap`] — the same pair of entities connected
//!   by two distinct edge types representing different roles
//!   (e.g. A invests in B AND A is a customer of B).
//! - [`detect_threshold`] — edges of a given type whose numeric
//!   attribute meets a comparison (e.g. revenue concentration
//!   `> 10%`).
//!
//! Custom-SQL pattern is intentionally deferred to a follow-up:
//! it would force `rusqlite` into the default dependency set just
//! to support an escape hatch we haven't seen demand for yet.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use petgraph::algo::tarjan_scc;
use petgraph::graph::NodeIndex;
use petgraph::Graph;

use crate::recipe::{Comparison, PatternDecl};

use super::graph::{Entity, PatternFinding, PatternKind, Relationship};

/// Run every declared pattern detector against the graph.
/// Returns the flattened list of findings; ordering follows the
/// order of `patterns` in the recipe so the audit step renders
/// findings in the recipe-author-chosen sequence.
pub fn detect_all(
    patterns: &[PatternDecl],
    entities: &[Entity],
    relationships: &[Relationship],
) -> Vec<PatternFinding> {
    let mut out = Vec::new();
    for pattern in patterns {
        let findings = match pattern {
            PatternDecl::CircularFlow {
                name,
                description: _,
                min_entities,
                edge_types,
            } => detect_circular_flow(name, *min_entities, edge_types, entities, relationships),
            PatternDecl::RoleOverlap {
                name,
                description: _,
                entity_roles,
            } => detect_role_overlap(name, entity_roles, relationships),
            PatternDecl::Threshold {
                name,
                description: _,
                edge_type,
                attribute,
                threshold,
                comparison,
            } => detect_threshold(
                name,
                edge_type,
                attribute,
                *threshold,
                *comparison,
                entities,
                relationships,
            ),
            PatternDecl::CustomSql {
                name,
                description: _,
                query,
            } => {
                // Reserved — not yet implemented. The runtime
                // emits a placeholder finding so the recipe
                // author can see the pattern was declared but
                // skipped, with the actual SQL preserved for
                // when the executor lands.
                tracing::warn!(
                    pattern = %name,
                    "PatternDecl::CustomSql is reserved but not yet \
                     implemented; skipping. The future executor will run \
                     this query on a read-only SQLite materialisation of \
                     the relationship graph."
                );
                let mut attributes = serde_json::Map::new();
                attributes.insert(
                    "status".into(),
                    serde_json::Value::String("reserved_not_yet_implemented".into()),
                );
                attributes.insert("query".into(), serde_json::Value::String(query.clone()));
                vec![PatternFinding {
                    pattern_name: name.clone(),
                    pattern_type: PatternKind::CustomSql,
                    entity_ids: Vec::new(),
                    relationship_ids: Vec::new(),
                    attributes,
                }]
            }
        };
        out.extend(findings);
    }
    out
}

// ---------------------------------------------------------------------------
// Circular flow
// ---------------------------------------------------------------------------

/// Detect directed cycles whose edges all match `edge_types`.
///
/// Algorithm: build a directed multigraph (one node per entity,
/// one edge per relationship of an allowed type), find every
/// strongly-connected component via Tarjan, then enumerate simple
/// cycles within each SCC of size `>= min_entities`. The
/// enumeration is bounded by SCC size — investigation graphs in
/// practice have small SCCs, so unbounded enumeration is fine for
/// v1.
pub fn detect_circular_flow(
    name: &str,
    min_entities: u32,
    edge_types: &[String],
    entities: &[Entity],
    relationships: &[Relationship],
) -> Vec<PatternFinding> {
    let allowed: BTreeSet<&str> = edge_types.iter().map(String::as_str).collect();
    let entity_index: HashMap<&str, NodeIndex> = entities
        .iter()
        .map(|e| (e.id.as_str(), NodeIndex::new(0))) // placeholder
        .collect::<HashMap<_, _>>();

    // Build the petgraph DiGraph<&str entity_id, &str relationship_id>.
    let mut g: Graph<&str, &str> = Graph::new();
    let mut id_to_node: HashMap<&str, NodeIndex> = HashMap::with_capacity(entities.len());
    for e in entities {
        let n = g.add_node(e.id.as_str());
        id_to_node.insert(e.id.as_str(), n);
    }
    for r in relationships {
        if !allowed.contains(r.relationship_type.as_str()) {
            continue;
        }
        let (Some(&from), Some(&to)) = (
            id_to_node.get(r.from_entity_id.as_str()),
            id_to_node.get(r.to_entity_id.as_str()),
        ) else {
            continue;
        };
        g.add_edge(from, to, r.id.as_str());
    }
    let _ = entity_index; // placeholder map unused; kept the build above clearer.

    let mut findings = Vec::new();
    for scc in tarjan_scc(&g) {
        if scc.len() < min_entities as usize {
            continue;
        }
        let nodes_in_scc: BTreeSet<NodeIndex> = scc.iter().copied().collect();
        for cycle in enumerate_simple_cycles(&g, &nodes_in_scc) {
            if cycle.len() < min_entities as usize {
                continue;
            }
            let entity_ids: Vec<String> = cycle.iter().map(|n| g[*n].to_string()).collect();
            // Find the relationship_ids that connect this cycle in
            // order. Take ANY edge between consecutive nodes that
            // matches `allowed`; multigraph means there could be
            // several — pick the first one for the finding payload.
            let mut relationship_ids = Vec::with_capacity(cycle.len());
            for i in 0..cycle.len() {
                let from = cycle[i];
                let to = cycle[(i + 1) % cycle.len()];
                if let Some(edge) = g.edges_connecting(from, to).next() {
                    relationship_ids.push((*edge.weight()).to_string());
                }
            }
            let mut attributes = serde_json::Map::new();
            attributes.insert(
                "cycle_length".into(),
                serde_json::Value::from(cycle.len() as u64),
            );
            findings.push(PatternFinding {
                pattern_name: name.to_string(),
                pattern_type: PatternKind::CircularFlow,
                entity_ids,
                relationship_ids,
                attributes,
            });
        }
    }
    findings
}

/// Enumerate every simple cycle whose nodes are all inside `scc`.
/// petgraph 0.6 doesn't ship a public simple-cycle iterator, so
/// we DFS within the SCC starting from each node.
fn enumerate_simple_cycles(
    g: &Graph<&str, &str>,
    scc: &BTreeSet<NodeIndex>,
) -> Vec<Vec<NodeIndex>> {
    let mut cycles = Vec::new();
    let mut seen: BTreeSet<Vec<NodeIndex>> = BTreeSet::new();

    for &start in scc {
        let mut path: Vec<NodeIndex> = Vec::new();
        let mut on_path: BTreeSet<NodeIndex> = BTreeSet::new();
        path.push(start);
        on_path.insert(start);
        dfs_cycles(
            g,
            scc,
            start,
            start,
            &mut path,
            &mut on_path,
            &mut cycles,
            &mut seen,
        );
    }
    cycles
}

#[allow(clippy::too_many_arguments)]
fn dfs_cycles(
    g: &Graph<&str, &str>,
    scc: &BTreeSet<NodeIndex>,
    start: NodeIndex,
    current: NodeIndex,
    path: &mut Vec<NodeIndex>,
    on_path: &mut BTreeSet<NodeIndex>,
    cycles: &mut Vec<Vec<NodeIndex>>,
    seen: &mut BTreeSet<Vec<NodeIndex>>,
) {
    for neighbor in g.neighbors_directed(current, petgraph::Direction::Outgoing) {
        if !scc.contains(&neighbor) {
            continue;
        }
        if neighbor == start && path.len() >= 2 {
            // Found a cycle. Canonicalize by rotating to start with
            // the lowest NodeIndex so we dedupe rotations.
            let mut canonical = path.clone();
            rotate_to_min(&mut canonical);
            if seen.insert(canonical.clone()) {
                cycles.push(canonical);
            }
            continue;
        }
        // Skip nodes already on the current path (would form a
        // smaller sub-cycle, or a node we'd revisit). To enumerate
        // only simple cycles we require all path nodes distinct.
        if on_path.contains(&neighbor) {
            continue;
        }
        // Bound: don't extend through nodes lower than start —
        // that cycle was found by the earlier start-iteration.
        if neighbor.index() < start.index() {
            continue;
        }
        path.push(neighbor);
        on_path.insert(neighbor);
        dfs_cycles(g, scc, start, neighbor, path, on_path, cycles, seen);
        path.pop();
        on_path.remove(&neighbor);
    }
}

fn rotate_to_min(path: &mut [NodeIndex]) {
    if path.is_empty() {
        return;
    }
    let mut min_idx = 0;
    for (i, n) in path.iter().enumerate() {
        if n.index() < path[min_idx].index() {
            min_idx = i;
        }
    }
    path.rotate_left(min_idx);
}

// ---------------------------------------------------------------------------
// Role overlap
// ---------------------------------------------------------------------------

/// Same pair of entities connected by two edge types representing
/// distinct roles. `entity_roles` maps a free-form role name to a
/// typed-edge specifier `"<edge_type>.<from|to>"` that says which
/// side of the edge the entity sits on.
///
/// Example: `entity_roles = { investor = "investment.from",
/// customer = "revenue.to" }` matches every entity pair `(A, B)`
/// where A invests in B AND A is a customer of B's revenue
/// (i.e. an investment edge A→B AND a revenue edge B→A).
pub fn detect_role_overlap(
    name: &str,
    entity_roles: &BTreeMap<String, String>,
    relationships: &[Relationship],
) -> Vec<PatternFinding> {
    if entity_roles.len() < 2 {
        return Vec::new();
    }

    // Parse each role spec into (edge_type, side). Skip malformed
    // specs — the recipe author can fix them after seeing the
    // empty findings.
    let parsed: Vec<(String, String, Side)> = entity_roles
        .iter()
        .filter_map(|(role, spec)| {
            let (edge, side) = spec.rsplit_once('.')?;
            let side = match side {
                "from" => Side::From,
                "to" => Side::To,
                _ => return None,
            };
            Some((role.clone(), edge.to_string(), side))
        })
        .collect();
    if parsed.len() != entity_roles.len() {
        // At least one malformed role spec — bail out to be safe.
        return Vec::new();
    }

    // For each role: build the set of subject entity ids per edge.
    // We index by (subject_entity_id, role) so a pair (A, B) where
    // A satisfies all roles becomes a finding.
    let mut role_subjects: HashMap<&str, BTreeMap<String, Vec<&str>>> = HashMap::new();
    // Outer key: subject entity id; inner key: role name; inner value:
    // list of "other" entity ids for that role.

    for (role, edge_type, side) in &parsed {
        for r in relationships {
            if r.relationship_type != *edge_type {
                continue;
            }
            let (subject, other) = match side {
                Side::From => (r.from_entity_id.as_str(), r.to_entity_id.as_str()),
                Side::To => (r.to_entity_id.as_str(), r.from_entity_id.as_str()),
            };
            role_subjects
                .entry(subject)
                .or_default()
                .entry(role.clone())
                .or_default()
                .push(other);
        }
    }

    let role_names: Vec<&str> = parsed.iter().map(|(r, _, _)| r.as_str()).collect();
    let mut findings = Vec::new();

    for (subject, by_role) in &role_subjects {
        // Subject must satisfy EVERY declared role.
        if !role_names.iter().all(|r| by_role.contains_key(*r)) {
            continue;
        }
        // For each combination of "other" entities — typically a
        // single common counterparty. Find the intersection of the
        // role lists: an entity that appears in EVERY role.
        let mut iter = role_names.iter();
        let first_role = iter.next().expect("non-empty role list");
        let mut intersection: BTreeSet<&str> =
            by_role.get(*first_role).unwrap().iter().copied().collect();
        for role in iter {
            let next: BTreeSet<&str> = by_role.get(*role).unwrap().iter().copied().collect();
            intersection = intersection.intersection(&next).copied().collect();
        }
        for counterparty in intersection {
            let mut attributes = serde_json::Map::new();
            attributes.insert(
                "subject_id".into(),
                serde_json::Value::String((*subject).to_string()),
            );
            attributes.insert(
                "counterparty_id".into(),
                serde_json::Value::String(counterparty.to_string()),
            );
            attributes.insert(
                "roles".into(),
                serde_json::Value::Array(
                    role_names
                        .iter()
                        .map(|r| serde_json::Value::String((*r).into()))
                        .collect(),
                ),
            );
            findings.push(PatternFinding {
                pattern_name: name.to_string(),
                pattern_type: PatternKind::RoleOverlap,
                entity_ids: vec![(*subject).to_string(), counterparty.to_string()],
                relationship_ids: Vec::new(),
                attributes,
            });
        }
    }
    findings
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    From,
    To,
}

// ---------------------------------------------------------------------------
// Threshold
// ---------------------------------------------------------------------------

/// Edges of `edge_type` whose `attribute` meets `comparison`
/// against `threshold`. Numeric coercion handles JSON integers,
/// floats, and numeric strings transparently. Edges whose
/// attribute is missing or non-numeric are skipped silently.
pub fn detect_threshold(
    name: &str,
    edge_type: &str,
    attribute: &str,
    threshold: f64,
    comparison: Comparison,
    entities: &[Entity],
    relationships: &[Relationship],
) -> Vec<PatternFinding> {
    let mut findings = Vec::new();
    // Edge scan (unchanged): numeric attribute on an edge of `edge_type`
    // (e.g. revenue concentration on a `revenue` edge).
    for r in relationships {
        if r.relationship_type != edge_type {
            continue;
        }
        let raw = match r.attributes.get(attribute) {
            Some(v) => v,
            None => continue,
        };
        let n = match coerce_number(raw) {
            Some(n) => n,
            None => continue,
        };
        if !comparison_matches(n, threshold, comparison) {
            continue;
        }
        findings.push(PatternFinding {
            pattern_name: name.to_string(),
            pattern_type: PatternKind::Threshold,
            entity_ids: vec![r.from_entity_id.clone(), r.to_entity_id.clone()],
            relationship_ids: vec![r.id.clone()],
            attributes: threshold_attrs(n, attribute, threshold),
        });
    }
    // Entity scan (additive): numeric attribute stamped on an ENTITY —
    // e.g. `sighting_count` written by `aggregate::stamp_edge_counts` for a
    // count-based hotspot threshold. Fires when the attribute lives on the
    // entity rather than an edge; never affects edge-attribute thresholds
    // (their attribute isn't on these entities).
    for e in entities {
        let raw = match e.attributes.get(attribute) {
            Some(v) => v,
            None => continue,
        };
        let n = match coerce_number(raw) {
            Some(n) => n,
            None => continue,
        };
        if !comparison_matches(n, threshold, comparison) {
            continue;
        }
        findings.push(PatternFinding {
            pattern_name: name.to_string(),
            pattern_type: PatternKind::Threshold,
            entity_ids: vec![e.id.clone()],
            relationship_ids: Vec::new(),
            attributes: threshold_attrs(n, attribute, threshold),
        });
    }
    findings
}

/// Build the standard finding-attribute bag for a threshold match.
fn threshold_attrs(value: f64, attribute: &str, threshold: f64) -> serde_json::Map<String, serde_json::Value> {
    let mut attributes = serde_json::Map::new();
    attributes.insert(
        "value".into(),
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
    );
    attributes.insert(
        "attribute".into(),
        serde_json::Value::String(attribute.into()),
    );
    attributes.insert(
        "threshold".into(),
        serde_json::Number::from_f64(threshold)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
    );
    attributes
}

fn coerce_number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn comparison_matches(value: f64, threshold: f64, op: Comparison) -> bool {
    match op {
        Comparison::GreaterThan => value > threshold,
        Comparison::GreaterOrEqual => value >= threshold,
        Comparison::LessThan => value < threshold,
        Comparison::LessOrEqual => value <= threshold,
        Comparison::Equal => (value - threshold).abs() < f64::EPSILON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::investigation::graph::Evidence;

    fn ent(id: &str, ty: &str, name: &str) -> Entity {
        Entity {
            id: id.into(),
            canonical_name: name.into(),
            entity_type: ty.into(),
            attributes: Default::default(),
            aliases: Vec::new(),
        }
    }

    fn rel(
        id: &str,
        from: &str,
        to: &str,
        ty: &str,
        attrs: serde_json::Map<String, serde_json::Value>,
    ) -> Relationship {
        Relationship {
            id: id.into(),
            from_entity_id: from.into(),
            to_entity_id: to.into(),
            relationship_type: ty.into(),
            attributes: attrs,
            evidence: Evidence {
                chunk_id: "chunk-1".into(),
                excerpt: "evidence".into(),
            },
            confidence: 1.0,
        }
    }

    #[test]
    fn circular_flow_finds_three_node_cycle() {
        let entities = vec![
            ent("A", "company", "A"),
            ent("B", "company", "B"),
            ent("C", "company", "C"),
        ];
        let relationships = vec![
            rel("r1", "A", "B", "revenue", Default::default()),
            rel("r2", "B", "C", "revenue", Default::default()),
            rel("r3", "C", "A", "revenue", Default::default()),
        ];
        let findings = detect_circular_flow(
            "money_cycles",
            3,
            &["revenue".to_string()],
            &entities,
            &relationships,
        );
        assert_eq!(findings.len(), 1, "expected 1 cycle, got {findings:?}");
        let f = &findings[0];
        assert_eq!(f.entity_ids.len(), 3);
        assert_eq!(f.relationship_ids.len(), 3);
        assert_eq!(f.pattern_type, PatternKind::CircularFlow);
    }

    #[test]
    fn circular_flow_respects_min_entities() {
        let entities = vec![ent("A", "company", "A"), ent("B", "company", "B")];
        let relationships = vec![
            rel("r1", "A", "B", "revenue", Default::default()),
            rel("r2", "B", "A", "revenue", Default::default()),
        ];
        // 2-node cycle exists but min_entities = 3 should suppress it.
        let findings = detect_circular_flow("x", 3, &["revenue".into()], &entities, &relationships);
        assert!(findings.is_empty());

        // Lower the bar — now finds the 2-cycle.
        let findings = detect_circular_flow("x", 2, &["revenue".into()], &entities, &relationships);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn circular_flow_filters_by_edge_type() {
        let entities = vec![
            ent("A", "company", "A"),
            ent("B", "company", "B"),
            ent("C", "company", "C"),
        ];
        let relationships = vec![
            rel("r1", "A", "B", "revenue", Default::default()),
            rel("r2", "B", "C", "investment", Default::default()),
            rel("r3", "C", "A", "revenue", Default::default()),
        ];
        // Only "revenue" allowed — the cycle has an "investment"
        // edge, so it should be excluded.
        let findings = detect_circular_flow("x", 3, &["revenue".into()], &entities, &relationships);
        assert!(findings.is_empty());

        // Allow both → finds the cycle.
        let findings = detect_circular_flow(
            "x",
            3,
            &["revenue".into(), "investment".into()],
            &entities,
            &relationships,
        );
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn role_overlap_finds_invest_in_customer() {
        let mut investment_attrs = serde_json::Map::new();
        investment_attrs.insert("amount_usd".into(), 100_000_000.into());
        let mut revenue_attrs = serde_json::Map::new();
        revenue_attrs.insert("amount_usd".into(), 50_000_000.into());

        let relationships = vec![
            rel(
                "inv-1",
                "Microsoft",
                "OpenAI",
                "investment",
                investment_attrs,
            ),
            rel("rev-1", "OpenAI", "Microsoft", "revenue", revenue_attrs),
            // Distractor: a third party with revenue from MSFT
            rel("rev-2", "Acme", "Microsoft", "revenue", Default::default()),
        ];
        let mut roles = BTreeMap::new();
        roles.insert("investor".into(), "investment.from".into());
        roles.insert("customer".into(), "revenue.to".into());

        let findings = detect_role_overlap("invest_in_customer", &roles, &relationships);
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        let f = &findings[0];
        assert_eq!(f.pattern_type, PatternKind::RoleOverlap);
        assert!(f.entity_ids.contains(&"Microsoft".to_string()));
        assert!(f.entity_ids.contains(&"OpenAI".to_string()));
    }

    #[test]
    fn role_overlap_returns_empty_when_no_pair_matches_all_roles() {
        let relationships = vec![
            rel(
                "inv-1",
                "Microsoft",
                "OpenAI",
                "investment",
                Default::default(),
            ),
            // No revenue edge OpenAI -> Microsoft
        ];
        let mut roles = BTreeMap::new();
        roles.insert("investor".into(), "investment.from".into());
        roles.insert("customer".into(), "revenue.to".into());
        let findings = detect_role_overlap("x", &roles, &relationships);
        assert!(findings.is_empty());
    }

    #[test]
    fn threshold_finds_matching_edges() {
        let mut high = serde_json::Map::new();
        high.insert("percentage_of_total".into(), 0.15.into());
        let mut low = serde_json::Map::new();
        low.insert("percentage_of_total".into(), 0.05.into());
        let relationships = vec![
            rel("rev-1", "A", "B", "revenue", high),
            rel("rev-2", "C", "D", "revenue", low),
        ];
        let findings = detect_threshold(
            "concentration",
            "revenue",
            "percentage_of_total",
            0.10,
            Comparison::GreaterThan,
            &[],
            &relationships,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].relationship_ids, vec!["rev-1"]);
        let value = findings[0]
            .attributes
            .get("value")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((value - 0.15).abs() < 1e-9);
    }

    #[test]
    fn threshold_skips_missing_or_non_numeric() {
        let relationships = vec![
            // Attribute missing
            rel("rev-1", "A", "B", "revenue", Default::default()),
            // Attribute non-numeric
            {
                let mut attrs = serde_json::Map::new();
                attrs.insert(
                    "percentage_of_total".into(),
                    serde_json::Value::String("not a number".into()),
                );
                rel("rev-2", "C", "D", "revenue", attrs)
            },
        ];
        let findings = detect_threshold(
            "x",
            "revenue",
            "percentage_of_total",
            0.0,
            Comparison::GreaterThan,
            &[],
            &relationships,
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn threshold_fires_on_entity_attribute() {
        // A count-based hotspot: the numeric attribute lives on the
        // ENTITY (stamped by aggregate::stamp_edge_counts), not an edge.
        let mut attrs = serde_json::Map::new();
        attrs.insert("sighting_count".into(), serde_json::json!(4));
        let installation = Entity {
            id: "e-installation-wpafb".into(),
            canonical_name: "Wright-Patterson AFB".into(),
            entity_type: "installation".into(),
            attributes: attrs,
            aliases: Vec::new(),
        };
        let findings = detect_threshold(
            "sighting_hotspots",
            "occurred_near",
            "sighting_count",
            3.0,
            Comparison::GreaterThan,
            std::slice::from_ref(&installation),
            &[],
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].entity_ids, vec!["e-installation-wpafb"]);
        assert!(findings[0].relationship_ids.is_empty());
    }

    #[test]
    fn detect_all_runs_each_declared_pattern() {
        // Minimal graph that triggers all three pattern shapes.
        // Edges include both A→B→C→A revenue (cycle), an
        // investment edge A→B (paired with a B→A revenue edge for
        // role-overlap), and a high-percentage revenue edge
        // (threshold).
        let entities = vec![
            ent("A", "company", "A"),
            ent("B", "company", "B"),
            ent("C", "company", "C"),
        ];
        let mut high = serde_json::Map::new();
        high.insert("percentage_of_total".into(), 0.2.into());
        let relationships = vec![
            rel("r1", "A", "B", "revenue", high.clone()),
            rel("r2", "B", "C", "revenue", Default::default()),
            rel("r3", "C", "A", "revenue", Default::default()),
            rel("inv-1", "A", "B", "investment", Default::default()),
            // B→A revenue: makes the role-overlap pair (A, B) match
            // — A is investor (investment.from = A) AND A is
            // customer (revenue.to = A on the B→A edge).
            rel("rev-ba", "B", "A", "revenue", Default::default()),
        ];
        let mut roles = BTreeMap::new();
        roles.insert("investor".into(), "investment.from".into());
        roles.insert("customer".into(), "revenue.to".into());

        let patterns = vec![
            PatternDecl::CircularFlow {
                name: "cycles".into(),
                description: String::new(),
                min_entities: 3,
                edge_types: vec!["revenue".into()],
            },
            PatternDecl::RoleOverlap {
                name: "ovlap".into(),
                description: String::new(),
                entity_roles: roles,
            },
            PatternDecl::Threshold {
                name: "thresh".into(),
                description: String::new(),
                edge_type: "revenue".into(),
                attribute: "percentage_of_total".into(),
                threshold: 0.1,
                comparison: Comparison::GreaterThan,
            },
        ];
        let findings = detect_all(&patterns, &entities, &relationships);
        // Every declared pattern produced at least one finding, in
        // the recipe-author-chosen order.
        let names: Vec<&str> = findings.iter().map(|f| f.pattern_name.as_str()).collect();
        assert!(names.contains(&"cycles"), "missing cycles: {findings:?}");
        assert!(
            names.contains(&"ovlap"),
            "missing role overlap: {findings:?}"
        );
        assert!(names.contains(&"thresh"), "missing threshold: {findings:?}");
        // Order is preserved per pattern family.
        let cycles_idx = names.iter().position(|&n| n == "cycles").unwrap();
        let ovlap_idx = names.iter().position(|&n| n == "ovlap").unwrap();
        let thresh_idx = names.iter().position(|&n| n == "thresh").unwrap();
        assert!(cycles_idx < ovlap_idx);
        assert!(ovlap_idx < thresh_idx);
    }
}
