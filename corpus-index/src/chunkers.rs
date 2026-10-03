// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `{id, content_hash}` row the index reads back.
//!
//! `CommittedChunk` is the row `CorpusIndex` records for every chunk, so it is
//! DEFINED here and `corpus-engine`'s `chunkers` module re-exports it (DE "The
//! read-port leaf, measured again": "`index/read` names the chunker's
//! `CommittedChunk`").

/// One chunk previously committed to the index, with the
/// content-hash that the index recorded for it. Used by
/// `chunk_delta` to compute what's changed between an old
/// version of a document and its new content.
#[derive(Debug, Clone)]
pub struct CommittedChunk {
    pub id: u64,
    pub content_hash: String,
}

/// The text a piece of a titled document is embedded as: the title on its own
/// line, then the text, unless the text already leads with the title.
///
/// ONE rule for every embedded text of a document. Ingest's `chunk_doc` heads
/// each leaf chunk with it, and RAPTOR heads each summary of that document
/// with it, so a summary names its work the way its leaves do. Summaries
/// embedded bare ranked 27th-152nd of 259 nodes on the pilot's whole-work
/// questions; under this header, 4th-21st (2026-10-02).
pub fn title_headed(title: Option<&str>, text: &str) -> String {
    match title {
        Some(t) if !text.starts_with(t) => format!("{t}\n\n{text}"),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::title_headed;

    #[test]
    fn title_headed_prepends_once_and_only_with_a_title() {
        assert_eq!(title_headed(Some("t"), "body"), "t\n\nbody");
        assert_eq!(title_headed(Some("t"), "t\n\nbody"), "t\n\nbody");
        assert_eq!(title_headed(None, "body"), "body");
    }
}
