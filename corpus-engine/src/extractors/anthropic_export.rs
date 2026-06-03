//! Anthropic claude.ai chat-export extractor.
//!
//! Parses the `conversations.json` produced by claude.ai's "Export
//! data" download. Emits one `ExtractedDoc` per conversation; the
//! `ThreadedTurnsChunker` splits each doc into turn-pair chunks.
//!
//! **Export schema** (relevant fields only):
//! ```json
//! [
//!   {
//!     "uuid": "...",
//!     "name": "Optional title",
//!     "summary": "",
//!     "created_at": "2026-03-12T01:29:07Z",
//!     "updated_at": "2026-03-14T01:32:55Z",
//!     "chat_messages": [
//!       {
//!         "uuid": "...",
//!         "sender": "human" | "assistant",
//!         "created_at": "...",
//!         "parent_message_uuid": "...",
//!         "content": [{"type": "text", "text": "..."}, ...]
//!       }
//!     ]
//!   }
//! ]
//! ```
//!
//! **Branch handling (v1).** Real exports occasionally branch when the
//! user edits a previous turn — multiple messages share the same
//! `parent_message_uuid`. v1 ignores the parent_message_uuid tree
//! and flattens messages by `created_at`. This is correct for the
//! common linear case and loses fidelity on edited branches. TODO:
//! pick the longest path through the tree (the leaf reachable by the
//! most-recent edit) for branch-aware ingest.
//!
//! **Empty messages.** Real exports often have leading rows with
//! `text: ""` and zero-length content blocks (placeholder turn
//! envelopes). These are dropped before formatting — they have no
//! retrievable content and would otherwise create empty turn blocks.
//!
//! **Output format.** Each conversation is rendered as a sequence of
//! turn blocks delimited by `### [YYYY-MM-DD HH:MM] sender\n\n<body>`.
//! Keeping the timestamp and first-person `[user]` marker in the
//! chunk content lights up the meta-atlas trace-axis classifier
//! (which checks for first-person + date signals).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use serde::Deserialize;

use super::{ExtractedDoc, Extractor};
use crate::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct RawConversation {
    uuid: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    chat_messages: Vec<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    sender: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    content: Vec<RawContentBlock>,
}

#[derive(Debug, Deserialize)]
struct RawContentBlock {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

pub struct AnthropicExportExtractor;

impl AnthropicExportExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AnthropicExportExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for AnthropicExportExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let file = File::open(source_path).map_err(|e| {
            Error::Extraction(format!("Failed to open {}: {e}", source_path.display()))
        })?;
        let convs: Vec<RawConversation> =
            serde_json::from_reader(BufReader::new(file)).map_err(|e| {
                Error::Extraction(format!(
                    "Failed to parse Anthropic export at {}: {e}",
                    source_path.display()
                ))
            })?;

        let iter = convs
            .into_iter()
            .filter_map(|c| convert_conversation(c).transpose());
        Ok(Box::new(iter))
    }
}

/// Convert one raw conversation into an `ExtractedDoc`. Returns
/// `Ok(None)` when the conversation has zero non-empty turns —
/// nothing for retrieval to bite on.
fn convert_conversation(c: RawConversation) -> Result<Option<ExtractedDoc>> {
    let mut msgs: Vec<RenderedTurn> = c
        .chat_messages
        .into_iter()
        .filter_map(render_turn)
        .collect();

    if msgs.is_empty() {
        return Ok(None);
    }

    // Flatten by created_at — branch handling is a v2 concern (see
    // module docstring). Stable sort preserves source order when
    // timestamps tie.
    msgs.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let body: String = msgs
        .iter()
        .map(|m| {
            format!(
                "### [{}] {}\n\n{}",
                short_ts(&m.created_at),
                m.sender,
                m.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let title = if c.name.trim().is_empty() {
        // Untitled conversation — first ~80 chars of first turn body
        // makes the doc legible in retrieval surfaces.
        msgs.first()
            .map(|m| short_summary(&m.body, 80))
            .unwrap_or_else(|| format!("conv-{}", &c.uuid[..8.min(c.uuid.len())]))
    } else {
        c.name.clone()
    };

    let meta = serde_json::json!({
        "conv_uuid": c.uuid,
        "created_at": c.created_at,
        "updated_at": c.updated_at,
        "msg_count": msgs.len(),
        "summary": c.summary,
    });

    Ok(Some(ExtractedDoc {
        title: Some(title),
        content: body,
        url: None,
        source_id: c.uuid.clone(),
        metadata: Some(meta),
        source_file: None,
        embed_text: None,
    }))
}

struct RenderedTurn {
    sender: &'static str,
    created_at: String,
    body: String,
}

fn render_turn(m: RawMessage) -> Option<RenderedTurn> {
    let sender = match m.sender.as_str() {
        "human" | "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    let mut body = String::new();
    if let Some(top) = m.text.as_ref() {
        if !top.trim().is_empty() {
            body.push_str(top.trim());
        }
    }
    for blk in m.content {
        if blk.kind != "text" {
            continue;
        }
        let t = blk.text.trim();
        if t.is_empty() {
            continue;
        }
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(t);
    }
    if body.is_empty() {
        return None;
    }
    Some(RenderedTurn {
        sender,
        created_at: m.created_at.unwrap_or_default(),
        body,
    })
}

/// Render an ISO-8601 timestamp as `YYYY-MM-DD HH:MM` for the turn
/// block heading. Falls back to the raw string if it doesn't parse,
/// and to "unknown-time" for empty input.
fn short_ts(raw: &str) -> String {
    if raw.is_empty() {
        return "unknown-time".to_string();
    }
    // Cheap parse — the export always emits `2026-03-12T01:29:07.923325Z`
    // shape. Avoid pulling chrono just for slicing.
    if raw.len() >= 16 && raw.as_bytes()[10] == b'T' {
        let date = &raw[..10];
        let time = &raw[11..16];
        return format!("{} {}", date, time);
    }
    raw.to_string()
}

fn short_summary(body: &str, max: usize) -> String {
    let cleaned: String = body
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_json() -> String {
        r#"[
          {
            "uuid": "conv-empty",
            "name": "",
            "summary": "",
            "created_at": "2026-01-01T00:00:00Z",
            "chat_messages": [
              {"sender": "human", "text": "", "content": [{"type":"text","text":""}], "created_at":"2026-01-01T00:00:00Z"}
            ]
          },
          {
            "uuid": "conv-cfo-runway",
            "name": "Runway discussion",
            "summary": "",
            "created_at": "2025-09-04T18:00:00Z",
            "updated_at": "2025-09-04T18:30:00Z",
            "chat_messages": [
              {"sender": "human", "created_at": "2025-09-04T18:01:00Z", "content":[{"type":"text","text":"What was our burn rate last month?"}]},
              {"sender": "assistant", "created_at": "2025-09-04T18:02:00Z", "content":[{"type":"text","text":"Based on the bank statement you shared, burn was $312k."}]},
              {"sender": "human", "created_at": "2025-09-04T18:10:00Z", "content":[{"type":"text","text":"And the runway?"}]},
              {"sender": "assistant", "created_at": "2025-09-04T18:11:00Z", "content":[{"type":"text","text":"At current burn, ~14 months."}]}
            ]
          }
        ]"#.to_string()
    }

    fn write_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.json");
        std::fs::write(&path, fixture_json()).unwrap();
        (dir, path)
    }

    #[test]
    fn skips_empty_conversations() {
        let (_dir, path) = write_fixture();
        let docs: Vec<_> = AnthropicExportExtractor::new()
            .extract(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 1, "empty conv must be skipped");
        assert_eq!(docs[0].source_id, "conv-cfo-runway");
    }

    #[test]
    fn turn_blocks_format_correctly() {
        let (_dir, path) = write_fixture();
        let docs: Vec<_> = AnthropicExportExtractor::new()
            .extract(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let body = &docs[0].content;
        assert!(
            body.contains("### [2025-09-04 18:01] user"),
            "first turn header missing: {}",
            body
        );
        assert!(body.contains("### [2025-09-04 18:02] assistant"));
        assert!(body.contains("What was our burn rate last month?"));
        assert!(body.contains("$312k"));
    }

    #[test]
    fn messages_sorted_by_created_at() {
        // Build out-of-order fixture.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let raw = r#"[{
          "uuid": "c1", "name": "n", "summary": "", "created_at": "2025-01-01T00:00:00Z",
          "chat_messages": [
            {"sender": "assistant", "created_at": "2025-01-01T00:00:30Z", "content":[{"type":"text","text":"second"}]},
            {"sender": "human", "created_at": "2025-01-01T00:00:10Z", "content":[{"type":"text","text":"first"}]}
          ]
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let docs: Vec<_> = AnthropicExportExtractor::new()
            .extract(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let body = &docs[0].content;
        let first_pos = body.find("first").unwrap();
        let second_pos = body.find("second").unwrap();
        assert!(
            first_pos < second_pos,
            "chronological order broken: {}",
            body
        );
    }

    #[test]
    fn untitled_conv_gets_summary_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let raw = r#"[{
          "uuid": "abcdef0123", "name": "", "summary": "", "created_at": "2025-01-01T00:00:00Z",
          "chat_messages": [
            {"sender": "human", "created_at": "2025-01-01T00:00:10Z", "content":[{"type":"text","text":"How does the meta-atlas dual-stream split work in practice?"}]}
          ]
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let docs: Vec<_> = AnthropicExportExtractor::new()
            .extract(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let t = docs[0].title.as_deref().unwrap();
        assert!(t.starts_with("How does the meta-atlas"), "title: {}", t);
    }

    #[test]
    fn metadata_carries_uuid_and_count() {
        let (_dir, path) = write_fixture();
        let docs: Vec<_> = AnthropicExportExtractor::new()
            .extract(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let meta = docs[0].metadata.as_ref().unwrap();
        assert_eq!(meta["conv_uuid"], "conv-cfo-runway");
        assert_eq!(meta["msg_count"], 4);
    }

    #[test]
    fn multi_block_message_concatenates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let raw = r#"[{
          "uuid": "c1", "name": "n", "summary": "", "created_at": "2025-01-01T00:00:00Z",
          "chat_messages": [
            {"sender": "human", "created_at": "2025-01-01T00:00:10Z",
             "content":[{"type":"text","text":"first block"},{"type":"text","text":"second block"}]}
          ]
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let docs: Vec<_> = AnthropicExportExtractor::new()
            .extract(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let body = &docs[0].content;
        assert!(
            body.contains("first block\n\nsecond block"),
            "body: {}",
            body
        );
    }

    #[test]
    fn non_text_blocks_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let raw = r#"[{
          "uuid": "c1", "name": "n", "summary": "", "created_at": "2025-01-01T00:00:00Z",
          "chat_messages": [
            {"sender": "human", "created_at": "2025-01-01T00:00:10Z",
             "content":[{"type":"text","text":"keep"},{"type":"tool_use","text":"drop"}]}
          ]
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let docs: Vec<_> = AnthropicExportExtractor::new()
            .extract(&path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(docs[0].content.contains("keep"));
        assert!(!docs[0].content.contains("drop"));
    }
}
