// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content-addressed artifact cache — the "Bazel for model workflows" layer.
//!
//! A step's output is keyed by its resolved inputs (which step, its id, the
//! *resolved* args, and the item's content fingerprint); a re-run with an
//! unchanged key skips the step and reuses the cached `Artifact`. Read-effect
//! steps cache; Write-effect steps never do (the runner enforces this). The
//! cache is persistent, so a re-run = free resume.

use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::model::{Artifact, ResolvedArgs};

/// A cache of step outputs keyed by content hash.
pub trait ArtifactCache: Send + Sync {
    fn get(&self, key: &str) -> Option<Artifact>;
    fn put(&self, key: &str, artifact: &Artifact);
}

/// A step's cache key: a stable hash of everything that determines its output —
/// the `uses` (which step), its id, the *resolved* args (which transitively
/// include upstream outputs via templating), and the item's content fingerprint
/// (so editing a source file invalidates it).
pub fn cache_key(uses: &str, step_id: &str, args: &ResolvedArgs, item_fingerprint: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"sovereign-workflow/v1\0");
    h.update(uses.as_bytes());
    h.update(b"\0");
    h.update(step_id.as_bytes());
    h.update(b"\0");
    h.update(serde_json::to_vec(args).unwrap_or_default());
    h.update(b"\0");
    h.update(item_fingerprint.as_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Never caches — every `get` misses, every `put` is a no-op. The default for
/// `Runner::new` and for `--no-cache`.
pub struct NoCache;

impl ArtifactCache for NoCache {
    fn get(&self, _key: &str) -> Option<Artifact> {
        None
    }
    fn put(&self, _key: &str, _artifact: &Artifact) {}
}

/// A flat content-addressed file cache: `<dir>/<key>.json` per artifact. No
/// eviction yet (a future GC by age/size); a missing or corrupt entry is a
/// clean miss, never an error, and a write failure never fails the run.
pub struct FileArtifactCache {
    dir: PathBuf,
}

impl FileArtifactCache {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }
    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }
}

impl ArtifactCache for FileArtifactCache {
    fn get(&self, key: &str) -> Option<Artifact> {
        let bytes = std::fs::read(self.path(key)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
    fn put(&self, key: &str, artifact: &Artifact) {
        if let Ok(bytes) = serde_json::to_vec(artifact) {
            let _ = std::fs::write(self.path(key), bytes);
        }
    }
}
