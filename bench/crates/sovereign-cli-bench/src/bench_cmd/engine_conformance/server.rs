// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a llama-server says it did with a call the daemon's remote client
//! sent it, asked of the server itself:
//!
//! - the prompt ids: `/apply-template` then `/tokenize` on the recorded wire
//!   body, held to the response's own `usage.prompt_tokens`;
//! - the sampler and grammar: the `/slots` entry of the last task, read over
//!   the same stage keys the engine side reports
//!   ([`SAMPLER_STAGE_PARAMS`]);
//! - prompt tokens evaluated: `timings.prompt_n` from the responses
//!   ([`WireReply::prompt_evaluated`]);
//! - the host truth: `/props` and `/models`.
//!
//! The battery runs one call at a time, so the slot with the newest task is
//! the call's own.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use sovereign_contracts::engine_observe::{canonical_sampler, SAMPLER_STAGE_PARAMS};

use super::project::WireReply;
use super::record::{Facets, Sampler};

/// A llama-server, or a router in front of several.
pub struct LlamaServer {
    base: String,
    http: reqwest::Client,
}

impl LlamaServer {
    /// A server at `base` (`http://127.0.0.1:8080`).
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("a reqwest client with a timeout builds"),
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder, path: &str) -> Result<Value, String> {
        let resp = req.send().await.map_err(|e| format!("{path}: {e}"))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| format!("{path}: {e}"))?;
        if !status.is_success() {
            return Err(format!("{path}: HTTP {status}: {body}"));
        }
        serde_json::from_str(&body).map_err(|e| format!("{path}: {e}"))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        let url = format!("{}{path}", self.base);
        self.send(self.http.post(url).json(body), path).await
    }

    async fn get(&self, path: &str, model: Option<&str>) -> Result<Value, String> {
        let url = format!("{}{path}", self.base);
        let mut req = self.http.get(url);
        if let Some(m) = model {
            req = req.query(&[("model", m)]);
        }
        self.send(req, path).await
    }

    /// The token ids the server's own template and tokenizer give the body.
    async fn prompt_ids(&self, body: &Value) -> Result<Vec<i64>, String> {
        let prompt = if body.get("messages").is_some() {
            self.post("/apply-template", body).await?["prompt"].clone()
        } else {
            body["prompt"].clone()
        };
        if let Some(ids) = prompt.as_array().filter(|a| a.iter().all(Value::is_i64)) {
            return Ok(ids.iter().filter_map(Value::as_i64).collect());
        }
        let Some(text) = prompt.as_str() else {
            return Err(format!("no prompt text to tokenize: {prompt}"));
        };
        self.tokenize(text, true, body.get("model")).await
    }

    /// `/tokenize`, special tokens parsed.
    pub async fn tokenize(
        &self,
        text: &str,
        add_special: bool,
        model: Option<&Value>,
    ) -> Result<Vec<i64>, String> {
        let mut req = json!({"content": text, "add_special": add_special, "parse_special": true});
        if let Some(m) = model {
            req["model"] = m.clone();
        }
        let resp = self.post("/tokenize", &req).await?;
        resp["tokens"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .ok_or_else(|| format!("/tokenize returned no tokens: {resp}"))
    }

    /// Add what the server says it did to a remote target's facets.
    pub async fn observe(&self, wire: &WireReply, f: &mut Facets) {
        let body = match wire.requests.as_slice() {
            [] => Err("the client sent no request".to_string()),
            [one] => Ok(one),
            many if many.windows(2).all(|w| w[0] == w[1]) => Ok(&many[0]),
            many => Err(format!("the client sent {} different requests", many.len())),
        };
        let model = body.as_ref().ok().and_then(|b| b["model"].as_str());
        let prompt = match &body {
            Ok(b) => self
                .prompt_ids(b)
                .await
                .and_then(|ids| prompt_oracle(ids, wire.prompt_tokens)),
            Err(why) => Err(why.clone()),
        };
        let slot = match &body {
            Ok(_) => self
                .get("/slots", model)
                .await
                .and_then(|s| slot_sampler(&s)),
            Err(why) => Err(why.clone()),
        };
        settle(f, "prompt_ids", prompt, |f, ids| f.prompt_ids = Some(ids));
        let grammar = slot.as_ref().map(|(_, g)| g.clone()).map_err(Clone::clone);
        settle(f, "sampler", slot.map(|(s, _)| s), |f, s| {
            f.sampler = Some(s)
        });
        settle(f, "grammar", grammar, |f, g| f.grammar = g);
        let prefill = wire
            .prompt_evaluated
            .ok_or_else(|| "no response carried timings.prompt_n".to_string());
        settle(f, "prefill_evaluated", prefill, |f, n| {
            f.prefill_evaluated = Some(n)
        });
        f.unobserved.insert(
            "greedy_tokens".into(),
            "a chat completion returns no token ids; the probe observes them".into(),
        );
    }

    /// The server's own account of the keys the host rows compare, for
    /// those it can answer.
    pub async fn host_observed(
        &self,
        reported: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, String> {
        let primary = reported.get("model_for_slow").and_then(Value::as_str);
        let models = self.get("/models", None).await?;
        let props = self.get("/props", primary).await?;
        Ok(host_truth(reported, &props, &models))
    }
}

/// Fill a facet, or record why it stays empty.
fn settle<T>(f: &mut Facets, facet: &str, r: Result<T, String>, fill: impl FnOnce(&mut Facets, T)) {
    match r {
        Ok(v) => {
            f.unobserved.remove(facet);
            fill(f, v);
        }
        Err(why) => {
            tracing::debug!(target: "engine_conformance", facet, %why, "server facet unobserved");
            f.unobserved.insert(facet.to_string(), why);
        }
    }
}

/// Hold the re-derived prompt to the count the server reported for the call
/// itself. When the two disagree the ids are not the prompt the model was
/// given, and saying so beats judging them.
pub fn prompt_oracle(ids: Vec<i64>, prompt_tokens: Option<u64>) -> Result<Vec<i64>, String> {
    match prompt_tokens {
        Some(n) if n != ids.len() as u64 => Err(format!(
            "/tokenize gave {} ids, the response's usage says {n}",
            ids.len()
        )),
        Some(_) => Ok(ids),
        None => {
            tracing::debug!(target: "engine_conformance", "no usage.prompt_tokens to hold the prompt ids to");
            Ok(ids)
        }
    }
}

/// The sampler and grammar of the slot that ran the newest task.
pub fn slot_sampler(slots: &Value) -> Result<(Sampler, Option<String>), String> {
    let slot = slots
        .as_array()
        .and_then(|a| {
            a.iter()
                .filter(|s| s["id_task"].is_i64())
                .max_by_key(|s| s["id_task"].as_i64())
        })
        .ok_or_else(|| "/slots shows no slot that has run a task".to_string())?;
    let params = &slot["params"];
    let raw: BTreeMap<String, f64> = SAMPLER_STAGE_PARAMS
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .filter_map(|k| Some((k.to_string(), params[*k].as_f64()?)))
        .collect();
    let order: Vec<&str> = params["samplers"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let (params_c, order_c) = canonical_sampler(&raw, &order);
    let grammar = params["grammar"]
        .as_str()
        .filter(|g| !g.is_empty())
        .map(str::to_string);
    Ok((
        Sampler {
            params: params_c,
            order: order_c,
        },
        grammar,
    ))
}

/// The server's truth for the host keys it can speak to. A model id is true
/// when the server serves it; otherwise the truth is the list it does serve.
/// Residency is a set, so it is sorted.
pub fn host_truth(
    reported: &BTreeMap<String, Value>,
    props: &Value,
    models: &Value,
) -> BTreeMap<String, Value> {
    let data = models["data"].as_array().cloned().unwrap_or_default();
    let names = |m: &Value| -> Vec<String> {
        std::iter::once(&m["id"])
            .chain(m["aliases"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let served: Vec<String> = data.iter().flat_map(names).collect();
    let mut out = BTreeMap::new();
    for key in ["model_for_fast", "model_for_slow", "code_model"] {
        if let Some(id) = reported.get(key) {
            let truth = match id.as_str() {
                Some(s) if served.iter().any(|x| x == s) => id.clone(),
                _ => json!(served),
            };
            out.insert(key.to_string(), truth);
        }
    }
    if let Some(n) = props["default_generation_settings"]["n_ctx"].as_u64() {
        out.insert("effective_context_size".into(), json!(n));
    }
    let primary = reported.get("model_for_slow").and_then(Value::as_str);
    let meta = data
        .iter()
        .find(|m| primary.is_some_and(|p| names(m).iter().any(|x| x == p)))
        .or(if data.len() == 1 { data.first() } else { None })
        .map(|m| &m["meta"]);
    if let Some(n) = meta.and_then(|m| m["n_ctx_train"].as_u64()) {
        out.insert("n_ctx_train".into(), json!(n));
    }
    // A router reports each model's status; a single server lists only the
    // model it holds.
    let mut resident: Vec<String> = data
        .iter()
        .filter(
            |m| match m["status"]["value"].as_str().or(m["status"].as_str()) {
                Some(s) => s == "loaded",
                None => true,
            },
        )
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    resident.sort();
    out.insert("resident_models".into(), json!(resident));
    out
}
