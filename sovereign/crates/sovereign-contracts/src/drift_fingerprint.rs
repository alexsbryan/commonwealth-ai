// SPDX-License-Identifier: AGPL-3.0-or-later
//! The drift fingerprint codec — the SHA-256 sidecar `sovereign drift detect`
//! writes and `drift_posture` / `capability_reconcile` / `spec_reconcile`
//! read.
//!
//! Moved down from `sovereign-code` (2026-09-21) so the reconcile commands in
//! `sovereign-cli-llm` can stamp the SAME fingerprint without linking the code
//! intelligence crate. `sovereign_code::drift_posture` re-exports every item
//! here at its historical path, so existing importers are unchanged.
//!
//! Why hashes, not mtimes: mtime flips on `touch`, `git checkout`, even
//! `cp -p` in some filesystems. The drift report is expensive (~25-30 min per
//! narrative); a no-op mtime change must not invalidate it. SHA-256 of the
//! file contents is the honest signal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sovereign_time::unix_now_u64;

/// File name of the fingerprint sidecar. Lives alongside `latest.md` /
/// `latest.md.json` so all drift state co-locates.
pub const FINGERPRINT_FILE: &str = ".fingerprint";

/// On-disk shape of the fingerprint sidecar. Schema-versioned so future
/// readers can detect format drift cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftFingerprint {
    /// Fingerprint format version; bumped when the shape changes.
    pub schema_version: u32,
    /// When the fingerprint was written, seconds since the Unix epoch.
    pub generated_at_unix: u64,
    /// Map from absolute narrative path → SHA-256 hex.
    pub narrative_hashes: std::collections::BTreeMap<String, String>,
    /// Where the markdown report landed.
    pub output_path: String,
}

impl DriftFingerprint {
    /// The schema version this codec writes.
    pub const SCHEMA_VERSION: u32 = 1;
}

/// Write the fingerprint sidecar. Called by `sovereign drift detect` on
/// successful render. Returns the path written.
pub fn write_fingerprint(
    drift_dir: &Path,
    narrative_paths: &[PathBuf],
    output_path: &Path,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(drift_dir)?;
    let mut hashes = std::collections::BTreeMap::new();
    for path in narrative_paths {
        let h = hash_file(path)?;
        hashes.insert(path.to_string_lossy().into_owned(), h);
    }
    let generated_at_unix = unix_now_u64();
    let fp = DriftFingerprint {
        schema_version: DriftFingerprint::SCHEMA_VERSION,
        generated_at_unix,
        narrative_hashes: hashes,
        output_path: output_path.to_string_lossy().into_owned(),
    };
    let out = drift_dir.join(FINGERPRINT_FILE);
    let body = serde_json::to_string_pretty(&fp).map_err(std::io::Error::other)?;
    std::fs::write(&out, body)?;
    Ok(out)
}

/// SHA-256 of a file's bytes, hex-encoded.
///
/// Public because the drift orchestrator needs the SAME hash to decide whether
/// a cached narrative atlas was built from the document it is about to be
/// reported against. Two implementations of one key is the §10.6 smell, and
/// here it would be worse than untidy: the fingerprint written at the end of a
/// run and the staleness check made at the start must agree on what "this
/// document changed" means, or the report can assert it analysed a document it
/// skipped.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("{:x}", h.finalize()))
}
