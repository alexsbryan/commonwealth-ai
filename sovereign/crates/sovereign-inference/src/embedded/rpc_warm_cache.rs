// SPDX-License-Identifier: AGPL-3.0-or-later
//! Offline pre-warming of the RPC worker's tensor cache from a local GGUF.
//!
//! The mesh RPC worker caches received weight tensors (>10MB) to disk by
//! content hash, so a warm reload skips the network transfer (see
//! `model_slot::serve_rpc_worker_if_configured`). This module produces that
//! cache **directly from a GGUF on disk** — no network, no GPU, no model load —
//! so a node can be handed the GGUF on a thumbdrive and pre-seed its cache
//! entirely offline. When the cluster later runs over a metered/throttled link,
//! the host's tensor-hash requests are all cache hits and **zero weight bytes
//! cross the wire**.
//!
//! Correctness rests on one fact, verified against a real cache: the worker
//! names each cache file `"%016x"` of the **FNV-1a (64-bit)** hash of the exact
//! tensor bytes the host sends, which for a model load are the raw GGUF tensor
//! bytes (`ggml_nbytes` of the tensor). We replicate that hash over the GGUF's
//! own bytes, so the filenames match byte-for-byte.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Tensors at or below this size are sent inline (never hashed/cached) by
/// llama.cpp RPC — mirror `HASH_THRESHOLD` in `ggml-rpc.cpp`.
const HASH_THRESHOLD: u64 = 10 * 1024 * 1024;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// GGUF default tensor-data alignment when `general.alignment` is absent.
const DEFAULT_ALIGNMENT: u64 = 32;
/// Streaming read chunk for hashing/writing a tensor without buffering it whole.
const CHUNK: usize = 8 * 1024 * 1024;

/// Outcome of a warm-cache run.
#[derive(Debug, Default, Clone)]
pub struct WarmCacheStats {
    pub tensors_total: usize,
    /// Tensors larger than the hash threshold (the cacheable ones).
    pub tensors_cacheable: usize,
    /// Newly written this run.
    pub written: usize,
    /// Already present (idempotent re-run / sneakernet'd).
    pub already_present: usize,
    pub bytes_written: u64,
    pub cache_dir: PathBuf,
}

/// Default cache directory the in-process worker reads
/// (`serve_rpc_worker_if_configured` → `rpc_cache_dir`).
pub fn default_cache_dir() -> Option<PathBuf> {
    std::env::var("SOVEREIGN_RPC_CACHE_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| Some(sovereign_core::rebrand::svrnmesh_root().join("rpc-cache")))
}

#[derive(Debug)]
struct TensorInfo {
    name: String,
    dims: Vec<u64>,
    ggml_type: u32,
    /// Offset within the tensor-data section.
    offset: u64,
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_gguf_string(r: &mut impl Read) -> std::io::Result<String> {
    let len = read_u64(r)? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Skip one GGUF metadata value of the given wire type (used to walk past the
/// metadata block to reach the tensor infos). Returns the alignment if the
/// caller already matched `general.alignment` (handled by the caller instead).
fn skip_metadata_value<R: Read + Seek>(r: &mut R, value_type: u32) -> std::io::Result<()> {
    match value_type {
        0 | 1 | 7 => {
            r.seek(SeekFrom::Current(1))?;
        } // u8 / i8 / bool
        2 | 3 => {
            r.seek(SeekFrom::Current(2))?;
        } // u16 / i16
        4..=6 => {
            r.seek(SeekFrom::Current(4))?;
        } // u32 / i32 / f32
        10..=12 => {
            r.seek(SeekFrom::Current(8))?;
        } // u64 / i64 / f64
        8 => {
            // string
            let len = read_u64(r)? as i64;
            r.seek(SeekFrom::Current(len))?;
        }
        9 => {
            // array: elem_type (u32) + count (u64) + elements
            let elem_type = read_u32(r)?;
            let count = read_u64(r)?;
            for _ in 0..count {
                skip_metadata_value(r, elem_type)?;
            }
        }
        other => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown GGUF metadata value type {other}"),
            ));
        }
    }
    Ok(())
}

/// Parse a GGUF header far enough to enumerate tensor infos and locate the
/// tensor-data section. Reads metadata structurally (no model load).
fn parse_gguf(path: &Path) -> std::io::Result<(Vec<TensorInfo>, u64)> {
    let file = fs::File::open(path)?;
    let mut r = std::io::BufReader::new(file);

    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a GGUF file (bad magic)",
        ));
    }
    let version = read_u32(&mut r)?;
    if version < 2 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsupported GGUF version {version} (need >= 2)"),
        ));
    }
    let tensor_count = read_u64(&mut r)?;
    let kv_count = read_u64(&mut r)?;

    // Walk the metadata, capturing general.alignment if present.
    let mut alignment = DEFAULT_ALIGNMENT;
    for _ in 0..kv_count {
        let key = read_gguf_string(&mut r)?;
        let vtype = read_u32(&mut r)?;
        if key == "general.alignment" && vtype == 4 {
            alignment = read_u32(&mut r)? as u64;
        } else {
            skip_metadata_value(&mut r, vtype)?;
        }
    }
    if alignment == 0 {
        alignment = DEFAULT_ALIGNMENT;
    }

    // Tensor infos.
    let mut tensors = Vec::with_capacity(tensor_count as usize);
    for _ in 0..tensor_count {
        let name = read_gguf_string(&mut r)?;
        let n_dims = read_u32(&mut r)?;
        let mut dims = Vec::with_capacity(n_dims as usize);
        for _ in 0..n_dims {
            dims.push(read_u64(&mut r)?);
        }
        let ggml_type = read_u32(&mut r)?;
        let offset = read_u64(&mut r)?;
        tensors.push(TensorInfo {
            name,
            dims,
            ggml_type,
            offset,
        });
    }

    // The tensor-data section begins at the next `alignment` boundary after the
    // tensor-info block.
    let pos = r.stream_position()?;
    let data_offset = pos.div_ceil(alignment) * alignment;
    Ok((tensors, data_offset))
}

/// Byte length of a contiguous tensor, matching `ggml_nbytes`:
/// `row_size(type, ne0) * ne1 * ne2 * …`. Uses the bound ggml size helpers so
/// every quant type is exact without a hand-coded table.
fn tensor_nbytes(t: &TensorInfo) -> u64 {
    if t.dims.is_empty() {
        return 0;
    }
    // SAFETY: ggml_row_size is a pure size calculation over the type enum.
    let row = unsafe {
        crate::llama::sys::ggml_row_size(
            t.ggml_type as crate::llama::sys::ggml_type,
            t.dims[0] as i64,
        )
    } as u64;
    let mut n = row;
    for d in &t.dims[1..] {
        n = n.saturating_mul(*d);
    }
    n
}

/// Streaming FNV-1a (64-bit) — byte-for-byte the hash the RPC worker names its
/// cache files with (`%016x`) and the host computes in `set_tensor_hash`. Public
/// so the byte-range warmer (`#5b`, sovereign-mesh) can hash a tensor as it
/// streams in from an HTTP range-GET and name its cache file identically — one
/// hash implementation shared by every warm path, so they can never diverge
/// (the correctness basis for the whole cache).
#[derive(Debug, Clone)]
pub struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(FNV_OFFSET)
    }
}

impl Fnv1a {
    pub fn new() -> Self {
        Self::default()
    }
    #[inline]
    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }
    pub fn finish(&self) -> u64 {
        self.0
    }
}

/// The RPC cache filename for a tensor's content hash — `"%016x"`, matching
/// `ggml-rpc.cpp`. The single place this format lives, shared by every warm path.
pub fn cache_file_name(hash: u64) -> String {
    format!("{hash:016x}")
}

/// Transformer-block index parsed from a GGUF tensor name (`blk.<N>.…`).
/// `None` for global tensors (token embeddings, final norm, output head) that
/// aren't part of any single layer — llama.cpp places those on the first/last
/// device, so the shard planner assigns them explicitly rather than by range.
pub fn tensor_layer(name: &str) -> Option<u32> {
    name.strip_prefix("blk.")?
        .split('.')
        .next()?
        .parse::<u32>()
        .ok()
}

/// One tensor's placement facts for sharded distribution: where it lives in the
/// GGUF, its content hash (= cache filename), its size, and which transformer
/// block it belongs to. The manifest is computed on the node that holds the GGUF
/// and lets every other node fetch + warm ONLY its assigned shard — never the
/// whole model. This is what makes a 500GB model across N nodes tenable: each
/// node materializes ~size/N, not the full model.
///
/// Serializable: the host serves the precomputed manifest (`#5b` byte-range
/// fetch) so a worker can select its tensors by `tensor_device` and range-GET
/// exactly its shard without re-hashing the whole file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TensorManifestEntry {
    pub name: String,
    /// `blk.<N>` index, or `None` for a global (non-layer) tensor.
    pub layer: Option<u32>,
    /// FNV-1a (64-bit) of the tensor bytes — the cache file is named `%016x` of it.
    pub hash: u64,
    /// `ggml_nbytes` of the tensor.
    pub nbytes: u64,
    /// Absolute byte offset of the tensor's data in the GGUF (for range fetches).
    pub gguf_offset: u64,
    /// Whether the tensor is RPC-cacheable (> the 10MB hash threshold). Only
    /// cacheable tensors get a cache file / can be a hash-hit; smaller ones are
    /// always sent inline and never approach the upload deadlock.
    pub cacheable: bool,
    /// File NAME (not path) of the shard holding this tensor's bytes;
    /// `gguf_offset` is relative to this file. For a single-file model this
    /// is the model's own file name for every entry.
    pub file: String,
}

/// Stream `nbytes` from `start` and return the FNV-1a hash — byte-for-byte the
/// hash the worker names its cache file with and the host computes in
/// `set_tensor_hash`. Shared by the manifest builder and the warmer so the two
/// can never diverge.
fn hash_tensor_at(
    file: &mut fs::File,
    start: u64,
    nbytes: u64,
    buf: &mut [u8],
) -> std::io::Result<u64> {
    file.seek(SeekFrom::Start(start))?;
    let mut hash = Fnv1a::new();
    let mut remaining = nbytes;
    while remaining > 0 {
        let take = remaining.min(buf.len() as u64) as usize;
        file.read_exact(&mut buf[..take])?;
        hash.update(&buf[..take]);
        remaining -= take as u64;
    }
    Ok(hash.finish())
}

/// Every sibling file name of a split GGUF (`<stem>-<idx>-of-<count>.gguf`),
/// including `file_name` itself, in shard order — `None` when the name is not
/// a split (or declares a single shard).
///
/// Pure name arithmetic, no I/O. This is the ONE definition of "what files does
/// this model name denote"; `shard_files`, `total_model_bytes`, and the mesh's
/// worker-side fetch all delegate here. It used to be copy-pasted in three
/// places, which is exactly the kind of drift that produced the 2026-07-19
/// empty-warm deadlock — the readers disagreeing about what "the whole model"
/// means is the bug class, so there is deliberately only one parser.
///
/// Detection is by naming convention, matching what `llama-gguf-split` emits.
/// We deliberately do NOT read `split.count` from the GGUF header: the callers
/// that need this (advertising files, summing bytes) must answer before opening
/// any file, and a header read would make the cheap path expensive.
pub fn split_shard_names(file_name: &str) -> Option<Vec<String>> {
    let of = file_name.rfind("-of-")?;
    let count: u32 = file_name
        .get(of + 4..)?
        .strip_suffix(".gguf")?
        .parse()
        .ok()?;
    let before = file_name.get(..of)?; // "<stem>-<idx>"
    let dash = before.rfind('-')?;
    let idx = before.get(dash + 1..)?;
    idx.parse::<u32>().ok()?; // validate numeric
    let width = idx.len();
    let stem = before.get(..dash)?;
    if count <= 1 {
        return None;
    }
    Some(
        (1..=count)
            .map(|i| format!("{stem}-{i:0width$}-of-{count:0width$}.gguf"))
            .collect(),
    )
}

/// All files holding this model's tensor data: `[path]` for a single-file
/// model, or every `-NNNNN-of-NNNNN.gguf` sibling (in shard order) for a
/// split. Split shards are each standalone GGUFs with their own header +
/// tensor-info + data section, so every downstream reader parses them
/// per-file. Returns `[path]` (never guesses) when any sibling is missing.
pub fn shard_files(model_path: &Path) -> Vec<std::path::PathBuf> {
    let single = vec![model_path.to_path_buf()];
    let Some(name) = model_path.file_name().and_then(|n| n.to_str()) else {
        return single;
    };
    let Some(dir) = model_path.parent() else {
        return single;
    };
    let Some(names) = split_shard_names(name) else {
        return single;
    };
    let mut files = Vec::with_capacity(names.len());
    for shard_name in names {
        let shard = dir.join(shard_name);
        if !shard.is_file() {
            return single; // a shard missing → don't guess
        }
        files.push(shard);
    }
    files
}

/// Build the full tensor manifest from a local GGUF — name, layer, content hash,
/// size, and per-file offset for every tensor. **Split-aware:** a
/// `-NNNNN-of-NNNNN` model is walked shard by shard, and each entry records the
/// shard file its bytes live in (`file`) with `gguf_offset` relative to THAT
/// file — a manifest built from only the header shard silently empty-warms
/// every worker and resurrects the upload deadlock (found live 2026-07-19).
/// Streams each cacheable tensor once to hash it (no model load, no GPU). This
/// is the shard planner's input: it maps a per-node layer assignment to the
/// exact set of cache blobs (by hash) / byte ranges each node must hold.
pub fn build_manifest(gguf_path: &Path) -> std::io::Result<Vec<TensorManifestEntry>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    for shard in shard_files(gguf_path) {
        let (tensors, data_offset) = parse_gguf(&shard)?;
        let mut file = fs::File::open(&shard)?;
        let shard_name = shard
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        out.reserve(tensors.len());
        for t in &tensors {
            let nbytes = tensor_nbytes(t);
            let cacheable = nbytes > HASH_THRESHOLD;
            let start = data_offset + t.offset;
            // Only cacheable tensors are ever hash-matched; a non-cacheable
            // tensor's hash is never looked up, so leave it 0 rather than pay
            // to stream it.
            let hash = if cacheable {
                hash_tensor_at(&mut file, start, nbytes, &mut buf)?
            } else {
                0
            };
            out.push(TensorManifestEntry {
                name: t.name.clone(),
                layer: tensor_layer(&t.name),
                hash,
                nbytes,
                gguf_offset: start,
                cacheable,
                file: shard_name.clone(),
            });
        }
    }
    Ok(out)
}

/// Light per-tensor size table for PLANNING — `(name, block index, nbytes)` for
/// every tensor across all shards, WITHOUT hashing or reading tensor data (a
/// header-table parse only, so it's instant even on a 400 GB split). This is the
/// byte-mass input `svrn mesh plan` overlays onto [`plan_shards`] to compute the
/// per-device *bytes* each node would hold — the mass-awareness the live planner
/// (which apportions by uniform block count) does not have. Split-aware.
pub fn tensor_sizes(gguf_path: &Path) -> std::io::Result<Vec<(String, Option<u32>, u64)>> {
    let mut out = Vec::new();
    for shard in shard_files(gguf_path) {
        let (tensors, _data_offset) = parse_gguf(&shard)?;
        out.reserve(tensors.len());
        for t in &tensors {
            out.push((t.name.clone(), tensor_layer(&t.name), tensor_nbytes(t)));
        }
    }
    Ok(out)
}

/// Warm `cache_dir` with only the cacheable tensors whose layer is accepted by
/// `want` — the per-node ("sharded") form of [`warm_cache_from_gguf`]. A worker
/// assigned layers `L..M` calls this with a filter for `L..M` (plus its assigned
/// globals), so it materializes only its `O(size/N)` shard, never the whole
/// model — the property that makes large-model distribution across many nodes
/// tenable. Idempotent (a present, correctly-sized file is left untouched).
pub fn warm_cache_slice(
    gguf_path: &Path,
    cache_dir: &Path,
    want: impl Fn(&str, Option<u32>) -> bool,
) -> std::io::Result<WarmCacheStats> {
    fs::create_dir_all(cache_dir)?;
    let mut stats = WarmCacheStats {
        tensors_total: 0,
        cache_dir: cache_dir.to_path_buf(),
        ..Default::default()
    };
    // Split-aware: walk every shard file; each is a standalone GGUF whose
    // offsets are file-relative. A single-file model is the one-shard case.
    for shard in shard_files(gguf_path) {
        warm_slice_one_file(&shard, cache_dir, &want, &mut stats)?;
    }
    Ok(stats)
}

fn warm_slice_one_file(
    gguf_path: &Path,
    cache_dir: &Path,
    want: &impl Fn(&str, Option<u32>) -> bool,
    stats: &mut WarmCacheStats,
) -> std::io::Result<()> {
    let (tensors, data_offset) = parse_gguf(gguf_path)?;
    stats.tensors_total += tensors.len();

    let mut file = fs::File::open(gguf_path)?;
    let mut buf = vec![0u8; CHUNK];

    for t in &tensors {
        let nbytes = tensor_nbytes(t);
        if nbytes <= HASH_THRESHOLD {
            continue;
        }
        // Only materialize this node's shard.
        if !want(&t.name, tensor_layer(&t.name)) {
            continue;
        }
        stats.tensors_cacheable += 1;

        let start = data_offset + t.offset;
        let hash = hash_tensor_at(&mut file, start, nbytes, &mut buf)?;
        let hash_str = cache_file_name(hash);
        let cache_file = cache_dir.join(&hash_str);

        // Idempotent: skip if already present with the right size.
        if let Ok(meta) = fs::metadata(&cache_file) {
            if meta.len() == nbytes {
                stats.already_present += 1;
                continue;
            }
        }

        // Stream the bytes into the cache file (atomic via temp + rename so a
        // partial write never looks like a valid cache entry).
        let tmp = cache_dir.join(format!(".{hash_str}.tmp"));
        {
            let mut out = std::io::BufWriter::new(fs::File::create(&tmp)?);
            file.seek(SeekFrom::Start(start))?;
            let mut remaining = nbytes;
            while remaining > 0 {
                let take = remaining.min(CHUNK as u64) as usize;
                file.read_exact(&mut buf[..take])?;
                out.write_all(&buf[..take])?;
                remaining -= take as u64;
            }
            out.flush()?;
        }
        fs::rename(&tmp, &cache_file)?;
        stats.written += 1;
        stats.bytes_written += nbytes;
        tracing::debug!(tensor = %t.name, hash = %hash_str, bytes = nbytes, "warmed RPC cache entry");
    }

    Ok(())
}

/// Pre-warm `cache_dir` from `gguf_path` for EVERY cacheable tensor — the
/// whole-model form (a node that holds the full GGUF, e.g. the entry/host node).
/// For per-node sharded distribution use [`warm_cache_slice`]. Idempotent.
pub fn warm_cache_from_gguf(gguf_path: &Path, cache_dir: &Path) -> std::io::Result<WarmCacheStats> {
    warm_cache_slice(gguf_path, cache_dir, |_name, _layer| true)
}

/// One device's shard under our placement plan: the contiguous block range it
/// holds, whether it owns the output head, and its normalized fraction.
/// `device_index` is the position in the RPC-first device list (matching
/// `with_devices` / `SOVEREIGN_RPC_TENSOR_SPLIT`). The input embedding always
/// stays on the host CPU (llama.cpp does this unconditionally) so it's never a shard.
///
/// Serializable: the host computes the plan ONCE and ships it whole to each
/// worker (the auto-warm orchestration), so warm-time placement and load-time
/// `-ot` overrides derive from the identical plan and cannot diverge — the
/// plan-agreement invariant that keeps every weight a cache hit (no bulk send).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeShard {
    pub device_index: usize,
    /// Inclusive `(first, last)` block range, or `None` if this device got no blocks.
    pub blocks: Option<(u32, u32)>,
    /// Owns the output head (`output.weight`).
    pub holds_output: bool,
    /// Normalized weight fraction.
    pub fraction: f32,
}

/// Round a device's free-VRAM weight to a coarse bucket (4 GiB) so transient
/// fluctuation can't change the shard apportionment when the device set is
/// unchanged. A belt alongside the per-device-set plan cache: small allocator
/// churn between reloads stays in-bucket → `plan_shards` returns the same split →
/// workers' warm caches stay valid. A nonzero-but-tiny device still gets one
/// bucket so it isn't apportioned to zero. Pure.
pub fn quantize_vram(bytes: u64) -> u64 {
    const BUCKET: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB
    if bytes == 0 {
        return 0;
    }
    (((bytes + BUCKET / 2) / BUCKET).max(1)) * BUCKET
}

/// Our OWN contiguous placement policy: apportion `n_layer` transformer blocks
/// across devices proportional to `weights` (RPC-first order) BY COUNT, with
/// exact integer counts via largest-remainder so they sum to `n_layer`. The
/// output head goes on the last block-holding device. We do NOT predict
/// llama.cpp's split — we ENFORCE this assignment via `tensor_buft_overrides` at
/// load and warm the same assignment, so there is nothing to diverge from.
///
/// Count-proportional is only byte-balanced when every block has ~equal mass (a
/// dense model, or a uniform all-MoE stack). For NON-UNIFORM models — a hybrid
/// SSM+MoE (a light attention/SSM block next to a 60x heavier MoE block) or a
/// DeepSeek-style leading-dense stack — block COUNT is a poor proxy for bytes,
/// so a count split can hand a small node a heavy contiguous run and OOM it.
/// [`plan_shards_weighted`] apportions by real byte mass instead; this is its
/// `block_bytes == []` case. Pure + deterministic.
pub fn plan_shards(n_layer: u32, weights: &[f32]) -> Vec<NodeShard> {
    plan_shards_weighted(n_layer, weights, &[], 0)
}

/// Byte-mass-aware contiguous placement — the split the live load needs on
/// NON-UNIFORM (MoE / hybrid) models. `block_bytes[i]` is block `i`'s resident
/// weight in bytes (len must equal `n_layer`); `head_bytes` is the output-head
/// mass, which rides on the last block-holding device. Each device's resident
/// bytes come out proportional to its `weights` entry — so a 64 GB node and a
/// 32 GB node hold ~2:1 of the *mass*, not of the block *count*. The head is
/// folded in: the head-holder is handed a smaller block budget (its share minus
/// the head it already carries) so it doesn't run ~`head_bytes` heavy.
///
/// Ranges stay CONTIGUOUS, so hops stay `D-1` and `tensor_device` /
/// `override_patterns` are unchanged — only the cut points move. Falls back to
/// the count-based apportionment of [`plan_shards`] when `block_bytes` is empty
/// or all-zero (a uniform model, or a caller with no size table). Note that when
/// a single block's mass exceeds a small node's whole share, no CONTIGUOUS split
/// can keep that node within budget — the caller (`mesh plan`) surfaces the
/// residual per-device overflow; splitting a block's experts off is a separate,
/// heavier lever we deliberately don't take here. Pure + deterministic.
pub fn plan_shards_weighted(
    n_layer: u32,
    weights: &[f32],
    block_bytes: &[u64],
    head_bytes: u64,
) -> Vec<NodeShard> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    // Effective device weights (all-zero → equal share).
    let wsum_f32: f32 = weights.iter().sum();
    let w: Vec<f64> = if wsum_f32 > 0.0 {
        weights.iter().map(|&x| x as f64).collect()
    } else {
        vec![1.0; n]
    };
    let wsum: f64 = w.iter().sum::<f64>().max(f64::MIN_POSITIVE);

    let have_mass = block_bytes.len() == n_layer as usize && block_bytes.iter().any(|&b| b > 0);

    let counts: Vec<u32> = if !have_mass {
        // ── Count-based (largest-remainder) apportionment ──
        let ideal: Vec<f64> = w.iter().map(|&wd| wd / wsum * n_layer as f64).collect();
        let mut counts: Vec<u32> = ideal.iter().map(|x| x.floor() as u32).collect();
        let mut remainder = n_layer.saturating_sub(counts.iter().sum());
        let mut by_frac: Vec<usize> = (0..n).collect();
        by_frac.sort_by(|&a, &b| {
            let (fa, fb) = (ideal[a] - ideal[a].floor(), ideal[b] - ideal[b].floor());
            fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
        });
        for &d in &by_frac {
            if remainder == 0 {
                break;
            }
            counts[d] += 1;
            remainder -= 1;
        }
        counts
    } else {
        // ── Byte-mass-aware apportionment: contiguous cuts by cumulative bytes ──
        let total_block: u64 = block_bytes.iter().sum();
        let total_mass = total_block as f64 + head_bytes as f64;
        // The head lands on the last device with weight > 0 (device order is
        // RPC-workers-first, host last; the host holds the head).
        let head_dev = (0..n).rev().find(|&d| w[d] > 0.0);
        let mut tgt_block: Vec<f64> = w.iter().map(|&wd| wd / wsum * total_mass).collect();
        if let Some(hd) = head_dev {
            tgt_block[hd] = (tgt_block[hd] - head_bytes as f64).max(0.0);
        }
        // prefix[i] = Σ block_bytes[0..i]; prefix[n_layer] = total_block.
        let mut prefix = vec![0u64; n_layer as usize + 1];
        for i in 0..n_layer as usize {
            prefix[i + 1] = prefix[i] + block_bytes[i];
        }
        let mut counts = vec![0u32; n];
        let mut cum_tgt = 0.0f64;
        let mut prev_cut = 0u32;
        for d in 0..n {
            cum_tgt += tgt_block[d];
            // Last device takes the remainder so the cuts always tile [0, n_layer)
            // with no rounding gap.
            let cut = if d == n - 1 {
                n_layer
            } else {
                closest_boundary(&prefix, cum_tgt, prev_cut, n_layer).max(prev_cut)
            };
            counts[d] = cut - prev_cut;
            prev_cut = cut;
        }
        counts
    };

    build_shards_from_counts(&counts, &w)
}

/// Smallest boundary `b` in `[lo, hi]` whose `prefix[b]` is closest to `target`.
/// `prefix` is non-decreasing, so we advance while it improves and stop once we
/// pass the target without improving. Deterministic; ties resolve to the smaller
/// `b` (the earlier cut). Used only by the byte-aware apportionment.
fn closest_boundary(prefix: &[u64], target: f64, lo: u32, hi: u32) -> u32 {
    let mut best = lo;
    let mut best_d = (prefix[lo as usize] as f64 - target).abs();
    let mut b = lo + 1;
    while b <= hi {
        let cur = prefix[b as usize] as f64;
        let d = (cur - target).abs();
        if d < best_d {
            best_d = d;
            best = b;
        } else if cur >= target {
            break; // past the target and not improving → monotonic, stop
        }
        b += 1;
    }
    best
}

/// Turn per-device contiguous block `counts` into the [`NodeShard`] plan: assign
/// each device its next contiguous range, put the output head on the last device
/// that got any blocks, and record its normalized weight fraction. Shared by both
/// apportionment paths so they can't diverge on shard shape.
fn build_shards_from_counts(counts: &[u32], eff_weights: &[f64]) -> Vec<NodeShard> {
    let n = counts.len();
    let wsum: f64 = eff_weights.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    let last_with_blocks = (0..n).rev().find(|&d| counts[d] > 0);
    let mut cur = 0u32;
    (0..n)
        .map(|d| {
            let blocks = if counts[d] > 0 {
                let b = (cur, cur + counts[d] - 1);
                cur += counts[d];
                Some(b)
            } else {
                None
            };
            NodeShard {
                device_index: d,
                blocks,
                holds_output: Some(d) == last_with_blocks,
                fraction: (eff_weights[d] / wsum) as f32,
            }
        })
        .collect()
}

/// Parse an EXPLICIT per-device block count list (e.g. `"11,21"` for a 32-layer
/// model across two devices), in the same device order as `weights` — RPC
/// workers first, then local. `None` unless the list is well-formed AND tiles
/// the model exactly: right number of devices, counts summing to `n_layer`.
///
/// This exists because the byte-mass apportionment above derives its cut points
/// from advertised VRAM, so there is otherwise NO way to aim the device boundary
/// at a chosen layer. Aiming it is the only way to run the discriminating
/// experiment for the distributed-decode crash: on a hybrid model like
/// Qwen3.5-4B (32 layers, 3:1 — layers 3/7/11/…/31 are full attention, the rest
/// Gated DeltaNet), the fault appears at a boundary that lands immediately
/// before a Gated DeltaNet layer, and `resolve_fused_ops` disables the fused GDN
/// kernel at exactly that layer. Moving the cut onto an attention layer and
/// watching whether the fault follows separates "boundary-on-GDN" from "any
/// boundary at all" in one run each.
///
/// Rejecting a malformed list rather than repairing it is deliberate: a split
/// that silently differs from what the operator asked for would make the
/// experiment's negative result meaningless.
pub fn parse_block_split(raw: &str, n_layer: u32, n_devices: usize) -> Option<Vec<u32>> {
    let counts: Vec<u32> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    if counts.len() != n_devices || counts.iter().sum::<u32>() != n_layer {
        return None;
    }
    Some(counts)
}

/// Build a plan from EXPLICIT per-device block counts, bypassing the byte-mass
/// apportionment. Shares `build_shards_from_counts` with the computed paths, so
/// `holds_output` placement, `fraction`, and contiguity are identical — only the
/// cut points differ. `None` if `counts` doesn't tile `n_layer` across
/// `weights`. Pure + deterministic.
pub fn plan_shards_explicit(
    n_layer: u32,
    weights: &[f32],
    counts: &[u32],
) -> Option<Vec<NodeShard>> {
    if counts.len() != weights.len() || counts.iter().sum::<u32>() != n_layer {
        return None;
    }
    let wsum_f32: f32 = weights.iter().sum();
    let w: Vec<f64> = if wsum_f32 > 0.0 {
        weights.iter().map(|&x| x as f64).collect()
    } else {
        vec![1.0; weights.len()]
    };
    Some(build_shards_from_counts(counts, &w))
}

// ───────────────────────────────────────────────────────────────────────────
// Does the plan actually FIT? — the shared decider
//
// `plan_shards_weighted` decides where each block GOES. Nothing above decides
// whether the device it goes to can HOLD it, and until 2026-07-28 nothing did:
// the live load gated only on POOLED memory, so a plan whose aggregate fit
// comfortably could still hand one worker more than it had. `mesh plan` grew
// the per-device check first, in its own private fold, which meant the preview
// and the load could disagree — the exact drift the preview exists to close.
//
// So the check lives here, beside the planner, and both callers use it.
// ───────────────────────────────────────────────────────────────────────────

/// A model's resident byte mass, decomposed the way a placement decision needs
/// it.
///
/// Read from the GGUF's tensor table — a header parse, no weight load — so this
/// is cheap enough to compute on every plan. The decomposition matters because
/// the pieces land in different places: block mass follows the shard split, the
/// output head rides with the last block-holder, and `token_embd` stays in host
/// RAM rather than on any device.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelMass {
    /// Per-block resident bytes, indexed by block number. Empty when the tensor
    /// table could not be read — see [`ModelMass::is_known`].
    pub block_bytes: Vec<u64>,
    /// Output head (`output.weight`) bytes. Rides with the last block-holder.
    pub head_bytes: u64,
    /// Input embedding (`token_embd`) bytes. Stays in host system RAM and is
    /// therefore NOT charged to any device's share.
    pub embd_bytes: u64,
    /// Global tensors that are neither head nor embedding, plus any tensor
    /// tagged with an out-of-range layer. Host overhead.
    pub other_global_bytes: u64,
    /// Routed-expert (`_exps`) mass — the COLD part of a mixture-of-experts
    /// model. Resident, so it counts against fit; only the router's top-k are
    /// read per token, so it is a small fraction of the per-token work.
    pub routed_expert_bytes: u64,
    /// The model carries `ssm_*` weights — a hybrid (Gated DeltaNet) stack.
    pub recurrent: bool,
}

impl ModelMass {
    /// Every resident byte, including the pieces that live in host RAM.
    pub fn total_bytes(&self) -> u64 {
        self.block_bytes.iter().sum::<u64>()
            + self.head_bytes
            + self.embd_bytes
            + self.other_global_bytes
    }

    /// Whether this mass can support a fit judgment.
    ///
    /// An all-zero block table means the tensor table was unreadable. The
    /// distinction is load-bearing: a fit check run against zeros would pass
    /// every device trivially, turning a failed header read into a clean bill
    /// of health.
    pub fn is_known(&self) -> bool {
        self.block_bytes.iter().any(|&b| b > 0)
    }
}

/// Decompose a GGUF tensor table into a [`ModelMass`].
///
/// `sizes` is the `(tensor_name, layer, nbytes)` table `tensor_sizes` returns.
/// `n_layer` is the header's block count; a tensor tagged with a layer outside
/// `0..n_layer` is counted as a global rather than silently dropped, because a
/// dropped byte is a byte the fit check will not charge anyone for. Pure.
pub fn model_mass_from_sizes(sizes: &[(String, Option<u32>, u64)], n_layer: u32) -> ModelMass {
    let mut m = ModelMass {
        block_bytes: vec![0u64; n_layer as usize],
        ..Default::default()
    };
    for (name, layer, nbytes) in sizes {
        if name.contains(".ssm_") {
            m.recurrent = true;
        }
        if is_routed_expert_tensor(name) {
            m.routed_expert_bytes += *nbytes;
        }
        match layer {
            Some(l) if (*l as usize) < m.block_bytes.len() => m.block_bytes[*l as usize] += *nbytes,
            Some(_) => m.other_global_bytes += *nbytes,
            None if is_output_tensor(name) => m.head_bytes += *nbytes,
            None if name.contains("token_embd") => m.embd_bytes += *nbytes,
            None => m.other_global_bytes += *nbytes,
        }
    }
    m
}

/// One device's share of a plan, weighed against what that device has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardFit {
    /// Index into the plan's device order (RPC workers first, host last) — the
    /// same index [`NodeShard::device_index`] carries. A caller displaying this
    /// against its own device numbering must map back through the order it
    /// passed in; the two spaces are easy to confuse and nothing here can catch
    /// it for you.
    pub device_index: usize,
    /// Resident bytes this device would hold: its block range plus the output
    /// head if it carries it.
    pub held_bytes: u64,
    /// Projected non-weight bytes (KV share + compute scratch) this device
    /// needs on top of the weights — `0` when no [`PlanOverheads`] was
    /// supplied and the multiplicative headroom is the only margin.
    pub overhead_bytes: u64,
    /// `held_bytes × headroom + overhead_bytes` — what must actually fit.
    pub need_bytes: u64,
    /// What this device has.
    pub capacity_bytes: u64,
}

/// Non-weight memory a distributed load needs, projected by llama.cpp's own
/// three-term accountant (`llama_cpp_4::fit::get_device_memory_data` →
/// `common_device_memory_collect`) rather than guessed from the weights.
///
/// Exists because the fit gate used to charge devices for weights only: the
/// 2026-08-02 loads passed `shard_fits` with a 0.07 GiB margin, reached
/// `serving`, and died allocating KV + compute buffers on the first inference.
/// The opposite error was live on the host side, where a `weights/8` KV proxy
/// over-charged an MLA model ~5× and refused loads that fit. The projection
/// measured ~278 ms warm on a 155 GB sharded GGUF (`no_alloc` load, freed
/// before returning — tests/device_memory_probe.rs), so it can sit in the
/// plan path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanOverheads {
    /// KV / recurrent-cache bytes for the WHOLE model at the load's context
    /// size. Charged to each device in proportion to the blocks it holds.
    pub context_total_bytes: u64,
    /// Compute-scratch bytes per accelerator device (the largest projected
    /// per-device compute buffer). Charged to every device in the plan: each
    /// backend reserves its own graph workspace.
    pub compute_accel_bytes: u64,
    /// The host CPU buffer's compute scratch — the scheduler-side workspace
    /// that only exists in the process driving the graph. Charged to the plan's
    /// LAST device (plan order is RPC workers first, host last).
    pub compute_host_bytes: u64,
}

impl PlanOverheads {
    /// Non-weight bytes device `idx` of `n_devices` needs when holding
    /// `blocks` of `total_blocks`.
    pub fn device_bytes(
        &self,
        idx: usize,
        n_devices: usize,
        blocks: u64,
        total_blocks: u64,
    ) -> u64 {
        let ctx = if total_blocks == 0 {
            0
        } else {
            (self.context_total_bytes as u128 * blocks as u128 / total_blocks as u128) as u64
        };
        let host_extra = if idx + 1 == n_devices {
            self.compute_host_bytes
        } else {
            0
        };
        ctx + self.compute_accel_bytes + host_extra
    }
}

impl ShardFit {
    /// Whether this device can hold its share with headroom.
    pub fn fits(&self) -> bool {
        self.need_bytes <= self.capacity_bytes
    }

    /// Spare room in bytes; negative when this device overflows.
    pub fn slack_bytes(&self) -> i128 {
        self.capacity_bytes as i128 - self.need_bytes as i128
    }
}

/// Judge a shard plan device by device.
///
/// Returns **one row per shard**, in plan order, fitting and overflowing alike —
/// not a pass/fail. A `Result<(), Overflow>` shape would force every caller that
/// wants to *show* the fit (which `mesh plan` does, `ok +12.4 GB` per row) to
/// keep its own traversal, and a second traversal is exactly the drift this
/// function exists to remove.
///
/// `capacities` is in **plan order** — the same order `plan_shards_weighted`'s
/// `weights` were in, RPC workers first and the host last. Passing it in the
/// caller's own device numbering silently mis-attributes every row.
///
/// `None` means **"cannot judge"**, and it is not a pass. It is returned when
/// the inputs do not describe each other — a capacity list of the wrong length,
/// an empty plan, a nonsensical headroom — or when [`ModelMass::is_known`] is
/// false, because judging against an unread tensor table would clear every
/// device on the strength of zeros. A caller that gets `None` must say so; it
/// must not report a fit.
pub fn shard_fits(
    plan: &[NodeShard],
    capacities: &[u64],
    mass: &ModelMass,
    headroom: f64,
    overheads: Option<&PlanOverheads>,
) -> Option<Vec<ShardFit>> {
    if plan.is_empty() || capacities.len() != plan.len() {
        return None;
    }
    if !headroom.is_finite() || headroom < 1.0 {
        return None;
    }
    if !mass.is_known() {
        return None;
    }
    let total_blocks = mass.block_bytes.len() as u64;
    let mut out = Vec::with_capacity(plan.len());
    for (pos, shard) in plan.iter().enumerate() {
        // The plan's own `device_index` is the authority on which capacity a
        // shard is judged against; position is only a fallback for a
        // hand-built plan that did not set it.
        let idx = if shard.device_index < capacities.len() {
            shard.device_index
        } else {
            pos
        };
        let mut held = 0u64;
        let mut blocks = 0u64;
        if let Some((a, b)) = shard.blocks {
            blocks = (b - a + 1) as u64;
            for blk in a..=b {
                // A range past the end of the table means the plan and the mass
                // describe different models. Refuse rather than under-charge.
                held = held.checked_add(*mass.block_bytes.get(blk as usize)?)?;
            }
        }
        if shard.holds_output {
            held += mass.head_bytes;
        }
        // With a projection, need is what the load will actually allocate:
        // weights (× the operator's safety headroom) plus this device's KV
        // share and compute scratch. Without one, headroom alone stands in for
        // the missing terms — the pre-2026-08-02 behaviour, kept as the
        // fallback so a failed projection can never brick a load.
        let overhead = overheads
            .map(|o| o.device_bytes(idx, capacities.len(), blocks, total_blocks))
            .unwrap_or(0);
        out.push(ShardFit {
            device_index: idx,
            held_bytes: held,
            overhead_bytes: overhead,
            need_bytes: (held as f64 * headroom) as u64 + overhead,
            capacity_bytes: capacities[idx],
        });
    }
    Some(out)
}

/// The cacheable output head (LM projection) — placed with the last block-holding
/// device, distinct from `token_embd` (input) which stays on the host CPU.
pub fn is_output_tensor(name: &str) -> bool {
    name == "output.weight"
}

/// A routed-expert weight tensor — `blk.N.ffn_{gate,up,down}_exps.weight`, the
/// `_exps` fused-expert stack. This is the COLD mass of an MoE model: only the
/// router's top-k experts are read per token, so it can be ~90% of the bytes yet
/// a small fraction of the per-token work. Distinct from the SHARED expert
/// (`_shexp`, run every token → hot) and the router (`ffn_gate_inp` → hot). Used
/// by `mesh plan` to report hot/cold mass; the split itself stays per-block
/// (a whole block, experts included, on one device) so single-stream decode
/// keeps its `D-1` hops rather than scattering a layer's experts across nodes.
pub fn is_routed_expert_tensor(name: &str) -> bool {
    name.contains("_exps.weight")
}

/// Which device a tensor lands on under `plan`, or `None` if it stays on the host
/// CPU (input embedding / non-block, non-output tensors). A worker warms a tensor
/// iff `tensor_device(...) == Some(its index)` — the single source of truth shared
/// by warming and the load-time overrides, so they cannot disagree.
pub fn tensor_device(name: &str, layer: Option<u32>, plan: &[NodeShard]) -> Option<usize> {
    match layer {
        Some(l) => plan
            .iter()
            .find(|s| s.blocks.is_some_and(|(a, b)| l >= a && l <= b))
            .map(|s| s.device_index),
        None if is_output_tensor(name) => {
            plan.iter().find(|s| s.holds_output).map(|s| s.device_index)
        }
        None => None,
    }
}

/// Warm `cache_dir` with exactly the tensors device `device_index` is assigned
/// under `plan` — the per-node entry point. A worker calls this for its own
/// device; it materializes only its shard (≈ size/N), never the whole model.
pub fn warm_cache_for_device(
    gguf_path: &Path,
    cache_dir: &Path,
    plan: &[NodeShard],
    device_index: usize,
) -> std::io::Result<WarmCacheStats> {
    warm_cache_slice(gguf_path, cache_dir, |name, layer| {
        tensor_device(name, layer, plan) == Some(device_index)
    })
}

/// Build the `-ot` regex overrides that ENFORCE `plan` at load: one
/// `^blk\.(L|…|M)\.` per block-holding device, plus `^output\.weight` on the output
/// owner. Returns `(regex, device_index)`; the caller resolves each `device_index`
/// to that device's `ggml_backend_dev_buffer_type`. `token_embd` gets no override
/// (llama.cpp keeps the input embedding on the host CPU).
pub fn override_patterns(plan: &[NodeShard]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for s in plan {
        if let Some((l, m)) = s.blocks {
            let nums: Vec<String> = (l..=m).map(|i| i.to_string()).collect();
            out.push((format!("^blk\\.({})\\.", nums.join("|")), s.device_index));
        }
    }
    if let Some(s) = plan.iter().find(|s| s.holds_output) {
        out.push(("^output\\.weight".to_string(), s.device_index));
    }
    out
}

/// The `tensor_split` that makes llama.cpp's PER-LAYER device map agree with
/// `plan` — required alongside the `-ot` overrides, not optional.
///
/// llama.cpp keeps two placement systems that never talk to each other. Our
/// `-ot` overrides place the WEIGHTS; `model.dev_layer(il)` — which
/// `resolve_fused_ops` consults to decide whether fused kernels stay enabled —
/// is computed at load from `tensor_split` (advertised free memory when unset)
/// over **`n_layer + 1` units**, the output head counting as the extra unit
/// (llama-model.cpp: `act_gpu_layers = min(n_gpu_layers, n_layer + 1)`, then
/// `upper_bound(splits, il / act_gpu_layers)`). Overrides are never consulted.
/// Left unpinned, dev_layer's boundary can straddle our cut by one layer; on a
/// hybrid (Gated DeltaNet) model one straddled layer disables the fused GDN
/// kernel globally, the unfused path emits `GGML_OP_SET`, and the first
/// distributed decode kills the host at ggml-rpc.cpp:498
/// (docs/DISTRIBUTED_GDN_CRASH_STATUS.md).
///
/// So hand llama.cpp cut points that sit HALFWAY between our block boundaries
/// in `(n_layer + 1)`-unit space: a device whose last block is `b` gets its
/// upper cut at `b + 0.5` — strictly between layer `b` and layer `b + 1`, so no
/// float tie can flip a layer (a cut at exactly `b + 1` would tie against
/// `upper_bound`'s strict `>`). The output-holding device's cut extends to
/// `n_layer + 1`, landing the output unit exactly where [`override_patterns`]
/// pins `output.weight`. Returns per-device weights in plan device order
/// (llama.cpp normalizes internally); blockless devices get zero width. Pure.
pub fn dev_layer_tensor_split(plan: &[NodeShard], n_layer: u32) -> Vec<f32> {
    let n_dev = plan.iter().map(|s| s.device_index + 1).max().unwrap_or(0);
    let mut out = vec![0.0f32; n_dev];
    let mut prev_cut = 0.0f32;
    for d in 0..n_dev {
        let shard = plan.iter().find(|s| s.device_index == d);
        let cut = match shard.and_then(|s| s.blocks) {
            Some(_) if shard.is_some_and(|s| s.holds_output) => (n_layer + 1) as f32,
            Some((_, last)) => last as f32 + 0.5,
            None => prev_cut,
        };
        out[d] = cut - prev_cut;
        prev_cut = cut;
    }
    out
}

/// Read the transformer block count (`<arch>.block_count`) from a GGUF's metadata
/// — needed to plan the per-block split before the model is loaded. `None` if
/// absent / not a u32.
pub fn gguf_block_count(path: &Path) -> std::io::Result<Option<u32>> {
    let mut r = std::io::BufReader::new(fs::File::open(path)?);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        return Ok(None);
    }
    let _version = read_u32(&mut r)?;
    let _tensor_count = read_u64(&mut r)?;
    let kv_count = read_u64(&mut r)?;
    for _ in 0..kv_count {
        let key = read_gguf_string(&mut r)?;
        let vtype = read_u32(&mut r)?;
        if key.ends_with(".block_count") && vtype == 4 {
            return Ok(Some(read_u32(&mut r)?));
        }
        skip_metadata_value(&mut r, vtype)?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one shard-name parser. `shard_files`, `total_model_bytes`, and the
    /// mesh's `split_sibling_names` all route through this, so a disagreement
    /// about what "the whole model" means is no longer expressible.
    #[test]
    fn split_shard_names_is_the_single_parser() {
        // A split expands to every sibling, in order, preserving zero-pad width.
        assert_eq!(
            split_shard_names("m-00001-of-00003.gguf").unwrap(),
            vec![
                "m-00001-of-00003.gguf",
                "m-00002-of-00003.gguf",
                "m-00003-of-00003.gguf"
            ]
        );
        // Any shard of the set names the same set — the worker is handed shard
        // 2's name by the manifest and must still resolve the whole model.
        assert_eq!(
            split_shard_names("m-00002-of-00003.gguf").unwrap(),
            split_shard_names("m-00001-of-00003.gguf").unwrap()
        );
        // Real unsloth shape: dashes in the stem must not confuse the parser.
        let ds4 = split_shard_names("DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00001-of-00005.gguf")
            .expect("5-way split");
        assert_eq!(ds4.len(), 5);
        assert_eq!(
            ds4[4],
            "DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00005-of-00005.gguf"
        );
        // Not a split: plain name, a 1-of-1 "split", and non-numeric junk.
        assert!(split_shard_names("model.gguf").is_none());
        assert!(split_shard_names("m-00001-of-00001.gguf").is_none());
        assert!(split_shard_names("m-abc-of-00003.gguf").is_none());
        assert!(split_shard_names("m-00001-of-xyz.gguf").is_none());
    }

    #[test]
    fn shard_files_enumerates_siblings_and_never_guesses() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            p
        };
        // Non-split: itself.
        let single = mk("model.gguf");
        assert_eq!(shard_files(&single), vec![single.clone()]);
        // Split with all siblings present: full ordered list.
        let s1 = mk("m-00001-of-00003.gguf");
        let s2 = mk("m-00002-of-00003.gguf");
        let s3 = mk("m-00003-of-00003.gguf");
        assert_eq!(shard_files(&s1), vec![s1.clone(), s2, s3]);
        // A missing sibling → fall back to the named file alone (never guess);
        // downstream the empty-warm guard turns that into a loud refusal.
        let t1 = mk("t-00001-of-00002.gguf");
        assert_eq!(shard_files(&t1), vec![t1.clone()]);
    }

    /// Verbatim replica of llama.cpp's layer→device assignment
    /// (llama-model.cpp `get_layer_buft_list`, llama-cpp-sys-4 0.4.2): cumulative
    /// sum → normalize → `upper_bound(splits, il / (n_layer + 1))`. `il == n_layer`
    /// is the output head (`dev_output`). Assumes `n_gpu_layers ≥ n_layer + 1`
    /// (we pass 999), so `i_gpu_start == 0`.
    fn llama_dev_layer(tensor_split: &[f32], il: u32, n_layer: u32) -> usize {
        let mut splits = tensor_split.to_vec();
        let mut sum = 0.0f32;
        for s in splits.iter_mut() {
            sum += *s;
            *s = sum;
        }
        for s in splits.iter_mut() {
            *s /= sum;
        }
        let v = il as f32 / (n_layer + 1) as f32;
        splits
            .iter()
            .position(|&s| s > v) // std::upper_bound: first element strictly greater
            .expect("il < n_layer + 1 ⇒ v < 1.0 = last split point")
    }

    /// Every layer (and the output unit) must land on the device the plan
    /// assigned — this is the invariant whose violation was the distributed-GDN
    /// host abort (DISTRIBUTED_GDN_CRASH_STATUS.md §4).
    fn assert_dev_layer_agrees(plan: &[NodeShard], n_layer: u32) {
        let split = dev_layer_tensor_split(plan, n_layer);
        for il in 0..n_layer {
            let want = tensor_device("blk.x.weight", Some(il), plan)
                .expect("plan covers every block");
            let got = llama_dev_layer(&split, il, n_layer);
            assert_eq!(got, want, "layer {il} straddles: plan={want} dev_layer={got}");
        }
        let want_out = plan.iter().find(|s| s.holds_output).unwrap().device_index;
        assert_eq!(llama_dev_layer(&split, n_layer, n_layer), want_out, "output unit");
    }

    #[test]
    fn dev_layer_split_agrees_on_the_crash_repro_cut() {
        // The exact 2026-07-27 repro: Qwen3.5-4B, 32 layers, RPC0 blocks 0..=7,
        // Vulkan0 blocks 8..=31 + output. Layer 8 (a Gated DeltaNet layer) is the
        // one llama.cpp put on RPC0 while our -ot put its weights on Vulkan0.
        let plan = vec![
            NodeShard { device_index: 0, blocks: Some((0, 7)), holds_output: false, fraction: 0.25 },
            NodeShard { device_index: 1, blocks: Some((8, 31)), holds_output: true, fraction: 0.75 },
        ];
        assert_dev_layer_agrees(&plan, 32);
        // And the specific regression layer, named:
        let split = dev_layer_tensor_split(&plan, 32);
        assert_eq!(llama_dev_layer(&split, 8, 32), 1, "layer 8 must be Vulkan0");
    }

    #[test]
    fn naive_block_count_weights_reproduce_the_off_by_one() {
        // Documents WHY the midpoint math is load-bearing: the naive pin
        // (H2, 2026-07-27) passed block-count fractions [8, 24] ≡ [0.25, 0.75].
        // Over llama.cpp's n_layer+1 = 33 units, 0.25 × 33 = 8.25 → layer 8
        // lands on device 0 — the byte-identical warning H2 observed.
        assert_eq!(llama_dev_layer(&[8.0, 24.0], 8, 32), 0, "the falsified H2 pin straddles");
        // The corrected split puts the cut at 7.5/33 → layer 8 on device 1.
        assert_eq!(llama_dev_layer(&[7.5, 25.5], 8, 32), 1);
    }

    #[test]
    fn dev_layer_split_agrees_across_shapes() {
        // Three devices, uneven cut (the 122B-style multi-worker case).
        assert_dev_layer_agrees(&plan_shards(48, &[0.3, 0.2, 0.5]), 48);
        // A blockless middle device (quarantined worker got weight ~0).
        let plan = vec![
            NodeShard { device_index: 0, blocks: Some((0, 10)), holds_output: false, fraction: 0.34 },
            NodeShard { device_index: 1, blocks: None, holds_output: false, fraction: 0.0 },
            NodeShard { device_index: 2, blocks: Some((11, 31)), holds_output: true, fraction: 0.66 },
        ];
        assert_dev_layer_agrees(&plan, 32);
        // Single device degenerate case.
        assert_dev_layer_agrees(&plan_shards(32, &[1.0]), 32);
        // Boundary at every possible cut point of a small model: no layer count
        // or cut position may straddle.
        for cut in 1..8u32 {
            let plan = vec![
                NodeShard { device_index: 0, blocks: Some((0, cut - 1)), holds_output: false, fraction: 0.5 },
                NodeShard { device_index: 1, blocks: Some((cut, 7)), holds_output: true, fraction: 0.5 },
            ];
            assert_dev_layer_agrees(&plan, 8);
        }
    }

    #[test]
    fn plan_shards_apportions_contiguously_and_sums() {
        let p = plan_shards(12, &[0.5, 0.25, 0.25]);
        let total: u32 = p
            .iter()
            .filter_map(|s| s.blocks)
            .map(|(a, b)| b - a + 1)
            .sum();
        assert_eq!(total, 12, "every block assigned exactly once");
        assert_eq!(p[0].blocks, Some((0, 5)));
        assert_eq!(p[1].blocks, Some((6, 8)));
        assert_eq!(p[2].blocks, Some((9, 11)));
        assert!(p[2].holds_output, "output on the last block-holding device");
        assert!(!p[0].holds_output);
    }

    #[test]
    fn plan_shards_equal_weights_when_all_zero() {
        let p = plan_shards(4, &[0.0, 0.0]);
        assert_eq!(p[0].blocks, Some((0, 1)));
        assert_eq!(p[1].blocks, Some((2, 3)));
    }

    #[test]
    fn quantize_vram_is_stable_within_a_bucket() {
        let gb = 1024 * 1024 * 1024u64;
        assert_eq!(quantize_vram(0), 0);
        // ~44.8GB and ~45.1GB land in the same bucket → stable plan across a
        // reload where free VRAM jittered by a few hundred MB.
        assert_eq!(
            quantize_vram(44_800 * 1024 * 1024),
            quantize_vram(45_100 * 1024 * 1024)
        );
        // A tiny-but-nonzero device still gets one bucket (never apportioned to 0).
        assert_eq!(quantize_vram(200 * 1024 * 1024), 4 * gb);
        // Monotonic across buckets.
        assert!(quantize_vram(50 * gb) >= quantize_vram(44 * gb));
    }

    #[test]
    fn tensor_device_and_overrides_share_one_assignment() {
        let plan = plan_shards(4, &[0.5, 0.5]); // dev0:0-1, dev1:2-3 + output
        assert_eq!(
            tensor_device("blk.0.attn_q.weight", Some(0), &plan),
            Some(0)
        );
        assert_eq!(
            tensor_device("blk.3.ffn_down.weight", Some(3), &plan),
            Some(1)
        );
        assert_eq!(tensor_device("output.weight", None, &plan), Some(1));
        assert_eq!(tensor_device("token_embd.weight", None, &plan), None);
        let pats = override_patterns(&plan);
        assert!(pats.iter().any(|(p, d)| *d == 0 && p == "^blk\\.(0|1)\\."));
        assert!(pats.iter().any(|(p, d)| *d == 1 && p == "^blk\\.(2|3)\\."));
        assert!(pats.iter().any(|(p, d)| *d == 1 && p == "^output\\.weight"));
    }

    #[test]
    fn tensor_layer_parses_block_index() {
        assert_eq!(tensor_layer("blk.27.attn_q.weight"), Some(27));
        assert_eq!(tensor_layer("blk.0.ffn_gate.weight"), Some(0));
        // Globals (non-layer) have no block index.
        assert_eq!(tensor_layer("token_embd.weight"), None);
        assert_eq!(tensor_layer("output.weight"), None);
        assert_eq!(tensor_layer("output_norm.weight"), None);
        // Malformed / non-numeric block index is not a layer.
        assert_eq!(tensor_layer("blk.x.weight"), None);
        assert_eq!(tensor_layer("blk."), None);
    }

    #[test]
    fn fnv1a_matches_canonical_and_ggml() {
        // The canonical FNV-1a/64 test vector for "a" — also exactly what
        // ggml-rpc.cpp's fnv_hash produces, which is the whole correctness basis
        // for the cache (host hash == warm-file name).
        let mut h = Fnv1a::new();
        h.update(b"a");
        assert_eq!(h.finish(), 0xaf63_dc4c_8601_ec8c);
        // Empty input is the offset basis.
        assert_eq!(Fnv1a::new().finish(), 0xcbf2_9ce4_8422_2325);
        // Streaming in pieces must equal hashing the whole — the property the
        // byte-range warmer relies on when it hashes HTTP chunks incrementally.
        let mut split = Fnv1a::new();
        split.update(b"he");
        split.update(b"llo");
        let mut whole = Fnv1a::new();
        whole.update(b"hello");
        assert_eq!(split.finish(), whole.finish());
        // Cache filename is the zero-padded 16-hex of the hash.
        assert_eq!(cache_file_name(0xaf63_dc4c_8601_ec8c), "af63dc4c8601ec8c");
    }

    // ── byte-mass-aware split (plan_shards_weighted) ──

    fn assigned_blocks(plan: &[NodeShard]) -> u32 {
        plan.iter()
            .filter_map(|s| s.blocks)
            .map(|(a, b)| b - a + 1)
            .sum()
    }
    fn held_bytes(s: &NodeShard, block_bytes: &[u64]) -> u64 {
        s.blocks
            .map(|(a, b)| (a..=b).map(|i| block_bytes[i as usize]).sum::<u64>())
            .unwrap_or(0)
    }
    fn count(s: &NodeShard) -> u32 {
        s.blocks.map(|(a, b)| b - a + 1).unwrap_or(0)
    }

    #[test]
    fn weighted_empty_bytes_is_the_count_split() {
        // No size table (or a wrong-length one) → byte-aware degrades to the exact
        // count-based plan, so `plan_shards` stays a pure special-case of it.
        let w = [0.5, 0.5];
        assert_eq!(plan_shards_weighted(4, &w, &[], 0), plan_shards(4, &w));
        assert_eq!(
            plan_shards_weighted(4, &w, &[1, 2, 3], 0),
            plan_shards(4, &w)
        );
    }

    #[test]
    fn weighted_uniform_mass_matches_count_split() {
        // When every block has equal mass, byte-proportional == count-proportional,
        // so the byte-aware split reproduces the count split (no regression on dense
        // or uniform all-MoE models).
        let bb = vec![500u64; 12];
        let w = [0.5, 0.25, 0.25];
        let byte: Vec<_> = plan_shards_weighted(12, &w, &bb, 0)
            .iter()
            .map(|s| s.blocks)
            .collect();
        let cnt: Vec<_> = plan_shards(12, &w).iter().map(|s| s.blocks).collect();
        assert_eq!(
            byte, cnt,
            "uniform mass → byte-aware ranges == count ranges"
        );
    }

    #[test]
    fn weighted_balances_clustered_mass_better_than_count() {
        // A DeepSeek-shaped model: 3 tiny leading dense blocks, then 9 heavy MoE
        // blocks. Two EQUAL devices should each end with ~half the BYTE mass — but
        // a count split (6 blocks each) gives dev0 only 3 of the 9 heavy blocks.
        let n = 12u32;
        let mut bb = vec![100u64; 3];
        bb.extend(std::iter::repeat(1000u64).take(9)); // total = 300 + 9000 = 9300
        let total: u64 = bb.iter().sum();

        let byte = plan_shards_weighted(n, &[1.0, 1.0], &bb, 0);
        assert_eq!(assigned_blocks(&byte), n, "tiles every block exactly once");

        let (b0, b1) = (held_bytes(&byte[0], &bb), held_bytes(&byte[1], &bb));
        let target = total / 2;
        assert!(
            (b0 as i64 - target as i64).unsigned_abs() <= total / 8,
            "dev0 byte mass {b0} within 12.5% of {target}"
        );

        // The win, made explicit: byte-aware is strictly more balanced than count.
        let cnt = plan_shards(n, &[1.0, 1.0]);
        let (c0, c1) = (held_bytes(&cnt[0], &bb), held_bytes(&cnt[1], &bb));
        let byte_ratio = b0.max(b1) as f64 / b0.min(b1) as f64;
        let count_ratio = c0.max(c1) as f64 / c0.min(c1) as f64;
        assert!(
            byte_ratio < count_ratio,
            "byte-aware imbalance {byte_ratio:.2}x < count imbalance {count_ratio:.2}x"
        );
    }

    #[test]
    fn weighted_folds_output_head_onto_last_device() {
        // The head rides on the last (host) device, so it should be handed a
        // smaller BLOCK budget to compensate — else it runs ~head_bytes heavy.
        let bb = vec![100u64; 10]; // block mass 1000
        let head = 300u64; // ~3 blocks of head mass on the host (dev1)
        let plan = plan_shards_weighted(10, &[1.0, 1.0], &bb, head);
        assert_eq!(assigned_blocks(&plan), 10);
        assert!(plan[1].holds_output, "last device holds the output head");
        assert!(
            count(&plan[0]) > count(&plan[1]),
            "head-holder gets fewer blocks: dev0={} dev1={}",
            count(&plan[0]),
            count(&plan[1])
        );
    }

    #[test]
    fn weighted_zero_weight_device_gets_no_blocks() {
        // A member advertising 0 VRAM must not be handed a shard.
        let bb = vec![100u64; 6];
        let plan = plan_shards_weighted(6, &[1.0, 0.0, 1.0], &bb, 0);
        assert!(plan[1].blocks.is_none(), "zero-VRAM device holds no blocks");
        assert_eq!(
            assigned_blocks(&plan),
            6,
            "the other two still tile all blocks"
        );
    }

    #[test]
    fn weighted_contiguous_and_gapless_on_heterogeneous_vram() {
        // Contiguity is load-bearing (hops stay D-1; override_patterns/tensor_device
        // assume it). Heterogeneous VRAM + skewed mass must still tile [0, n) with no
        // gap or overlap and ranges in ascending device order.
        let mut bb: Vec<u64> = (0..40)
            .map(|i| if i % 2 == 0 { 20 } else { 1200 })
            .collect();
        bb[0] = 5; // a lighter leading block for good measure
        let plan = plan_shards_weighted(40, &[4.0, 2.0, 2.0], &bb, 800);
        assert_eq!(assigned_blocks(&plan), 40);
        let mut next = 0u32;
        for s in &plan {
            if let Some((a, b)) = s.blocks {
                assert_eq!(a, next, "no gap/overlap between contiguous shards");
                assert!(b >= a);
                next = b + 1;
            }
        }
        assert_eq!(next, 40, "shards cover the whole block range");
    }

    #[test]
    fn routed_expert_classifier_splits_hot_from_cold() {
        // Cold routed experts (`_exps`) vs hot shared expert (`_shexp`) / router
        // (`ffn_gate_inp`) / attention — the distinction the hot/cold report rests on.
        assert!(is_routed_expert_tensor("blk.3.ffn_down_exps.weight"));
        assert!(is_routed_expert_tensor("blk.0.ffn_gate_exps.weight"));
        assert!(is_routed_expert_tensor("blk.7.ffn_up_exps.weight"));
        assert!(!is_routed_expert_tensor("blk.3.ffn_down_shexp.weight")); // shared = hot
        assert!(!is_routed_expert_tensor("blk.3.ffn_gate_inp.weight")); // router = hot
        assert!(!is_routed_expert_tensor("blk.3.attn_q.weight"));
        assert!(!is_routed_expert_tensor("output.weight"));
    }

    // ── the shared fit decider ──────────────────────────────────────────────

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A 4-block model: 1 GiB per block, a 1 GiB head, 2 GiB of embeddings.
    fn simple_sizes() -> Vec<(String, Option<u32>, u64)> {
        let mut v: Vec<(String, Option<u32>, u64)> = (0..4)
            .map(|i| (format!("blk.{i}.attn_q.weight"), Some(i), GIB))
            .collect();
        v.push(("output.weight".into(), None, GIB));
        v.push(("token_embd.weight".into(), None, 2 * GIB));
        v
    }

    #[test]
    fn model_mass_separates_what_lands_in_different_places() {
        let m = model_mass_from_sizes(&simple_sizes(), 4);
        assert_eq!(m.block_bytes, vec![GIB; 4]);
        assert_eq!(m.head_bytes, GIB, "the head rides with the last block-holder");
        assert_eq!(
            m.embd_bytes,
            2 * GIB,
            "token_embd stays in host RAM and must not be charged to a device"
        );
        assert_eq!(m.other_global_bytes, 0);
        assert_eq!(m.total_bytes(), 7 * GIB);
        assert!(m.is_known());
        assert!(!m.recurrent);
    }

    #[test]
    fn model_mass_counts_an_out_of_range_layer_rather_than_dropping_it() {
        // A dropped byte is a byte the fit check will not charge anyone for.
        let sizes = vec![
            ("blk.0.attn_q.weight".into(), Some(0), GIB),
            ("blk.99.attn_q.weight".into(), Some(99), 5 * GIB),
        ];
        let m = model_mass_from_sizes(&sizes, 1);
        assert_eq!(m.block_bytes, vec![GIB]);
        assert_eq!(m.other_global_bytes, 5 * GIB);
        assert_eq!(m.total_bytes(), 6 * GIB);
    }

    #[test]
    fn model_mass_notices_moe_and_recurrent_stacks() {
        let sizes = vec![
            ("blk.0.ffn_gate_exps.weight".into(), Some(0), 8 * GIB),
            ("blk.0.ssm_a".into(), Some(0), GIB),
        ];
        let m = model_mass_from_sizes(&sizes, 1);
        assert_eq!(m.routed_expert_bytes, 8 * GIB);
        assert!(m.recurrent);
        assert_eq!(
            m.block_bytes[0],
            9 * GIB,
            "routed-expert mass is resident and still counts against fit"
        );
    }

    #[test]
    fn an_unread_tensor_table_is_never_a_pass() {
        // This is the whole reason `shard_fits` returns Option. Judging against
        // an empty mass would clear every device on the strength of zeros —
        // turning a failed header parse into a clean bill of health.
        let mass = ModelMass::default();
        assert!(!mass.is_known());
        let plan = plan_shards(4, &[1.0, 1.0]);
        assert_eq!(shard_fits(&plan, &[GIB, GIB], &mass, 1.2, None), None);
    }

    #[test]
    fn shard_fits_reports_every_device_not_just_the_failures() {
        // A Result-shaped decider would force `mesh plan` to keep its own
        // traversal to print the fitting rows — and a second traversal is the
        // drift this function exists to remove.
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(4, &[1.0, 1.0]);
        let fits = shard_fits(&plan, &[10 * GIB, 10 * GIB], &mass, 1.2, None).expect("judgeable");
        assert_eq!(fits.len(), 2, "one row per shard, fitting rows included");
        assert!(fits.iter().all(|f| f.fits()));
        // Device 1 holds blocks 2-3 plus the head: 3 GiB, needing 3.6 with headroom.
        assert_eq!(fits[1].held_bytes, 3 * GIB);
        assert_eq!(fits[1].need_bytes, (3.0 * GIB as f64 * 1.2) as u64);
        assert!(fits[1].slack_bytes() > 0);
    }

    #[test]
    fn a_device_that_cannot_hold_its_share_overflows() {
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(4, &[1.0, 1.0]);
        // Device 0 holds blocks 0-1 = 2 GiB, needing 2.4 GiB; it has 2.
        let fits = shard_fits(&plan, &[2 * GIB, 10 * GIB], &mass, 1.2, None).expect("judgeable");
        assert!(!fits[0].fits());
        assert!(fits[0].slack_bytes() < 0);
        assert!(fits[1].fits(), "one overflow must not condemn the others");
    }

    #[test]
    fn pooled_memory_can_pass_where_a_device_fails() {
        // The exact hole the per-device gate closes: 12 GiB pooled against a
        // 7 GiB model is comfortable, and the split still overflows one device.
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(4, &[1.0, 1.0]);
        let capacities = [1 * GIB, 11 * GIB];
        assert!(
            capacities.iter().sum::<u64>() > (mass.total_bytes() as f64 * 1.2) as u64,
            "the aggregate gate would wave this through"
        );
        let fits = shard_fits(&plan, &capacities, &mass, 1.2, None).expect("judgeable");
        assert!(fits.iter().any(|f| !f.fits()));
    }

    #[test]
    fn headroom_scales_the_requirement_so_lowering_it_helps() {
        // `need = held × headroom`. The refusal message tells operators to
        // LOWER the headroom; this is the arithmetic that makes that correct.
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(4, &[1.0, 1.0]);
        let capacities = [2 * GIB, 10 * GIB];
        assert!(!shard_fits(&plan, &capacities, &mass, 1.2, None).expect("judgeable")[0].fits());
        assert!(
            shard_fits(&plan, &capacities, &mass, 1.0, None).expect("judgeable")[0].fits(),
            "lowering the headroom must be able to rescue a marginal fit"
        );
    }

    /// THE 2026-08-02 FAILURE SHAPE. A share whose weights squeeze inside the
    /// capacity at headroom 1.0 — a 0.07 GiB-class margin — passes a
    /// weights-only judgement, reaches `serving`, and dies allocating KV +
    /// compute on the first inference. With the projected overheads the same
    /// plan is refused BEFORE the minutes-long warm is spent on it.
    #[test]
    fn a_weights_only_pass_with_no_room_for_kv_is_refused_with_overheads() {
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(4, &[1.0, 1.0]);
        // Device 0 holds blocks 0-1 = 2 GiB and has 2 GiB + a sliver.
        let capacities = [2 * GIB + GIB / 16, 10 * GIB];
        assert!(
            shard_fits(&plan, &capacities, &mass, 1.0, None).expect("judgeable")[0].fits(),
            "precondition: weights-only judgement passes on the sliver margin"
        );
        let o = PlanOverheads {
            context_total_bytes: GIB,     // 256 MiB/block over 4 blocks
            compute_accel_bytes: GIB / 2, // every device reserves scratch
            compute_host_bytes: GIB,      // host-side scheduler buffer
        };
        let fits = shard_fits(&plan, &capacities, &mass, 1.0, Some(&o)).expect("judgeable");
        assert!(
            !fits[0].fits(),
            "2 GiB weights + 512 MiB KV share + 512 MiB scratch cannot live in 2.06 GiB"
        );
        // Overheads are charged where they land: device 0 gets its KV share +
        // accel scratch; device 1 (the host, last in plan order) additionally
        // carries the scheduler buffer.
        assert_eq!(fits[0].overhead_bytes, GIB / 2 + GIB / 2);
        assert_eq!(fits[1].overhead_bytes, GIB / 2 + GIB / 2 + GIB);
        assert_eq!(
            fits[1].need_bytes,
            fits[1].held_bytes + fits[1].overhead_bytes,
            "at headroom 1.0 the need is exactly weights + projected overheads"
        );
    }

    /// The KV share follows the blocks, not the device count — a device
    /// holding no blocks is charged no context (only its compute scratch).
    #[test]
    fn overhead_context_is_charged_pro_rata_by_blocks() {
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        // All four blocks on device 1; device 0 idles.
        let plan = vec![
            NodeShard {
                device_index: 0,
                blocks: None,
                holds_output: false,
                fraction: 0.0,
            },
            NodeShard {
                device_index: 1,
                blocks: Some((0, 3)),
                holds_output: true,
                fraction: 1.0,
            },
        ];
        let o = PlanOverheads {
            context_total_bytes: 4 * GIB,
            compute_accel_bytes: GIB / 4,
            compute_host_bytes: 0,
        };
        let fits = shard_fits(&plan, &[10 * GIB, 10 * GIB], &mass, 1.0, Some(&o))
            .expect("judgeable");
        assert_eq!(fits[0].overhead_bytes, GIB / 4, "no blocks → no KV, scratch only");
        assert_eq!(fits[1].overhead_bytes, 4 * GIB + GIB / 4, "all blocks → all KV");
    }

    #[test]
    fn inputs_that_do_not_describe_each_other_cannot_be_judged() {
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(4, &[1.0, 1.0]);
        assert_eq!(
            shard_fits(&plan, &[GIB], &mass, 1.2, None),
            None,
            "one capacity for two devices is not a verdict"
        );
        assert_eq!(shard_fits(&[], &[], &mass, 1.2, None), None);
        assert_eq!(
            shard_fits(&plan, &[GIB, GIB], &mass, 0.5, None),
            None,
            "headroom below 1.0 would ask a device to hold less than it holds"
        );
        assert_eq!(shard_fits(&plan, &[GIB, GIB], &mass, f64::NAN, None), None);
    }

    #[test]
    fn a_plan_for_a_different_model_cannot_be_judged() {
        // A block range past the end of the mass table means the two describe
        // different models. Refusing beats silently under-charging the device.
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(8, &[1.0, 1.0]);
        assert_eq!(shard_fits(&plan, &[GIB, GIB], &mass, 1.2, None), None);
    }

    #[test]
    fn a_device_holding_no_blocks_needs_nothing() {
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        // Four blocks across three devices weighted 10:1:0 — the last gets none.
        let plan = plan_shards_weighted(4, &[10.0, 1.0, 0.0], &mass.block_bytes, mass.head_bytes);
        let fits = shard_fits(&plan, &[10 * GIB, 10 * GIB, 0], &mass, 1.2, None).expect("judgeable");
        let idle = fits.last().expect("three rows");
        assert_eq!(idle.held_bytes, 0);
        assert!(
            idle.fits(),
            "a device holding nothing must not be reported as overflowing"
        );
    }

    #[test]
    fn fit_rows_follow_the_plans_device_index_not_their_position() {
        // The index-space trap: capacities arrive in PLAN order, and a caller
        // that maps them through its own device numbering silently attributes
        // every row to the wrong machine.
        let mass = model_mass_from_sizes(&simple_sizes(), 4);
        let plan = plan_shards(4, &[1.0, 1.0]);
        let fits = shard_fits(&plan, &[2 * GIB, 10 * GIB], &mass, 1.2, None).expect("judgeable");
        for (pos, f) in fits.iter().enumerate() {
            assert_eq!(f.device_index, plan[pos].device_index);
        }
        assert_eq!(fits[0].capacity_bytes, 2 * GIB);
        assert_eq!(fits[1].capacity_bytes, 10 * GIB);
    }
}
