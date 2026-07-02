// SPDX-License-Identifier: AGPL-3.0-or-later
//! `section` — the workflow **structure-aware chunk** leaf: split a document
//! into sections by a *configurable* line-anchored regex `boundary`, emitting a
//! collection of `{text, index, title, section_id}`.
//!
//! Where `tool:chunk` splits by size (700-char passages), `tool:section` splits
//! by *structure* — the unit the real enrichment Phase 1 runs over (a chapter),
//! not an arbitrary 700-char window. It reuses the corpus engine's own
//! `ChapterRegexDetector` (the exact detector the v2 pipeline feeds into phase 1),
//! so a workflow-authored section pass matches the bespoke path.
//!
//! **Flexible by config, not by source.** The `boundary` regex is the whole knob:
//! the default recognises `Chapter`/`Part` forms, but there is nothing
//! book-specific underneath. Any line-anchored regex works —
//! `(?m)^#{1,3}\s` for Markdown headings, `(?m)^\[\d\d:\d\d\]` for transcript
//! turns, `(?m)^(§|Article)\s+\d+` for legal sections. One leaf, any corpus.
//!
//! **Robust on any input.** When the boundary matches nothing (a note with no
//! headings, a Markdown file run with the chapter default), it falls back to the
//! whole document as a single section, so a source never silently vanishes from
//! the pipeline. `min_body_words` drops phantom matches (a Table of Contents
//! whose entries match the heading regex but have no body). `paragraph_chunks`
//! sub-divides each section into ~700-char passages (carrying the section title),
//! for the embedding-clustering granularity rather than the chapter-input one.
//!
//! `Read`-effect + idempotent: pure over its input, so the workflow cache skips
//! it on an unchanged file.

use async_trait::async_trait;

use corpus_engine::chunkers::sectioned::{ChapterRegexDetector, SectionDetector};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use crate::rag::chunk::chunk_text;

pub struct SectionTool;

#[async_trait]
impl Tool for SectionTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "section".to_string(),
            name: "section".to_string(),
            description: "Split a document (file `path`, or inline `text`) into structural \
                          sections by a configurable line-anchored regex `boundary` (default: \
                          book chapters; set `boundary` for Markdown headings, transcript turns, \
                          legal articles — any source). Emits a collection of \
                          {text, index, title, section_id}."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read and section" },
                    "text": { "type": "string", "description": "Inline text to section (used if no path)" },
                    "boundary": { "type": "string", "description": "Line-anchored regex marking section starts. Default: Chapter/Part. e.g. `(?m)^#{1,3}\\s` for Markdown." },
                    "min_body_words": { "type": "integer", "description": "Drop matches whose body has fewer than N words (phantom ToC entries). Default 0." },
                    "paragraph_chunks": { "type": "boolean", "description": "Sub-divide each section into ~700-char passages carrying the section title (default false = whole sections)." }
                }
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "index": { "type": "integer" },
                        "title": { "type": "string" },
                        "section_id": { "type": "string" }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        // Prefer inline `text`; otherwise read the file at `path`.
        let text = match params.get("text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => {
                let path = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Execution("section: need a `path` or `text`".into()))?;
                std::fs::read_to_string(path)
                    .map_err(|e| Error::Execution(format!("section: read {path}: {e}")))?
            }
        };

        // The boundary is the whole flexibility knob: default = book chapters, any
        // line-anchored regex otherwise. A bad regex is a loud error, not a
        // silently-empty pass.
        let min_body_words = params
            .get("min_body_words")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let detector = match params.get("boundary").and_then(|v| v.as_str()) {
            Some(pat) => ChapterRegexDetector::with_pattern(pat)
                .map_err(|e| Error::Execution(format!("section: invalid `boundary` regex: {e}")))?,
            None => ChapterRegexDetector::new(),
        }
        .with_min_body_words(min_body_words);

        let paragraph_chunks = params
            .get("paragraph_chunks")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let sections = detector.detect(&text);
        let mut out: Vec<serde_json::Value> = Vec::new();
        let mut index = 0usize;

        if sections.is_empty() {
            // No boundary matched (a heading-less note, or a source whose markers
            // don't fit the default) — treat the whole document as one section, so
            // any input yields output rather than vanishing from the pipeline.
            emit_section(
                &mut out,
                &mut index,
                "sec_0001",
                "",
                text.trim(),
                paragraph_chunks,
            );
        } else {
            for s in &sections {
                // `start_byte..end_byte` is the section *body* (the heading is the
                // title). Detector offsets are regex-match boundaries, so they're
                // char-aligned; `get` is defensive against any edge case.
                let body = text.get(s.start_byte..s.end_byte).unwrap_or("").trim();
                emit_section(
                    &mut out,
                    &mut index,
                    &s.id,
                    &s.title,
                    body,
                    paragraph_chunks,
                );
            }
        }

        Ok(StepOutput::Json(serde_json::Value::Array(out)))
    }
}

/// Emit one section as one item (whole body) or, when `paragraph_chunks`, as
/// several ~700-char passages — each carrying the section's `title`/`section_id`
/// so a downstream `stamp`/group can recover which section a chunk came from.
/// An empty body emits nothing.
fn emit_section(
    out: &mut Vec<serde_json::Value>,
    index: &mut usize,
    section_id: &str,
    title: &str,
    body: &str,
    paragraph_chunks: bool,
) {
    if body.is_empty() {
        return;
    }
    if paragraph_chunks {
        for c in chunk_text(body) {
            out.push(serde_json::json!({
                "text": c.content, "index": *index, "title": title, "section_id": section_id
            }));
            *index += 1;
        }
    } else {
        out.push(serde_json::json!({
            "text": body, "index": *index, "title": title, "section_id": section_id
        }));
        *index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    fn as_array(out: StepOutput) -> Vec<serde_json::Value> {
        match out {
            StepOutput::Json(serde_json::Value::Array(a)) => a,
            other => panic!("expected a JSON array collection, got {other:?}"),
        }
    }

    /// The flexibility claim: the SAME leaf sections a book by default and any
    /// other source by `boundary` — proving it's config-driven, not book-bound.
    #[tokio::test]
    async fn sections_a_book_by_default_and_markdown_by_config() {
        // Default (chapter) boundary. Preamble before the first chapter is dropped.
        let book = "Front matter preamble.\n\n\
                    Chapter 1\n\nThe shop stood in shabby Soho. Verloc kept it.\n\n\
                    Chapter 2\n\nThe Assistant Commissioner left Scotland Yard at dusk.";
        let arr = as_array(
            SectionTool
                .execute(&serde_json::json!({ "text": book }), &ctx())
                .await
                .unwrap(),
        );
        assert_eq!(arr.len(), 2, "two chapters: {arr:?}");
        assert!(arr[0]["title"].as_str().unwrap().contains("Chapter 1"));
        assert!(arr[0]["text"].as_str().unwrap().contains("Soho"));
        assert!(
            !arr[0]["text"].as_str().unwrap().contains("Front matter"),
            "preamble before the first boundary is dropped"
        );
        assert_eq!(arr[0]["index"], serde_json::json!(0));
        assert_eq!(arr[1]["index"], serde_json::json!(1));
        assert_eq!(arr[1]["section_id"], serde_json::json!("sec_0002"));

        // Markdown headings via config — the same leaf, a different `boundary`.
        let md = "# Intro\n\nHello world lives here.\n\n# Details\n\nMore content follows after.";
        let arr2 = as_array(
            SectionTool
                .execute(
                    &serde_json::json!({ "text": md, "boundary": r"(?m)^#\s+.*$" }),
                    &ctx(),
                )
                .await
                .unwrap(),
        );
        assert_eq!(arr2.len(), 2, "two markdown sections: {arr2:?}");
        assert!(arr2[0]["title"].as_str().unwrap().contains("Intro"));
        assert!(arr2[1]["text"].as_str().unwrap().contains("More content"));
    }

    /// Robustness: a source with no boundary match yields the whole document as
    /// one section rather than vanishing — so any input flows through a pipeline.
    #[tokio::test]
    async fn no_boundary_match_falls_back_to_whole_document() {
        let note = "Just a flat note with no headings at all. It has two sentences.";
        let arr = as_array(
            SectionTool
                .execute(&serde_json::json!({ "text": note }), &ctx())
                .await
                .unwrap(),
        );
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["text"].as_str().unwrap().contains("flat note"));
        assert_eq!(arr[0]["index"], serde_json::json!(0));
    }

    /// `paragraph_chunks` sub-divides an oversized section, every passage keeping
    /// its section's title + id (so the section is recoverable after the fan-out).
    #[tokio::test]
    async fn paragraph_chunks_subdivides_keeping_the_section_title() {
        let big = "Sentence about Verloc and his shabby shop. ".repeat(40); // > 700 chars
        let book = format!("Chapter 1\n\n{big}");
        let arr = as_array(
            SectionTool
                .execute(
                    &serde_json::json!({ "text": book, "paragraph_chunks": true }),
                    &ctx(),
                )
                .await
                .unwrap(),
        );
        assert!(
            arr.len() > 1,
            "an oversized section subdivides, got {}",
            arr.len()
        );
        for (i, c) in arr.iter().enumerate() {
            assert!(c["title"].as_str().unwrap().contains("Chapter 1"));
            assert_eq!(c["section_id"], serde_json::json!("sec_0001"));
            assert_eq!(
                c["index"],
                serde_json::json!(i),
                "indices are global + sequential"
            );
        }
    }

    /// A bad `boundary` regex fails loudly rather than silently emitting nothing.
    #[tokio::test]
    async fn bad_boundary_regex_is_a_loud_error() {
        assert!(SectionTool
            .execute(&serde_json::json!({ "text": "x", "boundary": "(" }), &ctx())
            .await
            .is_err());
        // Neither `path` nor `text` is also a loud error.
        assert!(SectionTool
            .execute(&serde_json::json!({}), &ctx())
            .await
            .is_err());
    }
}
