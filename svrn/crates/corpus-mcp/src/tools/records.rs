// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atoms_lookup`: the atoms a corpus's enrichment produced, by kind and/or
//! name substring, read from `atlas/atoms.json`.

use std::path::Path;

use serde_json::{json, Value};
use understanding_vocab::atoms::AtomEnvelope;
use understanding_vocab::read::read_atlas_atoms;

use super::{atom_text, refuse, truncate, ToolOutcome};

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
    let rows: Vec<Value> = matched
        .iter()
        .map(|a| {
            json!({
                "id": a.id(),
                "kind": a.atom_type().label(),
                "name": a.display_name(Some(120)),
                "text": truncate(atom_text(a), 400),
                "evidence_chunks": a.evidence().iter().map(|c| c.chunk_id.clone()).collect::<Vec<_>>(),
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
        for a in &matched {
            t.push_str(&format!(
                "- [{}] {} — {} ({})\n",
                a.atom_type().label(),
                a.display_name(Some(120)),
                truncate(atom_text(a), 200),
                a.id().as_str()
            ));
        }
        t
    };
    ToolOutcome::answer(
        text.trim_end(),
        Some(json!({ "corpus": corpus, "total_atoms": total, "atoms": rows })),
    )
}
