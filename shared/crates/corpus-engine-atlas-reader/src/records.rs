// SPDX-License-Identifier: AGPL-3.0-or-later
//! One record as every surface shows it: corpus-mcp's `atoms_lookup`, and the
//! typed rows `atlas-query --json` prints, from one builder (ARCH 8). A row is
//! the atom read under the ontology the corpus recorded: its declared type,
//! the attributes that type declares (absent ones named, never filled), each
//! `ref` attribute as the record it points at with its identity values as
//! written, the records and statements whose refs point at it, how each
//! folded attribute was folded with the readings it came from (`__folds`),
//! and, for a statement, how its subject was decided (`__resolve_link`).

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use understanding_vocab::atoms::AtomEnvelope;
use understanding_vocab::ontology::decl::AttrFamily;
use understanding_vocab::ontology::OntologyPolicies;

/// The records whose refs point at one record that a row lists; the rest are
/// counted (`referenced_by_total`).
pub const INBOUND_SHOWN: usize = 20;

/// Where an atom keeps how its attributes were folded (the build's
/// `outcome::write_fold`).
const FOLDS: &str = "__folds";
/// Where a statement keeps how its subject was decided (`resolve_link`).
const LINK: &str = "__resolve_link";

/// An atom's own text: an entity's or event's description, a claim's content.
pub fn atom_text(a: &AtomEnvelope) -> &str {
    match a {
        AtomEnvelope::Entity(e) => &e.description,
        AtomEnvelope::Event(e) => &e.description,
        AtomEnvelope::Configuration(c) => &c.description,
        AtomEnvelope::Claim(c) => &c.content,
        AtomEnvelope::Question(q) => &q.content,
        AtomEnvelope::Position(p) => &p.content,
        _ => "",
    }
}

/// `s` trimmed, cut to `max` characters with an ellipsis when longer.
pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>().trim_end())
    }
}

/// The declared type an atom is an instance of: an entity's `entity_type`,
/// an event's `event_type`, a claim's `claim_kind`.
pub fn declared_type(a: &AtomEnvelope) -> Option<&str> {
    match a {
        AtomEnvelope::Entity(e) => Some(e.entity_type.as_str_repr()),
        AtomEnvelope::Event(e) => Some(e.event_type.as_str_repr()),
        AtomEnvelope::Claim(c) => c.claim_kind.as_deref(),
        _ => None,
    }
}

fn stored(a: &AtomEnvelope) -> Option<&Map<String, Value>> {
    match a {
        AtomEnvelope::Entity(e) => Some(&e.attributes),
        AtomEnvelope::Event(e) => Some(&e.attributes),
        AtomEnvelope::Claim(c) => Some(&c.attributes),
        _ => None,
    }
}

/// Each (attribute, record id) an atom points at: its type's declared `ref`
/// attributes, one id or a list, and a claim's `subject`.
fn refs_of<'a>(
    a: &'a AtomEnvelope,
    policies: Option<&OntologyPolicies>,
) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    if let AtomEnvelope::Claim(c) = a {
        if let Some(subject) = &c.subject {
            out.push(("subject", subject.as_str()));
        }
    }
    let (Some(decl), Some(map)) = (
        policies
            .zip(declared_type(a))
            .and_then(|(p, t)| p.type_decl(t)),
        stored(a),
    ) else {
        return out;
    };
    for attr in &decl.attributes {
        if !matches!(attr.family, AttrFamily::Ref { .. }) {
            continue;
        }
        let Some((name, value)) = map.get_key_value(attr.name.as_str()) else {
            continue;
        };
        match value {
            Value::String(id) => out.push((name.as_str(), id.as_str())),
            Value::Array(ids) => out.extend(
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(|id| (name.as_str(), id)),
            ),
            _ => {}
        }
    }
    out
}

/// Every record of one atlas as rows: the atoms by id and, per atom, the
/// atoms whose refs point at it, indexed once.
pub struct RecordView<'a> {
    policies: Option<&'a OntologyPolicies>,
    by_id: HashMap<&'a str, &'a AtomEnvelope>,
    inbound: HashMap<&'a str, Vec<(&'a str, &'a str)>>,
}

impl<'a> RecordView<'a> {
    /// `policies` is the ontology the corpus recorded; without one, refs are
    /// ids and attributes are shown as stored.
    pub fn new(atoms: &'a [AtomEnvelope], policies: Option<&'a OntologyPolicies>) -> Self {
        let by_id = atoms.iter().map(|a| (a.id().as_str(), a)).collect();
        let mut inbound: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
        for a in atoms {
            for (attribute, target) in refs_of(a, policies) {
                inbound
                    .entry(target)
                    .or_default()
                    .push((attribute, a.id().as_str()));
            }
        }
        Self {
            policies,
            by_id,
            inbound,
        }
    }

    pub fn atom(&self, id: &str) -> Option<&'a AtomEnvelope> {
        self.by_id.get(id).copied()
    }

    /// A record's identity values as written, joined: what a reader knows it
    /// by (`birchhall.example`), absent for a record its type gives none.
    fn key(&self, a: &AtomEnvelope) -> Option<String> {
        let decl = self
            .policies
            .zip(declared_type(a))
            .and_then(|(p, t)| p.type_decl(t))?;
        let map = stored(a)?;
        let values: Vec<&str> = decl
            .identity
            .iter()
            .filter_map(|k| map.get(k).and_then(Value::as_str))
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    }

    /// One end of a ref as a row shows it; a target the atlas does not hold
    /// is marked `absent` rather than dropped.
    fn end(&self, attribute: &str, id: &str) -> Value {
        match self.by_id.get(id) {
            Some(a) => {
                let mut end = json!({
                    "attribute": attribute,
                    "id": id,
                    "type": declared_type(a),
                    "name": a.display_name(Some(120)),
                });
                if let Some(key) = self.key(a) {
                    end["key"] = Value::String(key);
                }
                end
            }
            None => json!({ "attribute": attribute, "id": id, "absent": true }),
        }
    }

    /// How each attribute was folded, as the atom keeps it, each reading
    /// given its statement's document date when the reading lacks one and,
    /// for a reference, the record it names.
    fn folds(&self, a: &AtomEnvelope) -> Map<String, Value> {
        let Some(Value::Object(folds)) = stored(a).and_then(|m| m.get(FOLDS)) else {
            return Map::new();
        };
        let mut out = folds.clone();
        for fold in out.values_mut() {
            let Some(readings) = fold.get_mut("readings").and_then(Value::as_array_mut) else {
                continue;
            };
            for r in readings {
                if r.get("date").is_none_or(Value::is_null) {
                    if let Some(date) = r["claim"]
                        .as_str()
                        .and_then(|c| self.by_id.get(c))
                        .and_then(|c| stored(c))
                        .and_then(|m| m.get("document_date"))
                    {
                        r["date"] = date.clone();
                    }
                }
                if let Some(named) = r["value"].as_str().and_then(|v| self.by_id.get(v)) {
                    let mut names = json!({
                        "type": declared_type(named),
                        "name": named.display_name(Some(120)),
                    });
                    if let Some(key) = self.key(named) {
                        names["key"] = Value::String(key);
                    }
                    r["names"] = names;
                }
            }
        }
        out
    }

    /// The row for `a`.
    pub fn row(&self, a: &AtomEnvelope) -> Value {
        let decl = self
            .policies
            .zip(declared_type(a))
            .and_then(|(p, t)| p.type_decl(t));
        let (attributes, unset): (Map<String, Value>, Vec<&str>) = match (decl, stored(a)) {
            (Some(decl), Some(map)) => {
                let mut held = Map::new();
                let mut unset = Vec::new();
                for attr in &decl.attributes {
                    match map.get(&attr.name) {
                        Some(v) => {
                            held.insert(attr.name.clone(), v.clone());
                        }
                        None => unset.push(attr.name.as_str()),
                    }
                }
                (held, unset)
            }
            // No declaration to read it by: as stored, reserved keys aside.
            (None, Some(map)) => (
                map.iter()
                    .filter(|(k, _)| !k.starts_with("__"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Vec::new(),
            ),
            (_, None) => (Map::new(), Vec::new()),
        };
        let refs: Vec<Value> = refs_of(a, self.policies)
            .into_iter()
            .map(|(attribute, id)| self.end(attribute, id))
            .collect();
        let from = self
            .inbound
            .get(a.id().as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let referenced_by: Vec<Value> = from
            .iter()
            .take(INBOUND_SHOWN)
            .map(|(attribute, id)| {
                let mut v = self.end(attribute, id);
                v["attribute"] = json!(self
                    .by_id
                    .get(id)
                    .and_then(|s| declared_type(s))
                    .map_or_else(|| attribute.to_string(), |t| format!("{t}.{attribute}")));
                v
            })
            .collect();
        let mut row = json!({
            "id": a.id(),
            "kind": a.atom_type().label(),
            "name": a.display_name(Some(120)),
            "text": truncate(atom_text(a), 400),
            "evidence_chunks": a.evidence().iter().map(|c| c.chunk_id.clone()).collect::<Vec<_>>(),
            "type": declared_type(a),
            "attributes": attributes,
            "unset": unset,
            "refs": refs,
            "referenced_by": referenced_by,
            "referenced_by_total": from.len(),
        });
        let folds = self.folds(a);
        if !folds.is_empty() {
            row["folds"] = Value::Object(folds);
        }
        if let Some(link) = stored(a).and_then(|m| m.get(LINK)) {
            row["link"] = link.clone();
        }
        row
    }
}

/// A fold as one line of text: a decided value as itself, anything else as
/// its outcome over the values it reached (`tie "A" | "B"`).
pub fn fold_line(attribute: &str, fold: &Value) -> String {
    let Some(outcome) = fold["outcome"].as_str() else {
        return format!("{attribute}: no outcome recorded");
    };
    let values: Vec<String> = fold["values"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| match v {
            Value::String(s) => format!("{:?}", truncate(s, 60)),
            other => other.to_string(),
        })
        .collect();
    let readings = fold["readings"].as_array().map_or(0, Vec::len);
    if outcome == "decided" {
        format!(
            "{attribute}: {} ({readings} reading(s))",
            values.join(" | ")
        )
    } else {
        format!(
            "{attribute}: {outcome} {} ({readings} reading(s))",
            values.join(" | ")
        )
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
