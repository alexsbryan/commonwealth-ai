// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-section LLM-extraction cache — Move 6 Phase 4 primitive.
//!
//! Caches Phase 1 LLM extraction output (a `SectionExtraction` JSON
//! blob) per section, keyed by
//! `sha256(section_text + prompt_version + model_id)`. Hits skip the
//! LLM round-trip entirely; misses persist the new output for next
//! time.
//!
//! Pipeline integration (literary_atlas, philosophy_atlas, etc.) is
//! a separate concern. This module ships the primitive + the on-disk
//! layout. Pipelines compose it via `cache.lookup(section_id, key) →
//! Option<Vec<u8>>` before issuing an LLM call.
//!
//! ## On-disk layout
//!
//! ```text
//! <atlas_dir>/section_cache/<key>.json
//! ```
//!
//! Where `<key>` is the 16-hex-char blake3 prefix of the cache-key
//! triple. Atomic writes via tmp+rename.

use std::io;
use std::path::{Path, PathBuf};

pub const SECTION_CACHE_DIR: &str = "section_cache";

/// Compute the deterministic cache key for a (text, prompt_version,
/// model_id) triple. 16-hex-char prefix of blake3.
pub fn cache_key(text: &str, prompt_version: &str, model_id: &str) -> String {
    let input = format!("section|{text}|{prompt_version}|{model_id}");
    let full = blake3::hash(input.as_bytes()).to_hex().to_string();
    full[..16].to_string()
}

fn cache_path(atlas_dir: &Path, key: &str) -> PathBuf {
    atlas_dir
        .join(SECTION_CACHE_DIR)
        .join(format!("{key}.json"))
}

/// Read cached extraction output. Returns `Ok(None)` on miss (file
/// absent); `Err` only on I/O errors other than NotFound.
pub fn lookup(atlas_dir: &Path, key: &str) -> io::Result<Option<Vec<u8>>> {
    let path = cache_path(atlas_dir, key);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Persist extraction output for `key`. Atomic via tmp+rename.
/// Creates the cache dir on first write.
pub fn store(atlas_dir: &Path, key: &str, bytes: &[u8]) -> io::Result<()> {
    let dir = atlas_dir.join(SECTION_CACHE_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{key}.json"));
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Total bytes consumed by the section cache. Useful for the
/// `sovereign atlas stats` surface to flag oversized caches.
pub fn disk_usage(atlas_dir: &Path) -> io::Result<u64> {
    let dir = atlas_dir.join(SECTION_CACHE_DIR);
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            }
        }
    }
    Ok(total)
}

/// Drop all cached entries. Used by operator-driven cache flush
/// (e.g. after a prompt-template rewrite changes the semantic
/// contract without bumping `prompt_version`).
pub fn clear(atlas_dir: &Path) -> io::Result<usize> {
    let dir = atlas_dir.join(SECTION_CACHE_DIR);
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().is_file() {
            std::fs::remove_file(entry.path())?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_stable_across_calls() {
        let k1 = cache_key("section text", "v1", "qwen3-4b");
        let k2 = cache_key("section text", "v1", "qwen3-4b");
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 16);
    }

    #[test]
    fn cache_key_changes_with_prompt_version() {
        let k1 = cache_key("text", "v1", "qwen3-4b");
        let k2 = cache_key("text", "v2", "qwen3-4b");
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_changes_with_model() {
        let k1 = cache_key("text", "v1", "qwen3-4b");
        let k2 = cache_key("text", "v1", "qwen3-9b");
        assert_ne!(k1, k2);
    }

    #[test]
    fn lookup_miss_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = lookup(tmp.path(), "missing-key").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn store_then_lookup_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = br#"{"atoms":[]}"#;
        let key = cache_key("section text", "v1", "qwen3-4b");
        store(tmp.path(), &key, payload).unwrap();
        let read_back = lookup(tmp.path(), &key).unwrap().unwrap();
        assert_eq!(read_back, payload);
    }

    #[test]
    fn store_creates_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("atlas/dir");
        store(&nested, "k1", b"x").unwrap();
        assert!(nested.join("section_cache/k1.json").exists());
    }

    #[test]
    fn disk_usage_sums_cache_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(disk_usage(tmp.path()).unwrap(), 0);
        store(tmp.path(), "k1", b"hello").unwrap();
        store(tmp.path(), "k2", b"world!").unwrap();
        assert_eq!(disk_usage(tmp.path()).unwrap(), 11);
    }

    #[test]
    fn clear_drops_all_entries() {
        let tmp = tempfile::tempdir().unwrap();
        store(tmp.path(), "k1", b"a").unwrap();
        store(tmp.path(), "k2", b"b").unwrap();
        let removed = clear(tmp.path()).unwrap();
        assert_eq!(removed, 2);
        assert!(lookup(tmp.path(), "k1").unwrap().is_none());
    }

    #[test]
    fn clear_on_absent_dir_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(clear(tmp.path()).unwrap(), 0);
    }
}
