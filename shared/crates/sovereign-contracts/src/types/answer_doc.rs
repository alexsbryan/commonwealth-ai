// SPDX-License-Identifier: AGPL-3.0-or-later
//! The answer document — one presentation of an answer + its source
//! ledger, shared by every surface that hands a turn's result to a
//! human as a *document* rather than as a live bubble.
//!
//! # Why this lives in `sovereign-contracts`
//!
//! It formats [`projection::Provenance`] and [`projection::Citation`]
//! and nothing else: no store, no runtime, no engine, no filesystem.
//! `sovereign-contracts` is the seam every surface already meets at
//! (`ARCH_LAYERS.toml` — the desktop, the CLI, the mesh client and the
//! mobile shell all depend on it), so putting the renderer here reaches
//! all of them for zero new edges. Putting it in `sovereign-core`
//! instead would make a terminal that wants to print an answer link the
//! runtime hub — the exact dependency this crate exists to avoid.
//!
//! # What did NOT move
//!
//! The `.docx` and `.pdf` encoders, and the write to the user's chosen
//! path. Both are host business: the encoders carry format-specific
//! byte layout the surface owns, and a file write to a user-picked
//! destination cannot cross a wire at all (sv-surface D7, "CANNOT
//! CROSS"). They consume [`Block`], which is why [`doc_blocks`] is
//! public.
//!
//! # Fed by the projection, not by the blob
//!
//! Until sv-surface D7/G9 the desktop built this document by reading
//! the persisted `metadata` JSON with pointers
//! (`/provenance/inference_backend`, `/provenance/sources`,
//! `retrieved_chunks`). That is what made it desktop-only: a client
//! attached over the wire never sees that blob. It is built from the
//! typed projection now, so the same document renders from a local
//! store or from a `TurnFrame::Complete`.
//!
//! Two consequences worth stating rather than discovering:
//!
//! - `ProvenanceSource::display_name` had to be ADDED to the projection
//!   in the same rung, or every watched folder would have rendered as
//!   its slug. See [`projection::ProvenanceSource::display_name`].
//! - [`projection::Citation`] is corpus-grounded only — a retrieved
//!   chunk with no `(corpus_id, chunk_id)` handle (a web-fetch result)
//!   is not a citation and does not enter the ledger. The blob reader
//!   listed it with an empty corpus handle. This is a deliberate,
//!   pinned difference, not a silent one:
//!   `projection::tests::web_only_chunks_are_not_citations`.

use super::projection::{Citation, Provenance};

/// One entry in the source ledger — a grounding passage, traceable to
/// the corpus it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceEntry {
    /// Document title, or `"(untitled passage)"` when the chunk has none.
    pub title: String,
    /// The corpus handle, rendered so a reader can find the passage again.
    pub corpus_id: String,
    /// The quoted grounding text, when non-empty.
    pub snippet: Option<String>,
    /// Source URL, when the chunk carries one.
    pub url: Option<String>,
}

/// Structured view of an answer + its provenance — the format-agnostic
/// intermediate every renderer walks, so none of them re-parses the
/// turn's result.
#[derive(Debug, Clone, PartialEq)]
pub struct AnswerDoc {
    /// Model + serving node that produced the answer, when known.
    pub answered_by: Option<String>,
    /// Human-facing names of the corpora that actually contributed
    /// (count > 0), folder display names preferred over slugs.
    pub corpora: Vec<String>,
    /// The answer text, trimmed.
    pub body: String,
    /// The source ledger.
    pub sources: Vec<SourceEntry>,
}

/// The fallback title for a passage whose chunk carries none.
const UNTITLED: &str = "(untitled passage)";

impl AnswerDoc {
    /// Build the document from a turn's typed result.
    ///
    /// `provenance` / `citations` are exactly what
    /// [`projection::project_message_metadata`] returns for a persisted
    /// message, and exactly what `TurnFrame::Complete` carries — which
    /// is the point: one document, either source.
    pub fn from_projection(
        content: &str,
        provenance: Option<&Provenance>,
        citations: &[Citation],
    ) -> Self {
        let answered_by = provenance
            .map(|p| p.inference_backend.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let corpora = provenance
            .map(|p| {
                p.sources
                    .iter()
                    // A corpus that contributed nothing is not a source
                    // the reader was "searched" against in any sense
                    // they would recognise.
                    .filter(|s| s.count > 0)
                    .map(|s| {
                        s.display_name
                            .as_deref()
                            .filter(|x| !x.is_empty())
                            .unwrap_or(s.origin.as_str())
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let sources = citations
            .iter()
            .map(|c| SourceEntry {
                title: c
                    .title
                    .as_deref()
                    .filter(|x| !x.is_empty())
                    .unwrap_or(UNTITLED)
                    .to_string(),
                corpus_id: c.corpus_id.clone(),
                snippet: Some(c.snippet.as_str())
                    .filter(|x| !x.is_empty())
                    .map(str::to_string),
                url: c
                    .url
                    .as_deref()
                    .filter(|x| !x.is_empty())
                    .map(str::to_string),
            })
            .collect();
        Self {
            answered_by,
            corpora,
            body: content.trim().to_string(),
            sources,
        }
    }

    /// The one-line "who answered, over what" credit, or `None` when
    /// neither half is known.
    pub fn meta_line(&self) -> Option<String> {
        let mut bits = Vec::new();
        if let Some(b) = &self.answered_by {
            bits.push(format!("answered by {b}"));
        }
        if !self.corpora.is_empty() {
            bits.push(format!("searched {}", self.corpora.join(", ")));
        }
        (!bits.is_empty()).then(|| bits.join(" \u{00B7} "))
    }
}

/// The sentence a document shows in place of a ledger when the answer
/// cited nothing. Named because three renderers say it and a fourth
/// would otherwise say it slightly differently.
pub const NO_SOURCES_NOTE: &str =
    "No corpus passages were cited for this answer — it came from the model's own \
     knowledge or a non-retrieval path.";

/// The document's closing credit.
pub const EXPORT_FOOTER: &str = "Exported from svrnmesh — provenance preserved.";

/// A flat, format-agnostic block sequence a binary-format renderer
/// walks. Public because the `.docx` and `.pdf` encoders stay with the
/// host that owns their byte layout.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// The document's one title, first block.
    Title(String),
    /// A section heading — today only `"Sources"`.
    Heading(String),
    /// The "answered by … · searched …" credit under the title.
    Meta(String),
    /// A paragraph of the answer body, Markdown-denoised.
    Para(String),
    /// A numbered ledger entry: `"1. Free Will — sep"`.
    SourceTitle(String),
    /// A grounding passage, rendered as a block quote.
    Quote(String),
    /// A source URL, rendered on its own line.
    Url(String),
    /// The closing credit, last block.
    Footer(String),
}

/// Flatten the document into blocks.
pub fn doc_blocks(doc: &AnswerDoc) -> Vec<Block> {
    let mut blocks = vec![Block::Title("svrnmesh answer".to_string())];
    if let Some(meta) = doc.meta_line() {
        blocks.push(Block::Meta(meta));
    }
    for para in doc.body.split("\n\n") {
        let cleaned = strip_markdown_light(para);
        if !cleaned.is_empty() {
            blocks.push(Block::Para(cleaned));
        }
    }
    if doc.sources.is_empty() {
        blocks.push(Block::Para(NO_SOURCES_NOTE.to_string()));
    } else {
        blocks.push(Block::Heading("Sources".to_string()));
        for (i, s) in doc.sources.iter().enumerate() {
            blocks.push(Block::SourceTitle(format!(
                "{}. {} \u{2014} {}",
                i + 1,
                s.title,
                s.corpus_id
            )));
            if let Some(snippet) = &s.snippet {
                blocks.push(Block::Quote(strip_markdown_light(snippet)));
            }
            if let Some(url) = &s.url {
                blocks.push(Block::Url(url.clone()));
            }
        }
    }
    blocks.push(Block::Footer(EXPORT_FOOTER.to_string()));
    blocks
}

/// Light Markdown de-noising so prose reads cleanly in PDF/Word (which
/// don't interpret Markdown): drops `**`, leading `#`/`>`, and
/// backticks. Not a parser — just enough to avoid stray markup in the
/// exported document.
pub fn strip_markdown_light(s: &str) -> String {
    let mut lines = Vec::new();
    for line in s.lines() {
        let l = line.trim_start();
        let l = l.trim_start_matches(['#', '>']).trim_start();
        let cleaned = l.replace("**", "").replace("__", "").replace('`', "");
        lines.push(cleaned.trim_end().to_string());
    }
    lines.join("\n").trim().to_string()
}

/// Render the answer + its source ledger as a self-contained Markdown
/// document — the "provenance survives the handoff" guarantee.
///
/// Byte-pinned: `answer_doc::tests::markdown_golden` and the desktop's
/// `export_tests::markdown_golden_survived_the_move`.
pub fn render_answer_markdown(doc: &AnswerDoc) -> String {
    let mut md = String::from("# svrnmesh answer\n\n");

    if let Some(meta) = doc.meta_line() {
        md.push_str(&format!("*{meta}*\n\n"));
    }

    md.push_str(&doc.body);
    md.push_str("\n\n");

    if doc.sources.is_empty() {
        md.push_str(&format!("---\n\n*{NO_SOURCES_NOTE}*\n\n"));
    } else {
        md.push_str(
            "---\n\n## Sources\n\nThis answer was grounded in the following passages \
             from your indexed corpora:\n\n",
        );
        for (i, s) in doc.sources.iter().enumerate() {
            md.push_str(&format!("{}. **{}** — `{}`\n", i + 1, s.title, s.corpus_id));
            if let Some(snippet) = &s.snippet {
                for line in snippet.lines() {
                    md.push_str(&format!("   > {line}\n"));
                }
            }
            if let Some(url) = &s.url {
                md.push_str(&format!("   <{url}>\n"));
            }
            md.push('\n');
        }
    }

    md.push_str(&format!("---\n*{EXPORT_FOOTER}*\n"));
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::projection::{project_message_metadata, ProvenanceSource};
    use serde_json::json;

    /// The persisted-metadata shape the desktop export read directly
    /// before sv-surface D7/G9 moved the renderer here. Kept as the
    /// golden's INPUT so the move is judged on the real blob, not on a
    /// hand-built projection that could quietly differ from it.
    fn fixture_blob() -> serde_json::Value {
        json!({
            "provenance": {
                "inference_backend": "Qwen3-8B-Q4_K_M",
                "sources": [
                    {"origin": "case-files-7f2a", "count": 3, "display_name": "Case Files"},
                    {"origin": "sep", "count": 2},
                    {"origin": "wikipedia", "count": 0}
                ]
            },
            "retrieved_chunks": [
                {"title": "Free Will", "corpus_id": "sep", "chunk_id": "sep:fw:3",
                 "snippet": "Compatibilism holds that...\nfreedom is not the absence of cause.",
                 "score": 0.91, "url": "https://plato.stanford.edu/entries/free-will/"},
                {"title": "", "corpus_id": "case-files-7f2a", "chunk_id": "cf:9",
                 "snippet": "The deposition of 12 March.", "score": 0.6}
            ]
        })
    }

    fn fixture_doc() -> AnswerDoc {
        let (prov, cites) = project_message_metadata(&Some(fixture_blob()));
        AnswerDoc::from_projection(
            "Free will is compatible with determinism.",
            prov.as_ref(),
            &cites,
        )
    }

    /// THE GOLDEN. These exact bytes were produced by the desktop's
    /// blob-reading `render_answer_markdown` on `fixture_blob()` before
    /// the renderer moved (sv-surface D7/G9); the desktop keeps a twin
    /// assertion so both ends of the move are pinned to one string.
    /// A change here is a change to what a user's exported document
    /// says — make it deliberately.
    const GOLDEN_MD: &str = concat!(
        "# svrnmesh answer\n",
        "\n",
        "*answered by Qwen3-8B-Q4_K_M \u{00B7} searched Case Files, sep*\n",
        "\n",
        "Free will is compatible with determinism.\n",
        "\n",
        "---\n",
        "\n",
        "## Sources\n",
        "\n",
        "This answer was grounded in the following passages from your indexed corpora:\n",
        "\n",
        "1. **Free Will** \u{2014} `sep`\n",
        "   > Compatibilism holds that...\n",
        "   > freedom is not the absence of cause.\n",
        "   <https://plato.stanford.edu/entries/free-will/>\n",
        "\n",
        "2. **(untitled passage)** \u{2014} `case-files-7f2a`\n",
        "   > The deposition of 12 March.\n",
        "\n",
        "---\n",
        "*Exported from svrnmesh \u{2014} provenance preserved.*\n",
    );

    #[test]
    fn markdown_golden() {
        assert_eq!(render_answer_markdown(&fixture_doc()), GOLDEN_MD);
    }

    /// The reason `display_name` was added to the projection in this
    /// rung: without it the meta line would read "searched
    /// case-files-7f2a, sep" and a user's own folder would appear under
    /// a slug they never typed.
    #[test]
    fn folder_display_name_beats_the_slug_and_empty_corpora_are_dropped() {
        let doc = fixture_doc();
        assert_eq!(
            doc.corpora,
            vec!["Case Files".to_string(), "sep".to_string()]
        );
        assert_eq!(
            doc.meta_line().as_deref(),
            Some("answered by Qwen3-8B-Q4_K_M \u{00B7} searched Case Files, sep")
        );
    }

    #[test]
    fn no_provenance_and_no_citations_says_so() {
        let doc = AnswerDoc::from_projection("Hello.", None, &[]);
        let md = render_answer_markdown(&doc);
        assert!(md.contains("Hello."));
        assert!(md.contains("No corpus passages were cited"));
        // Never silently implies sources that aren't there.
        assert!(!md.contains("## Sources"));
        assert!(!md.contains('*') || !md.contains("answered by"));
    }

    #[test]
    fn blocks_carry_the_ledger_the_binary_renderers_walk() {
        let blocks = doc_blocks(&fixture_doc());
        assert_eq!(
            blocks.first(),
            Some(&Block::Title("svrnmesh answer".into()))
        );
        assert_eq!(blocks.last(), Some(&Block::Footer(EXPORT_FOOTER.into())));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::Heading(h) if h == "Sources")));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::SourceTitle(t) if t == "1. Free Will \u{2014} sep")));
    }

    /// An empty `display_name` on the wire must not blank the label.
    #[test]
    fn blank_display_name_falls_back_to_origin() {
        let prov = Provenance {
            inference_backend: "local".into(),
            routing_tier: None,
            ttft_ms: None,
            total_ms: None,
            finish_reason: None,
            max_tokens_budget: None,
            completion_tokens: None,
            sources: vec![ProvenanceSource {
                origin: "sep".into(),
                count: 1,
                from_peer: None,
                display_name: Some(String::new()),
            }],
        };
        let doc = AnswerDoc::from_projection("x", Some(&prov), &[]);
        assert_eq!(doc.corpora, vec!["sep".to_string()]);
    }
}
