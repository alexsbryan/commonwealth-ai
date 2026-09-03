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
const SCHEMA_VERSION: u32 = 4;

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
        super::super::writer::write_atlas_ontology(tmp.path(), 1, &policies).unwrap();

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
