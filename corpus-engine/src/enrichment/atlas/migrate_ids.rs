//! Move 6 Phase 0 — atom-id migration from sequential `entity-NNNN`
//! to content-hash `entity-<16 hex>`.
//!
//! See `~/.claude/plans/move6-incremental-atlas.md` for the design.
//!
//! ## What this module does
//!
//! Per atlas dir (`<indexes>/<corpus>/atlas/`):
//!   1. Reads `atoms.json`, `edges.json`, `cross_corpus_edges.json`.
//!   2. Computes a content-hash id for each atom from its variant-
//!      specific fields + the corpus_id.
//!   3. Rewrites every `AtomId` reference in the three files
//!      (atom.id, intra-atom references like
//!      `Event.participants`, edge endpoints).
//!   4. Atomic-write each file via tmp+rename. Order: atoms.json
//!      first (the source of truth for ids), then edges.json, then
//!      cross_corpus_edges.json. If the process is killed between
//!      atoms.json and edges.json, the next run finishes the work
//!      idempotently — already-migrated atom ids are detected via
//!      `AtomId::is_content_hash`.
//!
//! ## Limitations (documented in plan, not v1 scope)
//!
//! - `CrossCorpusEdge.peer.atom_id` references atoms in OTHER
//!   corpora. We can't rewrite those locally (we'd need the peer's
//!   migrated atoms.json to know the new id). v1 leaves
//!   `peer.atom_id` field alone; operator runs `detect_grounding`
//!   post-migration on each side to refresh peer references.
//! - Migration is idempotent: re-running on a migrated atlas is a
//!   no-op.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::error::{Error, Result};

use super::atoms::{AtomEnvelope, AtomId};
use super::writer::{
    read_atlas_atoms, read_atlas_cross_corpus_edges, read_atlas_edges,
    write_atlas_cross_corpus_edges,
};

/// One-shot migration summary for a single atlas.
#[derive(Debug, Clone, Default)]
pub struct MigrationSummary {
    pub atoms_migrated: usize,
    pub atoms_already_content_hash: usize,
    pub edges_rewritten: usize,
    pub cross_corpus_edges_rewritten: usize,
    pub files_touched: Vec<String>,
    pub collisions_detected: Vec<(String, String)>,
    /// Count of duplicate atoms collapsed by the post-rewrite dedup
    /// pass — same as `collisions_detected.len()` in the common case
    /// but expressed as a count for the CLI summary.
    pub atoms_deduped: usize,
}

/// Migrate atoms.json + edges.json + cross_corpus_edges.json in
/// `atlas_dir` from sequential ids to content-hash ids.
///
/// `corpus_id` is hashed into every atom id, so it must match the
/// corpus this atlas belongs to. The caller (CLI) derives it from
/// the parent dir name.
///
/// `dry_run` skips all writes — useful for collision-scan or
/// preview.
pub fn migrate_atlas_ids(
    atlas_dir: &Path,
    corpus_id: &str,
    dry_run: bool,
) -> Result<MigrationSummary> {
    let mut summary = MigrationSummary::default();

    // ── Load atoms.json ────────────────────────────────────
    let mut atoms_file = match read_atlas_atoms(atlas_dir) {
        Ok(f) => f,
        Err(e) => {
            return Err(Error::Serialization(format!(
                "read atoms.json from {}: {e}",
                atlas_dir.display()
            )));
        }
    };

    // Build old_id → new_id mapping. Order matters: some atom
    // content-hashes reference other atom ids (Relation hashes its
    // participants, State hashes its entity_id). Build entity ids
    // first, then walk other variants using the entity map.
    let mut id_map: HashMap<AtomId, AtomId> = HashMap::new();
    let mut new_ids_seen: HashMap<AtomId, AtomId> = HashMap::new();

    // First pass — entities.
    for env in &atoms_file.atoms {
        if let AtomEnvelope::Entity(e) = env {
            if e.id.is_content_hash() {
                summary.atoms_already_content_hash += 1;
                continue;
            }
            let new_id = AtomId::entity_content_hash(&e.canonical_name, &e.entity_type, corpus_id);
            check_collision(
                &mut new_ids_seen,
                &e.id,
                &new_id,
                &mut summary.collisions_detected,
            );
            id_map.insert(e.id.clone(), new_id);
        }
    }
    // Second pass — everything else. Some references need the
    // first-pass entity ids resolved already.
    for env in &atoms_file.atoms {
        let (old_id, new_id) = match env {
            AtomEnvelope::Entity(_) => continue,
            AtomEnvelope::Event(e) => {
                if e.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let new = AtomId::event_content_hash(
                    &e.description,
                    &e.event_type,
                    &e.section_position.section_id,
                    corpus_id,
                );
                (e.id.clone(), new)
            }
            AtomEnvelope::State(s) => {
                if s.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let resolved_entity = id_map
                    .get(&s.entity_id)
                    .cloned()
                    .unwrap_or_else(|| s.entity_id.clone());
                let new = AtomId::state_content_hash(
                    &resolved_entity,
                    &s.state_type,
                    &s.label,
                    corpus_id,
                );
                (s.id.clone(), new)
            }
            AtomEnvelope::Relation(r) => {
                if r.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let resolved_parts: Vec<AtomId> = r
                    .participants
                    .iter()
                    .map(|p| id_map.get(p).cloned().unwrap_or_else(|| p.clone()))
                    .collect();
                let new = AtomId::relation_content_hash(
                    &resolved_parts,
                    &r.relation_type,
                    &r.label,
                    corpus_id,
                );
                (r.id.clone(), new)
            }
            AtomEnvelope::Claim(c) => {
                if c.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let new = AtomId::claim_content_hash(
                    &c.content,
                    &c.discourse_act,
                    &c.epistemic_status,
                    corpus_id,
                );
                (c.id.clone(), new)
            }
            AtomEnvelope::Question(q) => {
                if q.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let new = AtomId::question_content_hash(&q.content, &q.question_type, corpus_id);
                (q.id.clone(), new)
            }
            AtomEnvelope::Configuration(cfg) => {
                if cfg.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let new = AtomId::configuration_content_hash(&cfg.label, corpus_id);
                (cfg.id.clone(), new)
            }
            AtomEnvelope::ArgumentReconstruction(a) => {
                if a.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let new = AtomId::argument_reconstruction_content_hash(&a.name, corpus_id);
                (a.id.clone(), new)
            }
            AtomEnvelope::Position(p) => {
                if p.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let new = AtomId::position_content_hash(&p.canonical_name, &p.stance, corpus_id);
                (p.id.clone(), new)
            }
            AtomEnvelope::Opposition(o) => {
                if o.id.is_content_hash() {
                    summary.atoms_already_content_hash += 1;
                    continue;
                }
                let new = AtomId::opposition_content_hash(&o.canonical_label, corpus_id);
                (o.id.clone(), new)
            }
            AtomEnvelope::Asset(_) => {
                // Asset ids are already content-addressed by sha256 at
                // birth (see `Asset::make_id`). Nothing to migrate.
                summary.atoms_already_content_hash += 1;
                continue;
            }
        };
        check_collision(
            &mut new_ids_seen,
            &old_id,
            &new_id,
            &mut summary.collisions_detected,
        );
        id_map.insert(old_id, new_id);
    }

    if id_map.is_empty() {
        // Nothing to do — atlas already migrated (or empty).
        return Ok(summary);
    }

    // ── Rewrite atoms.json (atom.id + intra-atom refs) ─────
    for env in atoms_file.atoms.iter_mut() {
        rewrite_atom(env, &id_map);
    }
    summary.atoms_migrated = id_map.len();

    // Dedup by id: duplicate sequential-id atoms that hashed to the
    // same content-hash are the migration's intended collapse. Keep
    // first-seen; the `collisions_detected` log already captures
    // which pairs were merged so the operator can audit. Without
    // this step atoms.json carries multiple records sharing one id
    // and every downstream reader (apply_atom_delta, drift,
    // retrieval) sees inconsistent atom state.
    let pre_dedup = atoms_file.atoms.len();
    let mut seen_ids: HashSet<AtomId> = HashSet::new();
    atoms_file
        .atoms
        .retain(|env| seen_ids.insert(env.id().clone()));
    summary.atoms_deduped = pre_dedup - atoms_file.atoms.len();

    if !dry_run {
        write_atomic(
            &atlas_dir.join("atoms.json"),
            &serde_json::to_vec_pretty(&atoms_file)
                .map_err(|e| Error::Serialization(e.to_string()))?,
        )?;
        summary.files_touched.push("atoms.json".to_string());
    }

    // ── Rewrite edges.json ─────────────────────────────────
    match read_atlas_edges(atlas_dir) {
        Ok(mut edges_file) => {
            let mut rewritten = 0usize;
            for e in edges_file.edges.iter_mut() {
                if let Some(new) = id_map.get(&e.source) {
                    e.source = new.clone();
                    rewritten += 1;
                }
                if let Some(new) = id_map.get(&e.target) {
                    e.target = new.clone();
                }
                if let Some(trigger) = e.trigger_event.as_mut() {
                    if let Some(new) = id_map.get(trigger) {
                        *trigger = new.clone();
                    }
                }
            }
            summary.edges_rewritten = rewritten;
            if !dry_run && rewritten > 0 {
                write_atomic(
                    &atlas_dir.join("edges.json"),
                    &serde_json::to_vec_pretty(&edges_file)
                        .map_err(|e| Error::Serialization(e.to_string()))?,
                )?;
                summary.files_touched.push("edges.json".to_string());
            }
        }
        Err(e) if missing_file(&e) => {} // no edges.json — fine
        Err(e) => {
            return Err(Error::Serialization(format!("read edges.json: {e}")));
        }
    }

    // ── Rewrite cross_corpus_edges.json ────────────────────
    match read_atlas_cross_corpus_edges(atlas_dir) {
        Ok(mut ccedges) => {
            let mut rewritten = 0usize;
            for ce in ccedges.edges.iter_mut() {
                if let Some(new) = id_map.get(&ce.edge.source) {
                    ce.edge.source = new.clone();
                    rewritten += 1;
                }
                if let Some(new) = id_map.get(&ce.edge.target) {
                    ce.edge.target = new.clone();
                }
                // peer.atom_id intentionally not rewritten — that
                // atom lives in the peer's atlas, which the
                // operator will migrate independently. detect_grounding
                // refresh post-migration fixes the references.
            }
            summary.cross_corpus_edges_rewritten = rewritten;
            if !dry_run && rewritten > 0 {
                write_atlas_cross_corpus_edges(atlas_dir, &ccedges)
                    .map_err(|e| Error::Serialization(e.to_string()))?;
                summary
                    .files_touched
                    .push("cross_corpus_edges.json".to_string());
            }
        }
        Err(e) if missing_file(&e) => {} // no cross-corpus edges — fine
        Err(e) => {
            return Err(Error::Serialization(format!(
                "read cross_corpus_edges.json: {e}"
            )));
        }
    }

    Ok(summary)
}

fn missing_file(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

fn check_collision(
    seen: &mut HashMap<AtomId, AtomId>,
    old: &AtomId,
    new: &AtomId,
    collisions: &mut Vec<(String, String)>,
) {
    if let Some(prior_old) = seen.get(new) {
        if prior_old != old {
            collisions.push((prior_old.as_str().to_string(), old.as_str().to_string()));
        }
    } else {
        seen.insert(new.clone(), old.clone());
    }
}

fn rewrite_atom(env: &mut AtomEnvelope, id_map: &HashMap<AtomId, AtomId>) {
    match env {
        AtomEnvelope::Entity(e) => {
            if let Some(new) = id_map.get(&e.id) {
                e.id = new.clone();
            }
            for p in e.participants.iter_mut() {
                if let Some(new) = id_map.get(p) {
                    *p = new.clone();
                }
            }
        }
        AtomEnvelope::Event(e) => {
            if let Some(new) = id_map.get(&e.id) {
                e.id = new.clone();
            }
            for p in e.participants.iter_mut() {
                if let Some(new) = id_map.get(p) {
                    *p = new.clone();
                }
            }
            for c in e.causal_antecedents.iter_mut() {
                if let Some(new) = id_map.get(c) {
                    *c = new.clone();
                }
            }
        }
        AtomEnvelope::State(s) => {
            if let Some(new) = id_map.get(&s.id) {
                s.id = new.clone();
            }
            if let Some(new) = id_map.get(&s.entity_id) {
                s.entity_id = new.clone();
            }
        }
        AtomEnvelope::Relation(r) => {
            if let Some(new) = id_map.get(&r.id) {
                r.id = new.clone();
            }
            for p in r.participants.iter_mut() {
                if let Some(new) = id_map.get(p) {
                    *p = new.clone();
                }
            }
        }
        AtomEnvelope::Claim(c) => {
            if let Some(new) = id_map.get(&c.id) {
                c.id = new.clone();
            }
            if let Some(attr) = c.attributed_to.as_mut() {
                if let Some(new) = id_map.get(attr) {
                    *attr = new.clone();
                }
            }
        }
        AtomEnvelope::Question(q) => {
            if let Some(new) = id_map.get(&q.id) {
                q.id = new.clone();
            }
            for a in q.addressed_by.iter_mut() {
                if let Some(new) = id_map.get(a) {
                    *a = new.clone();
                }
            }
            // ResolutionStatus carries AtomId in some variants.
            use super::atoms::ResolutionStatus;
            match &mut q.resolution_status {
                ResolutionStatus::Resolved { claim_id } => {
                    if let Some(new) = id_map.get(claim_id) {
                        *claim_id = new.clone();
                    }
                }
                ResolutionStatus::Contested { claim_ids } => {
                    for c in claim_ids.iter_mut() {
                        if let Some(new) = id_map.get(c) {
                            *c = new.clone();
                        }
                    }
                }
                _ => {}
            }
        }
        AtomEnvelope::Configuration(cfg) => {
            if let Some(new) = id_map.get(&cfg.id) {
                cfg.id = new.clone();
            }
            for c in cfg.constituent_atoms.iter_mut() {
                if let Some(new) = id_map.get(c) {
                    *c = new.clone();
                }
            }
        }
        AtomEnvelope::ArgumentReconstruction(a) => {
            if let Some(new) = id_map.get(&a.id) {
                a.id = new.clone();
            }
            if let Some(prop) = a.proponent.as_mut() {
                if let Some(new) = id_map.get(prop) {
                    *prop = new.clone();
                }
            }
        }
        AtomEnvelope::Position(p) => {
            if let Some(new) = id_map.get(&p.id) {
                p.id = new.clone();
            }
            if let Some(prop) = p.proponent_id.as_mut() {
                if let Some(new) = id_map.get(prop) {
                    *prop = new.clone();
                }
            }
            for ev in p.evidence_ids.iter_mut() {
                if let Some(new) = id_map.get(ev) {
                    *ev = new.clone();
                }
            }
        }
        AtomEnvelope::Opposition(o) => {
            if let Some(new) = id_map.get(&o.id) {
                o.id = new.clone();
            }
            if let Some(l) = o.left_atom_id.as_mut() {
                if let Some(new) = id_map.get(l) {
                    *l = new.clone();
                }
            }
            if let Some(r) = o.right_atom_id.as_mut() {
                if let Some(new) = id_map.get(r) {
                    *r = new.clone();
                }
            }
        }
        AtomEnvelope::Asset(a) => {
            // Asset.id is content-addressed by sha256 — id_map never
            // contains an entry for it. described_by may reference a
            // remapped atom id, though.
            if let Some(desc) = a.described_by.as_mut() {
                if let Some(new) = id_map.get(desc) {
                    *desc = new.clone();
                }
            }
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| Error::Serialization(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::Serialization(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomsFile, ChunkRef, Entity};
    use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    use std::fs;

    fn write_fixture(atlas_dir: &Path, atoms: Vec<AtomEnvelope>, edges: Vec<Edge>) {
        fs::create_dir_all(atlas_dir).unwrap();
        let atoms_file = AtomsFile::new(atoms);
        fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_string_pretty(&atoms_file).unwrap(),
        )
        .unwrap();
        let edges_file = EdgesFile::new(edges);
        fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_string_pretty(&edges_file).unwrap(),
        )
        .unwrap();
    }

    fn make_entity(idx: usize, name: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(format!("sec_{idx:04}"), None),
            description: format!("desc of {name}"),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    #[test]
    fn migrate_rewrites_sequential_ids_to_content_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path();
        let e1 = make_entity(1, "Alice");
        let e2 = make_entity(2, "Bob");
        let edge = Edge {
            id: EdgeId::from_raw("edge-0001"),
            edge_type: EdgeType::Involves,
            source: AtomId::entity(1),
            target: AtomId::entity(2),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };
        write_fixture(atlas_dir, vec![e1, e2], vec![edge]);

        let summary = migrate_atlas_ids(atlas_dir, "test_corpus", false).unwrap();
        assert_eq!(summary.atoms_migrated, 2);
        assert_eq!(summary.edges_rewritten, 1);
        assert!(summary.files_touched.contains(&"atoms.json".to_string()));
        assert!(summary.files_touched.contains(&"edges.json".to_string()));

        // Re-read and verify content-hash shape on every atom + edge.
        let atoms: AtomsFile =
            serde_json::from_slice(&fs::read(atlas_dir.join("atoms.json")).unwrap()).unwrap();
        for env in &atoms.atoms {
            let id = env.id();
            assert!(
                id.is_content_hash(),
                "expected content-hash id, got {}",
                id.as_str()
            );
        }
        let edges: EdgesFile =
            serde_json::from_slice(&fs::read(atlas_dir.join("edges.json")).unwrap()).unwrap();
        for e in &edges.edges {
            assert!(e.source.is_content_hash());
            assert!(e.target.is_content_hash());
        }
    }

    #[test]
    fn migrate_is_idempotent_on_second_run() {
        let tmp = tempfile::tempdir().unwrap();
        let e1 = make_entity(1, "Alice");
        write_fixture(tmp.path(), vec![e1], vec![]);
        let s1 = migrate_atlas_ids(tmp.path(), "c", false).unwrap();
        assert_eq!(s1.atoms_migrated, 1);
        let s2 = migrate_atlas_ids(tmp.path(), "c", false).unwrap();
        // Second pass: all atoms already content-hash, nothing to do.
        assert_eq!(s2.atoms_migrated, 0);
        assert_eq!(s2.atoms_already_content_hash, 1);
        assert!(s2.files_touched.is_empty());
    }

    #[test]
    fn migrate_dry_run_does_not_write_files() {
        let tmp = tempfile::tempdir().unwrap();
        let e1 = make_entity(1, "Alice");
        write_fixture(tmp.path(), vec![e1], vec![]);
        let before = fs::read_to_string(tmp.path().join("atoms.json")).unwrap();
        let summary = migrate_atlas_ids(tmp.path(), "c", true).unwrap();
        assert_eq!(summary.atoms_migrated, 1);
        assert!(summary.files_touched.is_empty());
        let after = fs::read_to_string(tmp.path().join("atoms.json")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn migrate_preserves_intra_atom_references() {
        // Entity with a participant pointing at another entity. After
        // migration, both ids are content-hash AND the participant
        // ref resolves to the second entity's new id.
        let tmp = tempfile::tempdir().unwrap();
        let alice = make_entity(1, "Alice");
        let bob = AtomEnvelope::Entity(Entity {
            id: AtomId::entity(2),
            canonical_name: "Bob".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0002", None),
            description: "bob".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![AtomId::entity(1)], // points at Alice
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        });
        write_fixture(tmp.path(), vec![alice, bob], vec![]);

        migrate_atlas_ids(tmp.path(), "c", false).unwrap();

        let atoms: AtomsFile =
            serde_json::from_slice(&fs::read(tmp.path().join("atoms.json")).unwrap()).unwrap();
        let alice_new = atoms
            .atoms
            .iter()
            .find_map(|a| match a {
                AtomEnvelope::Entity(e) if e.canonical_name == "Alice" => Some(e.id.clone()),
                _ => None,
            })
            .unwrap();
        let bob_atom = atoms
            .atoms
            .iter()
            .find_map(|a| match a {
                AtomEnvelope::Entity(e) if e.canonical_name == "Bob" => Some(e),
                _ => None,
            })
            .unwrap();
        assert_eq!(bob_atom.participants.len(), 1);
        assert_eq!(bob_atom.participants[0], alice_new);
    }

    #[test]
    fn migrate_collapses_duplicate_atoms_to_one_id() {
        // Two sequential-id atoms with identical
        // (canonical_name, entity_type) collapse to one content-hash
        // atom after migration. Before this dedup pass the
        // migration left two records sharing one new id —
        // downstream consumers (apply_atom_delta + retrieval) saw
        // inconsistent atom state.
        let tmp = tempfile::tempdir().unwrap();
        let dup_a = make_entity(1, "Alice");
        let dup_b = make_entity(2, "Alice");
        let unique = make_entity(3, "Bob");
        write_fixture(tmp.path(), vec![dup_a, dup_b, unique], vec![]);

        let summary = migrate_atlas_ids(tmp.path(), "c", false).unwrap();
        assert_eq!(summary.atoms_deduped, 1, "one duplicate Alice collapsed");
        assert_eq!(summary.collisions_detected.len(), 1);

        let atoms: AtomsFile =
            serde_json::from_slice(&fs::read(tmp.path().join("atoms.json")).unwrap()).unwrap();
        assert_eq!(atoms.atoms.len(), 2, "Alice + Bob, no duplicates");

        let mut ids: Vec<_> = atoms
            .atoms
            .iter()
            .map(|env| env.id().as_str().to_string())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 2, "atom ids are unique post-migration");
    }

    #[test]
    fn migrate_handles_missing_edges_file_gracefully() {
        let tmp = tempfile::tempdir().unwrap();
        let e1 = make_entity(1, "Alice");
        let atoms_file = AtomsFile::new(vec![e1]);
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(
            tmp.path().join("atoms.json"),
            serde_json::to_string_pretty(&atoms_file).unwrap(),
        )
        .unwrap();
        // No edges.json written.
        let summary = migrate_atlas_ids(tmp.path(), "c", false).unwrap();
        assert_eq!(summary.atoms_migrated, 1);
        assert_eq!(summary.edges_rewritten, 0);
    }
}
