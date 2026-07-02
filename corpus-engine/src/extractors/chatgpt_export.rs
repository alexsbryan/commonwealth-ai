// SPDX-License-Identifier: AGPL-3.0-or-later
//! OpenAI ChatGPT chat-export extractor.
//!
//! Parses the `conversations.json` produced by ChatGPT's "Export data"
//! download (Settings → Data controls → Export). Emits one
//! [`ExtractedDoc`] per conversation, rendered in the **same turn-block
//! format** as [`super::anthropic_export::AnthropicExportExtractor`] so
//! the shared [`ThreadedTurnsChunker`](crate::chunkers::threaded_turns)
//! and the `conversational` enrichment domain consume both sources
//! identically — only the parse front-end differs.
//!
//! **Export schema** (relevant fields only):
//! ```json
//! [
//!   {
//!     "id": "6a0b…",
//!     "conversation_id": "6a0b…",
//!     "title": "Welcome to ChatGPT",
//!     "create_time": 1779146451.82,
//!     "update_time": 1779146562.65,
//!     "current_node": "51a0…",
//!     "default_model_slug": "auto",
//!     "mapping": {
//!       "<node-id>": {
//!         "id": "<node-id>",
//!         "parent": "<parent-node-id>" | null,
//!         "message": {
//!           "author":  {"role": "user"|"assistant"|"system"|"tool"},
//!           "create_time": 1779146549.15,
//!           "content": {"content_type": "text", "parts": ["…"]}
//!         } | null
//!       }
//!     }
//!   }
//! ]
//! ```
//!
//! **Tree traversal.** ChatGPT stores messages as a tree, not a list:
//! editing a turn forks a sibling branch. We reconstruct the *current*
//! linear thread by walking `parent` pointers **up** from `current_node`
//! to the root, then reversing into chronological order. This is exactly
//! the thread the user sees, and is branch-correct by construction —
//! strictly better than the Anthropic v1 flatten-by-timestamp, which
//! loses fidelity on edited branches. Real exports observed in the wild
//! omit `children[]` arrays entirely (only `parent` + `current_node` are
//! present), so the up-walk is the robust choice. **Fallback:** when
//! `current_node` is absent or doesn't resolve, flatten every
//! message-bearing node by `create_time` (mirrors Anthropic v1).
//!
//! **Roles (v1).** `user` → `user`, `assistant` → `assistant`. `system`
//! and `tool` messages (custom-instruction envelopes, plugin/tool
//! output) are dropped to match the Anthropic two-role model. *TODO:*
//! ingest `user_editable_context` system messages as custom
//! instructions.
//!
//! **Content types.** Only `text` / `multimodal_text` (the user-visible
//! message bodies) are rendered; `code` (analysis tool), `thoughts`,
//! `reasoning_recap`, `tether_*` (browsing), and `system_error` are
//! skipped so reasoning traces and tool gunk never leak into the corpus.
//! Within a multimodal message, dict parts (image pointers) are dropped
//! and only string parts survive.
//!
//! **Inline markers.** ChatGPT annotates assistant text with
//! Private-Use-Area markers shaped `\u{E200}<type>\u{E202}<payload>\u{E201}`.
//! Observed types: `entity` (payload = JSON `["category","Name","desc"]`,
//! rendered as the Name) and `url` (payload = `display\u{E202}https://…`,
//! rendered as a markdown link). Unknown types degrade to their first
//! payload field, and a final sweep removes any residual `U+E200..=U+E20F`
//! so no control characters survive into chunk text or embeddings.
//!
//! **Timestamps.** `create_time` is a float Unix epoch; rendered as
//! `YYYY-MM-DD HH:MM` UTC to match the Anthropic block heading (whose
//! ISO-string slice is also effectively UTC).
//!
//! **Empty conversations / messages** (zero renderable turns) are
//! dropped — nothing for retrieval to bite on.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

use super::{short_summary, ExtractedDoc, Extractor};
use crate::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct RawConversation {
    #[serde(default)]
    id: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    create_time: Option<f64>,
    #[serde(default)]
    update_time: Option<f64>,
    #[serde(default)]
    current_node: Option<String>,
    #[serde(default)]
    default_model_slug: Option<String>,
    #[serde(default)]
    mapping: HashMap<String, RawNode>,
}

#[derive(Debug, Deserialize)]
struct RawNode {
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    message: Option<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    author: RawAuthor,
    #[serde(default)]
    create_time: Option<f64>,
    #[serde(default)]
    content: Option<RawContent>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAuthor {
    #[serde(default)]
    role: String,
}

#[derive(Debug, Deserialize)]
struct RawContent {
    #[serde(default)]
    content_type: String,
    /// Parts are usually strings; multimodal messages interleave dicts
    /// (image asset pointers). Kept as untyped JSON so we can filter to
    /// the string parts without a fallible enum deserialize.
    #[serde(default)]
    parts: Vec<serde_json::Value>,
}

pub struct ChatgptExportExtractor;

impl ChatgptExportExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChatgptExportExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for ChatgptExportExtractor {
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
                    "Failed to parse ChatGPT export at {}: {e}",
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
/// `Ok(None)` when the conversation has zero renderable turns.
fn convert_conversation(c: RawConversation) -> Result<Option<ExtractedDoc>> {
    // Primary: current-branch path via parent walk. Fallback: all
    // message nodes flattened by create_time when `current_node` is
    // absent or doesn't resolve to a node in `mapping`.
    let ordered: Vec<&RawMessage> = {
        let path = walk_current_path(&c);
        if path.is_empty() {
            let mut all: Vec<&RawMessage> = c
                .mapping
                .values()
                .filter_map(|n| n.message.as_ref())
                .collect();
            all.sort_by(|a, b| {
                a.create_time
                    .unwrap_or(0.0)
                    .partial_cmp(&b.create_time.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            all
        } else {
            path
        }
    };

    let msgs: Vec<RenderedTurn> = ordered.into_iter().filter_map(render_turn).collect();
    if msgs.is_empty() {
        return Ok(None);
    }

    let body: String = msgs
        .iter()
        .map(|m| {
            format!(
                "### [{}] {}\n\n{}",
                short_ts_epoch(m.created_at),
                m.sender,
                m.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    // `conversation_id` is the stable id; fall back to `id` (the export
    // sets both to the same value, but tolerate either being absent).
    let conv_id = c
        .conversation_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| c.id.clone());

    let title = if c.title.trim().is_empty() {
        // Untitled conversation — first ~80 chars of the first turn body
        // keeps the doc legible in retrieval surfaces.
        msgs.first()
            .map(|m| short_summary(&m.body, 80))
            .unwrap_or_else(|| {
                if conv_id.is_empty() {
                    "chatgpt-conversation".to_string()
                } else {
                    format!("conv-{}", &conv_id[..8.min(conv_id.len())])
                }
            })
    } else {
        c.title.clone()
    };

    let meta = serde_json::json!({
        "source": "chatgpt",
        "conv_id": conv_id,
        "create_time": c.create_time,
        "update_time": c.update_time,
        "msg_count": msgs.len(),
        "model_slug": c.default_model_slug,
        "title": c.title,
    });

    Ok(Some(ExtractedDoc {
        title: Some(title),
        content: body,
        url: None,
        source_id: conv_id,
        metadata: Some(meta),
        source_file: None,
        embed_text: None,
    }))
}

/// Reconstruct the current linear thread by walking `parent` pointers up
/// from `current_node` to the root, then reversing into chronological
/// order. A `HashSet` guards against a cyclic `parent` chain (malformed
/// export) — without it a cycle would loop forever. Returns an empty
/// vec when `current_node` is `None` or doesn't resolve, signalling the
/// caller to use the create_time fallback.
fn walk_current_path(c: &RawConversation) -> Vec<&RawMessage> {
    let mut out: Vec<&RawMessage> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cursor: Option<&str> = c.current_node.as_deref();
    while let Some(id) = cursor {
        if !seen.insert(id) {
            break; // cycle guard
        }
        let Some(node) = c.mapping.get(id) else {
            break;
        };
        if let Some(msg) = node.message.as_ref() {
            out.push(msg);
        }
        cursor = node.parent.as_deref();
    }
    out.reverse();
    out
}

struct RenderedTurn {
    sender: &'static str,
    created_at: Option<f64>,
    body: String,
}

fn render_turn(m: &RawMessage) -> Option<RenderedTurn> {
    let sender = match m.author.role.as_str() {
        "user" => "user",
        "assistant" => "assistant",
        // system / tool / unknown — dropped in v1.
        _ => return None,
    };
    let content = m.content.as_ref()?;
    // Only the user-visible conversational content types. Skips
    // reasoning traces, tool code, browsing tethers, and custom-
    // instruction envelopes that share the `mapping` tree.
    match content.content_type.as_str() {
        "text" | "multimodal_text" | "" => {}
        _ => return None,
    }

    let mut body = String::new();
    for part in &content.parts {
        // Drop non-string parts (multimodal image pointers are dicts).
        let Some(s) = part.as_str() else {
            continue;
        };
        let cleaned = clean_markers(s);
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(trimmed);
    }
    if body.is_empty() {
        return None;
    }
    Some(RenderedTurn {
        sender,
        created_at: m.create_time,
        body,
    })
}

/// Render a float Unix-epoch timestamp as `YYYY-MM-DD HH:MM` (UTC) for
/// the turn-block heading. Falls back to `"unknown-time"` for missing or
/// out-of-range input. Mirrors the Anthropic extractor's `short_ts`
/// output shape so both sources chunk identically.
fn short_ts_epoch(ts: Option<f64>) -> String {
    let Some(secs_f) = ts else {
        return "unknown-time".to_string();
    };
    if !secs_f.is_finite() || secs_f < 0.0 {
        return "unknown-time".to_string();
    }
    let secs = secs_f.trunc() as i64;
    let nsec = (secs_f.fract() * 1_000_000_000.0) as u32;
    match chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nsec) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => "unknown-time".to_string(),
    }
}

/// Strip ChatGPT's Private-Use-Area inline annotation markers, rendering
/// their human-readable content. Markers are shaped
/// `\u{E200}<type>\u{E202}<payload…>\u{E201}` with `\u{E202}`-separated
/// payload fields. After replacing matched markers, a final pass removes
/// any residual `U+E200..=U+E20F` so unpaired/unknown control sequences
/// never survive into chunk text.
fn clean_markers(s: &str) -> String {
    // Fast path — the vast majority of turns carry no markers. Scan the
    // WHOLE annotation block, not just E200/E201/E202: a lone straggler
    // (e.g. an E206 nav marker) must still be stripped below.
    if !s.contains(is_pua_marker) {
        return s.to_string();
    }
    let replaced = marker_regex().replace_all(s, |caps: &regex::Captures| {
        render_marker(&caps[1], &caps[2])
    });
    if replaced.contains(is_pua_marker) {
        replaced.chars().filter(|c| !is_pua_marker(*c)).collect()
    } else {
        replaced.into_owned()
    }
}

/// ChatGPT's inline-annotation Private-Use-Area block (`U+E200..=U+E20F`).
fn is_pua_marker(c: char) -> bool {
    ('\u{E200}'..='\u{E20F}').contains(&c)
}

fn marker_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    // type = word chars; payload = anything (incl. newlines via `(?s)`)
    // up to the terminator, non-greedy so adjacent markers don't merge
    // into one match.
    CELL.get_or_init(|| Regex::new("(?s)\u{E200}(\\w+)\u{E202}(.*?)\u{E201}").unwrap())
}

fn render_marker(kind: &str, payload: &str) -> String {
    match kind {
        "entity" => {
            // payload = JSON ["category","Display Name","description"].
            // Use the display name (index 1); fall back to index 0, then
            // to the raw payload — a schema change degrades to *some*
            // readable text rather than dropping content.
            serde_json::from_str::<Vec<String>>(payload)
                .ok()
                .and_then(|v| v.get(1).or_else(|| v.first()).cloned())
                .unwrap_or_else(|| payload.to_string())
        }
        "url" => {
            // payload = `display\u{E202}https://…`. Render a markdown
            // link; if it doesn't split, emit whichever field we have.
            let mut it = payload.split('\u{E202}');
            let display = it.next().unwrap_or("").trim();
            match it.next().map(str::trim) {
                Some(href) if !href.is_empty() => {
                    if display.is_empty() {
                        href.to_string()
                    } else {
                        format!("[{display}]({href})")
                    }
                }
                _ => display.to_string(),
            }
        }
        // Unknown marker type — keep the first payload field, drop the rest.
        _ => payload
            .split('\u{E202}')
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the real-export shape: a `mapping` tree with `parent`
    /// pointers + `current_node` (no `children[]`), float-epoch
    /// timestamps, and the entity + url PUA markers.
    ///
    /// PUA marker chars can't be written into a raw string literal
    /// (raw strings don't process `\u{…}` escapes), so the fixture uses
    /// ASCII sentinels `@S@`/`@D@`/`@E@` for `U+E200`/`U+E202`/`U+E201`
    /// and substitutes the real codepoints here — unambiguous and
    /// readable, unlike invisible placeholder chars.
    fn fixture_json() -> String {
        r#"[
          {
            "id": "conv-empty",
            "conversation_id": "conv-empty",
            "title": "",
            "create_time": 1735689600.0,
            "current_node": "n2",
            "mapping": {
              "root": {"id": "root", "parent": null, "message": null},
              "n1": {"id": "n1", "parent": "root", "message": {
                "author": {"role": "system"}, "create_time": 1735689600.0,
                "content": {"content_type": "text", "parts": [""]}
              }},
              "n2": {"id": "n2", "parent": "n1", "message": {
                "author": {"role": "tool"}, "create_time": 1735689601.0,
                "content": {"content_type": "text", "parts": ["tool noise"]}
              }}
            }
          },
          {
            "id": "conv-reflexivity",
            "conversation_id": "conv-reflexivity",
            "title": "Reflexivity",
            "create_time": 1779146451.82,
            "update_time": 1779146562.65,
            "current_node": "a1",
            "default_model_slug": "auto",
            "mapping": {
              "client-created-root": {"id": "client-created-root", "parent": null, "message": null},
              "greet": {"id": "greet", "parent": "client-created-root", "message": {
                "author": {"role": "assistant"}, "create_time": 1779146451.77,
                "content": {"content_type": "text", "parts": ["What's on your mind today?"]}
              }},
              "u1": {"id": "u1", "parent": "greet", "message": {
                "author": {"role": "user"}, "create_time": 1779146549.15,
                "content": {"content_type": "text", "parts": ["Tell me about reflexivity in financial markets"]}
              }},
              "a1": {"id": "a1", "parent": "u1", "message": {
                "author": {"role": "assistant"}, "create_time": 1779146550.78,
                "content": {"content_type": "text", "parts": ["Most associated with @S@entity@D@[\"people\",\"George Soros\",\"investor and philosopher\"]@E@. See @S@url@D@ChatGPT@D@https://chatgpt.com@E@."]}
              }}
            }
          }
        ]"#
        .replace("@S@", "\u{E200}")
        .replace("@D@", "\u{E202}")
        .replace("@E@", "\u{E201}")
    }

    fn write_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.json");
        std::fs::write(&path, fixture_json()).unwrap();
        (dir, path)
    }

    fn extract(path: &Path) -> Vec<ExtractedDoc> {
        ChatgptExportExtractor::new()
            .extract(path)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn skips_empty_or_nonuser_conversations() {
        let (_dir, path) = write_fixture();
        let docs = extract(&path);
        // conv-empty has only system/tool turns (dropped) → skipped.
        assert_eq!(docs.len(), 1, "only the reflexivity conv should survive");
        assert_eq!(docs[0].source_id, "conv-reflexivity");
    }

    #[test]
    fn walks_current_path_in_chronological_order() {
        let (_dir, path) = write_fixture();
        let body = &extract(&path)[0].content;
        // greeting (assistant) → user question → assistant answer.
        let greet = body.find("What's on your mind").unwrap();
        let q = body.find("Tell me about reflexivity").unwrap();
        let a = body.find("Most associated with").unwrap();
        assert!(greet < q && q < a, "thread order broken:\n{body}");
    }

    #[test]
    fn turn_headers_match_threaded_turns_contract() {
        let (_dir, path) = write_fixture();
        let body = &extract(&path)[0].content;
        // The exact regex the ThreadedTurnsChunker parses
        // (chunkers/threaded_turns.rs). Proving the headers match here
        // is the contract that lets the shared chunker consume ChatGPT
        // output without any source-specific code.
        let re = Regex::new(r"(?m)^###\s+\[([^\]]+)\]\s+(user|assistant)\s*$").unwrap();
        let senders: Vec<&str> = re
            .captures_iter(body)
            .map(|c| c.get(2).unwrap().as_str())
            .collect();
        assert_eq!(
            senders,
            vec!["assistant", "user", "assistant"],
            "headers must parse as 3 turns in thread order:\n{body}"
        );
        // The captured timestamp is the YYYY-MM-DD HH:MM shape.
        let ts_re = Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$").unwrap();
        for cap in re.captures_iter(body) {
            assert!(
                ts_re.is_match(&cap[1]),
                "bad timestamp shape: {:?}",
                &cap[1]
            );
        }
    }

    #[test]
    fn short_ts_epoch_formats_known_constant() {
        // 1735689600 == 2025-01-01T00:00:00Z (a round, verifiable epoch).
        assert_eq!(short_ts_epoch(Some(1735689600.0)), "2025-01-01 00:00");
        assert_eq!(short_ts_epoch(None), "unknown-time");
        assert_eq!(short_ts_epoch(Some(-1.0)), "unknown-time");
    }

    #[test]
    fn entity_marker_renders_display_name() {
        let (_dir, path) = write_fixture();
        let body = &extract(&path)[0].content;
        assert!(
            body.contains("Most associated with George Soros."),
            "{body}"
        );
        assert!(
            !body.contains("investor and philosopher"),
            "desc leaked: {body}"
        );
    }

    #[test]
    fn url_marker_renders_markdown_link() {
        let (_dir, path) = write_fixture();
        let body = &extract(&path)[0].content;
        assert!(body.contains("[ChatGPT](https://chatgpt.com)"), "{body}");
    }

    #[test]
    fn no_pua_control_chars_survive() {
        let (_dir, path) = write_fixture();
        let body = &extract(&path)[0].content;
        assert!(
            !body.chars().any(|c| ('\u{E200}'..='\u{E20F}').contains(&c)),
            "PUA marker chars leaked into chunk text"
        );
    }

    #[test]
    fn metadata_carries_conv_id_and_count() {
        let (_dir, path) = write_fixture();
        let meta = extract(&path)[0].metadata.clone().unwrap();
        assert_eq!(meta["conv_id"], "conv-reflexivity");
        assert_eq!(meta["msg_count"], 3);
        assert_eq!(meta["source"], "chatgpt");
        assert_eq!(meta["model_slug"], "auto");
    }

    #[test]
    fn untitled_conversation_gets_summary_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let raw = r#"[{
          "id": "abcdef0123", "conversation_id": "abcdef0123", "title": "",
          "create_time": 1735689600.0, "current_node": "u1",
          "mapping": {
            "root": {"id": "root", "parent": null, "message": null},
            "u1": {"id": "u1", "parent": "root", "message": {
              "author": {"role": "user"}, "create_time": 1735689600.0,
              "content": {"content_type": "text", "parts": ["How does the meta-atlas dual-stream split work in practice?"]}
            }}
          }
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let t = extract(&path)[0].title.clone().unwrap();
        assert!(t.starts_with("How does the meta-atlas"), "title: {t}");
    }

    #[test]
    fn multimodal_dict_parts_dropped_keeps_strings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let raw = r#"[{
          "id": "c1", "conversation_id": "c1", "title": "n",
          "create_time": 1735689600.0, "current_node": "u1",
          "mapping": {
            "root": {"id": "root", "parent": null, "message": null},
            "u1": {"id": "u1", "parent": "root", "message": {
              "author": {"role": "user"}, "create_time": 1735689600.0,
              "content": {"content_type": "multimodal_text",
                "parts": [{"content_type": "image_asset_pointer", "asset_pointer": "file-x"}, "describe this image"]}
            }}
          }
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let body = &extract(&path)[0].content;
        assert!(body.contains("describe this image"), "{body}");
        assert!(
            !body.contains("image_asset_pointer"),
            "dict part leaked: {body}"
        );
    }

    #[test]
    fn reasoning_and_code_content_types_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let raw = r#"[{
          "id": "c1", "conversation_id": "c1", "title": "n",
          "create_time": 1735689600.0, "current_node": "a2",
          "mapping": {
            "root": {"id": "root", "parent": null, "message": null},
            "u1": {"id": "u1", "parent": "root", "message": {
              "author": {"role": "user"}, "create_time": 1735689600.0,
              "content": {"content_type": "text", "parts": ["visible question"]}
            }},
            "a1": {"id": "a1", "parent": "u1", "message": {
              "author": {"role": "assistant"}, "create_time": 1735689601.0,
              "content": {"content_type": "thoughts", "parts": ["secret chain of thought"]}
            }},
            "a2": {"id": "a2", "parent": "a1", "message": {
              "author": {"role": "assistant"}, "create_time": 1735689602.0,
              "content": {"content_type": "text", "parts": ["visible answer"]}
            }}
          }
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let body = &extract(&path)[0].content;
        assert!(
            body.contains("visible question") && body.contains("visible answer"),
            "{body}"
        );
        assert!(
            !body.contains("secret chain of thought"),
            "reasoning leaked: {body}"
        );
    }

    #[test]
    fn fallback_orders_by_create_time_when_current_node_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        // No `current_node` → fallback to create_time sort. Nodes are
        // listed out of order to prove the sort runs.
        let raw = r#"[{
          "id": "c1", "conversation_id": "c1", "title": "n", "create_time": 1735689600.0,
          "mapping": {
            "a": {"id": "a", "parent": "root", "message": {
              "author": {"role": "assistant"}, "create_time": 1735689630.0,
              "content": {"content_type": "text", "parts": ["second"]}
            }},
            "u": {"id": "u", "parent": "root", "message": {
              "author": {"role": "user"}, "create_time": 1735689610.0,
              "content": {"content_type": "text", "parts": ["first"]}
            }}
          }
        }]"#;
        std::fs::write(&path, raw).unwrap();
        let body = &extract(&path)[0].content;
        assert!(
            body.find("first").unwrap() < body.find("second").unwrap(),
            "{body}"
        );
    }

    #[test]
    fn unknown_marker_degrades_to_first_field() {
        // A `cite`-style marker the renderer doesn't special-case.
        let input = "see \u{E200}cite\u{E202}Source A\u{E202}turn0\u{E201} here";
        assert_eq!(clean_markers(input), "see Source A here");
    }

    #[test]
    fn clean_markers_strips_orphan_pua() {
        // An unpaired control char with no terminator must not survive.
        let input = "tail\u{E206}dangling";
        assert_eq!(clean_markers(input), "taildangling");
    }
}
