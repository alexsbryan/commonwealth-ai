// SPDX-License-Identifier: AGPL-3.0-or-later
//! Baseline-model throughput probe.
//!
//! Runs once at daemon startup (and once more whenever the
//! [`HardwareProfile`] fingerprint changes — see
//! `sovereign-mesh::daemon::EmbeddedDaemon::start_daemon`) to measure
//! how fast the bundled default model runs on this hardware.
//! Persisted to disk so subsequent boots don't re-pay the cost.
//!
//! The result rides the gossiped `NodeCapabilities.benchmark` field
//! and feeds the scoring pipeline at
//! [`sovereign_core::oicp::throughput_factor`] — the simplest
//! defensible "how fast is this peer" signal that doesn't require a
//! cross-model benchmark matrix.
//!
//! Why a probe and not an estimate from `tflops`: TFLOPS estimates
//! ignore quantization, KV-cache bandwidth, and thermal headroom —
//! all of which dominate llama.cpp throughput. A 10-second wall-clock
//! probe is more honest than any static heuristic.

use std::time::{Duration, Instant};

use sovereign_core::error::Result;
use sovereign_core::oicp::BenchmarkResult;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

/// The benchmark prompt is a fixed corpus-text excerpt: long enough
/// to exercise the prompt-processing path past the typical
/// short-prompt fast path, short enough that the probe completes in
/// a few seconds on weak hardware. Stable across daemon versions so
/// re-probes after a hardware change are commensurable.
const BENCHMARK_PROMPT: &str = include_str!("../assets/benchmark_prompt.txt");

/// Maximum tokens the probe asks for. We don't actually want a long
/// answer — we want enough generation to estimate `tg_tok_s`.
const BENCHMARK_MAX_TOKENS: u32 = 64;

/// Hard-stop the probe if the model takes this long. A model
/// genuinely incapable of completing in this window is so slow it
/// shouldn't be advertised at all; the daemon logs a warning and
/// proceeds with `benchmark = None`.
const BENCHMARK_TIMEOUT: Duration = Duration::from_secs(60);

/// Run a baseline benchmark on `provider`'s `Fast` slot.
///
/// The probe issues a single fixed-prompt completion through the
/// provider's `complete_stream` API and times two phases:
///
/// 1. **Prompt processing**: from dispatch to first token. The
///    prompt has a known length; `pp_tok_s = prompt_tokens / ttft`.
/// 2. **Token generation**: from first token to stream end.
///    `tg_tok_s = tokens_generated / generation_time`.
///
/// Returns `Err` only if the inference provider itself errors out;
/// timeout or zero-token responses fall through with reasonable
/// neutral values so the daemon can still publish *some* benchmark
/// rather than none.
pub async fn run_baseline_benchmark(
    provider: &dyn InferenceProvider,
    baseline_model_id: String,
    baseline_size_gb: f32,
) -> Result<BenchmarkResult> {
    use futures::StreamExt;

    tracing::info!(
        model = %baseline_model_id,
        size_gb = baseline_size_gb,
        "bench: starting baseline probe"
    );

    let mut request = CompletionRequest::new(BENCHMARK_PROMPT).with_speed(Speed::Fast);
    request.max_tokens = Some(BENCHMARK_MAX_TOKENS as usize);
    request.temperature = Some(0.0);

    let start = Instant::now();
    let mut stream = provider.complete_stream(&request).await?;

    let mut first_chunk_at: Option<Instant> = None;
    let mut chunk_count: u64 = 0;

    let probe = async {
        while let Some(item) = stream.next().await {
            match item {
                Ok(_) => {
                    if first_chunk_at.is_none() {
                        first_chunk_at = Some(Instant::now());
                    }
                    chunk_count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "bench: stream error during baseline probe"
                    );
                    break;
                }
            }
        }
    };

    if tokio::time::timeout(BENCHMARK_TIMEOUT, probe)
        .await
        .is_err()
    {
        tracing::warn!(
            timeout_secs = BENCHMARK_TIMEOUT.as_secs(),
            chunks_so_far = chunk_count,
            "bench: timed out — recording partial measurement"
        );
    }

    let measured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Approximate prompt-token count by character / 4 (English
    // heuristic). Same approximation as the streaming observer in
    // peer_inference — consistency matters more than precision when
    // the result is a routing multiplier.
    let prompt_tokens_est = (BENCHMARK_PROMPT.len() / 4).max(1) as f32;

    let (pp_tok_s, tg_tok_s) = match first_chunk_at {
        None => {
            tracing::warn!("bench: no tokens generated — recording neutral throughput");
            (0.0_f32, 0.0_f32)
        }
        Some(first) => {
            let ttft_secs = first.duration_since(start).as_secs_f32();
            let gen_secs = first.elapsed().as_secs_f32();
            let pp = if ttft_secs > 0.0 {
                prompt_tokens_est / ttft_secs
            } else {
                0.0
            };
            let tg = if gen_secs > 0.0 && chunk_count > 0 {
                chunk_count as f32 / gen_secs
            } else {
                0.0
            };
            (pp, tg)
        }
    };

    let result = BenchmarkResult {
        baseline_model_id,
        baseline_size_gb,
        pp_tok_s,
        tg_tok_s,
        measured_at,
    };

    tracing::info!(
        model = %result.baseline_model_id,
        pp_tok_s = result.pp_tok_s,
        tg_tok_s = result.tg_tok_s,
        size_gb = result.baseline_size_gb,
        duration_ms = start.elapsed().as_millis() as u64,
        "bench: completed"
    );

    Ok(result)
}

/// Stable hash of the operator's hardware. Re-running the benchmark
/// when this changes catches the "moved my GPU" case without
/// re-running on every daemon boot. The hash space is intentionally
/// small (u64) — we don't need cryptographic uniqueness, just
/// equality vs. the previous boot's persisted value.
pub fn hardware_fingerprint(
    cpu_cores: u32,
    system_ram_gb: u32,
    gpu_names: &[String],
    gpu_vram_gb_total: u32,
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    cpu_cores.hash(&mut h);
    system_ram_gb.hash(&mut h);
    gpu_vram_gb_total.hash(&mut h);
    for name in gpu_names {
        name.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_fingerprint_is_stable_for_same_inputs() {
        let names = vec!["RTX 4090".to_string()];
        let a = hardware_fingerprint(16, 64, &names, 24);
        let b = hardware_fingerprint(16, 64, &names, 24);
        assert_eq!(a, b);
    }

    #[test]
    fn hardware_fingerprint_changes_when_gpu_changes() {
        let a = hardware_fingerprint(16, 64, &["RTX 4090".into()], 24);
        let b = hardware_fingerprint(16, 64, &["RTX 5090".into()], 32);
        assert_ne!(a, b);
    }

    #[test]
    fn hardware_fingerprint_changes_when_ram_changes() {
        let names = vec!["RTX 4090".to_string()];
        let a = hardware_fingerprint(16, 64, &names, 24);
        let b = hardware_fingerprint(16, 96, &names, 24);
        assert_ne!(a, b);
    }
}
