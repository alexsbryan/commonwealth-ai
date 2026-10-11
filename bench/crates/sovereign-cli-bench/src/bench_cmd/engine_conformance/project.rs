// SPDX-License-Identifier: AGPL-3.0-or-later
//! From what one replay returned to a [`CaseRecord`]: the answer as the
//! caller received it, and the facets the engine recorded about itself.
//! Pure, so every projection is tested on constructed replies.
//!
//! Two readings of the same output are kept apart. The projection rows judge
//! what the caller received, so [`Output`] is read from the replay's answer.
//! The decode rows judge what the model generated, so the think block a
//! remote server returned is read from the wire, where the client may since
//! have dropped it ([`WireReply`]).

use serde::Deserialize;
use serde_json::Value;
use sovereign_contracts::engine_observe::{Observation, ReplayMethod};
use sovereign_contracts::oicp::chat_turn::parse_assistant_turn;
use sovereign_contracts::oicp::completion::ToolSchema;
use sovereign_contracts::oicp::forced_choice;

use super::case::Case;
use super::record::{CaseRecord, Facets, Outcome, Output, Sampler, ToolCall, Usage};

/// What the daemon's replay route returned
/// (`sovereign-daemon/src/routes_internal/engine_replay.rs`).
#[derive(Debug, Clone, Deserialize)]
pub struct Replay {
    /// `{kind: ok | refused | error, message?}`.
    pub outcome: Value,
    /// The method's answer.
    #[serde(default)]
    pub answer: Value,
    /// What the engine recorded while it ran.
    #[serde(default)]
    pub observations: Vec<Observation>,
}

/// The record for one replayed case, before any server-side observer adds to
/// it.
pub fn record(case: &Case, method: ReplayMethod, target: &str, replay: &Replay) -> CaseRecord {
    // Only a completion reaches the engine's prompt, sampler and prefill
    // sites, so only a completion's record says why one was not observed.
    let mut facets = if method.is_completion() {
        engine_facets(&replay.observations)
    } else {
        Facets::default()
    };
    let outcome = outcome(&replay.outcome);
    if outcome == Outcome::Ok {
        answer_facets(case, method, &replay.answer, &mut facets);
    }
    CaseRecord {
        case_id: case.case_id.clone(),
        target: target.into(),
        rows: case.rows.clone(),
        request: if method.is_completion() {
            case.input.clone()
        } else {
            Value::Null
        },
        outcome,
        facets,
    }
}

fn outcome(v: &Value) -> Outcome {
    let message = || v["message"].as_str().unwrap_or_default().to_string();
    match v["kind"].as_str() {
        Some("ok") => Outcome::Ok,
        Some("refused") => Outcome::Refused { message: message() },
        _ => Outcome::Error {
            message: format!("{}: {}", v["kind"], message()),
        },
    }
}

/// One value from the observations of one call. Several equal ones are one;
/// several that differ are not silently narrowed to one of them.
fn single<T: PartialEq>(mut items: Vec<T>, what: &str) -> Result<T, String> {
    match items.len() {
        0 => Err(format!("the engine recorded no {what}")),
        _ if items.windows(2).all(|w| w[0] == w[1]) => Ok(items.swap_remove(0)),
        n => Err(format!("{n} different {what} in one call")),
    }
}

/// The facets the engine recorded at its own sites. A facet the
/// observations do not settle is left out, with the reason.
pub fn engine_facets(observations: &[Observation]) -> Facets {
    let mut f = Facets::default();
    let mut prompts = Vec::new();
    let mut samplers = Vec::new();
    let mut generated = Vec::new();
    let mut prefill: Option<u64> = None;
    for o in observations {
        match o {
            Observation::PromptTokens { ids } => prompts.push(ids.clone()),
            Observation::Sampler {
                params,
                order,
                grammar,
            } => samplers.push((params.clone(), order.clone(), grammar.clone())),
            Observation::GeneratedTokens { ids } => generated.push(ids.clone()),
            Observation::Prefill { evaluated, .. } => {
                *prefill.get_or_insert(0) += evaluated;
            }
            Observation::WireRequest { .. } | Observation::WireResponse { .. } => {}
        }
    }
    let prompt = single(prompts, "prompt tokenizations");
    let sampler = single(samplers, "samplers");
    let tokens = single(generated, "generated-token runs");
    let prefill = prefill.ok_or_else(|| "the engine recorded no prefill".to_string());
    for (facet, why) in [
        ("prompt_ids", prompt.as_ref().err()),
        ("sampler", sampler.as_ref().err()),
        ("grammar", sampler.as_ref().err()),
        ("greedy_tokens", tokens.as_ref().err()),
        ("prefill_evaluated", prefill.as_ref().err()),
    ] {
        if let Some(why) = why {
            f.unobserved.insert(facet.to_string(), why.clone());
        }
    }
    f.prompt_ids = prompt.ok();
    f.greedy_tokens = tokens.ok();
    f.prefill_evaluated = prefill.ok();
    if let Ok((params, order, grammar)) = sampler {
        f.sampler = Some(Sampler { params, order });
        f.grammar = grammar;
    }
    f
}

fn answer_facets(case: &Case, method: ReplayMethod, answer: &Value, f: &mut Facets) {
    if method.is_completion() {
        let output = if method.is_stream() {
            stream_output(answer, &tools(case))
        } else {
            completion_output(answer, &tools(case))
        };
        // A forced choice answers with its label probabilities as JSON text.
        if case.input["structured_output"][forced_choice::SENTINEL] == true {
            f.label_probs = forced_choice::parse(answer["text"].as_str().unwrap_or(""));
            if f.label_probs.is_none() {
                f.unobserved.insert(
                    "label_probs".into(),
                    "a forced choice answered with text that is not label probabilities".into(),
                );
            }
        }
        f.output = Some(output);
        return;
    }
    match method {
        ReplayMethod::Embed | ReplayMethod::EmbedQuery | ReplayMethod::EmbedBatch => {
            f.embeddings = serde_json::from_value(answer.clone()).ok();
        }
        ReplayMethod::RerankBatch => {
            f.rerank_scores = serde_json::from_value(answer.clone()).ok();
        }
        ReplayMethod::CountTokens => f.token_count = answer.as_u64(),
        ReplayMethod::Host => {
            if let Value::Object(map) = answer {
                f.host_reported = map.clone().into_iter().collect();
            }
            // Residency is a set; the server side sorts it too
            // (`server::host_truth`).
            if let Some(Value::Array(ids)) = f.host_reported.get_mut("resident_models") {
                ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
            f.resident_bytes = answer["resident_bytes"].as_u64();
        }
        // Completions returned above.
        _ => {}
    }
}

/// The request's tools, whose schemas tell the turn parser which XML
/// parameters are strings.
fn tools(case: &Case) -> Vec<ToolSchema> {
    serde_json::from_value(case.input["tools"].clone()).unwrap_or_default()
}

/// The caller's text, split the way llama-server splits a turn
/// (`oicp_types::chat_turn`), so an inline think block and a separate
/// reasoning field compare as the same thing.
fn split_turn(raw: &str, tools: &[ToolSchema]) -> (String, Option<String>, Vec<ToolCall>) {
    let turn = parse_assistant_turn(raw, tools);
    let calls = turn
        .tool_calls
        .into_iter()
        .map(|c| ToolCall {
            arguments: serde_json::from_str(&c.arguments).unwrap_or(Value::String(c.arguments)),
            name: c.name,
        })
        .collect();
    (turn.content, turn.reasoning_content, calls)
}

/// A `CompletionResponse`, as the adapter's callers read it: completion
/// tokens are `tokens_used - prompt_tokens`
/// (`sovereign-serving-host/src/inference_adapter.rs`, the chat response's
/// `usage`).
pub fn completion_output(answer: &Value, tools: &[ToolSchema]) -> Output {
    let (text, reasoning, tool_calls) = split_turn(answer["text"].as_str().unwrap_or(""), tools);
    Output {
        text,
        reasoning,
        tool_calls,
        finish: answer["finish_reason"].as_str().map(str::to_string),
        usage: completion_usage(answer),
        frames: Vec::new(),
    }
}

/// Usage as the adapter derives it, or none when the answer does not carry
/// both counts.
pub fn completion_usage(answer: &Value) -> Option<Usage> {
    let prompt = answer["prompt_tokens"].as_u64()?;
    let used = answer["tokens_used"].as_u64()?;
    Some(Usage {
        prompt,
        completion: used.saturating_sub(prompt),
    })
}

/// A typed stream as the replay route renders its frames: `token`,
/// `finish` (with `reason` and `usage`) and `error`. The frame kinds listed
/// are those the caller could act on: `reasoning` and `tool-calls` when the
/// streamed text carries them, `usage` when the finish frame does.
pub fn stream_output(frames: &Value, tools: &[ToolSchema]) -> Output {
    let frames = frames.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut raw = String::new();
    let mut kinds: Vec<String> = Vec::new();
    let mut finish = None;
    let mut usage = None;
    let push = |k: &str, kinds: &mut Vec<String>| {
        if !kinds.iter().any(|x| x == k) {
            kinds.push(k.to_string());
        }
    };
    for frame in frames {
        match frame["kind"].as_str() {
            Some("token") => {
                raw.push_str(frame["text"].as_str().unwrap_or(""));
                push("token", &mut kinds);
            }
            Some("finish") => {
                push("finish", &mut kinds);
                finish = frame["reason"].as_str().map(str::to_string);
                if finish.as_deref() == Some("error") {
                    push("error", &mut kinds);
                }
                let u = &frame["usage"];
                if let (Some(prompt), Some(completion)) =
                    (u["prompt"].as_u64(), u["completion"].as_u64())
                {
                    push("usage", &mut kinds);
                    usage = Some(Usage { prompt, completion });
                }
            }
            Some("error") => push("error", &mut kinds),
            _ => {}
        }
    }
    let (text, reasoning, tool_calls) = split_turn(&raw, tools);
    if reasoning.is_some() {
        push("reasoning", &mut kinds);
    }
    if !tool_calls.is_empty() {
        push("tool-calls", &mut kinds);
    }
    Output {
        text,
        reasoning,
        tool_calls,
        finish,
        usage,
        frames: kinds,
    }
}

/// What a remote server returned on the wire for one call, before the client
/// projected it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireReply {
    /// The request bodies the client sent, in order.
    pub requests: Vec<Value>,
    /// Answer text, streamed deltas joined.
    pub content: String,
    /// `reasoning_content`, streamed deltas joined; `None` when the server
    /// sent none.
    pub reasoning: Option<String>,
    /// `usage.prompt_tokens` of the last response that carried one.
    pub prompt_tokens: Option<u64>,
    /// `timings.prompt_n` summed over the call's responses: prompt tokens
    /// the server evaluated, its cache hits excluded.
    pub prompt_evaluated: Option<u64>,
}

/// Read the wire observations of one call. `None` when the client sent
/// nothing, which is what an embedded target looks like.
pub fn wire_reply(observations: &[Observation]) -> Option<WireReply> {
    let mut reply = WireReply::default();
    let mut any = false;
    for o in observations {
        match o {
            Observation::WireRequest { body } => {
                any = true;
                reply.requests.push(body.clone());
            }
            Observation::WireResponse { body, .. } => {
                any = true;
                read_response(body, &mut reply);
            }
            _ => {}
        }
    }
    any.then_some(reply)
}

fn read_response(body: &str, reply: &mut WireReply) {
    let chunks: Vec<Value> = if body.trim_start().starts_with('{') {
        serde_json::from_str(body).into_iter().collect()
    } else {
        body.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|d| *d != "[DONE]")
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect()
    };
    // A new response replaces the text of an earlier one: the call's answer
    // is its last.
    let mut content = String::new();
    let mut reasoning: Option<String> = None;
    for chunk in &chunks {
        let choice = &chunk["choices"][0];
        let part = if choice["delta"].is_object() {
            &choice["delta"]
        } else {
            &choice["message"]
        };
        if let Some(t) = part["content"].as_str() {
            content.push_str(t);
        }
        if let Some(t) = part["reasoning_content"].as_str() {
            reasoning.get_or_insert_with(String::new).push_str(t);
        }
        if let Some(n) = chunk["usage"]["prompt_tokens"].as_u64() {
            reply.prompt_tokens = Some(n);
        }
    }
    // Timings are cumulative for the task and a stream repeats them on its
    // last chunks, so each response counts once, from its last.
    if let Some(n) = chunks
        .iter()
        .rev()
        .find_map(|c| c["timings"]["prompt_n"].as_u64())
    {
        *reply.prompt_evaluated.get_or_insert(0) += n;
    }
    reply.content = content;
    reply.reasoning = reasoning;
}

/// The think block the model generated, for the decode rows: the wire's
/// `reasoning_content` on a remote target, else the think block in the text
/// the server or the engine returned.
pub fn generated_reasoning(output: Option<&Output>, wire: Option<&WireReply>) -> Option<String> {
    match wire {
        Some(w) => Some(match &w.reasoning {
            Some(r) => r.clone(),
            None => split_turn(&w.content, &[]).1.unwrap_or_default(),
        }),
        None => output.map(|o| o.reasoning.clone().unwrap_or_default()),
    }
}
