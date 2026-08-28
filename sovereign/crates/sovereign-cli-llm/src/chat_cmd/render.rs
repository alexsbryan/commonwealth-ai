// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let Some(meta) = metadata else {
        return String::new();
    };
    let Some(prov) = meta.get("provenance") else {
        return String::new();
    };

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
    let Some(meta) = metadata else {
        return String::new();
    };
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
        let (think, visible) =
            split_reasoning("<think>Reasoning trace goes here</think>The actual answer.");
        assert_eq!(think, vec!["Reasoning trace goes here".to_string()]);
        assert_eq!(visible, "The actual answer.");
    }

    #[test]
    fn split_reasoning_handles_multiple_blocks() {
        let (think, visible) =
            split_reasoning("before<think>one</think>middle<think>two</think>after");
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

/// Per-segment provenance of the released answer — `NATIVE_GROUNDING.md`
/// §6's `answer_segments`, rendered for a terminal.
///
/// Empty string when the field is absent — since the 2026-08-11 flip
/// that means a turn that opted out (`SOVEREIGN_NATIVE_GROUNDING=0`) or
/// found no instrument, not the common case. Absent and empty are NOT the same
/// thing and are not rendered the same way: a turn that segmented and
/// found nothing prints a zero-segment header, a turn that never
/// segmented prints nothing at all (ARCH §18.3).
///
/// **This renders provenance, not a verdict.** A `sourced` segment means
/// the sentence was located verbatim inside one passage. It does not
/// mean a judge agreed the claim is true — the resolver certifies at
/// 0.7429 precision against the incumbent judge
/// (`sovereign/bench/calibration/resolver-precision/FINDINGS.md`), which
/// is why the labels talk about WHERE TEXT IS and never about whether it
/// is right.
pub fn answer_segments_footer(metadata: Option<&serde_json::Value>) -> String {
    let Some(meta) = metadata else {
        return String::new();
    };
    let Some(segs) = meta.get("answer_segments") else {
        return String::new();
    };
    if segs.is_null() {
        return String::new();
    }
    let Some(segs) = segs.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    let _ = writeln!(out, "--- provenance ({} segments) ---", segs.len());
    for (i, s) in segs.iter().enumerate() {
        let kind = s
            .get("kind")
            .and_then(|k| k.get("kind"))
            .and_then(|k| k.as_str());
        // The label vocabulary has ONE definition, in
        // `native_grounding::segments::render_segment_label`; these are
        // its wire spellings, matched here because the CLI reads JSON
        // rather than the enum.
        let label = match kind {
            Some("grounded") => "sourced",
            Some("parametric") => "model's own",
            Some("inference") => "words in sources, no single passage",
            Some("unverified") => "not found in sources",
            // A kind this build does not know is reported as unknown
            // rather than silently rendered as one of the four.
            Some(other) => other,
            None => "unreadable segment",
        };
        // The openable handle when the runtime resolved one; the pool
        // slot otherwise. A grounded segment whose address did not
        // resolve says so — the P1 citability bar is a count of badges
        // that resolve, so a footer that printed a slot number as if it
        // were an address would hide the misses it exists to show.
        let addr = match s.get("kind").and_then(|k| k.get("address")) {
            Some(a) if !a.is_null() => {
                let corpus = a.get("corpus_id").and_then(|c| c.as_str()).unwrap_or("?");
                let chunk = a.get("chunk_id").and_then(|c| c.as_u64());
                match chunk {
                    Some(c) => format!(" [{corpus}#{c}]"),
                    None => format!(" [{corpus}]"),
                }
            }
            _ if kind == Some("grounded") => " [no openable address]".to_string(),
            _ => String::new(),
        };
        let _ = writeln!(out, "  {:>2}. {label}{addr}", i + 1);
    }
    out
}

// ─── Typed renderers — the same output, from the wire instead of the store ───
//
// `provenance_header` and `retrieved_chunks_footer` above read the PERSISTED
// metadata blob, which only a process holding the store can produce. Phase 6
// makes `svrn chat` a client of the daemon's turn surface, and a client is
// handed `TurnFrame::Complete` — typed `Provenance` + `Citation` values that
// crossed a socket. These render the identical text from those.
//
// The blob readers are kept, not deleted: `bench`, `eval` and the inner-chaos
// harness still run turns in-process and still have a store to read. The two
// are one decider in the sense that matters (§10.6) because the typed values
// are PROJECTED from the same blob by
// `sovereign_contracts::types::projection` — there is one parse of the
// metadata shape, and these two functions are two renderings of its output,
// not two interpretations of the blob.

/// The provenance header, from the typed frame.
///
/// `routing_tier` is the wire's name for what the blob reader prints as
/// `intent` — the projection prefers `coarse_intent` and falls back to
/// `intent`, so the rendered string matches.
pub fn provenance_header_typed(
    provenance: Option<&sovereign_contracts::types::projection::Provenance>,
) -> String {
    let Some(prov) = provenance else {
        return String::new();
    };
    let mut out = String::new();
    let sources: Vec<&str> = prov.sources.iter().map(|s| s.origin.as_str()).collect();
    if sources.is_empty() {
        let _ = write!(out, "Searched (nothing)");
    } else {
        let _ = write!(out, "Searched {}", sources.join(", "));
    }
    if let Some(ms) = prov.total_ms.filter(|m| *m > 0) {
        let _ = write!(out, " · {:.1}s", ms as f64 / 1000.0);
    }
    if let Some(tier) = prov.routing_tier.as_deref() {
        let _ = write!(out, " · {tier}");
    }
    if !prov.inference_backend.is_empty() {
        let _ = write!(out, " · {}", prov.inference_backend);
    }
    out
}

/// The sources footer, from the typed frame.
///
/// `url` and `provenance_tier` are `Option` on [`Citation`] because they were
/// added in phase 6 for exactly this call site — the CLI's footer exists for
/// diagnostic visibility, and converting the host to a client while silently
/// dropping two of its five columns would have been a downgrade wearing a
/// convergence badge.
pub fn citations_footer(citations: &[sovereign_contracts::types::projection::Citation]) -> String {
    if citations.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "--- sources ({}) ---", citations.len());
    for (i, c) in citations.iter().enumerate() {
        let tier = c.provenance_tier.as_deref().unwrap_or("");
        let head = match c.title.as_deref() {
            Some(t) if !t.is_empty() => format!("{t} — {}", c.corpus_id),
            _ => c.corpus_id.clone(),
        };
        let _ = writeln!(out, "[{n:>2}] {head}  [{tier}]", n = i + 1);
        if let Some(u) = c.url.as_deref() {
            let _ = writeln!(out, "     {u}");
        }
        if !c.snippet.is_empty() {
            let _ = writeln!(out, "     {}", c.snippet);
        }
    }
    out
}
