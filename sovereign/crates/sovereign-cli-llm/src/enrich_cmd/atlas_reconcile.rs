// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich reconcile <corpus>` — Phase 4 multi-origin entity
//! reconciliation as a PRODUCTION pipeline step.
//!
//! Until now the multi-origin merger ([`reconcile`]) was exercised only
//! by `svrn bench enron` — it never ran in the enrichment pipeline,
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
use corpus_engine::enrichment::atlas::{
    append_atoms_and_edges, read_atlas_atoms, read_atlas_edges, read_atlas_ontology, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::ontology::TypeIndex;
use corpus_engine::enrichment::reconciliation::{
    reconcile, reify_merges, ReconciledEntity, ReconciliationAct, ReconciliationPolicy,
};
use serde::Serialize;

use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich reconcile",
    summary: "Run Phase 4 multi-origin reconciliation over a resolved atlas; persist the canonical clustering.",
    sections: &[
        HelpSection::Usage("svrn enrich reconcile <corpus-id>"),
        HelpSection::Examples(&[(
            "svrn enrich reconcile enron-sample-multi-wide",
            "Merge cross-inbox entity variants; write atlas/reconciliation.json + oplog.",
        )]),
        HelpSection::Notes(
            "Deterministic (no LLM/daemon). Non-destructive — atoms.json is preserved; the \
             merged clustering is written alongside it. Runs the same merger \
             `svrn bench enron` scores.",
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
    // gate on `EnrichConfig` (the `svrn enrich init` flow): corpora
    // enriched by the daemon's recipe pipeline — like the Enron samples —
    // have an atoms.json but no enrich-init config, and reconciliation
    // reads atoms.json directly, exactly as `svrn bench enron` does.
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

    // The identity criteria this atlas was built under, read from the atlas
    // itself (`atlas/ontology.json`) — the reconciler runs over atoms, not
    // over a recipe, and the atlas is what records the policies that produced
    // it. An atlas that declares nothing yields an empty map, so the policy is
    // byte-for-byte the bench's `--policy tuned` baseline
    // (cross_origin_required_signals = 2) and every Enron run is unaffected.
    let ontology = read_atlas_ontology(&atlas_dir);
    let declared = ontology
        .as_ref()
        .is_some_and(|o| o.policies.has_declarations());
    let policy = ReconciliationPolicy {
        identity: ontology
            .as_ref()
            .map(|o| TypeIndex::from_policies(&o.policies).effective_identity_policy())
            .unwrap_or_default(),
        ..Default::default()
    };
    println!("─── reconcile: {corpus_id} ───");
    println!("  input entity atoms : {input_atom_count}");
    if declared {
        println!(
            "  identity criteria  : {} external, {} descriptive",
            policy.identity.identity.len(),
            policy.identity.identity_fallback.len()
        );
    }
    // Deterministic; no judge LLM is invoked because `reconcile` carries no
    // InferenceFn.
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
    let oplog = corpus_engine::oplog::Oplog::<ReconciliationAct>::new(atlas_dir.clone());
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

    // A declared corpus asked for its identity criteria to be part of the
    // knowledge, so each merge also lands as a `same_as` Claim a reader can
    // find from either side. An undeclared corpus keeps today's silent merge
    // — making reification always-on is a DEFAULTS_LEDGER decision with the
    // Enron B³ lane as its gate, not this command's call.
    let mut same_as_claims = 0usize;
    if declared && !outcome.reified.is_empty() {
        match append_reified_merges(&atlas_dir, &outcome.reified) {
            Ok(n) => same_as_claims = n,
            Err(e) => {
                eprintln!("error: writing same_as claims: {e}");
                return 1;
            }
        }
    }

    println!("  canonical entities : {canonical_entity_count}  ({collapsed} atoms collapsed into {merged_entity_count} multi-source clusters)");
    println!("  oplog merges       : {}", outcome.oplog_entries.len());
    if declared {
        println!("  same_as claims     : {same_as_claims}");
    }
    if oplog_errs > 0 {
        eprintln!("  warn: {oplog_errs} oplog entries failed to append");
    }
    println!("  → {}", recon_path.display());
    0
}

/// Append one `same_as` Claim per merge to the atlas, continuing its id
/// sequences. Returns how many claims were written.
///
/// Ids continue from what is already on disk rather than restarting at 1:
/// this command runs AFTER `atlas-resolve` wrote the atlas, so a fresh
/// sequence would collide with the claims already there.
fn append_reified_merges(
    atlas_dir: &std::path::Path,
    reified: &[corpus_engine::enrichment::reconciliation::ReifiedMerge],
) -> Result<usize, String> {
    let atoms = read_atlas_atoms(atlas_dir).map_err(|e| format!("read atoms.json: {e}"))?;
    let edges = read_atlas_edges(atlas_dir).map_err(|e| format!("read edges.json: {e}"))?;
    let next_claim = 1 + atoms
        .atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Claim(c) => index_suffix(c.id.as_str()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let next_edge = 1 + edges
        .edges
        .iter()
        .filter_map(|e| index_suffix(e.id.as_str()))
        .max()
        .unwrap_or(0);

    let (claims, new_edges) = reify_merges(reified, next_claim, next_edge);
    let count = claims.len();
    let envelopes: Vec<AtomEnvelope> = claims.into_iter().map(AtomEnvelope::Claim).collect();
    append_atoms_and_edges(atlas_dir, &envelopes, &new_edges)
        .map_err(|e| format!("append to atlas: {e}"))?;
    Ok(count)
}

/// The numeric tail of a `<kind>-<index>` id. `None` for a content-hash id
/// (the v2 constructors) — those carry no sequence, so they contribute
/// nothing to "what is the next free index", which is the right answer
/// rather than a guess.
fn index_suffix(id: &str) -> Option<usize> {
    id.rsplit_once('-')?.1.parse().ok()
}
