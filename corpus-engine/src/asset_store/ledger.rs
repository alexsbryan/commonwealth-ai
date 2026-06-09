// SPDX-License-Identifier: AGPL-3.0-or-later
//! Append-only ledger of asset observations.
//!
//! One JSONL line per *unique* sha256 (idempotent on re-observation).
//! Append-only because the on-disk shape is naturally additive — a
//! re-observation by a second document does not mutate the first
//! observation's record. Re-observations are tracked via the standard
//! `Attaches` edges in the atom graph; the ledger captures only the
//! provenance of the *first* write.
//!
//! The ledger is the substrate's audit trail. `sovereign asset ls`
//! reads it; Phase 5 reconciliation stats compute against it; the
//! drift detector cross-references it against the asset atoms in
//! `atoms.json`.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One line of `assets/ledger.jsonl`.
///
/// Fields default-construct on missing keys so a ledger written by a
/// future version can still be read by an older binary — additive
/// fields don't break older readers. The wire shape is documented in
/// `AssetStore`'s rustdoc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerEntry {
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    pub size: u64,
    pub first_seen_source_doc_id: String,
    /// Seconds since the Unix epoch when this asset was first written.
    /// Recorded as an integer so the ledger round-trips through any
    /// JSON reader without floating-point shenanigans.
    pub first_seen_ts: i64,
    /// Optional path (relative to the asset store root or absolute
    /// when written by callers without a root context) of the typed
    /// parsed form. `None` when no sub-extractor produced a parsed
    /// cache (prose-shaped assets — text PDFs, DOCX).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_form: Option<PathBuf>,
}

/// Append a single entry. Caller is responsible for de-dup (the
/// `FilesystemAssetStore` does this — the ledger here is the lower
/// layer).
pub(crate) fn append(ledger_path: &Path, entry: &LedgerEntry) -> Result<()> {
    let line = serde_json::to_string(entry)
        .map_err(|e| Error::Extraction(format!("ledger serialise {}: {e}", entry.sha256)))?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_path)
        .map_err(Error::Io)?;
    f.write_all(line.as_bytes()).map_err(Error::Io)?;
    f.write_all(b"\n").map_err(Error::Io)?;
    Ok(())
}

/// Streaming reader over a ledger file. Skips malformed lines with a
/// `tracing::warn!` rather than aborting — the ledger is append-only
/// from many callers and a single torn write should not corrupt the
/// view.
pub struct LedgerReader {
    path: PathBuf,
}

impl LedgerReader {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Read every entry. Returns an empty vec if the file does not
    /// exist (a freshly-created corpus has no ledger yet).
    pub fn read_all(&self) -> Result<Vec<LedgerEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let f = std::fs::File::open(&self.path).map_err(Error::Io)?;
        let reader = BufReader::new(f);
        let mut out = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line.map_err(Error::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<LedgerEntry>(&line) {
                Ok(e) => out.push(e),
                Err(err) => {
                    tracing::warn!(
                        path = %self.path.display(),
                        line = lineno + 1,
                        "asset_ledger: skipping malformed line ({err})",
                    );
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let entry = LedgerEntry {
            sha256: "deadbeef".into(),
            original_filename: Some("a.txt".into()),
            mime: Some("text/plain".into()),
            size: 42,
            first_seen_source_doc_id: "doc-1".into(),
            first_seen_ts: 1_700_000_000,
            parsed_form: None,
        };
        append(&path, &entry).unwrap();
        let read = LedgerReader::new(&path).read_all().unwrap();
        assert_eq!(read, vec![entry]);
    }

    #[test]
    fn malformed_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        std::fs::write(&path, "{\"sha256\":\"ok\",\"size\":1,\"first_seen_source_doc_id\":\"d\",\"first_seen_ts\":1}\nnot json\n").unwrap();
        let read = LedgerReader::new(&path).read_all().unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].sha256, "ok");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let read = LedgerReader::new(dir.path().join("missing.jsonl"))
            .read_all()
            .unwrap();
        assert!(read.is_empty());
    }
}
