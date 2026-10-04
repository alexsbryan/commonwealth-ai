// SPDX-License-Identifier: AGPL-3.0-or-later
//! Self-reference detection — does this question ask ABOUT the document, and
//! the citation/output shapes an answer carries back.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

// ─── Self-reference detection ────────────────────────────────

/// Lowercase substrings that unambiguously mark a question as directed at
/// the attached document. Matching any of these short-circuits the
/// LLM-based router — we don't need a Fast-slot call to know that
/// "summarize this document" is about the document.
///
/// Kept as a flat list rather than regex for predictability and cost:
/// a substring scan over ~100 chars is microseconds; a regex is overkill.
pub(super) const SELF_REFERENCE_PHRASES: &[&str] = &[
    // "this <thing>" phrasings
    "this document",
    "this doc",
    "this pdf",
    "this file",
    "this text",
    "this paper",
    "this article",
    "this book",
    "this chapter",
    "this essay",
    "this report",
    // "the <thing>" phrasings — risk of false positive is low because
    // general-knowledge questions rarely mention "the document" etc.
    "the document",
    "the text",
    "the paper",
    "the article",
    "the book",
    "the chapter",
    "the essay",
    "the report",
    "the attached",
    // imperative summary/analysis phrasings
    "summarize this",
    "summarise this",
    "summary of this",
    "summarize the",
    "summarise the",
    // open-ended "what does/is this" patterns
    "what is this about",
    "what's this about",
    "what is this document",
    "what does this",
];

/// Return true when `request` explicitly references the attached document.
/// Case-insensitive substring match against [`SELF_REFERENCE_PHRASES`].
pub(super) fn detect_self_reference(request: &str) -> bool {
    let q = request.to_lowercase();
    SELF_REFERENCE_PHRASES.iter().any(|p| q.contains(p))
}

/// Common English function words + digit-only tokens are dropped when
/// extracting filename keywords — a question that mentions "the" or "2024"
/// is not meaningfully "about" the attached document.
pub(super) const FILENAME_STOPWORDS: &[&str] = &[
    "the", "and", "for", "but", "not", "you", "are", "with", "this", "that", "from", "into",
    "onto", "upon", "have", "had", "has", "was", "were", "been", "being", "its", "their", "them",
    "they", "our", "his", "her", "what", "which", "who", "whom", "when", "where", "why", "how",
    "too", "also", "just", "only", "pdf", "doc", "docx", "txt", "pages", "page", "chapter", "part",
    "vol", "volume", "edition", "copy", "draft", "final", "version", "revised",
];

/// ASCII-fold a string: strip diacritics so `"Schrödinger"` and
/// `"schrodinger"` compare equal. Lightweight char-by-char mapping that
/// covers the common Latin-1 Supplement range; sufficient for English
/// filenames with occasional accented loanwords.
pub(super) fn ascii_fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
            'ç' => 'c',
            'Ç' => 'C',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'È' | 'É' | 'Ê' | 'Ë' => 'E',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
            'ñ' => 'n',
            'Ñ' => 'N',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => 'O',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
            'ý' | 'ÿ' => 'y',
            'Ý' => 'Y',
            'ß' => 's',
            other => other,
        })
        .collect()
}

/// Extract content-word tokens from a document's title + filename.
///
/// Splits on any non-alphabetic character (so `11._Erwin_Schrodinger_-_What_is_Life__1944_.pdf`
/// yields `["erwin", "schrodinger", "what", "is", "life"]`), lowercases,
/// ASCII-folds diacritics, drops tokens shorter than 3 chars, drops
/// stopwords, de-duplicates.
pub(super) fn filename_tokens(asset: &DocumentAsset) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let sources = [asset.title.as_str(), asset.filename.as_str()];
    for s in sources {
        let folded = ascii_fold(&s.to_lowercase());
        for tok in folded.split(|c: char| !c.is_ascii_alphabetic()) {
            if tok.len() < 3 {
                continue;
            }
            if FILENAME_STOPWORDS.contains(&tok) {
                continue;
            }
            if seen.insert(tok.to_string()) {
                out.push(tok.to_string());
            }
        }
    }
    out
}

/// Represents one chunk that contributed to an answer, with enough
/// metadata for the frontend to render a rich citation popover.
///
/// The `label` is the string the model uses when citing this chunk —
/// `"§4"` for a synthesis section, `"passage 2"` for a RAG match, etc.
/// The frontend matches it against `[Source: <label>]` spans in the
/// prose and, on click, shows the `snippet` in a popover keyed by
/// `corpus_id` (which the Tauri handler fills with the document title).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CitedChunk {
    pub label: String,
    pub chunk_index: usize,
    pub content: String,
    pub snippet: String,
}

/// The full execution result handed back to `ask_document`. Bundles
/// everything needed to persist a rich assistant message — response
/// text, citation metadata, and the inference backend's own provenance
/// (model id + token count) which would otherwise be dropped on the
/// floor by `execute_*`.
#[derive(Debug, Clone)]
pub struct ExecutionOutput {
    pub text: String,
    pub citations: Vec<CitedChunk>,
    pub model_id: String,
    pub tokens_used: usize,
    pub latency_ms: u64,
    /// Why the synthesizing model stopped emitting. Plumbed from
    /// the underlying `CompletionResponse.finish_reason` so the
    /// desktop's DocumentAsk surface can light up the cutoff chip
    /// when ask_document's reply was length-truncated.
    pub finish_reason: Option<sovereign_core::types::FinishReason>,
    /// Completion-only token count from the synthesizing model.
    /// Mirrors `CompletionResponse.completion_tokens`.
    pub completion_tokens: Option<u32>,
}

impl ExecutionOutput {
    /// Sentinel used when a path decides there's nothing to do — e.g.
    /// `execute_rag` finds zero relevant chunks and signals the caller
    /// to fall through to the runtime pipeline.
    pub(super) fn empty() -> Self {
        Self {
            text: String::new(),
            citations: Vec::new(),
            model_id: String::new(),
            tokens_used: 0,
            latency_ms: 0,
            finish_reason: None,
            completion_tokens: None,
        }
    }
}

/// First `max` *bytes* of `content` — walked back to the nearest UTF-8
/// char boundary, then to a whitespace boundary when one is available
/// within the safe window. Matches the snippet format used elsewhere in
/// the codebase so citation popovers look consistent with
/// knowledge-query popovers.
///
/// The char-boundary walk is load-bearing for non-ASCII documents:
/// Conrad uses curly quotes (U+201C, 3 bytes), em-dashes (U+2014, 3
/// bytes), and ellipses (U+2026, 3 bytes); slicing at a raw byte index
/// in that text used to panic with "end byte index N is not a char
/// boundary" inside the Cargo runtime.
pub(super) fn short_snippet(content: &str, max: usize) -> String {
    if content.len() <= max {
        return content.to_string();
    }
    // Walk back to the nearest char boundary. is_char_boundary is
    // stable since 1.9; floor_char_boundary is nightly-only, so we
    // open-code the walk. At most 3 iterations (UTF-8 chars are ≤4
    // bytes), so the cost is negligible.
    let mut end = max;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = &content[..end];
    match truncated.rfind(char::is_whitespace) {
        Some(pos) if pos > 0 => format!("{}...", &truncated[..pos]),
        _ => format!("{truncated}..."),
    }
}

/// True when `request` mentions any of `tokens` as a whole word. The
/// question is ASCII-folded + lowercased before comparison so e.g. the
/// token `"schrodinger"` matches `"Schrödinger"` in the user's question.
pub(super) fn mentions_document(tokens: &[String], request: &str) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let q = ascii_fold(&request.to_lowercase());
    // Split query into alphabetic words; a token matches if it appears
    // as one of those words. Avoids `"life"` false-matching in `"wildlife"`.
    let words: std::collections::HashSet<&str> = q
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    tokens.iter().any(|t| words.contains(t.as_str()))
}
