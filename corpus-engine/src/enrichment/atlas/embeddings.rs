//! Persistent cache for atlas Entity embeddings.
//!
//! The eval / runtime atlas-grounded retrieval path needs an embedding
//! per non-placeholder Entity: the question embedding is cosined
//! against this bag and the top-K matches are fused into the chunk hit
//! set as virtual `ScoredChunk`s (see
//! `sovereign-cli/src/eval_cmd/runner.rs::atlas_top_k_as_chunks`).
//!
//! Without a cache, every daemon boot or eval run pays the full embed
//! cost — on a wiki-scale atlas with 50K+ entities that is ~40 min of
//! sequential `/v1/embeddings` calls. This module persists the
//! embeddings alongside `atoms.json` so the cost is paid once per
//! `(atoms.json content, embed model, filter signature)` triple.
//!
//! ## File layout: `atlas/atoms.embeddings.bin`
//!
//! ```text
//! magic[8]                      : "SOVATL01"
//! header_len: u32 LE
//! header_json: header_len bytes : UTF-8 JSON (CachedHeader)
//! data: entry_count * embed_dim * 4 bytes : raw f32 LE
//! ```
//!
//! The header carries everything needed to invalidate the cache:
//! `embed_model`, `embed_dim`, `atoms_content_hash` (SHA-256 of
//! atoms.json) and `filter_signature` (a caller-supplied string that
//! captures whatever subset was embedded, e.g. depth allowlist + min
//! description chars). A mismatch on any field forces a re-embed.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"SOVATL01";
const SCHEMA_VERSION: u32 = 1;

/// One row in the cache: canonical_name + the `embed_text` that was
/// passed to the embedder (so callers can use it as the virtual
/// chunk's `content` without re-deriving it from atoms.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAtlasEntry {
    pub canonical_name: String,
    pub embed_text: String,
    /// Length is `embed_dim` from the header.
    #[serde(skip)]
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedHeader {
    schema_version: u32,
    embed_model: String,
    embed_dim: usize,
    atoms_content_hash: String,
    filter_signature: String,
    entries: Vec<CachedRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedRow {
    canonical_name: String,
    embed_text: String,
}

/// Compute the SHA-256 of the atoms.json file at `<atlas_dir>/atoms.json`.
/// Used by callers to key the embeddings cache. Cheap (~30 ms on the
/// wiki-l5-tier2-full atoms.json file at ~50 MB).
pub fn atoms_content_hash(atlas_dir: &Path) -> io::Result<String> {
    let path = atlas_dir.join("atoms.json");
    let mut f = fs::File::open(&path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Read the embeddings cache. Returns `Ok(None)` on any
/// invalidation: missing file, magic mismatch, schema bump, or any
/// header field disagreeing with the caller's expectations. Returns
/// an `Err` only on hard I/O / format corruption — invalidation is a
/// soft miss.
pub fn read_atlas_embeddings(
    atlas_dir: &Path,
    expected_model: &str,
    expected_atoms_hash: &str,
    expected_filter_signature: &str,
) -> io::Result<Option<Vec<CachedAtlasEntry>>> {
    let path = atlas_dir.join("atoms.embeddings.bin");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    if bytes.len() < MAGIC.len() + 4 {
        return Ok(None);
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Ok(None);
    }
    let mut cursor = MAGIC.len();
    let header_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("header_len read: {e}"))
    })?) as usize;
    cursor += 4;
    if bytes.len() < cursor + header_len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "embeddings cache: header truncated",
        ));
    }
    let header: CachedHeader = serde_json::from_slice(&bytes[cursor..cursor + header_len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("header parse: {e}")))?;
    cursor += header_len;

    if header.schema_version != SCHEMA_VERSION {
        return Ok(None);
    }
    if header.embed_model != expected_model
        || header.atoms_content_hash != expected_atoms_hash
        || header.filter_signature != expected_filter_signature
    {
        return Ok(None);
    }

    let entry_count = header.entries.len();
    let expected_data_bytes = entry_count
        .saturating_mul(header.embed_dim)
        .saturating_mul(4);
    if bytes.len() != cursor + expected_data_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "embeddings cache: data size mismatch (expected {expected_data_bytes}, got {})",
                bytes.len() - cursor
            ),
        ));
    }

    let mut out = Vec::with_capacity(entry_count);
    let stride = header.embed_dim * 4;
    for (i, row) in header.entries.into_iter().enumerate() {
        let start = cursor + i * stride;
        let mut embedding = Vec::with_capacity(header.embed_dim);
        for j in 0..header.embed_dim {
            let b = &bytes[start + j * 4..start + j * 4 + 4];
            embedding.push(f32::from_le_bytes(b.try_into().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("f32 read: {e}"))
            })?));
        }
        out.push(CachedAtlasEntry {
            canonical_name: row.canonical_name,
            embed_text: row.embed_text,
            embedding,
        });
    }
    Ok(Some(out))
}

/// Write the embeddings cache. Atomic via sibling `.tmp` + rename so
/// a crash leaves a pre-existing cache intact rather than truncated.
/// `entries.len() == embeddings.len()` and every embedding has length
/// `embed_dim` — debug-asserted.
pub fn write_atlas_embeddings(
    atlas_dir: &Path,
    embed_model: &str,
    embed_dim: usize,
    atoms_content_hash: &str,
    filter_signature: &str,
    entries: &[CachedAtlasEntry],
) -> io::Result<PathBuf> {
    debug_assert!(
        entries.iter().all(|e| e.embedding.len() == embed_dim),
        "every entry's embedding must have length embed_dim"
    );
    fs::create_dir_all(atlas_dir)?;
    let header = CachedHeader {
        schema_version: SCHEMA_VERSION,
        embed_model: embed_model.to_string(),
        embed_dim,
        atoms_content_hash: atoms_content_hash.to_string(),
        filter_signature: filter_signature.to_string(),
        entries: entries
            .iter()
            .map(|e| CachedRow {
                canonical_name: e.canonical_name.clone(),
                embed_text: e.embed_text.clone(),
            })
            .collect(),
    };
    let header_bytes = serde_json::to_vec(&header)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("header serialise: {e}")))?;
    if header_bytes.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "embeddings cache: header exceeds 4 GB",
        ));
    }

    let path = atlas_dir.join("atoms.embeddings.bin");
    let tmp = atlas_dir.join(".atoms.embeddings.bin.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(MAGIC)?;
        f.write_all(&(header_bytes.len() as u32).to_le_bytes())?;
        f.write_all(&header_bytes)?;
        for entry in entries {
            for v in &entry.embedding {
                f.write_all(&v.to_le_bytes())?;
            }
        }
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn entry(name: &str, dim: usize, seed: f32) -> CachedAtlasEntry {
        CachedAtlasEntry {
            canonical_name: name.to_string(),
            embed_text: format!("{name} description text"),
            embedding: (0..dim).map(|i| seed + i as f32 * 0.1).collect(),
        }
    }

    #[test]
    fn roundtrip_two_entries() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let entries = vec![entry("Alice", 4, 1.0), entry("Bob", 4, 2.0)];
        write_atlas_embeddings(dir, "test-model", 4, "sha256:aaa", "depth=extracted", &entries)
            .unwrap();
        let read = read_atlas_embeddings(dir, "test-model", "sha256:aaa", "depth=extracted")
            .unwrap()
            .unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].canonical_name, "Alice");
        assert_eq!(read[0].embed_text, "Alice description text");
        assert_eq!(read[0].embedding, vec![1.0, 1.1, 1.2, 1.3]);
        assert_eq!(read[1].embedding, vec![2.0, 2.1, 2.2, 2.3]);
    }

    #[test]
    fn invalidates_on_model_mismatch() {
        let tmp = TempDir::new().unwrap();
        write_atlas_embeddings(tmp.path(), "model-A", 2, "h", "f", &[entry("X", 2, 0.0)]).unwrap();
        let read = read_atlas_embeddings(tmp.path(), "model-B", "h", "f").unwrap();
        assert!(read.is_none(), "different model must invalidate cache");
    }

    #[test]
    fn invalidates_on_atoms_hash_mismatch() {
        let tmp = TempDir::new().unwrap();
        write_atlas_embeddings(tmp.path(), "m", 2, "h1", "f", &[entry("X", 2, 0.0)]).unwrap();
        let read = read_atlas_embeddings(tmp.path(), "m", "h2", "f").unwrap();
        assert!(read.is_none(), "different atoms hash must invalidate cache");
    }

    #[test]
    fn invalidates_on_filter_signature_mismatch() {
        let tmp = TempDir::new().unwrap();
        write_atlas_embeddings(tmp.path(), "m", 2, "h", "f1", &[entry("X", 2, 0.0)]).unwrap();
        let read = read_atlas_embeddings(tmp.path(), "m", "h", "f2").unwrap();
        assert!(read.is_none(), "different filter must invalidate cache");
    }

    #[test]
    fn missing_file_is_a_soft_miss() {
        let tmp = TempDir::new().unwrap();
        let read = read_atlas_embeddings(tmp.path(), "m", "h", "f").unwrap();
        assert!(read.is_none());
    }

    #[test]
    fn truncated_data_is_hard_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("atoms.embeddings.bin");
        // Just the magic, no header_len → corrupt.
        fs::write(&path, MAGIC).unwrap();
        let res = read_atlas_embeddings(tmp.path(), "m", "h", "f").unwrap();
        assert!(res.is_none(), "too-short file must be a soft miss");
    }
}
