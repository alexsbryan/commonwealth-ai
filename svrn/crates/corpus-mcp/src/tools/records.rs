// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atoms_lookup`: the atoms a corpus's enrichment produced, by kind and/or
//! name substring, read from `atlas/atoms.json`, each shown as the record it
//! is under the ontology the corpus recorded (`atlas/ontology.json`): its
//! declared type, the attributes that type declares (absent ones named,
//! never filled), each `ref` attribute as the record it points at, and the
//! records and statements whose refs point at it, so "which deals are with
//! this company" is one lookup. A claim's `subject` is a ref. With no
//! recorded ontology the attributes are shown as stored and refs are ids,
//! and the answer says so.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use corpus_engine_atlas_reader::raw::read_atlas_ontology;
use serde_json::{json, Map, Value};
use understanding_vocab::atoms::AtomEnvelope;
use understanding_vocab::ontology::decl::AttrFamily;
use understanding_vocab::ontology::{AtlasOntologyFile, OntologyPolicies};
use understanding_vocab::read::read_atlas_atoms;

use super::{atom_text, refuse, truncate, ToolOutcome};

/// The records whose refs point at one record that a row lists; the rest are
/// counted (`referenced_by_total`).
const INBOUND_SHOWN: usize = 20;
/// The inbound refs a row's text names.
const INBOUND_IN_TEXT: usize = 5;

/// The declared type an atom is an instance of: an entity's `entity_type`,
/// a claim's `claim_kind`.
fn declared_type(a: &AtomEnvelope) -> Option<&str> {
    match a {
        AtomEnvelope::Entity(e) => Some(e.entity_type.as_str_repr()),
        AtomEnvelope::Claim(c) => c.claim_kind.as_deref(),
        _ => None,
    }
}

fn stored(a: &AtomEnvelope) -> Option<&Map<String, Value>> {
    match a {
        AtomEnvelope::Entity(e) => Some(&e.attributes),
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

/// One end of a ref as a row shows it; a target the atlas does not hold is
/// marked `absent` rather than dropped.
fn end(attribute: &str, id: &str, by_id: &HashMap<&str, &AtomEnvelope>) -> Value {
    match by_id.get(id) {
        Some(a) => json!({
            "attribute": attribute,
            "id": id,
            "type": declared_type(a),
            "name": a.display_name(Some(120)),
        }),
        None => json!({ "attribute": attribute, "id": id, "absent": true }),
    }
}

fn shown(v: &Value) -> String {
    match v {
        Value::String(s) => truncate(s, 80),
        other => truncate(&other.to_string(), 80),
    }
}

/// The ontology the corpus recorded, or the sentence that says why there is
/// none to read refs by.
fn recorded_ontology(atlas_dir: &Path) -> Result<AtlasOntologyFile, String> {
    let path = atlas_dir.join(AtlasOntologyFile::FILE);
    if !path.exists() {
        return Err(format!(
            "no ontology recorded at {}: attributes are shown as stored and refs as ids",
            path.display()
        ));
    }
    read_atlas_ontology(atlas_dir).ok_or_else(|| {
        format!(
            "{} is not an AtlasOntologyFile document: attributes are shown as stored and refs as ids",
            path.display()
        )
    })
}

pub(super) fn lookup(corpus: &str, atlas_dir: &Path, args: &Value) -> ToolOutcome {
    let path = atlas_dir.join("atoms.json");
    let missing = format!(
        "no atlas at {} — this corpus's enrichment was not built or pulled on this machine",
        path.display()
    );
    let file = match read_atlas_atoms(atlas_dir) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return refuse(&missing),
        Err(e) => return refuse(&e.to_string()),
    };
    let needle = args["query"].as_str().map(str::to_lowercase);
    let kind = args["kind"].as_str().map(str::to_lowercase);
    let limit = args["limit"].as_u64().map(|l| l as usize).unwrap_or(20);

    let total = file.atoms().len();
    let matched: Vec<&AtomEnvelope> = file
        .atoms()
        .iter()
        .filter(|a| kind.as_deref().is_none_or(|k| a.atom_type().label() == k))
        .filter(|a| {
            needle.as_deref().is_none_or(|q| {
                a.display_name(None).to_lowercase().contains(q)
                    || atom_text(a).to_lowercase().contains(q)
            })
        })
        .take(limit)
        .collect();
    let ontology = recorded_ontology(atlas_dir);
    let policies = ontology.as_ref().ok().map(|f| &f.policies);
    if let Err(why) = &ontology {
        tracing::debug!(corpus, %why, "atoms_lookup: refs are not resolved");
    }
    let by_id: HashMap<&str, &AtomEnvelope> =
        file.atoms().iter().map(|a| (a.id().as_str(), a)).collect();
    let mut inbound: BTreeMap<&str, Vec<(&str, &str)>> = matched
        .iter()
        .map(|a| (a.id().as_str(), Vec::new()))
        .collect();
    for a in file.atoms() {
        for (attribute, target) in refs_of(a, policies) {
            if let Some(from) = inbound.get_mut(target) {
                from.push((attribute, a.id().as_str()));
            }
        }
    }
    let rows: Vec<Value> = matched
        .iter()
        .map(|a| {
            let decl = policies.zip(declared_type(a)).and_then(|(p, t)| p.type_decl(t));
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
                // No declaration to read it by: as stored.
                (None, Some(map)) => (map.clone(), Vec::new()),
                (_, None) => (Map::new(), Vec::new()),
            };
            let refs: Vec<Value> = refs_of(a, policies)
                .into_iter()
                .map(|(attribute, id)| end(attribute, id, &by_id))
                .collect();
            let from = &inbound[a.id().as_str()];
            let referenced_by: Vec<Value> = from
                .iter()
                .take(INBOUND_SHOWN)
                .map(|(attribute, id)| {
                    let mut v = end(attribute, id, &by_id);
                    v["attribute"] = json!(by_id.get(id).and_then(|s| declared_type(s)).map_or_else(
                        || attribute.to_string(),
                        |t| format!("{t}.{attribute}")
                    ));
                    v
                })
                .collect();
            json!({
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
            })
        })
        .collect();
    let text = if matched.is_empty() {
        format!(
            "No atoms matched in `{corpus}` ({total} atoms in {}).",
            path.display()
        )
    } else {
        let mut t = format!(
            "{} of {total} atoms in `{corpus}` (schema {}):\n",
            matched.len(),
            file.schema_version
        );
        if let Err(why) = &ontology {
            t.push_str(&format!("({why})\n"));
        }
        for (a, row) in matched.iter().zip(&rows) {
            t.push_str(&format!(
                "- [{}] {} — {} ({})\n",
                a.atom_type().label(),
                a.display_name(Some(120)),
                truncate(atom_text(a), 200),
                a.id().as_str()
            ));
            let mut facts: Vec<String> = row["type"]
                .as_str()
                .map(|t| format!("type {t}"))
                .into_iter()
                .collect();
            let refs = row["refs"].as_array().unwrap();
            for (name, v) in row["attributes"].as_object().unwrap() {
                if !refs.iter().any(|r| r["attribute"] == name.as_str()) {
                    facts.push(format!("{name}: {}", shown(v)));
                }
            }
            for r in refs {
                let target = r["name"]
                    .as_str()
                    .map_or_else(|| "(not in this atlas)".to_string(), |n| truncate(n, 80));
                facts.push(format!(
                    "{} → {target} ({})",
                    r["attribute"].as_str().unwrap(),
                    r["id"].as_str().unwrap()
                ));
            }
            if !facts.is_empty() {
                t.push_str(&format!("    {}\n", facts.join(" · ")));
            }
            let from = row["referenced_by"].as_array().unwrap();
            if !from.is_empty() {
                let named: Vec<String> = from
                    .iter()
                    .take(INBOUND_IN_TEXT)
                    .map(|r| {
                        format!(
                            "{} ← {} ({})",
                            r["attribute"].as_str().unwrap(),
                            truncate(r["name"].as_str().unwrap_or(""), 60),
                            r["id"].as_str().unwrap()
                        )
                    })
                    .collect();
                t.push_str(&format!(
                    "    referenced by {}: {}\n",
                    row["referenced_by_total"],
                    named.join("; ")
                ));
            }
        }
        t
    };
    ToolOutcome::answer(
        text.trim_end(),
        Some(json!({
            "corpus": corpus,
            "total_atoms": total,
            "refs_resolved": ontology.is_ok(),
            "atoms": rows,
        })),
    )
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
