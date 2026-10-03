// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lightweight validator for downloaded GGUF files.
//!
//! Used by both the CLI (`sovereign setup`) and the desktop
//! download-model flow to decide whether a just-written file on
//! disk is actually a model, or (much more commonly in practice)
//! an HTML error page, an LFS pointer, a JSON error body, or a
//! silently-truncated download.
//!
//! The three-way check is:
//!   1. File must exist and be non-empty.
//!   2. File size must meet an optional floor derived from
//!      `models.toml::size_gb`.
//!   3. First four bytes must be `GGUF` (ASCII `0x47 0x47 0x55 0x46`).
//!
//! The magic-byte check is load-bearing. Every non-GGUF response
//! we've seen from HuggingFace in the wild — HTML (`<!DOCTYPE`,
//! `<html`), LFS pointers (`version `), JSON errors (`{"`) —
//! starts with bytes that can't collide with `GGUF`, so a single
//! 4-byte read decisively separates real models from garbage
//! without needing content-type heuristics or deep GGUF parsing.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Why a downloaded file failed validation. Carries enough context
/// (path, actual/expected sizes, first four bytes) that the
/// downstream error message can be actionable without the operator
/// having to `file(1)` or `head(1)` the path themselves.
#[derive(Debug, thiserror::Error)]
pub enum GgufValidationError {
    #[error("file is empty: {0}")]
    Empty(PathBuf),
    #[error(
        "downloaded file is suspiciously small — {actual} bytes at {path} \
         (expected at least {min} bytes from the manifest). Likely an \
         HTML error page, LFS pointer, or a truncated download."
    )]
    TooSmall {
        path: PathBuf,
        actual: u64,
        min: u64,
    },
    #[error(
        "not a GGUF file — first 4 bytes {found:?} at {path} \
         (expected b\"GGUF\" = [71, 71, 85, 70]). Likely an HTML error \
         page, LFS pointer, or JSON error body from the download URL."
    )]
    BadMagic { path: PathBuf, found: [u8; 4] },
    #[error("missing: {0}")]
    Missing(PathBuf),
    #[error("stat/open failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What we expect a given file to look like. `min_size_bytes` is
/// derived from `models.toml::size_gb * 0.5 * 1 GiB` in the
/// typical call site — a 50% floor catches wildly-truncated
/// downloads and HTML stubs without requiring an exact size match
/// (quantization + HF's actual file sizes drift slightly over
/// time).
pub struct GgufExpectation {
    pub min_size_bytes: Option<u64>,
}

impl GgufExpectation {
    /// Sentinel fallback used when no manifest entry is available:
    /// any real GGUF is at least tens of megabytes; 1 MB rejects
    /// every HTML/JSON/LFS response we've observed.
    pub const DEFAULT_MIN_BYTES: u64 = 1_000_000;

    pub fn from_size_gb(size_gb: f64) -> Self {
        // 50% tolerance: Qwen GGUFs on HF vary ±10% from the
        // round numbers in our manifest, so 0.5× is a generous
        // floor that still catches byte-level stubs.
        let floor_bytes = (size_gb * 0.5 * 1024.0 * 1024.0 * 1024.0) as u64;
        Self {
            min_size_bytes: Some(floor_bytes.max(Self::DEFAULT_MIN_BYTES)),
        }
    }

    pub fn unknown() -> Self {
        Self {
            min_size_bytes: None,
        }
    }
}

/// GGUF magic prefix — four ASCII bytes `GGUF`.
pub const GGUF_MAGIC: [u8; 4] = *b"GGUF";

pub fn validate_gguf(path: &Path, expected: &GgufExpectation) -> Result<(), GgufValidationError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(GgufValidationError::Missing(path.to_path_buf()));
        }
        Err(e) => {
            return Err(GgufValidationError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    let len = meta.len();
    if len == 0 {
        return Err(GgufValidationError::Empty(path.to_path_buf()));
    }
    let min = expected
        .min_size_bytes
        .unwrap_or(GgufExpectation::DEFAULT_MIN_BYTES);
    if len < min {
        return Err(GgufValidationError::TooSmall {
            path: path.to_path_buf(),
            actual: len,
            min,
        });
    }

    // Read the first four bytes. Intentionally not `fd.bytes()`
    // or any async read — this is a sync-cheap check we want to
    // run from any context. If the file is ≥4 bytes (already
    // ensured above), `read_exact` on a 4-byte buf cannot
    // return less.
    let mut file = std::fs::File::open(path).map_err(|e| GgufValidationError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut head = [0u8; 4];
    file.read_exact(&mut head)
        .map_err(|e| GgufValidationError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    if head != GGUF_MAGIC {
        return Err(GgufValidationError::BadMagic {
            path: path.to_path_buf(),
            found: head,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, bytes: &[u8]) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn accepts_file_with_gguf_magic_and_plausible_size() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("model.gguf");
        // Fake 2 MB GGUF: magic + pad.
        let mut data = Vec::with_capacity(2 * 1024 * 1024);
        data.extend_from_slice(&GGUF_MAGIC);
        data.resize(2 * 1024 * 1024, 0u8);
        write(&p, &data);

        validate_gguf(&p, &GgufExpectation::unknown()).unwrap();
    }

    #[test]
    fn rejects_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("empty.gguf");
        write(&p, &[]);
        match validate_gguf(&p, &GgufExpectation::unknown()) {
            Err(GgufValidationError::Empty(_)) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("nope.gguf");
        match validate_gguf(&p, &GgufExpectation::unknown()) {
            Err(GgufValidationError::Missing(_)) => {}
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn rejects_file_under_min_size() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("stub.gguf");
        // 200 KB file with valid magic — would pass magic check
        // alone, must be rejected by size floor.
        let mut data = Vec::with_capacity(200_000);
        data.extend_from_slice(&GGUF_MAGIC);
        data.resize(200_000, 0u8);
        write(&p, &data);

        let expected = GgufExpectation::from_size_gb(4.3);
        match validate_gguf(&p, &expected) {
            Err(GgufValidationError::TooSmall { actual, min, .. }) => {
                assert_eq!(actual, 200_000);
                // min should be at least 0.5 * 4.3 GB = ~2.3 GB.
                assert!(min > 2_000_000_000, "min={min}");
            }
            other => panic!("expected TooSmall, got {other:?}"),
        }
    }

    #[test]
    fn rejects_html_response_body() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("page.gguf");
        // Pad to above the default 1 MB floor so the magic check
        // is what catches it — this is the real-world failure
        // mode (CDN returns a fat HTML error page).
        let mut data = Vec::new();
        data.extend_from_slice(b"<!DOCTYPE html><html><body>rate limited</body></html>");
        data.resize(2_000_000, b'.');
        write(&p, &data);

        match validate_gguf(&p, &GgufExpectation::unknown()) {
            Err(GgufValidationError::BadMagic { found, .. }) => {
                assert_eq!(&found, b"<!DO");
            }
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn rejects_lfs_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("lfs.gguf");
        // Real LFS pointers start with `version https://...`.
        let mut data = Vec::new();
        data.extend_from_slice(b"version https://git-lfs.github.com/spec/v1\n");
        data.resize(1_500_000, b' '); // pad above 1 MB floor
        write(&p, &data);

        match validate_gguf(&p, &GgufExpectation::unknown()) {
            Err(GgufValidationError::BadMagic { found, .. }) => {
                assert_eq!(&found, b"vers");
            }
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn rejects_json_error_body() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("err.gguf");
        let mut data = Vec::new();
        data.extend_from_slice(br#"{"error":"unauthorized"}"#);
        data.resize(1_500_000, b' ');
        write(&p, &data);

        match validate_gguf(&p, &GgufExpectation::unknown()) {
            Err(GgufValidationError::BadMagic { found, .. }) => {
                assert_eq!(&found, b"{\"er");
            }
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn from_size_gb_enforces_default_minimum_for_tiny_models() {
        // A 50 MB model's 50% floor would be 25 MB — but the
        // default 1 MB floor still applies via the `.max()` call.
        let exp = GgufExpectation::from_size_gb(0.05);
        let min = exp.min_size_bytes.unwrap();
        assert!(min >= GgufExpectation::DEFAULT_MIN_BYTES);
    }
}
