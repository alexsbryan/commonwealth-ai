// SPDX-License-Identifier: AGPL-3.0-or-later
//! Raw reads over the atlas directory's small files.
//!
//! FIVE_PROGRAMS §12 decision 1: the reader leaf holds the RAW reads. These
//! are the single-byte-format readers every consumer side needs — the atoms
//! content hash (the summary's first cache key) and `ontology.json` (the
//! declared navigation map). The writers of these files stay in corpus-engine,
//! which re-imports the readers from here (ARCH §10.6 — a re-export, never a
//! twin); `AtlasOntologyFile` itself is vocabulary and lives in
//! `understanding_vocab::ontology`.

use std::fs;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};
use tracing::warn;

/// SHA-256 of `atoms.json`, hex, prefixed `sha256:` — the summary's first
/// cache key (the summary is derived FROM the atoms file).
pub fn atoms_content_hash(atlas_dir: &Path) -> std::io::Result<String> {
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

/// Read `atlas/ontology.json`, or `None` when the atlas declares none or the
/// file cannot be parsed. Companion to corpus-engine's `write_atlas_ontology`;
/// the summary reads it through this and nothing else opens the file by name.
pub fn read_atlas_ontology(
    atlas_dir: &Path,
) -> Option<understanding_vocab::ontology::AtlasOntologyFile> {
    let raw =
        fs::read(atlas_dir.join(understanding_vocab::ontology::AtlasOntologyFile::FILE)).ok()?;
    match serde_json::from_slice(&raw) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            warn!(
                atlas_dir = %atlas_dir.display(),
                error = %e,
                "atlas ontology: ontology.json present but unreadable; treating as undeclared"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn content_hash_changes_with_atoms_json() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("atoms.json"), b"[]").unwrap();
        let h1 = atoms_content_hash(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("atoms.json"), b"[{}]").unwrap();
        let h2 = atoms_content_hash(tmp.path()).unwrap();
        assert_ne!(h1, h2);
    }
}
