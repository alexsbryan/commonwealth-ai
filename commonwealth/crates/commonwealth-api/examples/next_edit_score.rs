// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon-free next-edit scorer — Phase 0 of the build-vs-adopt bakeoff
//! (`sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §7, §8 item 1).
//!
//! Serves `POST /v1/edit_predictions` with the **same pipeline the
//! daemon runs** (`commonwealth_api::routes_edit_predictions::
//! predict_response`), forwarding the one impure step — inference — to
//! any OpenAI-compatible endpoint. That is the whole design: a
//! candidate checkpoint is scored by the daemon's own decisions, not by
//! a second implementation's opinion of them, so a bakeoff number is a
//! statement about the model rather than about the harness.
//!
//! Because it speaks the route's contract exactly, **both existing eval
//! banks run against it unchanged** — the pre-registered gates in
//! `gym/next-edit/README.md` (G1–G4) and `gym/next-edit/gen/README.md`
//! (GM1–GM5) apply verbatim to whatever model is behind `--upstream`:
//!
//! ```text
//! # terminal 1 — the candidate, on llama.cpp
//! llama-server -m sweep-next-edit-1.5B.Q8_0.gguf --port 8089
//!
//! # terminal 2 — the scorer, speaking the daemon's contract
//! cargo run -p commonwealth-api --example next_edit_score -- \
//!     --upstream http://127.0.0.1:8089 --format sweep \
//!     --model-id sweep-1.5B --port 9799
//!
//! # terminal 3 — the pre-registered gates, unmodified
//! python3 scripts/next_edit_gen_eval.py --endpoint http://127.0.0.1:9799
//! python3 scripts/next_edit_eval.py     --endpoint http://127.0.0.1:9799
//! ```
//!
//! Two deliberate departures from the daemon, both because they measure
//! the *deployment*, not the model, and both reported in the banner:
//!
//! - **`--concurrency` waits instead of dropping.** The daemon's
//!   one-in-flight semaphore reports `busy` so ghost text and chat win
//!   the slot; here a queued consult would score as a spurious drop and
//!   defame the candidate. Permits are awaited, never refused, so
//!   `busy` can never appear in a bakeoff result.
//! - **`--timeout-ms` is settable.** The daemon's 15 s budget is an
//!   interactive-latency choice. Latency is measured and reported by
//!   the banks (GM5); it must not silently become a correctness drop on
//!   a cold or oversubscribed candidate.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use commonwealth_api::routes_edit_predictions::{
    predict_response, validate_wire, EditPredictionsRequestWire, InferError, InferenceCall,
    ModelSlot,
};
use commonwealth_api::next_edit_model::Prompt;

#[derive(Clone)]
struct Cfg {
    upstream: String,
    format: String,
    model_id: String,
    timeout_ms: u64,
    http: reqwest::Client,
    slot: Arc<tokio::sync::Semaphore>,
}

fn usage() -> ! {
    eprintln!(
        "next_edit_score — serve the daemon's next-edit pipeline over an arbitrary model\n\
         \n\
         REQUIRED\n  \
           --upstream URL      OpenAI-compatible base (e.g. http://127.0.0.1:8089)\n\
         OPTIONS\n  \
           --format FMT        wire dialect: region_instruct | zeta2 | sweep   [region_instruct]\n  \
           --model-id NAME     reported in sovereign_debug.model_id            [<upstream model>]\n  \
           --port N            listen port                                     [9799]\n  \
           --concurrency N     in-flight consults; queued, never refused       [1]\n  \
           --timeout-ms N      per-consult budget                              [15000]\n"
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let mut upstream = None;
    let mut format = "region_instruct".to_string();
    let mut model_id = None;
    let mut port: u16 = 9799;
    let mut concurrency: usize = 1;
    let mut timeout_ms: u64 = 15_000;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        // Every flag here takes exactly one value; a missing one is a
        // usage error rather than a default, because silently defaulting
        // `--format` or `--upstream` would score something other than
        // what the operator asked for.
        let mut value = || -> String {
            args.get(i + 1).cloned().unwrap_or_else(|| {
                eprintln!("{flag} requires a value");
                usage()
            })
        };
        match flag {
            "--upstream" => {
                upstream = Some(value());
                i += 1;
            }
            "--format" => {
                format = value();
                i += 1;
            }
            "--model-id" => {
                model_id = Some(value());
                i += 1;
            }
            "--port" => {
                port = value().parse().unwrap_or_else(|_| usage());
                i += 1;
            }
            "--concurrency" => {
                concurrency = value().parse().unwrap_or_else(|_| usage());
                i += 1;
            }
            "--timeout-ms" => {
                timeout_ms = value().parse().unwrap_or_else(|_| usage());
                i += 1;
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument: {other}");
                usage()
            }
        }
        i += 1;
    }
    let Some(upstream) = upstream else {
        eprintln!("--upstream is required");
        usage()
    };
    let upstream = upstream.trim_end_matches('/').to_string();

    // Refuse an unknown dialect rather than silently falling through to
    // the instruct format — a bakeoff that scored Sweep through the
    // wrong prompt would produce a confident, wrong verdict, which is
    // the failure this whole exercise exists to avoid.
    if !matches!(format.as_str(), "region_instruct" | "zeta2" | "sweep") {
        eprintln!(
            "unknown --format {format:?}: expected region_instruct | zeta2 | sweep\n\
             (a mis-dialled format scores the prompt, not the model — refusing rather than \
             guessing)"
        );
        std::process::exit(2);
    }

    let model_id = model_id.unwrap_or_else(|| "upstream".to_string());
    let cfg = Cfg {
        upstream: upstream.clone(),
        format: format.clone(),
        model_id: model_id.clone(),
        timeout_ms,
        http: reqwest::Client::new(),
        slot: Arc::new(tokio::sync::Semaphore::new(concurrency.max(1))),
    };

    let app = axum::Router::new()
        .route("/v1/edit_predictions", post(handle))
        .with_state(cfg);
    let addr = format!("127.0.0.1:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind {addr} failed: {e}");
            std::process::exit(1)
        }
    };

    // Glassbox: every number this process produces is conditioned on
    // these five values, so they are stated before the first request
    // rather than inferred afterwards from a shell history.
    eprintln!("next_edit_score — the daemon's pipeline, an arbitrary model");
    eprintln!("  listening    http://{addr}/v1/edit_predictions");
    eprintln!("  upstream     {upstream}");
    eprintln!("  format       {format}");
    eprintln!("  model_id     {model_id}");
    eprintln!("  concurrency  {concurrency} (queued, never refused — no `busy` drops)");
    eprintln!("  timeout      {timeout_ms} ms");
    eprintln!();
    eprintln!("  score it:  python3 scripts/next_edit_gen_eval.py --endpoint http://{addr}");

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("serve failed: {e}");
        std::process::exit(1);
    }
}

async fn handle(State(cfg): State<Cfg>, Json(wire): Json<EditPredictionsRequestWire>) -> Response {
    let started = std::time::Instant::now();
    if let Err(msg) = validate_wire(&wire) {
        return (axum::http::StatusCode::BAD_REQUEST, msg).into_response();
    }

    // The slot is always advertised: this process exists only because a
    // candidate is running behind it, so "no model" is an upstream
    // failure to be reported as `unavailable`, never a configuration
    // state to be inferred.
    let model = Some(ModelSlot {
        model_id: cfg.model_id.clone(),
        slot: "scorer".to_string(),
        format: cfg.format.clone(),
    });

    let out = predict_response(&wire, model, started, |call| async move {
        // Permits are AWAITED, not tried: see the module doc. A queued
        // consult is a throughput fact, not a model defect.
        let _permit = cfg.slot.clone().acquire_owned().await.map_err(|_| InferError("error"))?;
        let fut = upstream_call(&cfg, call);
        match tokio::time::timeout(std::time::Duration::from_millis(cfg.timeout_ms), fut).await {
            Err(_) => Err(InferError("timeout")),
            Ok(r) => r,
        }
    })
    .await;

    Json(out.body).into_response()
}

/// One inference against the upstream, in the dialect the plan chose.
/// Errors are mapped to the same closed drop-set the daemon reports, so
/// a bank sees `unavailable` / `timeout` / `error` and nothing novel.
async fn upstream_call(
    cfg: &Cfg,
    call: InferenceCall,
) -> Result<(String, Option<String>), InferError> {
    // Raw prompts go to the completion endpoint verbatim — a chat
    // template would wrap the fine-tune's special tokens in a user turn
    // and the model would never see its trained shape.
    let (url, body, chat) = match &call.prompt {
        Prompt::Raw(raw) => (
            format!("{}/v1/completions", cfg.upstream),
            serde_json::json!({
                "model": call.model_id,
                "prompt": raw,
                "max_tokens": call.max_tokens,
                "temperature": call.temperature,
                "stop": call.stop,
                "stream": false,
            }),
            false,
        ),
        Prompt::Chat(prompt) => (
            format!("{}/v1/chat/completions", cfg.upstream),
            serde_json::json!({
                "model": call.model_id,
                "messages": [{ "role": "user", "content": prompt }],
                "max_tokens": call.max_tokens,
                "temperature": call.temperature,
                "stream": false,
            }),
            true,
        ),
    };

    let resp = cfg.http.post(&url).json(&body).send().await.map_err(|e| {
        eprintln!("upstream unreachable: {e}");
        InferError("unavailable")
    })?;
    if !resp.status().is_success() {
        let code = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        eprintln!("upstream {code}: {}", detail.chars().take(300).collect::<String>());
        return Err(InferError("error"));
    }
    let payload: serde_json::Value = resp.json().await.map_err(|e| {
        eprintln!("upstream returned non-JSON: {e}");
        InferError("error")
    })?;

    let choice = payload
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| {
            eprintln!("upstream response has no choices[0]");
            InferError("error")
        })?;
    let content = if chat {
        choice.pointer("/message/content").and_then(|v| v.as_str())
    } else {
        choice.get("text").and_then(|v| v.as_str())
    };
    // An absent content field is an upstream contract break, not an
    // empty completion: reporting it as `""` would be scored as a noop
    // and silently credited to the model.
    let Some(content) = content else {
        eprintln!("upstream choice carried no content field: {choice}");
        return Err(InferError("error"));
    };
    let finish = choice.get("finish_reason").and_then(|v| v.as_str()).map(str::to_string);
    Ok((content.to_string(), finish))
}
