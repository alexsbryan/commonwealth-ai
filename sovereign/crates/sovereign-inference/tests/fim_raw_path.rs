// SPDX-License-Identifier: AGPL-3.0-or-later
//! FIM raw-prompt integration test (INLINE_COMPLETION.md F0/F2) —
//! exercises `PromptShape::Raw` end-to-end through `EmbeddedLlamaCpp`:
//! marker vocab probe on the real tokenizer, PSM prompt assembly,
//! no-template/no-BOS tokenization, typed stream, and (crucially for
//! F2) two SEQUENTIAL requests proving the LCP partial-keep path
//! leaves the slot usable (no KV desync on the second decode).
//!
//! Run explicitly (needs a local coder gguf, ABSOLUTE path — the
//! test's cwd is the crate dir, not the repo root):
//!   SOVEREIGN_FIM_TEST_GGUF=$PWD/sovereign/models/Mellum2-12B-A2.5B-Thinking-Q6_K.gguf \
//!     cargo test -p sovereign-inference --test fim_raw_path -- --ignored --nocapture

use std::path::{Path, PathBuf};

use futures::StreamExt;
use sovereign_core::setup_config::FimSection;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, PromptShape, SamplingMode, StreamFrame};
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_inference::fim::build_fim_prompt;

fn gated_gguf() -> Option<PathBuf> {
    std::env::var("SOVEREIGN_FIM_TEST_GGUF")
        .ok()
        .map(PathBuf::from)
}

async fn run_fim(
    engine: &EmbeddedLlamaCpp,
    info: &sovereign_core::types::FimSlotInfo,
    prefix: &str,
    suffix: &str,
) -> String {
    let prompt = build_fim_prompt(info.fim_style, prefix, suffix);
    let mut req = CompletionRequest::new(&prompt);
    req.prompt_shape = Some(PromptShape::Raw);
    req.sampling_mode = Some(SamplingMode::Code);
    req.model_id = Some(info.model_id.clone());
    req.max_tokens = Some(32);
    req.temperature = Some(0.0);
    let mut stream = engine
        .complete_stream_with_finish(&req)
        .await
        .expect("stream starts");
    let mut text = String::new();
    let mut finished = false;
    while let Some(frame) = stream.next().await {
        match frame {
            StreamFrame::Token(t) => text.push_str(&t),
            StreamFrame::Finish { .. } => finished = true,
            StreamFrame::Error(e) => panic!("stream error: {e}"),
        }
    }
    assert!(finished, "stream must end with a Finish frame");
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual: needs SOVEREIGN_FIM_TEST_GGUF pointing at a local coder gguf"]
async fn raw_fim_round_trip_and_lcp_second_request() {
    let Some(gguf) = gated_gguf() else {
        eprintln!("SKIP: set SOVEREIGN_FIM_TEST_GGUF to a local coder gguf");
        return;
    };
    let engine = EmbeddedLlamaCpp::load_full(&gguf, None, None, 4096, None)
        .expect("engine loads the coder gguf as the fast slot");

    // Alias-mode install: fim.path == fast path → probe on the fast
    // slot's own model, no duplicate load.
    let section = FimSection {
        path: gguf.clone(),
        context_size: Some(4096),
        max_tokens: Some(32),
        temperature: Some(0.0),
        max_prefix_chars: None,
        max_suffix_chars: None,
    };
    engine
        .install_fim_slot(&section, &gguf)
        .expect("fim slot installs");
    let info = engine
        .fim_slot_info()
        .expect("marker probe must succeed on a real coder tokenizer");
    assert!(info.aliased_to_fast, "same-path install must alias to fast");

    let first = run_fim(
        &engine,
        &info,
        "fn fibonacci(n: u32) -> u32 {\n    match n {\n        0 => 0,\n        1 => 1,\n        _ => ",
        "\n    }\n}\n",
    )
    .await;
    assert!(!first.trim().is_empty(), "first completion empty");
    assert!(
        !first.contains("<fim_")
            && !first.contains("<|endoftext|>")
            && !first.contains("<|im_end|>"),
        "marker leak: {first:?}"
    );

    // Second request — the LCP partial-keep path now has a populated
    // cache; a desync would show as garbage or a decode failure here.
    // Extend the prefix by one keystroke ("f" → "fi" style delta is
    // exactly what the keystroke path produces).
    let second = run_fim(
        &engine,
        &info,
        "fn fibonacci(n: u32) -> u32 {\n    match n {\n        0 => 0,\n        1 => 1,\n        _ => f",
        "\n    }\n}\n",
    )
    .await;
    assert!(
        !second.trim().is_empty(),
        "second completion empty — LCP partial-keep desynced the slot"
    );
    assert!(
        !second.contains("<fim_") && !second.contains("<|endoftext|>"),
        "marker leak on second request: {second:?}"
    );

    eprintln!("first:  {first:?}");
    eprintln!("second: {second:?}");
}

#[allow(dead_code)]
fn sanity(_p: &Path) {}
