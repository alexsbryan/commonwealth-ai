// SPDX-License-Identifier: AGPL-3.0-or-later
//! Helpers used by `Runtime::handle_attached_doc_turn` to render its
//! ReasonWithTools-style prefill, parse inline tool-call markers out
//! of model output, and clip query strings for narration chips.
//!
//! Kept separate from `runtime.rs` so the per-iteration loop can be
//! followed without scrolling past the rendering plumbing; the
//! attached-doc handler in `runtime/handlers/attached_doc.rs` (PR 4)
//! will continue to call into this module unchanged.

/// A single step in the `handle_attached_doc_turn` ReasonWithTools
/// loop. Held as a typed list so the conversation can be re-rendered
/// each iteration with superseded tool results compressed out of
/// the prefill.
#[derive(Debug)]
pub(crate) enum AttachedDocSegment {
    /// One tool dispatch + its result. The renderer keeps the most
    /// recent N results per tool_id verbatim and compresses older
    /// ones to a one-line marker.
    ToolCall {
        thinking: String,
        tool_id: String,
        query: String,
        result: String,
        passage_count: usize,
    },
    /// A forcing gate (no-retrieval, triangulation). Always rendered
    /// in full — gate text is short and load-bearing for the next
    /// turn's reasoning.
    Gate { thinking: String, gate_text: String },
    /// The "you've used all your searches — synthesize" cue that
    /// closes the iteration loop. Always rendered in full.
    FinalCue(String),
}

/// Cap on how many tool results per tool_id to keep verbatim in the
/// prefill. Older results on the same tool collapse to a one-line
/// `(superseded — N passages, content dropped)` marker. Two is enough
/// to let the model cross-reference its two most recent angles while
/// preventing the prefill from doubling on each iteration.
const MAX_TOOL_RESULTS_KEPT_PER_TOOL: usize = 2;

/// Render the attached-doc conversation as a single string for the
/// next inference call. Compresses older tool results on the same
/// tool to bound prefill cost.
pub(crate) fn render_attached_doc_conversation(
    header: &str,
    segments: &[AttachedDocSegment],
) -> String {
    use std::collections::{HashMap, HashSet};

    // Per tool_id, collect segment indices; keep the most recent N.
    let mut per_tool: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, seg) in segments.iter().enumerate() {
        if let AttachedDocSegment::ToolCall { tool_id, .. } = seg {
            per_tool.entry(tool_id.as_str()).or_default().push(i);
        }
    }
    let mut keep_full: HashSet<usize> = HashSet::new();
    for indices in per_tool.values() {
        for &i in indices.iter().rev().take(MAX_TOOL_RESULTS_KEPT_PER_TOOL) {
            keep_full.insert(i);
        }
    }

    let mut out = String::with_capacity(header.len() + segments.len() * 2048);
    out.push_str(header);
    for (i, seg) in segments.iter().enumerate() {
        match seg {
            AttachedDocSegment::ToolCall {
                thinking,
                tool_id,
                query,
                result,
                passage_count,
            } => {
                if keep_full.contains(&i) {
                    out.push_str(&format!(
                        " {thinking}\n\n[{tool_id} results for \"{query}\"]:\n{result}\n\nAssistant:",
                    ));
                } else {
                    out.push_str(&format!(
                        " {thinking}\n\n[{tool_id} results for \"{query}\" — {passage_count} passage(s); content dropped from this prefill, superseded by a later query on the same tool. The evidence informed the queries you issued after it.]\n\nAssistant:",
                    ));
                }
            }
            AttachedDocSegment::Gate {
                thinking,
                gate_text,
            } => {
                out.push_str(&format!(" {thinking}\n\n[gate] {gate_text}\n\nAssistant:"));
            }
            AttachedDocSegment::FinalCue(text) => {
                out.push_str(text);
            }
        }
    }
    out
}

/// Parse `<tool_call>{...}</tool_call>` marker out of arbitrary model
/// output. Returns `(tool_id, query)` or `None`.
///
/// Mirrors `executor::parse_tool_call` — see comment there. Kept
/// runtime-local rather than pub-cratifying so the executor's tool-call
/// shape can evolve independently if needed.
pub(crate) fn parse_tool_call_inline(text: &str) -> Option<(String, String)> {
    let start = text.find("<tool_call>")?;
    let end = text.find("</tool_call>")?;
    if end <= start {
        return None;
    }
    let json_str = &text[start + "<tool_call>".len()..end];
    let v: serde_json::Value = serde_json::from_str(json_str.trim()).ok()?;
    let tool_id = v.get("tool")?.as_str()?.to_string();
    let query = v.get("query")?.as_str()?.to_string();
    Some((tool_id, query))
}

/// Best-effort truncation for narration chips — clamps `s` to `max`
/// chars and adds an ellipsis when clipped. Keeps the desktop chip
/// from breaking layout on long search queries while preserving the
/// full query in the conversation prompt.
pub(crate) fn truncate_for_chip(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(max).collect();
        format!("{head}…")
    }
}
