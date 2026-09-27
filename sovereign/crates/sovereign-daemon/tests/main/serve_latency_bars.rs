// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two latency bars pre-registered for pb-svrn-dials-serve
//! (ralph/next/phase-b/STATE.md header, 2026-09-25), measured before the
//! switch: serving over loopback against serving in process, same host, same
//! models, n >= 5 each.
//!
//! - first-token latency p50: loopback at most 10% slower;
//! - embedding throughput at batch 32: loopback at least 90% of in-process.
//!
//! In process is the one serving assembly the daemon runs today
//! (`assemble_serving`); loopback is the daemon's terminal arm in its loopback
//! mode (`serve_client::loopback_provider`) dialing a `sovereign-serve` on a
//! free port, over the same config file. Ignored in the suite: it loads real
//! GGUFs. Run by hand, in the toolbox, with the serve binary built:
//!
//!   SOVEREIGN_MODELS_DIR=$PWD/sovereign/models \
//!   SOVEREIGN_SERVE_BIN=target/debug/sovereign-serve cargo test -p sovereign-daemon \
//!     --test main serve_latency_bars -- --ignored --nocapture

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use sovereign_contracts::{CompletionRequest, InferenceProvider, StreamFrame};

/// The smallest chat and embedding models in `sovereign/models/`, named
/// before the first run (the row's instruction): a small model is the strict
/// case, because the loopback cost is a larger share of its latency.
const CHAT_MODEL: &str = "Qwen3.5-0.8B-UD-Q6_K_XL.gguf";
const EMBED_MODEL: &str = "Qwen3-Embedding-0.6B-Q8_0.gguf";
const RUNS: usize = 7;
const BATCH: usize = 32;

/// The GGUF directory, from the existing override (`SOVEREIGN_MODELS_DIR`,
/// quality/env-flags.toml), never from the crate's own path: that escapes the
/// package root (boundary-gate, phase-b-25).
fn models_dir() -> std::path::PathBuf {
    std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(std::path::PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the GGUF directory, absolute (cargo runs tests from the crate dir)")
}

async fn first_token(p: &dyn InferenceProvider) -> Duration {
    let mut req = CompletionRequest::new("Name one river in Europe.");
    req.max_tokens = Some(8);
    req.enable_thinking = Some(false);
    let started = Instant::now();
    let mut stream = p.complete_stream_with_finish(&req).await.expect("stream");
    while let Some(frame) = stream.next().await {
        match frame {
            StreamFrame::Token(_) => return started.elapsed(),
            StreamFrame::Finish { .. } => panic!("finished before any token"),
            StreamFrame::Error(e) => panic!("stream error: {e}"),
        }
    }
    panic!("stream ended without a token")
}

async fn embed_throughput(p: &dyn InferenceProvider) -> f64 {
    let texts: Vec<String> = (0..BATCH)
        .map(|i| {
            format!(
                "Passage {i}: rivers carry sediment from highlands to the sea, shaping \
                 floodplains, deltas and the settlements that grow along their banks."
            )
        })
        .collect();
    let started = Instant::now();
    let out = p.embed_batch(&texts).await.expect("embed batch");
    assert_eq!(out.len(), BATCH);
    BATCH as f64 / started.elapsed().as_secs_f64()
}

/// One warm-up of each, discarded, then `RUNS` of each.
async fn measure(p: &dyn InferenceProvider) -> (Vec<Duration>, Vec<f64>) {
    first_token(p).await;
    embed_throughput(p).await;
    let mut ttft = Vec::new();
    let mut tput = Vec::new();
    for _ in 0..RUNS {
        ttft.push(first_token(p).await);
        tput.push(embed_throughput(p).await);
    }
    (ttft, tput)
}

fn p50<T: Copy + PartialOrd>(mut v: Vec<T>) -> T {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement: loads real GGUFs; run by hand before the switch (pb-svrn-dials-serve)"]
async fn serve_latency_bars() {
    // The loopback client's `serve_latency` laps (RUST_LOG=serve_latency=debug);
    // serve inherits RUST_LOG, and its laps are echoed from its log below.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stdout)
        .try_init();
    let chat = models_dir().join(CHAT_MODEL);
    let embed = models_dir().join(EMBED_MODEL);
    assert!(
        chat.exists() && embed.exists(),
        "models missing under {}",
        models_dir().display()
    );
    let root = tempfile::tempdir().expect("temp root");
    let dead_rails = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_rails_port = dead_rails.local_addr().unwrap().port();
    drop(dead_rails);
    let config_path = root.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[models]\nprimary = \"{}\"\nembed = \"{}\"\n\n[daemon]\nrails_base = \"http://127.0.0.1:{dead_rails_port}\"\n",
            chat.display(),
            embed.display()
        ),
    )
    .unwrap();
    let config = sovereign_contracts::setup_config::SetupConfig::load_from(&config_path).unwrap();

    // In process: the assembly the daemon runs today.
    let (in_ttft, in_tput) = {
        let config = config.clone();
        let parts = tokio::task::spawn_blocking(move || {
            sovereign_compute::assembly::assemble_serving(&config)
        })
        .await
        .unwrap()
        .expect("in-process assembly");
        let provider: Arc<dyn InferenceProvider> = parts.provider;
        measure(provider.as_ref()).await
    };

    // Loopback: serve on a free port over the same config file, dialed by the
    // terminal arm in its loopback mode.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let bin =
        std::env::var("SOVEREIGN_SERVE_BIN").expect("SOVEREIGN_SERVE_BIN names the serve binary");
    let mut serve = std::process::Command::new(bin)
        .args(["--data-dir", &root.path().display().to_string()])
        .args(["--listen", &format!("127.0.0.1:{port}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(root.path().join("serve.log")).unwrap())
        .spawn()
        .expect("spawn serve");
    let base = format!("http://127.0.0.1:{port}");
    let host = sovereign_turn_client::reach::ServingHost::at(base.as_str());
    assert!(
        host.wait_until_serving(Duration::from_secs(300)).await,
        "serve did not come up"
    );
    let serve_base = sovereign_daemon::serve_client::ServeBase {
        base: base.clone(),
        source: sovereign_daemon::serve_client::ServeBaseSource::Default,
    };
    let served = sovereign_daemon::serve_client::read_served_self(&base)
        .await
        .expect("self-report");
    let loopback = sovereign_daemon::serve_client::loopback_provider(
        &serve_base,
        served,
        config.effective_context_size(),
    );
    let (lo_ttft, lo_tput) = measure(&loopback).await;
    let _ = serve.kill();
    let serve_log = std::fs::read_to_string(root.path().join("serve.log"))
        .unwrap_or_else(|e| format!("serve_latency: no serve.log to echo laps from ({e})"));
    for line in serve_log
        .lines()
        .filter(|l| l.contains(sovereign_contracts::engine_state::LATENCY_TARGET))
    {
        println!("serve: {line}");
    }

    let (in_p50, lo_p50) = (p50(in_ttft.clone()), p50(lo_ttft.clone()));
    let (in_tp, lo_tp) = (p50(in_tput.clone()), p50(lo_tput.clone()));
    let ttft_ratio = lo_p50.as_secs_f64() / in_p50.as_secs_f64();
    let tput_ratio = lo_tp / in_tp;
    println!("BARS chat={CHAT_MODEL} embed={EMBED_MODEL} runs={RUNS} batch={BATCH}");
    println!(
        "  in-process first-token ms: {:?}",
        in_ttft.iter().map(|d| d.as_millis()).collect::<Vec<_>>()
    );
    println!(
        "  loopback   first-token ms: {:?}",
        lo_ttft.iter().map(|d| d.as_millis()).collect::<Vec<_>>()
    );
    println!(
        "  in-process embed/s: {:?}",
        in_tput.iter().map(|t| t.round()).collect::<Vec<_>>()
    );
    println!(
        "  loopback   embed/s: {:?}",
        lo_tput.iter().map(|t| t.round()).collect::<Vec<_>>()
    );
    println!(
        "  first-token p50 {:.1} ms -> {:.1} ms (x{ttft_ratio:.3}, bar <= 1.10); embed p50 {in_tp:.1}/s -> {lo_tp:.1}/s (x{tput_ratio:.3}, bar >= 0.90)",
        in_p50.as_secs_f64() * 1e3,
        lo_p50.as_secs_f64() * 1e3,
    );
    assert!(
        ttft_ratio <= 1.10,
        "first-token bar missed: x{ttft_ratio:.3}"
    );
    assert!(tput_ratio >= 0.90, "embedding bar missed: x{tput_ratio:.3}");
}
