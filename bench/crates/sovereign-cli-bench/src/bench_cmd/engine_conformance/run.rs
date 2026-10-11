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
use sovereign_contracts::engine_observe::ReplayMethod;

use super::case::{Case, CaseMethod};
use super::project::{self, Replay};
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
        return Err(format!("fault `{fault}` is not injected by the driver yet"));
    }
    let method = match case.method {
        CaseMethod::Replay(m) => m,
        CaseMethod::Throughput => return throughput(daemon, target, case).await,
        CaseMethod::Probe => return Err("the logprob probe is not built yet".into()),
    };
    let replay = daemon.replay(method, &case.input).await?;
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
    if method == ReplayMethod::Host {
        if let Some(s) = server {
            match s.host_observed(&rec.facets.host_reported).await {
                Ok(truth) => rec.facets.host_observed = truth,
                Err(why) => {
                    rec.facets.unobserved.insert("host_observed".into(), why);
                }
            }
        }
    }
    if method.is_completion() && rec.outcome == Outcome::Ok {
        let reasoning = project::generated_reasoning(rec.facets.output.as_ref(), wire.as_ref());
        match reasoning_tokens(daemon, server, reasoning.as_deref()).await {
            Ok(n) => rec.facets.reasoning_tokens = n,
            Err(why) => {
                rec.facets.unobserved.insert("reasoning_tokens".into(), why);
            }
        }
    }
    Ok(rec)
}

/// The think block's length in tokens, by the target's own tokenizer: the
/// server's when there is one, else the engine's `count_tokens`.
async fn reasoning_tokens(
    daemon: &Daemon,
    server: Option<&LlamaServer>,
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
            .tokenize(text, false, None)
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
