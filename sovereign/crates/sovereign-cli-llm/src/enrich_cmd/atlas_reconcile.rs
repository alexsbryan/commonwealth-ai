//! `sovereign enrich reconcile <corpus>` — Phase 4 multi-origin entity
//! reconciliation as a PRODUCTION pipeline step.
//!
//! Until now the multi-origin merger ([`reconcile`]) was exercised only
//! by `sovereign bench enron` — it never ran in the enrichment pipeline,
//! so a desktop user querying a multi-inbox corpus saw the raw, un-merged
//! per-mention atoms while the strong reconciliation numbers lived only
//! in the bench. This command closes that gap: it loads the resolved
//! `atoms.json`, runs the deterministic merger, and persists
//!
//!   - `atlas/reconciliation.json`     — the canonical entity clustering
//!     (every merge, with the signal evidence that justified it), and
//!   - `atlas/reconciliation_oplog.jsonl` — the append-only audit trail.
//!
//! It is **non-destructive**: `atoms.json` (the per-mention provenance
//! record) is left intact, so the raw atoms and the derived clustering
//! are both inspectable. `reconcile` takes no `InferenceFn` — the merge
//! is pure deterministic signal logic, so this needs no daemon and is
//! reproducible. It runs the same `reconcile` the bench scores, so the
//! bench's B³/precision numbers ARE this artifact's numbers.

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, Entity};
use corpus_engine::enrichment::atlas::{read_atlas_atoms, ATLAS_DIRNAME};
use corpus_engine::enrichment::reconciliation::{
    reconcile, OplogWriter, ReconciledEntity, ReconciliationPolicy,
};
use serde::Serialize;

use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich reconcile",
    summary: "Run Phase 4 multi-origin reconciliation over a resolved atlas; persist the canonical clustering.",
    sections: &[
        HelpSection::Usage("sovereign enrich reconcile <corpus-id>"),
        HelpSection::Examples(&[(
            "sovereign enrich reconcile enron-sample-multi-wide",
            "Merge cross-inbox entity variants; write atlas/reconciliation.json + oplog.",
        )]),
        HelpSection::Notes(
            "Deterministic (no LLM/daemon). Non-destructive — atoms.json is preserved; the \
             merged clustering is written alongside it. Runs the same merger \
             `sovereign bench enron` scores.",
        ),
    ],
};

/// `reconciliation.json` — the persisted canonical clustering. Only
/// entities the merger actually collapsed (more than one source atom)
/// are listed in detail; singletons are exactly the un-merged atoms in
/// atoms.json, so re-listing them would just duplicate that file.
#[derive(Serialize)]
struct ReconciliationArtifact<'a> {
    schema_version: u32,
    corpus: &'a str,
    policy: &'a ReconciliationPolicy,
    input_atom_count: usize,
    canonical_entity_count: usize,
    merged_entity_count: usize,
    merged_entities: Vec<&'a ReconciledEntity>,
}

pub async fn cmd_atlas_reconcile(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let corpus_id = match args.iter().find(|a| !a.starts_with('-')) {
        Some(c) => c.clone(),
        None => {
            eprintln!("error: <corpus-id> is required");
            help::print(&HELP);
            return 2;
        }
    };

    // The only precondition is a resolved atlas. We deliberately do NOT
    // gate on `EnrichConfig` (the `sovereign enrich init` flow): corpora
    // enriched by the daemon's recipe pipeline — like the Enron samples —
    // have an atoms.json but no enrich-init config, and reconciliation
    // reads atoms.json directly, exactly as `sovereign bench enron` does.
    let atlas_dir = paths::index_root(&corpus_id).join(ATLAS_DIRNAME);
    let atoms_path = atlas_dir.join("atoms.json");
    if !atoms_path.exists() {
        eprintln!(
            "error: no atoms.json at {}. Resolve the atlas for `{corpus_id}` first.",
            atoms_path.display()
        );
        return 2;
    }

    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: read {}: {e}", atoms_path.display());
            return 1;
        }
    };
    let entities: Vec<Entity> = atoms_file
        .atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => Some(e),
            _ => None,
        })
        .collect();
    let input_atom_count = entities.len();
    if input_atom_count == 0 {
        eprintln!(
            "error: 0 Entity atoms in {} — nothing to reconcile.",
            atoms_path.display()
        );
        return 2;
    }

    // Default policy — identical to the bench's `--policy tuned` baseline
    // (cross_origin_required_signals = 2). Deterministic; no judge LLM is
    // invoked because `reconcile` carries no InferenceFn.
    let policy = ReconciliationPolicy::default();
    println!("─── reconcile: {corpus_id} ───");
    println!("  input entity atoms : {input_atom_count}");
    let outcome = reconcile(entities, &policy);
    let canonical_entity_count = outcome.entities.len();
    let merged: Vec<&ReconciledEntity> = outcome
        .entities
        .iter()
        .filter(|e| e.source_atom_ids.len() > 1)
        .collect();
    let merged_entity_count = merged.len();
    let collapsed = input_atom_count.saturating_sub(canonical_entity_count);

    // Persist the audit trail (append-only) so the per-merge rationale
    // survives — the same writer the bench uses.
    let oplog = OplogWriter::new(atlas_dir.clone());
    let mut oplog_errs = 0usize;
    for entry in &outcome.oplog_entries {
        if oplog.append(entry).is_err() {
            oplog_errs += 1;
        }
    }

    // Persist the canonical clustering.
    let artifact = ReconciliationArtifact {
        schema_version: 1,
        corpus: &corpus_id,
        policy: &policy,
        input_atom_count,
        canonical_entity_count,
        merged_entity_count,
        merged_entities: merged,
    };
    let recon_path = atlas_dir.join("reconciliation.json");
    match serde_json::to_string_pretty(&artifact) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&recon_path, json) {
                eprintln!("error: write {}: {e}", recon_path.display());
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serialize reconciliation artifact: {e}");
            return 1;
        }
    }

    println!("  canonical entities : {canonical_entity_count}  ({collapsed} atoms collapsed into {merged_entity_count} multi-source clusters)");
    println!("  oplog merges       : {}", outcome.oplog_entries.len());
    if oplog_errs > 0 {
        eprintln!("  warn: {oplog_errs} oplog entries failed to append");
    }
    println!("  → {}", recon_path.display());
    0
}
