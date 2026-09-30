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
        HelpSection::Usage(
            "svrn enrich reconcile <corpus-id>\n\
             svrn enrich reconcile <corpus-id> --measure <out.json> [--judge-trials N] \
             [--name-similarity-threshold T] [--indexes-dir D] [--append-oplog]",
        ),
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
    if args.iter().any(|a| a == "--measure") {
        return measure(args);
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
        .atoms()
        .iter()
        .cloned()
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
    let oplog = oplog::Oplog::<ReconciliationAct>::new(atlas_dir.clone());
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

/// `svrn enrich reconcile <corpus> --measure <out.json> [--judge-trials N]
/// [--name-similarity-threshold T] [--indexes-dir D] [--append-oplog]` — the merger as a
/// measurement, for `svrn bench enron`, which scores it against ground truth.
///
/// It runs the same `reconcile` over the same entity atoms, under the
/// reconciler's compiled defaults plus the two knobs, never the atlas's
/// declared identity, because the bench sweeps the signal logic. With
/// `--append-oplog` it appends the reconciler's oplog entries, as `bench enron
/// run` always did (`diagnose` never did). It writes
/// `[policy, entities]` (a `ReconciliationPolicy` and every
/// `ReconciledEntity`, singletons included) to `<out.json>` and nothing to the
/// atlas: `reconciliation.json` has product readers, and a sweep's knobs must
/// not reach them.
fn measure(args: &[String]) -> i32 {
    let mut corpus: Option<&str> = None;
    let mut out: Option<std::path::PathBuf> = None;
    let mut indexes_dir: Option<std::path::PathBuf> = None;
    let mut policy = ReconciliationPolicy::default();
    let mut append_oplog = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--append-oplog" {
            append_oplog = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1);
        match (args[i].as_str(), value) {
            ("--measure", Some(v)) => out = Some(v.into()),
            ("--indexes-dir", Some(v)) => indexes_dir = Some(v.into()),
            ("--judge-trials", Some(v)) => match v.parse() {
                Ok(n) => policy.judge_trials = n,
                Err(e) => {
                    eprintln!("error: --judge-trials: {e}");
                    return 2;
                }
            },
            ("--name-similarity-threshold", Some(v)) => match v.parse() {
                Ok(t) => policy.name_similarity_threshold = t,
                Err(e) => {
                    eprintln!("error: --name-similarity-threshold: {e}");
                    return 2;
                }
            },
            (flag, None) if flag.starts_with("--") => {
                eprintln!("error: {flag} requires a value");
                return 2;
            }
            (flag, _) if flag.starts_with("--") => {
                eprintln!("error: unknown flag `{flag}`");
                return 2;
            }
            (positional, _) => {
                corpus = Some(positional);
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    let (Some(corpus), Some(out)) = (corpus, out) else {
        eprintln!("error: --measure needs <corpus-id> and an output path");
        return 2;
    };
    let atlas_dir = match indexes_dir {
        Some(d) => d.join(corpus).join(ATLAS_DIRNAME),
        None => paths::index_root(corpus).join(ATLAS_DIRNAME),
    };
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "error: read {}: {e}",
                atlas_dir.join("atoms.json").display()
            );
            return 1;
        }
    };
    let entities: Vec<Entity> = atoms_file
        .atoms()
        .iter()
        .cloned()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => Some(e),
            _ => None,
        })
        .collect();
    let outcome = reconcile(entities, &policy);
    if append_oplog {
        let oplog = oplog::Oplog::<ReconciliationAct>::new(atlas_dir.clone());
        for entry in &outcome.oplog_entries {
            let _ = oplog.append(entry);
        }
    }
    tracing::debug!(
        corpus,
        canonical = outcome.entities.len(),
        merges = outcome.oplog_entries.len(),
        append_oplog,
        "enrich reconcile --measure"
    );
    let written = serde_json::to_vec(&(&policy, &outcome.entities))
        .map_err(|e| e.to_string())
        .and_then(|bytes| std::fs::write(&out, bytes).map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("error: write {}: {e}", out.display());
        return 1;
    }
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
        .atoms()
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

#[cfg(test)]
mod tests {
    use corpus_engine::enrichment::atlas::atoms::{
        AtomId, AtomsFile, ChunkRef, SignalKind, SignalProvenance,
    };
    use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    use super::*;

    fn ent(id: &str) -> Entity {
        Entity {
            id: AtomId::from_raw(id),
            canonical_name: "Ken Lay".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec-001", None),
            description: String::new(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: SignalProvenance::new("ext", "msg-1", SignalKind::EmailHeader),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    /// `--measure` writes the policy and the clustering to its path, touches
    /// nothing else in the atlas unless asked, and appends the oplog only with
    /// `--append-oplog`. Failing inputs: drop the flag check and the first run
    /// writes an oplog; write reconciliation.json and the listing grows.
    #[test]
    fn measure_writes_the_clustering_and_the_oplog_only_when_asked() {
        let indexes = tempfile::tempdir().unwrap();
        let atlas = indexes.path().join("c").join(ATLAS_DIRNAME);
        let atoms = AtomsFile::new(vec![
            AtomEnvelope::Entity(ent("entity-001")),
            AtomEnvelope::Entity(ent("entity-002")),
        ]);
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(
            atlas.join("atoms.json"),
            serde_json::to_vec(&atoms).unwrap(),
        )
        .unwrap();
        let listing = || {
            let mut names: Vec<String> = std::fs::read_dir(&atlas)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        let before = listing();
        let out = indexes.path().join("m.json");
        let args = |extra: &[&str]| -> Vec<String> {
            let mut v = vec![
                "c".to_string(),
                "--measure".into(),
                out.display().to_string(),
                "--indexes-dir".into(),
                indexes.path().display().to_string(),
                "--judge-trials".into(),
                "5".into(),
            ];
            v.extend(extra.iter().map(|s| s.to_string()));
            v
        };

        assert_eq!(measure(&args(&[])), 0);
        let (policy, entities): (ReconciliationPolicy, Vec<ReconciledEntity>) =
            serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(policy.judge_trials, 5);
        assert_eq!(entities.len(), 1, "the two same-origin mentions merge");
        assert_eq!(entities[0].source_atom_ids.len(), 2);
        assert_eq!(listing(), before, "no oplog, no reconciliation.json");

        assert_eq!(measure(&args(&["--append-oplog"])), 0);
        let after = listing();
        assert_eq!(
            after.len(),
            before.len() + 1,
            "the oplog, and only it: {after:?}"
        );
    }

    #[test]
    fn measure_refuses_a_missing_path_or_corpus() {
        assert_eq!(measure(&["c".into(), "--measure".into()]), 2);
        assert_eq!(measure(&["--measure".into(), "/tmp/x.json".into()]), 2);
    }
}
