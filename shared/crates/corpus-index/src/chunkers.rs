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

/// `content` less the header [`title_headed`] puts on it: the one inverse, so
/// a reader locating a chunk in its stored text strips what ingest added. The
/// result is always a suffix of the text the chunk was cut from. It is that
/// whole text except when the text itself began with the title and a blank
/// line, which heading left as it was and this strips: the title line is then
/// missing from the result, never anything ingest did not add.
pub fn title_unheaded<'a>(title: Option<&str>, content: &'a str) -> &'a str {
    match title {
        Some(t) => content
            .strip_prefix(t)
            .and_then(|rest| rest.strip_prefix("\n\n"))
            .unwrap_or(content),
        None => content,
    }
}

#[cfg(test)]
mod tests {
    use super::{title_headed, title_unheaded};

    #[test]
    fn title_headed_prepends_once_and_only_with_a_title() {
        assert_eq!(title_headed(Some("t"), "body"), "t\n\nbody");
        assert_eq!(title_headed(Some("t"), "t\n\nbody"), "t\n\nbody");
        assert_eq!(title_headed(None, "body"), "body");
    }

    /// Unheading a headed text gives the text back, or (for a text that
    /// itself began with its title and a blank line) a suffix of it.
    #[test]
    fn title_unheaded_gives_back_a_suffix_of_the_headed_text() {
        for (title, text, back) in [
            (Some("t"), "body", "body"),
            (Some("t"), "tidal body", "tidal body"),
            (None, "body", "body"),
            (Some("Walden"), "Walden\n\nbody", "body"),
        ] {
            let headed = title_headed(title, text);
            let unheaded = title_unheaded(title, &headed);
            assert_eq!(unheaded, back, "{title:?} {text:?}");
            assert!(text.ends_with(unheaded));
        }
    }
}
