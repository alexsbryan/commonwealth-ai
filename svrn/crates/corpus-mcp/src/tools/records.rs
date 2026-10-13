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

use std::path::Path;

use corpus_engine_atlas_reader::raw::read_atlas_ontology;
use corpus_engine_atlas_reader::records::{
    atom_text, declared_type, fold_line, truncate, RecordView,
};
use serde_json::{json, Value};
use understanding_vocab::atoms::AtomEnvelope;
use understanding_vocab::ontology::AtlasOntologyFile;
use understanding_vocab::read::read_atlas_atoms;

use super::{refuse, ToolOutcome};

/// The inbound refs a row's text names.
const INBOUND_IN_TEXT: usize = 5;

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

/// Whether `a` matches `needle` (lowercased): by its display name, an
/// entity's aliases, or its text.
fn matches(a: &AtomEnvelope, needle: &str) -> bool {
    let aliases = match a {
        AtomEnvelope::Entity(e) => e.aliases.as_slice(),
        _ => &[],
    };
    a.display_name(None).to_lowercase().contains(needle)
        || aliases.iter().any(|x| x.to_lowercase().contains(needle))
        || atom_text(a).to_lowercase().contains(needle)
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
    let declared = args["type"].as_str();
    let ids: Option<Vec<&str>> = args["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect());
    let limit = args["limit"].as_u64().map(|l| l as usize).unwrap_or(20);
    let offset = args["offset"].as_u64().map(|o| o as usize).unwrap_or(0);

    let total = file.atoms().len();
    let ontology = recorded_ontology(atlas_dir);
    let policies = ontology.as_ref().ok().map(|f| &f.policies);
    if let Err(why) = &ontology {
        tracing::debug!(corpus, %why, "atoms_lookup: refs are not resolved");
    }
    let view = RecordView::new(file.atoms(), policies);
    let matching: Vec<&AtomEnvelope> = file
        .atoms()
        .iter()
        .filter(|a| {
            ids.as_ref()
                .is_none_or(|ids| ids.contains(&a.id().as_str()))
        })
        .filter(|a| kind.as_deref().is_none_or(|k| a.atom_type().label() == k))
        .filter(|a| declared.is_none_or(|t| declared_type(a) == Some(t)))
        .filter(|a| needle.as_deref().is_none_or(|q| matches(a, q)))
        .collect();
    let absent: Vec<&str> = ids
        .iter()
        .flatten()
        .copied()
        .filter(|id| view.atom(id).is_none())
        .collect();
    let found = matching.len();
    let matched: Vec<&AtomEnvelope> = matching.into_iter().skip(offset).take(limit).collect();
    let next_offset = (offset + matched.len() < found).then_some(offset + matched.len());
    let rows: Vec<Value> = matched.iter().map(|a| view.row(a)).collect();
    let mut text = if matched.is_empty() {
        format!(
            "No atoms matched in `{corpus}` ({total} atoms in {}).",
            path.display()
        )
    } else {
        let mut t = format!(
            "{}-{} of {found} matching, of {total} atoms in `{corpus}` (schema {}):\n",
            offset + 1,
            offset + matched.len(),
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
                let key = r["key"]
                    .as_str()
                    .map_or_else(String::new, |k| format!(" [{k}]"));
                facts.push(format!(
                    "{} → {target}{key} ({})",
                    r["attribute"].as_str().unwrap(),
                    r["id"].as_str().unwrap()
                ));
            }
            if !facts.is_empty() {
                t.push_str(&format!("    {}\n", facts.join(" · ")));
            }
            if let Some(folds) = row["folds"].as_object() {
                let lines: Vec<String> = folds.iter().map(|(attr, f)| fold_line(attr, f)).collect();
                t.push_str(&format!("    folded: {}\n", lines.join(" · ")));
            }
            if let Some(how) = row["link"]["decided_by"].as_str() {
                let score = row["link"]["choice_score"]
                    .as_f64()
                    .map_or_else(String::new, |s| {
                        format!(", choice score {s:.2} (uncalibrated)")
                    });
                t.push_str(&format!("    subject decided by {how}{score}\n"));
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
        if let Some(next) = next_offset {
            t.push_str(&format!("(more: offset {next})\n"));
        }
        t
    };
    if !absent.is_empty() {
        text.push_str(&format!("\nNot in this atlas: {}", absent.join(", ")));
    }
    ToolOutcome::answer(
        text.trim_end(),
        Some(json!({
            "corpus": corpus,
            "total_atoms": total,
            "matching": found,
            "offset": offset,
            "next_offset": next_offset,
            "absent_ids": absent,
            "refs_resolved": ontology.is_ok(),
            "atoms": rows,
        })),
    )
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
