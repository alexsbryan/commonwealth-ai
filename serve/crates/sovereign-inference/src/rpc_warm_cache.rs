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

use sovereign_contracts::gguf_shards::shard_files;

pub(crate) mod ggml_type;
mod plan;
mod split;
pub use plan::*;
pub use split::*;

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

/// Where the in-process RPC worker looks for cached tensors — or `None` when
/// the operator turned caching off (`SOVEREIGN_RPC_CACHE_DIR` = `off` / `0` /
/// empty). Default: `~/.svrnmesh/rpc-cache`.
///
/// THE resolution of that variable (ARCH §10.6). There were three, and the
/// third existed *because* this one was wrong: `sovereign-mesh`'s
/// `rpc_warm_http::worker_cache_dir` carried a note saying
/// "`sovereign-inference`'s `default_cache_dir` doesn't model the disabled
/// case, which is why this lives here". It didn't — it returned
/// `Some(PathBuf::from("off"))`, so with caching disabled the warm path wrote
/// shards into a directory literally named `off` while the worker, which DOES
/// model the disabled case, read nothing. The upload the cache exists to avoid
/// happened anyway, into a junk directory.
pub fn default_cache_dir() -> Option<PathBuf> {
    resolve_cache_dir(
        std::env::var("SOVEREIGN_RPC_CACHE_DIR").ok().as_deref(),
        || sovereign_contracts::rebrand::svrnmesh_root().join("rpc-cache"),
    )
}

/// The rule itself, with the environment and the home directory split off so
/// the disabling spellings are testable.
fn resolve_cache_dir(raw: Option<&str>, default: impl FnOnce() -> PathBuf) -> Option<PathBuf> {
    match raw {
        Some(v) => {
            let v = v.trim();
            (!(v.is_empty() || v.eq_ignore_ascii_case("off") || v == "0")).then(|| PathBuf::from(v))
        }
        None => Some(default()),
    }
}

#[cfg(test)]
mod cache_dir_tests {
    use super::*;

    /// Every spelling that means "off". The writer used to accept all of them
    /// as directory NAMES and warm shards into `./off`.
    #[test]
    fn the_disabling_spellings_yield_no_directory() {
        let d = || PathBuf::from("/default");
        for off in ["off", "OFF", "0", "", "  ", " off "] {
            assert_eq!(
                resolve_cache_dir(Some(off), d),
                None,
                "{off:?} must disable the cache, not name a directory"
            );
        }
    }

    #[test]
    fn unset_is_the_default_and_a_path_is_taken_trimmed() {
        assert_eq!(
            resolve_cache_dir(None, || PathBuf::from("/default")),
            Some(PathBuf::from("/default"))
        );
        assert_eq!(
            resolve_cache_dir(Some("  /tmp/cache  "), || PathBuf::from("/default")),
            Some(PathBuf::from("/tmp/cache"))
        );
    }
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
/// `row_size(type, ne0) * ne1 * ne2 * …`, from [`ggml_type::ggml_row_bytes`].
/// A type id ggml does not define is an `InvalidData` error naming it.
fn tensor_nbytes(t: &TensorInfo) -> std::io::Result<u64> {
    if t.dims.is_empty() {
        return Ok(0);
    }
    let row = ggml_type::ggml_row_bytes(t.ggml_type, t.dims[0]).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("tensor {}: unknown ggml type {}", t.name, t.ggml_type),
        )
    })?;
    let mut n = row;
    for d in &t.dims[1..] {
        n = n.saturating_mul(*d);
    }
    Ok(n)
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
            let nbytes = tensor_nbytes(t)?;
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
            out.push((t.name.clone(), tensor_layer(&t.name), tensor_nbytes(t)?));
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
        let nbytes = tensor_nbytes(t)?;
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

#[cfg(test)]
mod tests;
