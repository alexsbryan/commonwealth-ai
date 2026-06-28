// SPDX-License-Identifier: AGPL-3.0-or-later
//! ATLAS_STORAGE_V2 — Stage 0: the v2 store **writer** (dormant).
//!
//! Writes the per-corpus v2 store beside the v1 rkyv archive at the same
//! lifecycle points (`build sidecar / post-install / CLI`), gated by the
//! `SOVEREIGN_ATLAS_STORE_V2` env so it is a no-op until explicitly enabled.
//! The reader is **unchanged** in this stage — `AtlasGraph` still loads
//! `atoms.rkyv`; Stage 1 swaps the read path. Building here, dormant, lets us
//! generate real v2 stores and prove parity ("lance row == rkyv atom") before
//! anything touches the hot read path.
//!
//! Two artifacts, per `ATLAS_STORAGE_V2.md` §A/§B:
//! - `atoms.lance` — columnar atom store. The hot scalar columns (the
//!   `atlas_navigate`/enumeration projection) + an interned local `id` (u32,
//!   for CSR compactness) + the lossless canonical `payload` JSON (deep reads,
//!   so `atoms.lance` can replace `atoms.json`). Atoms are projected through
//!   the SAME [`super::archive::project`] the rkyv writer uses.
//! - `edges.csr` — a plain little-endian CSR triple (offsets / neighbors /
//!   types / conf), out-edges and a symmetric in-edge CSR, over the interned
//!   local ids. mmap-friendly so the Stage-1 BFS inner loop stays sync.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{Float32Array, RecordBatch, StringArray, UInt32Array, UInt8Array};

use super::archive::{arch_edge_type, project, ArchEdgeType, AtomKindTag, AtomRecord};
use super::edges::Edge;
use super::AtomEnvelope;

/// v2 store schema version. Bump on any on-disk layout change (atoms.lance
/// columns or the edges.csr binary format).
pub const STORE_FORMAT_VERSION: u32 = 1;

/// The columnar atom store directory name (a Lance table under `atlas/`).
pub const ATOMS_LANCE_DIRNAME: &str = "atoms.lance";
/// The CSR edge file name.
pub const EDGES_CSR_FILENAME: &str = "edges.csr";
/// Lance table name created under the atlas dir (yields `atoms.lance`).
const ATOMS_TABLE: &str = "atoms";

const CSR_MAGIC: u32 = 0x4353_5256; // "CSRV"
const CSR_VERSION: u32 = 1;

/// Whether the Stage-0 v2 writer is enabled. Off by default — set
/// `SOVEREIGN_ATLAS_STORE_V2=1` to dual-write the v2 store beside the rkyv.
pub fn store_v2_enabled() -> bool {
    std::env::var("SOVEREIGN_ATLAS_STORE_V2")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

// ── atoms.lance ──────────────────────────────────────────────────────────────

/// One atom as v2 columns: the interned local id, the hot scalar fields, and
/// the lossless canonical JSON payload. Derived from [`AtomRecord`] so the
/// columns match the rkyv projection field-for-field.
#[derive(Debug, Clone, PartialEq)]
struct StoreRow {
    local_id: u32,
    str_id: String,
    kind: u8,
    name: String,
    label: String,
    content: String,
    subtype: String,
    description: String,
    excerpt: String,
    salience: f32,
    confidence: f32,
    payload: String,
}

impl StoreRow {
    fn from_record(local_id: u32, r: &AtomRecord) -> Self {
        StoreRow {
            local_id,
            str_id: r.id.clone(),
            kind: kind_u8(r.kind),
            name: r.name.clone(),
            label: r.label.clone(),
            content: r.content.clone(),
            subtype: r.subtype.clone(),
            description: r.description.clone(),
            excerpt: r.excerpt.clone(),
            salience: r.salience,
            confidence: r.confidence,
            payload: String::from_utf8_lossy(&r.payload).into_owned(),
        }
    }
}

/// Stable u8 discriminant for the atom kind. Order matches [`AtomKindTag`] and
/// the rkyv archive's tag, so the `kind` column round-trips losslessly.
fn kind_u8(k: AtomKindTag) -> u8 {
    match k {
        AtomKindTag::Entity => 0,
        AtomKindTag::Event => 1,
        AtomKindTag::State => 2,
        AtomKindTag::Relation => 3,
        AtomKindTag::Claim => 4,
        AtomKindTag::Question => 5,
        AtomKindTag::Configuration => 6,
        AtomKindTag::ArgumentReconstruction => 7,
        AtomKindTag::Position => 8,
        AtomKindTag::Opposition => 9,
        AtomKindTag::Asset => 10,
    }
}

/// Stable u8 discriminant for the edge type (mirrors [`ArchEdgeType`]).
fn edge_type_u8(t: ArchEdgeType) -> u8 {
    match t {
        ArchEdgeType::Transition => 0,
        ArchEdgeType::Causes => 1,
        ArchEdgeType::Grounds => 2,
        ArchEdgeType::Tension => 3,
        ArchEdgeType::Involves => 4,
        ArchEdgeType::Composes => 5,
        ArchEdgeType::Configures => 6,
        ArchEdgeType::Grounding => 7,
        ArchEdgeType::Framing => 8,
        ArchEdgeType::Provenance => 9,
        ArchEdgeType::EvidenceFor => 10,
        ArchEdgeType::Concedes => 11,
        ArchEdgeType::OpposesIn => 12,
        ArchEdgeType::Attaches => 13,
    }
}

fn atoms_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::UInt32, false),
        Field::new("str_id", DataType::Utf8, false),
        Field::new("kind", DataType::UInt8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("label", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("subtype", DataType::Utf8, false),
        Field::new("description", DataType::Utf8, false),
        Field::new("excerpt", DataType::Utf8, false),
        Field::new("salience", DataType::Float32, false),
        Field::new("confidence", DataType::Float32, false),
        Field::new("payload", DataType::Utf8, false),
    ]))
}

fn atoms_batch(rows: &[StoreRow], sch: &Arc<Schema>) -> Result<RecordBatch, String> {
    let str_col = |f: &dyn Fn(&StoreRow) -> &str| {
        Arc::new(StringArray::from(
            rows.iter().map(f).collect::<Vec<_>>(),
        )) as arrow_array::ArrayRef
    };
    let cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(UInt32Array::from(
            rows.iter().map(|r| r.local_id).collect::<Vec<_>>(),
        )),
        str_col(&|r| r.str_id.as_str()),
        Arc::new(UInt8Array::from(
            rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        )),
        str_col(&|r| r.name.as_str()),
        str_col(&|r| r.label.as_str()),
        str_col(&|r| r.content.as_str()),
        str_col(&|r| r.subtype.as_str()),
        str_col(&|r| r.description.as_str()),
        str_col(&|r| r.excerpt.as_str()),
        Arc::new(Float32Array::from(
            rows.iter().map(|r| r.salience).collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            rows.iter().map(|r| r.confidence).collect::<Vec<_>>(),
        )),
        str_col(&|r| r.payload.as_str()),
    ];
    RecordBatch::try_new(sch.clone(), cols).map_err(|e| format!("atoms record batch: {e}"))
}

async fn write_atoms_lance(atlas_dir: &Path, rows: &[StoreRow]) -> Result<PathBuf, String> {
    let lance_dir = atlas_dir.join(ATOMS_LANCE_DIRNAME);
    // Rebuild semantics: drop any prior table so this is a clean overwrite.
    if lance_dir.exists() {
        std::fs::remove_dir_all(&lance_dir)
            .map_err(|e| format!("remove stale {}: {e}", lance_dir.display()))?;
    }
    let uri = atlas_dir
        .to_str()
        .ok_or_else(|| format!("non-utf8 atlas dir {}", atlas_dir.display()))?;
    let db = lancedb::connect(uri)
        .execute()
        .await
        .map_err(|e| format!("lancedb connect {uri}: {e}"))?;
    let sch = atoms_schema();
    let tbl = db
        .create_empty_table(ATOMS_TABLE, sch.clone())
        .execute()
        .await
        .map_err(|e| format!("create atoms.lance: {e}"))?;
    // Batched add bounds peak memory on large atlases (wikipedia = 1.67M atoms).
    const BATCH: usize = 50_000;
    for chunk in rows.chunks(BATCH) {
        let rb = atoms_batch(chunk, &sch)?;
        tbl.add(vec![rb])
            .execute()
            .await
            .map_err(|e| format!("atoms.lance add: {e}"))?;
    }
    Ok(lance_dir)
}

// ── edges.csr ────────────────────────────────────────────────────────────────

/// A directed edge resolved to interned local ids (src, tgt, type, conf).
type LocalEdge = (u32, u32, u8, f32);

/// CSR offsets prefix-sum keyed by `key(edge)`. `sorted` must already be
/// sorted by that key so the neighbor array is grouped.
fn csr_offsets(n_atoms: u32, sorted: &[LocalEdge], key: impl Fn(&LocalEdge) -> u32) -> Vec<u32> {
    let mut off = vec![0u32; n_atoms as usize + 1];
    for e in sorted {
        off[key(e) as usize + 1] += 1;
    }
    for i in 1..off.len() {
        off[i] += off[i - 1];
    }
    off
}

fn pad_to_4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

/// Serialize one CSR direction (offsets, neighbors, types, conf) into `buf`.
/// `neighbor` selects which endpoint is the stored neighbor for this direction.
fn push_csr(buf: &mut Vec<u8>, offsets: &[u32], sorted: &[LocalEdge], neighbor: impl Fn(&LocalEdge) -> u32) {
    for o in offsets {
        buf.extend_from_slice(&o.to_le_bytes());
    }
    for e in sorted {
        buf.extend_from_slice(&neighbor(e).to_le_bytes());
    }
    for e in sorted {
        buf.push(e.2);
    }
    pad_to_4(buf);
    for e in sorted {
        buf.extend_from_slice(&e.3.to_le_bytes());
    }
}

/// Build and write `edges.csr` (out-CSR + symmetric in-CSR) over the interned
/// local ids. Edges whose endpoints are not in `by_id` (dangling) are skipped.
fn write_edges_csr(
    atlas_dir: &Path,
    n_atoms: u32,
    by_id: &HashMap<String, u32>,
    edges: &[Edge],
) -> Result<PathBuf, String> {
    let mut valid: Vec<LocalEdge> = Vec::with_capacity(edges.len());
    for e in edges {
        let (Some(&s), Some(&t)) = (by_id.get(e.source.as_str()), by_id.get(e.target.as_str()))
        else {
            continue;
        };
        valid.push((s, t, edge_type_u8(arch_edge_type(e.edge_type)), e.confidence));
    }
    let n_edges = valid.len() as u32;

    let mut out = valid.clone();
    out.sort_by_key(|e| e.0);
    let out_off = csr_offsets(n_atoms, &out, |e| e.0);

    let mut inn = valid;
    inn.sort_by_key(|e| e.1);
    let in_off = csr_offsets(n_atoms, &inn, |e| e.1);

    let mut buf = Vec::with_capacity(32 + (n_atoms as usize + 1) * 8 + n_edges as usize * 18);
    buf.extend_from_slice(&CSR_MAGIC.to_le_bytes());
    buf.extend_from_slice(&CSR_VERSION.to_le_bytes());
    buf.extend_from_slice(&n_atoms.to_le_bytes());
    buf.extend_from_slice(&n_edges.to_le_bytes());
    // out-CSR: neighbor is the target.
    push_csr(&mut buf, &out_off, &out, |e| e.1);
    // in-CSR: neighbor is the source.
    push_csr(&mut buf, &in_off, &inn, |e| e.0);

    let path = atlas_dir.join(EDGES_CSR_FILENAME);
    let tmp = path.with_extension("csr.tmp");
    std::fs::write(&tmp, &buf)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

// ── public lifecycle entry points ────────────────────────────────────────────

/// Build the v2 store from in-memory atoms + edges and write `atoms.lance` +
/// `edges.csr` into `atlas_dir`. Returns the `atoms.lance` path. Atoms are
/// interned to local u32 ids in iteration order; `edges.csr` references them.
pub async fn write_store(
    atlas_dir: &Path,
    _corpus_id: &str,
    atoms: &[AtomEnvelope],
    edges: &[Edge],
) -> Result<PathBuf, String> {
    let mut by_id: HashMap<String, u32> = HashMap::with_capacity(atoms.len());
    let mut rows: Vec<StoreRow> = Vec::with_capacity(atoms.len());
    for (i, atom) in atoms.iter().enumerate() {
        let rec = project(atom);
        let local_id = i as u32;
        by_id.insert(rec.id.clone(), local_id);
        rows.push(StoreRow::from_record(local_id, &rec));
    }
    let lance = write_atoms_lance(atlas_dir, &rows).await?;
    write_edges_csr(atlas_dir, atoms.len() as u32, &by_id, edges)?;
    Ok(lance)
}

/// Read `atoms.json` (+ `edges.json`) from `atlas_dir`, build the v2 store, and
/// write it beside them. The disk-reading lifecycle entry (post-install / CLI),
/// mirroring [`super::archive::build_and_write_archive`].
pub async fn build_and_write_store(atlas_dir: &Path, corpus_id: &str) -> Result<PathBuf, String> {
    let atoms = super::read_atlas_atoms(atlas_dir)
        .map_err(|e| format!("read atoms.json for {corpus_id}: {e}"))?;
    let edges = super::read_atlas_edges(atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    write_store(atlas_dir, corpus_id, &atoms.atoms, &edges).await
}

/// Sync bridge for the disk-reading entry — runs the async build on a dedicated
/// thread with its own runtime, so it is safe to call from sync code whether or
/// not an ambient tokio runtime exists (no nested-runtime panic).
pub fn build_and_write_store_blocking(atlas_dir: &Path, corpus_id: &str) -> Result<PathBuf, String> {
    run_blocking(build_and_write_store(atlas_dir, corpus_id))
}

/// Sync bridge for the in-memory entry (the build sidecar already holds the
/// atoms/edges, so it skips the disk re-read).
pub fn write_store_blocking(
    atlas_dir: &Path,
    corpus_id: &str,
    atoms: &[AtomEnvelope],
    edges: &[Edge],
) -> Result<PathBuf, String> {
    run_blocking(write_store(atlas_dir, corpus_id, atoms, edges))
}

/// Drive a Lance (tokio) future to completion from sync code. Lance requires a
/// tokio reactor; running on a fresh, dedicated-thread multi-thread runtime
/// avoids both the "runtime within a runtime" panic and `block_in_place`'s
/// flavor constraints. The dormant gate means this is off the default path.
fn run_blocking<F>(fut: F) -> Result<PathBuf, String>
where
    F: std::future::Future<Output = Result<PathBuf, String>> + Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|e| format!("v2 store runtime: {e}"))?
                    .block_on(fut)
            })
            .join()
            .unwrap_or_else(|_| Err("v2 store build thread panicked".to_string()))
    })
}

/// True if the v2 store is missing or older than `atoms.json` — i.e. it should
/// be (re)built. Mirrors [`super::archive::archive_needs_build`].
pub fn store_needs_build(atlas_dir: &Path) -> bool {
    let lance = atlas_dir.join(ATOMS_LANCE_DIRNAME);
    if !lance.exists() {
        return true;
    }
    let mtime = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (
        mtime(atlas_dir.join("atoms.json")),
        mtime(lance.join("_versions")).or_else(|| mtime(lance.clone())),
    ) {
        (Some(json_t), Some(lance_t)) => json_t > lance_t,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::Entity;
    use crate::enrichment::atlas::edges::{EdgeProvenance, EdgeType};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    use arrow_array::Array;
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;
    use crate::enrichment::atlas::{AtomId, ChunkRef, Edge, EdgeId};

    fn entity(idx: usize, name: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: vec![format!("{name}-alias")],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(format!("sec_{idx:04}"), None),
            description: format!("description of {name}"),
            defining_quote: None,
            salience: 0.6,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    /// Read atoms.lance back into [`StoreRow`]s, sorted by local id (Lance does
    /// not guarantee scan order).
    async fn read_atoms_lance(atlas_dir: &Path) -> Vec<StoreRow> {
        let db = lancedb::connect(atlas_dir.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let tbl = db.open_table(ATOMS_TABLE).execute().await.unwrap();
        let batches: Vec<RecordBatch> = tbl
            .query()
            .execute()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let u32c = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .clone()
        };
        let u8c = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .clone()
        };
        let f32c = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .clone()
        };
        let strc = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .clone()
        };
        let mut rows = Vec::new();
        for b in &batches {
            let (id, kind, sal, conf) = (u32c(b, "id"), u8c(b, "kind"), f32c(b, "salience"), f32c(b, "confidence"));
            let (sid, name, label, content) = (strc(b, "str_id"), strc(b, "name"), strc(b, "label"), strc(b, "content"));
            let (subtype, desc, exc, payload) = (strc(b, "subtype"), strc(b, "description"), strc(b, "excerpt"), strc(b, "payload"));
            for i in 0..b.num_rows() {
                rows.push(StoreRow {
                    local_id: id.value(i),
                    str_id: sid.value(i).to_string(),
                    kind: kind.value(i),
                    name: name.value(i).to_string(),
                    label: label.value(i).to_string(),
                    content: content.value(i).to_string(),
                    subtype: subtype.value(i).to_string(),
                    description: desc.value(i).to_string(),
                    excerpt: exc.value(i).to_string(),
                    salience: sal.value(i),
                    confidence: conf.value(i),
                    payload: payload.value(i).to_string(),
                });
            }
        }
        rows.sort_by_key(|r| r.local_id);
        rows
    }

    /// Reconstruct the out-edge set from edges.csr.
    fn read_out_edges(path: &Path) -> Vec<LocalEdge> {
        let bytes = std::fs::read(path).unwrap();
        let mut p = 0usize;
        let u32_at = |b: &[u8], p: &mut usize| {
            let v = u32::from_le_bytes(b[*p..*p + 4].try_into().unwrap());
            *p += 4;
            v
        };
        assert_eq!(u32_at(&bytes, &mut p), CSR_MAGIC);
        assert_eq!(u32_at(&bytes, &mut p), CSR_VERSION);
        let n_atoms = u32_at(&bytes, &mut p);
        let n_edges = u32_at(&bytes, &mut p);
        let mut off = Vec::with_capacity(n_atoms as usize + 1);
        for _ in 0..=n_atoms {
            off.push(u32_at(&bytes, &mut p));
        }
        let mut nbr = Vec::with_capacity(n_edges as usize);
        for _ in 0..n_edges {
            nbr.push(u32_at(&bytes, &mut p));
        }
        let typ: Vec<u8> = bytes[p..p + n_edges as usize].to_vec();
        p += n_edges as usize;
        while p % 4 != 0 {
            p += 1;
        }
        let mut conf = Vec::with_capacity(n_edges as usize);
        for _ in 0..n_edges {
            conf.push(f32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()));
            p += 4;
        }
        let mut edges = Vec::new();
        for i in 0..n_atoms as usize {
            for j in off[i] as usize..off[i + 1] as usize {
                edges.push((i as u32, nbr[j], typ[j], conf[j]));
            }
        }
        edges
    }

    #[tokio::test]
    async fn lance_row_equals_rkyv_atom_on_a_frozen_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let atoms = vec![
            entity(1, "Alice"),
            entity(2, "Bob"),
            entity(3, "Carol"),
        ];
        let edges = vec![Edge {
            id: EdgeId::from_raw("edge-0001"),
            edge_type: EdgeType::Involves,
            source: AtomId::entity(1),
            target: AtomId::entity(3),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 0.9,
            provenance: EdgeProvenance::Derived,
        }];

        write_store(dir, "frozen", &atoms, &edges).await.unwrap();

        // PARITY: each lance row equals the rkyv projection of the same atom.
        let rows = read_atoms_lance(dir).await;
        assert_eq!(rows.len(), atoms.len());
        for (i, atom) in atoms.iter().enumerate() {
            let expect = StoreRow::from_record(i as u32, &project(atom));
            assert_eq!(rows[i], expect, "lance row {i} != rkyv projection");
            // Payload is lossless: it deserializes back to the original atom.
            let back: AtomEnvelope = serde_json::from_str(&rows[i].payload).unwrap();
            assert_eq!(back.id().as_str(), atom.id().as_str());
        }
        // Entity kind + scalar columns land as projected.
        assert!(rows.iter().all(|r| r.kind == 0)); // all Entity
        assert_eq!(rows[0].name, "Alice");
        assert_eq!(rows[1].name, "Bob");
        assert_eq!(rows[2].name, "Carol");
        assert_eq!(rows[0].description, "description of Alice");

        // edges.csr round-trips the single edge over interned local ids
        // (entity-1 → entity-3 = local 0 → local 2, type Involves=4).
        let out = read_out_edges(&dir.join(EDGES_CSR_FILENAME));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 0);
        assert_eq!(out[0].1, 2);
        assert_eq!(out[0].2, edge_type_u8(ArchEdgeType::Involves));
        assert!((out[0].3 - 0.9).abs() < 1e-6);
    }

    #[test]
    fn gate_is_off_by_default() {
        // Sanity: without the env, the writer is dormant.
        std::env::remove_var("SOVEREIGN_ATLAS_STORE_V2");
        assert!(!store_v2_enabled());
    }
}
