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
use arrow_array::{Array, Float32Array, RecordBatch, StringArray, UInt32Array, UInt8Array};
use futures::TryStreamExt;
use lancedb::query::ExecutableQuery;
use memmap2::Mmap;

use super::archive::{arch_edge_type, project, ArchEdgeType, AtomKindTag, AtomRecord};
use super::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use super::{AtomEnvelope, AtomId};

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

/// Inverse of [`edge_type_u8`] — the `edges.csr` type byte back to an
/// [`EdgeType`]. Unknown bytes fall back to `Involves` (a benign medium-weight
/// edge) rather than panicking on a corrupt file.
fn u8_to_edge_type(u: u8) -> EdgeType {
    match u {
        0 => EdgeType::Transition,
        1 => EdgeType::Causes,
        2 => EdgeType::Grounds,
        3 => EdgeType::Tension,
        4 => EdgeType::Involves,
        5 => EdgeType::Composes,
        6 => EdgeType::Configures,
        7 => EdgeType::Grounding,
        8 => EdgeType::Framing,
        9 => EdgeType::Provenance,
        10 => EdgeType::EvidenceFor,
        11 => EdgeType::Concedes,
        12 => EdgeType::OpposesIn,
        13 => EdgeType::Attaches,
        _ => EdgeType::Involves,
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

/// mmap'd reader over `edges.csr` — the v2 CSR edge file written by
/// [`write_edges_csr`]. Sync and paged: the `atlas_navigate` BFS inner loop
/// (ATLAS_STORAGE_V2 Stage 1) reads adjacency without faulting the whole file
/// resident, the v2 "edges stay sync mmap" invariant. Neighbors are interned
/// local u32 ids; the caller maps them back to atom-id strings via the
/// `atoms.lance` `id`/`str_id` columns.
pub struct CsrEdges {
    mmap: Mmap,
    n_atoms: u32,
    n_edges: u32,
    out: CsrDir,
    inn: CsrDir,
}

/// Byte offsets of one CSR direction's four arrays within the mmap.
struct CsrDir {
    off: usize,
    nbr: usize,
    typ: usize,
    conf: usize,
}

impl CsrEdges {
    /// Open + validate (magic, version, length). The pointer arithmetic
    /// mirrors [`write_edges_csr`]'s layout exactly.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file =
            std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        // SAFETY: edges.csr is a build-produced artifact (atomic tmp+rename),
        // immutable for the reader's lifetime — same trust model as the rkyv mmap.
        let mmap =
            unsafe { Mmap::map(&file) }.map_err(|e| format!("mmap {}: {e}", path.display()))?;
        let b = &mmap[..];
        if b.len() < 16 {
            return Err(format!("edges.csr too small: {} bytes", b.len()));
        }
        let rd = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        if rd(0) != CSR_MAGIC {
            return Err("edges.csr: bad magic".to_string());
        }
        let version = rd(4);
        if version != CSR_VERSION {
            return Err(format!(
                "edges.csr schema v{version} != reader v{CSR_VERSION}"
            ));
        }
        let n_atoms = rd(8);
        let n_edges = rd(12);
        let off_len = (n_atoms as usize + 1) * 4;
        let nbr_len = n_edges as usize * 4;
        let typ_len = n_edges as usize;
        let typ_pad = (4 - (typ_len % 4)) % 4;
        let conf_len = n_edges as usize * 4;
        let dir_len = off_len + nbr_len + typ_len + typ_pad + conf_len;
        let dir_at = |start: usize| CsrDir {
            off: start,
            nbr: start + off_len,
            typ: start + off_len + nbr_len,
            conf: start + off_len + nbr_len + typ_len + typ_pad,
        };
        let out_start = 16;
        let in_start = out_start + dir_len;
        let need = in_start + dir_len;
        if b.len() < need {
            return Err(format!("edges.csr truncated: {} < {need} bytes", b.len()));
        }
        Ok(CsrEdges {
            mmap,
            n_atoms,
            n_edges,
            out: dir_at(out_start),
            inn: dir_at(in_start),
        })
    }

    pub fn n_atoms(&self) -> u32 {
        self.n_atoms
    }

    pub fn n_edges(&self) -> u32 {
        self.n_edges
    }

    /// Out-edges of `local_id`: `(neighbor_local_id, edge_type_u8, confidence)`.
    /// Empty if `local_id` is out of range.
    pub fn out_edges(&self, local_id: u32) -> Vec<(u32, u8, f32)> {
        self.read_dir(&self.out, local_id)
    }

    /// In-edges of `local_id` — who points at it.
    pub fn in_edges(&self, local_id: u32) -> Vec<(u32, u8, f32)> {
        self.read_dir(&self.inn, local_id)
    }

    fn read_dir(&self, d: &CsrDir, local_id: u32) -> Vec<(u32, u8, f32)> {
        if local_id >= self.n_atoms {
            return Vec::new();
        }
        let b = &self.mmap[..];
        let rdu = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let rdf = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let lo = rdu(d.off + local_id as usize * 4) as usize;
        let hi = rdu(d.off + (local_id as usize + 1) * 4) as usize;
        (lo..hi)
            .map(|j| (rdu(d.nbr + j * 4), b[d.typ + j], rdf(d.conf + j * 4)))
            .collect()
    }
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

/// Reconstruct the v1 rkyv archive bytes from the v2 store (`atoms.lance` +
/// `edges.csr`) — the ATLAS_STORAGE_V2 Increment-C eval arm.
///
/// Reads every atom's canonical `payload` back into an `AtomEnvelope` and the
/// `edges.csr` adjacency back into `Edge`s (over the interned local ids), then
/// rebuilds the archive via the SAME [`super::archive::build_atlas_archive_bytes`]
/// the rkyv writer uses. Loading the result through the existing rkyv read path
/// lets `atlas_navigate` run over the v2 store unchanged, proving end-to-end
/// that the store is retrieval-complete and the v2 seeding is neutral — without
/// touching the daemon's `AtlasGraph`. (The production direct-read reader is a
/// follow-on; its atom-level data correctness is already covered by the B
/// parity test.)
pub async fn reconstruct_archive_bytes(
    atlas_dir: &Path,
    corpus_id: &str,
) -> Result<Vec<u8>, String> {
    let uri = atlas_dir
        .to_str()
        .ok_or_else(|| format!("non-utf8 atlas dir {}", atlas_dir.display()))?;
    let db = lancedb::connect(uri)
        .execute()
        .await
        .map_err(|e| format!("connect {uri}: {e}"))?;
    let tbl = db
        .open_table(ATOMS_TABLE)
        .execute()
        .await
        .map_err(|e| format!("open atoms.lance: {e}"))?;
    let batches: Vec<RecordBatch> = tbl
        .query()
        .execute()
        .await
        .map_err(|e| format!("scan atoms.lance: {e}"))?
        .try_collect()
        .await
        .map_err(|e| format!("collect atoms.lance: {e}"))?;

    // (local_id, str_id, atom) — re-sorted to the interned local order so the
    // CSR neighbor ids index correctly.
    let mut by_local: Vec<(u32, String, AtomEnvelope)> = Vec::new();
    for b in &batches {
        let col = |n: &str| b.column_by_name(n);
        let ids = col("id")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>())
            .ok_or("atoms.lance missing id column")?;
        let sids = col("str_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .ok_or("atoms.lance missing str_id column")?;
        let payloads = col("payload")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .ok_or("atoms.lance missing payload column")?;
        for i in 0..b.num_rows() {
            let env: AtomEnvelope = serde_json::from_str(payloads.value(i))
                .map_err(|e| format!("parse atom payload: {e}"))?;
            by_local.push((ids.value(i), sids.value(i).to_string(), env));
        }
    }
    by_local.sort_by_key(|x| x.0);
    let local_to_str: Vec<String> = by_local.iter().map(|x| x.1.clone()).collect();
    let atoms: Vec<AtomEnvelope> = by_local.into_iter().map(|x| x.2).collect();

    let csr = CsrEdges::open(&atlas_dir.join(EDGES_CSR_FILENAME))?;
    let mut edges: Vec<Edge> = Vec::new();
    for local in 0..csr.n_atoms() {
        let Some(src) = local_to_str.get(local as usize) else {
            continue;
        };
        for (nbr, ty, conf) in csr.out_edges(local) {
            let Some(tgt) = local_to_str.get(nbr as usize) else {
                continue;
            };
            edges.push(Edge {
                id: EdgeId::from_raw(format!("csr-{}", edges.len())),
                edge_type: u8_to_edge_type(ty),
                source: AtomId::from_raw(src.clone()),
                target: AtomId::from_raw(tgt.clone()),
                evidence: vec![],
                trigger_event: None,
                sub_question: None,
                confidence: conf,
                provenance: EdgeProvenance::Derived,
            });
        }
    }

    let slug = corpus_id.strip_prefix("sep-").unwrap_or(corpus_id);
    super::archive::build_atlas_archive_bytes(corpus_id, slug, &atoms, &edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::Entity;
    use crate::enrichment::atlas::edges::{EdgeProvenance, EdgeType};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
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

    #[test]
    fn csr_edges_reader_roundtrips_the_writer() {
        // Stage-1 foundation: the mmap CsrEdges reader reconstructs exactly
        // what write_edges_csr wrote, over interned local ids.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // 3 atoms → local 0,1,2; edges entity1→entity3 (Involves) and
        // entity2→entity3 (Causes), so local 2 has two in-edges, no out-edges.
        let mut by_id = HashMap::new();
        for (i, n) in [1usize, 2, 3].iter().enumerate() {
            by_id.insert(AtomId::entity(*n).as_str().to_string(), i as u32);
        }
        let mk = |src: usize, tgt: usize, ty: EdgeType, conf: f32, id: &str| Edge {
            id: EdgeId::from_raw(id),
            edge_type: ty,
            source: AtomId::entity(src),
            target: AtomId::entity(tgt),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: conf,
            provenance: EdgeProvenance::Derived,
        };
        let edges = vec![
            mk(1, 3, EdgeType::Involves, 0.9, "e1"),
            mk(2, 3, EdgeType::Causes, 0.5, "e2"),
        ];
        write_edges_csr(dir, 3, &by_id, &edges).unwrap();

        let csr = CsrEdges::open(&dir.join(EDGES_CSR_FILENAME)).unwrap();
        assert_eq!(csr.n_atoms(), 3);
        assert_eq!(csr.n_edges(), 2);

        // out-edges: local 0 → local 2 (Involves, 0.9); local 2 has none.
        let o0 = csr.out_edges(0);
        assert_eq!(o0.len(), 1);
        assert_eq!(o0[0].0, 2);
        assert_eq!(o0[0].1, edge_type_u8(ArchEdgeType::Involves));
        assert!((o0[0].2 - 0.9).abs() < 1e-6);
        assert_eq!(csr.out_edges(2).len(), 0);

        // in-edges of local 2: from local 0 and local 1.
        let i2 = csr.in_edges(2);
        assert_eq!(i2.len(), 2);
        let nbrs: std::collections::HashSet<u32> = i2.iter().map(|e| e.0).collect();
        assert!(nbrs.contains(&0) && nbrs.contains(&1));
        assert_eq!(csr.in_edges(0).len(), 0);

        // out-of-range local id → empty, not a panic.
        assert_eq!(csr.out_edges(99).len(), 0);
    }

    #[tokio::test]
    async fn reconstruct_roundtrips_store_to_archive() {
        // The Increment-C eval arm: v2 store → rkyv archive bytes → access.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let atoms = vec![entity(1, "Alice"), entity(2, "Bob"), entity(3, "Carol")];
        let edges = vec![Edge {
            id: EdgeId::from_raw("e1"),
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

        let bytes = reconstruct_archive_bytes(dir, "frozen").await.unwrap();
        let arch = rkyv::access::<
            crate::enrichment::atlas::archive::ArchivedAtlasArchiveData,
            rkyv::rancor::Error,
        >(&bytes)
        .unwrap();
        assert_eq!(arch.atoms.len(), 3);
        assert_eq!(arch.edges.len(), 1); // entity1→entity3, both endpoints present
        assert!(arch.by_id.get(AtomId::entity(1).as_str()).is_some());
        assert!(arch.by_id.get(AtomId::entity(3).as_str()).is_some());
    }
}
