// SPDX-License-Identifier: AGPL-3.0-or-later
//! The v2 store FORMAT and its READ half — ATLAS_STORAGE_V2 §A/§B.
//!
//! Carved out of corpus-engine's `atlas::store` (FIVE_PROGRAMS §12 decision 1,
//! 2026-09-21): the on-disk format — the `atoms.lance` schema, the `edges.csr`
//! binary layout, their version constants and the byte mappings — is ONE
//! decider, and the reader (`CsrEdges`, `LancePreload`'s parts, the freshness
//! reads) belongs to the consumers who READ the store. The WRITE half (the
//! store builder, the edge accounting, the rebuild decider) stays in
//! corpus-engine and imports this module — a store the engine writes and
//! everything else reads is the shape decision 1 drew.
//!
//! Two artifacts, per `ATLAS_STORAGE_V2.md` §A/§B:
//! - `atoms.lance` — columnar atom store (schema in [`atoms_schema`]).
//! - `edges.csr` — a plain little-endian CSR triple (offsets / neighbors /
//!   types / conf), out-edges and a symmetric in-CSR, mmap-friendly
//!   ([`CsrEdges`]).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{Array, Float32Array, RecordBatch, StringArray, UInt32Array, UInt8Array};
use futures::TryStreamExt;
use lancedb::query::ExecutableQuery;
use memmap2::Mmap;

use understanding_vocab::atoms::{AtomEnvelope, AtomType};
use understanding_vocab::edges::{EdgeProvenance, EdgeType};

use crate::projection::{project, AtomRecord};

/// v2 store schema version. Bump on any on-disk layout change (atoms.lance
/// columns or the edges.csr binary format).
pub const STORE_FORMAT_VERSION: u32 = 1;

/// The columnar atom store directory name (a Lance table under `atlas/`).
pub const ATOMS_LANCE_DIRNAME: &str = "atoms.lance";
/// The CSR edge file name.
pub const EDGES_CSR_FILENAME: &str = "edges.csr";
/// Lance table name created under the atlas dir (yields `atoms.lance`).
pub const ATOMS_TABLE: &str = "atoms";

pub const CSR_MAGIC: u32 = 0x4353_5256; // "CSRV"
                                        // v2 adds a per-edge provenance byte alongside the type byte. The Stage-1 BFS
                                        // can't distinguish a `scip_structural` call edge from a `containment_structural`
                                        // parent edge by `edge_type` alone (both are `Involves`); provenance is the
                                        // ground-truth discriminant the code-atlas CallChain filters on. Bumping the
                                        // version means a v1 `edges.csr` is rejected by `CsrEdges::open`; rebuild the
                                        // store with `sovereign atlas migrate-all <id>` (the v2 store is the only read
                                        // path after the v2 cleanup — there is no rkyv fallback).
pub const CSR_VERSION: u32 = 2;

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

    pub fn from_record(local_id: u32, r: &AtomRecord) -> Self {
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
/// Stable u8 discriminant for the atom kind — the `atoms.lance` `kind` column.
/// The byte values are an ON-DISK FORMAT: they are pinned here, never derived
/// from declaration order, so adding an [`AtomType`] variant cannot silently
/// renumber an existing atlas. Exhaustive by construction — a new variant
/// fails to compile until it is given a byte.
pub fn kind_u8(k: AtomType) -> u8 {
    match k {
        AtomType::Entity => 0,
        AtomType::Event => 1,
        AtomType::State => 2,
        AtomType::Relation => 3,
        AtomType::Claim => 4,
        AtomType::Question => 5,
        AtomType::Configuration => 6,
        AtomType::ArgumentReconstruction => 7,
        AtomType::Position => 8,
        AtomType::Opposition => 9,
        AtomType::Asset => 10,
        // Appended, never inserted: these bytes are on disk in every
        // `atoms.csr` written so far, so a new kind takes the next
        // number and the existing ten keep theirs.
        AtomType::Summary => 11,
    }
}

/// Stable u8 discriminant for the edge type — the `edges.csr` type byte, and
/// the exact inverse of [`u8_to_edge_type`]. Same on-disk-format discipline as
/// [`kind_u8`].
pub fn edge_type_u8(t: EdgeType) -> u8 {
    match t {
        EdgeType::Transition => 0,
        EdgeType::Causes => 1,
        EdgeType::Grounds => 2,
        EdgeType::Tension => 3,
        EdgeType::Involves => 4,
        EdgeType::Composes => 5,
        EdgeType::Configures => 6,
        EdgeType::Grounding => 7,
        EdgeType::Framing => 8,
        EdgeType::Provenance => 9,
        EdgeType::EvidenceFor => 10,
        EdgeType::Concedes => 11,
        EdgeType::OpposesIn => 12,
        EdgeType::Attaches => 13,
    }
}

/// Inverse of [`edge_type_u8`] — the `edges.csr` type byte back to an
/// [`EdgeType`]. Unknown bytes fall back to `Involves` (a benign medium-weight
/// edge) rather than panicking on a corrupt file.
pub fn u8_to_edge_type(u: u8) -> EdgeType {
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
pub fn prov_u8(p: EdgeProvenance) -> u8 {
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
pub fn u8_to_prov(u: u8) -> EdgeProvenance {
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
pub fn atoms_schema() -> Arc<Schema> {
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

pub fn atoms_batch(rows: &[AtomRow], sch: &Arc<Schema>) -> Result<RecordBatch, String> {
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
/// mmap'd reader over `edges.csr` — the v2 CSR edge file written by
/// corpus-engine's write half. Sync and paged: the `atlas_navigate` BFS inner
/// loop reads adjacency without faulting the whole file resident, the v2
/// "edges stay sync mmap" invariant. Neighbors are interned local u32 ids; the
/// caller maps them back to atom-id strings via the `atoms.lance`
/// `id`/`str_id` columns.
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
    /// mirrors the write half's layout exactly (corpus-engine's `push_csr`).
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

    /// Edges per kind, read off the out-direction type bytes. Every edge sits
    /// exactly once in the out-CSR, so this is the whole walked graph — the
    /// census [`AtlasInventory`] carries and `_summary.json` persists.
    pub fn edge_type_counts(&self) -> BTreeMap<EdgeType, u64> {
        let types = &self.mmap[self.out.typ..self.out.typ + self.n_edges as usize];
        let mut out = BTreeMap::new();
        for &t in types {
            *out.entry(u8_to_edge_type(t)).or_insert(0) += 1;
        }
        out
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
/// `edges.csr` mtime in ms since the epoch, `0` when there is no CSR. The
/// summary's third cache key: the store is written AFTER `atoms.json`, so a
/// summary keyed on the atoms file alone would stay current across the
/// store landing and carry an empty edge census for the life of the atlas.
pub fn csr_mtime_ms(atlas_dir: &Path) -> u64 {
    std::fs::metadata(atlas_dir.join(EDGES_CSR_FILENAME))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Edges per kind in `edges.csr`, or `None` when there is no readable CSR —
/// absence, reported as absence, so a store that has not been built reads
/// differently from one with no edges.
pub fn csr_edge_counts(atlas_dir: &Path) -> Option<BTreeMap<EdgeType, u64>> {
    let path = atlas_dir.join(EDGES_CSR_FILENAME);
    if !path.exists() {
        return None;
    }
    match CsrEdges::open(&path) {
        Ok(csr) => Some(csr.edge_type_counts()),
        Err(e) => {
            tracing::warn!(
                atlas = %atlas_dir.display(),
                error = %e,
                "atlas store: edges.csr present but unreadable; no edge census"
            );
            None
        }
    }
}

/// Read the `CSR_VERSION` from an `edges.csr` header without mmapping the whole
/// file. `None` if the file is missing, truncated, or has a bad magic — any of
/// which means the store is unreadable and must be (re)built.
pub fn edges_csr_version(path: &Path) -> Option<u32> {
    use std::io::Read;
    let mut hdr = [0u8; 8];
    std::fs::File::open(path).ok()?.read_exact(&mut hdr).ok()?;
    if u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) != CSR_MAGIC {
        return None;
    }
    Some(u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]))
}

/// Drive a Lance (tokio) future to completion from sync code. Lance requires a
/// tokio reactor; running on a fresh, dedicated-thread multi-thread runtime
/// avoids both the "runtime within a runtime" panic and `block_in_place`'s
/// flavor constraints. Used by the v2-store write bridges and
/// the [`LancePreload::open_blocking`] reader bridge (the daemon's sync
/// `AtlasGraph::load_from_disk` opening the v2 store off the hot query path),
/// and — since the seed table joined the mandatory artifacts — the atlas
/// writer's seed and the summary's row count. ONE such bridge for the whole
/// crate (ARCH §10.6); do not spawn a second runtime elsewhere.
///
/// `pub(crate)` rather than `pub(super)` since 2026-09-04: the wiki-class
/// `AtlasProvider` (`crate::wikipedia_columnar`) sits outside this module and
/// needs the same sync open, and a second bridge for it would be the second
/// implementation of one thing.
pub fn run_blocking<T, F>(fut: F) -> Result<T, String>
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
pub async fn read_atom_records(atlas_dir: &Path) -> Result<Vec<AtomRecord>, String> {
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
