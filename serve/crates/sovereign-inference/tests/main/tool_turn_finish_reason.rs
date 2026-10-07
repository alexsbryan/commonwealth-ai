// SPDX-License-Identifier: AGPL-3.0-or-later
//! A tool turn that stops on its `</tool_call>` marker finished; it did not
//! run out of budget. The plain (non-MTP) decode loop, which every tool
//! request takes, used to report `Length` for it: the reason was a variable
//! defaulted to `Length` that only the EOS branch overwrote. The adapter
//! hides that while the call parses (it reports `tool_calls`), and sends
//! `length` to the client when it does not — opencode on Qwen3.8-27B,
//! 2026-10-06, ended its session on a 26-token "length".
//!
//! Run explicitly (ABSOLUTE path to a chat GGUF that emits `<tool_call>`
//! markup, e.g. a Qwen3.x):
//!   SOVEREIGN_TOOL_STOP_TEST_GGUF=$PWD/sovereign/models/Qwen3.8-27B-UD-Q6_K_XL.gguf \
//!     cargo test -p sovereign-inference --test main tool_turn_finish_reason -- --ignored --nocapture

use std::path::PathBuf;

use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, FinishReason, ToolSchema};
use sovereign_inference::embedded::{EmbeddedLlamaCpp, SlotWindows};

const BUDGET: usize = 512;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual: needs SOVEREIGN_TOOL_STOP_TEST_GGUF pointing at a tool-calling chat gguf"]
async fn a_turn_that_stops_on_its_tool_call_marker_reports_stop() {
    let Some(gguf) = std::env::var("SOVEREIGN_TOOL_STOP_TEST_GGUF")
        .ok()
        .map(PathBuf::from)
    else {
        eprintln!("SKIP: set SOVEREIGN_TOOL_STOP_TEST_GGUF to a tool-calling chat gguf");
        return;
    };
    let engine = EmbeddedLlamaCpp::load_full(&gguf, None, None, SlotWindows::uniform(8192), None)
        .expect("engine loads the gguf as the fast slot");

    let mut req = CompletionRequest::new(
        "Read the file notes.txt in the current directory. Call the read tool now; do not answer in prose.",
    );
    req.tools = Some(vec![ToolSchema {
        name: "read".into(),
        description: Some("Read a file and return its contents".into()),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
    }]);
    req.max_tokens = Some(BUDGET);
    req.temperature = Some(0.0);

    let resp = engine.complete(&req).await.expect("completion succeeds");
    let generated = resp.tokens_used.saturating_sub(resp.prompt_tokens);
    eprintln!(
        "finish_reason={:?} generated={generated} text={:?}",
        resp.finish_reason, resp.text
    );
    // The precondition, not the verdict: without a marker stop this run
    // says nothing about the marker branch.
    assert!(
        resp.text.contains("</tool_call>") && generated < BUDGET,
        "COULD NOT JUDGE: the model did not stop on a tool-call marker within {BUDGET} tokens"
    );
    assert_eq!(
        resp.finish_reason,
        Some(FinishReason::Stop),
        "a marker stop at {generated}/{BUDGET} tokens is a finish, not a budget exhaustion"
    );
}
