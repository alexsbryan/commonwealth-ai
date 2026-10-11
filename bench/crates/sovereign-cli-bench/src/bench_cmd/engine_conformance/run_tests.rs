// SPDX-License-Identifier: AGPL-3.0-or-later
//! The driver's pure half: replies and server answers in, facets out.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use sovereign_contracts::engine_observe::{canonical_sampler, Observation, ReplayMethod};

use super::case::{Case, CaseMethod};
use super::project::{
    completion_output, engine_facets, generated_reasoning, record, stream_output, wire_reply,
    Replay,
};
use super::record::{Outcome, ToolCall, Usage};
use super::server::{host_truth, prompt_oracle, slot_sampler};

fn case(method: &str, input: Value) -> Case {
    serde_json::from_value(json!({"case_id": "c", "method": method, "input": input, "rows": ["r"]}))
        .unwrap()
}

#[test]
fn a_case_names_a_replay_method_or_a_driver_scenario_and_nothing_else() {
    assert_eq!(case("probe", json!({})).method, CaseMethod::Probe);
    assert_eq!(
        case("complete_stream_with_finish", json!({})).method,
        CaseMethod::Replay(ReplayMethod::CompleteStreamWithFinish)
    );
    assert!(serde_json::from_value::<Case>(json!({"case_id": "c", "method": "bogus"})).is_err());
}

#[test]
fn engine_observations_fill_facets_and_an_ambiguous_one_says_why_it_did_not() {
    let sampler = |t: f64| Observation::Sampler {
        params: BTreeMap::from([("temperature".to_string(), t)]),
        order: vec!["temperature".into()],
        grammar: Some("start: \"a\"".into()),
    };
    let f = engine_facets(&[
        Observation::PromptTokens { ids: vec![1, 2] },
        sampler(0.7),
        Observation::Prefill {
            evaluated: 2,
            reused: 0,
        },
        Observation::GeneratedTokens { ids: vec![9] },
    ]);
    assert_eq!(f.prompt_ids, Some(vec![1, 2]));
    assert_eq!(f.greedy_tokens, Some(vec![9]));
    assert_eq!(f.prefill_evaluated, Some(2));
    assert_eq!(f.grammar.as_deref(), Some("start: \"a\""));
    assert!(f.unobserved.is_empty(), "{:?}", f.unobserved);

    let f = engine_facets(&[
        Observation::PromptTokens { ids: vec![1] },
        Observation::PromptTokens { ids: vec![2] },
        sampler(0.7),
        sampler(0.7),
    ]);
    assert_eq!(f.prompt_ids, None, "two prompts are not narrowed to one");
    assert_eq!(
        f.unobserved["prompt_ids"],
        "2 different prompt tokenizations in one call"
    );
    assert!(f.sampler.is_some(), "two equal samplers are one");

    let f = engine_facets(&[]);
    assert_eq!(
        f.unobserved["prefill_evaluated"],
        "the engine recorded no prefill"
    );
}

#[test]
fn a_completion_splits_reasoning_and_calls_out_of_the_text_and_derives_usage_as_the_adapter_does() {
    let out = completion_output(
        &json!({
            "text": "<think>plan</think>\n\n<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}\n</tool_call>",
            "prompt_tokens": 10, "tokens_used": 15, "finish_reason": "tool_calls"
        }),
        &[],
    );
    assert_eq!(out.reasoning.as_deref(), Some("plan"));
    assert_eq!(
        out.tool_calls,
        vec![ToolCall {
            name: "read".into(),
            arguments: json!({"path": "a"})
        }]
    );
    assert_eq!(out.text, "");
    assert_eq!(
        out.usage,
        Some(Usage {
            prompt: 10,
            completion: 5
        })
    );
    assert_eq!(out.finish.as_deref(), Some("tool_calls"));

    let plain = completion_output(
        &json!({"text": "hello", "prompt_tokens": 3, "tokens_used": 4}),
        &[],
    );
    assert_eq!((plain.text.as_str(), plain.reasoning), ("hello", None));
    assert_eq!(
        completion_output(&json!({"text": "hello"}), &[]).usage,
        None,
        "absent counts are no usage, not zero"
    );
}

#[test]
fn a_stream_lists_the_frame_kinds_a_caller_could_act_on() {
    let out = stream_output(
        &json!([
            {"kind": "token", "text": "<think>x</think>"},
            {"kind": "token", "text": "hi"},
            {"kind": "finish", "reason": "stop", "usage": {"prompt": 4, "completion": 6}}
        ]),
        &[],
    );
    assert_eq!(out.text, "hi");
    assert_eq!(out.frames, ["token", "finish", "usage", "reasoning"]);
    assert_eq!(out.usage.map(|u| u.completion), Some(6));

    let bare = stream_output(
        &json!([{"kind": "token", "text": "hi"}, {"kind": "finish", "reason": "stop", "usage": null}]),
        &[],
    );
    assert!(
        !bare.frames.contains(&"usage".to_string()),
        "no usage frame without usage"
    );
    let failed = stream_output(
        &json!([{"kind": "finish", "reason": "error", "usage": null}]),
        &[],
    );
    assert!(failed.frames.contains(&"error".to_string()));
}

#[test]
fn the_wire_reply_reads_reasoning_usage_and_counts_each_responses_timings_once() {
    let json_reply = r#"{"choices":[{"message":{"content":"hi","reasoning_content":"r"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":12},"timings":{"prompt_n":7,"cache_n":5}}"#;
    let sse = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"a\"}}]}\n\n\
               data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n\
               data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"timings\":{\"prompt_n\":3}}\n\n\
               data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9},\"timings\":{\"prompt_n\":3}}\n\n\
               data: [DONE]\n\n";
    let w = wire_reply(&[
        Observation::WireRequest {
            body: json!({"a": 1}),
        },
        Observation::WireResponse {
            status: 200,
            body: json_reply.into(),
        },
        Observation::WireRequest {
            body: json!({"a": 2}),
        },
        Observation::WireResponse {
            status: 200,
            body: sse.into(),
        },
    ])
    .unwrap();
    assert_eq!(w.requests.len(), 2);
    assert_eq!(w.content, "b", "the call's answer is its last response");
    assert_eq!(w.reasoning.as_deref(), Some("a"));
    assert_eq!(w.prompt_tokens, Some(9));
    assert_eq!(
        w.prompt_evaluated,
        Some(10),
        "7 + 3, the stream's repeat not counted"
    );
    assert!(wire_reply(&[Observation::PromptTokens { ids: vec![] }]).is_none());
}

#[test]
fn reasoning_for_the_decode_rows_comes_from_the_wire_when_there_is_one() {
    let out = completion_output(&json!({"text": "answer"}), &[]);
    let wire = wire_reply(&[Observation::WireResponse {
        status: 200,
        body:
            r#"{"choices":[{"message":{"content":"answer","reasoning_content":"long thought"}}]}"#
                .into(),
    }]);
    assert_eq!(
        generated_reasoning(Some(&out), wire.as_ref()).as_deref(),
        Some("long thought"),
        "the client dropped it; the server generated it"
    );
    let inline = completion_output(&json!({"text": "<think>t</think>answer"}), &[]);
    assert_eq!(
        generated_reasoning(Some(&inline), None).as_deref(),
        Some("t")
    );
    assert_eq!(generated_reasoning(Some(&out), None).as_deref(), Some(""));
}

#[test]
fn a_refused_replay_records_the_refusal_and_no_answer_facets() {
    let c = case("embed", json!({"text": "x"}));
    let r = record(
        &c,
        ReplayMethod::Embed,
        "llama-server",
        &Replay {
            outcome: json!({"kind": "refused", "message": "no embedder"}),
            answer: Value::Null,
            observations: vec![],
        },
    );
    assert_eq!(
        r.outcome,
        Outcome::Refused {
            message: "no embedder".into()
        }
    );
    assert!(r.facets.embeddings.is_none());
    let ok = record(
        &c,
        ReplayMethod::Embed,
        "embedded",
        &Replay {
            outcome: json!({"kind": "ok"}),
            answer: json!([[0.5, 0.5]]),
            observations: vec![],
        },
    );
    assert_eq!(ok.facets.embeddings, Some(vec![vec![0.5, 0.5]]));
}

fn slots(temperature: f64, id_task: i64, grammar: &str) -> Value {
    json!({"id": 0, "id_task": id_task, "params": {
        "temperature": temperature, "top_k": 40, "top_p": 0.95, "min_p": 0.05,
        "typical_p": 1.0, "xtc_probability": 0.0, "xtc_threshold": 0.1, "top_n_sigma": -1.0,
        "repeat_last_n": 64, "repeat_penalty": 1.0, "presence_penalty": 0.0, "frequency_penalty": 0.0,
        "dry_multiplier": 0.0, "dry_base": 1.75, "dry_allowed_length": 2, "dry_penalty_last_n": -1,
        "n_predict": 128, "seed": 4294967295u64, "grammar": grammar,
        "samplers": ["penalties", "dry", "top_n_sigma", "top_k", "typ_p", "top_p", "min_p", "xtc", "temperature"]
    }})
}

#[test]
fn the_server_sampler_is_the_newest_tasks_slot_described_like_the_engines() {
    let (s, g) = slot_sampler(&json!([
        slots(0.7, 3, ""),
        slots(0.0, 9, "%llguidance {}\nstart: \"a\"")
    ]))
    .unwrap();
    assert_eq!(
        s.order,
        ["greedy"],
        "temperature 0 is argmax, truncation dropped"
    );
    assert!(s.params.is_empty(), "{:?}", s.params);
    assert_eq!(g.as_deref(), Some("%llguidance {}\nstart: \"a\""));

    // The embedded engine's description of the same greedy chain
    // (`server_sampling` path, sampler.rs) compares equal.
    let raw: BTreeMap<String, f64> = [
        ("temperature", 0.0),
        ("top_k", 40.0),
        ("top_p", 0.95),
        ("min_p", 0.05),
        ("repeat_penalty", 1.0),
        ("repeat_last_n", 64.0),
        ("frequency_penalty", 0.0),
        ("presence_penalty", 0.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let (params, order) = canonical_sampler(&raw, &["penalties", "greedy"]);
    assert_eq!((params, order), (s.params, s.order));

    let (warm, g) = slot_sampler(&json!([slots(0.7, 3, "")])).unwrap();
    assert_eq!(warm.order, ["top_k", "top_p", "min_p", "temperature"]);
    assert_eq!(warm.params["temperature"], 0.7);
    assert!(
        !warm.params.contains_key("n_predict"),
        "only stage keys are read"
    );
    assert_eq!(g, None);
    assert!(
        slot_sampler(&json!([{"id": 0}])).is_err(),
        "no slot has run a task"
    );
}

#[test]
fn the_prompt_ids_are_held_to_the_count_the_server_reported() {
    assert_eq!(prompt_oracle(vec![1, 2, 3], Some(3)), Ok(vec![1, 2, 3]));
    assert_eq!(
        prompt_oracle(vec![1, 2], Some(3)),
        Err("/tokenize gave 2 ids, the response's usage says 3".into())
    );
    assert_eq!(prompt_oracle(vec![1], None), Ok(vec![1]));
}

#[test]
fn the_host_truth_is_what_the_router_serves() {
    let reported: BTreeMap<String, Value> = [
        ("model_for_fast", json!("qwen-4b")),
        ("model_for_slow", json!("qwen-35b")),
        ("code_model", json!("coder")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let models = json!({"data": [
        {"id": "qwen-4b", "aliases": [], "status": {"value": "loaded"}},
        {"id": "qwen-35b", "aliases": ["primary"], "status": {"value": "unloaded"},
         "meta": {"n_ctx_train": 262144}}
    ]});
    let props = json!({"default_generation_settings": {"n_ctx": 32768}});
    let t = host_truth(&reported, &props, &models);
    assert_eq!(t["model_for_fast"], "qwen-4b");
    assert_eq!(t["code_model"], json!(["qwen-4b", "qwen-35b", "primary"]));
    assert_eq!(t["effective_context_size"], 32768);
    assert_eq!(t["n_ctx_train"], 262144);
    assert_eq!(t["resident_models"], json!(["qwen-4b"]));
}

#[test]
fn a_forced_choice_records_its_label_probabilities_and_other_methods_no_completion_reasons() {
    let c = case(
        "complete",
        json!({"prompt": "q", "structured_output": {"type": "string", "enum": ["A", "B"], "x_forced_choice": true}}),
    );
    let ok = |text: &str| Replay {
        outcome: json!({"kind": "ok"}),
        answer: json!({"text": text, "prompt_tokens": 5, "tokens_used": 5}),
        observations: vec![],
    };
    let r = record(
        &c,
        ReplayMethod::Complete,
        "embedded",
        &ok(r#"{"A":0.75,"B":0.25}"#),
    );
    assert_eq!(
        r.facets.label_probs,
        Some(BTreeMap::from([
            ("A".to_string(), 0.75),
            ("B".to_string(), 0.25)
        ]))
    );
    let r = record(&c, ReplayMethod::Complete, "embedded", &ok("A"));
    assert_eq!(r.facets.label_probs, None);
    assert!(r.facets.unobserved.contains_key("label_probs"));

    let plain = case("complete", json!({"prompt": "q"}));
    let r = record(
        &plain,
        ReplayMethod::Complete,
        "embedded",
        &ok(r#"{"A":0.75,"B":0.25}"#),
    );
    assert_eq!(
        r.facets.label_probs, None,
        "only a forced choice is read as one"
    );

    let host = case("host", json!({}));
    let r = record(
        &host,
        ReplayMethod::Host,
        "embedded",
        &Replay {
            outcome: json!({"kind": "ok"}),
            answer: json!({"resident_models": ["b", "a"]}),
            observations: vec![],
        },
    );
    assert!(r.facets.unobserved.is_empty(), "{:?}", r.facets.unobserved);
    assert_eq!(r.facets.host_reported["resident_models"], json!(["a", "b"]));
}
