//! Pure string-manipulation helpers — extracted out of `pipeline::types`.
//!
//! Used by every phase's `parse_*` to be forgiving about model output
//! framing (`<think>...</think>` reasoning blocks, fenced JSON,
//! placeholder echoes) without dragging the full type module into a
//! caller's import set.

/// True when a string is indistinguishable from a schema-template
/// placeholder a model would copy verbatim — `"..."`, `"…"`, any
/// combination of those dots and whitespace, or the literal token
/// `TODO`. Trim-tolerant; callers don't need to pre-trim.
///
/// Used wherever we'd otherwise silently persist a placeholder echo
/// (phase-1 parser, `characters_present` merge, manifest hydration).
pub fn is_placeholder_literal(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t == "..." || t == "…" || t.eq_ignore_ascii_case("todo") {
        return true;
    }
    t.chars().all(|c| c == '.' || c == '…' || c.is_whitespace())
}

/// Remove any `<think>...</think>` spans from `response`, returning a
/// copy with the reasoning blocks deleted. Thinking-capable models
/// (Qwen3, DeepSeek R1, o1-family) emit chain-of-thought between
/// these tags before their actual answer. The answer we care about
/// follows the closing tag; parsers that only read the head of the
/// response would otherwise miss the JSON entirely.
///
/// Non-destructive: if no `<think>` tag is present, the returned
/// string is byte-identical to the input. If an opening tag has no
/// matching close (the response was truncated mid-think), the entire
/// tail from `<think>` onward is dropped — callers should detect
/// this separately and surface a clear error, since a truncated
/// response has no JSON to parse anyway.
pub fn strip_reasoning_tags(response: &str) -> String {
    let mut out = String::with_capacity(response.len());
    let mut remaining = response;
    while let Some(open_idx) = remaining.find("<think>") {
        out.push_str(&remaining[..open_idx]);
        let after_open = &remaining[open_idx + "<think>".len()..];
        match after_open.find("</think>") {
            Some(close_idx) => {
                remaining = &after_open[close_idx + "</think>".len()..];
            }
            None => {
                // Unclosed <think> — drop the rest.
                remaining = "";
                break;
            }
        }
    }
    out.push_str(remaining);
    out
}

/// True when `response` opens a `<think>` block but never closes it.
/// This is the thinking-model truncation signature: the model spent
/// its whole output budget reasoning and never produced the requested
/// answer. A stray `{` inside the reasoning trace (e.g. the model
/// drafting sample JSON while it thinks) does NOT invalidate the
/// detection — the answer we care about sits after `</think>`, and
/// without that close tag we cannot have reached it.
pub fn is_truncated_thinking_response(response: &str) -> bool {
    let Some(open_idx) = response.find("<think>") else {
        return false;
    };
    let after_open = &response[open_idx + "<think>".len()..];
    !after_open.contains("</think>")
}

/// Extract the first JSON object from a model response, tolerating
/// leading prose and/or surrounding Markdown code fences. Returns the
/// JSON substring (without the fences) or `None` if nothing resembling
/// an object can be located.
///
/// Used by every phase's `parse_*` to be forgiving about model output
/// framing while still rejecting genuinely malformed bodies downstream
/// in the `serde_json::from_str` step.
pub fn extract_json_block(response: &str) -> Option<&str> {
    // Look for a ```json fenced block first.
    if let Some(start) = response.find("```json") {
        let rest = &response[start + "```json".len()..];
        if let Some(end) = rest.find("```") {
            return Some(rest[..end].trim());
        }
    }
    // Or any ``` fenced block whose content starts with `{`.
    if let Some(start) = response.find("```") {
        let rest = &response[start + 3..];
        if let Some(end) = rest.find("```") {
            let inner = rest[..end].trim();
            if inner.starts_with('{') {
                return Some(inner);
            }
        }
    }
    // Fall back to the first `{…}` block, picking the widest balanced
    // braces scan we can do cheaply.
    let bytes = response.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&response[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}
