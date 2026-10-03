// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `<think>` split every CLI that prints a model's answer applies. Moved
//! from sovereign-cli-llm's `chat_cmd::render` (pb-cli-llm-bench-move) so
//! bench's eval runner and svrn's chat verbs split the same way.

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
