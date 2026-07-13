// SPDX-License-Identifier: AGPL-3.0-or-later
//! ATLAS_STORAGE_V2 — the per-corpus v2 store (writer + reader).
//!
//! The v2 store is the sole atom backend. It is written at every atlas
//! lifecycle point (`build sidecar / post-install / CLI`) and read directly by
//! `AtlasGraph`. It replaced the v1 `atoms.rkyv` archive, which was retired once
//! every corpus had migrated (see `ATLAS_STORAGE_V2.md`); there is no fallback —
//! a corpus without a v2 store reports no atlas. Writes are fail-hard, with
//! `atoms.json` retained as the canonical export and `sovereign atlas
//! migrate-all` as the rebuild path.
//!
//! Two artifacts, per `ATLAS_STORAGE_V2.md` §A/§B:
//! - `atoms.lance` — columnar atom store. The hot scalar columns (the
//!   `atlas_navigate`/enumeration projection) + an interned local `id` (u32,
//!   for CSR compactness) + the lossless canonical `payload` JSON (deep reads,
//!   so `atoms.lance` replaces `atoms.json` for the reader). Atoms are projected
//!   through [`super::projection::project`].
//! - `edges.csr` — a plain little-endian CSR triple (offsets / neighbors /
//!   types / conf), out-edges and a symmetric in-edge CSR, over the interned
//!   local ids. mmap-friendly so the BFS inner loop stays sync.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{Array, Float32Array, RecordBatch, StringArray, UInt32Array, UInt8Array};
use futures::TryStreamExt;
use lancedb::query::ExecutableQuery;
use memmap2::Mmap;

use super::edges::{Edge, EdgeProvenance, EdgeType};
use super::projection::{arch_edge_type, project, ArchEdgeType, AtomKindTag, AtomRecord};
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
                                    // v2 adds a per-edge provenance byte alongside the type byte. The Stage-1 BFS
                                    // can't distinguish a `scip_structural` call edge from a `containment_structural`
                                    // parent edge by `edge_type` alone (both are `Involves`); provenance is the
                                    // ground-truth discriminant the code-atlas CallChain filters on. Bumping the
                                    // version means a v1 `edges.csr` is rejected by `CsrEdges::open`; rebuild the
                                    // store with `sovereign atlas migrate-all <id>` (the v2 store is the only read
                                    // path after the v2 cleanup — there is no rkyv fallback).
const CSR_VERSION: u32 = 2;

// ── atoms.lance ──────────────────────────────────────────────────────────────

/// One atom as v2 columns: the interned local id, the hot scalar fields, and
/// the lossless canonical JSON payload. Derived from [`AtomRecord`] so the
/// columns track the canonical projection field-for-field.
#[derive(Debug, Clone, PartialEq)]
pub struct AtomRow {
    pub local_id: u32,
    pub str_id: String,
    pub kind: u8,
    pub name: String,
    pub label: String,
    pub content: String,
    pub subtype: String,
    pub description: String,
    pub excerpt: String,
    pub salience: f32,
    pub confidence: f32,
    /// Lossless canonical `AtomEnvelope` JSON — the deep-read column.
    pub payload: String,
}

impl AtomRow {
    /// Re-parse the full [`AtomEnvelope`] from the payload column. `None` on an
    /// empty payload or parse failure. The cold deep read for the direct-read
    /// reader (hot fields are the scalar columns above).
    pub fn atom_envelope(&self) -> Option<AtomEnvelope> {
        if self.payload.is_empty() {
            return None;
        }
        serde_json::from_str(&self.payload).ok()
    }

    fn from_record(local_id: u32, r: &AtomRecord) -> Self {
        AtomRow {
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

/// Stable u8 discriminant for the atom kind. Order matches [`AtomKindTag`], so
/// the `kind` column round-trips losslessly.
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

/// Stable u8 discriminant for the edge provenance — the v2 CSR's per-edge
/// provenance byte. Distinct from the type byte: a code-atlas edge is always
/// `EdgeType::Involves`, so the CallChain over the CSR uses THIS to keep only
/// `ScipStructural` (call/use/impl) edges and drop `ContainmentStructural`
/// (Crate→Module→Item) and `CargoStructural` (dependency) edges.
fn prov_u8(p: EdgeProvenance) -> u8 {
    match p {
        EdgeProvenance::LlmExtraction => 0,
        EdgeProvenance::LlmPairwise => 1,
        EdgeProvenance::LlmConfiguration => 2,
        EdgeProvenance::Derived => 3,
        EdgeProvenance::WikilinkStructural => 4,
        EdgeProvenance::ContainmentStructural => 5,
        EdgeProvenance::ScipStructural => 6,
        EdgeProvenance::CargoStructural => 7,
        EdgeProvenance::TreeSitterStructural => 8,
    }
}

/// Inverse of [`prov_u8`]. Unknown bytes fall back to `Derived` (a benign,
/// non-structural provenance) rather than panicking on a corrupt file.
fn u8_to_prov(u: u8) -> EdgeProvenance {
    match u {
        0 => EdgeProvenance::LlmExtraction,
        1 => EdgeProvenance::LlmPairwise,
        2 => EdgeProvenance::LlmConfiguration,
        3 => EdgeProvenance::Derived,
        4 => EdgeProvenance::WikilinkStructural,
        5 => EdgeProvenance::ContainmentStructural,
        6 => EdgeProvenance::ScipStructural,
        7 => EdgeProvenance::CargoStructural,
        8 => EdgeProvenance::TreeSitterStructural,
        _ => EdgeProvenance::Derived,
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

fn atoms_batch(rows: &[AtomRow], sch: &Arc<Schema>) -> Result<RecordBatch, String> {
    let str_col = |f: &dyn Fn(&AtomRow) -> &str| {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>())) as arrow_array::ArrayRef
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

async fn write_atoms_lance(atlas_dir: &Path, rows: &[AtomRow]) -> Result<PathBuf, String> {
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

/// A directed edge resolved to interned local ids (src, tgt, type, conf, prov).
type LocalEdge = (u32, u32, u8, f32, u8);

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
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

/// Serialize one CSR direction into `buf`, in order: offsets, neighbors, type
/// bytes, provenance bytes, [pad to 4], confidence f32s. The two byte arrays
/// (type, prov) sit adjacent so a single pad re-aligns the f32 conf array.
/// `neighbor` selects which endpoint is the stored neighbor for this direction.
fn push_csr(
    buf: &mut Vec<u8>,
    offsets: &[u32],
    sorted: &[LocalEdge],
    neighbor: impl Fn(&LocalEdge) -> u32,
) {
    for o in offsets {
        buf.extend_from_slice(&o.to_le_bytes());
    }
    for e in sorted {
        buf.extend_from_slice(&neighbor(e).to_le_bytes());
    }
    for e in sorted {
        buf.push(e.2); // edge type
    }
    for e in sorted {
        buf.push(e.4); // edge provenance
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
        valid.push((
            s,
            t,
            edge_type_u8(arch_edge_type(e.edge_type)),
            e.confidence,
            prov_u8(e.provenance),
        ));
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
/// reads adjacency without faulting the whole file
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

/// Byte offsets of one CSR direction's five arrays within the mmap.
struct CsrDir {
    off: usize,
    nbr: usize,
    typ: usize,
    prov: usize,
    conf: usize,
}

impl CsrEdges {
    /// Open + validate (magic, version, length). The pointer arithmetic
    /// mirrors [`write_edges_csr`]'s layout exactly.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file =
            std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        // SAFETY: edges.csr is a build-produced artifact (atomic tmp+rename),
        // immutable for the reader's lifetime (atomic tmp+rename, never rewritten in place).
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
        let prov_len = n_edges as usize;
        // type + prov are adjacent byte arrays; pad once to re-align the f32 conf.
        let byte_pad = (4 - ((typ_len + prov_len) % 4)) % 4;
        let conf_len = n_edges as usize * 4;
        let dir_len = off_len + nbr_len + typ_len + prov_len + byte_pad + conf_len;
        let dir_at = |start: usize| CsrDir {
            off: start,
            nbr: start + off_len,
            typ: start + off_len + nbr_len,
            prov: start + off_len + nbr_len + typ_len,
            conf: start + off_len + nbr_len + typ_len + prov_len + byte_pad,
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

    /// Out-edges of `local_id`:
    /// `(neighbor_local_id, edge_type_u8, confidence, provenance_u8)`.
    /// Empty if `local_id` is out of range.
    pub fn out_edges(&self, local_id: u32) -> Vec<(u32, u8, f32, u8)> {
        self.read_dir(&self.out, local_id)
    }

    /// In-edges of `local_id` — who points at it.
    pub fn in_edges(&self, local_id: u32) -> Vec<(u32, u8, f32, u8)> {
        self.read_dir(&self.inn, local_id)
    }

    /// Out-degree of `local_id` — the adjacency-list length, read from the
    /// offsets array without materialising the neighbor tuples. The cheap
    /// half of the prominence `edge_degree` signal.
    pub fn out_degree(&self, local_id: u32) -> usize {
        self.degree(&self.out, local_id)
    }

    /// In-degree of `local_id`.
    pub fn in_degree(&self, local_id: u32) -> usize {
        self.degree(&self.inn, local_id)
    }

    fn degree(&self, d: &CsrDir, local_id: u32) -> usize {
        if local_id >= self.n_atoms {
            return 0;
        }
        let b = &self.mmap[..];
        let rdu = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let lo = rdu(d.off + local_id as usize * 4) as usize;
        let hi = rdu(d.off + (local_id as usize + 1) * 4) as usize;
        hi - lo
    }

    fn read_dir(&self, d: &CsrDir, local_id: u32) -> Vec<(u32, u8, f32, u8)> {
        if local_id >= self.n_atoms {
            return Vec::new();
        }
        let b = &self.mmap[..];
        let rdu = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let rdf = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let lo = rdu(d.off + local_id as usize * 4) as usize;
        let hi = rdu(d.off + (local_id as usize + 1) * 4) as usize;
        (lo..hi)
            .map(|j| {
                (
                    rdu(d.nbr + j * 4),
                    b[d.typ + j],
                    rdf(d.conf + j * 4),
                    b[d.prov + j],
                )
            })
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
    let mut rows: Vec<AtomRow> = Vec::with_capacity(atoms.len());
    for (i, atom) in atoms.iter().enumerate() {
        let rec = project(atom);
        let local_id = i as u32;
        by_id.insert(rec.id.clone(), local_id);
        rows.push(AtomRow::from_record(local_id, &rec));
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
pub fn build_and_write_store_blocking(
    atlas_dir: &Path,
    corpus_id: &str,
) -> Result<PathBuf, String> {
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
/// flavor constraints. Used by the v2-store write bridges and
/// the [`LancePreload::open_blocking`] reader bridge (the daemon's sync
/// `AtlasGraph::load_from_disk` opening the v2 store off the hot query path).
fn run_blocking<T, F>(fut: F) -> Result<T, String>
where
    T: Send,
    F: std::future::Future<Output = Result<T, String>> + Send,
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
            .unwrap_or_else(|_| Err("v2 store thread panicked".to_string()))
    })
}

/// Read the `CSR_VERSION` from an `edges.csr` header without mmapping the whole
/// file. `None` if the file is missing, truncated, or has a bad magic — any of
/// which means the store is unreadable and must be (re)built.
fn edges_csr_version(path: &Path) -> Option<u32> {
    use std::io::Read;
    let mut hdr = [0u8; 8];
    std::fs::File::open(path).ok()?.read_exact(&mut hdr).ok()?;
    if u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) != CSR_MAGIC {
        return None;
    }
    Some(u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]))
}

/// True if the v2 store is missing, stale (older than `atoms.json`), or written
/// at a superseded `edges.csr` format version — i.e. it should be (re)built.
pub fn store_needs_build(atlas_dir: &Path) -> bool {
    let lance = atlas_dir.join(ATOMS_LANCE_DIRNAME);
    if !lance.exists() {
        return true;
    }
    // A stale-version `edges.csr` (e.g. CSR v1, before the per-edge provenance
    // byte) is rejected by `CsrEdges::open` with no fallback, so the store is
    // unloadable even when `atoms.lance` exists and is newer than `atoms.json` —
    // which the mtime check below would read as "current". Gate on the header
    // version explicitly so `migrate-all` rebuilds it instead of skipping it.
    if edges_csr_version(&atlas_dir.join(EDGES_CSR_FILENAME)) != Some(CSR_VERSION) {
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

// ── LancePreload: the production direct-read reader ───────────────────────────

/// Read every atom out of `atoms.lance` into a resident [`AtomRecord`], the
/// **canonical** [`project`]ion of its lossless `payload`. Re-projecting the
/// payload (rather than reading the scalar columns) is deliberate: it keeps the
/// payload the single source of truth, so the scalar columns stay a pure read
/// optimization and the resident record is always the canonical projection. It
/// also recovers the relational fields (`aliases`/`participants`/`evidence`)
/// that the scalar columns drop and only the payload carries. Returned sorted
/// by interned local id (== position), so CSR neighbor ids index straight into
/// the `Vec`.
///
/// Parsing every payload is the SEP/other-scale "preload" cost (cheap — hundreds
/// to low-thousands of atoms). Wikipedia carries no atom store of its own — it
/// serves structural neighbors from the columnar `WikipediaGraph`
/// (`articles.lance` + `edges.lance`), not this reader.
async fn read_atom_records(atlas_dir: &Path) -> Result<Vec<AtomRecord>, String> {
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
    let mut by_local: Vec<(u32, AtomRecord)> = Vec::new();
    for b in &batches {
        let ids = b
            .column_by_name("id")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>())
            .ok_or("atoms.lance missing id column")?;
        let payloads = b
            .column_by_name("payload")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .ok_or("atoms.lance missing payload column")?;
        for i in 0..b.num_rows() {
            let env: AtomEnvelope = serde_json::from_str(payloads.value(i))
                .map_err(|e| format!("parse atom payload: {e}"))?;
            by_local.push((ids.value(i), project(&env)));
        }
    }
    by_local.sort_by_key(|x| x.0);
    Ok(by_local.into_iter().map(|x| x.1).collect())
}

/// Which CSR direction an edge query walks.
#[derive(Clone, Copy)]
enum Dir {
    Out,
    In,
}

/// A resident, **sync-queryable** view over one corpus's v2 store — the
/// production direct-read reader. Atoms live
/// resident as projected [`AtomRecord`]s; edges stay the mmap'd `edges.csr`, so
/// the `atlas_navigate` BFS inner loop is sync + paged (the v2 "hot BFS stays
/// sync" invariant).
///
/// **Open is async, query is sync** — the ATLAS_STORAGE_V2 "preload-sync"
/// decision. [`open`](Self::open) reads the whole atoms table once (Lance/tokio,
/// off the hot path), then every accessor (`atom` / `atoms` / `out_edges` /
/// `in_edges` / `edge_degree`) is a plain slice / mmap read — no async ripple
/// into the query API. [`open_blocking`](Self::open_blocking) bridges the async
/// open to the daemon's sync `AtlasGraph::load_from_disk` via the dedicated-
/// thread runtime, like the write bridges.
pub struct LancePreload {
    /// Projected atom records; index == interned local id (sorted at open).
    atoms: Vec<AtomRecord>,
    /// atom-id string → local id (point lookup + edge-endpoint resolution).
    by_str_id: HashMap<String, u32>,
    /// local id → atom-id string, so CSR edge endpoints surface as `&str`
    /// borrowed from resident data.
    local_to_str: Vec<String>,
    /// mmap'd CSR adjacency — sync + paged.
    csr: CsrEdges,
}

impl LancePreload {
    /// Open the v2 store under `atlas_dir` (`atoms.lance` + `edges.csr`).
    /// Async (reads the atoms table); see [`open_blocking`](Self::open_blocking)
    /// for the sync bridge.
    pub async fn open(atlas_dir: &Path) -> Result<Self, String> {
        let atoms = read_atom_records(atlas_dir).await?;
        let mut by_str_id = HashMap::with_capacity(atoms.len());
        let mut local_to_str = Vec::with_capacity(atoms.len());
        for (i, rec) in atoms.iter().enumerate() {
            by_str_id.insert(rec.id.clone(), i as u32);
            local_to_str.push(rec.id.clone());
        }
        let csr = CsrEdges::open(&atlas_dir.join(EDGES_CSR_FILENAME))?;
        // The CSR's interned id space is exactly the atoms table's rows; a
        // mismatch means the two artifacts were written from different atom
        // sets (a torn / partial build) and the neighbor ids would misindex.
        if csr.n_atoms() as usize != atoms.len() {
            return Err(format!(
                "v2 store mismatch: edges.csr n_atoms={} != atoms.lance rows={}",
                csr.n_atoms(),
                atoms.len()
            ));
        }
        Ok(Self {
            atoms,
            by_str_id,
            local_to_str,
            csr,
        })
    }

    /// Sync bridge for [`open`](Self::open) — drives the async read on the
    /// dedicated-thread runtime so the daemon's sync `AtlasGraph::load_from_disk`
    /// can open the v2 store without an ambient-runtime panic. Lifecycle-time
    /// (boot / corpus load), never the hot query path.
    pub fn open_blocking(atlas_dir: &Path) -> Result<Self, String> {
        let dir = atlas_dir.to_path_buf();
        run_blocking(async move { LancePreload::open(&dir).await })
    }

    /// Number of atoms.
    pub fn atom_count(&self) -> usize {
        self.atoms.len()
    }

    /// Number of edges (from the CSR header — dangling edges already dropped).
    pub fn edge_count(&self) -> usize {
        self.csr.n_edges() as usize
    }

    /// Point lookup by atom-id string. `None` if absent.
    pub fn atom(&self, atom_id: &str) -> Option<&AtomRecord> {
        let local = *self.by_str_id.get(atom_id)?;
        self.atoms.get(local as usize)
    }

    /// All atoms in interned local-id order.
    pub fn atoms(&self) -> impl Iterator<Item = &AtomRecord> + '_ {
        self.atoms.iter()
    }

    /// In + out edge degree — the prominence signal, counted from the CSR
    /// offsets without materialising neighbor tuples. `0` for an absent atom.
    pub fn edge_degree(&self, atom_id: &str) -> usize {
        match self.by_str_id.get(atom_id) {
            Some(&local) => self.csr.out_degree(local) + self.csr.in_degree(local),
            None => 0,
        }
    }

    /// Edges originating at `atom_id`:
    /// `(source_str, target_str, type, conf, provenance)`, the endpoint strings
    /// borrowed from the resident id table. Empty if the atom is absent.
    pub fn out_edges(&self, atom_id: &str) -> Vec<(&str, &str, EdgeType, f32, EdgeProvenance)> {
        self.adjacent(atom_id, Dir::Out)
    }

    /// Edges arriving at `atom_id`.
    pub fn in_edges(&self, atom_id: &str) -> Vec<(&str, &str, EdgeType, f32, EdgeProvenance)> {
        self.adjacent(atom_id, Dir::In)
    }

    fn adjacent(
        &self,
        atom_id: &str,
        dir: Dir,
    ) -> Vec<(&str, &str, EdgeType, f32, EdgeProvenance)> {
        let Some(&local) = self.by_str_id.get(atom_id) else {
            return Vec::new();
        };
        let raw = match dir {
            Dir::Out => self.csr.out_edges(local),
            Dir::In => self.csr.in_edges(local),
        };
        let self_str = self.local_to_str[local as usize].as_str();
        raw.into_iter()
            .filter_map(|(nbr, ty, conf, prov)| {
                let nbr_str = self.local_to_str.get(nbr as usize)?.as_str();
                // out-CSR neighbor is the target; in-CSR neighbor is the source.
                let (src, tgt) = match dir {
                    Dir::Out => (self_str, nbr_str),
                    Dir::In => (nbr_str, self_str),
                };
                Some((src, tgt, u8_to_edge_type(ty), conf, u8_to_prov(prov)))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::Entity;
    use crate::enrichment::atlas::edges::{EdgeProvenance, EdgeType};
    use crate::enrichment::atlas::{AtomId, ChunkRef, Edge, EdgeId};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

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

    /// Read atoms.lance back into [`AtomRow`]s, sorted by local id (Lance does
    /// not guarantee scan order).
    async fn read_atoms_lance(atlas_dir: &Path) -> Vec<AtomRow> {
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
            let (id, kind, sal, conf) = (
                u32c(b, "id"),
                u8c(b, "kind"),
                f32c(b, "salience"),
                f32c(b, "confidence"),
            );
            let (sid, name, label, content) = (
                strc(b, "str_id"),
                strc(b, "name"),
                strc(b, "label"),
                strc(b, "content"),
            );
            let (subtype, desc, exc, payload) = (
                strc(b, "subtype"),
                strc(b, "description"),
                strc(b, "excerpt"),
                strc(b, "payload"),
            );
            for i in 0..b.num_rows() {
                rows.push(AtomRow {
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
        let prov: Vec<u8> = bytes[p..p + n_edges as usize].to_vec();
        p += n_edges as usize;
        while !p.is_multiple_of(4) {
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
                edges.push((i as u32, nbr[j], typ[j], conf[j], prov[j]));
            }
        }
        edges
    }

    #[tokio::test]
    async fn lance_row_equals_projection_on_a_frozen_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let atoms = vec![entity(1, "Alice"), entity(2, "Bob"), entity(3, "Carol")];
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

        // PARITY: each lance row equals the canonical projection of the same atom.
        let rows = read_atoms_lance(dir).await;
        assert_eq!(rows.len(), atoms.len());
        for (i, atom) in atoms.iter().enumerate() {
            let expect = AtomRow::from_record(i as u32, &project(atom));
            assert_eq!(rows[i], expect, "lance row {i} != canonical projection");
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
        // v2: the provenance byte round-trips (Derived here).
        assert_eq!(out[0].4, prov_u8(EdgeProvenance::Derived));
    }

    #[test]
    fn csr_edges_reader_roundtrips_the_writer() {
        // The mmap CsrEdges reader reconstructs exactly
        // what write_edges_csr wrote, over interned local ids.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // 3 atoms → local 0,1,2; edges entity1→entity3 (Involves) and
        // entity2→entity3 (Causes), so local 2 has two in-edges, no out-edges.
        let mut by_id = HashMap::new();
        for (i, n) in [1usize, 2, 3].iter().enumerate() {
            by_id.insert(AtomId::entity(*n).as_str().to_string(), i as u32);
        }
        let mk =
            |src: usize, tgt: usize, ty: EdgeType, conf: f32, prov: EdgeProvenance, id: &str| {
                Edge {
                    id: EdgeId::from_raw(id),
                    edge_type: ty,
                    source: AtomId::entity(src),
                    target: AtomId::entity(tgt),
                    evidence: vec![],
                    trigger_event: None,
                    sub_question: None,
                    confidence: conf,
                    provenance: prov,
                }
            };
        // Distinct provenances prove the v2 CSR carries the discriminant the
        // CallChain filters on — both edges are `Involves`-typed, so only the
        // provenance byte tells `scip_structural` from `containment_structural`.
        let edges = vec![
            mk(
                1,
                3,
                EdgeType::Involves,
                0.9,
                EdgeProvenance::ScipStructural,
                "e1",
            ),
            mk(
                2,
                3,
                EdgeType::Causes,
                0.5,
                EdgeProvenance::ContainmentStructural,
                "e2",
            ),
        ];
        write_edges_csr(dir, 3, &by_id, &edges).unwrap();

        let csr = CsrEdges::open(&dir.join(EDGES_CSR_FILENAME)).unwrap();
        assert_eq!(csr.n_atoms(), 3);
        assert_eq!(csr.n_edges(), 2);

        // out-edges: local 0 → local 2 (Involves, 0.9, scip); local 2 has none.
        let o0 = csr.out_edges(0);
        assert_eq!(o0.len(), 1);
        assert_eq!(o0[0].0, 2);
        assert_eq!(o0[0].1, edge_type_u8(ArchEdgeType::Involves));
        assert!((o0[0].2 - 0.9).abs() < 1e-6);
        assert_eq!(o0[0].3, prov_u8(EdgeProvenance::ScipStructural));
        assert_eq!(csr.out_edges(2).len(), 0);

        // in-edges of local 2: from local 0 (scip) and local 1 (containment).
        let i2 = csr.in_edges(2);
        assert_eq!(i2.len(), 2);
        let nbrs: std::collections::HashSet<u32> = i2.iter().map(|e| e.0).collect();
        assert!(nbrs.contains(&0) && nbrs.contains(&1));
        // Provenance survives the in-CSR direction too, keyed by source neighbor.
        let prov_of = |from: u32| i2.iter().find(|e| e.0 == from).map(|e| e.3).unwrap();
        assert_eq!(prov_of(0), prov_u8(EdgeProvenance::ScipStructural));
        assert_eq!(prov_of(1), prov_u8(EdgeProvenance::ContainmentStructural));
        assert_eq!(csr.in_edges(0).len(), 0);

        // out-of-range local id → empty, not a panic.
        assert_eq!(csr.out_edges(99).len(), 0);
    }

    #[test]
    fn store_needs_build_flags_a_stale_csr_version() {
        // A freshly written v2 store (atoms.lance + a CSR v2 edges.csr) is current.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let atoms = vec![entity(1, "Alice"), entity(2, "Bob")];
        let edges = vec![Edge {
            id: EdgeId::from_raw("e1"),
            edge_type: EdgeType::Involves,
            source: AtomId::entity(1),
            target: AtomId::entity(2),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 0.9,
            provenance: EdgeProvenance::ScipStructural,
        }];
        write_store_blocking(dir, "c1", &atoms, &edges).unwrap();
        assert!(!store_needs_build(dir), "a fresh CSR v2 store is current");

        // Flipping the edges.csr header version byte to a superseded value makes
        // the store unreadable (CsrEdges::open rejects it), so a rebuild is
        // required even though atoms.lance is present. The mtime check alone
        // can't see this; the version guard must.
        let csr = dir.join(EDGES_CSR_FILENAME);
        let mut bytes = std::fs::read(&csr).unwrap();
        bytes[4] = 1; // CSR_VERSION 2 → the pre-provenance v1 format
        std::fs::write(&csr, &bytes).unwrap();
        assert_eq!(edges_csr_version(&csr), Some(1));
        assert!(
            store_needs_build(dir),
            "a v1 edges.csr must force a rebuild"
        );
    }

    #[tokio::test]
    async fn lance_preload_reads_back_parity_atoms_and_edges() {
        // Reader-parity: LancePreload reads the v2 store back into resident
        // records identical to the canonical projection, and the CSR edge
        // endpoints resolve to the right atom-id strings — so the daemon's
        // `AtlasGraph` hands out views straight from resident data.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let atoms = vec![entity(1, "Alice"), entity(2, "Bob"), entity(3, "Carol")];
        let mk =
            |src: usize, tgt: usize, ty: EdgeType, conf: f32, prov: EdgeProvenance, id: &str| {
                Edge {
                    id: EdgeId::from_raw(id),
                    edge_type: ty,
                    source: AtomId::entity(src),
                    target: AtomId::entity(tgt),
                    evidence: vec![],
                    trigger_event: None,
                    sub_question: None,
                    confidence: conf,
                    provenance: prov,
                }
            };
        let edges = vec![
            mk(
                1,
                3,
                EdgeType::Involves,
                0.9,
                EdgeProvenance::ScipStructural,
                "e1",
            ),
            mk(
                2,
                3,
                EdgeType::Causes,
                0.5,
                EdgeProvenance::ContainmentStructural,
                "e2",
            ),
        ];
        write_store(dir, "frozen", &atoms, &edges).await.unwrap();

        let pre = LancePreload::open(dir).await.unwrap();
        assert_eq!(pre.atom_count(), 3);
        assert_eq!(pre.edge_count(), 2);

        // PARITY: each resident record == the canonical projection of the
        // original — including aliases/evidence, which live only in the payload
        // (the scalar columns drop them), proving the payload round-trips them.
        for atom in &atoms {
            let got = pre.atom(atom.id().as_str()).expect("atom present");
            assert_eq!(
                *got,
                project(atom),
                "preload record != canonical projection"
            );
        }
        let alice = pre.atom(AtomId::entity(1).as_str()).unwrap();
        assert_eq!(alice.aliases, vec!["Alice-alias".to_string()]);
        assert_eq!(alice.evidence.len(), 1); // Entity first_appearance sec_0001
        assert_eq!(alice.evidence[0].chunk_id, "sec_0001");

        // Edges resolve to atom-id strings, directionally.
        let (e1, e2, e3) = (
            AtomId::entity(1).as_str().to_string(),
            AtomId::entity(2).as_str().to_string(),
            AtomId::entity(3).as_str().to_string(),
        );
        let out1 = pre.out_edges(&e1);
        assert_eq!(out1.len(), 1);
        assert_eq!(out1[0].0, e1); // source
        assert_eq!(out1[0].1, e3); // target
        assert_eq!(out1[0].2, EdgeType::Involves);
        assert!((out1[0].3 - 0.9).abs() < 1e-6);
        assert_eq!(out1[0].4, EdgeProvenance::ScipStructural); // provenance preserved

        // entity3: two in-edges (from 1 and 2), zero out.
        assert_eq!(pre.out_edges(&e3).len(), 0);
        let in3 = pre.in_edges(&e3);
        assert_eq!(in3.len(), 2);
        assert!(in3.iter().all(|e| e.1 == e3)); // all target entity3
        let srcs: std::collections::HashSet<&str> = in3.iter().map(|e| e.0).collect();
        assert!(srcs.contains(e1.as_str()) && srcs.contains(e2.as_str()));

        // Degree = in + out, no tuple alloc.
        assert_eq!(pre.edge_degree(&e3), 2);
        assert_eq!(pre.edge_degree(&e1), 1);

        // Absent atom → empty / zero / None, never a panic.
        assert!(pre.atom("nope").is_none());
        assert_eq!(pre.out_edges("nope").len(), 0);
        assert_eq!(pre.edge_degree("nope"), 0);
    }
}
