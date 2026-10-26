// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench engine-conformance run`: one pass of the case bank over one
//! target, through the daemon that serves it.
//!
//! Each case is replayed on the daemon's `/internal/engine/replay`, which
//! returns the answer and what the engine recorded at its own sites. For a
//! remote target the driver then asks the server what it did with the body
//! the client sent ([`super::server`]). A case the driver cannot run yet is
//! skipped by name, and leaves no record, so its rows read never-ran.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sovereign_contracts::engine_observe::{ProbeAnswer, ProbeRequest, ReplayMethod};
use sovereign_contracts::types::Speed;

use super::case::{Case, CaseMethod};
use super::project::{self, record_lifecycle, Replay};
use super::record::{CaseRecord, Outcome};
use super::server::LlamaServer;

/// The daemon's replay route, on its operator listener.
struct Daemon {
    url: String,
    http: reqwest::Client,
}

impl Daemon {
    fn new(base: &str) -> Self {
        Self {
            url: format!("{}/internal/engine/replay", base.trim_end_matches('/')),
            // A slow-model completion with a long prompt runs for minutes.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(900))
                .build()
                .expect("a reqwest client with a timeout builds"),
        }
    }

    /// Replay one call. `Err` is the route refusing or failing, not the
    /// engine: the engine's refusals come back inside `Replay::outcome`.
    async fn replay(&self, method: ReplayMethod, input: &Value) -> Result<Replay, String> {
        let resp = self
            .http
            .post(&self.url)
            .json(&json!({"method": method, "input": input}))
            .send()
            .await
            .map_err(|e| format!("replay: {e}"))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| format!("replay: {e}"))?;
        if !status.is_success() {
            return Err(format!("replay: HTTP {status}: {body}"));
        }
        serde_json::from_str(&body).map_err(|e| format!("replay: {e}"))
    }
}

struct Args {
    target: String,
    daemon: String,
    server: Option<String>,
    cases: PathBuf,
    out: PathBuf,
    only: Vec<String>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let (mut target, mut daemon, mut server, mut cases, mut out) = (None, None, None, None, None);
    let mut only = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let v = it
            .next()
            .ok_or_else(|| format!("`{arg}` needs a value"))?
            .clone();
        match arg.as_str() {
            "--target" => target = Some(v),
            "--daemon" => daemon = Some(v),
            "--server" => server = Some(v),
            "--cases" => cases = Some(PathBuf::from(v)),
            "--out" => out = Some(PathBuf::from(v)),
            "--only" => only.push(v),
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    Ok(Args {
        target: target.ok_or("--target is required")?,
        daemon: daemon.ok_or("--daemon is required")?,
        server,
        cases: cases.ok_or("--cases is required")?,
        out: out.ok_or("--out is required")?,
        only,
    })
}

/// Entry point. Exit 0 when the pass ran, 1 when no case could be replayed,
/// 2 on a usage or input error.
pub async fn cmd_run(args: &[String]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let bank = match super::read_jsonl::<Case>(&a.cases) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let bank: Vec<Case> = bank
        .into_iter()
        .filter(|c| a.only.is_empty() || a.only.contains(&c.case_id))
        .collect();
    let daemon = Daemon::new(&a.daemon);
    let server = a.server.as_deref().map(LlamaServer::new);
    let mut records = Vec::new();
    let mut skipped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (i, case) in bank.iter().enumerate() {
        let started = Instant::now();
        let result = run_case(&daemon, server.as_ref(), &a.target, case).await;
        let ms = started.elapsed().as_millis();
        match result {
            Ok(rec) => {
                eprintln!(
                    "[{}/{}] {} {} {:?} {} ({ms} ms)",
                    i + 1,
                    bank.len(),
                    case.case_id,
                    case.label,
                    case.method,
                    outcome_word(&rec.outcome)
                );
                records.push(rec);
            }
            Err(why) => {
                eprintln!("[{}/{}] {} skipped: {why}", i + 1, bank.len(), case.case_id);
                skipped.entry(why).or_default().push(case.case_id.clone());
            }
        }
    }
    if let Err(e) = write(&a.out, &records) {
        eprintln!("error: {e}");
        return 2;
    }
    println!(
        "{}: {} of {} cases recorded to {}",
        a.target,
        records.len(),
        bank.len(),
        a.out.display()
    );
    for (why, ids) in &skipped {
        println!("  skipped {}: {why}", ids.len());
    }
    if records.is_empty() && !bank.is_empty() {
        1
    } else {
        0
    }
}

fn outcome_word(o: &Outcome) -> &'static str {
    match o {
        Outcome::Ok => "ok",
        Outcome::Refused { .. } => "refused",
        Outcome::Error { .. } => "error",
    }
}

/// One case to one record, or the reason it was not run.
async fn run_case(
    daemon: &Daemon,
    server: Option<&LlamaServer>,
    target: &str,
    case: &Case,
) -> Result<CaseRecord, String> {
    if let Some(fault) = &case.fault {
        if fault == MID_STREAM_ERROR {
            return mid_stream_error(daemon, target, case).await;
        }
        return Err(format!("fault `{fault}` is not injected by the driver yet"));
    }
    let method = match case.method {
        CaseMethod::Replay(m) => m,
        CaseMethod::Throughput => return throughput(daemon, target, case).await,
        CaseMethod::Admission => return admission(daemon, target, case).await,
        CaseMethod::SplitLine => return split_line(daemon, target, case).await,
        CaseMethod::Probe => return probe(daemon, server, target, case).await,
    };
    // A lifecycle case names the extra GGUF by file name in
    // SOVEREIGN_MODELS_DIR (a path in a bank is a host's absolute path);
    // resolve it here, the way the e2e's ROLES do, so the daemon is asked to
    // load a path this host has.
    let input = if method == ReplayMethod::HostLifecycle {
        resolved_lifecycle_input(&case.input)
    } else {
        case.input.clone()
    };
    let replay = daemon.replay(method, &input).await?;
    let mut rec = project::record(case, method, target, &replay);
    let wire = project::wire_reply(&replay.observations);
    match (&wire, server) {
        (Some(w), Some(s)) => s.observe(w, &mut rec.facets).await,
        (Some(_), None) => {
            for facet in ["prompt_ids", "sampler", "grammar", "prefill_evaluated"] {
                rec.facets.unobserved.insert(
                    facet.into(),
                    "a remote target, and no --server to ask".into(),
                );
            }
        }
        (None, _) => {}
    }
    if method == ReplayMethod::Host || method == ReplayMethod::HostLifecycle {
        if let Some(s) = server {
            match s.host_observed(&rec.facets.host_reported).await {
                Ok(truth) => rec.facets.host_observed = truth,
                Err(why) => {
                    rec.facets.unobserved.insert("host_observed".into(), why);
                }
            }
        }
        if method == ReplayMethod::HostLifecycle {
            record_lifecycle(&replay.answer, &mut rec.facets);
            // The residency the report carries is the router's state after
            // the calls; without a server there is no other reading of it.
            if rec.facets.host_observed.is_empty() {
                rec.facets.unobserved.insert(
                    "host_observed".into(),
                    "no --server to read the state after the lifecycle calls".into(),
                );
            }
        }
    }
    if method.is_completion() && rec.outcome == Outcome::Ok {
        let reasoning = project::generated_reasoning(rec.facets.output.as_ref(), wire.as_ref());
        // The router routes /tokenize by model, as it routes the call.
        let model = wire
            .as_ref()
            .and_then(|w| w.requests.last())
            .map(|b| b["model"].clone());
        match reasoning_tokens(daemon, server, model.as_ref(), reasoning.as_deref()).await {
            Ok(n) => rec.facets.reasoning_tokens = n,
            Err(why) => {
                rec.facets.unobserved.insert("reasoning_tokens".into(), why);
            }
        }
        if method.is_stream() {
            // `projection.stream-usage`'s instrument: the count the finish
            // frame carried, and what it can be held to. The answer's own
            // length comes from the target's tokenizer; without one the
            // stream's delta count is the only tie, and it is recorded too.
            if let Some(usage) = rec.facets.output.as_ref().and_then(|o| o.usage.clone()) {
                // The answer a completion counts is everything the model
                // generated: its text, its think block, and a tool call's
                // name and arguments. A turn that answered with a call or a
                // think block and no prose is not an empty answer.
                let answer = rec.facets.output.as_ref().map(|o| {
                    let mut s = o.reasoning.clone().unwrap_or_default();
                    s.push_str(&o.text);
                    for call in &o.tool_calls {
                        s.push_str(&call.name);
                        s.push_str(&call.arguments.to_string());
                    }
                    s
                });
                let answer_tokens = match answer.as_deref() {
                    Some(generated) if !generated.is_empty() => {
                        match reasoning_tokens(daemon, server, model.as_ref(), Some(generated))
                            .await
                        {
                            Ok(n) => n,
                            Err(why) => {
                                rec.facets
                                    .unobserved
                                    .insert("stream_usage.answer_tokens".into(), why);
                                None
                            }
                        }
                    }
                    _ => Some(0),
                };
                let deltas = rec
                    .facets
                    .output
                    .as_ref()
                    .map(|o| o.frames.iter().filter(|k| *k == "token").count() as u64)
                    .unwrap_or(0);
                tracing::debug!(
                    target: "engine_conformance",
                    completion = usage.completion,
                    ?answer_tokens,
                    deltas,
                    "stream usage: the finish frame's count and what it can be held to"
                );
                rec.facets.stream_usage = Some(super::record::StreamUsageFacet {
                    completion: usage.completion,
                    // One `token` frame is one delta; a stream that emitted
                    // the answer in one frame still counts one, so the frame
                    // count alone is not the answer's length.
                    answer_tokens,
                    deltas,
                });
            } else {
                rec.facets.unobserved.insert(
                    "stream_usage".into(),
                    "the finish frame carried no usage counts".into(),
                );
            }
        }
    }
    Ok(rec)
}

/// `host.lifecycle`'s input with the extra slot's GGUF resolved. The bank
/// names a file in `SOVEREIGN_MODELS_DIR` (never an absolute path, which is
/// one host's); an absolute path is taken as given.
fn resolved_lifecycle_input(input: &Value) -> Value {
    let mut out = input.clone();
    let Some(path) = input["extra"]["path"].as_str() else {
        return out;
    };
    let resolved = std::path::Path::new(path);
    if resolved.is_absolute() {
        return out;
    }
    let Some(dir) = std::env::var_os("SOVEREIGN_MODELS_DIR").map(std::path::PathBuf::from) else {
        // No directory to resolve against: leave the name as the bank wrote
        // it, and the daemon's own refusal is the answer (never a silence).
        tracing::debug!(target: "engine_conformance", path, "SOVEREIGN_MODELS_DIR unset; lifecycle extra path left as named");
        return out;
    };
    out["extra"]["path"] = json!(dir.join(resolved).to_string_lossy());
    out
}

/// The think block's length in tokens, by the target's own tokenizer: the
/// server's when there is one, else the engine's `count_tokens`.
async fn reasoning_tokens(
    daemon: &Daemon,
    server: Option<&LlamaServer>,
    model: Option<&Value>,
    reasoning: Option<&str>,
) -> Result<Option<u64>, String> {
    let Some(text) = reasoning else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(Some(0));
    }
    if let Some(s) = server {
        return s
            .tokenize(text, false, model)
            .await
            .map(|ids| Some(ids.len() as u64));
    }
    let replay = daemon
        .replay(ReplayMethod::CountTokens, &json!({"text": text}))
        .await?;
    replay
        .answer
        .as_u64()
        .map(Some)
        .ok_or_else(|| format!("count_tokens answered {}", replay.answer))
}

/// The compute probe: the daemon's engine when it holds weights, else the
/// server behind a remote target, asked for the model the daemon names at
/// the probe's speed.
async fn probe(
    daemon: &Daemon,
    server: Option<&LlamaServer>,
    target: &str,
    case: &Case,
) -> Result<CaseRecord, String> {
    let req: ProbeRequest = serde_json::from_value(case.input.clone())
        .map_err(|e| format!("not a probe request: {e}"))?;
    let replay = daemon.replay(ReplayMethod::Probe, &case.input).await?;
    let answer: Result<ProbeAnswer, String> = match (replay.outcome["kind"].as_str(), server) {
        (Some("ok"), _) => serde_json::from_value(replay.answer).map_err(|e| e.to_string()),
        (Some("refused"), Some(s)) => {
            let host = daemon.replay(ReplayMethod::Host, &json!({})).await?;
            let key = match req.speed {
                Speed::Slow | Speed::Medium => "model_for_slow",
                _ => "model_for_fast",
            };
            let model = host.answer[key]
                .as_str()
                .ok_or_else(|| format!("the host report names no {key}"))?;
            tracing::debug!(target: "engine_conformance", model, "probing the server, the daemon holds no weights");
            s.probe(model, &req).await
        }
        _ => Err(format!("the probe did not run: {}", replay.outcome)),
    };
    let mut rec = CaseRecord {
        case_id: case.case_id.clone(),
        target: target.into(),
        rows: case.rows.clone(),
        request: case.input.clone(),
        outcome: Outcome::Ok,
        facets: Default::default(),
    };
    match answer {
        Ok(a) => project::probe_facets(&a, &mut rec.facets),
        Err(message) => rec.outcome = Outcome::Error { message },
    }
    Ok(rec)
}

/// `concurrency` completions of the same request at once; aggregate
/// generated tokens per second of wall time.
async fn throughput(daemon: &Daemon, target: &str, case: &Case) -> Result<CaseRecord, String> {
    let n = case.input["concurrency"]
        .as_u64()
        .ok_or("throughput needs input.concurrency")?;
    let mut request = case.input.clone();
    if let Value::Object(m) = &mut request {
        m.remove("concurrency");
    }
    let started = Instant::now();
    let replies =
        futures::future::join_all((0..n).map(|_| daemon.replay(ReplayMethod::Complete, &request)))
            .await;
    let wall = started.elapsed().as_secs_f64();
    let mut generated = 0u64;
    let mut failure = None;
    for r in replies {
        let r = r?;
        if r.outcome["kind"] != "ok" {
            failure.get_or_insert_with(|| r.outcome.to_string());
            continue;
        }
        let usage = project::completion_usage(&r.answer)
            .ok_or_else(|| format!("a throughput reply carried no usage: {}", r.answer))?;
        generated += usage.completion;
    }
    let mut rec = CaseRecord {
        case_id: case.case_id.clone(),
        target: target.into(),
        rows: case.rows.clone(),
        request: request.clone(),
        outcome: match failure {
            None => Outcome::Ok,
            Some(message) => Outcome::Error { message },
        },
        facets: Default::default(),
    };
    tracing::debug!(target: "engine_conformance", n, generated, wall, "throughput measured");
    rec.facets.tokens_per_s = Some(generated as f64 / wall);
    Ok(rec)
}

/// The fault a case injects to make a stream fail mid-answer
/// (`projection.stream-error`). The case names it; the driver asks the
/// daemon's `stream_fault` for the cut and corrupt stream and reads it back
/// through the client's own reader, so the verdict is on the client.
const MID_STREAM_ERROR: &str = "mid-stream-error";

/// `cost.admission` (L I3): one slot, two callers. The first — a long
/// generation, the slot's only permit — is held open while a second arrives.
/// The second carries a turn admission (`turn_admission`, the field the
/// client writes for an already-accepted turn); the claim is that an admitted
/// caller PARKS past the slot's bound where an unadmitted one is SHED, and
/// the two calls are queued in that order so the shed is decided first.
/// `scenario::admission` reads the sequence from how each call ended.
///
/// The case names the held call's `think_budget`/`max_tokens` long enough to
/// outlast the bound on the reference engine's own model; a 0-length answer
/// cannot hold the slot and the run reports could-not-judge rather than a
/// false pass.
async fn admission(daemon: &Daemon, target: &str, case: &Case) -> Result<CaseRecord, String> {
    let mut base = case.input.clone();
    let turn = base["admission"]
        .as_str()
        .unwrap_or("es-c2-admission")
        .to_string();
    let mut held = base.clone();
    if let Value::Object(m) = &mut held {
        m.remove("admission");
        m.insert("turn_admission".into(), json!(turn));
    }
    // Start the holder, give it the slot, then arrive behind it with a call
    // that carries no admission: the shed is this second call's.
    let holder = tokio::spawn({
        let d = Daemon {
            url: daemon.url.clone(),
            http: daemon.http.clone(),
        };
        let req = held.clone();
        async move { d.replay(ReplayMethod::Complete, &req).await }
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    let behind = daemon.replay(ReplayMethod::Complete, &base).await;
    let holder = holder
        .await
        .map_err(|e| format!("the admitted holder task failed: {e}"))?;
    let mut facets = super::record::Facets::default();
    let mut outcome = Outcome::Ok;
    let mut seq = Vec::new();
    for (who, r) in [("admitted", holder), ("unadmitted", behind)] {
        let r = r?;
        seq.push(admission_word(who, &r));
        if r.outcome["kind"] != "ok" && outcome == Outcome::Ok {
            outcome = Outcome::Error {
                message: r.outcome.to_string(),
            };
        }
    }
    tracing::debug!(target: "engine_conformance", ?seq, turn, "admission sequence");
    facets.admission = seq;
    Ok(CaseRecord {
        case_id: case.case_id.clone(),
        target: target.into(),
        rows: case.rows.clone(),
        request: base,
        outcome,
        facets,
    })
}

/// How one admission call ended: `shed` for the slot's named refusal
/// (`Error::QueueShed`), `parked` for an admitted call served after it, and
/// `served` for an unadmitted call the slot took anyway (idle host — not the
/// failure the row names, which the scenario reports separately).
fn admission_word(who: &str, r: &Replay) -> String {
    let kind = r.outcome["kind"].as_str().unwrap_or("error");
    let message = r.outcome["message"].as_str().unwrap_or_default();
    if kind != "ok" && (message.contains("queue") || message.contains("busy")) {
        format!("shed:{who}")
    } else if kind == "ok" {
        format!("served:{who}")
    } else {
        format!("error:{who}")
    }
}

/// `projection.stream-split-line` (L I3): the daemon returns the answer's
/// typed frames as SSE bytes, one line cut across two chunks. Read back
/// through the client's own reader, which buffers to a newline; the delta
/// survives whole.
async fn split_line(daemon: &Daemon, target: &str, case: &Case) -> Result<CaseRecord, String> {
    let input = json!({"fault": "split-line", "prompt": case.input["prompt"], "preferred_speed": case.input["preferred_speed"], "prompt_shape": case.input["prompt_shape"], "max_tokens": case.input["max_tokens"], "temperature": case.input["temperature"]});
    let replay = daemon.replay(ReplayMethod::StreamFault, &input).await?;
    let mut facets = super::record::Facets::default();
    let outcome = if replay.answer["kind"] == "ok" {
        let chunks: Vec<String> = replay.answer["chunks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        let (text, deltas) = read_chunked_text(&chunks);
        facets.split_line = Some(super::record::SplitLine {
            text: text.clone(),
            deltas,
        });
        facets.output = Some(super::record::Output {
            text: text.clone(),
            ..Default::default()
        });
        Outcome::Ok
    } else {
        Outcome::Error {
            message: replay.answer.to_string(),
        }
    };
    Ok(CaseRecord {
        case_id: case.case_id.clone(),
        target: target.into(),
        rows: case.rows.clone(),
        request: case.input.clone(),
        outcome,
        facets,
    })
}

/// Read SSE bytes through the client's own reader (the one the untyped
/// stream uses), one chunk at a time: the text and how many deltas arrived.
fn read_chunked_text(chunks: &[String]) -> (String, usize) {
    let bytes: Vec<&[u8]> = chunks.iter().map(|c| c.as_bytes()).collect();
    let (deltas, _terminal) = oicp_client::read_sse_chunks(bytes, false);
    (deltas.concat(), deltas.len())
}

/// `projection.stream-error` (L I3): the answer's first tokens, then the
/// server's mid-stream `error` datum and no `[DONE]`. Through the client's
/// own reader, that is one `Finish{Error}` frame, as the engine's stream
/// ends.
async fn mid_stream_error(
    daemon: &Daemon,
    target: &str,
    case: &Case,
) -> Result<CaseRecord, String> {
    let mut input = case.input.clone();
    input["fault"] = json!("mid-stream");
    let replay = daemon.replay(ReplayMethod::StreamFault, &input).await?;
    let mut facets = super::record::Facets::default();
    let outcome = if replay.answer["kind"] == "ok" {
        let chunks: Vec<String> = replay.answer["chunks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        let finish = read_to_error(&chunks);
        // The frame list is what the reader ended on, not a claim about it:
        // `error` only when the reason is one, so a reader that swallowed the
        // datum shows `stop` and the row fails.
        let frames = if finish == "error" {
            vec!["error".into()]
        } else {
            vec!["finish".into()]
        };
        facets.output = Some(super::record::Output {
            finish: Some(finish),
            frames,
            ..Default::default()
        });
        Outcome::Ok
    } else {
        Outcome::Error {
            message: replay.answer.to_string(),
        }
    };
    Ok(CaseRecord {
        case_id: case.case_id.clone(),
        target: target.into(),
        rows: case.rows.clone(),
        request: case.input.clone(),
        outcome,
        facets,
    })
}

/// Read the corrupted stream through the client's own reader, and report the
/// finish reason it ends on (`error` when the datum failed it, else the
/// server's or the reader's own).
fn read_to_error(chunks: &[String]) -> String {
    let bytes: Vec<&[u8]> = chunks.iter().map(|c| c.as_bytes()).collect();
    let (_deltas, terminal) = oicp_client::read_sse_chunks(bytes, false);
    match terminal {
        sovereign_contracts::types::StreamFrame::Finish { reason, .. } => {
            reason.as_openai_str().to_string()
        }
        _ => "no-finish".into(),
    }
}

fn write(path: &std::path::Path, records: &[CaseRecord]) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let body: String = records
        .iter()
        .map(|r| serde_json::to_string(r).map(|s| s + "\n"))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    std::fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))
}
