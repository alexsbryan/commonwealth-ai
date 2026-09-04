// SPDX-License-Identifier: AGPL-3.0-or-later
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

use super::ann_store::{ann_table_mtime_ms, ann_table_present, ann_table_rows};
use super::atoms::AtomType;
use super::{atoms_content_hash, read_atlas_atoms, AtomEnvelope};
use crate::enrichment::pipeline::atlas::EnrichmentDepth;
use tracing::debug;

const SUMMARY_FILE: &str = "_summary.json";
// v2 (2026-05-12) adds `atom_counts` so consumers can render per-type
// breakdowns without re-reading atoms.json. v1 caches are auto-
// invalidated by `read_or_compute_summary`'s schema_version check and
// transparently recomputed on next read.
// v3 (2026-09-01) adds `ontology`, so a reader can say what a corpus
// declared without opening atoms.json or the enrich config.
// v4 (2026-09-02) adds `subtype_counts` and `ontology.specializes`. v3 named
// the author's types but could not say how many atoms each one has, so a
// reader wanting "coin 13" had to open atoms.json and re-derive it — which is
// the whole thing this file exists to avoid. `specializes` rides along because
// a count without the hierarchy cannot answer "how many coins" for a corpus
// where `sceatta` is one.
// v5 (2026-09-04) adds `ann` and the `ann_mtime_ms` cache key: the ANN seed
// table's coverage (ei-3-index; EPISTEMIC_INDEX section 1, Ideas row, where
// the artifacts are mandatory and coverage is REPORTED). The table is a
// second INPUT to the summary, so it is a second KEY -- a summary computed
// between the atoms write and the seed write must not stay "current" once the
// table lands.
const SCHEMA_VERSION: u32 = 5;

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
    /// Per-SUBTYPE atom counts — the author's own nouns, counted across every
    /// atom kind rather than within one. A `role_of` type lands as a State on
    /// a rigid atom (`ruler role_of person`), so counting `ruler` inside the
    /// Entity bucket would report zero for a role that landed perfectly; the
    /// key here is whatever [`projection::subtype_of`] says, which is the one
    /// answer to "what type is this atom" (§10.6).
    ///
    /// Atoms with no subtype are ABSENT, not counted under `""` — a corpus
    /// that classified nothing has an empty map, which reads differently from
    /// one that classified everything as the empty string. Added in schema v4.
    ///
    /// Counts are OWN only: `sceatta` does not add to `coin`. The roll-up
    /// needs the hierarchy, which rides in [`OntologySummary::specializes`] so
    /// a consumer can do it and this map stays a plain census.
    #[serde(default)]
    pub subtype_counts: BTreeMap<String, u64>,
    /// What this atlas was extracted under, when the recipe declared an
    /// ontology. `None` for every prebuilt genre and every prose-only custom
    /// atlas — declaring nothing is the common case and costs no key on the
    /// wire. Added in schema v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ontology: Option<OntologySummary>,
    /// ANN seed-table coverage: how many of this atlas's atoms carry an
    /// embedding the walk can seed on. `None` means there is NO seed table --
    /// the corpus cannot ground, which is a different fact from a table that
    /// covers zero atoms, so it is never rendered as `0` (ARCH 18.3).
    /// Added in schema v5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ann: Option<AnnSummary>,
    /// `atoms_ann.lance` mtime (ms since epoch) when the summary was computed;
    /// `0` when there was no table. The SECOND cache key, alongside
    /// `atoms_mtime_ms`/`atoms_size_bytes`: the seed table is written after
    /// `atoms.json`, so keying on the atoms file alone would freeze a
    /// no-coverage summary in place for the life of the atlas. Added in v5.
    #[serde(default)]
    pub ann_mtime_ms: u64,
}

/// The ANN seed table's contribution to the summary -- the coverage row
/// `corpus_list` and `svrn corpus status` print (EPISTEMIC_INDEX section 1).
///
/// One ratio, two honest numbers: `embedded_atoms` of the summary's own
/// `atom_count`. The denominator is that field rather than a second count, so
/// this file holds ONE atom census (ARCH 10.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnSummary {
    /// Rows in `atoms_ann.lance`. The production grounding filter admits a
    /// subset of atoms (extracted depth, a signal floor), so this is normally
    /// well below `atom_count`; a shortfall is not a defect, a zero is.
    pub embedded_atoms: u64,
}

/// The headline facts about a declared ontology, for a caller that wants to
/// label a corpus without reading `atlas/ontology.json` itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OntologySummary {
    /// The `[enrichment.ontology] version` the policies were parsed under.
    pub version: u32,
    /// Declared type name → its atom kind, in name order.
    pub declared: BTreeMap<String, String>,
    /// The clock supersession folds on (`document_date` | `narrative` | `none`).
    pub clock: String,
    /// Type → how two mentions of it are judged the same thing:
    /// `external:<keys>`, `fallback:<keys>`, or absent when the type resolves
    /// on its canonical name (the reported default).
    pub identity_criteria: BTreeMap<String, String>,
    /// Declared type → the type it `specializes`, for the types that declare
    /// one. Absent for the rest, so an empty map means a flat ontology.
    ///
    /// Here because a subtype census is not answerable without it: "how many
    /// coins" in a corpus that also declares `sceatta specializes coin` is the
    /// two counts added, and a consumer holding only names and counts cannot
    /// know to add them. One level per entry — walk it for the transitive
    /// closure. Added in schema v4.
    #[serde(default)]
    pub specializes: BTreeMap<String, String>,
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
            subtype_counts: BTreeMap::new(),
            ontology: None,
            ann: None,
            ann_mtime_ms: 0,
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
        if let AtomEnvelope::Entity(e) = a {
            if matches!(e.enrichment_depth, EnrichmentDepth::Extracted) {
                tier2_count += 1;
            }
        }
        *atom_counts.entry(a.atom_type()).or_insert(0) += 1;
    }
    // The declared-subtype census comes from the ONE tally, which the
    // ontology-coverage rollup also calls — two readers, one count (§10.6).
    let (subtype_counts, unsubtyped) = super::projection::subtype_tally(&atoms.atoms);
    // Traced as a total, not per atom (§9.1): the decision an operator needs
    // to see is "how many atoms this census does not account for", and this
    // loop runs over every atom in the corpus — 1.5M on the meta-atlas — so a
    // line each would be the wrong shape for the same fact. Without it, a
    // census summing to less than `atom_count` looks like a bug in the census.
    debug!(
        atlas = %atlas_dir.display(),
        atoms = atom_count, subtypes = subtype_counts.len(), unsubtyped,
        "atlas summary: subtype census"
    );

    Ok(AtlasSummary {
        schema_version: SCHEMA_VERSION,
        atom_count,
        tier2_count,
        fingerprint,
        atoms_mtime_ms,
        atoms_size_bytes,
        atom_counts,
        subtype_counts,
        ontology: read_ontology_summary(atlas_dir),
        ann: read_ann_summary(atlas_dir),
        ann_mtime_ms: ann_table_mtime_ms(atlas_dir),
    })
}

/// Count `atoms_ann.lance`'s rows for the summary. `None` when there is no
/// table or it cannot be read -- absence, reported as absence.
///
/// Opening Lance needs a reactor and this function is sync, so it borrows the
/// atlas module's ONE sync bridge (`store::run_blocking`, the same one
/// `write_store_blocking` uses) rather than standing a second runtime up
/// (ARCH 10.6). The cost is a table open over tens-to-hundreds of rows, paid
/// only on a cache miss; the atoms.json parse above it is the larger term.
fn read_ann_summary(atlas_dir: &Path) -> Option<AnnSummary> {
    if !ann_table_present(atlas_dir) {
        return None;
    }
    let rows =
        super::store::run_blocking(async move { Ok::<_, String>(ann_table_rows(atlas_dir).await) })
            .ok()
            .flatten()?;
    Some(AnnSummary {
        embedded_atoms: rows,
    })
}

/// Project `atlas/ontology.json` into the summary's view. `None` when the
/// corpus declares nothing, which is also what a pre-ontology atlas reads as.
fn read_ontology_summary(atlas_dir: &Path) -> Option<OntologySummary> {
    let file = super::writer::read_atlas_ontology(atlas_dir)?;
    let p = &file.policies;
    if !p.has_declarations() {
        return None;
    }
    let mut identity_criteria = BTreeMap::new();
    for (name, keys) in &p.identity.identity {
        identity_criteria.insert(name.clone(), format!("external:{}", keys.join(",")));
    }
    for (name, keys) in &p.identity.identity_fallback {
        identity_criteria
            .entry(name.clone())
            .or_insert_with(|| format!("fallback:{}", keys.join(",")));
    }
    Some(OntologySummary {
        version: file.ontology_version,
        declared: p
            .shape
            .types
            .iter()
            .map(|t| {
                (
                    t.name.clone(),
                    serde_json::to_string(&t.kind)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string(),
                )
            })
            .collect(),
        clock: serde_json::to_string(&p.change.clock)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string(),
        identity_criteria,
        specializes: p
            .shape
            .types
            .iter()
            .filter_map(|t| t.specializes.clone().map(|parent| (t.name.clone(), parent)))
            .collect(),
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

/// Read `atlas/_summary.json` only when it is CURRENT for the live
/// `atoms.json` — same mtime, size and schema. Never computes and never
/// writes: the view for a host that promises not to write into an index
/// (`corpus-mcp`). `None` when there is no `atoms.json`, no cache, or the
/// cache is stale; [`read_or_compute_summary`] is the ONE other reader of the
/// cache key and goes through here, so the key is decided once.
pub fn read_current_summary(atlas_dir: &Path) -> Option<AtlasSummary> {
    let live_meta = fs::metadata(atlas_dir.join("atoms.json")).ok()?;
    let live_size = live_meta.len();
    let live_mtime_ms = live_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let cached = read_summary_file(atlas_dir)?;
    (cached.atoms_mtime_ms == live_mtime_ms
        && cached.atoms_size_bytes == live_size
        && cached.ann_mtime_ms == ann_table_mtime_ms(atlas_dir)
        && cached.schema_version == SCHEMA_VERSION)
        .then_some(cached)
}

fn read_summary_file(atlas_dir: &Path) -> Option<AtlasSummary> {
    let path = atlas_dir.join(SUMMARY_FILE);
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
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
