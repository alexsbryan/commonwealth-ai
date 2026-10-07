// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a model is shown to write a [`super::typed::TypedQuery`]: the declared
//! ontology rendered as documentation, and a JSON Schema whose enums hold only
//! what the ontology declares.
//!
//! A port of the feature-fidelity query-layer probe's `documentation()` and
//! `schema()` (`research/ontology-retrieval/ontology-proof/ans/k2_query.py`),
//! which parsed 37/40 non-co-occurrence questions on ft-ans-dev-b at T 0 with
//! thinking off. Same bytes for the same declaration — `tests/typed.rs` holds
//! that against the probe's own output — with one departure, named: the probe
//! glossed attributes from a hand-written table; here the gloss is the
//! attribute's declared `description`, so a recipe carries its own.
//!
//! One `anyOf` branch per declared entity type. A branch's `filters` name only
//! that type's own non-ref attributes (or `name`); its `relations` name only
//! the declared relations and `<type>.<attr>` refs that reach it, each usable
//! from either end; a relation's `where` reaches one more hop and stops — the
//! depth [`super::typed::Where`] holds by type. Property order is generation
//! order under the sampler's grammar, so the crate builds serde_json with
//! `preserve_order`.

use serde_json::{json, Map, Value};

use crate::enrichment::ontology::OntologyPolicies;
use understanding_vocab::ontology::decl::{AttrDecl, AttrFamily, OntologyTypeDecl, TypeKind};

/// The system line the probe sent with the documentation.
pub const QUERY_SYSTEM: &str = "You translate a question about a knowledge base into one typed \
query over its schema. Use only the types, attributes and relations the schema declares. Output \
only the JSON query.";

/// The grammar and the four out-of-domain examples, verbatim from the probe.
const GRAMMAR: &str = include_str!("typed_query_grammar.md");

const OPS: [&str; 4] = ["eq", "lt", "gt", "contains"];
const AGGREGATES: [&str; 4] = ["none", "count", "argmax", "argmin"];
/// The probe's per-array cap, on filters and on relations alike.
const MAX_ITEMS: usize = 4;

/// The prompt body and the response schema for one declared ontology.
#[derive(Debug, Clone)]
pub struct QueryGrammar {
    /// The ontology as documentation, then the grammar and examples. The
    /// caller appends `\n\nQuestion: <q>`.
    pub documentation: String,
    /// `{"anyOf": [one branch per entity type]}`.
    pub schema: Value,
}

/// One way to reach another entity type: `(relation, other_type, gloss)`.
type Edge = (String, String, String);

struct Declared<'a> {
    /// Entity types, declaration order.
    entities: Vec<&'a OntologyTypeDecl>,
    /// Edges per entity type, in the order the declaration yields them.
    edges: Vec<(String, Vec<Edge>)>,
}

impl<'a> Declared<'a> {
    fn new(policies: &'a OntologyPolicies) -> Self {
        let types = &policies.shape.types;
        let entities: Vec<&OntologyTypeDecl> = types
            .iter()
            .filter(|t| t.kind == TypeKind::Entity)
            .collect();
        let is_entity = |n: &str| entities.iter().any(|e| e.name == n);
        let mut edges: Vec<(String, Vec<Edge>)> = Vec::new();
        let mut push = |at: &str, edge: Edge| match edges.iter_mut().find(|(t, _)| t == at) {
            Some((_, list)) => list.push(edge),
            None => edges.push((at.to_string(), vec![edge])),
        };
        for t in types {
            if t.kind == TypeKind::Relation {
                if let (Some(from), Some(to)) = (t.from.as_deref(), t.to.as_deref()) {
                    if is_entity(from) && is_entity(to) {
                        let r = &t.name;
                        push(
                            from,
                            (r.clone(), to.into(), format!("this {from} {r} that {to}")),
                        );
                        push(
                            to,
                            (r.clone(), from.into(), format!("that {from} {r} this {to}")),
                        );
                    }
                }
            }
            for a in &t.attributes {
                if let AttrFamily::Ref { of } = &a.family {
                    if is_entity(&t.name) && is_entity(of) {
                        let (owner, attr) = (&t.name, &a.name);
                        let r = format!("{owner}.{attr}");
                        push(
                            owner,
                            (
                                r.clone(),
                                of.clone(),
                                format!("this {owner}'s {attr} is that {of}"),
                            ),
                        );
                        push(
                            of,
                            (
                                r,
                                owner.clone(),
                                format!("that {owner}'s {attr} is this {of}"),
                            ),
                        );
                    }
                }
            }
        }
        Self { entities, edges }
    }

    fn edges(&self, t: &str) -> &[Edge] {
        self.edges
            .iter()
            .find(|(n, _)| n == t)
            .map_or(&[], |(_, e)| e.as_slice())
    }

    fn own_attrs(&self, t: &str) -> Vec<&'a AttrDecl> {
        self.entities
            .iter()
            .find(|e| e.name == t)
            .map(|e| {
                e.attributes
                    .iter()
                    .filter(|a| !matches!(a.family, AttrFamily::Ref { .. }))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn filters(&self, t: &str) -> Value {
        let mut attribute = vec![Value::from("name")];
        attribute.extend(
            self.own_attrs(t)
                .iter()
                .map(|a| Value::from(a.name.as_str())),
        );
        json!({"type": "array", "maxItems": MAX_ITEMS, "items": {
            "type": "object", "additionalProperties": false,
            "required": ["attribute", "op", "value", "negate"],
            "properties": {
                "attribute": {"enum": attribute},
                "op": {"enum": OPS},
                "value": {"type": ["string", "number"]},
                "negate": {"type": "boolean"}}}})
    }

    fn relations(&self, t: &str, depth: u8) -> Value {
        let items: Vec<Value> = self
            .edges(t)
            .iter()
            .map(|(r, o, _)| {
                let mut props = Map::new();
                props.insert("relation".into(), json!({"const": r}));
                props.insert("other_type".into(), json!({"const": o}));
                props.insert("other_name".into(), json!({"type": ["string", "null"]}));
                props.insert("negate".into(), json!({"type": "boolean"}));
                if depth == 1 {
                    // The far end's own conditions; depth 2 ends here.
                    props.insert(
                        "where".into(),
                        json!({"anyOf": [{"type": "null"}, {
                            "type": "object", "additionalProperties": false,
                            "required": ["filters", "relations"],
                            "properties": {"filters": self.filters(o), "relations": self.relations(o, 2)}}]}),
                    );
                }
                object(props)
            })
            .collect();
        if items.is_empty() {
            json!({"type": "array", "maxItems": 0})
        } else {
            json!({"type": "array", "maxItems": MAX_ITEMS, "items": {"anyOf": items}})
        }
    }

    fn schema(&self) -> Value {
        let branches: Vec<Value> = self
            .entities
            .iter()
            .map(|e| {
                let t = e.name.as_str();
                let mut over: Vec<Value> = vec![Value::Null];
                over.extend(
                    self.own_attrs(t)
                        .iter()
                        .filter(|a| {
                            matches!(
                                a.family,
                                AttrFamily::Time { .. } | AttrFamily::Quantity { .. }
                            )
                        })
                        .map(|a| Value::from(a.name.as_str())),
                );
                let mut related: Vec<&str> =
                    self.edges(t).iter().map(|(_, o, _)| o.as_str()).collect();
                related.sort_unstable();
                related.dedup();
                over.extend(related.into_iter().map(Value::from));
                let mut props = Map::new();
                props.insert("target_type".into(), json!({"const": t}));
                props.insert("filters".into(), self.filters(t));
                props.insert("relations".into(), self.relations(t, 1));
                props.insert("aggregate".into(), json!({"enum": AGGREGATES}));
                props.insert("aggregate_over".into(), json!({"enum": over}));
                let required: Vec<Value> = props.keys().map(|k| Value::from(k.as_str())).collect();
                json!({"type": "object", "additionalProperties": false, "properties": props, "required": required})
            })
            .collect();
        json!({"anyOf": branches})
    }
}

/// `{"type": "object", "additionalProperties": false, "properties": ..,
/// "required": every property}` — the probe's closed-object shape for a
/// relation item (a branch lists `properties` first, as the probe did).
fn object(props: Map<String, Value>) -> Value {
    let required: Vec<Value> = props.keys().map(|k| Value::from(k.as_str())).collect();
    json!({"type": "object", "additionalProperties": false, "required": required, "properties": props})
}

/// The probe's attribute kind: `time range`, `quantity, unit g`,
/// `text, one of ['gold', 'silver']` (a Python list's repr, as it printed).
fn attr_kind(a: &AttrDecl) -> String {
    match &a.family {
        AttrFamily::Text { values } if !values.is_empty() => {
            let quoted: Vec<String> = values.iter().map(|v| format!("'{v}'")).collect();
            format!("text, one of [{}]", quoted.join(", "))
        }
        AttrFamily::Text { .. } => "text".into(),
        AttrFamily::Quantity { unit: Some(u) } => format!("quantity, unit {u}"),
        AttrFamily::Quantity { unit: None } => "quantity".into(),
        AttrFamily::Time { range: true } => "time range".into(),
        AttrFamily::Time { range: false } => "time".into(),
        AttrFamily::Ref { .. } => "ref".into(),
    }
}

/// The documentation and schema a model writes a typed query against.
pub fn query_grammar(policies: &OntologyPolicies) -> QueryGrammar {
    let d = Declared::new(policies);
    let mut l: Vec<String> = vec![
        "KNOWLEDGE BASE SCHEMA".into(),
        String::new(),
        policies.prose.guidance.trim().to_string(),
        String::new(),
        "Entity types and their attributes:".into(),
    ];
    for e in &d.entities {
        l.push(
            format!("- {}: {}", e.name, e.description)
                .trim_end()
                .to_string(),
        );
        for a in d.own_attrs(&e.name) {
            l.push(format!(
                "    {} ({}): {}",
                a.name,
                attr_kind(a),
                a.description
            ));
        }
    }
    l.push(String::new());
    l.push("Relations (each usable from either end; `other_type` names the far end):".into());
    let mut seen: Vec<(&str, &str, &str)> = Vec::new();
    for e in &d.entities {
        for (r, o, gloss) in d.edges(&e.name) {
            let key = (e.name.as_str(), r.as_str(), o.as_str());
            if !seen.contains(&key) {
                seen.push(key);
                l.push(format!(
                    "- from a {}: relation `{r}`, other_type `{o}` — {gloss}",
                    e.name
                ));
            }
        }
    }
    for t in policies
        .shape
        .types
        .iter()
        .filter(|t| t.kind == TypeKind::Relation)
    {
        l.push(format!("  `{}` means: {}", t.name, t.description));
    }
    l.push(String::new());
    l.push(GRAMMAR.trim_end_matches('\n').to_string());
    QueryGrammar {
        documentation: l.join("\n"),
        schema: d.schema(),
    }
}

#[cfg(test)]
#[path = "tests/typed_prompt.rs"]
mod tests;
