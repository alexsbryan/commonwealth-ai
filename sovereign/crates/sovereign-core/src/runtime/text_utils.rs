//! Stateless text utilities used by the runtime dispatch and prompt-
//! assembly paths.
//!
//! All UTF-8-safe: every truncation helper walks back to a `char`
//! boundary before slicing. The spawned-stream task in
//! `handle_message_stream` is the load-bearing reason — a panic from
//! `&s[..n]` inside that task silently drops the channel and the
//! desktop UI sits inert with no tokens. The Joan Robinson em-dash
//! incident (see `truncate_does_not_panic_inside_multibyte_char`) is
//! the pinned regression.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::types::{Message, Role};

/// Per-chunk content budget inside the prompt-context block. 600
/// chars ≈ 150 tokens — enough for a topical paragraph or a tight
/// passage; smaller would lose context, larger would crowd out other
/// chunks at the merged top-K. Used by [`truncate_chunk_content`].
pub(crate) const MAX_CHUNK_CHARS: usize = 600;

/// Epoch seconds. Centralised here so every persisted `created_at` /
/// `version` field uses the same clock and unit — the rest of the
/// runtime imports `now()` rather than reaching for `SystemTime`
/// directly.
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// UTF-8-safe truncate by character count. Used by the atlas-navigate
/// dedupe key in `prepare_knowledge_query_plan` — slicing by byte
/// offset can panic mid-codepoint on prose containing curly quotes /
/// dashes / accented chars (real SEP content is full of them).
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Emit a `retrieval_audit::pipeline_stage` event listing the top-40
/// (title, corpus_id, score) tuples plus total count at the named
/// stage. Forensic instrument for tracing chunk attrition across
/// the noise floor / cap / reservation / truncate pipeline — see
/// marathon T3 trace (2026-05-18) for the use-case that motivated it.
///
/// Gated behind `SOVEREIGN_FORENSIC=1` so production runs pay no
/// cost. Set the env var on the eval CLI (or daemon, depending on
/// which Runtime instance you want to instrument) to surface the
/// events.
pub(crate) fn audit_pipeline_stage(
    chunks: &[corpus_engine::ScoredChunk],
    stage: &'static str,
    query: &str,
) {
    if std::env::var("SOVEREIGN_FORENSIC").ok().as_deref() != Some("1") {
        return;
    }
    let comp: Vec<(String, String, f32)> = chunks
        .iter()
        .take(40)
        .map(|c| {
            (
                c.corpus_id.clone(),
                c.title.clone().unwrap_or_default(),
                c.score,
            )
        })
        .collect();
    tracing::info!(
        target: "retrieval_audit",
        event = "pipeline_stage",
        stage,
        total = chunks.len(),
        query = %truncate_with_ellipsis(query, 120),
        top40 = ?comp,
        "retrieval_audit: pipeline_stage"
    );
}

/// Truncate `content` to at most `max_bytes`, breaking on a word
/// boundary when possible and appending `"..."`.
///
/// Byte index `max_bytes` may land inside a multi-byte UTF-8 scalar
/// (em-dash `—` is 3 bytes, smart quotes 3 bytes, emoji 4). A naive
/// `&content[..max_bytes]` panics `"byte index N is not a char
/// boundary"`. When that panic fires inside the spawned streaming
/// task the mpsc channel drops with zero tokens emitted and the
/// desktop UI sits inert — exactly the failure mode observed on the
/// Joan Robinson turn after source-expansion started pulling chunks
/// containing em-dashes. Walk backward to the nearest char boundary
/// before slicing; if we also find a word boundary within the
/// remaining content, prefer that for readability.
pub(crate) fn truncate_with_ellipsis(content: &str, max_bytes: usize) -> String {
    if content.len() > max_bytes {
        let mut cut = max_bytes;
        while cut > 0 && !content.is_char_boundary(cut) {
            cut -= 1;
        }
        let truncated = &content[..cut];
        match truncated.rfind(' ') {
            Some(pos) => format!("{}...", &truncated[..pos]),
            None => format!("{truncated}..."),
        }
    } else {
        content.to_string()
    }
}

/// Shorthand for the prompt-context truncation budget.
pub(crate) fn truncate_chunk_content(content: &str) -> String {
    truncate_with_ellipsis(content, MAX_CHUNK_CHARS)
}

/// Build the "Current date: YYYY-MM-DD" anchor + recency-reasoning
/// discipline appended to system prompts. Without it the model
/// presents 2024-era predictions of "early 2026" launches as
/// still future-looking even when today is May 2026 — because the
/// prompt never told it what year it was.
///
/// Discipline is SHAPE-level per `feedback_no_teaching_to_test.md` —
/// no bank-derived examples, just general date-reasoning guidance:
/// compare source dates to today, flag stale predictions, don't
/// present them as still future-looking.
///
/// Split out as a free function so it can be unit-tested without
/// constructing a full `Runtime` (which needs InferenceProvider +
/// StateStore + half the dependency graph).
pub(crate) fn today_anchor_block(today_iso: &str) -> String {
    format!(
        "Current date: {today_iso}. When evaluating retrieved or \
         user-provided sources, compare their publication date or \
         context to today's date. If a source predicts an event for a \
         date that has already passed, do NOT present that prediction \
         as still future-looking — either the event happened (look \
         for more recent confirming sources before asserting it) or \
         the prediction was wrong (say so)."
    )
}

/// Render a trailing slice of conversation history into a system-
/// prompt block. `max_turns` and `chars_per_msg` are caller-supplied
/// so callers can tune for the synthesis vs. compaction paths.
/// Returns `None` when there's nothing worth saying (single in-flight
/// user message + no compacted preamble).
pub(crate) fn format_conversation_history(
    messages: &[Message],
    max_turns: usize,
    chars_per_msg: usize,
    compacted_preamble: Option<&str>,
) -> Option<String> {
    if messages.len() < 2 && compacted_preamble.is_none() {
        return None;
    }
    // Drop the trailing in-flight user message (`handle_message_stream`
    // pushes it onto `context.conversation.messages` before calling
    // synthesis). If the tail isn't a user message we keep everything.
    let cap = if matches!(messages.last(), Some(m) if m.role == Role::User) {
        messages.len().saturating_sub(1)
    } else {
        messages.len()
    };
    let start = cap.saturating_sub(max_turns);
    let slice = if cap == 0 { &[][..] } else { &messages[start..cap] };

    let mut sections: Vec<String> = Vec::new();
    if let Some(preamble) = compacted_preamble.map(str::trim).filter(|s| !s.is_empty()) {
        sections.push(format!("Earlier in the conversation:\n{preamble}"));
    }
    let mut lines = vec!["Prior conversation (most recent last):".to_string()];
    for m in slice {
        let label = match m.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::System => "SYSTEM",
        };
        let body = m.content.trim();
        if body.is_empty() {
            continue;
        }
        let trimmed = truncate_with_ellipsis(body, chars_per_msg);
        lines.push(format!("{label}: {trimmed}"));
    }
    if lines.len() > 1 {
        sections.push(lines.join("\n"));
    }
    if sections.is_empty() {
        return None;
    }
    Some(sections.join("\n\n"))
}

#[cfg(test)]
mod truncate_chunk_tests {
    use super::{truncate_chunk_content, MAX_CHUNK_CHARS};

    /// Em-dash (U+2014, 3 bytes as UTF-8) placed so its first byte
    /// lands inside the truncation window and the char straddles the
    /// `MAX_CHUNK_CHARS` boundary. Naive `&content[..MAX_CHUNK_CHARS]`
    /// panics with "byte index N is not a char boundary"; the fixed
    /// helper must walk back to the last char boundary.
    #[test]
    fn truncate_does_not_panic_inside_multibyte_char() {
        let a_block = "a".repeat(MAX_CHUNK_CHARS - 1); // byte 0..=598
        // Inject em-dash at byte 598..601 so byte 600 lands inside it.
        let content = format!("{a_block}—tail");
        let out = truncate_chunk_content(&content);
        assert!(out.ends_with("..."), "should have truncation marker");
        // The slice must have stopped at or before byte 598 (start of
        // the em-dash), so the em-dash itself is excluded.
        assert!(
            !out.contains('—'),
            "em-dash straddling boundary must be dropped, not split"
        );
    }

    /// Smart double-quote (U+201C/U+201D, 3 bytes) at the boundary:
    /// same class of failure as em-dash. Belt-and-suspenders test.
    #[test]
    fn truncate_handles_smart_quote_at_boundary() {
        let a_block = "a".repeat(MAX_CHUNK_CHARS - 2);
        let content = format!("{a_block}“word”tail");
        let out = truncate_chunk_content(&content);
        assert!(out.ends_with("..."));
    }

    /// Content shorter than the limit: returned verbatim, no marker.
    #[test]
    fn truncate_passthrough_when_short() {
        let content = "Joan Robinson was an economist.";
        assert_eq!(truncate_chunk_content(content), content);
    }

    /// ASCII-only content at the exact boundary length: no truncation.
    #[test]
    fn truncate_at_exact_boundary_no_marker() {
        let content = "a".repeat(MAX_CHUNK_CHARS);
        let out = truncate_chunk_content(&content);
        assert_eq!(out.len(), MAX_CHUNK_CHARS);
        assert!(!out.ends_with("..."));
    }
}

#[cfg(test)]
mod truncate_chars_tests {
    use super::truncate_chars;

    /// The atlas-navigate dedupe key feeds this real prose (em-dashes,
    /// smart quotes). `&s[..n]` would panic mid-codepoint; the
    /// char-iterator form must produce a clean prefix.
    #[test]
    fn handles_multibyte_chars_without_panic() {
        let s = "Joan Robinson — economist — “theory of employment”";
        let out = truncate_chars(s, 20);
        assert_eq!(out.chars().count(), 20);
        // Returned value is itself a valid UTF-8 string.
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn ascii_truncation_is_byte_for_byte_prefix() {
        let s = "the quick brown fox";
        assert_eq!(truncate_chars(s, 9), "the quick");
    }

    #[test]
    fn limit_at_or_above_length_returns_full_string() {
        let s = "short";
        assert_eq!(truncate_chars(s, 100), "short");
        assert_eq!(truncate_chars(s, 5), "short");
    }

    #[test]
    fn zero_limit_returns_empty() {
        assert_eq!(truncate_chars("anything", 0), "");
    }
}

#[cfg(test)]
mod format_conversation_history_tests {
    use super::format_conversation_history;
    use crate::types::{Message, Role};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            id: "id".into(),
            conversation_id: "conv".into(),
            role,
            content: content.into(),
            created_at: 0,
            metadata: None,
            version: 0,
        }
    }

    /// Single trailing user message with no preamble = nothing worth
    /// rendering (it's the in-flight turn that synthesis is about to
    /// answer; including it would be redundant).
    #[test]
    fn single_trailing_user_returns_none() {
        let msgs = vec![msg(Role::User, "hello")];
        assert!(format_conversation_history(&msgs, 8, 500, None).is_none());
    }

    /// Empty preamble (whitespace-only) is treated as absent — the
    /// `Earlier in the conversation:` header should not appear with
    /// no body.
    #[test]
    fn whitespace_preamble_does_not_render_header() {
        let msgs = vec![
            msg(Role::User, "first"),
            msg(Role::Assistant, "second"),
            msg(Role::User, "in-flight"),
        ];
        let out = format_conversation_history(&msgs, 8, 500, Some("   ")).unwrap();
        assert!(!out.contains("Earlier in the conversation"));
        assert!(out.contains("USER: first"));
        assert!(out.contains("ASSISTANT: second"));
        // The in-flight tail user must NOT appear.
        assert!(!out.contains("in-flight"));
    }

    /// Trailing message that's NOT user (assistant-trailing replay) is
    /// kept in full — there's no in-flight turn to elide.
    #[test]
    fn assistant_trailing_is_kept() {
        let msgs = vec![
            msg(Role::User, "q"),
            msg(Role::Assistant, "a"),
        ];
        let out = format_conversation_history(&msgs, 8, 500, None).unwrap();
        assert!(out.contains("USER: q"));
        assert!(out.contains("ASSISTANT: a"));
    }

    /// Per-message budget caps each rendered line (ellipsis appended).
    #[test]
    fn per_message_budget_truncates() {
        let long = "x".repeat(800);
        let msgs = vec![
            msg(Role::User, "q"),
            msg(Role::Assistant, &long),
        ];
        let out = format_conversation_history(&msgs, 8, 100, None).unwrap();
        assert!(out.contains("ASSISTANT:"));
        assert!(out.contains("..."));
    }

    /// `max_turns` clamps the trailing slice.
    #[test]
    fn max_turns_clamps_slice() {
        let msgs: Vec<_> = (0..10)
            .map(|i| msg(
                if i % 2 == 0 { Role::User } else { Role::Assistant },
                &format!("turn-{i}"),
            ))
            .collect();
        // 10 messages, last is U/A pattern; tail-user-elide makes cap=9.
        // max_turns=3 keeps only 3 messages.
        let out = format_conversation_history(&msgs, 3, 500, None).unwrap();
        assert!(out.contains("turn-6") || out.contains("turn-7") || out.contains("turn-8"));
        assert!(!out.contains("turn-0"));
    }
}
