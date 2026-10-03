// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas summary ORCHESTRATOR — read-through-cache with best-effort
//! persist.
//!
//! The summary shape, its derivation and the cache-KEY decision live in the
//! `corpus-engine-atlas-reader` leaf since 2026-09-21 (FIVE_PROGRAMS §12
//! decision 1); they are re-exported below at their historical paths. What
//! stays here is the write: `read_or_compute_summary` persists
//! `atlas/_summary.json` (best-effort, non-fatal) so the next read is a cache
//! hit. The leaf never writes the atlas; this file is where the summary's one
//! write lives.

pub use corpus_engine_atlas_reader::summary::{
    compute_summary, read_current_summary, AnnSummary, AtlasSummary, OntologySummary,
    SCHEMA_VERSION, SUMMARY_FILE,
};

use std::fs;
use std::io;
use std::path::Path;

pub fn read_or_compute_summary(atlas_dir: &Path) -> io::Result<Option<AtlasSummary>> {
    let atoms_path = atlas_dir.join("atoms.json");
    if !atoms_path.exists() {
        return Ok(None);
    }
    if let Some(cached) = read_current_summary(atlas_dir) {
        return Ok(Some(cached));
    }

    let fresh = compute_summary(atlas_dir)?;
    // Best-effort persist — a write failure is non-fatal (next read
    // recomputes). Don't propagate, just trace.
    if let Err(e) = write_summary_file(atlas_dir, &fresh) {
        tracing::warn!(
            atlas_dir = %atlas_dir.display(),
            error = %e,
            "atlas summary: cache write failed (non-fatal)"
        );
    }
    Ok(Some(fresh))
}

fn write_summary_file(atlas_dir: &Path, summary: &AtlasSummary) -> io::Result<()> {
    let path = atlas_dir.join(SUMMARY_FILE);
    let tmp = atlas_dir.join(format!(".{SUMMARY_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(summary).map_err(io::Error::other)?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, AtomsFile, ChunkRef, Entity};
    use crate::enrichment::atlas::{AtomEnvelope, AtomType};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    use std::collections::BTreeMap;
    use understanding_vocab::read::read_atlas_atoms;

    fn write_atoms(dir: &Path, depths: &[EnrichmentDepth]) {
        std::fs::create_dir_all(dir).unwrap();
        let atoms: Vec<AtomEnvelope> = depths
            .iter()
            .enumerate()
            .map(|(i, d)| {
                AtomEnvelope::Entity(Entity {
                    id: AtomId::entity(i + 1),
                    canonical_name: format!("Entity {i}"),
                    aliases: Vec::new(),
                    entity_type: EntityType::Concept,
                    first_appearance: ChunkRef::new("sec_0001", None),
                    description: "x".into(),
                    defining_quote: None,
                    salience: 1.0,
                    enrichment_depth: *d,
                    affiliation: None,
                    role: None,
                    participants: Vec::new(),
                    provenance: Default::default(),
                    attributes: serde_json::Map::new(),
                    concept_kind: None,
                })
            })
            .collect();
        let file = AtomsFile::new(atoms);
        std::fs::write(
            dir.join("atoms.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }

    /// The edge census follows the CSR, and the CSR is a cache key: a
    /// summary taken before the store lands is stale the moment it does.
    /// Failing input: drop `csr_mtime_ms` from `read_current_summary`'s key
    /// and the second read below returns the empty census as current.
    #[test]
    fn edge_counts_follow_the_csr_and_key_the_cache() {
        use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
        let tmp = tempfile::tempdir().unwrap();
        write_atoms(
            tmp.path(),
            &[EnrichmentDepth::Extracted, EnrichmentDepth::Extracted],
        );
        let before = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        assert_eq!(before.edge_counts, None, "no store is not an empty store");
        assert_eq!(before.csr_mtime_ms, 0);
        assert!(read_current_summary(tmp.path()).is_some());

        // The store lands after atoms.json, with one Involves edge.
        let atoms_file = read_atlas_atoms(tmp.path()).unwrap();
        let atoms = atoms_file.atoms();
        let edges = vec![Edge {
            id: EdgeId::from_raw("e1"),
            edge_type: EdgeType::Involves,
            source: AtomId::entity(1),
            target: AtomId::entity(2),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }];
        super::super::store::write_store_blocking(tmp.path(), "c", &atoms, &edges).unwrap();
        assert!(
            read_current_summary(tmp.path()).is_none(),
            "a summary computed before the CSR must not stay current"
        );
        let after = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        assert_eq!(
            after.edge_counts,
            Some(BTreeMap::from([(EdgeType::Involves, 1)]))
        );
        assert!(after.csr_mtime_ms > 0);
        assert!(read_current_summary(tmp.path()).is_some());
    }

    #[test]
    fn missing_atoms_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_or_compute_summary(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn computed_summary_counts_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        write_atoms(
            tmp.path(),
            &[
                EnrichmentDepth::Structural,
                EnrichmentDepth::Extracted,
                EnrichmentDepth::Extracted,
                EnrichmentDepth::StructuralClassified,
            ],
        );
        let s = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        assert_eq!(s.atom_count, 4);
        assert_eq!(s.tier2_count, 2);
        assert!(!s.fingerprint.is_empty());
        // v2 — per-type counts. All four atoms are Entities in this
        // fixture; the map should record that without burning a
        // separate atoms.json pass.
        assert_eq!(s.atom_counts.get(&AtomType::Entity).copied(), Some(4));
        assert_eq!(s.atom_counts.get(&AtomType::Claim).copied(), None);
    }

    #[test]
    fn v1_cache_is_invalidated_and_recomputed_with_atom_counts() {
        // Simulate an old v1 _summary.json on disk (schema_version=1,
        // no atom_counts field). The cache-key check rejects it on
        // SCHEMA_VERSION mismatch and recomputes — the recomputed
        // v2 summary must include atom_counts.
        let tmp = tempfile::tempdir().unwrap();
        write_atoms(
            tmp.path(),
            &[EnrichmentDepth::Extracted, EnrichmentDepth::Extracted],
        );

        let live_meta = std::fs::metadata(tmp.path().join("atoms.json")).unwrap();
        let live_mtime_ms = live_meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // Hand-craft a v1 sidecar that would otherwise be a cache hit
        // (matching mtime + size). Only schema_version=1 should make
        // it miss.
        let v1_sidecar = serde_json::json!({
            "schema_version": 1,
            "atom_count": 2,
            "tier2_count": 2,
            "fingerprint": "stale",
            "atoms_mtime_ms": live_mtime_ms,
            "atoms_size_bytes": live_meta.len(),
        });
        std::fs::write(
            tmp.path().join(SUMMARY_FILE),
            serde_json::to_vec_pretty(&v1_sidecar).unwrap(),
        )
        .unwrap();

        let fresh = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        assert_eq!(fresh.schema_version, SCHEMA_VERSION);
        assert_eq!(fresh.atom_counts.get(&AtomType::Entity).copied(), Some(2));
        assert_ne!(fresh.fingerprint, "stale");
    }

    /// A declared ontology beside the atoms shows up in the summary, and an
    /// atlas without `ontology.json` reads as declaring nothing rather than
    /// as an error.
    #[test]
    fn summary_projects_a_declared_ontology_and_tolerates_its_absence() {
        use crate::enrichment::ontology::{OntologyTypeDecl, TypeKind};

        let tmp = tempfile::tempdir().unwrap();
        write_atoms(tmp.path(), &[EnrichmentDepth::Extracted]);
        assert!(
            compute_summary(tmp.path()).unwrap().ontology.is_none(),
            "no ontology.json means the corpus declares nothing"
        );

        let mut policies = crate::enrichment::ontology::OntologyPolicies::default();
        policies.shape.types = vec![OntologyTypeDecl {
            name: "coin".into(),
            kind: TypeKind::Entity,
            identity: vec!["find_id".into()],
            ..Default::default()
        }];
        // Axis 3 is the policy's own map; `OntologyV1::into_policies` fills it
        // from the type decls at parse time, which is what this mirrors.
        policies
            .identity
            .identity
            .insert("coin".into(), vec!["find_id".into()]);
        super::super::writer::write_atlas_ontology(tmp.path(), "custom_atlas", 1, &policies)
            .unwrap();

        let s = compute_summary(tmp.path()).unwrap();
        let o = s.ontology.expect("the declaration is recorded");
        assert_eq!(o.version, 1);
        assert_eq!(o.declared.get("coin").map(String::as_str), Some("entity"));
        assert_eq!(o.clock, "document_date");
        assert_eq!(
            o.identity_criteria.get("coin").map(String::as_str),
            Some("external:find_id")
        );
    }

    #[test]
    fn cache_hit_avoids_recompute() {
        let tmp = tempfile::tempdir().unwrap();
        write_atoms(tmp.path(), &[EnrichmentDepth::Extracted]);
        let s1 = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        // The summary file should now exist on disk.
        assert!(tmp.path().join(SUMMARY_FILE).exists());
        let s2 = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        assert_eq!(s1, s2);
    }

    /// Phase C2 invariant: the atlas summary survives the
    /// `tar cf -C indexes <corpus>` / `tar xf` roundtrip used by
    /// `/internal/index/transfer`. The puller must read the same
    /// counts + fingerprint as the pusher reported in gossip.
    #[test]
    fn summary_survives_tar_roundtrip() {
        use std::process::Command;
        let src = tempfile::tempdir().unwrap();
        let corpus_dir = src.path().join("wikipedia");
        let atlas_dir = corpus_dir.join("atlas");
        write_atoms(
            &atlas_dir,
            &[
                EnrichmentDepth::Structural,
                EnrichmentDepth::Extracted,
                EnrichmentDepth::Extracted,
            ],
        );
        let pre = read_or_compute_summary(&atlas_dir).unwrap().unwrap();

        // Mirror the wire format: tar cf -C <indexes> <corpus_id>.
        let tar_path = src.path().join("wikipedia.tar");
        let status = Command::new("tar")
            .args([
                "cf",
                tar_path.to_str().unwrap(),
                "-C",
                src.path().to_str().unwrap(),
                "wikipedia",
            ])
            .status()
            .expect("tar cf");
        assert!(status.success());

        // Unpack into a fresh dir, the way the puller does.
        let dst = tempfile::tempdir().unwrap();
        let status = Command::new("tar")
            .args([
                "xf",
                tar_path.to_str().unwrap(),
                "-C",
                dst.path().to_str().unwrap(),
            ])
            .status()
            .expect("tar xf");
        assert!(status.success());

        // The receiver MUST see the same counts + fingerprint.
        let dst_atlas = dst.path().join("wikipedia").join("atlas");
        // Drop the cached summary the way the receiver does — we
        // care that recomputing yields the same numbers, not that
        // a stale cache was carried across.
        let _ = std::fs::remove_file(dst_atlas.join("_summary.json"));
        let post = read_or_compute_summary(&dst_atlas).unwrap().unwrap();
        assert_eq!(pre.atom_count, post.atom_count);
        assert_eq!(pre.tier2_count, post.tier2_count);
        assert_eq!(pre.fingerprint, post.fingerprint);
    }

    #[test]
    fn atoms_change_invalidates_cache() {
        let tmp = tempfile::tempdir().unwrap();
        write_atoms(tmp.path(), &[EnrichmentDepth::Structural]);
        let s1 = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        // Sleep so mtime is observably different on platforms with
        // 1-sec mtime granularity.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_atoms(
            tmp.path(),
            &[EnrichmentDepth::Extracted, EnrichmentDepth::Extracted],
        );
        let s2 = read_or_compute_summary(tmp.path()).unwrap().unwrap();
        assert_ne!(s1.atom_count, s2.atom_count);
        assert_eq!(s2.tier2_count, 2);
        assert_ne!(s1.fingerprint, s2.fingerprint);
    }

    // ── ANN coverage (ei-3-index) ────────────────────────────────────────────

    /// Build a real `atoms_ann.lance` beside the atoms so the coverage row has
    /// something to count. Uses the ONE writer (`context_loader::backfill_ann`)
    /// rather than hand-rolling a Lance table, so the test cannot drift from
    /// what an ingest actually produces.
    fn seed_table_for(dir: &Path) {
        let embed: crate::types::EmbedFn = std::sync::Arc::new(|text: &str| {
            let n = text.len() as f32;
            Box::pin(async move { Ok(vec![n, 1.0, 0.0, 0.0]) })
        });
        let filter = crate::enrichment::atlas::context_filter::AtlasContextFilter {
            min_description_chars: 1,
            depth_allowlist: vec!["extracted".into()],
            max_entries: None,
            top_k: 3,
            include_claims: false,
            include_tensions: false,
            include_configurations: false,
            include_declared_claim_types: false,
            seed_kinds: None,
        };
        let out = crate::enrichment::atlas::context_loader::backfill_ann_blocking(
            &embed, dir, "t", &filter,
        )
        .expect("seed table builds");
        assert!(matches!(
            out,
            crate::enrichment::atlas::context_loader::BackfillOutcome::Built(_)
        ));
    }

    /// ei-3-index bar 5 — coverage is a COUNT of what the seed table holds, and
    /// its absence is `None`, never `Some(0)`. Watched failing before the
    /// field existed: every summary in the fleet reported atoms and said
    /// nothing about whether any of them could seed a walk.
    #[test]
    fn the_summary_reports_seed_table_coverage_and_absence_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("atlas");
        write_atoms(
            &dir,
            &[EnrichmentDepth::Extracted, EnrichmentDepth::Extracted],
        );

        let before = compute_summary(&dir).unwrap();
        assert_eq!(before.atom_count, 2);
        assert!(
            before.ann.is_none(),
            "no seed table is an absence, not a coverage of zero"
        );

        seed_table_for(&dir);
        let after = compute_summary(&dir).unwrap();
        assert_eq!(
            after.ann.as_ref().map(|a| a.embedded_atoms),
            Some(2),
            "both extracted entities seed the walk"
        );
    }

    /// ei-3-index bar 6 — THE cache-key regression this order had to avoid.
    /// Watched failing with the v4 key (atoms mtime + size + schema only):
    /// the summary computed between the atoms write and the seed write stayed
    /// "current" forever, so `corpus_list` reported "NO seed table" for an
    /// atlas that had one — permanently, because nothing ever touches
    /// `atoms.json` again. The seed table is a second INPUT, so it is a
    /// second KEY.
    #[test]
    fn a_summary_cached_before_the_seed_table_is_stale_once_it_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("atlas");
        write_atoms(&dir, &[EnrichmentDepth::Extracted]);

        // The 0-coverage summary an eager `corpus status` persists mid-build.
        let pre = read_or_compute_summary(&dir).unwrap().unwrap();
        assert!(pre.ann.is_none());
        assert!(
            read_current_summary(&dir).is_some(),
            "cache is current pre-seed"
        );

        seed_table_for(&dir);

        assert!(
            read_current_summary(&dir).is_none(),
            "a summary that predates the seed table must not read as current"
        );
        let healed = read_or_compute_summary(&dir).unwrap().unwrap();
        assert_eq!(healed.ann.as_ref().map(|a| a.embedded_atoms), Some(1));
    }
}

#[cfg(test)]
mod subtype_census_tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, AtomsFile, ChunkRef, Entity, State};
    use crate::enrichment::atlas::SectionRange;
    use crate::enrichment::atlas::{AtomEnvelope, AtomType};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, StateType};

    fn entity(i: usize, entity_type: EntityType) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(i),
            canonical_name: format!("Entity {i}"),
            aliases: Vec::new(),
            entity_type,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    /// The atom a `role_of` type produces: a State on the rigid person atom.
    fn role_state(i: usize, label: &str) -> AtomEnvelope {
        AtomEnvelope::State(State {
            id: AtomId::from_raw(&format!("state-{i:04}")),
            entity_id: AtomId::entity(1),
            label: label.into(),
            state_type: StateType::Other(label.into()),
            evidence: Vec::new(),
            section_range: SectionRange {
                start: "sec_0001".into(),
                end: "sec_0001".into(),
            },
            confidence: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    fn write(dir: &Path, atoms: Vec<AtomEnvelope>) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("atoms.json"),
            serde_json::to_vec_pretty(&AtomsFile::new(atoms)).unwrap(),
        )
        .unwrap();
    }

    /// The census counts the AUTHOR's nouns across every atom kind, so a
    /// `role_of` type — which lands as a State, never as an entity of that
    /// type — is counted where a per-kind breakdown reports zero for it.
    ///
    /// Falsifier: count subtypes only within the Entity bucket and `ruler`
    /// disappears.
    #[test]
    fn the_census_counts_declared_nouns_across_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            vec![
                entity(1, EntityType::Other("coin".into())),
                entity(2, EntityType::Other("coin".into())),
                entity(3, EntityType::Other("sceatta".into())),
                role_state(1, "ruler"),
                role_state(2, "ruler"),
            ],
        );
        let s = compute_summary(tmp.path()).unwrap();
        assert_eq!(s.subtype_counts.get("coin").copied(), Some(2));
        assert_eq!(s.subtype_counts.get("sceatta").copied(), Some(1));
        assert_eq!(
            s.subtype_counts.get("ruler").copied(),
            Some(2),
            "a role lands as a State and is still the author's noun"
        );
        // Own counts only — the roll-up is the consumer's, using `specializes`.
        assert_eq!(
            s.subtype_counts.get("coin").copied(),
            Some(2),
            "`sceatta` does not silently add itself to `coin`"
        );
    }

    /// An atom with no subtype is ABSENT from the census, never a count under
    /// the empty string — "nothing was classified" and "everything was
    /// classified as ``" are different findings (§18.3).
    ///
    /// Falsifier: drop the `is_empty` guard and `""` appears as a key.
    #[test]
    fn an_unclassified_atom_is_absent_not_empty_keyed() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            vec![
                entity(1, EntityType::Other("coin".into())),
                // `Other("unclassified")` is one of the two on-disk spellings
                // of absence that `subtype_of` folds to empty.
                AtomEnvelope::State(State {
                    state_type: StateType::Other("unclassified".into()),
                    ..match role_state(1, "x") {
                        AtomEnvelope::State(s) => s,
                        _ => unreachable!(),
                    }
                }),
            ],
        );
        let s = compute_summary(tmp.path()).unwrap();
        assert_eq!(s.subtype_counts.get("coin").copied(), Some(1));
        assert!(
            !s.subtype_counts.contains_key(""),
            "absence is absent: {:?}",
            s.subtype_counts
        );
    }
}
