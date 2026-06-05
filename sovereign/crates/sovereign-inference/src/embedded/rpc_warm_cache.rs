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

#[inline]
fn fnv1a_update(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// Pre-warm `cache_dir` from `gguf_path`: for every tensor larger than the RPC
/// hash threshold, write `cache_dir/<016x FNV-1a>` containing the tensor's raw
/// bytes — the exact files the worker would otherwise receive over the network.
/// Idempotent: a file already present with the right size is left untouched.
pub fn warm_cache_from_gguf(gguf_path: &Path, cache_dir: &Path) -> std::io::Result<WarmCacheStats> {
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
        stats.tensors_cacheable += 1;

        // First pass: stream the tensor's bytes to compute its FNV-1a hash.
        let start = data_offset + t.offset;
        file.seek(SeekFrom::Start(start))?;
        let mut hash = FNV_OFFSET;
        let mut remaining = nbytes;
        while remaining > 0 {
            let take = remaining.min(CHUNK as u64) as usize;
            file.read_exact(&mut buf[..take])?;
            fnv1a_update(&mut hash, &buf[..take]);
            remaining -= take as u64;
        }
        let hash_str = format!("{hash:016x}");
        let cache_file = cache_dir.join(&hash_str);

        // Idempotent: skip if already present with the right size.
        if let Ok(meta) = fs::metadata(&cache_file) {
            if meta.len() == nbytes {
                stats.already_present += 1;
                continue;
            }
        }

        // Second pass: stream the bytes into the cache file (atomic via temp +
        // rename so a partial write never looks like a valid cache entry).
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
