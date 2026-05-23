//! Terminal rendering helpers — translate the desktop's
//! `RoutingMeta.svelte` / `parse-message.ts` conventions into
//! plain-text for the CLI.
//!
//! Stays pure: every helper is `(input) -> String` so tests don't
//! need a Runtime. The ask / session / show commands pipe their
//! persisted-message metadata through these functions.

use std::fmt::Write;

/// Parse `<think>...</think>` blocks out of a raw assistant message,
/// returning `(reasoning_blocks, visible_body)`. The desktop does
/// this client-side in `parse-message.ts`; mirroring it here means
/// the CLI shows exactly the same split without re-routing through
/// the daemon.
pub fn split_reasoning(raw: &str) -> (Vec<String>, String) {
    let mut reasoning = Vec::new();
    let mut visible = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<think>") {
        visible.push_str(&rest[..start]);
        let after_open = &rest[start + "<think>".len()..];
        match after_open.find("</think>") {
            Some(end) => {
                let block = after_open[..end].trim();
                if !block.is_empty() {
                    reasoning.push(block.to_string());
                }
                rest = &after_open[end + "</think>".len()..];
            }
            None => {
                // Unterminated reasoning block. Treat the remainder
                // as reasoning and stop — matches what the desktop
                // does when the model cuts off mid-think.
                let tail = after_open.trim();
                if !tail.is_empty() {
                    reasoning.push(tail.to_string());
                }
                rest = "";
            }
        }
    }
    visible.push_str(rest);
    (reasoning, visible.trim().to_string())
}

/// Render the one-line provenance header the desktop shows above
/// the reasoning disclosure. Reads the `sources` array and the
/// `total_latency_ms` field out of the message metadata.
///
/// Returns an empty string when no provenance is present — keeps
/// callers simple (just `println!("{}", header)` unconditionally).
pub fn provenance_header(metadata: Option<&serde_json::Value>) -> String {
    let Some(meta) = metadata else { return String::new() };
    let Some(prov) = meta.get("provenance") else { return String::new() };

    let sources = prov
        .get("sources")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("origin").and_then(|o| o.as_str()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let latency = prov
        .get("total_latency_ms")
        .and_then(|l| l.as_u64())
        .unwrap_or(0);

    let mut out = String::new();
    if sources.is_empty() {
        let _ = write!(out, "Searched (nothing)");
    } else {
        let _ = write!(out, "Searched {}", sources.join(", "));
    }
    if latency > 0 {
        let _ = write!(out, " · {:.1}s", latency as f64 / 1000.0);
    }
    if let Some(intent) = prov.get("intent").and_then(|i| i.as_str()) {
        let _ = write!(out, " · {intent}");
    }
    if let Some(backend) = prov.get("inference_backend").and_then(|b| b.as_str()) {
        let _ = write!(out, " · {backend}");
    }
    out
}

/// Render the retrieved-chunks footer as a numbered list. Each
/// entry follows the shape `Runtime::prepare_knowledge_context`
/// emits: `{title, corpus_id, url, snippet, provenance_tier}`.
///
/// Empty string when metadata is absent or the retrieved-chunks
/// array is empty. The desktop tucks this behind a disclosure
/// triangle; the CLI unfolds it unconditionally because the whole
/// point is diagnostic visibility.
pub fn retrieved_chunks_footer(metadata: Option<&serde_json::Value>) -> String {
    let Some(meta) = metadata else { return String::new() };
    let Some(chunks) = meta.get("retrieved_chunks").and_then(|c| c.as_array()) else {
        return String::new();
    };
    if chunks.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "--- sources ({}) ---", chunks.len());
    for (i, c) in chunks.iter().enumerate() {
        let title = c.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let corpus_id = c.get("corpus_id").and_then(|s| s.as_str()).unwrap_or("?");
        let url = c.get("url").and_then(|s| s.as_str());
        let snippet = c.get("snippet").and_then(|s| s.as_str()).unwrap_or("");
        let tier = c
            .get("provenance_tier")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let head = if title.is_empty() {
            corpus_id.to_string()
        } else {
            format!("{title} — {corpus_id}")
        };
        let _ = writeln!(out, "[{n:>2}] {head}  [{tier}]", n = i + 1);
        if let Some(u) = url {
            let _ = writeln!(out, "     {u}");
        }
        if !snippet.is_empty() {
            let _ = writeln!(out, "     {snippet}");
        }
    }
    out
}

/// Render a reasoning block as a collapsed or expanded section,
/// depending on `show_reasoning`.
///
/// Collapsed: one-line summary showing block count + char count —
///   mirrors the desktop's `▶ REASONING` disclosure handle.
/// Expanded: full block content wrapped in `> ` quote markers so
///   it's distinguishable from the visible answer.
pub fn render_reasoning(blocks: &[String], show: bool) -> String {
    if blocks.is_empty() {
        return String::new();
    }
    if !show {
        let chars: usize = blocks.iter().map(|b| b.chars().count()).sum();
        return format!(
            "▶ reasoning ({} block{}, {} chars — rerun with --show-reasoning to expand)",
            blocks.len(),
            if blocks.len() == 1 { "" } else { "s" },
            chars
        );
    }
    let mut out = String::new();
    let _ = writeln!(out, "▼ reasoning");
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out, "---");
        }
        for line in b.lines() {
            let _ = writeln!(out, "> {line}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_reasoning_handles_single_block() {
        let (think, visible) = split_reasoning(
            "<think>Reasoning trace goes here</think>The actual answer.",
        );
        assert_eq!(think, vec!["Reasoning trace goes here".to_string()]);
        assert_eq!(visible, "The actual answer.");
    }

    #[test]
    fn split_reasoning_handles_multiple_blocks() {
        let (think, visible) = split_reasoning(
            "before<think>one</think>middle<think>two</think>after",
        );
        assert_eq!(think.len(), 2);
        assert_eq!(visible, "beforemiddleafter");
    }

    #[test]
    fn split_reasoning_tolerates_unterminated() {
        let (think, visible) = split_reasoning("head<think>cut off...");
        assert_eq!(think, vec!["cut off...".to_string()]);
        assert_eq!(visible, "head");
    }

    #[test]
    fn provenance_header_includes_sources_and_latency() {
        let meta = json!({
            "provenance": {
                "sources": [
                    { "origin": "conversation-history", "count": 2 },
                    { "origin": "folder-9ef2f912ea2e", "count": 3 }
                ],
                "total_latency_ms": 11_100,
                "intent": "KnowledgeQuery",
                "inference_backend": "qwen3-8b"
            }
        });
        let h = provenance_header(Some(&meta));
        assert!(h.contains("conversation-history"));
        assert!(h.contains("folder-9ef2f912ea2e"));
        assert!(h.contains("11.1s"));
        assert!(h.contains("KnowledgeQuery"));
        assert!(h.contains("qwen3-8b"));
    }

    #[test]
    fn provenance_header_handles_empty_sources() {
        let meta = json!({
            "provenance": {
                "sources": [],
                "total_latency_ms": 2_000
            }
        });
        let h = provenance_header(Some(&meta));
        assert!(h.contains("nothing"));
        assert!(h.contains("2.0s"));
    }

    #[test]
    fn retrieved_chunks_footer_numbers_entries() {
        let meta = json!({
            "retrieved_chunks": [
                {
                    "title": "The Prince",
                    "corpus_id": "folder-abc",
                    "url": null,
                    "snippet": "About political history with Antoninus...",
                    "provenance_tier": "corpus"
                }
            ]
        });
        let f = retrieved_chunks_footer(Some(&meta));
        assert!(f.contains("sources (1)"));
        assert!(f.contains("The Prince — folder-abc"));
        assert!(f.contains("[corpus]"));
        assert!(f.contains("Antoninus"));
    }

    #[test]
    fn render_reasoning_collapsed_counts_chars() {
        let blocks = vec!["abc".to_string(), "defg".to_string()];
        let s = render_reasoning(&blocks, false);
        assert!(s.contains("2 blocks"));
        assert!(s.contains("7 chars"));
    }

    #[test]
    fn render_reasoning_expanded_quotes_each_line() {
        let blocks = vec!["line one\nline two".to_string()];
        let s = render_reasoning(&blocks, true);
        assert!(s.contains("> line one"));
        assert!(s.contains("> line two"));
    }
}
