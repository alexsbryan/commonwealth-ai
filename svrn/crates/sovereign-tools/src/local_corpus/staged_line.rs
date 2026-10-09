// SPDX-License-Identifier: AGPL-3.0-or-later
//! The staged JSONL line, and what the staging states about the file it read
//! (ADDRESSED_TEXT §3, `DocSource::Hashed`). Beside `extract_stage`, which
//! writes the lines, so that file does not grow past its ceiling.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StagedLine<'a> {
    pub(super) id: &'a str,
    pub(super) title: &'a str,
    pub(super) content: &'a str,
    pub(super) source_path: &'a str,
    /// `corpus_index::index::STAGED_SOURCE_SHA256`, pinned by a test.
    pub(super) source_sha256: &'a str,
    /// `corpus_index::index::STAGED_EXTRACTOR`, pinned by a test.
    pub(super) extractor: &'a str,
}

/// What this staging states about a file it read itself: the sha256 of its
/// bytes and its own extractor, `local-stage:<ext>@<version>` (`ocr` when the
/// text came from OCR). The initial ingest writes it on the staged line and
/// the watched-folder delta returns it with each fetch as
/// `DocSource::Hashed`, so both paths store the same record.
pub fn stated_source(
    path: &Path,
    ocr: bool,
) -> std::io::Result<(kernel_types::Sha256Hash, String)> {
    let sha256 = kernel_types::Sha256Hash::of_reader(std::fs::File::open(path)?)?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("none");
    let kind = if ocr {
        "ocr".to_string()
    } else {
        ext.to_ascii_lowercase()
    };
    Ok((
        sha256,
        format!("local-stage:{kind}@{}", env!("CARGO_PKG_VERSION")),
    ))
}

#[cfg(test)]
mod tests {
    /// The staged line's stated keys are the ones the JSONL extractor reads.
    #[test]
    fn the_staged_line_states_the_keys_the_jsonl_extractor_reads() {
        let line = super::StagedLine {
            id: "a",
            title: "a",
            content: "c",
            source_path: "a",
            source_sha256: "s",
            extractor: "e",
        };
        let v = serde_json::to_value(&line).unwrap();
        assert_eq!(v[corpus_index::index::STAGED_SOURCE_SHA256], "s");
        assert_eq!(v[corpus_index::index::STAGED_EXTRACTOR], "e");
    }
}
