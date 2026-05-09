//! Glassbox prompt-size accounting and configurable, conservative
//! trimming of inbound chat-completion requests.
//!
//! Why this exists. Tools-using clients (opencode, Aider, the
//! Anthropic SDK's openai-compat shim) ship a large per-turn
//! preamble: their full system prompt, prose tool descriptions, and
//! the running conversation history. With grammar-constrained tool
//! calls active, every token of every turn runs the JSON validator
//! over `buffer + token` candidates, so prompt size is a direct
//! linear input to per-turn latency. The 2026-05-08 grammar-active
//! ATOS run measured ~100 s/turn at 31–34 K accumulated context on
//! Qwen-Coder Q6_K, almost all of it spent on prompt processing
//! before the first generated token.
//!
//! The compactor sits at the very top of `chat_completion` (and
//! `chat_completion_stream`) so every transformation lands before
//! prompt-flattening, before slot pick, and before the request hits
//! the model. Two surfaces:
//!
//!   - `PromptSizeReport::measure(&request)` — always-on. Per-role
//!     character accounting + tools-schema char count. Logged at
//!     `tracing::info` so operators see what the model is actually
//!     reading on every request, not only at debug-knob time.
//!   - `PromptCompactor::from_env().compact(&mut request)` — opt-in.
//!     One safe, generic transformation today: cap individual
//!     `role="tool"` message bodies via
//!     `SOVEREIGN_TOOL_RESULT_MAX_BYTES`. Future-extensible by
//!     adding new env-driven knobs in `from_env` plus
//!     transformations in `compact`.
//!
//! Design constraints (per ARCH_PRINCIPLES § 0, § 1):
//!
//!   - Disabled by default. With no env var set the compactor is a
//!     no-op and the request reaches the inference path bit-identical
//!     to today's daemon. Trim risk is opt-in.
//!   - Glassbox. Pre/post sizes are logged per-transformation. An
//!     operator inspecting daemon stdout can see exactly which
//!     messages the compactor touched and by how much.
//!   - Single responsibility. This module knows *only* about
//!     measuring and trimming `ChatCompletionRequest` shape; it
//!     never reaches into inference internals or sampler config.
//!   - No client-specific heuristics yet. Trim rules ship as
//!     generic protections (cap unbounded tool output) rather than
//!     guessing at opencode's prompt markers; opencode-specific trims
//!     will land once `PromptSizeReport` data from real loops is in.

use commonwealth_api::openai_types::ChatCompletionRequest;

/// Per-message-class character accounting for one chat-completion
/// request. Computed at the entry point of `chat_completion` and
/// logged unconditionally so the operator has a continuous record of
/// prompt size composition over time.
///
/// All counts are in *characters* (Rust `String::len`, i.e. bytes),
/// not tokens. Token counting requires a tokenizer keyed to the
/// chosen slot's model, which the adapter doesn't have at this
/// point in the pipeline. Bytes are a faithful proxy for relative
/// growth; absolute token-cost lives downstream in the inference
/// log line which already reports `prompt_tokens`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PromptSizeReport {
    pub system_chars: usize,
    pub user_chars: usize,
    pub assistant_chars: usize,
    pub tool_chars: usize,
    pub other_chars: usize,
    /// Sum of name + description + JSON-encoded `parameters` schema
    /// for every entry in `request.tools`. Tracked separately
    /// because the chat-template path injects this into the model's
    /// prompt as text, and it's commonly the largest single segment
    /// when a client (e.g. opencode) ships a dozen-plus tools.
    pub tools_schema_chars: usize,
    pub message_count: usize,
    pub tool_count: usize,
}

impl PromptSizeReport {
    pub fn measure(req: &ChatCompletionRequest) -> Self {
        let mut r = Self {
            message_count: req.messages.len(),
            ..Self::default()
        };
        for m in &req.messages {
            let n = m.content.len();
            match m.role.as_str() {
                "system" => r.system_chars += n,
                "user" => r.user_chars += n,
                "assistant" => r.assistant_chars += n,
                "tool" => r.tool_chars += n,
                _ => r.other_chars += n,
            }
        }
        if let Some(tools) = req.tools.as_ref() {
            r.tool_count = tools.len();
            for t in tools {
                r.tools_schema_chars += t.function.name.len();
                r.tools_schema_chars += t
                    .function
                    .description
                    .as_ref()
                    .map(String::len)
                    .unwrap_or(0);
                r.tools_schema_chars += serde_json::to_string(&t.function.parameters)
                    .map(|s| s.len())
                    .unwrap_or(0);
            }
        }
        r
    }

    pub fn total_chars(&self) -> usize {
        self.system_chars
            + self.user_chars
            + self.assistant_chars
            + self.tool_chars
            + self.other_chars
            + self.tools_schema_chars
    }

    /// Emit a tracing::info line with every component count.
    /// Designed so the line is greppable (`prompt_size:`) and every
    /// field is a discrete `key=value` pair that downstream log
    /// pipelines can parse without regex.
    pub fn log(&self, phase: &'static str) {
        tracing::info!(
            phase = phase,
            messages = self.message_count,
            tools = self.tool_count,
            system_chars = self.system_chars,
            user_chars = self.user_chars,
            assistant_chars = self.assistant_chars,
            tool_chars = self.tool_chars,
            other_chars = self.other_chars,
            tools_schema_chars = self.tools_schema_chars,
            total_chars = self.total_chars(),
            "prompt_size: per-class character accounting"
        );
    }
}

/// Configurable, opt-in request trimmer. Today exposes one knob:
/// `SOVEREIGN_TOOL_RESULT_MAX_BYTES`. The shape is built to grow:
/// new env-driven trims add a field on the struct and a branch in
/// `compact`, no architectural changes.
#[derive(Debug, Default, Clone, Copy)]
pub struct PromptCompactor {
    /// When `Some(n)`, every `role="tool"` message whose content
    /// exceeds `n` bytes is replaced with the first `n/2` bytes,
    /// a truncation marker, and the last `n/2` bytes. The model
    /// retains the start (which usually contains the failure
    /// summary) and the end (which usually contains the actionable
    /// trailing diagnostic) and loses the middle (which is usually
    /// repetitive build output). Generic protection: bash/cargo
    /// invocations from any client can produce arbitrarily-large
    /// tool output and the model rarely needs more than a few KB.
    ///
    /// Env: `SOVEREIGN_TOOL_RESULT_MAX_BYTES=<usize>`. Unset → no
    /// trimming. Set to `0` is treated the same as unset (degenerate;
    /// a zero-cap would erase the whole tool result and the
    /// truncation marker is itself >0 bytes).
    pub cap_tool_result_bytes: Option<usize>,
}

impl PromptCompactor {
    /// Build a compactor configuration from environment variables.
    /// Read once per request so flipping a knob at runtime takes
    /// effect on the next request — no daemon restart.
    pub fn from_env() -> Self {
        Self::from_env_lookup(|key| std::env::var(key).ok())
    }

    /// Test-friendly variant: takes a closure that resolves env-var
    /// names to optional values. Lets unit tests pin the parsing
    /// rules without mutating process-global env (which races against
    /// parallel test execution). Production calls go through
    /// `from_env`.
    pub fn from_env_lookup<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let cap_tool_result_bytes = get("SOVEREIGN_TOOL_RESULT_MAX_BYTES")
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0);
        Self { cap_tool_result_bytes }
    }

    /// True if any transformation is active. Used by the call-site
    /// to skip both the post-compact measurement and the
    /// "compactor: applied" log line when nothing would change —
    /// keeps the disabled-default path exactly as cheap as today.
    pub fn is_active(&self) -> bool {
        self.cap_tool_result_bytes.is_some()
    }

    /// Apply every active transformation in-place. Logs every
    /// transformation it performs at `tracing::info` so the operator
    /// has a record of which message was trimmed by how much.
    /// Transformation order is deterministic: tool-result cap
    /// first; future transformations append.
    pub fn compact(&self, req: &mut ChatCompletionRequest) {
        if let Some(cap) = self.cap_tool_result_bytes {
            for (idx, m) in req.messages.iter_mut().enumerate() {
                if m.role == "tool" && m.content.len() > cap {
                    let original_len = m.content.len();
                    m.content = truncate_middle(&m.content, cap);
                    tracing::info!(
                        message_index = idx,
                        original_chars = original_len,
                        capped_chars = m.content.len(),
                        cap_bytes = cap,
                        "prompt_compactor: tool-result truncated"
                    );
                }
            }
        }
    }
}

/// Truncate a string to roughly `cap` bytes by keeping the first
/// half and last half and replacing the middle with a marker. The
/// returned string may be slightly longer than `cap` because the
/// marker has non-zero length; that's intentional — making the
/// marker fit inside `cap` would push removed-byte accounting onto
/// the caller, which would hide the trim from the model. Returns
/// the input unchanged when it's already at-or-under `cap`.
///
/// Char-boundary safety: we cut on the last char-boundary at-or-
/// before `cap/2` (head) and the first char-boundary at-or-after
/// `len - cap/2` (tail). Slicing inside a multi-byte char would
/// panic; this function is total.
fn truncate_middle(s: &str, cap: usize) -> String {
    let len = s.len();
    if len <= cap {
        return s.to_string();
    }
    let half = cap / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, len - half);
    let removed = tail_start.saturating_sub(head_end);
    let head = &s[..head_end];
    let tail = &s[tail_start..];
    format!(
        "{head}\n[...sovereign prompt_compactor truncated {removed} bytes...]\n{tail}"
    )
}

/// Stable char-boundary helpers. `str::floor_char_boundary` and
/// `str::ceil_char_boundary` are still nightly-only on the Rust
/// release we're targeting; reimplement minimally.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_api::openai_types::{ChatMessage, ToolDefinition, ToolFunction};

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    fn tool_def(name: &str, desc: &str, params: serde_json::Value) -> ToolDefinition {
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: Some(desc.into()),
                parameters: params,
            },
        }
    }

    fn req_with(messages: Vec<ChatMessage>, tools: Option<Vec<ToolDefinition>>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages,
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools,
            tool_choice: None,
            response_format: None,
            oicp: None,
            chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
        }
    }

    // ---- PromptSizeReport ----

    #[test]
    fn report_per_role_accounting() {
        let req = req_with(
            vec![
                msg("system", "sys"),                 // 3
                msg("user", "user-msg"),              // 8
                msg("assistant", "ass!"),             // 4
                msg("tool", "tool-result"),           // 11
                msg("function", "legacy"),            // 6 → "other"
            ],
            None,
        );
        let r = PromptSizeReport::measure(&req);
        assert_eq!(r.system_chars, 3);
        assert_eq!(r.user_chars, 8);
        assert_eq!(r.assistant_chars, 4);
        assert_eq!(r.tool_chars, 11);
        assert_eq!(r.other_chars, 6);
        assert_eq!(r.message_count, 5);
        assert_eq!(r.tool_count, 0);
        assert_eq!(r.tools_schema_chars, 0);
        assert_eq!(r.total_chars(), 32);
    }

    #[test]
    fn report_counts_tools_schema_chars() {
        // name=4 + desc=11 + params=`{"type":"object"}` (17 chars).
        let params = serde_json::json!({"type": "object"});
        let tools = vec![tool_def("edit", "edit a file", params)];
        let req = req_with(vec![msg("user", "u")], Some(tools));
        let r = PromptSizeReport::measure(&req);
        assert_eq!(r.tool_count, 1);
        assert_eq!(r.user_chars, 1);
        // 4 (name) + 11 (desc) + 17 ({"type":"object"})
        assert_eq!(r.tools_schema_chars, 4 + 11 + 17);
    }

    #[test]
    fn report_handles_empty_request() {
        let req = req_with(vec![], None);
        let r = PromptSizeReport::measure(&req);
        assert_eq!(r.total_chars(), 0);
        assert_eq!(r.message_count, 0);
        assert_eq!(r.tool_count, 0);
    }

    #[test]
    fn report_logs_without_panic() {
        // Smoke: ensure the tracing macro accepts the field set.
        // The log line itself is captured by the global subscriber
        // (or dropped if none); we only assert the call doesn't
        // panic at runtime.
        let req = req_with(vec![msg("user", "u")], None);
        PromptSizeReport::measure(&req).log("test");
    }

    // ---- PromptCompactor ----

    #[test]
    fn compactor_default_inactive_no_op() {
        let pc = PromptCompactor::default();
        assert!(!pc.is_active());

        let big = "x".repeat(100_000);
        let mut req = req_with(vec![msg("tool", &big)], None);
        pc.compact(&mut req);
        assert_eq!(req.messages[0].content.len(), 100_000);
    }

    #[test]
    fn compactor_caps_oversized_tool_result() {
        let pc = PromptCompactor {
            cap_tool_result_bytes: Some(200),
        };
        assert!(pc.is_active());

        let big = "a".repeat(10_000);
        let mut req = req_with(
            vec![msg("system", "keep"), msg("tool", &big), msg("user", "keep")],
            None,
        );
        pc.compact(&mut req);

        // System + user untouched.
        assert_eq!(req.messages[0].content, "keep");
        assert_eq!(req.messages[2].content, "keep");

        // Tool result was truncated. Final length should be roughly
        // cap + marker_len, not arbitrary.
        let cap = 200;
        let trimmed = &req.messages[1].content;
        assert!(
            trimmed.len() < 10_000,
            "expected trim to shrink content, got len={}",
            trimmed.len()
        );
        assert!(
            trimmed.len() < cap + 200,
            "expected len near cap+marker, got len={}",
            trimmed.len()
        );
        assert!(trimmed.contains("sovereign prompt_compactor truncated"));
        // Head + tail preserved.
        assert!(trimmed.starts_with("aaaa"));
        assert!(trimmed.ends_with("aaaa"));
    }

    #[test]
    fn compactor_leaves_undersized_tool_result_alone() {
        let pc = PromptCompactor {
            cap_tool_result_bytes: Some(1_000),
        };
        let small = "tiny tool output".to_string();
        let mut req = req_with(vec![msg("tool", &small)], None);
        pc.compact(&mut req);
        assert_eq!(req.messages[0].content, "tiny tool output");
    }

    #[test]
    fn compactor_only_caps_tool_role_not_user_or_assistant() {
        let pc = PromptCompactor {
            cap_tool_result_bytes: Some(50),
        };
        let big = "X".repeat(500);
        let mut req = req_with(
            vec![
                msg("user", &big),
                msg("assistant", &big),
                msg("tool", &big),
            ],
            None,
        );
        pc.compact(&mut req);
        assert_eq!(req.messages[0].content.len(), 500); // user untouched
        assert_eq!(req.messages[1].content.len(), 500); // assistant untouched
        assert!(req.messages[2].content.len() < 500);    // tool capped
    }

    #[test]
    fn compactor_truncate_middle_handles_multibyte_safely() {
        // A string of 2-byte UTF-8 chars where the cut point lands
        // mid-codepoint. Must not panic.
        let s: String = std::iter::repeat('é').take(2_000).collect(); // 4_000 bytes
        let out = truncate_middle(&s, 101); // odd cap, mid-codepoint cuts
        assert!(out.len() < s.len());
        // Round-trip parse: the result must still be valid UTF-8.
        // (truncate_middle returns String; just exercise.)
        let _ = out.chars().count();
    }

    /// Build a `lookup` closure that pretends the named var has
    /// the given value (or is unset when `value` is `None`).
    fn fake_env(key: &'static str, value: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        let val = value.map(|s| s.to_string());
        move |k: &str| if k == key { val.clone() } else { None }
    }

    #[test]
    fn compactor_from_env_unset_inactive() {
        let pc = PromptCompactor::from_env_lookup(fake_env(
            "SOVEREIGN_TOOL_RESULT_MAX_BYTES",
            None,
        ));
        assert!(!pc.is_active());
    }

    #[test]
    fn compactor_from_env_zero_treated_as_disabled() {
        let pc = PromptCompactor::from_env_lookup(fake_env(
            "SOVEREIGN_TOOL_RESULT_MAX_BYTES",
            Some("0"),
        ));
        assert!(!pc.is_active());
    }

    #[test]
    fn compactor_from_env_positive_activates() {
        let pc = PromptCompactor::from_env_lookup(fake_env(
            "SOVEREIGN_TOOL_RESULT_MAX_BYTES",
            Some("1024"),
        ));
        assert!(pc.is_active());
        assert_eq!(pc.cap_tool_result_bytes, Some(1024));
    }

    #[test]
    fn compactor_from_env_garbage_treated_as_disabled() {
        let pc = PromptCompactor::from_env_lookup(fake_env(
            "SOVEREIGN_TOOL_RESULT_MAX_BYTES",
            Some("not-a-number"),
        ));
        assert!(!pc.is_active());
    }
}
