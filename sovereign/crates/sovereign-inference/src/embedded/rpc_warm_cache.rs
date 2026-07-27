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
        .or_else(|| dirs::home_dir().map(|h| h.join(".sovereign").join("rpc-cache")))
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

/// All files holding this model's tensor data: `[path]` for a single-file
/// model, or every `-NNNNN-of-NNNNN.gguf` sibling (in shard order) for a
/// split. Split shards are each standalone GGUFs with their own header +
/// tensor-info + data section, so every downstream reader parses them
/// per-file. Returns `[path]` (never guesses) when any sibling is missing.
pub(crate) fn shard_files(model_path: &Path) -> Vec<std::path::PathBuf> {
    let single = vec![model_path.to_path_buf()];
    let Some(name) = model_path.file_name().and_then(|n| n.to_str()) else {
        return single;
    };
    let Some(dir) = model_path.parent() else {
        return single;
    };
    // Parse `<stem>-<idx>-of-<count>.gguf` (same shape total_model_bytes uses).
    let parsed = name.rfind("-of-").and_then(|of| {
        let count: u32 = name.get(of + 4..)?.strip_suffix(".gguf")?.parse().ok()?;
        let before = name.get(..of)?;
        let dash = before.rfind('-')?;
        let idx = before.get(dash + 1..)?;
        idx.parse::<u32>().ok()?;
        Some((before.get(..dash)?.to_string(), count, idx.len()))
    });
    let Some((stem, count, width)) = parsed else {
        return single;
    };
    if count <= 1 {
        return single;
    }
    let mut files = Vec::with_capacity(count as usize);
    for i in 1..=count {
        let shard = dir.join(format!("{stem}-{i:0width$}-of-{count:0width$}.gguf"));
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

/// Per-device BLOCK-COUNT fractions for `plan`, in device order — the value to
/// hand llama.cpp as `tensor_split` so its own per-layer device assignment
/// agrees with the `-ot` overrides built from the same plan.
///
/// This exists because `-ot` moves tensor BUFFERS only. llama.cpp keeps a
/// separate notion of which device owns layer `il`, derived from the device list
/// + `tensor_split`; with no `tensor_split` it falls back to a VRAM-proportional
/// default. On a heterogeneous mesh the two rules round differently and the cut
/// points disagree by a layer — measured 2026-07-27 on Qwen3.5-4B across
/// RuggedFox+BeefyMac: device weights 40 GB / 116 GB put llama.cpp's cut at
/// 40/156 ≈ 25.6% of 32 layers ≈ 8.2 (so layer 8 → RPC0) while the byte-mass
/// plan gives RPC0 exactly 25.0% (blocks 0..=7, so `blk.8.*` → local). That one
/// layer has its OPS on one device and its WEIGHTS on the other, which is
/// exactly what `resolve_fused_ops` reports. On a hybrid model the straddled
/// layer is a Gated DeltaNet layer, its fused kernel is disabled, and the
/// unfused path's `GGML_OP_SET` reaches the RPC worker with a `buffer == nullptr
/// && data == nullptr` dst — which passes `create_node`'s asymmetric guard
/// (ggml-rpc.cpp:1285 only rejects null-buffer-with-non-null-data) and
/// segfaults the worker in the backend's set op.
///
/// Deriving both from one plan removes the disagreement at the source rather
/// than steering around it. Empty when no device holds blocks. Pure.
pub fn tensor_split_from_plan(plan: &[NodeShard]) -> Vec<f32> {
    let total: u32 = plan
        .iter()
        .filter_map(|s| s.blocks.map(|(a, b)| b - a + 1))
        .sum();
    if total == 0 {
        return Vec::new();
    }
    plan.iter()
        .map(|s| {
            let n = s.blocks.map(|(a, b)| b - a + 1).unwrap_or(0);
            n as f32 / total as f32
        })
        .collect()
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
}
