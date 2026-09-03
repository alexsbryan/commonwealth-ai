// SPDX-License-Identifier: AGPL-3.0-or-later
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

use crate::types::{Message, Role};

/// Per-chunk content budget inside the prompt-context block. 2000
/// chars ≈ 500 tokens. Used by [`truncate_chunk_content`].
///
/// Was 600 until 2026-08-10, a figure fitted to a ~530-char chunk
/// geometry. Measured against the chaos-saltgrass dev bank (typical
/// chunk 1550-2050 chars), head-600 truncation showed the synthesis
/// model only ~30% of every admitted chunk, and on 3 of the bank's 8
/// stable competence misses the gold answer sat wholly beyond the cut
/// (`boat-hook` at char 674, `logwood` at 1533, `salt barrel` at
/// 1413) — the model truthfully reported "the passages do not state
/// ..." about text retrieval had already paid for. 2000 seats a whole
/// typical corpus chunk while still guarding the budget against a
/// single pathological multi-kilobyte chunk. Raised in the same
/// commit as `MAX_KNOWLEDGE_CHARS`/`EXPANDED_KNOWLEDGE_CHARS`:
/// raising the per-chunk seat under a fixed total budget would have
/// evicted tail chunks instead (the gold carriers on those same
/// misses sat at pool ranks 9-11), so the cap and the budgets move
/// together.
pub(crate) const MAX_CHUNK_CHARS: usize = 2000;

#[cfg(test)]
mod prompt_budget_constants {
    //! The per-chunk seat and the two knowledge budgets are ONE decision, and
    //! this test is where that decision is enforced.
    //!
    //! It exists because the paragraph that used to carry the rule went stale
    //! in the worst available way. `SYSTEM_OVERVIEW.md` still says
    //! `text_utils::MAX_CHUNK_CHARS = 600` and still says *"Do NOT fix this
    //! class by raising MAX_CHUNK_CHARS"* — months after the constant was
    //! deliberately raised to 2000 on bench evidence (see its doc comment).
    //! A session that obeyed the doc would have been undoing a measurement,
    //! and the docs-gate could not see it: that gate checks whether cited
    //! paths and symbols EXIST, never whether the claim about them still
    //! holds. Prose cannot fail loudly. This can.
    //!
    //! Three constants, three modules, one budget. Under a fixed total,
    //! raising the per-chunk seat alone evicts tail chunks instead of showing
    //! more of each — which is why they are asserted as a triple rather than
    //! one at a time.
    use super::MAX_CHUNK_CHARS;
    use crate::runtime::formatters::MAX_KNOWLEDGE_CHARS;
    use crate::runtime::prompts::EXPANDED_KNOWLEDGE_CHARS;

    #[test]
    fn the_prompt_budget_triple_moves_together_or_not_at_all() {
        assert_eq!(
            (
                MAX_CHUNK_CHARS,
                MAX_KNOWLEDGE_CHARS,
                EXPANDED_KNOWLEDGE_CHARS
            ),
            (2000, 24000, 56000),
            "prompt-budget constants changed. They are a measured triple, not \
             three preferences: the per-chunk seat decides how much of each \
             admitted chunk the synthesis model reads, and the two knowledge \
             budgets decide how many chunks fit. Raising one alone trades \
             breadth for depth silently. Change them together, with a bench \
             run behind it, and update this tuple in the same commit."
        );
    }
}

pub(crate) use crate::time::unix_now as now;

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
/// prompt block. `max_turns` bounds how many messages are visible;
/// `chars_per_msg_fn` is invoked per message with that message's
/// **age** (0 = newest visible, growing) and returns the body cap.
///
/// The age-aware shape (see `crate::runtime::chars_for_message_age`)
/// keeps recent turns high-fidelity for coreference while compressing
/// older turns. Callers that want the legacy uniform behaviour pass a
/// closure that ignores age and returns a constant.
///
/// Returns `None` when there's nothing worth saying (single in-flight
/// user message + no compacted preamble).
pub(crate) fn format_conversation_history(
    messages: &[Message],
    max_turns: usize,
    chars_per_msg_fn: impl Fn(usize) -> usize,
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
    let slice = if cap == 0 {
        &[][..]
    } else {
        &messages[start..cap]
    };

    let mut sections: Vec<String> = Vec::new();
    if let Some(preamble) = compacted_preamble.map(str::trim).filter(|s| !s.is_empty()) {
        sections.push(format!("Earlier in the conversation:\n{preamble}"));
    }
    let mut lines = vec!["Prior conversation (most recent last):".to_string()];
    let slice_len = slice.len();
    for (i, m) in slice.iter().enumerate() {
        let label = match m.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::System => "SYSTEM",
        };
        let body = m.content.trim();
        if body.is_empty() {
            continue;
        }
        // Age = how many turns back from the newest visible message.
        // slice is rendered oldest-first; i=0 is the oldest visible,
        // i=slice_len-1 is the newest. Newest visible has age 0.
        let age = slice_len.saturating_sub(1).saturating_sub(i);
        let trimmed = truncate_with_ellipsis(body, chars_per_msg_fn(age));
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
        assert!(format_conversation_history(&msgs, 8, |_| 500, None).is_none());
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
        let out = format_conversation_history(&msgs, 8, |_| 500, Some("   ")).unwrap();
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
        let msgs = vec![msg(Role::User, "q"), msg(Role::Assistant, "a")];
        let out = format_conversation_history(&msgs, 8, |_| 500, None).unwrap();
        assert!(out.contains("USER: q"));
        assert!(out.contains("ASSISTANT: a"));
    }

    /// Per-message budget caps each rendered line (ellipsis appended).
    #[test]
    fn per_message_budget_truncates() {
        let long = "x".repeat(800);
        let msgs = vec![msg(Role::User, "q"), msg(Role::Assistant, &long)];
        let out = format_conversation_history(&msgs, 8, |_| 100, None).unwrap();
        assert!(out.contains("ASSISTANT:"));
        assert!(out.contains("..."));
    }

    /// Age-aware truncation: recent visible turns keep more body
    /// than older ones. Drives the marathon-graceful behaviour where
    /// the user's most recent exchange stays high-fidelity in the
    /// prompt while turns 4+ ago compress.
    ///
    /// Uses ASCII sentinels chosen to be absent from the header text
    /// "Prior conversation (most recent last):" and the role labels
    /// "USER:" / "ASSISTANT:" — so the per-char count exactly matches
    /// the truncated body. `truncate_with_ellipsis` is *byte*-based,
    /// so single-byte ASCII gives clean 1-char = 1-byte semantics.
    #[test]
    fn age_aware_truncation_gives_recent_turns_more_room() {
        // 6 visible turns. Newest is at index 5 (age 0). Each body
        // is 1500 copies of a distinct char so we can identify it
        // after the cut.
        let sentinels = ['b', 'd', 'f', 'g', 'h', 'j'];
        let bodies: Vec<String> = sentinels
            .iter()
            .map(|c| c.to_string().repeat(1500))
            .collect();
        let msgs: Vec<_> = bodies
            .iter()
            .enumerate()
            .map(|(i, body)| {
                msg(
                    if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    body,
                )
            })
            .collect();

        // Age budget shaped like the production tier:
        //   age 0-1 → 1000 chars (newest pair)
        //   age 2-3 → 600 chars (mid pair)
        //   age 4+  → 300 chars (oldest)
        let chars_for_age = |age: usize| match age {
            0..=1 => 1000,
            2..=3 => 600,
            _ => 300,
        };
        let out = format_conversation_history(&msgs, 8, chars_for_age, None)
            .expect("six visible turns should render a history block");

        // Layout: oldest first, newest last. Verify cap held by
        // counting sentinel char occurrences (none in header/labels).
        //   index 0 → b → age 5 → cap 300
        //   index 1 → d → age 4 → cap 300
        //   index 2 → f → age 3 → cap 600
        //   index 3 → g → age 2 → cap 600
        //   index 4 → h → age 1 → cap 1000
        //   index 5 → j → age 0 → cap 1000
        let count_char = |c: char| out.chars().filter(|&x| x == c).count();
        assert_eq!(count_char('b'), 300, "oldest visible (age 5) caps at 300");
        assert_eq!(count_char('d'), 300, "age 4 caps at 300");
        assert_eq!(count_char('f'), 600, "age 3 caps at 600");
        assert_eq!(count_char('g'), 600, "age 2 caps at 600");
        assert_eq!(count_char('h'), 1000, "age 1 caps at 1000");
        assert_eq!(count_char('j'), 1000, "newest (age 0) caps at 1000");
    }

    /// `max_turns` clamps the trailing slice.
    #[test]
    fn max_turns_clamps_slice() {
        let msgs: Vec<_> = (0..10)
            .map(|i| {
                msg(
                    if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    &format!("turn-{i}"),
                )
            })
            .collect();
        // 10 messages, last is U/A pattern; tail-user-elide makes cap=9.
        // max_turns=3 keeps only 3 messages.
        let out = format_conversation_history(&msgs, 3, |_| 500, None).unwrap();
        assert!(out.contains("turn-6") || out.contains("turn-7") || out.contains("turn-8"));
        assert!(!out.contains("turn-0"));
    }
}
