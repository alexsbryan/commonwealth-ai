// SPDX-License-Identifier: AGPL-3.0-or-later
//! The resolved-atlas SUMMARY — the derived per-corpus statistics and their
//! cache READ.
//!
//! Carved out of corpus-engine's `atlas::summary` (FIVE_PROGRAMS §12 decision
//! 1, 2026-09-21). The leaf owns the summary SHAPE, the one derivation
//! ([`compute_summary`]) and the one cache-KEY decision
//! ([`read_current_summary`]); corpus-engine keeps `read_or_compute_summary`
//! — the orchestrator that PERSISTS the cache best-effort — because this leaf
//! never writes the atlas. The types ride in mesh gossip, so they are serde
//! only, no engine.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ann_store::{ann_table_mtime_ms, ann_table_present, ann_table_rows};
use crate::raw::{atoms_content_hash, read_atlas_ontology};
use crate::store::{csr_edge_counts, csr_mtime_ms};
use tracing::debug;
use understanding_vocab::atoms::{AtomEnvelope, AtomType};
use understanding_vocab::edges::EdgeType;
use understanding_vocab::read::read_atlas_atoms;
use understanding_vocab::taxonomy::EnrichmentDepth;

pub const SUMMARY_FILE: &str = "_summary.json";
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
// v6 (2026-09-08) adds `edge_counts` + `csr_mtime_ms`: edges per kind as the
// CSR holds them, for the navigation-row admissibility check
// (`inventory.rs`) and `svrn atlas map-check`. The CSR is a third INPUT, so
// its mtime is a third KEY, for the reason the seed table's is.
pub const SCHEMA_VERSION: u32 = 6;

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
    /// Edges per kind in `edges.csr` — the graph the walk follows, so an
    /// edge to a chunk reference (which has no seat in the CSR) is not
    /// counted as walkable. `None` when there is no READABLE store: no CSR,
    /// or one at a superseded format version, which `CsrEdges::open` refuses
    /// and the walk therefore cannot open either — 662 of 1,770 installed
    /// SEP siblings on 2026-09-08, all at CSR v1. That is a different fact
    /// from `Some({})`, a built store with no edges, and it is never
    /// rendered as one (ARCH 18.3); `svrn atlas migrate-all` rebuilds it.
    /// Added in schema v6; read by `inventory::AtlasInventory`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_counts: Option<BTreeMap<EdgeType, u64>>,
    /// `edges.csr` mtime (ms since epoch) when computed; `0` without one.
    /// The THIRD cache key, beside the atoms file's and the seed table's.
    #[serde(default)]
    pub csr_mtime_ms: u64,
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
            edge_counts: None,
            csr_mtime_ms: 0,
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
    let atom_count = atoms.atoms().len() as u64;

    // Single pass over atoms — count tier-2 (extracted entities) and
    // per-type totals together. The previous code iterated only
    // entities for tier2; the type-counter folds in the other seven
    // variants without a second pass.
    let mut tier2_count: u64 = 0;
    let mut atom_counts: BTreeMap<AtomType, u64> = BTreeMap::new();
    for a in atoms.atoms() {
        if let AtomEnvelope::Entity(e) = a {
            if matches!(e.enrichment_depth, EnrichmentDepth::Extracted) {
                tier2_count += 1;
            }
        }
        *atom_counts.entry(a.atom_type()).or_insert(0) += 1;
    }
    // The declared-subtype census comes from the ONE tally, which the
    // ontology-coverage rollup also calls — two readers, one count (§10.6).
    let (subtype_counts, unsubtyped) = crate::projection::subtype_tally(atoms.atoms());
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
        edge_counts: csr_edge_counts(atlas_dir),
        csr_mtime_ms: csr_mtime_ms(atlas_dir),
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
        crate::store::run_blocking(async move { Ok::<_, String>(ann_table_rows(atlas_dir).await) })
            .ok()
            .flatten()?;
    Some(AnnSummary {
        embedded_atoms: rows,
    })
}

/// Project `atlas/ontology.json` into the summary's view. `None` when the
/// corpus declares nothing, which is also what a pre-ontology atlas reads as.
fn read_ontology_summary(atlas_dir: &Path) -> Option<OntologySummary> {
    let file = crate::raw::read_atlas_ontology(atlas_dir)?;
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

/// Read `atlas/_summary.json` only when it is CURRENT for the live
/// `atoms.json` — same mtime, size and schema. Never computes and never
/// writes: the view for a host that promises not to write into an index
/// (corpus-mcp). `None` when there is no `atoms.json`, no cache, or the
/// cache is stale; corpus-engine's `read_or_compute_summary` is the ONE other
/// reader of the cache key and goes through here, so the key is decided once.
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
        && cached.csr_mtime_ms == csr_mtime_ms(atlas_dir)
        && cached.schema_version == SCHEMA_VERSION)
        .then_some(cached)
}

fn read_summary_file(atlas_dir: &Path) -> Option<AtlasSummary> {
    let path = atlas_dir.join(SUMMARY_FILE);
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}
