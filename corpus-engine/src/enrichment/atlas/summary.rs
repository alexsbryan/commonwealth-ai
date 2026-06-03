//! Cached atlas summary — atom count, Tier-2 count, fingerprint.
//!
//! Mesh gossip needs to advertise these per corpus on every round so
//! peers can rank atlases by depth (`tier2_count`) and verify a
//! pulled atlas matches before trusting it (`fingerprint`).
//! Recomputing them from `atoms.json` every gossip tick is wasteful
//! at wiki scale (~50 MB JSON, ~50K entities); instead we persist a
//! tiny `_summary.json` next to the atlas and invalidate by
//! `atoms.json` size + mtime. A change to atoms.json (e.g. Tier-2
//! extraction added more `extracted` entries) flips the cache key
//! and the next reader recomputes.
//!
//! ## File layout: `atlas/_summary.json`
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "atom_count": 51280,
//!   "tier2_count": 612,
//!   "fingerprint": "sha256:7c3f…",
//!   "atoms_mtime_ms": 1715000000000,
//!   "atoms_size_bytes": 52428800
//! }
//! ```
//!
//! Consumers (`sovereign-mesh::capabilities`,
//! `sovereign-tools::atlas_postinstall`) call
//! [`read_or_compute_summary`] which transparently rewrites the
//! sidecar on cache miss.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::atoms::AtomType;
use super::{atoms_content_hash, read_atlas_atoms, AtomEnvelope};
use crate::enrichment::pipeline::atlas::EnrichmentDepth;

const SUMMARY_FILE: &str = "_summary.json";
// v2 (2026-05-12) adds `atom_counts` so consumers can render per-type
// breakdowns without re-reading atoms.json. v1 caches are auto-
// invalidated by `read_or_compute_summary`'s schema_version check and
// transparently recomputed on next read.
const SCHEMA_VERSION: u32 = 2;

/// Atlas-level statistics carried in mesh gossip and shown in
/// `sovereign corpus status` / `sovereign mesh status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AtlasSummary {
    pub schema_version: u32,
    /// Total atoms (entities + events + …) in `atoms.json`.
    pub atom_count: u64,
    /// Entities whose `enrichment_depth` is `extracted` — i.e.
    /// Tier-2 deep-enriched. Drives the mesh's "do I have a deeper
    /// atlas than this peer?" comparison.
    pub tier2_count: u64,
    /// SHA-256 of `atoms.json` (hex, no `sha256:` prefix —
    /// matches [`atoms_content_hash`] for direct comparison).
    pub fingerprint: String,
    /// `atoms.json` mtime when the summary was computed (ms since
    /// epoch). Cache key.
    pub atoms_mtime_ms: u64,
    /// `atoms.json` size in bytes when computed. Cache key — we
    /// pair size + mtime because mtime alone can collide on
    /// fast-rebuilt atlases.
    pub atoms_size_bytes: u64,
    /// Per-`AtomType` atom counts. Lets consumers render type
    /// breakdowns (e.g. the desktop's atlas inspector) without
    /// re-reading atoms.json. Added in schema v2. `#[serde(default)]`
    /// keeps v1 caches that lack the field deserialising — the
    /// schema_version check below will still reject them and force
    /// a recompute, but defensive defaulting protects against
    /// partially-written or hand-edited files.
    #[serde(default)]
    pub atom_counts: BTreeMap<AtomType, u64>,
}

impl AtlasSummary {
    /// Empty placeholder used by callers that need a stand-in for a
    /// corpus without an atlas yet (e.g. fresh install gossip
    /// before the post-install hook completes).
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            atom_count: 0,
            tier2_count: 0,
            fingerprint: String::new(),
            atoms_mtime_ms: 0,
            atoms_size_bytes: 0,
            atom_counts: BTreeMap::new(),
        }
    }
}

/// Compute the summary by reading + parsing atoms.json. Fingerprint
/// is the SHA-256 of the same file. Use [`read_or_compute_summary`]
/// at hot paths — this is the cache-miss branch.
pub fn compute_summary(atlas_dir: &Path) -> io::Result<AtlasSummary> {
    let atoms_path = atlas_dir.join("atoms.json");
    let meta = fs::metadata(&atoms_path)?;
    let atoms_size_bytes = meta.len();
    let atoms_mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let fingerprint = atoms_content_hash(atlas_dir)?;
    let atoms = read_atlas_atoms(atlas_dir)?;
    let atom_count = atoms.atoms.len() as u64;

    // Single pass over atoms — count tier-2 (extracted entities) and
    // per-type totals together. The previous code iterated only
    // entities for tier2; the type-counter folds in the other seven
    // variants without a second pass.
    let mut tier2_count: u64 = 0;
    let mut atom_counts: BTreeMap<AtomType, u64> = BTreeMap::new();
    for a in &atoms.atoms {
        let t = match a {
            AtomEnvelope::Entity(e) => {
                if matches!(e.enrichment_depth, EnrichmentDepth::Extracted) {
                    tier2_count += 1;
                }
                AtomType::Entity
            }
            AtomEnvelope::Event(_) => AtomType::Event,
            AtomEnvelope::State(_) => AtomType::State,
            AtomEnvelope::Relation(_) => AtomType::Relation,
            AtomEnvelope::Claim(_) => AtomType::Claim,
            AtomEnvelope::Question(_) => AtomType::Question,
            AtomEnvelope::Configuration(_) => AtomType::Configuration,
            AtomEnvelope::ArgumentReconstruction(_) => AtomType::ArgumentReconstruction,
            AtomEnvelope::Position(_) => AtomType::Position,
            AtomEnvelope::Opposition(_) => AtomType::Opposition,
            AtomEnvelope::Asset(_) => AtomType::Asset,
        };
        *atom_counts.entry(t).or_insert(0) += 1;
    }

    Ok(AtlasSummary {
        schema_version: SCHEMA_VERSION,
        atom_count,
        tier2_count,
        fingerprint,
        atoms_mtime_ms,
        atoms_size_bytes,
        atom_counts,
    })
}

/// Read `atlas/_summary.json` if present and the cache key matches
/// the live `atoms.json`. Otherwise recompute, persist, and return
/// the fresh summary.
///
/// `Ok(None)` means there's no `atoms.json` to summarise (caller
/// treats as "no atlas yet").
pub fn read_or_compute_summary(atlas_dir: &Path) -> io::Result<Option<AtlasSummary>> {
    let atoms_path = atlas_dir.join("atoms.json");
    if !atoms_path.exists() {
        return Ok(None);
    }
    let live_meta = fs::metadata(&atoms_path)?;
    let live_size = live_meta.len();
    let live_mtime_ms = live_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    if let Some(cached) = read_summary_file(atlas_dir) {
        if cached.atoms_mtime_ms == live_mtime_ms
            && cached.atoms_size_bytes == live_size
            && cached.schema_version == SCHEMA_VERSION
        {
            return Ok(Some(cached));
        }
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

fn read_summary_file(atlas_dir: &Path) -> Option<AtlasSummary> {
    let path = atlas_dir.join(SUMMARY_FILE);
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_summary_file(atlas_dir: &Path, summary: &AtlasSummary) -> io::Result<()> {
    let path = atlas_dir.join(SUMMARY_FILE);
    let tmp = atlas_dir.join(format!(".{SUMMARY_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(summary)
        .map_err(io::Error::other)?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, AtomsFile, ChunkRef, Entity};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

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
        write_atoms(tmp.path(), &[EnrichmentDepth::Extracted, EnrichmentDepth::Extracted]);

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
}
