// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation title generation.
//!
//! Two responsibilities:
//!
//! 1. `generate_title_from_messages` — pure "given these messages, produce a
//!    short title" function. Uses the Fast slot, no thinking budget, low
//!    temperature. Strips quotes and punctuation the model likes to add.
//!
//! 2. `try_auto_title` — gated helper that fetches the conversation, decides
//!    whether a title should be generated (no existing title, at least one
//!    full user+assistant exchange), generates one, and persists it via
//!    `ConversationStore::update_conversation_title`.
//!
//! `try_auto_title` is idempotent — calling it on a conversation that already
//! has a title is a no-op that returns `Ok(None)`. Safe to call after every
//! assistant save.

use crate::error::Result;
use crate::slot_policy::Workload;
use crate::traits::{InferenceProvider, StateStore};
use crate::types::{CompletionRequest, Message, Role};

/// Hard cap on the characters we include from a single message so the prompt
/// stays small — title prompts run on the Fast slot and must be cheap.
const MESSAGE_SNIPPET_CHARS: usize = 400;

/// Maximum tokens the model may emit for the title.
///
/// Thinking-enabled models (e.g. Qwen 3.5) default to emitting a `<think>`
/// block even when we pass `think_budget: Some(0)`. If the cap is too low
/// the model runs out of tokens inside the think block and we get back
/// something like `"<think>\n..."` with no actual title. 80 gives modest
/// thinking room plus space for a short title, while still running cheaply
/// on the Fast slot. The sanitizer strips the think block regardless.
const TITLE_MAX_TOKENS: usize = 80;

/// Hard cap on the stored title length (characters, not tokens).
const TITLE_MAX_CHARS: usize = 120;

/// Minimum message count before we generate a title — we want at least one
/// user turn and one assistant turn so the model has something to summarise.
const MIN_MESSAGES_FOR_TITLE: usize = 2;

/// Produce a short conversation title from the first user+assistant exchange.
///
/// Runs on the Fast slot with `think_budget: 0`. The result is post-processed
/// to strip leading/trailing quotes, trailing periods, and any newlines the
/// model likes to add.
pub async fn generate_title_from_messages(
    inference: &dyn InferenceProvider,
    messages: &[Message],
) -> Result<String> {
    // Find the first user and first assistant message — not necessarily
    // messages[0] and messages[1], though in practice they usually are.
    let user_msg = messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .unwrap_or("");
    let assistant_msg = messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .map(|m| m.content.as_str())
        .unwrap_or("");

    let user_snippet = truncate_to_char_boundary(user_msg, MESSAGE_SNIPPET_CHARS);
    let assistant_snippet = truncate_to_char_boundary(assistant_msg, MESSAGE_SNIPPET_CHARS);

    // Show, don't tell. A reasoning-distilled Fast model treats verbose prose
    // rules as *content* — given a paragraph of "do not do X", it echoes the
    // rules back as a bulleted outline instead of obeying them. So the prompt
    // is almost pure demonstration: two worked (conversation → Title:) pairs
    // and a one-line instruction. The model pattern-matches the shape — a short
    // title on the line after `Title:` — rather than narrating the task. Paired
    // with `enable_thinking(false)` below, which stops the chain-of-thought at
    // the chat-template level (not just the sampler).
    let prompt = format!(
        "Give each conversation a short title of a few words.\n\n\
         User: How do I center a div horizontally and vertically in CSS?\n\
         Assistant: Use flexbox on the parent: display:flex with justify-content \
         and align-items both set to center.\n\
         Title: Centering a div with flexbox\n\n\
         User: What were the main causes of the French Revolution?\n\
         Assistant: A fiscal crisis, the inequitable estate system, Enlightenment \
         ideas, and food scarcity all converged.\n\
         Title: Causes of the French Revolution\n\n\
         User: {user_snippet}\n\n\
         Assistant: {assistant_snippet}"
    );

    let system_message = "Output only the title — a few words, nothing else.";

    // SLOT_POLICY §3 Housekeep: title generation is advisory turn-loop
    // hygiene. Bundle supplies latency=Fast + think=0; the honest
    // 80-token budget is the FastShort gate.
    let mut request = Workload::Housekeep
        .request(prompt)
        .with_system(system_message)
        .with_output_budget(TITLE_MAX_TOKENS as u32);
    request.temperature = Some(0.3);
    // Hard-off the reasoning scaffold: the distilled Fast model otherwise
    // narrates its plan ("the user wants a title…") and that becomes the
    // output. `enable_thinking(false)` renders the chat template without the
    // think block — stronger than `think_budget: 0`, which only nudges the
    // sampler and is widely ignored.
    request.enable_thinking = Some(false);
    // Prefill the assistant turn with "Title:" so the model physically
    // continues a title instead of opening with a preamble ("the user
    // wants me to…"). With the few-shot pairs above demonstrating that
    // "Title:" is followed by a few clean words, the prefill leaves the
    // model mid-pattern — the single most reliable way to stop a
    // narration-happy distilled model. The rendered response is only the
    // continuation (the prefix is part of the prompt), so it needs no
    // stripping.
    request.assistant_prefix = Some("Title:".to_string());

    let response = inference.complete(&request).await?;
    let cleaned = sanitize_title(&response.text);

    tracing::debug!(
        title = %cleaned,
        model = %response.model_id,
        latency_ms = response.latency_ms,
        "title: generated"
    );

    Ok(cleaned)
}

/// Gate + generate + persist. Safe to call after every assistant message save.
///
/// - `Ok(None)` when the conversation already has a title, or there are not
///   yet enough messages to generate a meaningful one.
/// - `Ok(Some(title))` when a new title was generated and saved.
/// - `Err(_)` only when the store fails; inference failures are wrapped
///   through `generate_title_from_messages`.
pub async fn try_auto_title(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    conversation_id: &str,
) -> Result<Option<String>> {
    let conversation = store.get_conversation(conversation_id).await?;

    if conversation.title.is_some() {
        tracing::debug!(conversation_id = %conversation_id, "title: already set, skipping");
        return Ok(None);
    }

    if conversation.messages.len() < MIN_MESSAGES_FOR_TITLE {
        tracing::debug!(
            conversation_id = %conversation_id,
            messages = conversation.messages.len(),
            "title: not enough messages yet"
        );
        return Ok(None);
    }

    // Confirm we have at least one user AND one assistant message — otherwise
    // the prompt would be lopsided.
    let has_user = conversation.messages.iter().any(|m| m.role == Role::User);
    let has_assistant = conversation
        .messages
        .iter()
        .any(|m| m.role == Role::Assistant);
    if !has_user || !has_assistant {
        tracing::debug!(
            conversation_id = %conversation_id,
            has_user,
            has_assistant,
            "title: missing one side of the exchange"
        );
        return Ok(None);
    }

    let title = generate_title_from_messages(inference, &conversation.messages).await?;

    if title.is_empty() {
        tracing::warn!(
            conversation_id = %conversation_id,
            "title: generated title was empty after sanitisation, skipping save"
        );
        return Ok(None);
    }

    store
        .update_conversation_title(conversation_id, &title)
        .await?;

    tracing::info!(
        conversation_id = %conversation_id,
        title = %title,
        "title: auto-generated and saved"
    );

    Ok(Some(title))
}

/// The same preamble openers used by `strip_thinking_response` — duplicated
/// here as a const so `sanitize_title` can detect the markdown thinking
/// pattern without calling the full response-stripping path.
const PREAMBLE_OPENERS: &[&str] = &[
    "Thinking Process:",
    "Thinking process:",
    "thinking process:",
    "**Thinking Process",
    "Reasoning Process:",
    "Internal reasoning:",
];

/// Clean up model output into a storable title:
/// - strip `<think>...</think>` blocks (complete and unclosed) — thinking
///   models emit these even when told not to, and truncated thinking caused
///   titles like `"<think>"` to be saved verbatim in a prior trial
/// - detect markdown planning preambles ("Thinking Process:" etc.) and
///   extract the actual title from the tail of the output rather than
///   the preamble header
/// - take the first non-empty line of what's left
/// - strip outer quotes (both straight and curly)
/// - strip trailing period
/// - trim to TITLE_MAX_CHARS at a char boundary
///
/// Returns "" when nothing usable remains; callers treat empty as "skip save".
fn sanitize_title(raw: &str) -> String {
    let after_think = strip_think_blocks(raw);

    // Some Fast-slot models bypass XML thinking and write their planning as
    // visible Markdown ("Thinking Process:" / "Reasoning Process:" etc.).
    // In that case taking the first non-empty line gives us the preamble
    // header — not the title. Instead, scan for an explicit "Title:" prefix
    // first, then fall back to the last non-empty line (models put the
    // actual answer last when using planning preambles).
    let first_line = {
        let trimmed = after_think.trim_start();
        if PREAMBLE_OPENERS.iter().any(|m| trimmed.starts_with(m)) {
            // Try "Title: ..." prefix anywhere in the output first.
            let title_from_prefix = after_think
                .lines()
                .find_map(|l| {
                    let t = l.trim();
                    t.strip_prefix("Title:")
                        .or_else(|| t.strip_prefix("title:"))
                        .map(|rest| rest.trim())
                        .filter(|rest| !rest.is_empty())
                })
                .map(str::to_string);

            if let Some(t) = title_from_prefix {
                t
            } else {
                // Fall back to the last non-empty line — the model puts the
                // actual answer after the planning dump.
                after_think
                    .lines()
                    .map(str::trim)
                    .rfind(|l| !l.is_empty())
                    .unwrap_or("")
                    .to_string()
            }
        } else {
            // Normal case: first non-empty line.
            after_think
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string()
        }
    };

    let mut s = first_line.trim().to_string();

    // Strip a wrapping pair of quotes if the model added them.
    for _ in 0..2 {
        if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
            || (s.starts_with('\u{201C}') && s.ends_with('\u{201D}') && s.len() >= 2)
            || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        {
            s = s
                .chars()
                .skip(1)
                .take(s.chars().count().saturating_sub(2))
                .collect();
            s = s.trim().to_string();
        }
    }

    // Strip trailing period (but not "!" or "?") — it was explicitly disallowed
    // in the prompt, but catch it just in case.
    while s.ends_with('.') {
        s.pop();
    }
    s = s.trim().to_string();

    // Clamp to max chars at a char boundary.
    if s.chars().count() > TITLE_MAX_CHARS {
        s = s.chars().take(TITLE_MAX_CHARS).collect();
    }

    s
}

/// Remove `<think>...</think>` blocks from raw model output.
///
/// Handles three shapes:
/// 1. Complete `<think>X</think>Y` — drop the block, keep `Y`.
/// 2. Unclosed `<think>X` (thinking truncated by max_tokens) — drop from
///    `<think>` to end of string. Whatever came before is kept.
/// 3. No tag — return input unchanged.
///
/// Repeated blocks are all removed. Case-sensitive match on `<think>` /
/// `</think>` since the model families we use emit them in lowercase.
///
/// Public so other modules that parse Fast-slot output can reuse this
/// helper — thinking-enabled models (Qwen 3.5) emit `<think>` blocks even
/// with `think_budget: Some(0)`, so anywhere we pattern-match on the
/// response text needs to strip them first.
pub fn strip_think_blocks(raw: &str) -> String {
    strip_think_blocks_impl(raw)
}

/// Strip a thinking-mode trace from a model response that the user
/// is supposed to see. Handles four observed shapes:
///
/// 1. Standard `<think>X</think>Y` — drops the block, keeps Y.
/// 2. No-opener-but-has-closer `X</think>Y` — happens when the
///    chat template prepended `<think>` to the assistant turn so
///    the opener is in the prompt rather than the output. Take
///    everything after the LAST `</think>`.
/// 3. No tags at all — pass through unchanged.
/// 4. Iter6: markdown-style planning preamble. Some models (notably
///    Qwen3.5-9B-vOP under heavy meta-instruction prompts) bypass
///    the chat template's thinking frame entirely and write their
///    planning as visible Markdown ("Thinking Process:" / "Thinking
///    process:" / "Step 1:" headers). When we see this opener and
///    NO `</think>` close fired, the model often runs out of
///    tokens mid-planning and never gets to a reply. Best we can
///    do post-hoc: drop the preamble and surface whatever follows
///    a clean break. If nothing reply-shaped follows, return the
///    original text — at least the operator can see what the model
///    was trying to do.
///
/// `enable_thinking: true` on Qwen3.x produces shape 2 today; the
/// strip-think helper that's been running in voice-eval is exactly
/// this logic. Reused here so the runtime and the eval surface the
/// same model output.
pub fn strip_thinking_response(raw: &str) -> String {
    if let Some(idx) = raw.rfind("</think>") {
        return raw[idx + "</think>".len()..].trim_start().to_string();
    }

    // Shape 4: markdown planning preamble. Conservative: only fire
    // when the response OPENS with one of the known preamble
    // headers (tolerating leading whitespace) — never strip from a
    // mid-response occurrence, since "Thinking Process:" in normal
    // text is a legitimate thing a witness reply might mention.
    let trimmed = raw.trim_start();
    const PREAMBLE_OPENERS: &[&str] = &[
        "Thinking Process:",
        "Thinking process:",
        "thinking process:",
        "**Thinking Process",
        "Reasoning Process:",
        "Internal reasoning:",
    ];
    let has_preamble = PREAMBLE_OPENERS.iter().any(|m| trimmed.starts_with(m));
    if has_preamble {
        // Try to find a clean break to a reply: a "Final reply:" /
        // "Reply:" / "Response:" delimiter, OR a double-newline
        // followed by text that doesn't look like more planning
        // (no leading number, no leading "**", no leading "*").
        const REPLY_DELIMITERS: &[&str] = &[
            "Final Reply:",
            "Final reply:",
            "Reply:",
            "Response:",
            "Final Response:",
            "Final response:",
        ];
        for delim in REPLY_DELIMITERS {
            if let Some(idx) = trimmed.rfind(delim) {
                let after = trimmed[idx + delim.len()..].trim_start();
                if !after.is_empty() {
                    return after.to_string();
                }
            }
        }
        // No delimiter found — model probably ran out of tokens
        // mid-planning. Return empty so downstream code surfaces a
        // "no reply" failure cleanly rather than a 9KB planning
        // dump in the user-facing message stream.
        return String::new();
    }

    strip_think_blocks_impl(raw)
}

/// Could this leading text still be (the start of) a thinking trace —
/// either a `<think>` block or a markdown planning preamble — and so
/// warrant continued buffering? Used by [`strip_thinking_stream`] to
/// decide when it is safe to STOP buffering and start streaming: a
/// thinking trace always OPENS the output, so once the start cannot
/// become any known marker there is nothing left to strip.
///
/// `trimmed` must already be `trim_start`'d. Returns true while still
/// ambiguous — the buffer is empty, is a prefix of a marker (e.g.
/// `"<th"`, `"Thinking Pro"`), or already opens with one (`"<think>…"`,
/// `"Thinking Process:\n…"`); false once the start is definitively not
/// a thinking trace and should stream verbatim.
///
/// The marker list mirrors `strip_thinking_response`'s `</think>` closer
/// and `PREAMBLE_OPENERS` — keep the two in sync (a marker recognised by
/// the terminal-flush fallback but NOT here would leak its preamble into
/// the stream instead of being buffered and stripped).
fn could_be_thinking_prefix(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return true;
    }
    const MARKERS: &[&str] = &[
        "<think>",
        "Thinking Process:",
        "Thinking process:",
        "thinking process:",
        "**Thinking Process",
        "Reasoning Process:",
        "Internal reasoning:",
    ];
    MARKERS
        .iter()
        .any(|m| m.starts_with(trimmed) || trimmed.starts_with(m))
}

/// Streaming counterpart to [`strip_thinking_response`] for the
/// witness path.
///
/// Given a `Stream<Result<String>>` of incoming chat tokens, returns
/// a `Stream<Result<String>>` that yields the *reply* portion only —
/// i.e. content emitted after the model closes its planning trace
/// with `</think>`. Behaviour:
///
/// * **Buffer phase.** While the output's start could still be a
///   thinking trace — a `<think>` block awaiting its closer, or a
///   markdown preamble (see [`could_be_thinking_prefix`]) — tokens
///   are accumulated into an internal buffer and nothing is emitted.
///   The moment the start proves it is NOT a thinking trace, the
///   strip cuts over to passthrough and streams the buffer, so a
///   thinking-DISABLED answer is never held back as one trailing
///   frame (the 2026-06-26 blank-screen bug).
/// * **Cutover.** When a chunk arrives that completes a `</think>`
///   marker (taking buffer continuity into account, so the marker
///   may straddle chunk boundaries), the strip emits a single
///   chunk consisting of all content *after* that closer (with
///   leading whitespace trimmed) and switches to passthrough mode.
/// * **Passthrough.** Subsequent chunks are forwarded unchanged.
/// * **Stream end without `</think>`.** Falls back to
///   [`strip_thinking_response`] over the entire buffer and emits
///   the result as a single trailing chunk. This handles the
///   markdown-preamble shape (no closer ever emitted) — the user
///   sees the cleaned reply at the end, same UX as today's
///   non-streaming witness path.
///
/// Errors propagate immediately — they're emitted before the
/// terminal flush and the buffer is dropped.
///
/// One known imperfection: if the model emits multiple
/// `</think>...</think>` blocks (uncommon on Qwen3.x witness work
/// but possible), this strip cuts over at the FIRST closer rather
/// than the last — meaning content between subsequent block pairs
/// would leak through. The non-streaming sibling uses `rfind` and
/// gets the LAST closer; we'd need full buffering to match exactly,
/// which would defeat streaming. Live with it; flag if it becomes
/// real.
pub fn strip_thinking_stream<S>(
    inner: S,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = crate::error::Result<String>> + Send>>
where
    S: futures::Stream<Item = crate::error::Result<String>> + Send + 'static,
{
    use futures::StreamExt;
    const CLOSE: &str = "</think>";

    enum Phase {
        Buffering(String),
        Emitting,
        /// Trailing flush has been emitted; stream should now end.
        Done,
    }

    struct State<S> {
        inner: std::pin::Pin<Box<S>>,
        phase: Phase,
    }

    let init = State {
        inner: Box::pin(inner),
        phase: Phase::Buffering(String::new()),
    };

    let s = futures::stream::unfold(init, |mut st| async move {
        loop {
            match st.phase {
                Phase::Done => return None,
                Phase::Emitting => match st.inner.next().await {
                    Some(Ok(chunk)) => return Some((Ok(chunk), st)),
                    Some(Err(e)) => return Some((Err(e), st)),
                    None => return None,
                },
                Phase::Buffering(mut buffer) => match st.inner.next().await {
                    Some(Err(e)) => {
                        // Drop buffer; surface error and end.
                        st.phase = Phase::Done;
                        return Some((Err(e), st));
                    }
                    Some(Ok(chunk)) => {
                        buffer.push_str(&chunk);
                        if let Some(idx) = buffer.find(CLOSE) {
                            let after = buffer[idx + CLOSE.len()..].trim_start().to_string();
                            st.phase = Phase::Emitting;
                            if !after.is_empty() {
                                return Some((Ok(after), st));
                            }
                            // Empty after-tag — keep looping into
                            // Emitting mode to consume next chunk.
                            continue;
                        }
                        // No `</think>` yet. A thinking trace, by definition,
                        // OPENS the output — either a `<think>` block or a
                        // markdown planning preamble (see
                        // `strip_thinking_response`). As soon as the buffer is
                        // enough to prove it is NEITHER, there is nothing to
                        // strip: switch to passthrough and stream NOW.
                        //
                        // Without this, a thinking-DISABLED turn (enable_thinking
                        // = false, so no `<think>` is ever emitted) stays in
                        // Buffering for the whole generation and flushes one
                        // trailing frame at stream close — a blank screen until
                        // the answer is fully generated (2026-06-26 breaker:
                        // creative GenerativeQuery turns + any non-gated
                        // streaming path).
                        if !could_be_thinking_prefix(buffer.trim_start()) {
                            st.phase = Phase::Emitting;
                            return Some((Ok(buffer), st));
                        }
                        // Still ambiguous (buffer is a prefix of a known thinking
                        // marker, or already opens with one) — keep buffering.
                        st.phase = Phase::Buffering(buffer);
                        continue;
                    }
                    None => {
                        // Stream closed without ever seeing </think>.
                        // Fall back to strip_thinking_response over
                        // the full buffer (handles markdown-preamble
                        // shapes) and emit the cleaned text as a
                        // single trailing chunk.
                        st.phase = Phase::Done;
                        if buffer.is_empty() {
                            return None;
                        }
                        let cleaned = strip_thinking_response(&buffer);
                        if cleaned.is_empty() {
                            return None;
                        }
                        return Some((Ok(cleaned), st));
                    }
                },
            }
        }
    });
    Box::pin(s)
}

/// Strip `[Source: ...]` citation markers from a witness reply.
///
/// Why this exists on a code path with no corpus to cite from:
/// the witness/relational register has no system-prompt language
/// inviting citations, no retrieval feeding the prompt, and no
/// downstream UI that renders citations on this surface — yet
/// modern fine-tunes (the 35B Darwin in particular, observed
/// 2026-05-05) sometimes emit `[Source: <something>]` anyway,
/// reaching for the RAG-formatted idiom from their training
/// distribution when asked to ground in "the record." The marker
/// reads to the user as a fabricated citation, which it is.
///
/// We strip the markers post-hoc rather than instructing the prompt
/// to avoid them, because instructing the prompt to avoid citations
/// is itself prompt-noise that primes the model toward citation
/// behavior. The witness contract stays clean of the topic; this
/// transformer cleans up the rare leakage.
///
/// Match shape: literal `[Source:` open, search for the next `]`
/// close within 200 chars of the open (anything longer is treated
/// as not-a-marker — likely real bracketed prose). Trailing space
/// before the marker is also trimmed when the marker was preceded
/// by " ", so `"foo [Source: X]."` becomes `"foo."` instead of
/// `"foo ."`. Other casings (`[source: X]`, `[SOURCE: X]`) and
/// other RAG idioms (e.g. `[1]`, `[citation needed]`) are NOT
/// touched — extend if observed in production.
pub fn strip_source_citations(raw: &str) -> String {
    const OPEN: &str = "[Source:";
    const CLOSE: char = ']';
    const MAX_MARKER_LEN: usize = 200;

    let mut out = String::with_capacity(raw.len());
    let mut remaining = raw;

    loop {
        match remaining.find(OPEN) {
            None => {
                out.push_str(remaining);
                return out;
            }
            Some(open_idx) => {
                // Search for the close within the cap.
                let search_end = (open_idx + MAX_MARKER_LEN).min(remaining.len());
                let after_open = &remaining[open_idx..search_end];
                match after_open.find(CLOSE) {
                    None => {
                        // No close in the cap — bail; emit verbatim.
                        out.push_str(remaining);
                        return out;
                    }
                    Some(rel_close) => {
                        // Strip `[Source: ...]` and an optional single
                        // preceding space so we don't leave a "word ."
                        // seam.
                        let mut emit_end = open_idx;
                        if emit_end > 0 && remaining.as_bytes()[emit_end - 1] == b' ' {
                            emit_end -= 1;
                        }
                        out.push_str(&remaining[..emit_end]);
                        let drop_end = open_idx + rel_close + 1;
                        remaining = &remaining[drop_end..];
                    }
                }
            }
        }
    }
}

/// Streaming counterpart to [`strip_source_citations`].
///
/// Composes after [`strip_thinking_stream`] in the witness streaming
/// path: thinking-tag stripper drops the planning trace; this drops
/// hallucinated citation markers from the reply. Errors propagate.
///
/// Implementation: small state machine with a buffered window.
/// `Normal` emits incoming text minus any trailing partial-prefix of
/// `[Source:`. `InMarker` swallows everything until `]` (capped at
/// 200 chars; if the cap is exceeded the buffer is treated as not-a-
/// marker and emitted verbatim — better to leak a stray `[Source:`
/// than swallow legitimate prose with brackets).
pub fn strip_source_citations_stream<S>(
    inner: S,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = crate::error::Result<String>> + Send>>
where
    S: futures::Stream<Item = crate::error::Result<String>> + Send + 'static,
{
    use futures::StreamExt;
    const OPEN: &str = "[Source:";
    const MAX_MARKER_LEN: usize = 200;

    struct State<S> {
        inner: std::pin::Pin<Box<S>>,
        /// Buffer holds bytes we haven't decided on yet — either the
        /// trailing part of the previous emit that might be the start
        /// of a `[Source:` marker, or the contents of an in-progress
        /// marker.
        buffer: String,
        ended: bool,
    }

    let init = State {
        inner: Box::pin(inner),
        buffer: String::new(),
        ended: false,
    };

    let s = futures::stream::unfold(init, |mut st| async move {
        if st.ended {
            return None;
        }
        loop {
            // 1. Try to drop any complete markers in the buffer.
            if let Some(open_idx) = st.buffer.find(OPEN) {
                let search_end = (open_idx + MAX_MARKER_LEN).min(st.buffer.len());
                if let Some(rel_close) = st.buffer[open_idx..search_end].find(']') {
                    // Complete marker — drop, optionally with one
                    // preceding space.
                    let close_idx = open_idx + rel_close + 1;
                    let mut emit_end = open_idx;
                    if emit_end > 0 && st.buffer.as_bytes()[emit_end - 1] == b' ' {
                        emit_end -= 1;
                    }
                    let to_emit = st.buffer[..emit_end].to_string();
                    st.buffer = st.buffer[close_idx..].to_string();
                    if !to_emit.is_empty() {
                        return Some((Ok(to_emit), st));
                    }
                    // Empty emit — loop to handle next marker / refill.
                    continue;
                }
                if st.buffer.len() - open_idx >= MAX_MARKER_LEN {
                    // Cap exceeded with no close — treat as not-a-marker
                    // and emit verbatim. Real prose with a bracketed
                    // segment longer than 200 chars is rare; the
                    // alternative (swallowing it) is worse.
                    let to_emit = std::mem::take(&mut st.buffer);
                    return Some((Ok(to_emit), st));
                }
                // Incomplete marker — emit content before `[Source:`,
                // hold the rest including any preceding space, pull
                // more. (The space is held alongside the marker so
                // that, when the marker completes, the strip drops
                // both together — preventing a "word ." seam where
                // a "word [Source: X]." was.)
                let mut hold_start = open_idx;
                if hold_start > 0 && st.buffer.as_bytes()[hold_start - 1] == b' ' {
                    hold_start -= 1;
                }
                let to_emit = st.buffer[..hold_start].to_string();
                st.buffer = st.buffer[hold_start..].to_string();
                if !to_emit.is_empty() {
                    return Some((Ok(to_emit), st));
                }
                // No new emit; fall through to pull more input.
            } else {
                // 2. No `[Source:` in buffer. Emit everything except
                //    any tail that could be a partial prefix.
                let safe_end = compute_safe_prefix_end(&st.buffer, OPEN);
                if safe_end > 0 {
                    let to_emit = st.buffer[..safe_end].to_string();
                    st.buffer = st.buffer[safe_end..].to_string();
                    return Some((Ok(to_emit), st));
                }
                // safe_end == 0 means buffer is either empty or
                // entirely a partial prefix. Pull more.
            }

            // 3. Pull next chunk (or end).
            match st.inner.next().await {
                Some(Ok(chunk)) => {
                    st.buffer.push_str(&chunk);
                    continue;
                }
                Some(Err(e)) => {
                    st.ended = true;
                    return Some((Err(e), st));
                }
                None => {
                    // Stream ended; flush whatever's in the buffer
                    // (may include an unclosed `[Source:` — we'd rather
                    // leak the marker than swallow what follows).
                    st.ended = true;
                    if st.buffer.is_empty() {
                        return None;
                    }
                    let last = std::mem::take(&mut st.buffer);
                    return Some((Ok(last), st));
                }
            }
        }
    });
    Box::pin(s)
}

/// Returns the byte index up to which `buffer` is safe to emit
/// without emitting a partial prefix of `marker`. The "unsafe tail"
/// is the longest suffix of `buffer` that is also a prefix of
/// `marker` — that tail must be held back in case the next chunk
/// completes the marker.
///
/// Two extra invariants beyond the basic suffix-as-prefix check, both
/// motivated by the "drop one preceding space when stripping a marker"
/// rule (which keeps `"foo [Source: X]."` from becoming `"foo ."`):
///
/// 1. When the suffix-prefix match is found and the char immediately
///    before the matched suffix is a space, hold the space too.
///    Otherwise the preceding space gets emitted in one round and the
///    marker gets dropped in the next, leaving a stranded space.
/// 2. Always hold back a single trailing whitespace char (space, tab,
///    newline). If the next chunk starts a marker, we want that
///    whitespace available to drop with it. If the next chunk is
///    ordinary content, the held char rejoins the stream cleanly on
///    the next emission. At stream end the held char is flushed.
fn compute_safe_prefix_end(buffer: &str, marker: &str) -> usize {
    if buffer.is_empty() {
        return 0;
    }
    let mut safe = buffer.len();
    let max_overlap = marker.len().saturating_sub(1).min(buffer.len());
    // Suffix-as-prefix check (longest overlap wins).
    for overlap in (1..=max_overlap).rev() {
        let split_at = buffer.len() - overlap;
        if !buffer.is_char_boundary(split_at) {
            continue;
        }
        let tail = &buffer[split_at..];
        if marker.starts_with(tail) {
            safe = safe.min(split_at);
            // Invariant 1: hold back preceding space too.
            if split_at > 0 && buffer.as_bytes()[split_at - 1] == b' ' {
                safe = safe.min(split_at - 1);
            }
            break;
        }
    }
    // Invariant 2: hold back a single trailing whitespace char.
    if safe == buffer.len() {
        let bytes = buffer.as_bytes();
        let last = bytes[bytes.len() - 1];
        if matches!(last, b' ' | b'\t' | b'\n') {
            let new_safe = buffer.len() - 1;
            if buffer.is_char_boundary(new_safe) {
                safe = new_safe;
            }
        }
    }
    safe
}

fn strip_think_blocks_impl(raw: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";

    let mut out = String::with_capacity(raw.len());
    let mut remaining = raw;

    loop {
        match remaining.find(OPEN) {
            Some(open_idx) => {
                // Text before the opening tag is kept.
                out.push_str(&remaining[..open_idx]);
                let after_open = &remaining[open_idx + OPEN.len()..];
                match after_open.find(CLOSE) {
                    Some(close_idx) => {
                        // Complete block — skip over it and continue.
                        remaining = &after_open[close_idx + CLOSE.len()..];
                    }
                    None => {
                        // Unclosed — drop everything from `<think>` to EOF.
                        break;
                    }
                }
            }
            None => {
                out.push_str(remaining);
                break;
            }
        }
    }

    out
}

/// Truncate a string to at most `max` bytes, walking back to a valid UTF-8
/// char boundary so we never split a multi-byte codepoint.
fn truncate_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_straight_quotes() {
        assert_eq!(sanitize_title("\"Hello world\""), "Hello world");
    }

    #[test]
    fn sanitize_strips_curly_quotes() {
        assert_eq!(sanitize_title("\u{201C}Hello world\u{201D}"), "Hello world");
    }

    #[test]
    fn sanitize_strips_trailing_period() {
        assert_eq!(sanitize_title("Hello world."), "Hello world");
    }

    #[test]
    fn sanitize_keeps_question_and_exclaim() {
        assert_eq!(sanitize_title("What is quantum?"), "What is quantum?");
        assert_eq!(sanitize_title("Eureka!"), "Eureka!");
    }

    #[test]
    fn sanitize_takes_first_line_only() {
        assert_eq!(
            sanitize_title("Hello world\nExplanation: this title is about..."),
            "Hello world"
        );
    }

    #[test]
    fn sanitize_empty_yields_empty() {
        assert_eq!(sanitize_title(""), "");
        assert_eq!(sanitize_title("   \n  "), "");
    }

    // ── <think> block handling ──────────────────────────────────

    #[test]
    fn sanitize_strips_complete_think_block() {
        assert_eq!(
            sanitize_title("<think>reasoning here</think>Real Title"),
            "Real Title"
        );
    }

    #[test]
    fn sanitize_strips_unclosed_think_prefix() {
        // The specific failure mode from the Apr 14 trial: max_tokens cut
        // off inside the think block, so no title followed. Must return "".
        assert_eq!(sanitize_title("<think>unfinished thinking"), "");
        assert_eq!(sanitize_title("<think>"), "");
    }

    #[test]
    fn sanitize_handles_multiline_think() {
        let input = "<think>\nlet me think\nabout this\n</think>\n\nThe Title";
        assert_eq!(sanitize_title(input), "The Title");
    }

    #[test]
    fn sanitize_strips_multiple_think_blocks() {
        let input = "<think>first</think>Title<think>second</think>";
        assert_eq!(sanitize_title(input), "Title");
    }

    #[test]
    fn sanitize_preserves_content_before_unclosed_think() {
        // If the model emits a title then opens thinking (odd but possible),
        // keep the pre-think content.
        assert_eq!(
            sanitize_title("Real Title\n<think>postscript reasoning"),
            "Real Title"
        );
    }

    #[test]
    fn sanitize_preserves_angle_brackets_in_title() {
        // Non-<think> angle brackets must survive.
        assert_eq!(sanitize_title("C++ vs <tag>"), "C++ vs <tag>");
    }

    // ── Markdown thinking preamble handling ─────────────────────

    #[test]
    fn sanitize_extracts_title_from_thinking_process_preamble() {
        let input = "Thinking Process:\nLet me analyze...\nStep 1: user asks about X\n\nFree Will and Determinism";
        assert_eq!(sanitize_title(input), "Free Will and Determinism");
    }

    #[test]
    fn sanitize_extracts_title_prefix_from_preamble() {
        let input = "Thinking Process:\nAnalyzing...\nTitle: Exploring Free Will";
        assert_eq!(sanitize_title(input), "Exploring Free Will");
    }

    #[test]
    fn sanitize_preamble_does_not_fire_on_midtext() {
        // "Thinking Process:" not at the start — should use first-line logic.
        let input = "Real Title\nThinking Process: some explanation";
        assert_eq!(sanitize_title(input), "Real Title");
    }

    #[test]
    fn sanitize_reasoning_process_preamble() {
        let input = "Reasoning Process:\nStep 1...\nStep 2...\nPhilosophy of Mind";
        assert_eq!(sanitize_title(input), "Philosophy of Mind");
    }

    #[test]
    fn strip_think_blocks_empty_on_think_only() {
        assert_eq!(strip_think_blocks("<think>only thinking"), "");
        assert_eq!(strip_think_blocks("<think></think>"), "");
    }

    #[test]
    fn strip_think_blocks_no_tags_passthrough() {
        assert_eq!(strip_think_blocks("hello world"), "hello world");
    }

    #[test]
    fn truncate_respects_multibyte_boundary() {
        let s = "Schrödinger's cat";
        // byte position 7 lands inside 'ö' — should walk back.
        let t = truncate_to_char_boundary(s, 7);
        assert!(s.starts_with(t));
        // Valid UTF-8: no panic on str::chars().
        let _ = t.chars().count();
    }

    // ---------------------------------------------------------------
    // strip_thinking_stream — streaming counterpart tests.
    // ---------------------------------------------------------------

    use crate::error::Result as CoreResult;
    use futures::StreamExt;

    fn ok_stream(
        chunks: Vec<&'static str>,
    ) -> impl futures::Stream<Item = CoreResult<String>> + Send + 'static {
        futures::stream::iter(chunks.into_iter().map(|s| Ok(s.to_string())))
    }

    async fn collect(
        stream: std::pin::Pin<Box<dyn futures::Stream<Item = CoreResult<String>> + Send>>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let mut s = stream;
        while let Some(item) = s.next().await {
            out.push(item.expect("stream yielded an error in test"));
        }
        out
    }

    #[tokio::test]
    async fn strip_stream_emits_after_close_tag() {
        // The canonical Qwen3.x witness shape: planning, then </think>,
        // then the reply. Should buffer the planning silently and emit
        // the reply.
        let inner = ok_stream(vec![
            "<think>planning planning</think>\n\n",
            "Hello, ",
            "world.",
        ]);
        let out = collect(strip_thinking_stream(inner)).await;
        // First chunk emitted: everything after </think>, trim_start'd.
        assert_eq!(out[0], "Hello, ");
        // Subsequent chunks pass through.
        assert_eq!(out[1], "world.");
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn strip_stream_handles_close_tag_straddling_chunk_boundary() {
        // </think> arrives split across chunk boundaries — the buffer
        // should accumulate enough to recognise it.
        let inner = ok_stream(vec!["<think>plan</thi", "nk>", "Hi."]);
        let out = collect(strip_thinking_stream(inner)).await;
        assert_eq!(out[0], "Hi.");
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn strip_stream_no_closer_falls_back_to_strip_helper() {
        // Model never emits </think> — we should still surface a
        // cleaned reply at end via strip_thinking_response, matching
        // the non-streaming witness UX. Use the markdown-preamble
        // shape that strip_thinking_response handles via
        // PREAMBLE_OPENERS.
        let inner = ok_stream(vec![
            "Thinking Process:\n\nFirst I\nReply: ",
            "Hello there.",
        ]);
        let out = collect(strip_thinking_stream(inner)).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], "Hello there.");
    }

    #[tokio::test]
    async fn strip_stream_clean_text_with_no_tags_streams_incrementally() {
        // No tags, no preamble (the thinking-DISABLED shape). The first
        // chunk already proves it is not a thinking trace, so the strip
        // must cut over to passthrough IMMEDIATELY and forward each chunk
        // as it arrives — NOT buffer the whole answer and flush one
        // trailing frame (the 2026-06-26 blank-screen bug).
        let inner = ok_stream(vec!["Just a clean ", "witness reply."]);
        let out = collect(strip_thinking_stream(inner)).await;
        assert_eq!(
            out,
            vec!["Just a clean ".to_string(), "witness reply.".to_string()],
            "clean output must stream chunk-by-chunk, not arrive as one frame"
        );
    }

    #[tokio::test]
    async fn strip_stream_long_no_thinking_answer_first_chunk_is_not_last() {
        // Regression for the GenerativeQuery / creative blank-screen: a
        // long thinking-disabled answer must show its FIRST chunk well
        // before the last, i.e. the consumer sees progress immediately.
        let inner = ok_stream(vec![
            "Once upon a time ",
            "there was a houseplant ",
            "who plotted quietly ",
            "in the corner.",
        ]);
        let out = collect(strip_thinking_stream(inner)).await;
        assert_eq!(
            out.len(),
            4,
            "every chunk should pass through as it arrives"
        );
        assert_eq!(out[0], "Once upon a time ");
        assert_eq!(out[3], "in the corner.");
    }

    #[tokio::test]
    async fn strip_stream_markdown_preamble_still_buffers_and_strips() {
        // Guard the fix's boundary: the markdown-preamble thinking shape
        // (no `<think>` tag) must STILL be recognised and stripped — the
        // early-stream cutover must not leak "Thinking Process:" prose.
        let inner = ok_stream(vec!["Thinking Pro", "cess:\nplan plan\nReply: ", "Done."]);
        let out = collect(strip_thinking_stream(inner)).await;
        assert_eq!(
            out.len(),
            1,
            "preamble shape flushes the stripped reply once"
        );
        assert_eq!(out[0], "Done.");
    }

    #[tokio::test]
    async fn strip_stream_propagates_inner_error() {
        let inner = futures::stream::iter(vec![
            Ok::<String, crate::error::Error>("<think>p".to_string()),
            Err(crate::error::Error::Other("boom".into())),
        ]);
        let mut s = strip_thinking_stream(inner);
        // First item should be the error — buffer dropped, no later
        // emissions.
        let first = s.next().await.expect("stream yielded none");
        assert!(first.is_err());
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn strip_stream_empty_after_close_does_not_emit_empty_chunk() {
        // </think> at the very end with nothing after — the empty
        // after-tag string must not be yielded as a no-op chunk.
        let inner = ok_stream(vec!["<think>only planning</think>"]);
        let out = collect(strip_thinking_stream(inner)).await;
        assert!(out.is_empty(), "expected no chunks, got {out:?}");
    }

    // ── Citation stripper ─────────────────────────────────────

    #[test]
    fn citations_drop_marker_and_preceding_space() {
        let raw = "the framework defines projects [Source: Project management] as goal-bound.";
        assert_eq!(
            strip_source_citations(raw),
            "the framework defines projects as goal-bound."
        );
    }

    #[test]
    fn citations_drop_multiple_in_one_paragraph() {
        let raw =
            "Robinson [Source: Joan Robinson] critiqued the model [Source: Cambridge Capital].";
        assert_eq!(strip_source_citations(raw), "Robinson critiqued the model.");
    }

    #[test]
    fn citations_leave_real_brackets_alone() {
        // [1], [citation needed], etc. are not citation-source markers.
        let raw = "Some say so [1], others disagree [citation needed].";
        assert_eq!(strip_source_citations(raw), raw);
    }

    #[test]
    fn citations_no_change_when_clean() {
        let raw = "You said you feel like a chain gang in a coal mine.";
        assert_eq!(strip_source_citations(raw), raw);
    }

    #[test]
    fn citations_unclosed_marker_left_alone() {
        // No `]` within the cap — emit verbatim rather than swallow rest.
        let raw = "weird sentence with [Source: this never closes and just keeps going forever";
        assert_eq!(strip_source_citations(raw), raw);
    }

    #[test]
    fn citations_at_start_of_string() {
        let raw = "[Source: X] is the source.";
        // No preceding space to consume; just drop the marker.
        assert_eq!(strip_source_citations(raw), " is the source.");
    }

    // ── Citation stripper — streaming ─────────────────────────

    #[tokio::test]
    async fn citations_stream_strips_marker_split_across_chunks() {
        // Worst case: marker is fragmented byte-by-byte across chunks.
        let inner = ok_stream(vec![
            "the framework ",
            "[",
            "Source",
            ":",
            " Project",
            " management",
            "]",
            " continues.",
        ]);
        let out = collect(strip_source_citations_stream(inner)).await;
        assert_eq!(out.concat(), "the framework continues.");
    }

    #[tokio::test]
    async fn citations_stream_passthrough_when_no_marker() {
        let inner = ok_stream(vec!["You feel like a chain gang ", "in a coal mine."]);
        let out = collect(strip_source_citations_stream(inner)).await;
        assert_eq!(out.concat(), "You feel like a chain gang in a coal mine.");
    }

    #[tokio::test]
    async fn citations_stream_holds_partial_prefix_until_resolved() {
        // Buffer ends with `[` — need to wait for next chunk to know.
        let inner = ok_stream(vec!["text and [", "1] more text"]);
        let out = collect(strip_source_citations_stream(inner)).await;
        assert_eq!(out.concat(), "text and [1] more text");
    }

    #[tokio::test]
    async fn citations_stream_unclosed_marker_emits_at_end() {
        // Marker opens but stream ends before close — the opener leaks
        // verbatim rather than swallowing everything.
        let inner = ok_stream(vec!["start [Source: never closes"]);
        let out = collect(strip_source_citations_stream(inner)).await;
        assert_eq!(out.concat(), "start [Source: never closes");
    }

    #[tokio::test]
    async fn citations_stream_composes_with_thinking_stripper() {
        // Real wire shape: planning trace + close + reply containing a
        // hallucinated citation. Composition order matches runtime:
        // strip_thinking first, then strip_source_citations.
        let inner = ok_stream(vec![
            "<think>planning</think>",
            "You said it ",
            "[Source: X]",
            " feels like waiting.",
        ]);
        let out = collect(strip_source_citations_stream(strip_thinking_stream(inner))).await;
        assert_eq!(out.concat(), "You said it feels like waiting.");
    }

    #[test]
    fn safe_prefix_end_handles_partial_prefix() {
        // Hold the partial marker prefix AND its preceding space, so
        // when the marker completes the space drops with it.
        assert_eq!(compute_safe_prefix_end("hello [", "[Source:"), 5);
        assert_eq!(compute_safe_prefix_end("hello [So", "[Source:"), 5);
        // Trailing whitespace is held back (Invariant 2) so a future
        // marker can absorb it.
        assert_eq!(compute_safe_prefix_end("hello world ", "[Source:"), 11);
        assert_eq!(compute_safe_prefix_end("hello world", "[Source:"), 11);
        assert_eq!(compute_safe_prefix_end("", "[Source:"), 0);
        // Buffer entirely a prefix — must hold all of it.
        assert_eq!(compute_safe_prefix_end("[So", "[Source:"), 0);
        // Single trailing space alone — hold it.
        assert_eq!(compute_safe_prefix_end(" ", "[Source:"), 0);
    }
}
