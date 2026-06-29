// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content hash of a corpus's `atoms.json` — the staleness key shared by the
//! meta-atlas builder and the atlas summary.
//!
//! (Formerly this module also held the `atoms.embeddings.bin` embedding cache.
//! ATLAS_STORAGE_V2 Phase B retired that cache: atom embeddings now live once,
//! per corpus, in `atoms_ann.lance` and the query-time bag is derived from it —
//! nothing re-embeds at load. Only the content hash remains here.)

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Compute the SHA-256 of the atoms.json file at `<atlas_dir>/atoms.json`.
/// The staleness key for the meta-atlas + atlas summary. Cheap (~30 ms on a
/// ~50 MB atoms.json).
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
        assert!(h1.starts_with("sha256:"));
        assert_ne!(h1, h2, "hash must change when atoms.json changes");
    }
}
