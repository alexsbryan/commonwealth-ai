// SPDX-License-Identifier: AGPL-3.0-or-later
//! Evidence extension (v0.5 §2): stored texts, the record of where each came
//! from, spans into them, and the alignment of a quotation against them.
//!
//! **Units.** Every range is a half-open `[start, end)` count of Unicode code
//! points into a stored text (or, for [`Difference::quote`], into the
//! request's quote). Every reply that carries a range also carries the exact
//! string it names, so a client in any language checks a range by comparing
//! strings, never by knowing how the host stores text.
//!
//! **Names.** A text is named by the sha256 of its UTF-8 bytes, 64 lowercase
//! hex characters. Ingest-side identities (chunk ids, row ids) never appear
//! here.

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::manifest::KnowledgeManifest;

/// The named refusals of the evidence routes (v0.5 §2.5). Each travels as
/// `{"error": "<reason>"}`, the ingest routes' body shape, and as
/// [`Unavailable::reason`]. A client matches these strings exactly and
/// treats any other reason as opaque.
pub mod reasons {
    /// No corpus the caller may read holds a text of that name.
    pub const TEXT_NOT_HELD: &str = "text not held";
    /// The corpus keeps no texts: built before texts were stored, or its
    /// recipe opted out.
    pub const TEXTS_NOT_STORED: &str = "texts not stored";
    /// This one document's text was not stored, though the corpus keeps
    /// texts.
    pub const TEXT_NOT_STORED: &str = "text not stored";
    /// `start`/`end` fall outside the text, or `start > end`.
    pub const RANGE_OUTSIDE_TEXT: &str = "range outside text";
    /// A requested corpus the caller may not read, or that the host does not
    /// hold. One answer for both, so a reply never tells a caller which
    /// corpora exist beyond its reach: the corpus-level twin of
    /// [`TEXT_NOT_HELD`].
    pub const CORPUS_NOT_HELD: &str = "corpus not held";
    /// A hit's text is stored but its record could not be read or served:
    /// a damaged store, named on the hit (`KnowledgeResult::document_absent`),
    /// never an absent field.
    pub const DOCUMENT_UNREADABLE: &str = "document unreadable";

    /// Every reason a hit may give for carrying no `document` (§3).
    pub const HIT_ABSENCES: [&str; 3] = [TEXTS_NOT_STORED, TEXT_NOT_STORED, DOCUMENT_UNREADABLE];
}

/// Context, in code points, a host puts on each side of a span or slice when
/// the request names none.
pub const DEFAULT_CONTEXT: u32 = 32;

/// `knowledge.evidence` in the manifest (v0.5 §2.1): where the evidence
/// routes are mounted. Paths are relative to the manifest's origin, like
/// `search_endpoint`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceEndpoints {
    /// `GET {text_endpoint}/{text_sha256}`, returning a [`TextSlice`].
    /// Advertising it requires the `evidence:text` feature.
    pub text_endpoint: String,
    /// `POST {align_endpoint}`, taking an [`AlignRequest`]. Present iff the
    /// host advertises `evidence:align`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align_endpoint: Option<String>,
}

/// One stored text, and where it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// sha256 of the text's UTF-8 bytes, 64 lowercase hex: the text's name.
    pub text_sha256: String,
    /// The extractor that produced the text, with its version
    /// (`<tag>@<version>` on the reference host). Opaque to clients.
    pub extractor: String,
    /// The source the extractor read.
    pub source: SourceRef,
    /// The document's metadata as the extractor or the recipe declared it,
    /// verbatim. `None` when neither declared any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// The source of a stored text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// The host's id for the source document: an attribute, never the
    /// text's name.
    pub id: String,
    /// sha256 of the bytes the extractor read. `null` on the wire exactly
    /// when the document is a record inside a file of many (a JSONL line, a
    /// CSV row, a dump entry), whose own bytes were never a file. Serialized
    /// even when `null`, so the absence is stated rather than omitted.
    pub sha256: Option<String>,
}

/// A range of a stored text, as found by a search or an alignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// The corpus it was found in: a location, never the text's name. The
    /// same text may be held by several corpora.
    pub corpus_id: String,
    /// The text the range points into.
    pub document: Document,
    /// First code point of the range.
    pub start: u64,
    /// One past the last code point of the range.
    pub end: u64,
    /// The text's `[start, end)`, exactly.
    pub exact: String,
    /// Up to the requested context of code points before `start`.
    pub prefix: String,
    /// Up to the requested context of code points after `end`.
    pub suffix: String,
}

/// `GET {text_endpoint}/{text_sha256}` response: the whole text, or a range
/// of it with context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextSlice {
    /// The text this slice is cut from.
    pub document: Document,
    /// First code point of `text`; `0` when no range was asked.
    pub start: u64,
    /// One past the last code point of `text`; the text's length when no
    /// range was asked.
    pub end: u64,
    /// The text's `[start, end)`, exactly.
    pub text: String,
    /// Up to `context` code points before `start`.
    pub before: String,
    /// Up to `context` code points after `end`.
    pub after: String,
}

/// `POST {align_endpoint}` request: find where a quotation stands in the
/// stored texts, and how it differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignRequest {
    /// The quotation, as the writer quoted it. An ellipsis (`...` or `…`)
    /// marks an elision; square brackets mark editorial text.
    pub quote: String,
    /// Corpora to align against. Empty means every corpus the caller may
    /// read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora: Vec<String>,
    /// Most alignments to return; [`AlignRequest::DEFAULT_LIMIT`] when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Code points of `prefix`/`suffix` on each span; [`DEFAULT_CONTEXT`]
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
}

impl AlignRequest {
    /// Alignments returned when `limit` is absent.
    pub const DEFAULT_LIMIT: u32 = 5;

    /// `limit`, or its default.
    pub fn effective_limit(&self) -> u32 {
        self.limit.unwrap_or(Self::DEFAULT_LIMIT)
    }

    /// `context`, or its default.
    pub fn effective_context(&self) -> u32 {
        self.context.unwrap_or(DEFAULT_CONTEXT)
    }
}

/// `POST {align_endpoint}` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlignResponse {
    /// Best first. Empty when nothing reached the host's coverage floor.
    pub alignments: Vec<Alignment>,
    /// The aligner's identity (`align/1 norm/0` on the reference host). It
    /// changes whenever the same input could align differently, so a client
    /// keys cached alignments on it.
    pub aligner: String,
    /// Each corpus that was aligned against, with the digest of its texts.
    pub corpora: Vec<CorpusTexts>,
    /// Each requested corpus that could not be aligned against, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora_unavailable: Vec<Unavailable>,
}

/// A corpus an alignment read, and the digest of what it read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusTexts {
    /// The corpus.
    pub corpus_id: String,
    /// sha256, 64 lowercase hex, over the corpus's sorted
    /// `"<text_sha256> <source_sha256 or -> <extractor>"` lines (v0.5
    /// §2.4). Opaque to clients: it changes exactly when a text, a source
    /// or an extractor changes, and never when chunking or embedding does.
    pub texts_digest: String,
}

/// A corpus that could not be read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unavailable {
    /// The corpus.
    pub corpus_id: String,
    /// Why, by name: one of [`reasons`] where one applies.
    pub reason: String,
}

/// One place a quotation stands, and how it differs from the source there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alignment {
    /// The stretch of source text the quotation aligns with.
    pub span: Span,
    /// Every difference between the quotation and `span`, in quote order.
    /// Empty when the quotation is verbatim under the host's matching
    /// normalisation.
    pub differences: Vec<Difference>,
    /// Aligned quote tokens over quote tokens, in `[0, 1]`.
    pub coverage: f32,
}

/// One difference between a quotation and its source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Difference {
    /// What kind of difference.
    pub kind: DifferenceKind,
    /// `[start, end)` in code points into the request's quote. Empty
    /// (`start == end`) for `omitted`, where it marks the point.
    pub quote: [u64; 2],
    /// `[start, end)` in code points into the text. Empty for `added`,
    /// where it marks the point.
    pub source: [u64; 2],
}

/// The closed set of differences an alignment reports (v0.5 §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferenceKind {
    /// The quote has other words where the source has these.
    Substituted,
    /// The quote has words the source does not.
    Added,
    /// The source has words the quote dropped, with no ellipsis to say so.
    Omitted,
    /// An ellipsis in the quote stands for this stretch of source.
    Elided,
    /// Square-bracketed editorial text in the quote stands for this stretch
    /// of source (possibly empty, as for `[sic]`).
    Bracketed,
}

impl DifferenceKind {
    /// Every kind, in declaration order.
    pub const ALL: [DifferenceKind; 5] = [
        DifferenceKind::Substituted,
        DifferenceKind::Added,
        DifferenceKind::Omitted,
        DifferenceKind::Elided,
        DifferenceKind::Bracketed,
    ];

    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            DifferenceKind::Substituted => "substituted",
            DifferenceKind::Added => "added",
            DifferenceKind::Omitted => "omitted",
            DifferenceKind::Elided => "elided",
            DifferenceKind::Bracketed => "bracketed",
        }
    }
}

/// The bytes [`CorpusTexts::texts_digest`] is the sha256 of (v0.5 §2.4):
/// one line per stored text of the corpus,
/// `"<text_sha256> <source_sha256, or - for a record> <extractor>\n"`, the
/// distinct lines sorted bytewise and concatenated. Each item is
/// `(text_sha256, source sha256, extractor)`. The one derivation: a host
/// computing the digest and a client re-checking one both go through this,
/// and hash it with sha256 themselves.
pub fn texts_digest_preimage<'a>(
    texts: impl IntoIterator<Item = (&'a str, Option<&'a str>, &'a str)>,
) -> String {
    let mut lines: Vec<String> = texts
        .into_iter()
        .map(|(text, source, extractor)| format!("{text} {} {extractor}\n", source.unwrap_or("-")))
        .collect();
    lines.sort();
    lines.dedup();
    lines.concat()
}

/// Whether `s` is a well-formed text or source name: 64 lowercase hex.
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
