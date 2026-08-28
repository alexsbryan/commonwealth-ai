// SPDX-License-Identifier: AGPL-3.0-or-later
//! Meta-atlas builder — walks every installed atlas, per-atom
//! classifies articulation, clusters by `lookup_key`, persists to
//! `~/.svrnmesh/meta-atlas/canonical_atoms.json`.
//!
//! Move 5 Stage 3.
//!
//! Reuses:
//!   - [`crate::atlas_canonical::lookup_key`] for the normalised key.
//!   - [`crate::enrichment::atlas::read_atlas_atoms`] for atom I/O.
//!   - [`crate::enrichment::atlas::atoms_content_hash`] for staleness.
//!   - [`super::classifier::classify_articulation`] for per-atom axis.
//!
//! Persistence shape lives in [`MetaAtlasFile`]. Atomic-write via
//! `.tmp` + rename.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atlas_canonical::lookup_key;
use crate::enrichment::atlas::{
    atoms_content_hash, read_atlas_atoms, AtomEnvelope, AtomId, ChunkRef, ATLAS_DIRNAME,
};
use crate::enrichment::pipeline::atlas::EntityType;
use crate::stream_axes::{ArticulationVector, Stability, StreamAxes};

/// One MetaAtom = one canonical-name equivalence class across the
/// installed atlases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaAtom {
    pub canonical_key: String,
    pub display: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub aliases: BTreeSet<String>,
    pub anchors: Vec<Anchor>,
}

/// One per (atlas, atom) the meta-atom is attested in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub corpus_id: String,
    pub atom_id: AtomId,
    pub primary_chunk: ChunkRef,
    pub articulation: ArticulationVector,
    /// `None` when the atlas's owning corpus had no `stream` block in
    /// its `_corpus_meta.json` at build time. `sovereign corpus
    /// stream-axes` is the backfill path; meta-atlas writes the
    /// anchor anyway so retrieval still gets the per-atom
    /// articulation tag.
    pub stability: Option<Stability>,
    pub salience: f32,
    pub atlas_content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSeen {
    pub corpus_id: String,
    pub content_hash: String,
    pub eligible_entities: usize,
    pub stability: Option<Stability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaAtlasFile {
    pub schema_version: String,
    pub built_at: u64,
    pub atlases_seen: Vec<AtlasSeen>,
    pub atoms: Vec<MetaAtom>,
}

impl MetaAtlasFile {
    pub const SCHEMA_VERSION: &'static str = "1.0";
}

/// Default path the persisted meta-atlas lives at.
pub fn default_meta_atlas_path() -> Option<PathBuf> {
    Some(
        sovereign_contracts::rebrand::svrnmesh_root()
            .join("meta-atlas")
            .join("canonical_atoms.json"),
    )
}

/// Build the meta-atlas by walking every installed atlas.
///
/// `indexes_dir` is the root directory containing per-corpus dirs
/// (e.g. `~/.svrnmesh/indexes/`). The function looks for
/// `<indexes_dir>/<corpus>/atlas/atoms.json` files and reads
/// `<indexes_dir>/<corpus>/_corpus_meta.json` for per-corpus
/// stability. Corpora that have an atlas but no `_corpus_meta.json`
/// (atlas-only sibling dirs, e.g. obsidian-vault's atlas without the
/// watched-folder chunk corpus next to it) are included with
/// `stability = None`.
///
/// Atom filtering: only the `Entity` variant participates in the
/// canonical-name clustering. Other atom variants (Claim, Event, …)
/// are classified per-atom but don't contribute anchors since they
/// lack a meaningful `canonical_key`. Future Moves can broaden if a
/// use case emerges.
pub fn build_meta_atlas(indexes_dir: &Path) -> std::io::Result<MetaAtlasFile> {
    let mut atlases_seen: Vec<AtlasSeen> = Vec::new();
    // Cluster keyed by canonical_key → anchors collected before
    // sorting + emission.
    let mut clusters: HashMap<String, Cluster> = HashMap::new();

    if !indexes_dir.is_dir() {
        return Ok(MetaAtlasFile {
            schema_version: MetaAtlasFile::SCHEMA_VERSION.to_string(),
            built_at: crate::stream_axes::timestamp_now(),
            atlases_seen,
            atoms: Vec::new(),
        });
    }

    for entry in std::fs::read_dir(indexes_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "meta_atlas: read_dir entry failed");
                continue;
            }
        };
        let corpus_path = entry.path();
        if !corpus_path.is_dir() {
            continue;
        }
        let corpus_id = match corpus_path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.starts_with('.') && !n.starts_with('_') => n.to_string(),
            _ => continue,
        };
        let atlas_dir = corpus_path.join(ATLAS_DIRNAME);
        if !atlas_dir.is_dir() {
            continue;
        }
        let atoms_file = match read_atlas_atoms(&atlas_dir) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(corpus = %corpus_id, error = %e, "meta_atlas: read_atlas_atoms failed");
                continue;
            }
        };
        let content_hash = atoms_content_hash(&atlas_dir)
            .unwrap_or_else(|e| {
                tracing::warn!(corpus = %corpus_id, error = %e, "meta_atlas: atoms_content_hash failed");
                String::new()
            });

        // Per-corpus stream block (stability) lives in
        // `_corpus_meta.json`. Missing → None (atlas-sibling case).
        let stability = read_corpus_stability(&corpus_path);

        // Move 5.1 — drop Rolling-stability corpora from anchoring.
        // Rolling = continuously-updated within a window
        // (conversation-history, codex-session, newsworthy event
        // stream). Those are practice-stream sources; they should
        // NEVER anchor a canonical-entity lookup ("Einstein" should
        // not resolve to whatever the user typed about Einstein last
        // week). Filter at build time so the resulting
        // `canonical_atoms.json` is clean for every consumer.
        if stability == Some(crate::stream_axes::Stability::Rolling) {
            tracing::info!(
                corpus = %corpus_id,
                "meta_atlas: skipping Rolling-stability corpus from anchoring"
            );
            atlases_seen.push(AtlasSeen {
                corpus_id: corpus_id.clone(),
                content_hash: content_hash.clone(),
                eligible_entities: 0,
                stability,
            });
            continue;
        }

        let mut contributed = 0usize;
        for env in &atoms_file.atoms {
            let entity = match env {
                AtomEnvelope::Entity(e) => e,
                _ => continue,
            };
            // Skip Initiative entities (personal-domain project
            // shape) and any entity whose canonical_name normalises
            // empty.
            if matches!(entity.entity_type, EntityType::Initiative) {
                continue;
            }
            let key = lookup_key(&entity.canonical_name);
            if key.is_empty() {
                continue;
            }
            // Per-atom articulation — feeds the synthesis-prompt
            // sectioning at retrieval time.
            //
            // chunk_preview = description (the corpus-side gloss).
            // For wiki atoms tagged Other("article") this carries a
            // lead-sentence shape often enough that the
            // chunk-preview classifier picks up the right markers;
            // see Stage 5 calibration histogram.
            let articulation = super::classifier::classify_articulation(env, &entity.description);

            let anchor = Anchor {
                corpus_id: corpus_id.clone(),
                atom_id: entity.id.clone(),
                primary_chunk: entity.first_appearance.clone(),
                articulation,
                stability,
                salience: entity.salience,
                atlas_content_hash: content_hash.clone(),
            };

            let bucket = clusters.entry(key.clone()).or_insert_with(|| Cluster {
                display: entity.canonical_name.clone(),
                aliases: BTreeSet::new(),
                anchors: Vec::new(),
                max_salience: f32::NEG_INFINITY,
            });
            // Display picks the canonical_name of the highest-
            // salience anchor. Stable across builds.
            if entity.salience > bucket.max_salience {
                bucket.max_salience = entity.salience;
                bucket.display = entity.canonical_name.clone();
            }
            // Alias union, normalised.
            for alias in &entity.aliases {
                let a_key = lookup_key(alias);
                if !a_key.is_empty() && a_key != key {
                    bucket.aliases.insert(a_key);
                }
            }
            bucket.anchors.push(anchor);
            contributed += 1;
        }
        atlases_seen.push(AtlasSeen {
            corpus_id: corpus_id.clone(),
            content_hash,
            eligible_entities: contributed,
            stability,
        });
    }

    let mut atoms: Vec<MetaAtom> = clusters
        .into_iter()
        .map(|(key, bucket)| MetaAtom {
            canonical_key: key,
            display: bucket.display,
            aliases: bucket.aliases,
            anchors: bucket.anchors,
        })
        .collect();
    // Stable ordering for deterministic diffs.
    atoms.sort_by(|a, b| a.canonical_key.cmp(&b.canonical_key));
    atlases_seen.sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));

    Ok(MetaAtlasFile {
        schema_version: MetaAtlasFile::SCHEMA_VERSION.to_string(),
        built_at: crate::stream_axes::timestamp_now(),
        atlases_seen,
        atoms,
    })
}

struct Cluster {
    display: String,
    aliases: BTreeSet<String>,
    anchors: Vec<Anchor>,
    max_salience: f32,
}

/// Move 6 P7: partial rebuild for a single corpus.
///
/// When an atlas delta lands on `corpus_id` (Phase 5 hook), only
/// that corpus's anchors in the meta-atlas need re-clustering. This
/// function:
///   1. Loads existing `canonical_atoms.json`.
///   2. Drops every anchor whose `corpus_id == target_corpus`.
///   3. Removes meta-atoms whose anchor list is empty after the drop.
///   4. Re-walks the target corpus's atlas + classifies each atom.
///   5. Inserts the new anchors back into the file's clusters
///      (merging into existing meta-atoms by canonical_key, or
///      creating new ones).
///   6. Persists the result atomically.
///
/// Cost vs. full `build_meta_atlas`: O(target_atoms) instead of
/// O(total_atoms across all atlases). For a newsworthy refresh that
/// touches ~20 wiki atoms, the partial rebuild is ~milliseconds
/// instead of seconds.
///
/// `meta_atlas_path` defaults to `default_meta_atlas_path()` when
/// `None`.
pub fn rebuild_for_corpus(
    indexes_dir: &Path,
    target_corpus_id: &str,
    meta_atlas_path: Option<&Path>,
) -> std::io::Result<MetaAtlasFile> {
    use std::collections::BTreeSet;

    let resolved_path = match meta_atlas_path {
        Some(p) => p.to_path_buf(),
        None => default_meta_atlas_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "default_meta_atlas_path returned None (no HOME)",
            )
        })?,
    };

    // Load existing file (or start fresh if absent).
    let mut file = if resolved_path.exists() {
        read_meta_atlas(&resolved_path)?
    } else {
        MetaAtlasFile {
            schema_version: MetaAtlasFile::SCHEMA_VERSION.to_string(),
            built_at: crate::stream_axes::timestamp_now(),
            atlases_seen: Vec::new(),
            atoms: Vec::new(),
        }
    };

    // Drop anchors belonging to target_corpus_id; retire empty
    // meta-atoms.
    for atom in file.atoms.iter_mut() {
        atom.anchors.retain(|a| a.corpus_id != target_corpus_id);
    }
    file.atoms.retain(|a| !a.anchors.is_empty());

    // Drop the corpus's row from atlases_seen so we re-emit it
    // below.
    file.atlases_seen
        .retain(|a| a.corpus_id != target_corpus_id);

    // Re-walk the target corpus only.
    let corpus_path = indexes_dir.join(target_corpus_id);
    let atlas_dir = corpus_path.join(ATLAS_DIRNAME);
    if atlas_dir.is_dir() {
        let atoms_file = match read_atlas_atoms(&atlas_dir) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(corpus = %target_corpus_id, error = %e, "rebuild_for_corpus: read_atlas_atoms failed");
                // Persist what we have (anchors dropped) and return.
                file.built_at = crate::stream_axes::timestamp_now();
                write_meta_atlas(&file, &resolved_path)?;
                return Ok(file);
            }
        };
        let content_hash = atoms_content_hash(&atlas_dir).unwrap_or_default();
        let stability = read_corpus_stability(&corpus_path);

        if stability != Some(crate::stream_axes::Stability::Rolling) {
            let mut contributed = 0usize;
            // Build new anchors keyed by canonical_key (mirrors the
            // full builder's pattern, but only for this corpus).
            let mut new_anchors_by_key: HashMap<String, Vec<Anchor>> = HashMap::new();
            let mut new_display_by_key: HashMap<String, String> = HashMap::new();
            let mut new_aliases_by_key: HashMap<String, BTreeSet<String>> = HashMap::new();

            for env in &atoms_file.atoms {
                let entity = match env {
                    AtomEnvelope::Entity(e) => e,
                    _ => continue,
                };
                if matches!(entity.entity_type, EntityType::Initiative) {
                    continue;
                }
                let key = lookup_key(&entity.canonical_name);
                if key.is_empty() {
                    continue;
                }
                let articulation =
                    super::classifier::classify_articulation(env, &entity.description);
                let anchor = Anchor {
                    corpus_id: target_corpus_id.to_string(),
                    atom_id: entity.id.clone(),
                    primary_chunk: entity.first_appearance.clone(),
                    articulation,
                    stability,
                    salience: entity.salience,
                    atlas_content_hash: content_hash.clone(),
                };
                new_anchors_by_key
                    .entry(key.clone())
                    .or_default()
                    .push(anchor);
                new_display_by_key
                    .entry(key.clone())
                    .or_insert_with(|| entity.canonical_name.clone());
                for alias in &entity.aliases {
                    let a_key = lookup_key(alias);
                    if !a_key.is_empty() && a_key != key {
                        new_aliases_by_key
                            .entry(key.clone())
                            .or_default()
                            .insert(a_key);
                    }
                }
                contributed += 1;
            }

            // Merge new anchors into existing meta-atoms (or create
            // new ones).
            let mut by_key: HashMap<String, usize> = HashMap::new();
            for (idx, atom) in file.atoms.iter().enumerate() {
                by_key.insert(atom.canonical_key.clone(), idx);
            }
            for (key, anchors) in new_anchors_by_key {
                if let Some(&idx) = by_key.get(&key) {
                    file.atoms[idx].anchors.extend(anchors);
                    if let Some(aliases) = new_aliases_by_key.get(&key) {
                        file.atoms[idx].aliases.extend(aliases.iter().cloned());
                    }
                } else {
                    let display = new_display_by_key
                        .remove(&key)
                        .unwrap_or_else(|| key.clone());
                    let aliases = new_aliases_by_key.remove(&key).unwrap_or_default();
                    file.atoms.push(MetaAtom {
                        canonical_key: key,
                        display,
                        aliases,
                        anchors,
                    });
                }
            }

            file.atlases_seen.push(AtlasSeen {
                corpus_id: target_corpus_id.to_string(),
                content_hash,
                eligible_entities: contributed,
                stability,
            });
        } else {
            tracing::info!(
                corpus = %target_corpus_id,
                "rebuild_for_corpus: skipping Rolling-stability corpus from anchoring"
            );
        }
    }

    // Re-sort for deterministic output.
    file.atoms
        .sort_by(|a, b| a.canonical_key.cmp(&b.canonical_key));
    file.atlases_seen
        .sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));
    file.built_at = crate::stream_axes::timestamp_now();

    write_meta_atlas(&file, &resolved_path)?;
    Ok(file)
}

/// Atomically write a `MetaAtlasFile` to `out_path`. Creates parent
/// dirs as needed. Writes to `<out_path>.tmp` then renames so a
/// process kill mid-write doesn't leave a partial file in place.
pub fn write_meta_atlas(file: &MetaAtlasFile, out_path: &Path) -> std::io::Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = out_path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, out_path)?;
    Ok(())
}

pub fn read_meta_atlas(path: &Path) -> std::io::Result<MetaAtlasFile> {
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Read the `stream.stability` field from a corpus's
/// `_corpus_meta.json`. Returns `None` when the file is missing, when
/// it doesn't carry a `stream` block, or when parsing fails. Used by
/// the builder to attach per-corpus stability to each anchor.
fn read_corpus_stability(corpus_dir: &Path) -> Option<Stability> {
    let meta_path = crate::corpus::Corpus::meta_in(&corpus_dir);
    let s = std::fs::read_to_string(&meta_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let block: StreamAxes = serde_json::from_value(v.get("stream")?.clone()).ok()?;
    Some(block.stability)
}

// ── tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::{
        atoms::{AtomsFile, Entity},
        AtomEnvelope, AtomId, ChunkRef,
    };
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    use std::fs;

    fn make_entity(
        idx: usize,
        canonical: &str,
        aliases: Vec<&str>,
        et: EntityType,
        salience: f32,
        defining_quote: Option<&str>,
    ) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(idx),
            canonical_name: canonical.to_string(),
            aliases: aliases.into_iter().map(String::from).collect(),
            entity_type: et,
            first_appearance: ChunkRef::new(format!("sec_{idx:04}"), None),
            description: format!("desc of {canonical}"),
            defining_quote: defining_quote.map(String::from),
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    fn write_atlas_with_meta(
        root: &Path,
        corpus_id: &str,
        atoms: Vec<AtomEnvelope>,
        stability: Option<Stability>,
    ) {
        let corpus_dir = root.join(corpus_id);
        let atlas_dir = corpus_dir.join("atlas");
        fs::create_dir_all(&atlas_dir).unwrap();
        let file = AtomsFile::new(atoms);
        fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_string_pretty(&file).unwrap(),
        )
        .unwrap();
        if let Some(s) = stability {
            let meta = serde_json::json!({
                "corpus_id": corpus_id,
                "stream": {
                    "stability": s.as_str(),
                    "source": "derived",
                    "derived_at": 0,
                    "from_signal": "test"
                }
            });
            fs::write(
                crate::corpus::Corpus::meta_in(&corpus_dir),
                serde_json::to_string_pretty(&meta).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn cross_corpus_cluster_collects_anchors() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_with_meta(
            tmp.path(),
            "wikipedia",
            vec![make_entity(
                1,
                "Albert Einstein",
                vec!["Einstein"],
                EntityType::Other("article".into()),
                0.5,
                None,
            )],
            Some(Stability::Frozen),
        );
        write_atlas_with_meta(
            tmp.path(),
            "sep",
            vec![make_entity(
                1,
                "Albert Einstein",
                vec![],
                EntityType::Concept,
                0.95,
                Some("Einstein argued that..."),
            )],
            Some(Stability::Frozen),
        );

        let file = build_meta_atlas(tmp.path()).unwrap();
        let einstein = file
            .atoms
            .iter()
            .find(|a| a.canonical_key == "albert einstein")
            .expect("einstein meta-atom present");
        assert_eq!(einstein.anchors.len(), 2);
        // Display picks the higher-salience anchor's canonical_name.
        assert_eq!(einstein.display, "Albert Einstein");
        // Per-atom articulation differs: wiki Other("article") goes
        // via chunk-preview fallback (Inventory-ish), sep Concept
        // with defining_quote is Argument-dominant.
        let wiki_anchor = einstein
            .anchors
            .iter()
            .find(|a| a.corpus_id == "wikipedia")
            .unwrap();
        let sep_anchor = einstein
            .anchors
            .iter()
            .find(|a| a.corpus_id == "sep")
            .unwrap();
        assert!(wiki_anchor.articulation.inventory >= 0.4);
        assert!(sep_anchor.articulation.argument >= 0.7);
        // Stability flows through.
        assert_eq!(wiki_anchor.stability, Some(Stability::Frozen));
        // atlases_seen records 2 corpora.
        assert_eq!(file.atlases_seen.len(), 2);
    }

    #[test]
    fn initiative_entities_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_with_meta(
            tmp.path(),
            "personal",
            vec![
                make_entity(1, "Q4 Launch", vec![], EntityType::Initiative, 0.9, None),
                make_entity(2, "Sarah Chen", vec![], EntityType::Person, 0.9, None),
            ],
            None,
        );
        let file = build_meta_atlas(tmp.path()).unwrap();
        assert!(file.atoms.iter().all(|a| a.canonical_key != "q4 launch"));
        assert!(file.atoms.iter().any(|a| a.canonical_key == "sarah chen"));
    }

    #[test]
    fn corpus_without_stream_block_anchors_carry_none_stability() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_with_meta(
            tmp.path(),
            "atlas-only",
            vec![make_entity(1, "Foo", vec![], EntityType::Person, 0.9, None)],
            None,
        );
        let file = build_meta_atlas(tmp.path()).unwrap();
        let foo = file
            .atoms
            .iter()
            .find(|a| a.canonical_key == "foo")
            .unwrap();
        assert_eq!(foo.anchors[0].stability, None);
    }

    #[test]
    fn deterministic_ordering_across_runs() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_with_meta(
            tmp.path(),
            "a",
            vec![
                make_entity(1, "Zebra", vec![], EntityType::Person, 0.5, None),
                make_entity(2, "Alpha", vec![], EntityType::Person, 0.5, None),
            ],
            None,
        );
        let file = build_meta_atlas(tmp.path()).unwrap();
        let keys: Vec<&str> = file
            .atoms
            .iter()
            .map(|a| a.canonical_key.as_str())
            .collect();
        assert_eq!(keys, vec!["alpha", "zebra"]);
    }

    #[test]
    fn rebuild_for_corpus_drops_old_and_inserts_new() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();

        // Seed two corpora with one shared canonical entity.
        write_atlas_with_meta(
            indexes,
            "wiki",
            vec![make_entity(
                1,
                "Einstein",
                vec![],
                EntityType::Person,
                0.5,
                None,
            )],
            Some(Stability::Frozen),
        );
        write_atlas_with_meta(
            indexes,
            "sep",
            vec![make_entity(
                1,
                "Einstein",
                vec![],
                EntityType::Concept,
                0.9,
                Some("Quotable"),
            )],
            Some(Stability::Frozen),
        );

        let initial = build_meta_atlas(indexes).unwrap();
        let meta_path = tmp.path().join("meta_atlas.json");
        write_meta_atlas(&initial, &meta_path).unwrap();
        let einstein = initial
            .atoms
            .iter()
            .find(|a| a.canonical_key == "einstein")
            .expect("einstein present");
        assert_eq!(einstein.anchors.len(), 2);

        // Update wiki's einstein to have a different salience; rebuild
        // just wiki.
        std::fs::remove_file(indexes.join("wiki/atlas/atoms.json")).unwrap();
        let new_atoms = AtomsFile::new(vec![make_entity(
            2,
            "Einstein",
            vec![],
            EntityType::Person,
            0.99,
            None,
        )]);
        std::fs::write(
            indexes.join("wiki/atlas/atoms.json"),
            serde_json::to_string_pretty(&new_atoms).unwrap(),
        )
        .unwrap();

        let after = rebuild_for_corpus(indexes, "wiki", Some(&meta_path)).unwrap();
        let einstein = after
            .atoms
            .iter()
            .find(|a| a.canonical_key == "einstein")
            .expect("einstein still present");
        // Two anchors: sep unchanged, wiki refreshed.
        assert_eq!(einstein.anchors.len(), 2);
        let wiki_anchor = einstein
            .anchors
            .iter()
            .find(|a| a.corpus_id == "wiki")
            .unwrap();
        assert!((wiki_anchor.salience - 0.99).abs() < 1e-6);
        let sep_anchor = einstein
            .anchors
            .iter()
            .find(|a| a.corpus_id == "sep")
            .unwrap();
        assert!((sep_anchor.salience - 0.9).abs() < 1e-6);
    }

    #[test]
    fn rebuild_for_corpus_removes_meta_atom_when_only_corpus_drops_it() {
        let tmp = tempfile::tempdir().unwrap();
        let indexes = tmp.path();
        write_atlas_with_meta(
            indexes,
            "wiki",
            vec![make_entity(
                1,
                "OnlyHere",
                vec![],
                EntityType::Person,
                0.5,
                None,
            )],
            Some(Stability::Frozen),
        );
        let initial = build_meta_atlas(indexes).unwrap();
        let meta_path = tmp.path().join("meta.json");
        write_meta_atlas(&initial, &meta_path).unwrap();
        assert!(initial.atoms.iter().any(|a| a.canonical_key == "onlyhere"));

        // Replace wiki atlas with one that doesn't contain OnlyHere.
        let new_atoms = AtomsFile::new(vec![]);
        std::fs::write(
            indexes.join("wiki/atlas/atoms.json"),
            serde_json::to_string_pretty(&new_atoms).unwrap(),
        )
        .unwrap();
        let after = rebuild_for_corpus(indexes, "wiki", Some(&meta_path)).unwrap();
        assert!(
            !after.atoms.iter().any(|a| a.canonical_key == "onlyhere"),
            "meta-atom should be retired when last anchor goes"
        );
    }

    #[test]
    fn roundtrip_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        write_atlas_with_meta(
            tmp.path(),
            "wiki",
            vec![make_entity(
                1,
                "Test",
                vec![],
                EntityType::Person,
                0.5,
                None,
            )],
            Some(Stability::Frozen),
        );
        let built = build_meta_atlas(tmp.path()).unwrap();
        let out = tmp.path().join("canonical_atoms.json");
        write_meta_atlas(&built, &out).unwrap();
        let back = read_meta_atlas(&out).unwrap();
        assert_eq!(back.atoms.len(), built.atoms.len());
        assert_eq!(back.schema_version, MetaAtlasFile::SCHEMA_VERSION);
    }
}
