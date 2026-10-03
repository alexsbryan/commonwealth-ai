// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OpenAI-compatible dialect on the wire: what a vendor host is sent for
//! each structured-output mode, and what is read back.

use super::DaemonInferenceClient;
use crate::mock_provider::{mock_openai_host, CONTENT_OK};
use crate::test_env::scoped_home;
use corpus_engine::enrichment::pipeline::ChatPrompt;
use serde_json::{json, Value};
use sovereign_contracts::egress::ConsentGrant;
use sovereign_contracts::types::Custody;

const TOOL_CALL_REPLY: &str = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"phase_1__atlas_","arguments":"{\"questions_raised\":[\"why?\"]}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

/// One vendor provider in `providers.toml`, released by a personal grant. The
/// daemon base is a dead port so the vendor is never mistaken for it.
fn vendor_client(base_url: &str, extra_toml: &str) -> DaemonInferenceClient {
    let home = std::env::var("HOME").expect("scoped_home set HOME");
    let dir = format!("{home}/.config/sovereign");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        format!("{dir}/providers.toml"),
        format!(
            "[providers.vendor]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\n{extra_toml}"
        ),
    )
    .unwrap();
    DaemonInferenceClient::new("http://127.0.0.1:9", "vendor:m", "embed")
        .unwrap()
        .with_consent(Some(ConsentGrant {
            run_id: "dialect-test".into(),
            granted_at_unix: 0,
            release_floor: Custody::Personal,
        }))
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {"questions_raised": {"type": "array", "items": {"type": "string"}}},
        "required": ["questions_raised"]
    })
}

/// THE FAILING INPUT: before 2026-10-03 `tool-use-forced` on an
/// OpenAI-compatible host sent `response_format: json_object` and no tool, so
/// a host that enforces no schema there (DeepSeek's chat API) dropped
/// required fields. 15 of 20 wessex-hoard Phase 1 chapters lost
/// `questions_raised` that way.
#[tokio::test]
async fn tool_use_forced_sends_the_schema_as_a_forced_function_and_reads_its_arguments() {
    let _home = scoped_home();
    let (base, recorded) = mock_openai_host(TOOL_CALL_REPLY).await;
    let client = vendor_client(&base, "structured_output_mode = \"tool-use-forced\"\n");
    let prompt = ChatPrompt::new("sys", "user").with_response_schema("phase 1 (atlas)", schema());

    let out = client.complete(&prompt).await.expect("a tool call answers");
    assert_eq!(out, r#"{"questions_raised":["why?"]}"#);

    let body: Value = serde_json::from_str(&recorded.lock().unwrap()[0]).unwrap();
    assert!(
        body.get("response_format").is_none(),
        "the schema rides the tool, not a response_format: {body}"
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["parameters"], schema());
    assert_eq!(
        body["tools"][0]["function"]["name"], "phase_1__atlas_",
        "a free-text schema name folds to the function-name alphabet"
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "function": {"name": "phase_1__atlas_"}})
    );
}

/// `tool-use-auto` lets the model answer in text; the text is used, not
/// refused, and the call still succeeds.
#[tokio::test]
async fn tool_use_auto_accepts_a_text_answer() {
    let _home = scoped_home();
    let (base, recorded) = mock_openai_host(CONTENT_OK).await;
    let client = vendor_client(&base, "structured_output_mode = \"tool-use-auto\"\n");
    let prompt = ChatPrompt::new("sys", "user").with_response_schema("s", schema());

    assert_eq!(client.complete(&prompt).await.unwrap(), "ok");
    let body: Value = serde_json::from_str(&recorded.lock().unwrap()[0]).unwrap();
    assert_eq!(body["tool_choice"], "auto");
}

/// `extra_params` reached only the Anthropic body until 2026-10-03, so
/// OpenRouter's `provider` routing knob (send only to upstreams that honour
/// the request's parameters) could not be expressed.
#[tokio::test]
async fn extra_params_reach_an_openai_compatible_body() {
    let _home = scoped_home();
    let (base, recorded) = mock_openai_host(CONTENT_OK).await;
    let client = vendor_client(
        &base,
        "[providers.vendor.extra_params]\nprovider = { require_parameters = true }\n",
    );
    client
        .complete(&ChatPrompt::new("sys", "user"))
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&recorded.lock().unwrap()[0]).unwrap();
    assert_eq!(body["provider"], json!({"require_parameters": true}));
}
