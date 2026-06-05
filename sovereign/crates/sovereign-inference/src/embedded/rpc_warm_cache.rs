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
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| Path::new(&h).join(".sovereign").join("rpc-cache"))
        })
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
        4 | 5 | 6 => {
            r.seek(SeekFrom::Current(4))?;
        } // u32 / i32 / f32
        10 | 11 | 12 => {
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
        crate::llama::sys::ggml_row_size(t.ggml_type as crate::llama::sys::ggml_type, t.dims[0] as i64)
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
    name.strip_prefix("blk.")?.split('.').next()?.parse::<u32>().ok()
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

/// Build the full tensor manifest from a local GGUF — name, layer, content hash,
/// size, and GGUF offset for every tensor. Streams each cacheable tensor once to
/// hash it (no model load, no GPU). This is the shard planner's input: it maps a
/// per-node layer assignment to the exact set of cache blobs (by hash) / byte
/// ranges each node must hold, so workers fetch + warm only their shard.
pub fn build_manifest(gguf_path: &Path) -> std::io::Result<Vec<TensorManifestEntry>> {
    let (tensors, data_offset) = parse_gguf(gguf_path)?;
    let mut file = fs::File::open(gguf_path)?;
    let mut buf = vec![0u8; CHUNK];
    let mut out = Vec::with_capacity(tensors.len());
    for t in &tensors {
        let nbytes = tensor_nbytes(t);
        let cacheable = nbytes > HASH_THRESHOLD;
        let start = data_offset + t.offset;
        // Only cacheable tensors are ever hash-matched; a non-cacheable tensor's
        // hash is never looked up, so leave it 0 rather than pay to stream it.
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
        });
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
    let (tensors, data_offset) = parse_gguf(gguf_path)?;
    fs::create_dir_all(cache_dir)?;

    let mut stats = WarmCacheStats {
        tensors_total: tensors.len(),
        cache_dir: cache_dir.to_path_buf(),
        ..Default::default()
    };

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

    Ok(stats)
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

/// Our OWN contiguous placement policy: apportion `n_layer` transformer blocks
/// across devices proportional to `weights` (RPC-first order), with exact integer
/// counts via largest-remainder so they sum to `n_layer`. The output head goes on
/// the last block-holding device. We do NOT predict llama.cpp's split — we ENFORCE
/// this assignment via `tensor_buft_overrides` at load and warm the same
/// assignment, so there is nothing to diverge from. Pure + deterministic.
pub fn plan_shards(n_layer: u32, weights: &[f32]) -> Vec<NodeShard> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let total: f32 = weights.iter().sum();
    let eff: Vec<f32> = if total > 0.0 { weights.to_vec() } else { vec![1.0; n] };
    let sum: f32 = eff.iter().sum();
    // Largest-remainder apportionment of `n_layer` blocks.
    let ideal: Vec<f32> = eff.iter().map(|&w| w / sum * n_layer as f32).collect();
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
                fraction: eff[d] / sum,
            }
        })
        .collect()
}

/// The cacheable output head (LM projection) — placed with the last block-holding
/// device, distinct from `token_embd` (input) which stays on the host CPU.
pub fn is_output_tensor(name: &str) -> bool {
    name == "output.weight"
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
    fn plan_shards_apportions_contiguously_and_sums() {
        let p = plan_shards(12, &[0.5, 0.25, 0.25]);
        let total: u32 = p.iter().filter_map(|s| s.blocks).map(|(a, b)| b - a + 1).sum();
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
    fn tensor_device_and_overrides_share_one_assignment() {
        let plan = plan_shards(4, &[0.5, 0.5]); // dev0:0-1, dev1:2-3 + output
        assert_eq!(tensor_device("blk.0.attn_q.weight", Some(0), &plan), Some(0));
        assert_eq!(tensor_device("blk.3.ffn_down.weight", Some(3), &plan), Some(1));
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
}
