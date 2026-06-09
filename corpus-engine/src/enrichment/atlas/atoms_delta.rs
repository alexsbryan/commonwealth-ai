// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atoms-delta primitive — Move 6 Phase 2.
//!
//! `apply_atom_delta(atlas_dir, delta)` mutates an atlas's on-disk
//! state by adding new atoms, removing the atom set produced by
//! specified docs, or replacing per-doc atom sets atomically.
//!
//! This is the lever every Phase 5 source-side hook (newsworthy
//! refresh, watched-folder edit, wiki delta-ingest) eventually
//! calls. Per-doc incremental updates replace today's full
//! atlas-rebuild cost.
//!
//! ## Files mutated
//!
//! `atoms.json` (source of truth) plus three derived sidecars:
//!   - `doc_to_atoms.json` (doc → atoms ownership)
//!   - `edges.json` (edges whose endpoints reference removed atoms
//!     are pruned)
//!   - `cross_corpus_edges.json` (same for local-side endpoints;
//!     `peer.atom_id` references are left for post-delta
//!     `detect_grounding` refresh — same contract as the
//!     content-hash migration in Phase 0)
//!
//! ## Atomicity contract
//!
//! Best-effort per-file atomic writes via `<file>.tmp` + rename.
//! The four files are renamed in order: atoms.json (canonical) →
//! doc_to_atoms.json → edges.json → cross_corpus_edges.json. If the
//! process is killed mid-rename, the next read sees a state where
//! atoms.json reflects the new shape but a derived sidecar may
//! lag. Sidecar staleness is recoverable: re-running `sovereign
//! atlas build-doc-index` re-derives `doc_to_atoms.json` from
//! atoms.json; re-running the edge detector regenerates edges
//! from atom evidence. Multi-file atomic-via-journal is deferred
//! to a future Move if the recovery contract turns out to be
//! insufficient.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::error::{Error, Result};

use super::atoms::{AtomEnvelope, AtomId};
use super::doc_to_atoms::{self, DocToAtomsFile};

/// Delta description. All four lists may be empty; `apply_atom_delta`
/// is a no-op when the delta is empty.
#[derive(Debug, Clone, Default)]
pub struct AtomsDelta {
    /// Net-new atoms to append. The caller is responsible for the
    /// atom's doc_id being derivable via
    /// [`doc_to_atoms::extract_doc_id`]; otherwise the atom is
    /// added to atoms.json but not registered with any doc (and so
    /// won't be removed by later delta passes).
    pub added: Vec<AtomEnvelope>,
    /// Drop all atoms produced by these docs. Used when a source
    /// document is deleted (e.g. wiki newsworthy article rotated
    /// out of the tracked window).
    pub removed_doc_ids: Vec<String>,
    /// Replace the atom set for these docs. Each tuple is
    /// `(doc_id, new_atoms)`. The doc's existing atoms are dropped
    /// and the new atoms inserted in the same pass. Used for the
    /// re-extract case (wiki newsworthy refresh, vault file edit).
    pub upserted_docs: Vec<(String, Vec<AtomEnvelope>)>,
    /// Edges to insert. `apply_atom_delta` first drops every edge
    /// whose source/target references a dropped atom, then inserts
    /// these. Caller is responsible for the new edges' endpoints
    /// referencing atoms either present in atoms.json post-apply or
    /// in this delta's `added`/`upserted_docs`. Used by structural
    /// pipelines (P3) where per-doc re-extraction emits both
    /// atoms and the Involves edges between them.
    pub added_edges: Vec<super::edges::Edge>,
}

impl AtomsDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed_doc_ids.is_empty()
            && self.upserted_docs.is_empty()
            && self.added_edges.is_empty()
    }
}

/// Observability counts. Returned by [`apply_atom_delta`] and
/// logged by Phase 5 hooks.
#[derive(Debug, Clone, Default)]
pub struct DeltaSummary {
    pub atoms_before: usize,
    pub atoms_after: usize,
    pub atoms_added: usize,
    pub atoms_removed: usize,
    pub docs_removed: usize,
    pub docs_upserted: usize,
    pub edges_dropped: usize,
    pub cross_corpus_edges_dropped: usize,
    pub files_touched: Vec<String>,
}

/// Apply `delta` to the atlas at `atlas_dir`. Reads + rewrites
/// atoms.json, doc_to_atoms.json, edges.json,
/// cross_corpus_edges.json (when present).
///
/// Returns counts for observability. Empty delta is a no-op (returns
/// `Ok` with zeros, no files touched).
pub fn apply_atom_delta(atlas_dir: &Path, delta: AtomsDelta) -> Result<DeltaSummary> {
    let mut summary = DeltaSummary::default();
    if delta.is_empty() {
        return Ok(summary);
    }

    // ── Load current state ─────────────────────────────────
    let mut atoms_file = super::writer::read_atlas_atoms(atlas_dir)
        .map_err(|e| Error::Serialization(format!("read atoms.json: {e}")))?;
    summary.atoms_before = atoms_file.atoms.len();

    let mut doc_index = doc_to_atoms::read(atlas_dir)
        .map_err(|e| Error::Serialization(format!("read doc_to_atoms.json: {e}")))?
        .unwrap_or_else(DocToAtomsFile::new);

    // ── Compute affected atom_ids ──────────────────────────
    // Union of: atoms in removed_doc_ids + atoms in upserted_docs.
    let mut atoms_to_drop: HashSet<AtomId> = HashSet::new();
    for doc_id in &delta.removed_doc_ids {
        for id in doc_index.atoms_for(doc_id) {
            atoms_to_drop.insert(id.clone());
        }
        summary.docs_removed += 1;
    }
    for (doc_id, _) in &delta.upserted_docs {
        for id in doc_index.atoms_for(doc_id) {
            atoms_to_drop.insert(id.clone());
        }
        summary.docs_upserted += 1;
    }

    // ── Apply atoms.json mutations ─────────────────────────
    // Drop affected atoms.
    let pre_len = atoms_file.atoms.len();
    atoms_file
        .atoms
        .retain(|env| !atoms_to_drop.contains(env.id()));
    summary.atoms_removed = pre_len - atoms_file.atoms.len();

    // Insert added + upserted atoms. Dedup by id (content-hash means
    // re-extracting the same conceptual atom produces the same id;
    // overwrite with the new atom shape rather than duplicate).
    let mut new_atoms: HashMap<AtomId, AtomEnvelope> = HashMap::new();
    for env in delta.added {
        new_atoms.insert(env.id().clone(), env);
    }
    for (_, atoms) in &delta.upserted_docs {
        for env in atoms {
            new_atoms.insert(env.id().clone(), env.clone());
        }
    }
    // Replace in-place if already present (same content-hash id);
    // otherwise append.
    let mut by_id: HashMap<AtomId, usize> = HashMap::new();
    for (idx, env) in atoms_file.atoms.iter().enumerate() {
        by_id.insert(env.id().clone(), idx);
    }
    let mut appended = 0usize;
    for (id, env) in new_atoms {
        if let Some(&idx) = by_id.get(&id) {
            atoms_file.atoms[idx] = env;
        } else {
            atoms_file.atoms.push(env);
            appended += 1;
        }
    }
    summary.atoms_added = appended;
    summary.atoms_after = atoms_file.atoms.len();

    // ── Rebuild doc index ──────────────────────────────────
    // Rebuilding from scratch is O(atoms) — same cost as walking
    // the delta but simpler invariant. The atlas builder paths
    // already do this; the partial-rebuild optimisation lives in
    // Phase 7.
    doc_index = doc_to_atoms::build_from_atoms_file(&atoms_file);

    // ── Persist atoms.json + doc_to_atoms.json ─────────────
    write_atomic(
        &atlas_dir.join("atoms.json"),
        &serde_json::to_vec_pretty(&atoms_file)
            .map_err(|e| Error::Serialization(format!("serialise atoms.json: {e}")))?,
    )?;
    summary.files_touched.push("atoms.json".to_string());

    doc_to_atoms::write(atlas_dir, &doc_index)
        .map_err(|e| Error::Serialization(format!("write doc_to_atoms.json: {e}")))?;
    summary
        .files_touched
        .push(doc_to_atoms::DOC_TO_ATOMS_FILENAME.to_string());

    // ── Edges.json: drop dead edges + insert new edges ────
    let need_edge_pass = !atoms_to_drop.is_empty() || !delta.added_edges.is_empty();
    if need_edge_pass {
        let mut edges_file = match super::writer::read_atlas_edges(atlas_dir) {
            Ok(f) => f,
            Err(e) if missing_file(&e) => super::edges::EdgesFile::new(Vec::new()),
            Err(e) => {
                return Err(Error::Serialization(format!("read edges.json: {e}")));
            }
        };
        let pre = edges_file.edges.len();
        edges_file
            .edges
            .retain(|e| !atoms_to_drop.contains(&e.source) && !atoms_to_drop.contains(&e.target));
        summary.edges_dropped = pre - edges_file.edges.len();

        let mut edges_added = 0usize;
        let mut edges_replaced = 0usize;
        if !delta.added_edges.is_empty() {
            // Dedup by edge.id when re-inserting (delta might
            // include edges whose ids collide with surviving
            // ones — overwrite to keep the new metadata).
            let mut by_id: std::collections::HashMap<super::edges::EdgeId, usize> =
                std::collections::HashMap::new();
            for (i, e) in edges_file.edges.iter().enumerate() {
                by_id.insert(e.id.clone(), i);
            }
            for edge in delta.added_edges.iter() {
                if let Some(&idx) = by_id.get(&edge.id) {
                    edges_file.edges[idx] = edge.clone();
                    edges_replaced += 1;
                } else {
                    edges_file.edges.push(edge.clone());
                    edges_added += 1;
                }
            }
        }

        let touched = summary.edges_dropped > 0 || edges_added > 0 || edges_replaced > 0;
        if touched {
            write_atomic(
                &atlas_dir.join("edges.json"),
                &serde_json::to_vec_pretty(&edges_file)
                    .map_err(|e| Error::Serialization(format!("serialise edges.json: {e}")))?,
            )?;
            summary.files_touched.push("edges.json".to_string());
        }

        match super::writer::read_atlas_cross_corpus_edges(atlas_dir) {
            Ok(mut ccedges) => {
                let pre = ccedges.edges.len();
                ccedges.edges.retain(|ce| {
                    !atoms_to_drop.contains(&ce.edge.source)
                        && !atoms_to_drop.contains(&ce.edge.target)
                });
                summary.cross_corpus_edges_dropped = pre - ccedges.edges.len();
                if summary.cross_corpus_edges_dropped > 0 {
                    super::writer::write_atlas_cross_corpus_edges(atlas_dir, &ccedges).map_err(
                        |e| Error::Serialization(format!("write cross_corpus_edges.json: {e}")),
                    )?;
                    summary
                        .files_touched
                        .push("cross_corpus_edges.json".to_string());
                }
            }
            Err(e) if missing_file(&e) => {}
            Err(e) => {
                return Err(Error::Serialization(format!(
                    "read cross_corpus_edges.json: {e}"
                )));
            }
        }
    }

    Ok(summary)
}

fn missing_file(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
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

    fn make_entity(name: &str, chunk_id: &str, corpus: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity_content_hash(name, &EntityType::Person, corpus),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(chunk_id, None),
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

    fn seed(atlas_dir: &Path, atoms: Vec<AtomEnvelope>, edges: Vec<Edge>) {
        fs::create_dir_all(atlas_dir).unwrap();
        let af = AtomsFile::new(atoms);
        fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_string_pretty(&af).unwrap(),
        )
        .unwrap();
        let ef = EdgesFile::new(edges);
        fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_string_pretty(&ef).unwrap(),
        )
        .unwrap();
        doc_to_atoms::build_and_write(atlas_dir).unwrap();
    }

    fn read_atoms(atlas_dir: &Path) -> AtomsFile {
        let raw = fs::read(atlas_dir.join("atoms.json")).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }

    fn read_edges(atlas_dir: &Path) -> EdgesFile {
        let raw = fs::read(atlas_dir.join("edges.json")).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }

    #[test]
    fn empty_delta_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), vec![make_entity("A", "doc_a", "c")], vec![]);
        let s = apply_atom_delta(tmp.path(), AtomsDelta::default()).unwrap();
        assert_eq!(s.atoms_added, 0);
        assert_eq!(s.atoms_removed, 0);
        assert!(s.files_touched.is_empty());
    }

    #[test]
    fn added_atoms_append_to_atoms_file() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), vec![make_entity("A", "doc_a", "c")], vec![]);
        let delta = AtomsDelta {
            added: vec![make_entity("B", "doc_b", "c")],
            ..Default::default()
        };
        let s = apply_atom_delta(tmp.path(), delta).unwrap();
        assert_eq!(s.atoms_added, 1);
        assert_eq!(s.atoms_before, 1);
        assert_eq!(s.atoms_after, 2);
        let after = read_atoms(tmp.path());
        assert_eq!(after.atoms.len(), 2);
    }

    #[test]
    fn removed_doc_drops_its_atoms_and_their_edges() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_entity("A", "doc_a", "c");
        let b = make_entity("B", "doc_b", "c");
        let a_id = a.id().clone();
        let b_id = b.id().clone();
        let edge = Edge {
            id: EdgeId::from_raw("e1"),
            edge_type: EdgeType::Involves,
            source: a_id,
            target: b_id,
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };
        seed(tmp.path(), vec![a, b], vec![edge]);

        let delta = AtomsDelta {
            removed_doc_ids: vec!["doc_a".to_string()],
            ..Default::default()
        };
        let s = apply_atom_delta(tmp.path(), delta).unwrap();
        assert_eq!(s.atoms_removed, 1);
        assert_eq!(s.docs_removed, 1);
        assert_eq!(s.edges_dropped, 1); // edge referenced doc_a's atom
        let after_atoms = read_atoms(tmp.path());
        assert_eq!(after_atoms.atoms.len(), 1);
        assert_eq!(after_atoms.atoms[0].id().as_str(), {
            match &after_atoms.atoms[0] {
                AtomEnvelope::Entity(e) => e.id.as_str(),
                _ => unreachable!(),
            }
        });
        let after_edges = read_edges(tmp.path());
        assert!(after_edges.edges.is_empty());
    }

    #[test]
    fn upsert_replaces_doc_atoms() {
        let tmp = tempfile::tempdir().unwrap();
        // Initially doc_a produces "Alice"; after upsert it produces
        // "Alice2" (different canonical_name → different content-hash
        // id; old Alice atom is retired).
        seed(tmp.path(), vec![make_entity("Alice", "doc_a", "c")], vec![]);

        let delta = AtomsDelta {
            upserted_docs: vec![(
                "doc_a".to_string(),
                vec![make_entity("Alice2", "doc_a", "c")],
            )],
            ..Default::default()
        };
        let s = apply_atom_delta(tmp.path(), delta).unwrap();
        assert_eq!(s.atoms_removed, 1);
        assert_eq!(s.atoms_added, 1);
        assert_eq!(s.docs_upserted, 1);
        let after = read_atoms(tmp.path());
        assert_eq!(after.atoms.len(), 1);
        match &after.atoms[0] {
            AtomEnvelope::Entity(e) => assert_eq!(e.canonical_name, "Alice2"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn upsert_same_atom_is_idempotent() {
        // Re-extracting a doc that produces the same conceptual atom
        // (same canonical_name+entity_type+corpus) yields the same
        // content-hash id → no churn. The atom is replaced in place
        // but ids stay stable.
        let tmp = tempfile::tempdir().unwrap();
        let alice_a = make_entity("Alice", "doc_a", "c");
        let alice_a_id = alice_a.id().clone();
        seed(tmp.path(), vec![alice_a.clone()], vec![]);

        let delta = AtomsDelta {
            upserted_docs: vec![("doc_a".to_string(), vec![alice_a])],
            ..Default::default()
        };
        let s = apply_atom_delta(tmp.path(), delta).unwrap();
        // Counters track raw drop + insert (1+1); net atoms_after
        // unchanged. The id stays stable across the round-trip
        // (content-hash invariant).
        assert_eq!(s.atoms_removed, 1);
        assert_eq!(s.atoms_added, 1);
        assert_eq!(s.atoms_after, 1);
        let after = read_atoms(tmp.path());
        assert_eq!(after.atoms[0].id(), &alice_a_id);
    }

    #[test]
    fn doc_to_atoms_sidecar_stays_in_sync() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), vec![make_entity("A", "doc_a", "c")], vec![]);

        let delta = AtomsDelta {
            added: vec![make_entity("B", "doc_b", "c")],
            ..Default::default()
        };
        apply_atom_delta(tmp.path(), delta).unwrap();

        let sidecar = doc_to_atoms::read(tmp.path()).unwrap().unwrap();
        assert_eq!(sidecar.len(), 2);
        assert_eq!(sidecar.atoms_for("doc_a").len(), 1);
        assert_eq!(sidecar.atoms_for("doc_b").len(), 1);
    }

    #[test]
    fn missing_edges_file_tolerated() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed atoms.json + sidecar but no edges.json.
        let af = AtomsFile::new(vec![make_entity("A", "doc_a", "c")]);
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(
            tmp.path().join("atoms.json"),
            serde_json::to_string_pretty(&af).unwrap(),
        )
        .unwrap();
        doc_to_atoms::build_and_write(tmp.path()).unwrap();

        let delta = AtomsDelta {
            removed_doc_ids: vec!["doc_a".to_string()],
            ..Default::default()
        };
        let s = apply_atom_delta(tmp.path(), delta).unwrap();
        assert_eq!(s.atoms_removed, 1);
        assert_eq!(s.edges_dropped, 0);
    }

    #[test]
    fn added_edges_merge_into_edges_file() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_entity("A", "doc_a", "c");
        let b = make_entity("B", "doc_b", "c");
        let a_id = a.id().clone();
        let b_id = b.id().clone();
        seed(tmp.path(), vec![a, b], vec![]);

        let new_edge = Edge {
            id: EdgeId::from_raw("new-edge-1"),
            edge_type: EdgeType::Involves,
            source: a_id,
            target: b_id,
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };
        let delta = AtomsDelta {
            added_edges: vec![new_edge.clone()],
            ..Default::default()
        };
        apply_atom_delta(tmp.path(), delta).unwrap();
        let edges = read_edges(tmp.path());
        assert_eq!(edges.edges.len(), 1);
        assert_eq!(edges.edges[0].id, new_edge.id);
    }

    #[test]
    fn added_edges_replace_existing_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_entity("A", "doc_a", "c");
        let b = make_entity("B", "doc_b", "c");
        let a_id = a.id().clone();
        let b_id = b.id().clone();
        let original = Edge {
            id: EdgeId::from_raw("e1"),
            edge_type: EdgeType::Involves,
            source: a_id.clone(),
            target: b_id.clone(),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 0.5,
            provenance: EdgeProvenance::Derived,
        };
        seed(tmp.path(), vec![a, b], vec![original]);

        // Same edge id, new confidence — should overwrite.
        let replacement = Edge {
            id: EdgeId::from_raw("e1"),
            edge_type: EdgeType::Involves,
            source: a_id,
            target: b_id,
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };
        let delta = AtomsDelta {
            added_edges: vec![replacement],
            ..Default::default()
        };
        apply_atom_delta(tmp.path(), delta).unwrap();
        let edges = read_edges(tmp.path());
        assert_eq!(edges.edges.len(), 1);
        assert!((edges.edges[0].confidence - 1.0).abs() < 1e-6);
    }

    #[test]
    fn upsert_preserves_unrelated_doc_atoms() {
        let tmp = tempfile::tempdir().unwrap();
        seed(
            tmp.path(),
            vec![
                make_entity("Alice", "doc_a", "c"),
                make_entity("Bob", "doc_b", "c"),
            ],
            vec![],
        );

        let delta = AtomsDelta {
            upserted_docs: vec![("doc_a".into(), vec![make_entity("Alice2", "doc_a", "c")])],
            ..Default::default()
        };
        apply_atom_delta(tmp.path(), delta).unwrap();

        let after = read_atoms(tmp.path());
        assert_eq!(after.atoms.len(), 2);
        let names: Vec<String> = after
            .atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Entity(e) => Some(e.canonical_name.clone()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"Alice2".to_string()));
        assert!(names.contains(&"Bob".to_string()));
    }
}
