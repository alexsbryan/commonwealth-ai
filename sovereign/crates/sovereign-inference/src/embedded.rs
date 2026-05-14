use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::openai::OpenAIChatTemplateParams;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;

use sovereign_core::error::Error;
use sovereign_core::model_family::{EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

// ─── ModelSlot ─────────────────────────────────────────────────

struct SlotContext {
    ctx: llama_cpp_2::context::LlamaContext<'static>,
    _model: Arc<LlamaModel>,
}

unsafe impl Send for SlotContext {}
unsafe impl Sync for SlotContext {}

struct ModelSlot {
    model: Arc<LlamaModel>,
    context: Mutex<SlotContext>,
    model_id: String,
    /// In-memory footprint of the loaded model, in bytes — captured
    /// at load time. Sourced from `LlamaModel::size()` (the
    /// backend's own accounting of the buffers it allocated) with a
    /// gguf file-size fallback when `size()` reports zero. Used by
    /// the LRU eviction policy to decide which slots to drop when a
    /// new load would exceed the operator's `max_extras_memory_gb`
    /// budget.
    size_bytes: u64,
    /// Last time this slot was used to serve a request, expressed
    /// as milliseconds since UNIX epoch. Updated by `complete()` /
    /// `complete_stream()` after dispatch. The eviction policy
    /// orders slots by ascending `last_used` and drops the coldest
    /// first. `AtomicU64` so updates don't need a write lock on
    /// `ExtrasState`.
    last_used: std::sync::atomic::AtomicU64,
    /// Per-slot inflight gate — single permit. The slot's
    /// `Mutex<SlotContext>` already serialises generation, but the
    /// gate is acquired BEFORE `spawn_blocking` so concurrent
    /// callers don't each park a tokio blocking-pool thread (and
    /// each clone an 8 KB prompt into the closure) waiting their
    /// turn. With the gate, only one caller proceeds into the
    /// blocking task; the rest queue cheaply at the async layer.
    ///
    /// Observed motivator: KnowledgeView Phase 2 entity extraction
    /// fans out 4+ parallel `complete()` calls per chunk batch. All
    /// 4 used to land in `blocking_lock` on the slot mutex with
    /// 4 blocking threads held, while the user's first chat request
    /// queued behind them. With the permit, the queue forms in the
    /// async runtime, threads stay free.
    inflight: Arc<tokio::sync::Semaphore>,
}

/// Current millis-since-epoch as `u64`. Saturates at 0 if the
/// system clock is set before 1970 (impossible in practice;
/// guarded so we never panic).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Hard wall-clock deadline for any single chat inference, in
/// seconds. Defends against pathological JSON-Schema mask states
/// where the in-house constraint enforcer's per-token mask
/// computation degrades from ~25 ms/token to 100s of ms/token —
/// the slot's `Mutex<SlotContext>` is held for the entire
/// generate_sync / generate_stream_sync call, so a stuck inference
/// blocks every other request to that slot until the deadline
/// fires and `ctx.clear_kv_cache()` runs.
///
/// Tunable via `SOVEREIGN_INFERENCE_TIMEOUT_SECS`. Default 300s —
/// generous enough that any legitimate Slow-slot Phase-1 call
/// (~60-160s observed worst-case under heavy grammar) finishes
/// with margin, short enough that a runaway mask-state is killed
/// before it pins the daemon for ~30 min and starves the mesh.
fn inference_deadline_secs() -> u64 {
    use std::sync::OnceLock;
    static DEADLINE: OnceLock<u64> = OnceLock::new();
    *DEADLINE.get_or_init(|| {
        std::env::var("SOVEREIGN_INFERENCE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(300)
    })
}

impl ModelSlot {
    fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        context_size: u32,
        n_gpu_layers: u32,
    ) -> Result<Self> {
        // Operator escape hatch: `SOVEREIGN_FORCE_CPU_CHAT=1` forces
        // the chat slot fully onto CPU. The known-safe CPU config (per
        // the embed-slot invariant note) is the combination of:
        //   1. `n_gpu_layers = 0` on the model — weights stay in RAM.
        //   2. `with_offload_kqv(false).with_op_offload(false)` on the
        //      context — KV cache + ops also run on CPU.
        // Earlier versions of this gate set only (2), which left the
        // model weights on the GPU and still required Metal compute
        // pipelines for matmul. That hybrid path is what causes
        // `ggml_metal_encoder_set_pipeline` null-derefs to bypass the
        // gate entirely. Both must be set.
        //
        // Use this when chat decodes are crashing in ggml's Metal /
        // Vulkan backend on a particular quant/architecture combo and
        // you want to confirm whether the fault is in the GPU path or
        // upstream of it. Restart the desktop / daemon after setting.
        let force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let effective_gpu_layers = if force_cpu { 0 } else { n_gpu_layers };

        let model_params =
            LlamaModelParams::default().with_n_gpu_layers(effective_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load model: {e}")))?;

        let model_id = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let model = Arc::new(model);

        let n_threads = llama_threads_for_host();

        // GPU offload for chat slots. `with_n_gpu_layers(999)` alone
        // puts the model weights on the GPU but leaves the KV cache in
        // CPU memory and may route some compute ops back to CPU.
        // Setting `offload_kqv(true)` + `op_offload(true)` pins the
        // whole forward pass on the GPU.
        //
        // Measured impact (M2 Max, Qwen3.5-9B.Q8_0): decode tok/sec
        // ~49 without these flags, ~70-80 with them — the last 5s
        // of the Joan Robinson fix. Strix Halo Vulkan shows a similar
        // gap (embed ingest was 3.7 chunks/s CPU-only with weights on
        // GPU but KQV in RAM; see docs/TOOLBOX_SETUP.md §9).
        //
        // Try-GPU-then-CPU-fallback: a future llama.cpp upgrade could
        // reintroduce a crash on some quant/backend combo, and silent
        // fallback to CPU is what made the original embed regression
        // invisible. Always log the actual backend so operators can
        // spot a silent fallback.
        let wants_gpu = !force_cpu
            && cfg!(any(target_os = "macos", target_os = "linux"))
            && n_gpu_layers > 0;
        if force_cpu {
            tracing::warn!(
                model_id = %model_id,
                "SOVEREIGN_FORCE_CPU_CHAT set — chat slot loaded with n_gpu_layers=0; \
                 model weights and ops stay on CPU"
            );
        }
        let build_ctx_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                // Chat slots are serial: a Mutex<SlotContext> guards the
                // single context and generate_sync builds LlamaBatch with
                // n_seq=1. The llama-cpp-2 default n_seq_max=16 would
                // split n_ctx into 16 slots (e.g. 16384 → 1024 per seq)
                // and waste 15/16ths of the configured window. Pin to 1
                // so the full context_size is the usable per-request
                // window. (EmbedSlot keeps its own n_seq_max=16 — it
                // genuinely batches.)
                .with_n_seq_max(1)
                // n_batch = context_size so the full context window is available
                // for prompt processing. llama.cpp automatically splits the batch
                // into n_ubatch=512 micro-batches for GPU/CPU kernel calls, so
                // memory pressure is unchanged — only the Rust-side assertion limit
                // is relaxed.
                .with_n_batch(context_size)
                .with_n_ubatch(512)
                // llama-cpp-2 defaults both thread counts to 4, which
                // caps matmul at four cores regardless of machine
                // size. Embedding prefill and chat prompt processing
                // are both dominated by `n_threads_batch`; `n_threads`
                // governs single-token decode. Set both to the
                // platform's parallelism so the CPU backend actually
                // uses all the cores (BLAS on macOS, plain ggml
                // elsewhere). The default-of-4 behaviour was the
                // reason a 600M embed model ran at 2 seq/s while only
                // 400% of the CPU was busy.
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
                .with_op_offload(gpu)
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_ctx_params(true))
                    .map(|c| (c, true))
            }
        } else {
            Err(llama_cpp_2::LlamaContextLoadError::NullReturn)
        } {
            Ok(pair) => pair,
            Err(e) => {
                if wants_gpu {
                    tracing::warn!(
                        error = ?e,
                        model_id = %model_id,
                        "GPU chat-slot context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_ctx_params(false))
                        .map_err(|e| {
                            Error::Inference(format!("Failed to create context: {e}"))
                        })?
                };
                (ctx, false)
            }
        };

        let compute_backend = if used_gpu {
            gpu_backend_label()
        } else {
            embed_compute_backend_label()
        };

        tracing::info!(
            model_id = %model_id,
            params = model.n_params(),
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            n_threads,
            compute_backend,
            "ModelSlot::load — slot ready"
        );

        // Slot footprint for the LRU eviction policy. Prefer
        // `LlamaModel::size()` — that's the in-memory size the
        // backend actually allocated (covers GPU offload + any
        // backend-specific padding) — and fall back to the gguf
        // file size when the backend reports zero. For most quants
        // these track within ~1%, but on Vulkan with split offload
        // the loaded size can differ enough to swing budget
        // accounting on tight hardware.
        let loaded_size = model.size();
        let file_size = std::fs::metadata(model_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let size_bytes = if loaded_size > 0 {
            loaded_size
        } else {
            file_size
        };

        Ok(Self {
            model: model.clone(),
            context: Mutex::new(SlotContext {
                ctx,
                _model: model,
            }),
            model_id,
            size_bytes,
            last_used: std::sync::atomic::AtomicU64::new(now_millis()),
            inflight: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    /// Build a sibling slot that shares another slot's loaded model
    /// but stands up its own `LlamaContext` with different sequence
    /// parameters. Used for the FastShort companion to the Fast slot
    /// — same weights, but `n_seq_max=8 / n_ctx=16384 / n_ubatch=2048`
    /// so continuous-batched short calls (Phase 1b coverage, Phase 3
    /// cluster naming, Phase 5 positions, Phase 6 tensions) get the
    /// 2.1–2.8× wall-clock speedup measured in
    /// `bench_decode_batch.rs`. The original Fast slot keeps
    /// `n_seq_max=1` so long-context Phase 1 chapter ingestion still
    /// has the full window per call.
    ///
    /// Each FastShort sequence's KV cache slice is `n_ctx /
    /// n_seq_max` tokens (i.e. 2048 for the default config). 97.4%
    /// of wiki-tier2-500 sections fit inside that budget after
    /// Phase 1b prompt overhead; the overflow tail falls through to
    /// the FastLong claim via OICP-v0.3 §2.4 hard gates.
    fn from_existing_model(
        backend: &Arc<LlamaBackend>,
        model: Arc<LlamaModel>,
        model_id: String,
        size_bytes: u64,
        context_size: u32,
        n_seq_max: u32,
        n_ubatch: u32,
    ) -> Result<Self> {
        let force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let wants_gpu = !force_cpu && cfg!(any(target_os = "macos", target_os = "linux"));

        let n_threads = llama_threads_for_host();

        let build_ctx_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                .with_n_seq_max(n_seq_max)
                .with_n_batch(context_size)
                .with_n_ubatch(n_ubatch)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
                .with_op_offload(gpu)
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_ctx_params(true))
                    .map(|c| (c, true))
            }
        } else {
            Err(llama_cpp_2::LlamaContextLoadError::NullReturn)
        } {
            Ok(pair) => pair,
            Err(e) => {
                if wants_gpu {
                    tracing::warn!(
                        error = ?e,
                        model_id = %model_id,
                        "FastShort GPU context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_ctx_params(false))
                        .map_err(|e| {
                            Error::Inference(format!(
                                "FastShort context creation failed: {e}"
                            ))
                        })?
                };
                (ctx, false)
            }
        };

        tracing::info!(
            model_id = %model_id,
            n_seq_max,
            n_ctx = context_size,
            n_ubatch,
            used_gpu,
            "FastShort slot ready — continuous-batched short-call companion to Fast"
        );

        Ok(Self {
            model: Arc::clone(&model),
            context: Mutex::new(SlotContext {
                ctx,
                _model: model,
            }),
            model_id,
            size_bytes,
            last_used: std::sync::atomic::AtomicU64::new(now_millis()),
            inflight: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    fn generate_sync(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        request: &CompletionRequest,
        quirks: &ModelQuirks,
    ) -> Result<(String, usize, usize)> {
        // Clear KV cache before each new inference sequence.
        // `clear_kv_cache()` is also called at the end of a successful run, but
        // if a prior call failed mid-decode the cache is left dirty. Pre-clearing
        // guarantees a clean slate regardless of the prior exit path, preventing
        // M-RoPE position mismatch errors ("X = 830, Y = 0: X < Y required").
        ctx.clear_kv_cache();

        let full_prompt = format_prompt(model, model_id, request, quirks)?;

        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(
            request.max_tokens,
            tokens.len(),
            n_ctx,
        )?;

        let mut batch = LlamaBatch::new(n_batch, 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;

        let mut sampler = build_sampler(model, request, quirks);
        let mut output = String::new();
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        // Wall-clock deadline. See `inference_deadline_secs` for
        // rationale. We check at the top of each sample-loop iteration
        // so a stuck mask-sample step still bounds at ~one slow token
        // (typically 100s of ms in the pathological state) past the
        // deadline before we clean up and return.
        let deadline_secs = inference_deadline_secs();
        let deadline = Instant::now()
            + std::time::Duration::from_secs(deadline_secs);
        let started_at = Instant::now();

        // Think-block budget forcing: track position inside <think>…</think>
        // and inject a closing tag once the budget is exhausted so the model
        // is forced to summarise rather than spiral indefinitely.
        let mut tail = String::with_capacity(32);
        let mut in_think = false;
        let mut think_tokens = 0usize;
        let mut think_budget_fired = false;
        // Stray-close tracking: thinking-capable models occasionally
        // emit `</think>` without a preceding `<think>` open (chat
        // template rendered with `enable_thinking: false` but the
        // model produced CoT anyway). When we observe the close
        // without the open, the leading text was de-facto thinking;
        // we wrap retroactively after the loop so the desktop's
        // think-block parser folds it correctly. See plan §1.3.
        let mut saw_open_tag = false;
        let mut saw_close_tag = false;

        // Tool-emission stop conditions for tools-using turns.
        //
        // Two emission shapes exist:
        //   A. Marker-wrapped — chat-template default, model emits
        //      `<tool_call>{...}</tool_call>` then continues.
        //   B. Grammar-locked bare JSON — when `tool_choice=required`
        //      installs the envelope schema, the model emits one
        //      balanced JSON object with NO `<tool_call>` markers.
        //
        // Both must terminate generation; otherwise the sampler runs
        // to `max_tokens` or hits the pathological-mask deadline.
        // The 2026-05-13 codex smoke saw shape B with ~10K tokens
        // generated per turn for ~80-byte envelopes — every token
        // past the close `}` was wasted on a grammar mask cycling
        // through whitespace / preferred-but-masked tokens.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let mut tc_json_depth = 0i32;
        let mut tc_json_in_string = false;
        let mut tc_json_escape_next = false;
        let mut tc_json_ever_opened = false;
        // Optional per-token sampler-role trace, gated on
        // SOVEREIGN_TRACE_SAMPLER_ROLES. Used during gym
        // diagnostics (gym 003 path-typo, 2026-05-13) to confirm
        // the Content/greedy sampler engages inside JSON-string
        // fields on /v1/chat/completions ingress. Counters are
        // summarised at end-of-generation. Cheap when off.
        let mut role_explore_n = 0usize;
        let mut role_content_n = 0usize;
        let mut tokens_in_string_n = 0usize;
        let role_trace_on = std::env::var("SOVEREIGN_TRACE_SAMPLER_ROLES").is_ok();

        while n_generated < max_tokens {
            if Instant::now() > deadline {
                let elapsed = started_at.elapsed().as_secs();
                tracing::warn!(
                    model = %model_id,
                    elapsed_s = elapsed,
                    deadline_s = deadline_secs,
                    n_generated,
                    schema = request.structured_output.is_some(),
                    "inference:deadline exceeded — clearing KV cache and returning"
                );
                ctx.clear_kv_cache();
                return Err(Error::Inference(format!(
                    "inference deadline exceeded after {elapsed}s ({n_generated} tokens generated; \
                     deadline={deadline_secs}s) — likely pathological JSON-Schema mask state. \
                     Set SOVEREIGN_INFERENCE_TIMEOUT_SECS to override."
                )));
            }

            let role = if tools_present && tc_json_in_string {
                SamplerRole::Content
            } else {
                SamplerRole::Explore
            };
            match role {
                SamplerRole::Content => role_content_n += 1,
                SamplerRole::Explore => role_explore_n += 1,
            }
            if tc_json_in_string {
                tokens_in_string_n += 1;
            }
            let token = sampler.sample(ctx, -1, role);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                if role_trace_on && tools_present {
                    tracing::info!(
                        role = ?role,
                        in_string = tc_json_in_string,
                        depth = tc_json_depth,
                        piece = %piece.replace('\n', "\\n"),
                        "sampler_trace token"
                    );
                }
                // Update sliding tail for tag detection.
                tail.push_str(&piece);
                if tail.len() > 32 {
                    // Walk back from len-32 to the nearest char boundary so we
                    // never slice through a multi-byte UTF-8 sequence (Qwen3).
                    let mut drain_to = tail.len() - 32;
                    while drain_to > 0 && !tail.is_char_boundary(drain_to) {
                        drain_to -= 1;
                    }
                    tail.drain(..drain_to);
                }
                if !in_think && tail.contains("<think>") {
                    in_think = true;
                    saw_open_tag = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                    saw_close_tag = true;
                } else if !in_think && tail.contains("</think>") {
                    // Stray close — record so we can wrap post-loop.
                    saw_close_tag = true;
                }
                if in_think {
                    think_tokens += 1;
                }

                output.push_str(&piece);

                // Tool-emission stop. Two cases, both unconditional
                // once detected (no `!in_think` guard — see fn-scope
                // comment): marker-wrapped close-tag, or grammar-
                // locked balanced JSON envelope. Track brace depth
                // incrementally on the piece bytes, ignoring braces
                // inside JSON strings.
                if tools_present {
                    if tail.contains("</tool_call>") {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            tail = %tail,
                            "inference: stopping on </tool_call> marker"
                        );
                        n_generated += 1;
                        break;
                    }
                    if !in_think {
                        for b in piece.bytes() {
                            if tc_json_escape_next {
                                tc_json_escape_next = false;
                                continue;
                            }
                            if tc_json_in_string {
                                match b {
                                    b'\\' => tc_json_escape_next = true,
                                    b'"' => tc_json_in_string = false,
                                    _ => {}
                                }
                                continue;
                            }
                            match b {
                                b'"' => tc_json_in_string = true,
                                b'{' => {
                                    tc_json_depth += 1;
                                    tc_json_ever_opened = true;
                                }
                                b'}' => tc_json_depth -= 1,
                                _ => {}
                            }
                        }
                        if tc_json_ever_opened && tc_json_depth == 0 {
                            tracing::info!(
                                model = %model_id,
                                n_generated,
                                "inference: stopping on balanced JSON envelope"
                            );
                            n_generated += 1;
                            break;
                        }
                    }
                }
            }

            n_generated += 1;

            batch.clear();
            let pos = (tokens.len() + n_generated - 1) as i32;
            batch
                .add(token, pos, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;

            // Budget forcing: inject </think> if the think block runs too long.
            if in_think && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET) && !think_budget_fired {
                think_budget_fired = true;
                let force = "\n</think>\n\n";
                if let Ok(close_tokens) = model.str_to_token(force, AddBos::Never) {
                    for &ct in &close_tokens {
                        if let Ok(fp) = model.token_to_piece(ct, &mut decoder, true, None) {
                            output.push_str(&fp);
                        }
                        sampler.accept(ct);
                        batch.clear();
                        batch
                            .add(ct, (tokens.len() + n_generated) as i32, &[0], true)
                            .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
                        ctx.decode(&mut batch)
                            .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;
                        n_generated += 1;
                    }
                }
                in_think = false;
            }
        }

        ctx.clear_kv_cache();

        // Sampler-role summary: emitted once per generation so the
        // operator can spot when Content (greedy) didn't engage.
        // Healthy shape on a tool-call turn: a sizeable fraction of
        // tokens carry role=Content (the cmd-string bytes). When
        // role_content_n is 0 despite tools_present, the JSON-string
        // boundary tracker never triggered — diagnostic for cases
        // where the model emits an envelope-content shape without
        // `<tool_call>` markers but somehow also without entering
        // a JSON string from the tracker's perspective.
        if tools_present {
            tracing::info!(
                model = %model_id,
                role_content_n,
                role_explore_n,
                tokens_in_string_n,
                tc_json_ever_opened,
                "sampler_trace role summary"
            );
        }

        if saw_close_tag && !saw_open_tag {
            tracing::warn!(
                model = model_id,
                "thinking-tag stray close: model emitted </think> without a \
                 preceding <think> — wrapping output as thinking content"
            );
            output = format!("<think>{output}");
        }

        Ok((output, tokens.len(), n_generated))
    }

    /// Multi-sequence batched autoregressive decode for short-call
    /// enrichment phases routed to the FastShort slot. Caller supplies
    /// up to the slot's `n_seq_max` requests; we run one packed
    /// prefill + N autoregressive decode steps (each step decodes one
    /// new token per still-active seq), retiring seqs on EOS or per-
    /// request `max_tokens`. Returns one `(text, prompt_tok,
    /// decoded_tok)` tuple per request, in the same order as input.
    ///
    /// Methodology mirrors `bench_decode_batch.rs::run_k_calls` —
    /// proven to deliver 2.1–2.8× wall-clock speedup vs sequential
    /// `generate_sync` calls on the same shape (Strix Halo / ROCm /
    /// Qwen3.5-2B Q6_K, 900 in / 128 out).
    ///
    /// **Scope (intentionally narrower than `generate_sync`):**
    /// - No thinking-block tracking. Phase 1b/3/5/6 composers don't
    ///   enable thinking (chat closure passes `think_budget=0`); the
    ///   thinking-detection state machine in `generate_sync` would
    ///   cost per-token UTF-8 work × N seqs without ever firing.
    /// - No in-loop deadline enforcement. Outer Tokio cancellation is
    ///   sufficient for enrichment, and the loop's tail is bounded by
    ///   the smallest per-request `max_output_tokens` (typically 512).
    /// - Per-seq `ConstrainedSampler` (so per-seq JSON-schema masks
    ///   are supported just like single-seq decode).
    fn generate_sync_batched(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        requests: &[&CompletionRequest],
        quirks: &ModelQuirks,
    ) -> Result<Vec<(String, usize, usize)>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        ctx.clear_kv_cache();

        // Tokenize every request up-front. `format_prompt` runs the
        // production chat-template renderer (system + user + thinking
        // injection per `quirks`).
        let tokenized: Vec<Vec<LlamaToken>> = requests
            .iter()
            .map(|r| {
                let prompt = format_prompt(model, model_id, r, quirks)?;
                model
                    .str_to_token(&prompt, AddBos::Always)
                    .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))
            })
            .collect::<Result<Vec<_>>>()?;

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;
        // Per-seq KV slice when the context has n_seq_max > 1.
        let n_seq_max = std::cmp::max(1, requests.len());
        let n_ctx_per_seq = n_ctx / n_seq_max;

        // Per-seq output cap. Each request's prompt + cap must fit
        // its own KV slice — refuse cleanly rather than overflow mid-
        // decode.
        let max_outputs: Vec<usize> = requests
            .iter()
            .zip(tokenized.iter())
            .map(|(r, toks)| {
                clamp_max_tokens(r.max_tokens, toks.len(), n_ctx_per_seq)
            })
            .collect::<Result<_>>()?;

        // ── Prefill: pack every prompt into one batch ───────────────
        let mut batch = LlamaBatch::new(n_batch, requests.len() as i32);
        let mut current_logit_idx: Vec<i32> = Vec::with_capacity(requests.len());
        let mut batch_pos: i32 = 0;
        for (seq, tokens) in tokenized.iter().enumerate() {
            let last = tokens.len() - 1;
            for (pos, &tok) in tokens.iter().enumerate() {
                let need_logits = pos == last;
                batch
                    .add(tok, pos as i32, &[seq as i32], need_logits)
                    .map_err(|e| {
                        Error::Inference(format!("Batched prefill add failed: {e}"))
                    })?;
                if need_logits {
                    current_logit_idx.push(batch_pos);
                }
                batch_pos += 1;
            }
        }
        ctx.decode(&mut batch).map_err(|e| {
            Error::Inference(format!("Batched prefill decode failed: {e}"))
        })?;

        // ── Per-seq decode state ────────────────────────────────────
        let mut samplers: Vec<ConstrainedSampler> = requests
            .iter()
            .map(|r| build_sampler(model, r, quirks))
            .collect();
        let mut decoders: Vec<encoding_rs::Decoder> = (0..requests.len())
            .map(|_| encoding_rs::UTF_8.new_decoder())
            .collect();
        let mut outputs: Vec<String> = vec![String::new(); requests.len()];
        let mut n_generated: Vec<usize> = vec![0; requests.len()];
        let mut next_pos: Vec<i32> = tokenized
            .iter()
            .map(|t| t.len() as i32)
            .collect();
        let mut active = vec![true; requests.len()];

        // ── Autoregressive decode loop ──────────────────────────────
        loop {
            // Sample one token per still-active seq. Track the next
            // tokens we need to feed in the following decode step;
            // a seq that hits EOS or the per-request cap retires
            // and contributes no further tokens to subsequent batches.
            let mut next_tokens: Vec<Option<LlamaToken>> = vec![None; requests.len()];
            for seq in 0..requests.len() {
                if !active[seq] {
                    continue;
                }
                // Batched path doesn't track per-sequence JSON state;
                // default to Explore (matches pre-Investment-#16 behavior).
                let token = samplers[seq].sample(ctx, current_logit_idx[seq], SamplerRole::Explore);
                samplers[seq].accept(token);

                if model.is_eog_token(token) {
                    active[seq] = false;
                    continue;
                }

                if let Ok(piece) =
                    model.token_to_piece(token, &mut decoders[seq], true, None)
                {
                    outputs[seq].push_str(&piece);
                }
                n_generated[seq] += 1;

                if n_generated[seq] >= max_outputs[seq] {
                    active[seq] = false;
                    continue;
                }

                next_tokens[seq] = Some(token);
            }

            if !active.iter().any(|&a| a) {
                break;
            }

            // Build the next decode batch: one token per still-active
            // seq. Preserve seq_id so each seq's KV cache slice keeps
            // accumulating against its own positions; track the new
            // batch index so the next sample call reads the right
            // logits.
            batch.clear();
            let mut new_logit_idx = vec![-1i32; requests.len()];
            let mut bi: i32 = 0;
            for seq in 0..requests.len() {
                if !active[seq] {
                    continue;
                }
                let Some(tok) = next_tokens[seq] else {
                    // Active seq must have a token to feed unless it
                    // retired this round (handled above) — defensive
                    // skip.
                    continue;
                };
                batch
                    .add(tok, next_pos[seq], &[seq as i32], true)
                    .map_err(|e| {
                        Error::Inference(format!("Batched decode add failed: {e}"))
                    })?;
                new_logit_idx[seq] = bi;
                next_pos[seq] += 1;
                bi += 1;
            }
            current_logit_idx = new_logit_idx;

            ctx.decode(&mut batch).map_err(|e| {
                Error::Inference(format!("Batched decode step failed: {e}"))
            })?;
        }

        // Cleanup — leave the KV cache empty so the next caller (or a
        // partial-batch retry) doesn't pick up stale positions.
        ctx.clear_kv_cache();

        let mut results = Vec::with_capacity(requests.len());
        for (seq, output) in outputs.into_iter().enumerate() {
            results.push((output, tokenized[seq].len(), n_generated[seq]));
        }
        Ok(results)
    }

    fn generate_stream_sync(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<Result<String>>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        // Pre-clear for the same reason as generate_sync: a prior failed decode
        // leaves the cache dirty, causing M-RoPE position errors on the next call.
        ctx.clear_kv_cache();

        let full_prompt = format_prompt(model, model_id, request, quirks)?;

        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(
            request.max_tokens,
            tokens.len(),
            n_ctx,
        )?;

        let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;

        let mut sampler = build_sampler(model, request, quirks);
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        // Wall-clock deadline mirrors generate_sync — a stuck
        // mask-sample step can starve the slot regardless of whether
        // streaming is on. Cancellation checks below still take
        // priority for clean shutdowns; the deadline is the backstop
        // for the case where the sample step itself is slow enough
        // that cancel-checks fire too rarely to help.
        let deadline_secs = inference_deadline_secs();
        let deadline = Instant::now()
            + std::time::Duration::from_secs(deadline_secs);
        let started_at = Instant::now();

        let mut tail = String::with_capacity(32);
        let mut in_think = false;
        let mut think_tokens = 0usize;
        let mut think_budget_fired = false;
        // Stray-close tracking: see plan §1.3. We can't rewrap once
        // pieces have flushed to the receiver, but we log so the
        // operator can see this happen.
        let mut saw_open_tag = false;
        let mut saw_close_tag = false;

        // See generate_sync: stop on `</tool_call>` (marker) or
        // balanced JSON envelope (grammar-locked) when tools were
        // requested.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let mut tc_json_depth = 0i32;
        let mut tc_json_in_string = false;
        let mut tc_json_escape_next = false;
        let mut tc_json_ever_opened = false;

        while n_generated < max_tokens {
            if Instant::now() > deadline {
                let elapsed = started_at.elapsed().as_secs();
                tracing::warn!(
                    model = %model_id,
                    elapsed_s = elapsed,
                    deadline_s = deadline_secs,
                    n_generated,
                    schema = request.structured_output.is_some(),
                    "inference:deadline exceeded (stream) — clearing KV cache and returning"
                );
                ctx.clear_kv_cache();
                return Err(Error::Inference(format!(
                    "inference deadline exceeded after {elapsed}s ({n_generated} tokens generated; \
                     deadline={deadline_secs}s) — likely pathological JSON-Schema mask state. \
                     Set SOVEREIGN_INFERENCE_TIMEOUT_SECS to override."
                )));
            }

            // Antifragile-routing cancel check: redirect-induced
            // cancellation on the session-level CancellationToken stops
            // the sampler without waiting for the receiver to drop.
            // Receiver-drop (below) remains the de-facto cancel path when
            // no token is threaded; both converge on `break`.
            if let Some(t) = cancel {
                if t.is_cancelled() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via CancellationToken"
                    );
                    break;
                }
            }

            let role = if tools_present && tc_json_in_string {
                SamplerRole::Content
            } else {
                SamplerRole::Explore
            };
            let token = sampler.sample(ctx, -1, role);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                tail.push_str(&piece);
                if tail.len() > 32 {
                    // Walk back from len-32 to the nearest char boundary so we
                    // never slice through a multi-byte UTF-8 sequence (Qwen3).
                    let mut drain_to = tail.len() - 32;
                    while drain_to > 0 && !tail.is_char_boundary(drain_to) {
                        drain_to -= 1;
                    }
                    tail.drain(..drain_to);
                }
                if !in_think && tail.contains("<think>") {
                    in_think = true;
                    saw_open_tag = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                    saw_close_tag = true;
                } else if !in_think && tail.contains("</think>") {
                    saw_close_tag = true;
                }
                if in_think {
                    think_tokens += 1;
                }

                // Tool-emission JSON-balance bookkeeping — done BEFORE
                // the send so we don't need to re-borrow piece bytes
                // after the move into the channel.
                let mut json_envelope_complete = false;
                if tools_present && !in_think {
                    for b in piece.bytes() {
                        if tc_json_escape_next {
                            tc_json_escape_next = false;
                            continue;
                        }
                        if tc_json_in_string {
                            match b {
                                b'\\' => tc_json_escape_next = true,
                                b'"' => tc_json_in_string = false,
                                _ => {}
                            }
                            continue;
                        }
                        match b {
                            b'"' => tc_json_in_string = true,
                            b'{' => {
                                tc_json_depth += 1;
                                tc_json_ever_opened = true;
                            }
                            b'}' => tc_json_depth -= 1,
                            _ => {}
                        }
                    }
                    if tc_json_ever_opened && tc_json_depth == 0 {
                        json_envelope_complete = true;
                    }
                }

                if tx.blocking_send(Ok(piece)).is_err() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via receiver-drop"
                    );
                    break;
                }

                // Tool-emission stop — see generate_sync for rationale.
                if tools_present {
                    if tail.contains("</tool_call>") {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            tail = %tail,
                            "inference: stopping on </tool_call> marker (stream)"
                        );
                        break;
                    }
                    if json_envelope_complete {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            "inference: stopping on balanced JSON envelope (stream)"
                        );
                        break;
                    }
                }
            }

            n_generated += 1;

            batch.clear();
            batch
                .add(token, (tokens.len() + n_generated - 1) as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;

            if in_think && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET) && !think_budget_fired {
                think_budget_fired = true;
                let force = "\n</think>\n\n";
                if let Ok(close_tokens) = model.str_to_token(force, AddBos::Never) {
                    for &ct in &close_tokens {
                        let fp = model
                            .token_to_piece(ct, &mut decoder, true, None)
                            .unwrap_or_default();
                        sampler.accept(ct);
                        batch.clear();
                        batch
                            .add(ct, (tokens.len() + n_generated) as i32, &[0], true)
                            .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
                        ctx.decode(&mut batch)
                            .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;
                        n_generated += 1;
                        if tx.blocking_send(Ok(fp)).is_err() {
                            break;
                        }
                    }
                }
                in_think = false;
            }
        }

        ctx.clear_kv_cache();

        if saw_close_tag && !saw_open_tag {
            tracing::warn!(
                model = model_id,
                "thinking-tag stray close (legacy stream): model emitted \
                 </think> without a preceding <think> — desktop will repair \
                 the rendering but tokens already flushed; cause is usually \
                 enable_thinking:false on a thinking model"
            );
        }

        Ok(())
    }

    /// Streaming generator that yields typed [`StreamFrame`]s and
    /// always terminates the channel with [`StreamFrame::Finish`]
    /// carrying the real [`FinishReason`] (`Stop` on EOS, `Length`
    /// on `max_tokens`, `Cancelled` on receiver-drop /
    /// CancellationToken, `Error(_)` on decode/batch faults).
    ///
    /// Mirrors [`Self::generate_stream_sync`] but for the new
    /// [`InferenceProvider::complete_stream_with_finish`] surface.
    /// The legacy helper keeps working for callers still on the
    /// `Result<String>` shape.
    fn generate_stream_sync_with_finish(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<StreamFrame>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        ctx.clear_kv_cache();

        let full_prompt = format_prompt(model, model_id, request, quirks)?;
        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(
            request.max_tokens,
            tokens.len(),
            n_ctx,
        )?;
        let prompt_tokens = tokens.len();

        let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;

        let mut sampler = build_sampler(model, request, quirks);
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        let mut tail = String::with_capacity(32);
        let mut in_think = false;
        let mut think_tokens = 0usize;
        let mut think_budget_fired = false;
        // Stray-close tracking: see plan §1.3. Streaming can't
        // rewrap once tokens have flushed; we log so the operator
        // can see this happen and the desktop's parse-message
        // synthesises a virtual `<think>` open client-side.
        let mut saw_open_tag = false;
        let mut saw_close_tag = false;

        // See generate_sync: stop on `</tool_call>` (marker) or
        // balanced JSON envelope (grammar-locked) when tools were
        // requested.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let mut tc_json_depth = 0i32;
        let mut tc_json_in_string = false;
        let mut tc_json_escape_next = false;
        let mut tc_json_ever_opened = false;

        // Default to Length: if the loop exits via the `while`
        // condition we hit max_tokens. Each `break` path overwrites
        // this with the more-specific reason.
        let mut reason = FinishReason::Length;

        while n_generated < max_tokens {
            if let Some(t) = cancel {
                if t.is_cancelled() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via CancellationToken"
                    );
                    reason = FinishReason::Cancelled;
                    break;
                }
            }

            let role = if tools_present && tc_json_in_string {
                SamplerRole::Content
            } else {
                SamplerRole::Explore
            };
            let token = sampler.sample(ctx, -1, role);
            sampler.accept(token);

            if model.is_eog_token(token) {
                reason = FinishReason::Stop;
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                tail.push_str(&piece);
                if tail.len() > 32 {
                    let mut drain_to = tail.len() - 32;
                    while drain_to > 0 && !tail.is_char_boundary(drain_to) {
                        drain_to -= 1;
                    }
                    tail.drain(..drain_to);
                }
                if !in_think && tail.contains("<think>") {
                    in_think = true;
                    saw_open_tag = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                    saw_close_tag = true;
                } else if !in_think && tail.contains("</think>") {
                    saw_close_tag = true;
                }
                if in_think {
                    think_tokens += 1;
                }

                // JSON-balance bookkeeping BEFORE the send so we don't
                // need to re-borrow piece bytes after the move.
                let mut json_envelope_complete = false;
                if tools_present && !in_think {
                    for b in piece.bytes() {
                        if tc_json_escape_next {
                            tc_json_escape_next = false;
                            continue;
                        }
                        if tc_json_in_string {
                            match b {
                                b'\\' => tc_json_escape_next = true,
                                b'"' => tc_json_in_string = false,
                                _ => {}
                            }
                            continue;
                        }
                        match b {
                            b'"' => tc_json_in_string = true,
                            b'{' => {
                                tc_json_depth += 1;
                                tc_json_ever_opened = true;
                            }
                            b'}' => tc_json_depth -= 1,
                            _ => {}
                        }
                    }
                    if tc_json_ever_opened && tc_json_depth == 0 {
                        json_envelope_complete = true;
                    }
                }

                if tx.blocking_send(StreamFrame::Token(piece)).is_err() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via receiver-drop"
                    );
                    // Receiver is gone — don't try to send a Finish
                    // frame either. Caller already knows.
                    return Ok(());
                }

                // Tool-emission stop — see generate_sync for rationale.
                if tools_present {
                    if tail.contains("</tool_call>") {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            tail = %tail,
                            "inference: stopping on </tool_call> marker (stream-finish)"
                        );
                        reason = FinishReason::Stop;
                        break;
                    }
                    if json_envelope_complete {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            "inference: stopping on balanced JSON envelope (stream-finish)"
                        );
                        reason = FinishReason::Stop;
                        break;
                    }
                }
            }

            n_generated += 1;

            batch.clear();
            batch
                .add(token, (tokens.len() + n_generated - 1) as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;

            if in_think && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET) && !think_budget_fired {
                think_budget_fired = true;
                let force = "\n</think>\n\n";
                if let Ok(close_tokens) = model.str_to_token(force, AddBos::Never) {
                    for &ct in &close_tokens {
                        let fp = model
                            .token_to_piece(ct, &mut decoder, true, None)
                            .unwrap_or_default();
                        sampler.accept(ct);
                        batch.clear();
                        batch
                            .add(ct, (tokens.len() + n_generated) as i32, &[0], true)
                            .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
                        ctx.decode(&mut batch)
                            .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;
                        n_generated += 1;
                        if tx.blocking_send(StreamFrame::Token(fp)).is_err() {
                            return Ok(());
                        }
                    }
                }
                in_think = false;
            }
        }

        ctx.clear_kv_cache();

        if saw_close_tag && !saw_open_tag {
            tracing::warn!(
                model = model_id,
                "thinking-tag stray close (typed stream): model emitted \
                 </think> without a preceding <think> — desktop will repair \
                 the rendering but tokens already flushed; cause is usually \
                 enable_thinking:false on a thinking model"
            );
        }

        let usage = StreamUsage {
            prompt_tokens: prompt_tokens as u32,
            completion_tokens: n_generated as u32,
            total_tokens: (prompt_tokens + n_generated) as u32,
        };
        let _ = tx.blocking_send(StreamFrame::Finish {
            reason,
            usage: Some(usage),
        });

        Ok(())
    }
}

// ─── EmbedSlot ────────────────────────────────────────────────

/// Dedicated embedding slot. Loads an embedding model (e.g.
/// `Qwen3-Embedding-0.6B-Q4_K_M.gguf`) with `embeddings = true` and
/// mean pooling, so a single decode pass yields one normalized
/// sentence vector.
///
/// The slot stores its `LlamaContext` with an extended `'static`
/// lifetime via the same `Arc<LlamaModel>` trick the chat slot uses.
/// We clear the KV cache between calls so each `embed()` invocation
/// is an independent sequence.
struct EmbedSlot {
    model: Arc<LlamaModel>,
    /// Pool of contexts for parallel batch embedding. contexts[0] is
    /// the primary context used by single `embed()` calls. All contexts
    /// share the same model weights (via Arc) — only the KV cache is
    /// per-context (~50MB each for a 2048-token embedding model).
    contexts: Vec<Mutex<EmbedSlotContext>>,
    /// Embedding dimensionality reported by the model. Cached so the
    /// hot path doesn't have to call `model.n_embd()` on every embed.
    n_embd: usize,
    /// Per-input truncation limit. Any single text longer than this
    /// is clipped before decode. Named separately from the context
    /// size so bin-packing logic doesn't conflate "how big one seq
    /// can be" with "how big a packed sub-batch can be".
    max_input_tokens: usize,
    /// Total tokens the KV cache can hold across all active
    /// sequences in one packed decode. Governs how many chunks we
    /// can pack into a single sub-batch in `run_embed_batch_sync`.
    ctx_tokens: usize,
    /// Upper bound on distinct sequences per packed batch, matching
    /// the value passed to `LlamaContextParams::with_n_seq_max`.
    n_seq_max: usize,
    /// Family-specific embedding configuration: pooling strategy,
    /// instruction prefixes, EOS appending. None = Qwen3-Embedding and similar
    /// defaults (mean pooling, no instructions, no EOS).
    embed_quirks: Option<EmbedQuirks>,
}

struct EmbedSlotContext {
    ctx: llama_cpp_2::context::LlamaContext<'static>,
    _model: Arc<LlamaModel>,
}

unsafe impl Send for EmbedSlotContext {}
unsafe impl Sync for EmbedSlotContext {}

impl EmbedSlot {
    fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        n_gpu_layers: u32,
        embed_quirks: Option<EmbedQuirks>,
    ) -> Result<Self> {
        // Force full-GPU offload when a GPU is present but the caller
        // passed 0. On Apple Silicon, Metal embed runs at ~33 seq/sec
        // vs ~2.8 seq/sec CPU (12×). On Strix Halo Vulkan it's ~40+
        // seq/sec vs 3.7-4.9 chunks/s CPU (see docs/TOOLBOX_SETUP.md
        // §9). Intel Macs stay excluded: the Metal scheduler crashes
        // in `ggml_metal_synchronize` there (hardware OOM / command-
        // buffer timeout), which is why the macOS arm is gated on
        // aarch64 rather than the broader "macos" predicate.
        // The old `GGML_ASSERT(buf_src)` crash was fixed in
        // llama-cpp-2 0.1.141; the fallback-to-CPU path below still
        // guards against context-creation failures on any platform.
        let gpu_default_available = cfg!(all(target_os = "macos", target_arch = "aarch64"))
            || cfg!(target_os = "linux");
        let requested_gpu_layers = if gpu_default_available && n_gpu_layers == 0 {
            999 // every layer on GPU
        } else {
            n_gpu_layers
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load embed model: {e}")))?;

        let n_embd = model.n_embd() as usize;
        // Per-input truncation: any single text longer than this is
        // clipped. Must satisfy max_input_tokens ≤ ctx_tokens / n_seq_max
        // so a full-length chunk always fits in one KV cache slot.
        let max_input_tokens = 1024;
        // Aggregate KV cache capacity. 16 slots × 1024 tokens = 16384.
        // On CPU (Intel Mac or a GPU-fallback path) this is pure RAM —
        // ~3.7 GB for a 28-layer/1024-dim model, well within 16 GB+.
        // On Apple Silicon Metal or Linux ROCm/Vulkan the same 16384
        // tokens work fine (30 GB+ unified on macOS; 124 GiB GTT on
        // Strix Halo). Intel Mac never reaches the GPU path because
        // the aarch64 gate above keeps requested_gpu_layers=0 there,
        // so wants_gpu=false and we always take the CPU context branch.
        let ctx_tokens: u32 = 16384;
        // 16 parallel sequence slots. Drives 256-seq batches through
        // 256/16=16 decode calls instead of 256/4=64, giving ~4× the
        // throughput. n_seq_max was temporarily reduced to 4 while
        // debugging Metal OOM on Intel Macs; now that Intel never
        // enters the Metal path (aarch64-only gate above) there is no
        // GPU memory pressure and we can restore the original value.
        let n_seq_max: u32 = 16;
        let model = Arc::new(model);

        let pooling_type = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => LlamaPoolingType::Last,
            Some(PoolingStrategy::Cls)  => LlamaPoolingType::Cls,
            _                           => LlamaPoolingType::Mean,
        };

        let n_threads = llama_threads_for_host();
        let wants_gpu =
            cfg!(any(target_os = "macos", target_os = "linux")) && requested_gpu_layers > 0;
        let build_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(ctx_tokens))
                .with_n_batch(ctx_tokens)
                .with_n_ubatch(2048)
                .with_n_seq_max(n_seq_max)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_embeddings(true)
                .with_pooling_type(pooling_type)
                // GPU offload toggles. `with_offload_kqv(true)` puts
                // the KV cache in GPU memory; `with_op_offload(true)`
                // lets the GGML scheduler route compute ops to the GPU.
                // When both are true (the default when GPU works), the
                // decode runs entirely on the GPU. The CPU-only fallback
                // disables both explicitly so the scheduler keeps every
                // tensor in main memory.
                .with_offload_kqv(gpu)
                .with_op_offload(gpu)
        };

        // Try GPU first if compiled in; fall back to CPU on any
        // context-creation error. The common failure mode on older
        // llama.cpp builds was Metal's `GGML_ASSERT(buf_src)` — ugly
        // from the crash's perspective, but if it happens before
        // `new_context` returns we just see `Err` here and retry.
        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_params(true))
                    .map(|c| (c, true))
            }
        } else {
            Err(llama_cpp_2::LlamaContextLoadError::NullReturn)
        } {
            Ok(pair) => pair,
            Err(e) => {
                if wants_gpu {
                    tracing::warn!(
                        error = ?e,
                        "GPU embed context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_params(false))
                        .map_err(|e| {
                            Error::Inference(format!(
                                "Failed to create embed context: {e}"
                            ))
                        })?
                };
                (ctx, false)
            }
        };

        let pooling_name = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => "last-token",
            Some(PoolingStrategy::Cls)  => "cls",
            _                           => "mean",
        };
        // Report the actual backend so logs show whether GPU
        // took (33 seq/sec on M2 Max Metal, measured; ~40+ seq/sec
        // on Strix Halo Vulkan) or we fell back to CPU (2.8 seq/sec
        // peak on M2 Max, 3.7-4.9 chunks/s on Strix Halo).
        // Key diagnostic — a silent CPU fallback is the exact class
        // of regression that triggered the benchmark-driven
        // investigation of this path.
        let compute_backend = if used_gpu {
            gpu_backend_label()
        } else {
            embed_compute_backend_label()
        };
        tracing::info!(
            dims = n_embd,
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            pooling = pooling_name,
            n_ctx = ctx_tokens,
            n_seq_max,
            max_input_tokens,
            n_threads,
            compute_backend,
            "EmbedSlot::load — slot ready"
        );

        Ok(Self {
            model: model.clone(),
            contexts: vec![Mutex::new(EmbedSlotContext {
                ctx,
                _model: model,
            })],
            n_embd,
            max_input_tokens,
            ctx_tokens: ctx_tokens as usize,
            n_seq_max: n_seq_max as usize,
            embed_quirks,
        })
    }

    /// Embed a document string, applying the document-side instruction prefix
    /// and EOS token from the embed quirks (when configured).
    fn embed_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        text: &str,
        n_embd: usize,
        max_input_tokens: usize,
        embed_quirks: Option<&EmbedQuirks>,
    ) -> Result<Vec<f32>> {
        let prepared = if let Some(eq) = embed_quirks {
            let prefixed = format!("{}{text}", eq.document_instruction);
            if eq.append_eos_token {
                format!("{prefixed}<|endoftext|>")
            } else {
                prefixed
            }
        } else {
            text.to_string()
        };
        Self::run_embed_sync(model, ctx, &prepared, n_embd, max_input_tokens)
    }

    /// Embed a query string, applying the query-side instruction prefix
    /// and EOS token from the embed quirks (when configured).
    fn embed_query_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        query: &str,
        n_embd: usize,
        max_input_tokens: usize,
        embed_quirks: Option<&EmbedQuirks>,
    ) -> Result<Vec<f32>> {
        let prepared = if let Some(eq) = embed_quirks {
            let prefixed = format!("{}{query}", eq.query_instruction);
            if eq.append_eos_token {
                format!("{prefixed}<|endoftext|>")
            } else {
                prefixed
            }
        } else {
            query.to_string()
        };
        Self::run_embed_sync(model, ctx, &prepared, n_embd, max_input_tokens)
    }

    /// Single-sequence embedding routine. Kept alongside the
    /// multi-sequence [`run_embed_batch_sync`] for single-query
    /// paths (e.g. `embed_query`) where building a packed batch
    /// would be pure overhead.
    fn run_embed_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        input: &str,
        n_embd: usize,
        max_input_tokens: usize,
    ) -> Result<Vec<f32>> {
        ctx.clear_kv_cache();

        let mut tokens = model
            .str_to_token(input, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Embed tokenization failed: {e}")))?;
        if tokens.len() > max_input_tokens {
            tokens.truncate(max_input_tokens);
        }
        if tokens.is_empty() {
            return Ok(vec![0.0; n_embd]);
        }

        let mut batch = LlamaBatch::new(max_input_tokens, 1);
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Embed batch add failed: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;

        let raw = ctx
            .embeddings_seq_ith(0)
            .map_err(|e| Error::Inference(format!("Failed to read embeddings: {e}")))?;
        Ok(l2_normalize(raw.to_vec()))
    }

    /// Batch-embed multiple texts by packing distinct sequences
    /// into a single `LlamaBatch` and calling `ctx.decode` once per
    /// sub-batch.
    ///
    /// ## Why this exists
    ///
    /// The prior implementation was a sequential loop over
    /// `embed_sync`. Each iteration reset the KV cache and ran a
    /// full decode pass for one sequence, so a 256-input call
    /// performed 256 single-sequence prefills. On Apple-Silicon
    /// CPU with Accelerate (Metal is disabled for embedding
    /// contexts due to a llama.cpp `GGML_ASSERT(buf_src)` crash —
    /// see the `EmbedSlot::load` comment) the per-sequence decode
    /// is dominated by the attention + FFN matmuls. Accelerate's
    /// SGEMM amortises dispatch + cache misses across the batch
    /// dimension, so packing N sequences into one decode delivers
    /// close to N× the FLOPs with ~1× the wall time until we
    /// saturate the vector engine.
    ///
    /// ## Strategy
    ///
    /// 1. Tokenize every input up front (no llama.cpp state, so
    ///    we can do this outside the context lock).
    /// 2. Bin-pack tokenized inputs into sub-batches subject to:
    ///      (a) total tokens ≤ `slot.ctx_tokens`
    ///      (b) sequence count ≤ `slot.n_seq_max`
    /// 3. For each sub-batch: clear KV cache, add tokens with
    ///    distinct `seq_id`s, `decode()`, then read one pooled
    ///    embedding per sequence.
    /// 4. Splice results back in input order — bin-packing is
    ///    order-preserving so this is just concatenation.
    ///
    /// A pathological input longer than `slot.ctx_tokens` is
    /// truncated to `slot.max_input_tokens` up front, so single
    /// oversized chunks always fit in their own sub-batch.
    fn run_embed_batch_sync(
        slot: &EmbedSlot,
        inputs: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize every input without holding the context lock.
        // Tokenization is pure (model vocab is immutable after
        // load); keeping it outside the lock shortens the
        // critical section and lets a future caller tokenize in
        // parallel if we ever move that behind a thread pool.
        let mut prepared: Vec<Vec<LlamaToken>> = Vec::with_capacity(inputs.len());
        for text in inputs {
            let prefixed = match slot.embed_quirks.as_ref() {
                Some(eq) => {
                    let mut s = format!("{}{}", eq.document_instruction, text);
                    if eq.append_eos_token {
                        s.push_str("<|endoftext|>");
                    }
                    s
                }
                None => text.to_string(),
            };
            let mut toks = slot
                .model
                .str_to_token(&prefixed, AddBos::Always)
                .map_err(|e| Error::Inference(format!("Embed tokenization failed: {e}")))?;
            if toks.len() > slot.max_input_tokens {
                toks.truncate(slot.max_input_tokens);
            }
            prepared.push(toks);
        }

        let batch_start = std::time::Instant::now();
        let mut ctx_lock = slot.contexts[0].blocking_lock();
        let mut results: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());

        let mut sub_batches = 0usize;
        let mut cursor = 0usize;
        // Reuse one `LlamaBatch` allocation across sub-batches;
        // `clear()` resets n_tokens without freeing the backing
        // memory. Matches the context's n_batch capacity.
        let mut batch = LlamaBatch::new(slot.ctx_tokens, slot.n_seq_max as i32);

        // Pack to ~90% of the nominal ctx budget. llama.cpp uses
        // some headroom inside the unified KV cache for scheduler
        // bookkeeping; packing to exactly `ctx_tokens` has tripped
        // `NoKvCacheSlot` at decode time on real-world corpora.
        let pack_budget = slot.ctx_tokens.saturating_sub(slot.ctx_tokens / 10);

        while cursor < prepared.len() {
            // Greedily accumulate sequences into `batch` until the
            // next one would overflow either bound. `take >= 1`
            // always — a single input exceeding ctx_tokens already
            // had its tokens truncated to max_input_tokens, and
            // max_input_tokens ≤ ctx_tokens / n_seq_max by
            // construction at `EmbedSlot::load`.
            batch.clear();
            let mut tokens_in_batch = 0usize;
            let mut seqs_in_batch = 0usize;
            let sub_start = cursor;

            while cursor < prepared.len() {
                let next_len = prepared[cursor].len().max(1);
                if seqs_in_batch > 0
                    && (tokens_in_batch + next_len > pack_budget
                        || seqs_in_batch + 1 > slot.n_seq_max)
                {
                    break;
                }
                let seq_id = seqs_in_batch as i32;
                let toks = &prepared[cursor];
                if toks.is_empty() {
                    // Zero-token input: record a sentinel so we
                    // can inject a zero vector without running it
                    // through llama.cpp (adding zero tokens to a
                    // batch is a no-op that then has no seq to
                    // read from).
                    // We handle this by NOT advancing seqs_in_batch
                    // for this input; instead we account for it
                    // after the decode. Track its global index.
                    // Simpler: fabricate a synthetic token? Cheaper
                    // to skip. We'll handle below by post-filling.
                } else {
                    for (pos, &tok) in toks.iter().enumerate() {
                        batch
                            .add(tok, pos as i32, &[seq_id], true)
                            .map_err(|e| {
                                Error::Inference(format!("Embed batch add failed: {e}"))
                            })?;
                    }
                    tokens_in_batch += next_len;
                    seqs_in_batch += 1;
                }
                cursor += 1;
            }

            // Decode whatever sequences were packed. If every
            // input in the range was empty, `seqs_in_batch == 0`
            // and we skip the decode entirely.
            if seqs_in_batch > 0 {
                ctx_lock.ctx.clear_kv_cache();
                ctx_lock
                    .ctx
                    .decode(&mut batch)
                    .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;
            }

            // Walk the inputs that belonged to this sub-batch and
            // emit one embedding per input, mapping local seq_id
            // back to the original input slot. Empty inputs get
            // zero vectors of the right length.
            let mut local_seq: i32 = 0;
            for local_idx in sub_start..cursor {
                if prepared[local_idx].is_empty() {
                    results.push(vec![0.0; slot.n_embd]);
                } else {
                    let raw = ctx_lock
                        .ctx
                        .embeddings_seq_ith(local_seq)
                        .map_err(|e| {
                            Error::Inference(format!("Failed to read embedding: {e}"))
                        })?;
                    results.push(l2_normalize(raw.to_vec()));
                    local_seq += 1;
                }
            }
            sub_batches += 1;
        }

        let total_ms = batch_start.elapsed().as_millis();
        let seqs_per_sec = if total_ms > 0 {
            (inputs.len() as f64 / total_ms as f64) * 1000.0
        } else {
            0.0
        };
        tracing::info!(
            sequences = inputs.len(),
            sub_batches,
            total_ms,
            seqs_per_sec = format!("{seqs_per_sec:.1}"),
            "Batch embed complete"
        );

        Ok(results)
    }
}

// ─── RerankSlot ───────────────────────────────────────────────

/// Dedicated cross-encoder reranker slot. Loads a reranker GGUF
/// (e.g. `jina-reranker-v3-Q6_K.gguf`, `bge-reranker-v2-m3-Q8_0.gguf`)
/// with `embeddings = true` and `pooling_type = LLAMA_POOLING_TYPE_RANK`
/// so each decode pass yields a single scalar relevance logit per
/// (query, doc) pair.
///
/// The slot is lazy-loaded on first rerank call and held resident
/// thereafter — at ~500 MB on disk for a 0.6B Q6_K model, idle
/// cost is acceptable on `default`/`high`/`very_high` profiles. We
/// clear the KV cache between calls so each pair is an independent
/// sequence.
///
/// Public so `StandaloneReranker` (see `reranker_standalone.rs`) can
/// hold one directly without going through `EmbeddedLlamaCpp` —
/// useful for eval processes that don't want to pay the cost of
/// loading host chat slots they'll never call.
pub struct RerankSlot {
    model: Arc<LlamaModel>,
    // std::sync::Mutex (not tokio::sync) because score_batch runs
    // inside spawn_blocking — the lock is held across a blocking
    // llama.cpp decode, never across an `.await`.
    ctx: std::sync::Mutex<RerankSlotContext>,
    /// Hard cap on the full prompt (chat-template wrapping + query +
    /// doc + trailing rerank token). Pairs longer than this are
    /// truncated on the doc side.
    max_input_tokens: usize,
    /// Token-ID of `<|score_token|>` — read once at load time. The
    /// reranker emits this token's logit at the last-input position
    /// and that logit IS the relevance score.
    score_token_id: i32,
    /// Token-ID of `<|rerank_token|>` — placed at the end of the
    /// prompt so the model conditions on "now produce a score".
    rerank_token_id: i32,
    /// Pre-tokenised chat-template prefix (everything up to where
    /// the query gets spliced in). Built once at load time so the
    /// hot path skips re-tokenising the boilerplate.
    prefix_tokens: Vec<LlamaToken>,
    /// Pre-tokenised separator between query and document (the
    /// `<|im_end|>\n<doc>` glue in the Qwen3 chat template).
    middle_tokens: Vec<LlamaToken>,
    /// Pre-tokenised suffix between document and `<|rerank_token|>`
    /// (the `<|im_end|>\n<|im_start|>assistant\n` glue).
    suffix_tokens: Vec<LlamaToken>,
    /// File stem of the loaded reranker — surfaced in provenance +
    /// telemetry so a user can tell which reranker scored a given
    /// result. Format matches `ModelSlot.model_id`.
    model_id: String,
}

/// Build the Qwen3-style chat-template fragments used to wrap a
/// (query, doc) pair into a jina-reranker-v3 prompt.
///
/// The protocol: a chat-template prompt containing the query +
/// document, ending with the special `<|rerank_token|>` so the
/// model conditions on "produce a score next". The relevance
/// score is then read off as the logit of `<|score_token|>` at
/// the last-input position.
///
/// `SOVEREIGN_RERANK_PROMPT_VARIANT` selects between templates so
/// we can A/B without recompiling. Values:
///   - `verbose` (default-historic) — full system message + XML tags
///   - `lean` — no system message, `Query: ... Document: ...` body
fn jina_v3_prompt_fragments() -> (String, String, String) {
    let variant = std::env::var("SOVEREIGN_RERANK_PROMPT_VARIANT")
        .unwrap_or_else(|_| "verbose".to_string());
    match variant.as_str() {
        "lean" => {
            let prefix = "<|im_start|>user\nQuery: ".to_string();
            let middle = "\n\nDocument: ".to_string();
            let suffix = "\n<|im_end|>\n<|im_start|>assistant\n".to_string();
            (prefix, middle, suffix)
        }
        _ => {
            let prefix = "<|im_start|>system\nYou are a search relevance assessor. \
                Read the query and document, then judge how relevant the document \
                is to the query.\n<|im_end|>\n<|im_start|>user\n<query>\n"
                .to_string();
            let middle = "\n</query>\n<document>\n".to_string();
            let suffix = "\n</document>\n<|im_end|>\n<|im_start|>assistant\n".to_string();
            (prefix, middle, suffix)
        }
    }
}

struct RerankSlotContext {
    ctx: llama_cpp_2::context::LlamaContext<'static>,
    _model: Arc<LlamaModel>,
}

unsafe impl Send for RerankSlotContext {}
unsafe impl Sync for RerankSlotContext {}

impl RerankSlot {
    pub fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        n_gpu_layers: u32,
        rerank_quirks: Option<RerankQuirks>,
    ) -> Result<Self> {
        // Same GPU-first / CPU-fallback policy as EmbedSlot.
        let gpu_default_available = cfg!(all(target_os = "macos", target_arch = "aarch64"))
            || cfg!(target_os = "linux");
        let requested_gpu_layers = if gpu_default_available && n_gpu_layers == 0 {
            999
        } else {
            n_gpu_layers
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load reranker model: {e}")))?;

        // The reranker emits one scalar per sequence — n_embd is
        // typically 1 in this mode but we don't read it; we just
        // grab the first float of the rank output.
        let max_input_tokens = rerank_quirks
            .as_ref()
            .map(|q| q.max_context)
            .unwrap_or(8192);
        let ctx_tokens: u32 = max_input_tokens as u32;
        let model = Arc::new(model);

        let n_threads = llama_threads_for_host();
        let wants_gpu =
            cfg!(any(target_os = "macos", target_os = "linux")) && requested_gpu_layers > 0;
        // Generative mode: pooling=None, embeddings=false. The
        // reranker is a Qwen3 LLM whose relevance score lives in
        // the logit for `<|score_token|>` at the position right
        // after a trailing `<|rerank_token|>` — there is no BERT-
        // style pooled rank head to read from. Setting
        // pooling=Rank on this model silently returns zeros (the
        // mode jina-reranker-v3 doesn't implement).
        let build_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(ctx_tokens))
                .with_n_batch(ctx_tokens)
                .with_n_ubatch(ctx_tokens.min(2048))
                .with_n_seq_max(1)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
                .with_op_offload(gpu)
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_params(true))
                    .map(|c| (c, true))
            }
        } else {
            Err(llama_cpp_2::LlamaContextLoadError::NullReturn)
        } {
            Ok(pair) => pair,
            Err(e) => {
                if wants_gpu {
                    tracing::warn!(
                        error = ?e,
                        "GPU rerank context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_params(false))
                        .map_err(|e| {
                            Error::Inference(format!(
                                "Failed to create rerank context: {e}"
                            ))
                        })?
                };
                (ctx, false)
            }
        };

        let model_id = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("reranker")
            .to_string();

        // Resolve the two special tokens by name. They're added by
        // Jina's fine-tune and embedded in the GGUF tokenizer.
        // `str_to_token` returns them when the string matches a
        // special token exactly. If either lookup fails, the load
        // fails up front — we never want to silently fall through to
        // garbage scores at query time.
        let score_token_id =
            resolve_special_token(&model, "<|score_token|>").ok_or_else(|| {
                Error::Inference(
                    "Reranker GGUF lacks `<|score_token|>` — does this model \
                     follow the jina-reranker-v3 protocol?"
                        .to_string(),
                )
            })?;
        let rerank_token_id =
            resolve_special_token(&model, "<|rerank_token|>").ok_or_else(|| {
                Error::Inference(
                    "Reranker GGUF lacks `<|rerank_token|>` — does this model \
                     follow the jina-reranker-v3 protocol?"
                        .to_string(),
                )
            })?;

        // Pre-tokenise the static chat-template fragments. The hot
        // path stitches: prefix + query_tokens + middle + doc_tokens
        // + suffix + rerank_token. Boilerplate is non-trivial (~40
        // tokens) so caching saves a meaningful slice of per-pair
        // CPU.
        let (prefix_str, middle_str, suffix_str) = jina_v3_prompt_fragments();
        let prefix_tokens = model
            .str_to_token(&prefix_str, AddBos::Never)
            .map_err(|e| Error::Inference(format!("tokenise rerank prefix: {e}")))?;
        let middle_tokens = model
            .str_to_token(&middle_str, AddBos::Never)
            .map_err(|e| Error::Inference(format!("tokenise rerank middle: {e}")))?;
        let suffix_tokens = model
            .str_to_token(&suffix_str, AddBos::Never)
            .map_err(|e| Error::Inference(format!("tokenise rerank suffix: {e}")))?;

        let compute_backend = if used_gpu {
            gpu_backend_label()
        } else {
            embed_compute_backend_label()
        };
        tracing::info!(
            slot = "rerank",
            model_id = %model_id,
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            n_ctx = ctx_tokens,
            max_input_tokens,
            n_threads,
            compute_backend,
            score_token = score_token_id,
            rerank_token = rerank_token_id,
            prefix_tokens_n = prefix_tokens.len(),
            middle_tokens_n = middle_tokens.len(),
            suffix_tokens_n = suffix_tokens.len(),
            "RerankSlot::load — slot ready"
        );

        Ok(Self {
            model: model.clone(),
            ctx: std::sync::Mutex::new(RerankSlotContext {
                ctx,
                _model: model,
            }),
            max_input_tokens,
            score_token_id,
            rerank_token_id,
            prefix_tokens,
            middle_tokens,
            suffix_tokens,
            model_id,
        })
    }

}

/// Resolve a special token by its string form. Returns `None` when
/// the tokenizer doesn't carry the token (which means the model
/// doesn't speak the jina-reranker-v3 protocol — caller surfaces
/// a hard error).
fn resolve_special_token(model: &LlamaModel, name: &str) -> Option<i32> {
    // Special tokens are added by `str_to_token` when the string
    // matches the literal token form. `AddBos::Never` prevents
    // smuggling a BOS in front of the lookup.
    let toks = model.str_to_token(name, AddBos::Never).ok()?;
    // The lookup is well-formed only when it produces exactly one
    // token — otherwise the string didn't match a single special
    // entry and we treat the model as incompatible.
    if toks.len() == 1 {
        Some(toks[0].0)
    } else {
        None
    }
}

// Continuation of the RerankSlot impl block (split for the helper above).
impl RerankSlot {

    /// Score a single (query, doc) pair via the jina-reranker-v3
    /// protocol: build a Qwen3 chat-template prompt that wraps the
    /// pair, end with `<|rerank_token|>`, run one forward pass, and
    /// read the logit of `<|score_token|>` at the last input
    /// position. Higher = more relevant.
    #[allow(clippy::too_many_arguments)]
    fn score_pair_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        query: &str,
        doc: &str,
        max_input_tokens: usize,
        score_token_id: i32,
        rerank_token_id: i32,
        prefix_tokens: &[LlamaToken],
        middle_tokens: &[LlamaToken],
        suffix_tokens: &[LlamaToken],
    ) -> Result<f32> {
        ctx.clear_kv_cache();

        // Tokenise the dynamic parts.
        let query_tokens = model
            .str_to_token(query, AddBos::Never)
            .map_err(|e| Error::Inference(format!("Rerank query tokenise: {e}")))?;
        let doc_tokens = model
            .str_to_token(doc, AddBos::Never)
            .map_err(|e| Error::Inference(format!("Rerank doc tokenise: {e}")))?;

        // Truncation budget. Fixed overhead = prefix + middle +
        // suffix + 1 (the trailing rerank token). Whatever's left
        // is split: query keeps its full length up to a ceiling
        // (queries are usually short), and doc takes everything
        // else. When even the query won't fit, truncate query too.
        let fixed = prefix_tokens.len()
            + middle_tokens.len()
            + suffix_tokens.len()
            + 1;
        let dynamic_budget = max_input_tokens.saturating_sub(fixed);
        const QUERY_HARD_CAP: usize = 256;
        let query_budget = query_tokens.len().min(QUERY_HARD_CAP).min(dynamic_budget);
        let doc_budget = dynamic_budget.saturating_sub(query_budget);
        if dynamic_budget == 0 {
            return Err(Error::Inference(
                "Rerank context too small for prompt scaffold".to_string(),
            ));
        }

        let mut tokens: Vec<LlamaToken> =
            Vec::with_capacity(prefix_tokens.len() + query_budget + middle_tokens.len()
                + doc_budget + suffix_tokens.len() + 1);
        tokens.extend_from_slice(prefix_tokens);
        tokens.extend(query_tokens.iter().take(query_budget).copied());
        tokens.extend_from_slice(middle_tokens);
        tokens.extend(doc_tokens.iter().take(doc_budget).copied());
        tokens.extend_from_slice(suffix_tokens);
        tokens.push(LlamaToken(rerank_token_id));

        let mut batch = LlamaBatch::new(tokens.len(), 1);
        let last = tokens.len() - 1;
        for (i, &tok) in tokens.iter().enumerate() {
            // We only need logits at the LAST position — that's
            // where the model predicts the token after
            // `<|rerank_token|>` and the score-token logit lives.
            // Marking every position would waste memory and decode
            // time; marking only the last position is exactly what
            // the protocol needs.
            batch
                .add(tok, i as i32, &[0], i == last)
                .map_err(|e| Error::Inference(format!("Rerank batch add: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Rerank decode: {e}")))?;

        // Read the score-token logit at the last position. This is
        // the model's pre-softmax confidence that the next token
        // SHOULD be `<|score_token|>` — calibrated by fine-tuning
        // to be high for relevant pairs and low for irrelevant
        // ones.
        let logits = ctx.get_logits_ith(last as i32);
        let idx = score_token_id as usize;
        if idx >= logits.len() {
            return Err(Error::Inference(format!(
                "score_token_id {score_token_id} out of vocab ({} tokens)",
                logits.len()
            )));
        }
        Ok(logits[idx])
    }

    /// Score multiple docs against one query. Sequential under the
    /// slot mutex — generative decode of ~50-100 tokens per pair is
    /// fast on a 0.6B model (Vulkan/ROCm: ~10ms/pair), so a 50-doc
    /// rerank pass costs ~0.5s. True intra-sequence batching is
    /// possible but the engineering cost only pays off above ~200
    /// pairs/call, well beyond what corpus search produces.
    pub fn score_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let mut out = Vec::with_capacity(docs.len());
        let mut guard = self
            .ctx
            .lock()
            .map_err(|e| Error::Inference(format!("rerank lock poisoned: {e}")))?;
        for doc in docs {
            let score = Self::score_pair_sync(
                &self.model,
                &mut guard.ctx,
                query,
                doc,
                self.max_input_tokens,
                self.score_token_id,
                self.rerank_token_id,
                &self.prefix_tokens,
                &self.middle_tokens,
                &self.suffix_tokens,
            )?;
            out.push(score);
        }
        Ok(out)
    }
}

/// L2-normalize in place, preserving direction; all-zero vectors
/// pass through unchanged so downstream cosine-similarity search
/// treats them consistently.
fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

/// How many CPU threads to hand llama.cpp for a single context.
///
/// Benchmarked on M2 Max (8P + 4E) with Qwen3-Embedding-0.6B:
///
///   n_threads=1  → 0.44 seq/sec    (baseline)
///   n_threads=4  → 1.63 seq/sec    (3.7× — scales)
///   n_threads=8  → 2.83 seq/sec    (6.4× — peak)
///   n_threads=10 → 2.76 seq/sec    (6.3× — plateau)
///   n_threads=12 → 2.41 seq/sec    (5.5× — REGRESSION)
///
/// Peak is at the performance-core count. Going higher puts work
/// on the efficiency cores, which are slower per-thread AND drag
/// the P-cores down by forcing cross-cluster synchronisation. The
/// crate's default of 4 under-uses any modern machine; our
/// earlier fix of "use everything available" over-used them on
/// big.LITTLE chips.
///
/// Clamp to `[2, 8]`: 8 is the P-core count on current Apple
/// Silicon flagships and roughly the elbow on x86 too (SMT
/// threads behave like E-cores here — they don't help SGEMM).
fn llama_threads_for_host() -> usize {
    let raw = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    raw.clamp(2, 8)
}

/// Label emitted in slot-load logs when the GPU path wasn't taken
/// (GPU context creation failed, or caller didn't request it).
/// Tells the operator which CPU math backend GGML is using.
fn embed_compute_backend_label() -> &'static str {
    // On Apple Silicon, GGML's CPU backend links Accelerate's SGEMM
    // — that's where real CPU-path throughput comes from. Other
    // platforms fall back to plain llama.cpp CPU kernels.
    #[cfg(target_os = "macos")]
    {
        "cpu+accelerate"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "cpu"
    }
}

/// Label emitted in slot-load logs when `wants_gpu` succeeded.
/// The specific backend (metal / rocm / vulkan) is still visible
/// in the nearby `ggml_*_init` llama.cpp output at startup.
fn gpu_backend_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "gpu+metal"
    }
    #[cfg(not(target_os = "macos"))]
    {
        // llama-cpp-2's `rocm` vs `vulkan` feature is selected at
        // the workspace level (see crates/sovereign-inference/Cargo.toml)
        // and not re-exposed as a rustc cfg here, so we just say "gpu".
        "gpu"
    }
}

// ─── EmbeddedLlamaCpp (triple-slot) ────────────────────────────

/// Configuration for loading the inference provider.
pub struct InferenceConfig {
    pub fast_model: PathBuf,
    pub primary_model: Option<PathBuf>,
    pub embed_model: Option<PathBuf>,
    pub context_size: u32,
    pub gpu_layers: Option<u32>,
    /// Model family for the fast slot. Drives thinking injection and
    /// sampling defaults. Defaults to `Unknown` (conservative no-thinking).
    pub fast_family: ModelFamily,
    /// Model family for the primary (thoughtful) slot.
    pub primary_family: ModelFamily,
    /// Model family for the embed slot. Must be `Qwen3Embedding` for
    /// last-token pooling and query instruction support.
    pub embed_family: ModelFamily,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            fast_model: PathBuf::from("models/fast.gguf"),
            primary_model: None,
            embed_model: None,
            context_size: 2048,
            gpu_layers: None,
            fast_family: ModelFamily::Unknown,
            primary_family: ModelFamily::Unknown,
            embed_family: ModelFamily::Unknown,
        }
    }
}

/// Triple-slot inference provider wrapping llama.cpp via FFI.
///
/// - **Fast slot**: always loaded, small model for classification and simple queries.
/// - **Primary slot**: loaded on-demand for deep reasoning, unloaded after idle timeout.
/// - **Embed slot**: optional dedicated embedding model for RAG / corpus indexing.
///   When absent, `embed()` returns a clear "no embed model configured" error so
///   callers can guide the user to configure one rather than silently producing
///   garbage vectors.
/// Default sequence-batch parameters for the FastShort companion slot.
/// Total `n_ctx` divided across `n_seq_max` gives 2048 tokens per
/// sequence, which fits 97.4% of wiki-tier2-500 Phase 1b calls. Set
/// `SOVEREIGN_FAST_SHORT_DISABLE=1` to skip building this slot (saves
/// ~750MB KV cache on 2B Q6_K models — useful on low-memory hosts).
pub(crate) const FAST_SHORT_N_SEQ_MAX: u32 = 8;
pub(crate) const FAST_SHORT_N_CTX: u32 = 16_384;
pub(crate) const FAST_SHORT_N_UBATCH: u32 = 2_048;

/// Time the coalescer waits between accepting the first request and
/// firing the batched decode. Concurrent enrichment-phase callers
/// (Phase 1b/3/5/6 dispatched via `buffer_unordered` or `try_join_all`)
/// arrive within microseconds of each other; 5ms is generous enough
/// to fill an 8-seq batch in practice while staying inside the OICP
/// v0.3 §2.2 "fast = hundreds of ms" TTFT envelope. A solo caller
/// pays at most this 5ms latency tax — then decodes immediately
/// (proven zero-overhead at K=1 in `bench_decode_batch.rs`).
const FAST_SHORT_COALESCE_WINDOW_MS: u64 = 5;

// ─── FastShort coalescer ───────────────────────────────────────
//
// Continuous-batched dispatch in front of the FastShort `LlamaContext`
// (`n_seq_max=8`). Concurrent callers each enqueue a request via
// `complete()` and `await` a `oneshot::Receiver`; a long-lived
// background task drains the queue, coalesces up to `n_seq_max` jobs
// inside `FAST_SHORT_COALESCE_WINDOW_MS`, then dispatches one
// `generate_sync_batched` call. The bench (`bench_decode_batch.rs`)
// proves the per-call wall-clock at K=8 lands at ~46% of the K=1
// wall-clock for the 900-tok/128-tok shape — a 2.1× speedup which
// projects to ~15min savings per wiki-tier2-500 Phase 1b pass.

struct CoalescerJob {
    request: CompletionRequest,
    response: tokio::sync::oneshot::Sender<Result<CompletionResponse>>,
}

struct FastShortCoalescer {
    enqueue: tokio::sync::mpsc::UnboundedSender<CoalescerJob>,
}

impl FastShortCoalescer {
    /// Spawn the long-lived drainer task and return a handle that
    /// callers use to enqueue requests via `complete`. The slot is
    /// `Arc`-shared with the caller so the existing `Arc::strong_count`
    /// idle-eviction logic keeps working unchanged.
    fn spawn(slot: Arc<ModelSlot>, quirks: ModelQuirks, n_seq_max: usize) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CoalescerJob>();
        tokio::spawn(async move {
            loop {
                let Some(first) = rx.recv().await else {
                    tracing::debug!(
                        slot = "fast_short",
                        "coalescer drainer exiting (channel closed)"
                    );
                    return;
                };
                let mut batch = vec![first];

                // Coalesce window: try to fill the batch within the
                // latency budget. A solo caller (no concurrent demand)
                // exits this loop immediately on the first timeout.
                let deadline = tokio::time::Instant::now()
                    + tokio::time::Duration::from_millis(FAST_SHORT_COALESCE_WINDOW_MS);
                while batch.len() < n_seq_max {
                    let remaining = deadline
                        .saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match tokio::time::timeout(remaining, rx.recv()).await {
                        Ok(Some(job)) => batch.push(job),
                        Ok(None) => break, // channel closed mid-coalesce
                        Err(_) => break,   // timeout → fire what we have
                    }
                }

                Self::dispatch_batch(Arc::clone(&slot), quirks.clone(), batch).await;
            }
        });
        Self { enqueue: tx }
    }

    /// Acquire the slot's inflight permit, then run
    /// `generate_sync_batched` against the collected jobs on a
    /// blocking thread. Each job's `oneshot::Sender` resolves with
    /// its individual `(text, tokens)` slice.
    async fn dispatch_batch(
        slot: Arc<ModelSlot>,
        quirks: ModelQuirks,
        batch: Vec<CoalescerJob>,
    ) {
        let permit = match slot.inflight.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                for job in batch {
                    let _ = job
                        .response
                        .send(Err(Error::Inference(
                            "FastShort slot permit closed".into(),
                        )));
                }
                return;
            }
        };
        let batch_size = batch.len();
        let join = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let start = Instant::now();
            slot.last_used
                .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            let mut ctx_lock = slot.context.blocking_lock();

            let model_id = slot.model_id.clone();
            let requests_refs: Vec<&CompletionRequest> =
                batch.iter().map(|j| &j.request).collect();

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ModelSlot::generate_sync_batched(
                    &slot.model,
                    &model_id,
                    &mut ctx_lock.ctx,
                    &requests_refs,
                    &quirks,
                )
            }));
            drop(requests_refs);

            match result {
                Ok(Ok(per_seq_results)) => {
                    let latency_ms = start.elapsed().as_millis() as u64;
                    tracing::debug!(
                        slot = "fast_short",
                        batch_size,
                        latency_ms,
                        "FastShort batched inference complete"
                    );
                    for (job, (text, prompt_tokens, completion_tokens)) in
                        batch.into_iter().zip(per_seq_results)
                    {
                        let resp = CompletionResponse {
                            text,
                            tokens_used: prompt_tokens + completion_tokens,
                            prompt_tokens,
                            model_id: model_id.clone(),
                            latency_ms,
                            oicp_meta: None,
                        };
                        let _ = job.response.send(Ok(resp));
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        slot = "fast_short",
                        error = %e,
                        batch_size,
                        "FastShort batched inference returned error — failing all batch members"
                    );
                    let msg = e.to_string();
                    for job in batch {
                        let _ = job
                            .response
                            .send(Err(Error::Inference(msg.clone())));
                    }
                }
                Err(_) => {
                    tracing::error!(
                        slot = "fast_short",
                        batch_size,
                        "FastShort batched inference panicked — likely context overflow"
                    );
                    for job in batch {
                        let _ = job.response.send(Err(Error::Inference(
                            "FastShort batched inference panicked — \
                             a batch member likely exceeded the per-seq context window"
                                .to_string(),
                        )));
                    }
                }
            }
        });
        if let Err(e) = join.await {
            tracing::error!(error = %e, "FastShort dispatch task join failed");
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.enqueue
            .send(CoalescerJob {
                request,
                response: tx,
            })
            .map_err(|_| {
                Error::Inference("FastShort coalescer is shut down".into())
            })?;
        rx.await.map_err(|_| {
            Error::Inference("FastShort coalescer dropped response".into())
        })?
    }
}

pub struct EmbeddedLlamaCpp {
    #[allow(dead_code)]
    backend: Arc<LlamaBackend>,
    fast: Arc<ModelSlot>,
    /// Continuous-batched companion to `fast`, sharing the same loaded
    /// `LlamaModel` via `Arc` but standing up its own `LlamaContext`
    /// with `n_seq_max=8`. Short-call enrichment phases (Phase 1b
    /// coverage, Phase 3 cluster naming, Phase 5/6) route here when
    /// the request's OICP `max_output_tokens` is small enough to fit
    /// the FastShort claim's hard gate (per OICP-v0.3 §2.4).
    ///
    /// `None` when:
    ///   - `SOVEREIGN_FAST_SHORT_DISABLE=1` opts out (memory escape
    ///     hatch),
    ///   - the second `new_context` call failed (logged at warn-level;
    ///     the daemon stays operational by routing all callers to
    ///     `fast` as before).
    ///
    /// The coalescer routes solo-vs-batched dispatch (see
    /// `fast_short_coalescer`). Held as `Arc` so the inflight
    /// semaphore + KV cache stay shared across in-flight requests.
    fast_short: Option<Arc<ModelSlot>>,
    /// Continuous-batching dispatcher in front of `fast_short`. Receives
    /// `complete` requests, drains them in batches of up to
    /// `n_seq_max` jobs inside a 5ms coalesce window, and delivers each
    /// caller its own `CompletionResponse`. `None` whenever
    /// `fast_short` is `None` — the two fields are constructed and
    /// discarded as a unit.
    fast_short_coalescer: Option<Arc<FastShortCoalescer>>,
    /// The lazy chat slot. When `code_path` is set, this slot
    /// hot-swaps between the Main responder and the Code specialist
    /// based on the incoming request's `capability_hint`. Keeping
    /// one on-demand slot (instead of two held-warm slots) trades
    /// a ~5–30s reload on hint-switch for a smaller memory
    /// footprint on mid-range hardware — a fair default for solo
    /// users. Peers running a collective can dedicate a separate
    /// code node; the mesh scheduler will route `code`-hinted
    /// requests there before the local hot-swap kicks in.
    primary: Arc<Mutex<Option<ModelSlot>>>,
    primary_path: Option<PathBuf>,
    primary_ctx_size: u32,
    gpu_layers: u32,
    primary_backend: Arc<LlamaBackend>,
    last_primary_use: Arc<Mutex<Option<Instant>>>,
    /// PR-E2: Path whose GGUF currently occupies the lazy slot.
    /// `None` → slot unloaded. Used to decide whether an incoming
    /// request requires a hot-swap (different path than the one
    /// already loaded) vs. can reuse the resident model.
    primary_loaded_path: Arc<Mutex<Option<PathBuf>>>,
    /// PR-E2: Optional Code specialist GGUF. When set, a request
    /// whose OICP envelope carries `capability_hint = "code"` is
    /// served from this path (loaded into the lazy slot, swapping
    /// out the Main responder if needed). `None` = no code slot,
    /// all substantive work goes to the Main responder.
    code_path: Option<PathBuf>,
    /// Quirks to apply when the Code specialist is loaded.
    /// Different from `primary_quirks` because code models can
    /// have different chat templates / thinking control than
    /// general models (e.g., DeepSeek Coder vs Qwen3.5).
    code_quirks: ModelQuirks,
    embed_slot: Option<Arc<EmbedSlot>>,
    /// Optional cross-encoder reranker slot. Behind a Mutex<Option<…>>
    /// rather than `Option<Arc<…>>` so it can be installed lazily
    /// after the inference provider is constructed — the daemon
    /// reads its path from `models.toml` or
    /// `SOVEREIGN_RERANK_MODEL_PATH` and calls
    /// `install_rerank_slot()`. Inference reads via a brief lock,
    /// clones the inner Arc, and releases.
    rerank_slot: std::sync::Mutex<Option<Arc<RerankSlot>>>,
    hardware: HardwareProfile,
    /// Quirks for the fast slot — controls thinking injection and sampling defaults.
    fast_quirks: ModelQuirks,
    /// Quirks for the primary (thoughtful) slot.
    primary_quirks: ModelQuirks,

    /// Operator-declared additional chat slots. The map itself is
    /// behind an `RwLock` so the runtime `/internal/models/load`
    /// + `/internal/models/unload` endpoints can mutate the lineup
    /// without daemon restart. Inference (the hot path) takes a
    /// read lock, clones the `Arc<ModelSlot>` for the matched slot,
    /// and releases — so concurrent mutations don't block in-flight
    /// requests, and dropped slots stay alive until their last
    /// in-flight request finishes (RAII via `Arc`).
    extras: Arc<std::sync::RwLock<ExtrasState>>,
    /// Inflight gate for the lazy slot (Primary or Code). Single
    /// permit. The fast / extras slots have their own per-slot
    /// `inflight` semaphores on `ModelSlot`; the lazy path can't use
    /// those since the slot is `None` until first use, so the
    /// `EmbeddedLlamaCpp` owns a parallel semaphore that covers both
    /// hot-swap and generation. See `ModelSlot::inflight` for the
    /// rationale.
    lazy_inflight: Arc<tokio::sync::Semaphore>,
}

/// Internal state for the operator-declared extras lineup. Held
/// behind a single RwLock in `EmbeddedLlamaCpp.extras` so all three
/// fields stay in sync — a partial mutation (slot loaded but
/// `by_model_id` not updated) would route requests to the wrong
/// place. Keeping them in one struct guarantees lock acquisition
/// covers the whole record.
struct ExtrasState {
    /// slot_name → loaded chat slot. Each slot is held resident
    /// until the operator unloads it OR the LRU policy evicts it.
    slots: HashMap<String, Arc<ModelSlot>>,
    /// Index from advertised model_id (gguf file stem) →
    /// `slots` key. Lookup is the hot path for
    /// `select_slot_for_request`, so we keep it pre-resolved.
    by_model_id: HashMap<String, String>,
    /// Per-slot quirks. Keys mirror `slots`. All entries today
    /// carry `ModelFamily::Unknown` defaults — runtime adapts to
    /// the gguf header. Reserved as its own field so a future
    /// op-flag could pin a specific family per slot.
    quirks: HashMap<String, ModelQuirks>,
    /// Optional total-memory budget in bytes for the extras
    /// lineup. When `Some(b)`, a `load_extra` that would push
    /// `(sum of size_bytes) + new_size` above `b` triggers LRU
    /// eviction of the coldest non-busy slots. `None` (default)
    /// disables eviction. Wired from
    /// `SetupConfig::ModelsSection.max_extras_memory_gb`.
    budget_bytes: Option<u64>,
}

impl ExtrasState {
    fn new() -> Self {
        Self {
            slots: HashMap::new(),
            by_model_id: HashMap::new(),
            quirks: HashMap::new(),
            budget_bytes: None,
        }
    }
}

/// Which slot to dispatch a request to. Computed from
/// `(request.preferred_speed, request.oicp.capability_hint,
/// request.model_id, configured slots)` — see
/// [`EmbeddedLlamaCpp::select_slot_for_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum SlotTarget {
    Fast,
    /// Continuous-batched companion to the Fast slot. Selected for
    /// short-call enrichment phases (Phase 1b/3/5/6) and any other
    /// request whose `max_tokens` fits the FastShort claim's hard
    /// gate. Routed through `fast_short_coalescer` which collects
    /// concurrent arrivals into a single `generate_sync_batched`
    /// call against the n_seq_max=8 context.
    FastShort,
    Primary,
    Code,
    /// Operator-declared additional eagerly-loaded chat slot. The
    /// inner string is the key in `EmbeddedLlamaCpp.extras` —
    /// looked up via `extras_by_model_id` from
    /// `request.model_id`. See `pick_slot` for the precedence rules.
    Extra(String),
}

/// Pure slot-selection policy extracted from
/// [`EmbeddedLlamaCpp::select_slot_for_request`] so the rules can
/// be unit-tested without loading real GGUF weights. Inputs are
/// intentionally scalar: the request + "is the primary configured?"
/// + "is the code slot configured?" + an optional pre-resolved
/// extras key (the model_id → extras-slot lookup). The hot-swap
/// logic in `complete` / `complete_stream` depends on this decision
/// being deterministic for a fixed (request, slot-configuration)
/// pair.
///
/// Precedence (in order):
/// 1. **Extras match on `request.model_id`.** Operator-declared
///    routing wins over heuristic Speed/code-hint selection. This
///    is what makes per-phase model recruitment work end-to-end.
/// 2. Code-hint + Code slot configured.
/// 3. Speed-based: Fast → fast slot; Medium/Slow → primary if
///    configured else fast.
fn pick_slot(
    request: &CompletionRequest,
    has_primary: bool,
    has_code: bool,
    has_fast_short: bool,
    extras_match: Option<String>,
) -> SlotTarget {
    if let Some(name) = extras_match {
        return SlotTarget::Extra(name);
    }
    let wants_code = request
        .oicp
        .as_ref()
        .and_then(|o| o.capability_hint.as_ref())
        .map(|h| h.as_str() == sovereign_core::oicp::CapabilityHint::CODE)
        .unwrap_or(false);
    if wants_code && has_code {
        return SlotTarget::Code;
    }

    // FastShort routing: when the FastShort companion slot is built
    // and the request fits its envelope (small output budget, fast
    // latency), route there. The OICP scheduler at the daemon edge
    // already does the analogous selection between FastShort /
    // FastLong claims; this is the local-dispatch mirror so a
    // sovereign daemon serving its own request can also benefit
    // without the round-trip through the mesh router.
    //
    // Threshold is the FastShort claim's `max_output` (512 — see
    // `synthesize_slot_claims`). Phase 1b/3/5/6 composers set their
    // `max_output_tokens` to ≤512 (some at 256 / 1024 — note: 1024
    // would overflow this gate and fall through to Fast, which is
    // correct: those phases prefer per-call window over batching).
    let max_out = request.max_tokens.unwrap_or(usize::MAX);
    let fits_fast_short = max_out <= 512
        && request.preferred_speed == Speed::Fast;
    if has_fast_short && fits_fast_short {
        return SlotTarget::FastShort;
    }

    match request.preferred_speed {
        Speed::Fast => SlotTarget::Fast,
        Speed::Medium | Speed::Slow => {
            if has_primary {
                SlotTarget::Primary
            } else {
                SlotTarget::Fast
            }
        }
    }
}

impl EmbeddedLlamaCpp {
    /// Load with a single model (used for both fast and primary slots).
    /// This is the simple path for development and small deployments.
    pub fn load(model_path: &Path) -> Result<Self> {
        Self::load_dual(
            model_path,
            None, // No separate primary model.
            2048,
            None,
        )
    }

    /// Load with separate fast and primary models. No embed slot.
    pub fn load_dual(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        Self::load_full(
            fast_model_path,
            primary_model_path,
            None,
            context_size,
            gpu_layers,
        )
    }

    /// Load with separate fast, primary, and embed models. The embed
    /// slot is optional — when absent, `embed()` returns a clear
    /// "no embedding model configured" error.
    pub fn load_full(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        embed_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        Self::load_full_with_families(
            fast_model_path,
            primary_model_path,
            embed_model_path,
            None,
            context_size,
            gpu_layers,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
        )
    }

    /// Load with separate fast, primary, embed, and optional code
    /// models, specifying the model family for each slot. The family
    /// drives thinking injection, sampling defaults, and
    /// embedding pooling/instruction behaviour.
    ///
    /// The `code` slot is a PR-E2 addition: when configured, code-
    /// hinted inference requests are routed to the Code specialist
    /// GGUF (via hot-swap into the shared lazy slot) instead of the
    /// Main responder. Passing `None` preserves the pre-E2 two-slot
    /// behaviour — all substantive work goes to the primary.
    #[allow(clippy::too_many_arguments)]
    pub fn load_full_with_families(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        embed_model_path: Option<&Path>,
        code_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
        fast_family: ModelFamily,
        primary_family: ModelFamily,
        embed_family: ModelFamily,
        code_family: ModelFamily,
    ) -> Result<Self> {
        let hardware = HardwareProfile::detect();
        let n_gpu_layers = gpu_layers.unwrap_or(hardware.recommended_gpu_layers);

        let fast_quirks = fast_family.default_quirks();
        let primary_quirks = primary_family.default_quirks();
        let code_quirks = code_family.default_quirks();
        let embed_quirks = embed_family.default_quirks().embed;

        let mut backend = LlamaBackend::init()
            .map_err(|e| Error::Inference(format!("Failed to init llama backend: {e}")))?;
        // Suppress llama.cpp/ggml C-level stderr noise by default. Set
        // SOVEREIGN_LLAMA_LOGS=1 when diagnosing a model-load failure
        // (e.g. an unsupported quant or arch returning a "null result")
        // so the underlying libllama error reaches daemon.err.
        if std::env::var("SOVEREIGN_LLAMA_LOGS").ok().as_deref() != Some("1") {
            backend.void_logs();
        }
        let backend = Arc::new(backend);

        tracing::info!(slot = "fast", family = ?fast_family, "loading slot");
        let fast = Arc::new(ModelSlot::load(
            &backend,
            fast_model_path,
            context_size,
            n_gpu_layers,
        )?);
        tracing::info!(slot = "fast", family = ?fast_family, "slot loaded");

        // FastShort companion: a second LlamaContext on the same Fast
        // model with n_seq_max=8 for continuous-batched short calls.
        // Sharing weights via Arc<LlamaModel> means the only marginal
        // cost is ~750MB KV cache (Strix Halo unified memory makes
        // this a non-issue; tighter hosts can opt out). Failure is
        // logged but non-fatal — the daemon still serves all callers
        // through `fast` as before.
        let fast_short_disabled = std::env::var("SOVEREIGN_FAST_SHORT_DISABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let (fast_short, fast_short_coalescer) = if fast_short_disabled {
            tracing::info!(
                slot = "fast_short",
                "skipped (SOVEREIGN_FAST_SHORT_DISABLE=1)"
            );
            (None, None)
        } else {
            match ModelSlot::from_existing_model(
                &backend,
                Arc::clone(&fast.model),
                fast.model_id.clone(),
                fast.size_bytes,
                FAST_SHORT_N_CTX,
                FAST_SHORT_N_SEQ_MAX,
                FAST_SHORT_N_UBATCH,
            ) {
                Ok(slot) => {
                    let slot = Arc::new(slot);
                    let coalescer = Arc::new(FastShortCoalescer::spawn(
                        Arc::clone(&slot),
                        fast_quirks.clone(),
                        FAST_SHORT_N_SEQ_MAX as usize,
                    ));
                    (Some(slot), Some(coalescer))
                }
                Err(e) => {
                    tracing::warn!(
                        slot = "fast_short",
                        error = %e,
                        "failed to build continuous-batched companion; \
                         all callers will route to `fast` as before"
                    );
                    (None, None)
                }
            }
        };

        // Embedding models are usually small (~100-500 MB) and we want
        // them resident any time corpus ingestion or search runs, so we
        // load eagerly rather than lazily. Failure to load is logged
        // but not fatal — the rest of the runtime keeps working and
        // `embed()` will return a clear error.
        let embed_slot = match embed_model_path {
            Some(path) => {
                tracing::info!(
                    slot = "embed",
                    path = %path.display(),
                    family = ?embed_family,
                    "loading slot"
                );
                // Embedding offload mirrors the chat slot: pass the
                // hardware-derived n_gpu_layers and let `EmbedSlot::load`
                // try-GPU-then-fallback. The old comment here said
                // "embedding workloads are not compute-bound enough to
                // benefit from GPU offloading" — measured to be wrong
                // on Strix Halo Vulkan (ingest was stuck at ~4 chunks/s
                // CPU-only; GPU embed is ~10× faster) and on Apple
                // Silicon Metal (33 vs 2.8 seq/sec, 12×). The Metal
                // GGML_ASSERT(buf_src) crash referenced in the old
                // comment was fixed in llama-cpp-2 0.1.141.
                match EmbedSlot::load(&backend, path, n_gpu_layers, embed_quirks) {
                    Ok(slot) => {
                        tracing::info!(slot = "embed", "slot loaded");
                        Some(Arc::new(slot))
                    }
                    Err(e) => {
                        tracing::warn!(slot = "embed", error = %e, "slot load failed — embedding unavailable");
                        None
                    }
                }
            }
            None => None,
        };

        if let Some(p) = code_model_path {
            tracing::info!(
                slot = "code",
                path = %p.display(),
                family = ?code_family,
                "code specialist configured — will load on first code-hinted request"
            );
        }

        Ok(Self {
            backend: Arc::clone(&backend),
            fast,
            fast_short,
            fast_short_coalescer,
            primary: Arc::new(Mutex::new(None)),
            primary_path: primary_model_path.map(|p| p.to_path_buf()),
            primary_ctx_size: context_size,
            gpu_layers: n_gpu_layers,
            primary_backend: backend,
            last_primary_use: Arc::new(Mutex::new(None)),
            primary_loaded_path: Arc::new(Mutex::new(None)),
            code_path: code_model_path.map(|p| p.to_path_buf()),
            code_quirks,
            embed_slot,
            rerank_slot: std::sync::Mutex::new(None),
            hardware,
            fast_quirks,
            primary_quirks,
            extras: Arc::new(std::sync::RwLock::new(ExtrasState::new())),
            lazy_inflight: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    /// Install operator-declared additional chat slots after
    /// construction. Each `(slot_name, gguf_path)` entry is
    /// eagerly loaded and held resident until the operator
    /// unloads it (via `unload_extra` or by replacing the lineup
    /// through another `install_extras` call).
    ///
    /// The advertised `model_id` (gguf file stem) is the routing
    /// key — when an inbound request carries `model_id` matching
    /// one of these stems, the slot picker returns
    /// [`SlotTarget::Extra`] before falling through to
    /// Speed-based routing.
    ///
    /// Loading is sequential (each call to `ModelSlot::load` opens
    /// the gguf and uploads weights). On success the slot becomes
    /// part of the routing table; on failure the slot is skipped
    /// with a `warn!` log and the remaining slots still load. This
    /// matches the embed-slot policy — partial slot inventories are
    /// preferable to a hard fail at boot, which would prevent the
    /// daemon from serving _any_ requests.
    ///
    /// **Concurrency**: holds a write lock on `extras` for the full
    /// load duration. In-flight requests on existing extras slots
    /// are unaffected — they hold a clone of the `Arc<ModelSlot>`
    /// and don't need the outer lock. New requests during the load
    /// briefly block on the read lock acquisition, then proceed
    /// against the post-load lineup.
    ///
    /// Re-calling this method **replaces** the previous extras
    /// lineup. Slots dropped from the new lineup release their
    /// `Arc<ModelSlot>` once their last in-flight request
    /// completes (RAII). Used by the daemon at startup with the
    /// `[models.extra]` config table.
    pub fn install_extras(
        &self,
        extras: BTreeMap<String, PathBuf>,
        context_size: u32,
    ) -> Result<()> {
        // Preserve any operator-set budget across reloads. The
        // budget is set via `set_extras_memory_budget` and lives
        // alongside the slot map; reinstalling slots shouldn't
        // implicitly reset eviction behaviour.
        let preserved_budget = self
            .extras
            .read()
            .map(|g| g.budget_bytes)
            .unwrap_or(None);
        let default_quirks = ModelFamily::Unknown.default_quirks();
        let mut state = ExtrasState::new();
        state.budget_bytes = preserved_budget;
        for (slot_name, path) in extras {
            tracing::info!(
                slot = %slot_name,
                path = %path.display(),
                "loading extras slot"
            );
            match ModelSlot::load(
                &self.primary_backend,
                &path,
                context_size,
                self.gpu_layers,
            ) {
                Ok(slot) => {
                    let model_id = slot.model_id.clone();
                    let arc = Arc::new(slot);
                    state.slots.insert(slot_name.clone(), Arc::clone(&arc));
                    state.by_model_id.insert(model_id.clone(), slot_name.clone());
                    state.quirks.insert(slot_name.clone(), default_quirks.clone());
                    tracing::info!(
                        slot = %slot_name,
                        model_id = %model_id,
                        "extras slot loaded"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        slot = %slot_name,
                        path = %path.display(),
                        error = %e,
                        "extras slot load failed — skipping; remaining extras still load"
                    );
                }
            }
        }
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        *guard = state;
        Ok(())
    }

    /// Install (or replace) the cross-encoder reranker slot at
    /// runtime. Loads the GGUF eagerly so the first rerank call
    /// doesn't pay the model-load latency. Family defaults to
    /// `ModelFamily::Reranker` quirks (8192 ctx, max_batch 50);
    /// pass `Some(_)` to override per-deployment.
    ///
    /// Returns the model_id (file stem) of the installed reranker,
    /// matching the pattern other slot installers use.
    ///
    /// Replacing an in-use reranker is safe: the old `Arc<RerankSlot>`
    /// drops when the last in-flight rerank call finishes (RAII via
    /// Arc cloning in the `rerank_batch` hot path).
    pub fn install_rerank_slot(
        &self,
        path: PathBuf,
        family: ModelFamily,
    ) -> Result<String> {
        let rerank_quirks = family.default_quirks().rerank;
        tracing::info!(
            slot = "rerank",
            path = %path.display(),
            family = ?family,
            "installing rerank slot"
        );
        let slot = RerankSlot::load(
            &self.primary_backend,
            &path,
            self.gpu_layers,
            rerank_quirks,
        )?;
        let model_id = slot.model_id.clone();
        let arc = Arc::new(slot);
        let mut guard = self
            .rerank_slot
            .lock()
            .map_err(|e| Error::Inference(format!("rerank slot lock poisoned: {e}")))?;
        *guard = Some(arc);
        tracing::info!(slot = "rerank", model_id = %model_id, "rerank slot installed");
        Ok(model_id)
    }

    /// Returns the model_id of the currently installed reranker, if any.
    pub fn rerank_model_id(&self) -> Option<String> {
        self.rerank_slot
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.model_id.clone()))
    }

    /// Add (or replace) a single extras slot at runtime. Returns
    /// the `model_id` (gguf file stem) advertised for this slot,
    /// which is what callers send in `request.model` / `model_id`
    /// to land here.
    ///
    /// If a slot with `slot_name` already exists, its
    /// `Arc<ModelSlot>` is replaced and the `model_id → slot_name`
    /// index updated. The previous slot's Arc drops once the last
    /// in-flight request finishes; if no requests are in flight,
    /// the model unloads immediately. Use [`unload_extra`] to drop
    /// without replacement.
    ///
    /// LRU eviction is **not** applied — this entry point assumes
    /// the caller has already accounted for memory. For
    /// budget-aware loading, use [`load_extra_with_budget`].
    pub fn load_extra(
        &self,
        slot_name: String,
        path: PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.load_extra_with_budget(slot_name, path, context_size, None)
    }

    /// Budget-aware variant of [`load_extra`]. When `budget_bytes`
    /// is `Some(B)`, this method first peeks at the new gguf's
    /// on-disk size; if `(sum of currently-loaded extras' sizes) +
    /// (new size) > B`, it evicts cold (least-recently-used)
    /// slots in ascending `last_used` order until the new slot
    /// fits. Slots currently serving a request — detected via
    /// `Arc::strong_count` > 1 — are skipped to avoid yanking the
    /// rug from under in-flight inference. If after walking every
    /// non-busy slot the new load still doesn't fit, the call
    /// returns an error with the budget shortfall.
    ///
    /// `None` skips eviction entirely (matches
    /// [`load_extra`] historical behaviour).
    pub fn load_extra_with_budget(
        &self,
        slot_name: String,
        path: PathBuf,
        context_size: u32,
        budget_bytes: Option<u64>,
    ) -> Result<String> {
        tracing::info!(
            slot = %slot_name,
            path = %path.display(),
            "load_extra: preparing slot"
        );

        // The pre-load eviction check uses the gguf file size as an
        // estimate of the upcoming slot's footprint. After load, the
        // ModelSlot stores `LlamaModel::size()` instead — usually
        // very close, but the file-size estimate is what's available
        // BEFORE we've called `ModelSlot::load`, and that's the
        // gating moment for "should we make room first?". Pre-load
        // and post-load can diverge by a few percent; the budget is
        // a soft cap by design.
        let new_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        // LRU eviction pass under the write lock. Done BEFORE
        // ModelSlot::load so we don't briefly OOM by holding the
        // old + new slot's weights simultaneously. Eviction drops
        // the slot from the map; the actual model unload happens
        // when the last `Arc<ModelSlot>` reference releases (in-
        // flight requests still hold theirs and finish naturally).
        if let Some(budget) = budget_bytes {
            self.evict_extras_for_new_load(&slot_name, new_size, budget)?;
        }

        let slot = ModelSlot::load(
            &self.primary_backend,
            &path,
            context_size,
            self.gpu_layers,
        )?;
        let model_id = slot.model_id.clone();
        let arc = Arc::new(slot);

        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        // If this slot_name was previously bound to a different
        // model_id, drop the stale `by_model_id` entry first to
        // keep the index injective.
        if let Some(prev_arc) = guard.slots.remove(&slot_name) {
            let prev_model_id = &prev_arc.model_id;
            guard.by_model_id.remove(prev_model_id);
        }
        guard.slots.insert(slot_name.clone(), Arc::clone(&arc));
        guard.by_model_id.insert(model_id.clone(), slot_name.clone());
        guard
            .quirks
            .insert(slot_name.clone(), ModelFamily::Unknown.default_quirks());
        tracing::info!(slot = %slot_name, model_id = %model_id, "load_extra: slot loaded");
        Ok(model_id)
    }

    /// Walk the extras lineup in ascending `last_used` order,
    /// dropping cold slots until `(remaining sum) + new_size <=
    /// budget`. Skips any slot whose `Arc<ModelSlot>` strong count
    /// exceeds 1 (in-flight request) and any slot that matches
    /// `incoming_slot_name` (we'll replace it shortly anyway).
    /// Returns `Err` when not enough cold capacity is available
    /// to make room — caller surfaces the message verbatim.
    ///
    /// The pure selection step lives in [`pick_evictions`] for
    /// unit testability; this method handles the impure parts
    /// (snapshot under lock, mutate state, log evictions).
    fn evict_extras_for_new_load(
        &self,
        incoming_slot_name: &str,
        new_size: u64,
        budget_bytes: u64,
    ) -> Result<()> {
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        // Snapshot non-busy candidates and the current footprint
        // (excluding any slot we're about to replace) into pure
        // values so the selection step can be unit-tested.
        let current: u64 = guard
            .slots
            .iter()
            .filter(|(name, _)| name.as_str() != incoming_slot_name)
            .map(|(_, slot)| slot.size_bytes)
            .sum();
        let candidates: Vec<EvictionCandidate> = guard
            .slots
            .iter()
            .filter(|(name, _)| name.as_str() != incoming_slot_name)
            .filter_map(|(name, arc)| {
                if Arc::strong_count(arc) > 1 {
                    None // in-flight; preserve
                } else {
                    Some(EvictionCandidate {
                        slot_name: name.clone(),
                        last_used_ms: arc
                            .last_used
                            .load(std::sync::atomic::Ordering::Relaxed),
                        size_bytes: arc.size_bytes,
                    })
                }
            })
            .collect();

        let plan = pick_evictions(&candidates, current, new_size, budget_bytes);
        match plan {
            EvictionPlan::Fits => Ok(()),
            EvictionPlan::Evict(slot_names) => {
                let mut evicted = Vec::with_capacity(slot_names.len());
                for slot_name in slot_names {
                    if let Some(arc) = guard.slots.remove(&slot_name) {
                        let model_id = arc.model_id.clone();
                        guard.by_model_id.remove(&model_id);
                        guard.quirks.remove(&slot_name);
                        evicted.push((slot_name, model_id));
                    }
                }
                drop(guard);
                for (slot_name, model_id) in evicted {
                    tracing::info!(
                        slot = %slot_name,
                        model_id = %model_id,
                        reason = "lru_eviction",
                        "extras slot dropped to free memory for incoming load"
                    );
                }
                Ok(())
            }
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                tracing::warn!(
                    budget_gb = budget_bytes as f64 / (1u64 << 30) as f64,
                    cold_total_mb = cold_total / (1024 * 1024),
                    need_to_free_mb = need_to_free / (1024 * 1024),
                    "load_extra rejected: insufficient cold capacity"
                );
                Err(Error::Inference(format!(
                    "load_extra rejected: cannot free enough memory under \
                     max_extras_memory_gb budget. Cold (non-busy) extras \
                     total {} MB but need to free {} MB. Either raise the \
                     budget, unload an in-flight slot manually, or wait \
                     for current inference to drain.",
                    cold_total / (1024 * 1024),
                    need_to_free / (1024 * 1024),
                )))
            }
        }
    }
}

/// Eviction-candidate snapshot. Pulled out of the live RwLock so
/// the selection algorithm in [`pick_evictions`] is a pure
/// function over scalar inputs — testable without loading any
/// real GGUF.
#[derive(Debug, Clone)]
struct EvictionCandidate {
    slot_name: String,
    last_used_ms: u64,
    size_bytes: u64,
}

/// Outcome of [`pick_evictions`]. Three states map to three
/// caller actions: nothing to do, evict these names in order, or
/// fail loud because the cold lineup can't free enough.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EvictionPlan {
    /// Already within budget — caller proceeds without mutating.
    Fits,
    /// Drop these slot names in order. The slice is sorted by
    /// ascending `last_used` so the coldest slots go first.
    Evict(Vec<String>),
    /// Cold capacity isn't enough. Caller surfaces an error to
    /// the operator; busy slots are preserved.
    Insufficient {
        /// How many bytes the caller still needs to free after
        /// evicting every cold candidate.
        need_to_free: u64,
        /// Sum of `size_bytes` across the cold candidates. Useful
        /// for the operator-facing error message.
        cold_total: u64,
    },
}

/// Pure LRU selection. Given a list of non-busy candidates plus
/// the current footprint and incoming load size, returns which
/// slots to evict so the new load fits under `budget_bytes`.
/// Sorts candidates ascending by `last_used_ms` and consumes them
/// until `freed >= need_to_free`.
///
/// Keeping this pure (no `&self`, no lock) means the routing-
/// regression suite can lock-step LRU behaviour without depending
/// on a real llama.cpp load.
fn pick_evictions(
    candidates: &[EvictionCandidate],
    current_total: u64,
    incoming_size: u64,
    budget_bytes: u64,
) -> EvictionPlan {
    if current_total.saturating_add(incoming_size) <= budget_bytes {
        return EvictionPlan::Fits;
    }
    let need_to_free = current_total
        .saturating_add(incoming_size)
        .saturating_sub(budget_bytes);
    let mut sorted: Vec<&EvictionCandidate> = candidates.iter().collect();
    sorted.sort_by_key(|c| c.last_used_ms);
    let mut freed: u64 = 0;
    let mut to_evict: Vec<String> = Vec::new();
    for c in &sorted {
        if freed >= need_to_free {
            break;
        }
        freed = freed.saturating_add(c.size_bytes);
        to_evict.push(c.slot_name.clone());
    }
    if freed >= need_to_free {
        EvictionPlan::Evict(to_evict)
    } else {
        EvictionPlan::Insufficient {
            need_to_free,
            cold_total: sorted.iter().map(|c| c.size_bytes).sum(),
        }
    }
}

// Reopen the impl block so subsequent methods on EmbeddedLlamaCpp
// keep their existing namespace.
impl EmbeddedLlamaCpp {

    /// Drop an extras slot. Returns the `model_id` that was bound
    /// to that slot, or `None` if the slot wasn't loaded. The slot's
    /// `Arc<ModelSlot>` releases once any in-flight request on it
    /// completes — concurrent inference is not interrupted.
    pub fn unload_extra(&self, slot_name: &str) -> Result<Option<String>> {
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        let Some(arc) = guard.slots.remove(slot_name) else {
            return Ok(None);
        };
        let model_id = arc.model_id.clone();
        guard.by_model_id.remove(&model_id);
        guard.quirks.remove(slot_name);
        drop(guard);
        tracing::info!(slot = %slot_name, model_id = %model_id, "unload_extra: slot dropped");
        Ok(Some(model_id))
    }

    /// Install (or clear) the extras memory budget. `Some(bytes)`
    /// turns LRU eviction on; subsequent `load_extra` calls evict
    /// cold slots when adding the new slot would exceed the budget.
    /// `None` disables eviction (the historical default — slots are
    /// held until unloaded).
    ///
    /// The daemon calls this once at startup with
    /// `cfg.models.max_extras_memory_bytes()`. Future runtime
    /// adjustments (operator wants to tighten the budget without
    /// restart) re-call this entry point.
    pub fn set_extras_memory_budget(&self, budget_bytes: Option<u64>) -> Result<()> {
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        guard.budget_bytes = budget_bytes;
        match budget_bytes {
            Some(b) => tracing::info!(
                budget_gb = b as f64 / (1u64 << 30) as f64,
                "extras memory budget set; LRU eviction active"
            ),
            None => tracing::info!("extras memory budget cleared; eviction disabled"),
        }
        Ok(())
    }

    /// Read the currently-configured extras memory budget. `None`
    /// means eviction is disabled.
    pub fn extras_memory_budget(&self) -> Option<u64> {
        self.extras.read().ok().and_then(|g| g.budget_bytes)
    }

    /// Inspect the currently-loaded extras lineup. Returns
    /// `(slot_name, model_id)` pairs in deterministic
    /// (slot_name-sorted) order. Used by `/v1/models` registration
    /// + the `/internal/models/inventory` endpoint.
    pub fn extras_inventory(&self) -> Vec<(String, String)> {
        let guard = match self.extras.read() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(error = %e, "extras lock poisoned during inventory read");
                return Vec::new();
            }
        };
        let mut out: Vec<(String, String)> = guard
            .by_model_id
            .iter()
            .map(|(model_id, slot_name)| (slot_name.clone(), model_id.clone()))
            .collect();
        out.sort();
        out
    }

    /// True when a Code specialist GGUF has been configured.
    /// Exposed on the concrete type (tests + direct constructors);
    /// trait-level consumers use `InferenceProvider::code_model_id()`.
    pub fn has_code_slot(&self) -> bool {
        self.code_path.is_some()
    }

    /// Pick which slot should serve this request.
    ///
    /// Rules:
    /// - `request.model_id` matches an extras slot → `Extra(name)`.
    ///   Operator-declared routing wins over heuristics.
    /// - `code` hint + code slot configured → `Code` (hot-swaps
    ///   the lazy slot to load the code GGUF).
    /// - `Speed::Fast` → `Fast` (always loaded).
    /// - Everything else → `Primary` if a primary GGUF is
    ///   configured, otherwise `Fast` (degraded fallback).
    fn select_slot_for_request(&self, request: &CompletionRequest) -> SlotTarget {
        let extras_match = request.model_id.as_deref().and_then(|mid| {
            // Hot-path read: clone the matched slot_name out of the
            // index and release the lock immediately. Concurrent
            // load/unload mutators block briefly but inference dispatch
            // never holds the lock across the actual generate_sync.
            let guard = self.extras.read().ok()?;
            guard.by_model_id.get(mid).cloned()
        });

        // Named-slot match. The fast / primary / code slots are
        // configured at startup, so they don't appear in the extras
        // index — but a client can still address them by model_id
        // (the GGUF file stem) via the OpenAI `model` field. Without
        // this check, such a request would fall through to OICP-based
        // slot picking, which defaults to `Slow → Primary` and
        // silently routes the user to a different model than they
        // asked for.
        if extras_match.is_none() {
            if let Some(mid) = request.model_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if mid == self.fast.model_id {
                    return SlotTarget::Fast;
                }
                if let Some(pid) = self
                    .primary_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .and_then(|s| s.to_str())
                {
                    if mid == pid {
                        return SlotTarget::Primary;
                    }
                }
                if let Some(cid) = self
                    .code_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .and_then(|s| s.to_str())
                {
                    if mid == cid {
                        return SlotTarget::Code;
                    }
                }
            }
        }

        pick_slot(
            request,
            self.primary_path.is_some(),
            self.code_path.is_some(),
            self.fast_short.is_some(),
            extras_match,
        )
    }

    /// Resolve a slot_name in `extras` to its `(Arc<ModelSlot>,
    /// ModelQuirks)` pair, cloning out of the lock so the dispatch
    /// path doesn't hold the read lock across inference. Returns
    /// `None` if the slot was unloaded between
    /// `select_slot_for_request` and dispatch (operator unloaded
    /// mid-flight) — caller surfaces a clear error.
    fn extras_lookup(&self, slot_name: &str) -> Option<(Arc<ModelSlot>, ModelQuirks)> {
        let guard = self.extras.read().ok()?;
        let slot = guard.slots.get(slot_name)?.clone();
        let quirks = guard
            .quirks
            .get(slot_name)
            .cloned()
            .unwrap_or_else(|| ModelFamily::Unknown.default_quirks());
        Some((slot, quirks))
    }

    /// Returns true if an embedding model is loaded and ready.
    pub fn has_embed_slot(&self) -> bool {
        self.embed_slot.is_some()
    }

    /// Start a background task that unloads cold extras slots
    /// once they pass the configured idle threshold.
    ///
    /// Polling cadence: every 10 seconds. A slot is dropped when:
    /// - its `last_used` timestamp is older than `idle_secs` ago, AND
    /// - its `Arc<ModelSlot>` strong count is exactly 1 (no
    ///   in-flight request is holding it).
    ///
    /// `idle_secs == 0` is "disabled" — the task isn't spawned.
    /// This keeps the historical "extras stay loaded forever"
    /// default for operators who haven't opted in. Set
    /// `[daemon].extras_idle_secs = 1800` (30 min) for a balance
    /// between reload tax (full reload on next use) and VRAM
    /// reclaim on a shared host.
    ///
    /// Independent from `start_idle_monitor` (which manages the
    /// lazy primary slot) — extras and primary have different
    /// load semantics so they need separate idle policies.
    pub fn start_extras_idle_monitor(self: &Arc<Self>, idle_secs: u64) {
        if idle_secs == 0 {
            // Disabled — don't spawn the task.
            tracing::debug!("extras idle monitor disabled (idle_secs=0)");
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tracing::info!(idle_secs, "extras idle monitor started");
            let threshold_ms = idle_secs.saturating_mul(1000);
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                let now = now_millis();
                // Snapshot stale slot names under read lock, then
                // call unload_extra (which takes the write lock)
                // outside the read scope to avoid lock-upgrade
                // deadlocks.
                let stale_names: Vec<String> = {
                    let guard = match this.extras.read() {
                        Ok(g) => g,
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "extras lock poisoned in idle monitor — exiting"
                            );
                            return;
                        }
                    };
                    guard
                        .slots
                        .iter()
                        .filter_map(|(name, arc)| {
                            // strong_count == 1 means only the map
                            // holds this slot — safe to drop.
                            if Arc::strong_count(arc) > 1 {
                                return None;
                            }
                            let last_used = arc
                                .last_used
                                .load(std::sync::atomic::Ordering::Relaxed);
                            if now.saturating_sub(last_used) >= threshold_ms {
                                Some(name.clone())
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                for name in stale_names {
                    match this.unload_extra(&name) {
                        Ok(Some(model_id)) => {
                            tracing::info!(
                                slot = %name,
                                model_id = %model_id,
                                idle_secs,
                                "extras: idle-unload"
                            );
                        }
                        Ok(None) => {
                            // Slot vanished between snapshot and
                            // unload — race with operator unload.
                            // Benign.
                        }
                        Err(e) => {
                            tracing::warn!(
                                slot = %name,
                                error = %e,
                                "extras idle-unload failed"
                            );
                        }
                    }
                }
            }
        });
    }

    /// Start a background task that unloads the primary model after idle timeout.
    pub fn start_idle_monitor(self: &Arc<Self>, timeout_secs: u64) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                let should_unload = {
                    let last_use = this.last_primary_use.lock().await;
                    match *last_use {
                        Some(t) => t.elapsed().as_secs() >= timeout_secs,
                        None => false,
                    }
                };

                if should_unload {
                    let mut primary = this.primary.lock().await;
                    if primary.is_some() {
                        // Report which model was in the slot at unload
                        // time — "primary" or "code" — so operators can
                        // correlate idle-unloads with hot-swap activity.
                        let slot_label = {
                            let loaded = this.primary_loaded_path.lock().await;
                            match loaded.as_ref() {
                                Some(p) if Some(p.as_path()) == this.code_path.as_deref() => "code",
                                _ => "primary",
                            }
                        };
                        tracing::info!(
                            slot = slot_label,
                            idle_secs = timeout_secs,
                            "unloading slot (idle timeout)"
                        );
                        *primary = None;
                        *this.primary_loaded_path.lock().await = None;
                        *this.last_primary_use.lock().await = None;
                    }
                }
            }
        });
    }

}

#[async_trait]
impl InferenceProvider for EmbeddedLlamaCpp {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let target = self.select_slot_for_request(request);
        let slot_name: String = match &target {
            SlotTarget::Fast => "fast".into(),
            SlotTarget::FastShort => "fast_short".into(),
            SlotTarget::Primary => "primary".into(),
            SlotTarget::Code => "code".into(),
            SlotTarget::Extra(name) => format!("extras:{name}"),
        };
        let prompt_chars = request.prompt.len();
        let system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0);

        tracing::debug!(
            slot = %slot_name,
            speed = ?request.preferred_speed,
            prompt_chars,
            system_chars,
            max_tokens = ?request.max_tokens,
            think_budget = ?request.think_budget,
            temperature = ?request.temperature,
            "inference.complete: call"
        );

        // Extras slot: eagerly loaded, no hot-swap. Run it through
        // the same generate_sync path as the fast slot, just with a
        // different ModelSlot reference.
        if let SlotTarget::Extra(name) = &target {
            let Some((slot, quirks)) = self.extras_lookup(name) else {
                // Slot was unloaded between selection and dispatch
                // (concurrent /internal/models/unload). Fail fast
                // with a clear error rather than silently routing
                // to a different slot.
                return Err(Error::Inference(format!(
                    "extras slot {name:?} was unloaded between selection \
                     and dispatch — retry the request, or pick a different \
                     model_id"
                )));
            };
            // Acquire the slot's inflight permit BEFORE spawn_blocking
            // so concurrent callers queue at the async layer, not in
            // the blocking pool. See ModelSlot.inflight docs.
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("slot permit closed: {e}")))?;
            let request = request.clone();
            let slot_label_owned = slot_name.clone();
            let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                // Stamp last_used at dispatch start. Using start
                // (rather than completion) makes the LRU eviction
                // policy treat a long-running request as still-warm
                // throughout — preferred behaviour for batch atlas
                // pipelines that fire 5-10min Phase 1 calls.
                slot.last_used
                    .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                let mut ctx_lock = slot.context.blocking_lock();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut ctx_lock.ctx, &request, &quirks)
                }));
                let (text, prompt_tokens, completion_tokens) = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::warn!(slot = %slot_label_owned, error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(
                            slot = %slot_label_owned,
                            "inference panicked — likely context overflow"
                        );
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history."
                                .to_string(),
                        ));
                    }
                };
                let latency_ms = start.elapsed().as_millis() as u64;
                Ok(CompletionResponse {
                    text,
                    tokens_used: prompt_tokens + completion_tokens,
                    prompt_tokens,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                })
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;
            if let Ok(ref resp) = result {
                tracing::info!(
                    slot = %slot_name,
                    model = %resp.model_id,
                    latency_ms = resp.latency_ms,
                    tokens_used = resp.tokens_used,
                    response_chars = resp.text.len(),
                    "inference.complete: done"
                );
            }
            return result;
        }

        // FastShort: continuous-batched dispatch via the coalescer.
        // The coalescer batches concurrent arrivals into a single
        // `generate_sync_batched` call against the n_seq_max=8 ctx;
        // a solo caller pays at most a 5ms coalesce-window wait then
        // decodes immediately. `pick_slot` only selects FastShort
        // when both `fast_short` slot and `fast_short_coalescer` are
        // present, so the unwraps below match construction in
        // `load_full_with_families`.
        if matches!(target, SlotTarget::FastShort) {
            let coalescer = self
                .fast_short_coalescer
                .as_ref()
                .expect("FastShort selected without coalescer present")
                .clone();
            let result = coalescer.complete(request.clone()).await;
            if let Ok(ref resp) = result {
                tracing::info!(
                    slot = "fast_short",
                    model = %resp.model_id,
                    latency_ms = resp.latency_ms,
                    tokens_used = resp.tokens_used,
                    response_chars = resp.text.len(),
                    "inference.complete: done"
                );
            }
            return result;
        }

        // Primary + Code share the lazy slot (hot-swap). Fast uses
        // the always-resident slot. Pick path + quirks up front so
        // the blocking task has a single load-or-reuse code path.
        let lazy_target = match target {
            SlotTarget::Fast => None,
            SlotTarget::FastShort => unreachable!("handled above"),
            SlotTarget::Primary => Some((
                self.primary_path.clone().unwrap(),
                self.primary_quirks.clone(),
                "primary",
            )),
            SlotTarget::Code => Some((
                self.code_path.clone().unwrap(),
                self.code_quirks.clone(),
                "code",
            )),
            SlotTarget::Extra(_) => unreachable!("handled above"),
        };

        if let Some((target_path, quirks, slot_label)) = lazy_target {
            // Throttle lazy-slot dispatch to 1 inflight at the async
            // layer; otherwise concurrent callers stack up blocking
            // threads waiting on `primary.blocking_lock()`. See the
            // `lazy_inflight` field doc.
            let _permit = self
                .lazy_inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let loaded_path = Arc::clone(&self.primary_loaded_path);
            let request = request.clone();

            let result: Result<(CompletionResponse, &'static str)> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();

                // Hot-swap check: if the lazy slot is holding a
                // different model than we need, unload + reload.
                let mut primary = primary_lock.blocking_lock();
                let mut loaded = loaded_path.blocking_lock();
                let needs_swap = loaded.as_deref() != Some(target_path.as_path());
                if needs_swap {
                    if primary.is_some() {
                        tracing::info!(
                            slot = slot_label,
                            from = ?loaded.as_ref().map(|p| p.display().to_string()),
                            to = %target_path.display(),
                            "hot-swapping lazy slot"
                        );
                    } else {
                        tracing::info!(
                            slot = slot_label,
                            path = %target_path.display(),
                            "loading lazy slot (first use)"
                        );
                    }
                    *primary = None;
                    let s = ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers)?;
                    *primary = Some(s);
                    *loaded = Some(target_path.clone());
                }
                drop(loaded);

                let slot = primary.as_ref().unwrap();
                let model_id = slot.model_id.clone();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut ctx_lock.ctx, &request, &quirks)
                }));

                let (text, prompt_tokens, completion_tokens) = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::warn!(slot = slot_label, error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(slot = slot_label, "inference panicked — likely context overflow");
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;

                *last_use.blocking_lock() = Some(Instant::now());

                Ok((CompletionResponse {
                    text,
                    tokens_used: prompt_tokens + completion_tokens,
                    prompt_tokens,
                    // `slot` may have been dropped on the fresh-ctx
                    // path; use the model_id cloned before the
                    // branch.
                    model_id: model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                }, slot_label))
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;

            match result {
                Ok((resp, slot_label)) => {
                    tracing::info!(
                        slot = slot_label,
                        model = %resp.model_id,
                        latency_ms = resp.latency_ms,
                        tokens_used = resp.tokens_used,
                        response_chars = resp.text.len(),
                        "inference.complete: done"
                    );
                    Ok(resp)
                }
                Err(e) => Err(e),
            }
        } else {
            // Use fast slot.
            let slot = Arc::clone(&self.fast);
            // Throttle fast-slot dispatch to 1 inflight at the async
            // layer. Same rationale as the extras path. KV enrichment
            // Phase 2 fans out 4+ parallel calls on this slot — without
            // the gate they all park blocking-pool threads and clone
            // 8 KB prompts; with it, only one proceeds and the rest
            // queue cheaply as async tasks.
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("fast slot permit closed: {e}")))?;
            let request = request.clone();
            let quirks = self.fast_quirks.clone();

            let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut ctx_lock.ctx, &request, &quirks)
                }));

                let (text, prompt_tokens, completion_tokens) = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::warn!(slot = "fast", error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(slot = "fast", "inference panicked — likely context overflow");
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;

                Ok(CompletionResponse {
                    text,
                    tokens_used: prompt_tokens + completion_tokens,
                    prompt_tokens,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                })
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;

            if let Ok(ref resp) = result {
                tracing::info!(
                    slot = "fast",
                    model = %resp.model_id,
                    latency_ms = resp.latency_ms,
                    tokens_used = resp.tokens_used,
                    response_chars = resp.text.len(),
                    "inference.complete: done"
                );
            }
            result
        }
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        // FastShort doesn't support streaming yet — the coalescer
        // returns whole responses, not token-by-token chunks. Coerce
        // streaming requests that would route to FastShort back onto
        // the Fast slot. Phase 1b/3/5/6 enrichment never streams (they
        // call `complete`), so this fallback only fires for callers
        // misconfigured to ask for streaming with a small max_output.
        let raw_target = self.select_slot_for_request(request);
        let target = if matches!(raw_target, SlotTarget::FastShort) {
            tracing::debug!(
                "FastShort selected for streaming request; falling back to Fast \
                 (FastShort doesn't expose a streaming API)"
            );
            SlotTarget::Fast
        } else {
            raw_target
        };
        let slot_name: String = match &target {
            SlotTarget::Fast => "fast".into(),
            SlotTarget::FastShort => "fast_short".into(),
            SlotTarget::Primary => "primary".into(),
            SlotTarget::Code => "code".into(),
            SlotTarget::Extra(name) => format!("extras:{name}"),
        };

        tracing::debug!(
            slot = %slot_name,
            speed = ?request.preferred_speed,
            prompt_chars = request.prompt.len(),
            system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0),
            max_tokens = ?request.max_tokens,
            "inference.complete_stream: call"
        );

        let request = request.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(32);

        // Extras slot: eagerly loaded, mirrors the fast-slot streaming
        // path with a different ModelSlot reference.
        if let SlotTarget::Extra(name) = &target {
            let Some((slot, quirks)) = self.extras_lookup(name) else {
                return Err(Error::Inference(format!(
                    "extras slot {name:?} was unloaded between selection \
                     and dispatch — retry the request"
                )));
            };
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("slot permit closed: {e}")))?;
            let slot_label_owned = slot_name.clone();
            tokio::task::spawn_blocking(move || {
                // Hold the permit for the streaming task's lifetime.
                let _permit = _permit;
                let start = Instant::now();
                slot.last_used
                    .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) = ModelSlot::generate_stream_sync(
                    &slot.model,
                    &slot.model_id,
                    &mut ctx_lock.ctx,
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = %slot_label_owned, error = %e, "stream error");
                    let _ = tx.blocking_send(Err(e));
                } else {
                    tracing::info!(
                        slot = %slot_label_owned,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream: done"
                    );
                }
            });
            return Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));
        }

        // Primary + Code share the lazy slot (hot-swap). Fast uses the
        // always-resident slot. Resolve path + quirks + slot-label up
        // front so the blocking task has one code path.
        let lazy_target: Option<(PathBuf, ModelQuirks, &'static str)> = match target {
            SlotTarget::Fast => None,
            // FastShort is handled before this match (or coerced to
            // Fast in the streaming path), so this arm never fires.
            SlotTarget::FastShort => unreachable!("FastShort handled above"),
            SlotTarget::Primary => Some((
                self.primary_path.clone().unwrap(),
                self.primary_quirks.clone(),
                "primary",
            )),
            SlotTarget::Code => Some((
                self.code_path.clone().unwrap(),
                self.code_quirks.clone(),
                "code",
            )),
            SlotTarget::Extra(_) => unreachable!("handled above"),
        };

        if let Some((target_path, quirks, slot_label)) = lazy_target {
            let _permit = self
                .lazy_inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let loaded_path = Arc::clone(&self.primary_loaded_path);

            tokio::task::spawn_blocking(move || {
                // Hold the permit for the streaming task's lifetime.
                let _permit = _permit;
                let start = Instant::now();

                // Hot-swap check: if the lazy slot is holding a different
                // model than we need, unload + reload. For streaming we
                // report the swap on the error channel only if the load
                // actually fails — the swap itself is internal plumbing.
                let mut primary = primary_lock.blocking_lock();
                let mut loaded = loaded_path.blocking_lock();
                let needs_swap = loaded.as_deref() != Some(target_path.as_path());
                if needs_swap {
                    if primary.is_some() {
                        tracing::info!(
                            slot = slot_label,
                            from = ?loaded.as_ref().map(|p| p.display().to_string()),
                            to = %target_path.display(),
                            "hot-swapping lazy slot (streaming)"
                        );
                    } else {
                        tracing::info!(
                            slot = slot_label,
                            path = %target_path.display(),
                            "loading lazy slot (first use, streaming)"
                        );
                    }
                    *primary = None;
                    *loaded = None;
                    match ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers) {
                        Ok(slot) => {
                            *primary = Some(slot);
                            *loaded = Some(target_path.clone());
                        }
                        Err(e) => {
                            tracing::error!(slot = slot_label, error = %e, "slot load failed");
                            let _ = tx.blocking_send(Err(e));
                            return;
                        }
                    }
                }
                drop(loaded);

                let slot = primary.as_ref().unwrap();
                let mut ctx_lock = slot.context.blocking_lock();
                *last_use.blocking_lock() = Some(Instant::now());
                if let Err(e) =
                    ModelSlot::generate_stream_sync(&slot.model, &slot.model_id, &mut ctx_lock.ctx, &request, &tx, &quirks, None)
                {
                    tracing::warn!(slot = slot_label, error = %e, "stream error");
                    let _ = tx.blocking_send(Err(e));
                } else {
                    tracing::info!(
                        slot = slot_label,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream: done"
                    );
                }
            });
        } else {
            let slot = Arc::clone(&self.fast);
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("fast slot permit closed: {e}")))?;
            let quirks = self.fast_quirks.clone();
            tokio::task::spawn_blocking(move || {
                // Hold the permit for the streaming task's lifetime.
                let _permit = _permit;
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) =
                    ModelSlot::generate_stream_sync(&slot.model, &slot.model_id, &mut ctx_lock.ctx, &request, &tx, &quirks, None)
                {
                    tracing::warn!(slot = "fast", error = %e, "stream error");
                    let _ = tx.blocking_send(Err(e));
                } else {
                    tracing::info!(
                        slot = "fast",
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream: done"
                    );
                }
            });
        }

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    /// Typed-frame override: yield [`StreamFrame`] with an accurate
    /// terminal [`FinishReason`]. Mirrors [`complete_stream`]'s
    /// slot-routing dance so per-slot mutex / hot-swap semantics stay
    /// identical — only the channel item type and the inner generate
    /// helper change.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        let target = self.select_slot_for_request(request);
        let slot_name: String = match &target {
            SlotTarget::Fast => "fast".into(),
            SlotTarget::FastShort => "fast_short".into(),
            SlotTarget::Primary => "primary".into(),
            SlotTarget::Code => "code".into(),
            SlotTarget::Extra(name) => format!("extras:{name}"),
        };

        tracing::debug!(
            slot = %slot_name,
            speed = ?request.preferred_speed,
            prompt_chars = request.prompt.len(),
            system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0),
            max_tokens = ?request.max_tokens,
            "inference.complete_stream_with_finish: call"
        );

        let request = request.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(32);

        if let SlotTarget::Extra(name) = &target {
            let Some((slot, quirks)) = self.extras_lookup(name) else {
                return Err(Error::Inference(format!(
                    "extras slot {name:?} was unloaded between selection \
                     and dispatch — retry the request"
                )));
            };
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("slot permit closed: {e}")))?;
            let slot_label_owned = slot_name.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();
                slot.last_used
                    .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) = ModelSlot::generate_stream_sync_with_finish(
                    &slot.model,
                    &slot.model_id,
                    &mut ctx_lock.ctx,
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = %slot_label_owned, error = %e, "stream error");
                    let _ = tx.blocking_send(StreamFrame::Finish {
                        reason: FinishReason::Error(format!("{e}")),
                        usage: None,
                    });
                } else {
                    tracing::info!(
                        slot = %slot_label_owned,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream_with_finish: done"
                    );
                }
            });
            return Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));
        }

        let lazy_target: Option<(PathBuf, ModelQuirks, &'static str)> = match target {
            SlotTarget::Fast => None,
            // FastShort is handled before this match (or coerced to
            // Fast in the streaming path), so this arm never fires.
            SlotTarget::FastShort => unreachable!("FastShort handled above"),
            SlotTarget::Primary => Some((
                self.primary_path.clone().unwrap(),
                self.primary_quirks.clone(),
                "primary",
            )),
            SlotTarget::Code => Some((
                self.code_path.clone().unwrap(),
                self.code_quirks.clone(),
                "code",
            )),
            SlotTarget::Extra(_) => unreachable!("handled above"),
        };

        if let Some((target_path, quirks, slot_label)) = lazy_target {
            let _permit = self
                .lazy_inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let loaded_path = Arc::clone(&self.primary_loaded_path);

            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();

                let mut primary = primary_lock.blocking_lock();
                let mut loaded = loaded_path.blocking_lock();
                let needs_swap = loaded.as_deref() != Some(target_path.as_path());
                if needs_swap {
                    if primary.is_some() {
                        tracing::info!(
                            slot = slot_label,
                            from = ?loaded.as_ref().map(|p| p.display().to_string()),
                            to = %target_path.display(),
                            "hot-swapping lazy slot (streaming, typed)"
                        );
                    } else {
                        tracing::info!(
                            slot = slot_label,
                            path = %target_path.display(),
                            "loading lazy slot (first use, streaming, typed)"
                        );
                    }
                    *primary = None;
                    *loaded = None;
                    match ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers) {
                        Ok(slot) => {
                            *primary = Some(slot);
                            *loaded = Some(target_path.clone());
                        }
                        Err(e) => {
                            tracing::error!(slot = slot_label, error = %e, "slot load failed");
                            let _ = tx.blocking_send(StreamFrame::Finish {
                                reason: FinishReason::Error(format!("{e}")),
                                usage: None,
                            });
                            return;
                        }
                    }
                }
                drop(loaded);

                let slot = primary.as_ref().unwrap();
                let mut ctx_lock = slot.context.blocking_lock();
                *last_use.blocking_lock() = Some(Instant::now());
                if let Err(e) = ModelSlot::generate_stream_sync_with_finish(
                    &slot.model,
                    &slot.model_id,
                    &mut ctx_lock.ctx,
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = slot_label, error = %e, "stream error");
                    let _ = tx.blocking_send(StreamFrame::Finish {
                        reason: FinishReason::Error(format!("{e}")),
                        usage: None,
                    });
                } else {
                    tracing::info!(
                        slot = slot_label,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream_with_finish: done"
                    );
                }
            });
        } else {
            let slot = Arc::clone(&self.fast);
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("fast slot permit closed: {e}")))?;
            let quirks = self.fast_quirks.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) = ModelSlot::generate_stream_sync_with_finish(
                    &slot.model,
                    &slot.model_id,
                    &mut ctx_lock.ctx,
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = "fast", error = %e, "stream error");
                    let _ = tx.blocking_send(StreamFrame::Finish {
                        reason: FinishReason::Error(format!("{e}")),
                        usage: None,
                    });
                } else {
                    tracing::info!(
                        slot = "fast",
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream_with_finish: done"
                    );
                }
            });
        }

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let slot = self.embed_slot.as_ref().ok_or_else(|| {
            Error::Inference(
                "No embedding model is configured. Open Settings → Embedding model and \
                 select a GGUF embedding model (e.g. Qwen3-Embedding-0.6B-Q4_K_M.gguf), \
                 then retry.".to_string(),
            )
        })?;
        let slot = Arc::clone(slot);
        let text_len = text.len();
        let text = text.to_string();

        tracing::debug!(text_chars = text_len, "inference.embed: call");

        tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let mut ctx_lock = slot.contexts[0].blocking_lock();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                EmbedSlot::embed_sync(
                    &slot.model,
                    &mut ctx_lock.ctx,
                    &text,
                    slot.n_embd,
                    slot.max_input_tokens,
                    slot.embed_quirks.as_ref(),
                )
            }));
            match result {
                Ok(Ok(v)) => {
                    tracing::debug!(
                        dims = v.len(),
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.embed: done"
                    );
                    Ok(v)
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "embed error");
                    Err(e)
                }
                Err(_) => {
                    tracing::error!("embed panicked — input may be malformed");
                    Err(Error::Inference(
                        "Embedding inference panicked — input may be malformed or \
                         the embed model is incompatible.".to_string(),
                    ))
                }
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Embed task failed: {e}")))?
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let slot = self.embed_slot.as_ref().ok_or_else(|| {
            Error::Inference(
                "No embedding model is configured. Open Settings → Embedding model and \
                 select a GGUF embedding model (e.g. Qwen3-Embedding-0.6B-Q4_K_M.gguf), \
                 then retry.".to_string(),
            )
        })?;
        let slot = Arc::clone(slot);
        let query = query.to_string();

        tokio::task::spawn_blocking(move || {
            let mut ctx_lock = slot.contexts[0].blocking_lock();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                EmbedSlot::embed_query_sync(
                    &slot.model,
                    &mut ctx_lock.ctx,
                    &query,
                    slot.n_embd,
                    slot.max_input_tokens,
                    slot.embed_quirks.as_ref(),
                )
            }));
            match result {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::Inference(
                    "Embedding inference panicked — input may be malformed or \
                     the embed model is incompatible.".to_string(),
                )),
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Embed task failed: {e}")))?
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let slot = self.embed_slot.as_ref().ok_or_else(|| {
            Error::Inference(
                "No embedding model is configured. Open Settings → Embedding model and \
                 select a GGUF embedding model (e.g. Qwen3-Embedding-0.6B-Q4_K_M.gguf), \
                 then retry.".to_string(),
            )
        })?;
        let slot = Arc::clone(slot);
        let texts = texts.to_vec();

        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                EmbedSlot::run_embed_batch_sync(&slot, &texts)
            }));
            match result {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::Inference(
                    "Batch embedding inference panicked — one or more inputs may be \
                     malformed or the embed model is incompatible.".to_string(),
                )),
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Embed batch task failed: {e}")))?
    }

    /// Reranker override. Routes to the lazily-installed `RerankSlot`
    /// when present; returns `Error::NotImplemented` otherwise so the
    /// caller (corpus-engine `search_with_rerank`) falls back to the
    /// un-reranked fusion result without surfacing a hard error.
    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let slot = {
            let guard = self
                .rerank_slot
                .lock()
                .map_err(|e| Error::Inference(format!("rerank slot lock poisoned: {e}")))?;
            guard.as_ref().map(Arc::clone)
        };
        let slot = slot.ok_or_else(|| {
            Error::NotImplemented(
                "No reranker model is installed. Set rerank.path in models.toml \
                 (or SOVEREIGN_RERANK_MODEL_PATH) and restart the daemon."
                    .to_string(),
            )
        })?;
        let query = query.to_string();
        let docs = docs.to_vec();
        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                slot.score_batch(&query, &docs)
            }));
            match result {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::Inference(
                    "Rerank inference panicked — model may be incompatible with \
                     pooling=rank, or an input pair exceeded its context."
                        .to_string(),
                )),
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Rerank task failed: {e}")))?
    }

    fn model_id_for(&self, speed: Speed) -> String {
        let use_primary = matches!(speed, Speed::Slow | Speed::Medium);
        if use_primary {
            // Primary slot is lazy-loaded; derive from path without locking.
            self.primary_path
                .as_ref()
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or(&self.fast.model_id)
                .to_string()
        } else {
            self.fast.model_id.clone()
        }
    }

    /// PR-E2: filename stem of the configured code GGUF, or `None` if
    /// the user skipped configuring a Code specialist. Mirrors
    /// `model_id_for` — derived from the path without locking.
    fn code_model_id(&self) -> Option<String> {
        self.code_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: if self.hardware.gpu_available {
                Speed::Medium
            } else {
                Speed::Slow
            },
            relative_reasoning: Depth::Deep,
        }
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        // Honour the operator-set budget — when present, LRU
        // eviction kicks in if the new slot's size would push the
        // lineup past the cap. When absent, behaviour matches the
        // historical "hold forever" semantics.
        let budget = self.extras_memory_budget();
        self.load_extra_with_budget(slot_name, path, context_size, budget)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.unload_extra(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        // Delegate to the inherent method to avoid the recursive
        // call that `EmbeddedLlamaCpp::extras_inventory` would
        // produce if we just wrote `self.extras_inventory()` here.
        Self::extras_inventory(self)
    }

    async fn warmup_primary(&self) -> Result<()> {
        // No primary configured (single-model BYOM setup) — nothing
        // to warm; return success so callers don't have to special-
        // case the topology.
        let Some(target_path) = self.primary_path.clone() else {
            return Ok(());
        };

        // Acquire the same lazy-slot inflight permit `complete()` /
        // `complete_stream()` use, so a warmup can't race a hot-swap
        // out from under an in-flight request.
        let _permit = self
            .lazy_inflight
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
        let primary_lock = Arc::clone(&self.primary);
        let backend = Arc::clone(&self.primary_backend);
        let ctx_size = self.primary_ctx_size;
        let gpu_layers = self.gpu_layers;
        let loaded_path = Arc::clone(&self.primary_loaded_path);

        // Load on a blocking thread — `ModelSlot::load` is
        // synchronous llama.cpp work and can take 10–90s on a 35B.
        // Mirrors the dispatch path's blocking section.
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut primary = primary_lock.blocking_lock();
            let mut loaded = loaded_path.blocking_lock();
            // Already warm with the right model? Idempotent fast
            // path — no log spam, no work.
            if loaded.as_deref() == Some(target_path.as_path()) && primary.is_some() {
                return Ok(());
            }
            let prior = loaded.clone();
            tracing::info!(
                slot = "primary",
                from = ?prior.as_ref().map(|p| p.display().to_string()),
                to = %target_path.display(),
                "warmup_primary: loading lazy slot ahead of first request"
            );
            // If a different model is currently resident (e.g. a
            // Code-slot hot-swap left it loaded), drop it before
            // loading the primary — same eviction step the dispatch
            // path runs on hot-swap.
            *primary = None;
            let started = Instant::now();
            let s = ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers)?;
            *primary = Some(s);
            *loaded = Some(target_path.clone());
            tracing::info!(
                slot = "primary",
                latency_ms = started.elapsed().as_millis() as u64,
                "warmup_primary: lazy slot ready"
            );
            Ok(())
        })
        .await
        .map_err(|e| Error::Inference(format!("warmup join failed: {e}")))?
    }
}

// ─── Shared helpers ────────────────────────────────────────────

/// Resolve the actual `max_tokens` budget for a generation, given the
/// caller's request, the prompt length, and the loaded context window.
///
/// Three behaviours we want to preserve, in order of importance:
///   1. **Never exceed the context window.** llama.cpp's KV cache
///      asserts on overflow and crashes the daemon. The decode loop
///      treats this cap as the upper bound for `n_generated`.
///   2. **Never reject a request that could partially succeed.** If a
///      caller asks for max_tokens=4096 on a 32k context but their
///      prompt is 30k tokens, we'd rather emit ~2k tokens than 503
///      with "Prompt too long" — opencode and aider don't recover
///      from that gracefully and the user just sees a dead chat.
///   3. **Default generously when the caller omits max_tokens.** A
///      1024-token cap (the previous default) clipped most coding
///      replies mid-thought. We hand back the entire remaining
///      context window in that case; the EOG sampler still terminates
///      naturally for short answers.
///
/// Errors only when the prompt itself doesn't fit. That's a real
/// "your input is too big" — no clamping can recover.
pub(crate) fn clamp_max_tokens(
    requested: Option<usize>,
    prompt_tokens: usize,
    n_ctx: usize,
) -> Result<usize> {
    if prompt_tokens >= n_ctx {
        return Err(Error::Inference(format!(
            "Prompt too long: {prompt_tokens} tokens already meets or exceeds \
             the context window of {n_ctx}. Shorten the conversation."
        )));
    }
    let headroom = n_ctx - prompt_tokens;
    let resolved = match requested {
        Some(asked) if asked > headroom => {
            tracing::warn!(
                requested = asked,
                clamped_to = headroom,
                prompt_tokens,
                n_ctx,
                "max_tokens exceeded context headroom; clamping to fit instead of rejecting"
            );
            headroom
        }
        Some(asked) => asked,
        // No explicit cap — give the model the whole remaining
        // window. Generation still stops at EOG for short replies.
        None => headroom,
    };
    Ok(resolved)
}

fn format_prompt(
    model: &LlamaModel,
    model_id: &str,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> Result<String> {
    // Inject thinking-mode token into the system message based on family quirks.
    // `think_budget == Some(0)` signals the caller wants thinking suppressed.
    // For SystemPromptToken families (Qwen3, Qwen3.5, SmolLM3): append /think or /no_think.
    // For AlwaysOn (Phi-4-reasoning): thinking cannot be disabled — don't inject.
    // For None (Gemma3, Llama3, Phi-4): no thinking tokens exist — don't inject.
    let base_system = request.system_message.as_deref().unwrap_or("");
    let system_with_thinking = match &quirks.thinking {
        ThinkingControl::SystemPromptToken { enable, disable } => {
            let token = if request.think_budget == Some(0) { disable } else { enable };
            if base_system.is_empty() {
                token.clone()
            } else {
                format!("{base_system}\n{token}")
            }
        }
        // AlwaysOn: thinking is structural — injecting /no_think degrades output.
        // None: family has no thinking tokens — nothing to inject.
        ThinkingControl::AlwaysOn | ThinkingControl::None => base_system.to_string(),
    };

    // Tool-use prompt augmentation. Qwen3.5's chat template expects tool
    // schemas to appear in the system prompt inside a `<tools>...</tools>`
    // block of newline-separated JSON objects. The model then emits tool
    // calls as `<tool_call>{"name": "...", "arguments": {...}}</tool_call>`
    // which `parse_tool_calls_from_text` below decodes.
    //
    // We append to the existing system prompt (rather than replacing or
    // injecting a separate message) so the thinking-token augmentation
    // above still lands. Tools without a system prompt get a minimal
    // "You may call one of the following tools" lead-in so the model
    // doesn't try to emit tool calls in free chat by default.
    let system_with_tools = if let Some(tools) = request.tools.as_ref().filter(|t| !t.is_empty()) {
        let mut block = String::from(
            "\n\n# Tools\n\nYou may call one or more of the following tools by emitting a \
             `<tool_call>{\"name\": ..., \"arguments\": {...}}</tool_call>` block. One call per \
             block. After a block, stop — the runtime will execute the tool and feed the result \
             back to you in the next turn.\n\n<tools>\n"
        );
        for t in tools {
            let entry = serde_json::json!({
                "name": t.name,
                "description": t.description.clone().unwrap_or_default(),
                "parameters": t.parameters,
            });
            block.push_str(&entry.to_string());
            block.push('\n');
        }
        block.push_str("</tools>");
        if system_with_thinking.is_empty() {
            block.trim_start().to_string()
        } else {
            format!("{system_with_thinking}{block}")
        }
    } else {
        system_with_thinking
    };
    let system_with_thinking = system_with_tools;

    // Three-tier prompt-building strategy. Each tier is tried in
    // order; the first one that succeeds wins.
    //
    // 1. **Basic `apply_chat_template`** — calls llama.cpp's
    //    built-in `llama_chat_apply_template`. Fast, no JSON
    //    serialization, but its template parser only supports a
    //    limited Jinja-like subset (no macros, no complex control
    //    flow). Works for Qwen3 / Llama3 / base Phi-4 / etc.
    //
    // 2. **`apply_chat_template_oaicompat` with `use_jinja=true`**
    //    — calls llama.cpp's full minja-based Jinja2 path (the
    //    same one llama-server's `--jinja` flag uses). Handles
    //    templates that use macros or complex control flow.
    //    Required for Gemma 3/4 (their gguf templates start with
    //    `{%- macro format_parameters() -%}`).
    //
    // 3. **Plain-text concat** (last resort) — `{system}\n\n{user}`
    //    with no role markers. The model has no turn boundaries
    //    so it'll role-play multi-turn ("User: ... Assistant: ...")
    //    until `max_tokens`. We loud-warn so operators see this is
    //    happening — it was a silent fallback up until 2026-04-26
    //    (gemma-4-31B atlas bench debugging session).
    let template = match model.chat_template(None) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                model_id = %model_id,
                error = ?e,
                "model.chat_template(None) returned no template — model gguf has no \
                 tokenizer.chat_template metadata. Falling back to plain-text concat; \
                 model output may include hallucinated role markers and may not stop \
                 at the right place."
            );
            return Ok(plain_text_prompt(&system_with_thinking, &request.prompt));
        }
    };

    // Templates that use Jinja2 macros (`{%- macro ... -%}`),
    // `{% set ... %}` blocks, conditional includes, or other
    // constructs beyond the llama.cpp built-in parser's subset
    // need to skip tier 1 entirely — `apply_chat_template` would
    // return `FfiError(-1)` for every call, wasting work and
    // logging noise. Gemma 3 / Gemma 4 / Llama 3.1+ templates all
    // qualify. Detect once at the call site and route directly.
    let needs_jinja = template_needs_jinja(&template);

    // Tier 1: built-in subset (skipped for Jinja-only templates).
    if !needs_jinja {
        let mut messages = Vec::new();
        if !system_with_thinking.is_empty() {
            messages.push(
                LlamaChatMessage::new("system".to_string(), system_with_thinking.clone())
                    .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
            );
        }
        messages.push(
            LlamaChatMessage::new("user".to_string(), request.prompt.clone())
                .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
        );
        match model.apply_chat_template(&template, &messages, true) {
            Ok(formatted) => return Ok(formatted),
            Err(e) => {
                tracing::debug!(
                    model_id = %model_id,
                    error = ?e,
                    "apply_chat_template (built-in subset) failed; retrying via Jinja2 path"
                );
            }
        }
    }

    // Tier 2: Jinja2 via oaicompat path. Build an OpenAI-style
    // messages JSON array — simpler than allocating
    // `LlamaChatMessage` again because the oaicompat ABI takes
    // raw JSON anyway.
    let messages_json = build_oai_messages_json(&system_with_thinking, &request.prompt)?;
    let params = OpenAIChatTemplateParams {
        messages_json: &messages_json,
        tools_json: None,
        tool_choice: None,
        json_schema: None,
        grammar: None,
        reasoning_format: None,
        chat_template_kwargs: None,
        add_generation_prompt: true,
        use_jinja: true,
        parallel_tool_calls: false,
        // Per-request override (`request.enable_thinking`) wins over
        // the historical default-false. Callers that pin this — the
        // relational/witness path in particular — get the chat
        // template to wrap planning in `<think>...</think>` so the
        // post-process strip can drop it cleanly. Default-false is
        // preserved for every other call site.
        enable_thinking: request.enable_thinking.unwrap_or(false),
        // BOS/EOS handling: the caller (generate_sync /
        // generate_stream_sync) tokenises with `AddBos::Always`,
        // which prepends the model's BOS at tokenisation time. If
        // we also asked the template renderer to emit BOS, we'd
        // double-prepend and the first decode position would be
        // off by one. EOS is never wanted on a generation prompt.
        add_bos: false,
        add_eos: false,
        parse_tool_calls: false,
    };
    match model.apply_chat_template_oaicompat(&template, &params) {
        Ok(result) => {
            tracing::trace!(
                model_id = %model_id,
                jinja_required = needs_jinja,
                prompt_chars = result.prompt.len(),
                "apply_chat_template_oaicompat (Jinja2) ok"
            );
            return Ok(result.prompt);
        }
        Err(e) => {
            tracing::warn!(
                model_id = %model_id,
                error = ?e,
                template_head = %template_head_for_log(&template),
                "apply_chat_template_oaicompat (Jinja2) failed — minja rejected the \
                 gguf's chat template. Falling back to plain-text concat; model output \
                 may include hallucinated role markers."
            );
        }
    }

    // Tier 3: plain-text concat.
    Ok(plain_text_prompt(&system_with_thinking, &request.prompt))
}

/// Cheap heuristic for whether a chat template requires the full
/// Jinja2 (minja) renderer rather than llama.cpp's built-in
/// limited-subset parser. The built-in parser handles plain
/// `{{ var }}` substitution, simple `{% if %} ... {% endif %}`
/// blocks, and `{% for x in xs %}` loops — but trips on macros
/// (`{%- macro ... -%}`), `{% set ... %}` blocks, and a few other
/// constructs that the recent generation of model templates use.
///
/// We don't try to be exhaustive — any false negative just
/// degrades back to the existing fall-through behaviour (tier-1
/// returns FfiError, tier 2 takes over). The win here is a clean
/// fast path for known-Jinja templates so we don't log a
/// debug-level noise line on every chat call.
fn template_needs_jinja(template: &llama_cpp_2::model::LlamaChatTemplate) -> bool {
    let s = match template.to_str() {
        Ok(s) => s,
        Err(_) => return true, // non-utf8 — punt to the safer renderer
    };
    s.contains("{%- macro")
        || s.contains("{% macro")
        || s.contains("{%- set")
        || s.contains("{% set")
        || s.contains("{%- include")
        || s.contains("{% include")
}

/// Final-fallback prompt when no chat-template path works. Just
/// concatenates system + user with a blank line. The model has no
/// turn boundaries; expect it to role-play multi-turn output.
fn plain_text_prompt(system: &str, user: &str) -> String {
    if system.is_empty() {
        user.to_string()
    } else {
        format!("{system}\n\n{user}")
    }
}

/// Serialize a (system, user) pair into the OpenAI-compatible
/// messages JSON shape that `apply_chat_template_oaicompat`
/// expects. System is omitted when empty so templates that don't
/// support a system role (e.g. Gemma) don't get a stray empty
/// turn.
fn build_oai_messages_json(system: &str, user: &str) -> Result<String> {
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(2);
    if !system.is_empty() {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    messages.push(serde_json::json!({"role": "user", "content": user}));
    serde_json::to_string(&messages)
        .map_err(|e| Error::Inference(format!("Failed to serialize chat messages: {e}")))
}

/// First ~80 chars of a chat template, with newlines escaped, for
/// log output. Lets operators identify which template format is
/// hitting the fallback path without dumping a full multi-KB
/// macro definition into the daemon log.
fn template_head_for_log(template: &llama_cpp_2::model::LlamaChatTemplate) -> String {
    let s = template.to_str().unwrap_or("<non-utf8>");
    let head: String = s.chars().take(80).collect();
    head.replace('\n', "\\n")
}

/// Token budget for `<think>` blocks. After this many tokens inside a thinking
/// block the generation loop injects `</think>` into the KV cache and stream,
/// forcing the model to transition to its answer rather than spiral endlessly.
const THINK_BUDGET: usize = 512;

/// Build the sampler chain.
///
/// Technique rationale:
/// - **DRY** ("Don't Repeat Yourself"): penalises repeated *token sequences*
///   rather than individual tokens. This is the primary fix for the
///   "completely totally wholly fully …" cascade — those are all different
///   tokens, so standard `penalties` misses them, but DRY catches the
///   recurring suffix pattern they form.
/// - **min_p**: replaces top_p. When the model is confident (high-probability
///   next token) min_p becomes more selective, preventing runaway picks from
///   a collapsed distribution. When uncertain it relaxes, preserving diversity.
/// - **penalties**: kept as a backstop for single-token repetition that DRY's
///   `allowed_length = 2` intentionally ignores.
/// Sampler chain plus optional schema-driven logit mask.
///
/// History: the per-token loop used to call `LlamaSampler::sample`
/// directly, which runs the entire chain inside llama.cpp. Grammar
/// enforcement was provided either by `LlamaSampler::llguidance`
/// (silent fallthrough on every BYOM model) or `LlamaSampler::grammar`
/// (crashes the daemon process at `GGML_ASSERT(!stacks.empty())`,
/// `llama-grammar.cpp:940`, reproducible across Vulkan AND ROCm AND
/// single-slot daemon configurations). See
/// `memory/project_grammar_alpha_blocker.md`.
///
/// `ConstrainedSampler` replaces both: it owns a `LlamaSampler` chain
/// (without any grammar sampler), and optionally owns a
/// `JsonConstraint` that masks token logits in pure Rust before the
/// chain runs. No call into `llama-grammar.cpp` ever fires.
/// Which "role" the sampler is currently filling. The same primary
/// slot serves multiple cognitive tasks per turn — picking the next
/// tool and authoring its arguments behave very differently from
/// each other and from filling content inside a JSON string value
/// (paths, file contents, Rust source). Investment #16 (2026-05-13):
/// instead of one global temperature, the sampler holds a per-role
/// `LlamaSampler` chain and the generation loop picks which to pull
/// each token from based on its position in the emit stream.
///
/// Roles are intentionally coarse-grained at v1 — easy to extend
/// (e.g. add `Decision` for the first K tokens after a `:` in a JSON
/// envelope), but the two-way split captures the dominant cost:
/// high-T exploration helps tool selection escape attractors,
/// low-T content fills paths/code without char-level drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplerRole {
    /// Default: tool envelope structure, decision points, anywhere
    /// outside a JSON string. Per-request `temperature` applies.
    Explore,
    /// Inside a JSON string value (path, command text, file
    /// content). Greedy or low-T to suppress char-level drift like
    /// `/Users/ale xsbryan` observed in the 2026-05-13 codex smoke.
    Content,
}

pub struct ConstrainedSampler {
    /// Per-role inner chains. `accept()` advances both so the DRY /
    /// penalty trackers stay in sync regardless of which one
    /// produced any given token.
    inner_explore: LlamaSampler,
    inner_content: LlamaSampler,
    constraint: Option<crate::json_constraint::JsonConstraint>,
    /// Vocab-sized bitmap of tokens whose rendered bytes contain a
    /// 3+ byte UTF-8 leading byte (CJK / Devanagari / Hangul / etc.).
    /// When `Some`, `sample()` clamps those tokens' logits to
    /// `-INFINITY` on every step, regardless of whether a JSON
    /// constraint is active. Populated by `build_sampler` when the
    /// `SOVEREIGN_BLOCK_NON_LATIN` env var is set.
    non_latin_denylist: Option<std::sync::Arc<Vec<bool>>>,
}

impl ConstrainedSampler {
    /// Per-token: pull candidates from ctx, mask via constraint (if
    /// any), apply the rest of the chain, return the selected token.
    ///
    /// Performance note (2026-05-12): the mask is currently full-vocab
    /// O(152K × per-candidate-parser-cost). A previous attempt to
    /// pre-filter via `LlamaSampler::top_k(2048)` before the mask
    /// achieved 2.3× speedup on the trivial-prompt probe but
    /// produced incorrect output on real prompts (Chinese tokens
    /// slipped through the grammar). The root issue is that
    /// per-candidate incremental parsing is structurally wrong;
    /// the right fix is a precomputed token-acceptance table per
    /// FSM state (the pattern LM-Format-Enforcer and Outlines use).
    /// Until that lands, the slower-but-correct mask stays.
    pub fn sample(
        &mut self,
        ctx: &llama_cpp_2::context::LlamaContext<'_>,
        idx: i32,
        role: SamplerRole,
    ) -> LlamaToken {
        let mut data = if idx < 0 {
            ctx.token_data_array()
        } else {
            ctx.token_data_array_ith(idx)
        };
        if let Some(c) = self.constraint.as_mut() {
            c.mask(&mut data);
        }
        // Non-Latin denylist: independent of the JSON-schema mask, so
        // it covers free-form chat and non-`structured_output` paths
        // that the grammar layer doesn't reach. A single L1 lookup per
        // candidate; the bitmap was built once at slot-load.
        if let Some(deny) = self.non_latin_denylist.as_ref() {
            for entry in data.data.iter_mut() {
                let id = entry.id().0 as usize;
                if deny.get(id).copied().unwrap_or(false) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            }
        }
        let inner = match role {
            SamplerRole::Explore => &self.inner_explore,
            SamplerRole::Content => &self.inner_content,
        };
        data.apply_sampler(inner);
        data.selected_token()
            .expect("sampler chain failed to select a token")
    }

    /// Advance both inner chains (DRY / penalties state stays in
    /// sync regardless of which role produced this token) and the
    /// constraint state machine.
    pub fn accept(&mut self, token: LlamaToken) {
        self.inner_explore.accept(token);
        self.inner_content.accept(token);
        if let Some(c) = self.constraint.as_mut() {
            c.accept(token);
        }
    }
}

fn build_sampler(
    model: &LlamaModel,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> ConstrainedSampler {
    // Schema-driven constraint (pure Rust, bypasses llama-grammar.cpp
    // entirely). Compiled at request time from the OpenAI-style
    // response_format → CompletionRequest.structured_output. Failure
    // to compile is a real schema problem; warn loudly and fall back
    // to unconstrained sampling so the request doesn't hard-fail.
    let constraint = request.structured_output.as_ref().and_then(|schema| {
        match crate::json_constraint::JsonConstraint::new(schema, model) {
            Ok(c) => {
                tracing::info!("grammar-constrained decoding enabled (in-house mask, no llama-grammar.cpp)");
                Some(c)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "JsonConstraint compile failed — falling back to free-form sampling"
                );
                None
            }
        }
    });

    // Non-Latin token denylist: opt-in via env var. Built once per
    // model and cached for the daemon's lifetime. Applies on every
    // inference path (chat, completion, structured) so callers don't
    // have to remember to set `x-asciiExtended` on every schema.
    // Default OFF — some corpora (Confucian-philosophy articles,
    // multilingual chat) legitimately need CJK tokens.
    let non_latin_denylist = if non_latin_block_enabled() {
        Some(crate::json_constraint::non_latin_denylist_for(model))
    } else {
        None
    };

    // Temperature: per-request override → family default.
    let temp = request.temperature.unwrap_or(quirks.default_temperature);
    // Top-k: per-request override → family default → hard fallback of 40.
    let top_k_val = request.top_k
        .or(quirks.default_top_k)
        .unwrap_or(40) as i32;
    // Sequence breakers tell DRY where one "thought unit" ends and another
    // begins — any of these tokens resets the repeated-suffix detector.
    let breakers: &[&[u8]] = &[b"\n", b".", b"?", b"!", b":", b"\"", b"*"];

    // Build per-role inner chains. Both share DRY+penalties (state
    // is kept in sync by `accept()` advancing both). The difference
    // is the final sampling stage: Explore uses request T; Content
    // is greedy unless overridden.
    //
    // Content-role temperature override: env var
    // `SOVEREIGN_CONTENT_TEMPERATURE` lets an operator dial Content
    // up if greedy turns out too tight (e.g. would still pick the
    // same wrong path every time). Default 0.0 (greedy). Range
    // [0.0, 2.0].
    let content_temp = std::env::var("SOVEREIGN_CONTENT_TEMPERATURE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| (0.0..=2.0).contains(v))
        .unwrap_or(0.0);

    let build_chain = |chain_temp: f32| {
        let mut samplers: Vec<LlamaSampler> = Vec::new();
        samplers.push(LlamaSampler::dry(model, 0.8, 1.75, 2, -1, breakers.iter().copied()));
        samplers.push(LlamaSampler::penalties(128, 1.15, 0.1, 0.1));
        if chain_temp < 0.01 {
            samplers.push(LlamaSampler::greedy());
        } else {
            samplers.push(LlamaSampler::top_k(top_k_val));
            samplers.push(LlamaSampler::min_p(0.05, 1));
            samplers.push(LlamaSampler::temp(chain_temp));
            samplers.push(LlamaSampler::dist(rand_seed()));
        }
        LlamaSampler::chain_simple(samplers)
    };

    ConstrainedSampler {
        inner_explore: build_chain(temp),
        inner_content: build_chain(content_temp),
        constraint,
        non_latin_denylist,
    }
}

/// Read `SOVEREIGN_BLOCK_NON_LATIN` once per `build_sampler` call.
/// Accepts the same truthy spellings as `SOVEREIGN_GRAMMAR_TIMING`
/// (`1` / `true`, case-insensitive) so operators have a consistent
/// toggle convention across grammar features.
fn non_latin_block_enabled() -> bool {
    match std::env::var("SOVEREIGN_BLOCK_NON_LATIN") {
        Ok(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
}

// ─── JSON Schema → GBNF Grammar ─────────────────────────────

/// Convert a JSON schema to a GBNF (GGML BNF) grammar string.
///
/// A single tool call extracted from model output. The adapter maps
/// this into `commonwealth_api::openai_types::ToolCall` (with a
/// generated id) before emitting the chat-completion response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolCall {
    pub name: String,
    /// Raw JSON string for the tool arguments. Kept as a string so the
    /// client sees exactly what the model produced — if the model emits
    /// partial JSON we don't silently coerce it.
    pub arguments: String,
}

/// Extract Qwen3.5-style tool calls from free-form model output.
///
/// Expected markup:
///
/// ```text
/// <tool_call>{"name": "get_weather", "arguments": {"city": "SF"}}</tool_call>
/// ```
///
/// Multiple blocks per response are supported. Whitespace inside a block
/// is ignored. A block whose body is not valid JSON, or whose JSON lacks
/// a top-level `"name"` field, is **skipped** — not treated as an error
/// — and logged at `warn` so the adapter can tag an `atos_tool_events`
/// row with `phase='parse_error'` and the raw payload. Returning an
/// empty vec on all-malformed output is intentional: a tool-less
/// response is a valid answer.
///
/// Observability: the parser stays pure (no I/O); the caller is
/// responsible for logging. This keeps the function unit-testable
/// without a tracing subscriber.
/// Find the byte length of a balanced JSON object starting at the
/// first `{` in `s`. String-aware (won't trip on `}` inside `"..."`
/// values). Returns None when the input doesn't contain a complete
/// `{...}` (depth never returns to zero) — which is also the
/// "model truncated mid-emission" signal we use to abandon parsing.
///
/// Used by the lenient tool-call extractor when `</tool_call>` is
/// missing. Some quantized models reliably emit `<tool_call>{...}`
/// with a valid JSON object but never close the XML tag; falling
/// back to brace-balancing recovers the call instead of dropping it.
fn find_balanced_json_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Escape unescaped control characters (newline, CR, tab) that appear
/// inside JSON string values. Intentionally narrow: only operates
/// inside `"..."` runs and only on the three control chars known to
/// trip serde — the rest of the body is passed through byte-for-byte.
///
/// Why this exists: some models (Qwen3-Coder-30B observed 2026-05-08)
/// emit a syntactically-correct JSON envelope around their tool call
/// but fail to escape literal `\n`/`\r`/`\t` inside the `content`
/// string value. The result balances braces and parses up to the
/// invalid-control-in-string error, so the daemon was rejecting the
/// whole call. This pre-pass converts those raw bytes to their
/// `\n`/`\r`/`\t` escape forms so `serde_json::from_str` accepts the
/// body. Already-escaped sequences (preceded by an unescaped `\`)
/// pass through untouched.
///
/// Returns the original string if nothing needed escaping (no
/// allocation), otherwise a normalized copy.
pub fn escape_unescaped_control_chars_in_string_values(
    body: &str,
) -> std::borrow::Cow<'_, str> {
    let needs_fix = {
        let mut in_string = false;
        let mut escape = false;
        let mut hit = false;
        for c in body.chars() {
            if in_string {
                if escape {
                    escape = false;
                } else {
                    match c {
                        '"' => in_string = false,
                        '\\' => escape = true,
                        '\n' | '\r' | '\t' => {
                            hit = true;
                            break;
                        }
                        _ => {}
                    }
                }
            } else if c == '"' {
                in_string = true;
            }
        }
        hit
    };
    if !needs_fix {
        return std::borrow::Cow::Borrowed(body);
    }

    let mut out = String::with_capacity(body.len() + 16);
    let mut in_string = false;
    let mut escape = false;
    for c in body.chars() {
        if in_string {
            if escape {
                out.push(c);
                escape = false;
                continue;
            }
            match c {
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\\' => {
                    escape = true;
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

pub fn parse_tool_calls_from_text(text: &str) -> Vec<ParsedToolCall> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_start) = text[cursor..].find("<tool_call>") {
        let start = cursor + rel_start + "<tool_call>".len();
        // Find the matching closer. Use the literal `</tool_call>`
        // marker — we intentionally do NOT allow nested `<tool_call>`
        // blocks (Qwen3.5 never emits them).
        //
        // Lenient mode: some quantized models emit `<tool_call>` then
        // the JSON body but forget the closing `</tool_call>` tag
        // (FINAL-Bench at Q6_K_M shows this consistently). When the
        // closer is missing, fall back to brace-balancing inside the
        // remaining text — if we find a complete JSON object, use
        // its end as the implicit closer and continue scanning.
        let (body, advance) = match text[start..].find("</tool_call>") {
            Some(rel_end) => (
                text[start..start + rel_end].trim().to_string(),
                start + rel_end + "</tool_call>".len(),
            ),
            None => match find_balanced_json_end(&text[start..]) {
                Some(json_len) => {
                    let body = text[start..start + json_len].trim().to_string();
                    (body, start + json_len)
                }
                None => break, // can't recover; stop scanning
            },
        };
        cursor = advance;

        // Parse the body. Accept either:
        //   {"name": "...", "arguments": {...}}
        //   {"name": "...", "arguments": "<string>"}
        // If serde rejects the raw body (commonly: raw newlines inside
        // a string value), retry on a control-char-normalized copy.
        let parsed = serde_json::from_str::<serde_json::Value>(&body)
            .or_else(|_| {
                let fixed =
                    escape_unescaped_control_chars_in_string_values(&body);
                serde_json::from_str::<serde_json::Value>(&fixed)
            });
        match parsed {
            Ok(obj) => {
                let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                    continue; // malformed — skip (caller may log via diagnostic API below)
                };
                let args_str = match obj.get("arguments") {
                    Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                out.push(ParsedToolCall {
                    name: name.to_string(),
                    arguments: args_str,
                });
            }
            Err(_) => {
                // malformed JSON — skip; caller logs via tracing::warn!
                continue;
            }
        }
    }
    out
}

/// Same as [`parse_tool_calls_from_text`] but returns the raw bodies
/// of any blocks that failed to parse, so the caller can attribute
/// parse-error telemetry (feeds `atos_tool_events.phase='parse_error'`).
pub fn parse_tool_calls_with_errors(text: &str) -> (Vec<ParsedToolCall>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_start) = text[cursor..].find("<tool_call>") {
        let start = cursor + rel_start + "<tool_call>".len();
        // Same lenient closer behaviour as `parse_tool_calls_from_text`:
        // accept missing `</tool_call>` when the body parses as a
        // balanced JSON object. Models with damaged closing-tag
        // emission (FINAL-Bench Q6_K_M) emit valid call payloads
        // surrounded by half-broken markup.
        let (body, advance) = match text[start..].find("</tool_call>") {
            Some(rel_end) => (
                text[start..start + rel_end].trim().to_string(),
                start + rel_end + "</tool_call>".len(),
            ),
            None => match find_balanced_json_end(&text[start..]) {
                Some(json_len) => {
                    let body = text[start..start + json_len].trim().to_string();
                    (body, start + json_len)
                }
                None => {
                    errors.push(text[start..].to_string());
                    break;
                }
            },
        };
        cursor = advance;
        let body = body.as_str();

        // Same retry-on-normalized-body pattern as the non-with-errors
        // variant (Qwen3-Coder emits raw \n inside a content string).
        let parsed = serde_json::from_str::<serde_json::Value>(body)
            .or_else(|_| {
                let fixed =
                    escape_unescaped_control_chars_in_string_values(body);
                serde_json::from_str::<serde_json::Value>(&fixed)
            });
        match parsed {
            Ok(obj) => {
                let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                    errors.push(body.to_string());
                    continue;
                };
                let args_str = match obj.get("arguments") {
                    Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                out.push(ParsedToolCall {
                    name: name.to_string(),
                    arguments: args_str,
                });
            }
            Err(_) => errors.push(body.to_string()),
        }
    }
    (out, errors)
}

/// Handles the subset of JSON schema used by tool descriptors:
/// - `"type": "object"` with `"properties"` and `"required"`
/// - `"type": "string"` / `"integer"` / `"number"` / `"boolean"`
/// - Flat schemas only (no nested objects or arrays of objects)
///
/// The grammar constrains the token sampler so the model can only
/// produce valid JSON matching the schema. This eliminates malformed
/// JSON, missing required fields, and type errors.
pub fn json_schema_to_gbnf(schema: &serde_json::Value) -> String {
    let mut rules = Vec::new();

    // Primitive rules shared by all schemas.
    rules.push(r#"ws ::= [ \t\n]*"#.to_string());
    rules.push(r#"string ::= "\"" ([^"\\] | "\\" .)* "\""  "#.to_string());
    rules.push(r#"integer ::= "-"? [0-9]+"#.to_string());
    rules.push(r#"number ::= "-"? [0-9]+ ("." [0-9]+)?"#.to_string());
    rules.push(r#"boolean ::= "true" | "false""#.to_string());
    rules.push(r#"null ::= "null""#.to_string());

    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let required: Vec<String> = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            if props.is_empty() {
                // Any JSON object.
                rules.push(r#"root ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}""#.to_string());
                rules.push(r#"value ::= string | number | integer | boolean | null"#.to_string());
            } else {
                // Build a rule that produces exactly the required fields in order,
                // then optionally the non-required fields. This is simpler than
                // allowing arbitrary order and covers our tool calling needs.
                let mut field_parts = Vec::new();

                // Required fields first, in schema order.
                for key in &required {
                    if let Some(prop_schema) = props.get(key) {
                        let type_rule = prop_type_rule(prop_schema);
                        field_parts.push(format!(
                            r#"ws "\"{}\"" ws ":" ws {}"#,
                            key, type_rule
                        ));
                    }
                }

                // Optional fields.
                for (key, prop_schema) in &props {
                    if required.contains(key) {
                        continue;
                    }
                    let type_rule = prop_type_rule(prop_schema);
                    field_parts.push(format!(
                        r#"(ws "," ws "\"{}\"" ws ":" ws {})?"#,
                        key, type_rule
                    ));
                }

                if field_parts.is_empty() {
                    rules.push(r#"root ::= "{" ws "}""#.to_string());
                } else {
                    let fields_joined = field_parts.join(r#" ws "," "#);
                    rules.push(format!(r#"root ::= "{{" {} ws "}}""#, fields_joined));
                }
            }
        }
        _ => {
            // Fallback: allow any JSON value.
            rules.push(r#"root ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}""#.to_string());
            rules.push(r#"value ::= string | number | integer | boolean | null"#.to_string());
        }
    }

    rules.join("\n")
}

/// Get the GBNF rule name for a property's type.
fn prop_type_rule(schema: &serde_json::Value) -> &'static str {
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("string") => "string",
        Some("integer") => "integer",
        Some("number") => "number",
        Some("boolean") => "boolean",
        _ => "string", // default to string for unknown types
    }
}

#[cfg(test)]
mod parse_tool_calls_tests {
    //! Lock the parser's behaviour against the two real-world model
    //! emission failure modes:
    //!   1. **Closed tags** — happy path, both `<tool_call>` and
    //!      `</tool_call>` present (Qwen3.5 baseline).
    //!   2. **Missing closing tag** — quantized models (FINAL-Bench
    //!      Q6_K_M observed) emit `<tool_call>{...JSON...}` and
    //!      stop without `</tool_call>`. The lenient brace-balancer
    //!      recovers the call instead of dropping it.
    use super::{parse_tool_calls_from_text, parse_tool_calls_with_errors};

    #[test]
    fn closed_tag_extracts_one_call() {
        let text = r#"prelude <tool_call>{"name":"write","arguments":{"path":"a.rs"}}</tool_call> tail"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("a.rs"));
    }

    #[test]
    fn missing_closing_tag_with_balanced_json_recovers() {
        // FINAL-Bench Q6_K_M reliably truncates the closing tag.
        // We accept the body when the JSON balances cleanly.
        let text = r#"<tool_call>{"name":"write","arguments":{"filePath":"Cargo.toml","content":"[package]\nname = \"x\"\n"}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1, "lenient mode should recover the call");
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("Cargo.toml"));
    }

    #[test]
    fn missing_closing_tag_with_truncated_json_returns_no_calls() {
        // Body never balances — model truncated mid-string. Drop it
        // rather than emit a corrupt call. `_with_errors` should also
        // surface the leftover so telemetry catches it.
        let text = r#"<tool_call>{"name":"write","arguments":{"content":"unterminated string"#;
        let calls = parse_tool_calls_from_text(text);
        assert!(calls.is_empty());
        let (out, errors) = parse_tool_calls_with_errors(text);
        assert!(out.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn brace_inside_string_does_not_close_early() {
        // A string value containing `}` shouldn't close the JSON.
        let text = r#"<tool_call>{"name":"write","arguments":{"content":"loop { x += 1; }"}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn multiple_calls_extract_in_order_even_with_mixed_closers() {
        let text = r#"<tool_call>{"name":"a","arguments":{}}</tool_call>some text<tool_call>{"name":"b","arguments":{"x":1}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn raw_newlines_inside_string_value_recover_via_normalization() {
        // Qwen3-Coder-30B observed 2026-05-08: balanced JSON envelope
        // but raw `\n` (0x0A) bytes inside the `content` string value
        // instead of the `\\n` escape. Without normalization this
        // fails serde with "control character in string". The pre-pass
        // converts the raw bytes to escapes and re-parses.
        let body = "{\"name\":\"write\",\"arguments\":{\"path\":\"a.rs\",\"content\":\"fn main() {\nprintln!(\\\"hi\\\");\n}\"}}";
        let text = format!("<tool_call>{}</tool_call>", body);
        let calls = parse_tool_calls_from_text(&text);
        assert_eq!(calls.len(), 1, "normalization should recover the call");
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("println"));
    }

    #[test]
    fn already_escaped_sequences_are_preserved_through_normalization() {
        use super::escape_unescaped_control_chars_in_string_values;
        // `\\n` (literal backslash + n) must stay `\\n` after the
        // normalize pass — it's an already-correct escape sequence.
        let input = r#"{"x":"a\nb"}"#;
        let out = escape_unescaped_control_chars_in_string_values(input);
        assert_eq!(out.as_ref(), input, "no raw control chars present");
    }

    #[test]
    fn normalization_skips_control_chars_outside_strings() {
        use super::escape_unescaped_control_chars_in_string_values;
        // A newline between fields (outside any string value) must
        // be left alone. JSON allows it as whitespace; serde already
        // accepts it.
        let input = "{\"x\":\"a\",\n\"y\":\"b\"}";
        let out = escape_unescaped_control_chars_in_string_values(input);
        assert_eq!(out.as_ref(), input);
    }
}

#[cfg(test)]
mod pick_slot_tests {
    //! PR-E2: the slot dispatch rules ultimately drive the hot-swap
    //! decision in `complete` / `complete_stream`. Lock them here
    //! with fast, GPU-free table tests so a regression in routing
    //! is caught by `cargo test` long before it reaches a live
    //! llama.cpp load.
    use super::{pick_slot, SlotTarget};
    use sovereign_core::oicp::{CapabilityHint, InferenceRequirements};
    use sovereign_core::types::{CompletionRequest, Speed};

    fn req(speed: Speed, hint: Option<CapabilityHint>) -> CompletionRequest {
        let mut r = CompletionRequest::new("hi");
        r.preferred_speed = speed;
        if let Some(h) = hint {
            r.oicp = Some(InferenceRequirements::new().with_hint(h));
        }
        r
    }

    #[test]
    fn fast_speed_without_code_hint_goes_to_fast() {
        let r = req(Speed::Fast, None);
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Fast);
        assert_eq!(pick_slot(&r, false, false, false, None), SlotTarget::Fast);
    }

    #[test]
    fn fast_short_routing_picks_fast_short_for_small_max_tokens() {
        // Phase 1b/3/5/6 composers attach max_output_tokens ≤512.
        // When the FastShort companion is built, those calls route to
        // the continuous-batched path — that's where the 2.1× wall-
        // clock speedup lives.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(512);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
        // Smaller budgets pass too.
        r.max_tokens = Some(256);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
    }

    #[test]
    fn fast_short_not_selected_when_companion_absent() {
        // SOVEREIGN_FAST_SHORT_DISABLE=1 (or load failure) leaves
        // has_fast_short=false. Every short call falls through to
        // the original Fast slot — no regression vs pre-rework.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(256);
        assert_eq!(
            pick_slot(&r, true, false, false, None),
            SlotTarget::Fast
        );
    }

    #[test]
    fn fast_short_skipped_for_large_output_budget() {
        // Phase 1 chapter ingestion asks for the full output budget
        // (24576 via PHASE1_SEED_OUTPUT_BUDGET). It must route to Fast
        // (which keeps `n_seq_max=1` for the full per-call window),
        // not FastShort — whose 2048-per-seq slice would mid-decode
        // overflow on a long chapter.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(8192);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Fast
        );
    }

    #[test]
    fn fast_short_skipped_when_speed_is_not_fast() {
        // Slow-speed callers (chat synthesis on the primary) ignore
        // FastShort entirely even if their max_output is small.
        // FastShort is for the Fast-slot model only.
        let mut r = req(Speed::Slow, None);
        r.max_tokens = Some(256);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Primary
        );
    }

    #[test]
    fn slow_speed_without_code_hint_picks_primary_when_available() {
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, true, false, false, None), SlotTarget::Primary);
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Primary);
    }

    #[test]
    fn slow_speed_without_primary_falls_back_to_fast() {
        // Degraded config: user only configured a fast GGUF. Pre-E2
        // behaviour that must not regress — a Medium/Slow request
        // still runs instead of erroring out.
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, false, false, false, None), SlotTarget::Fast);
    }

    #[test]
    fn code_hint_with_code_slot_picks_code_even_on_fast_speed() {
        // Code specialist wins over Speed::Fast dispatch. The hint
        // semantics are "this work needs code reasoning"; Fast-slot
        // generals can't do that well.
        let r = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Code);
    }

    #[test]
    fn code_hint_without_code_slot_follows_speed_rules() {
        // Solo-user with no dedicated coder: fall back to whatever
        // speed-tier would normally serve this request. The peer-
        // mesh scheduler is responsible for routing the hint to a
        // better-matched peer; locally we do what we can.
        let r_fast = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(pick_slot(&r_fast, true, false, false, None), SlotTarget::Fast);

        let r_slow = req(Speed::Slow, Some(CapabilityHint::code()));
        assert_eq!(pick_slot(&r_slow, true, false, false, None), SlotTarget::Primary);
    }

    #[test]
    fn non_code_hint_does_not_pick_code_even_when_configured() {
        // The extension hint `x:prose` must not accidentally trip
        // the code dispatch — only the standardized `code` hint
        // should.
        let hint = CapabilityHint::extension("prose").unwrap();
        let r = req(Speed::Slow, Some(hint));
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Primary);
    }

    #[test]
    fn extras_match_wins_over_speed_routing() {
        // Operator-declared per-phase routing must override the
        // Speed-based heuristic. A Fast-speed request with
        // model_id matching an extras slot lands on the extras
        // slot, NOT on the fast slot.
        let r = req(Speed::Fast, None);
        assert_eq!(
            pick_slot(&r, true, false, false, Some("reasoning".into())),
            SlotTarget::Extra("reasoning".into())
        );
    }

    #[test]
    fn extras_match_wins_over_code_hint() {
        // Even a code-hinted request defers to an explicit extras
        // routing — the operator's declared model recruitment is
        // higher-precedence than any heuristic.
        let r = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(
            pick_slot(&r, true, true, false, Some("bulk".into())),
            SlotTarget::Extra("bulk".into())
        );
    }

    #[test]
    fn no_extras_match_falls_through_to_speed_routing() {
        // Untagged request OR tagged request whose model_id missed
        // the extras lookup → existing Speed/code rules apply
        // unchanged. Locks the back-compat invariant.
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, true, false, false, None), SlotTarget::Primary);
    }
}

#[cfg(test)]
mod eviction_tests {
    //! Exercises the LRU eviction selection algorithm. Pure and
    //! lock-free — runs without loading any real llama.cpp model.
    use super::{pick_evictions, EvictionCandidate, EvictionPlan};

    fn cand(name: &str, last_used_ms: u64, size_mb: u64) -> EvictionCandidate {
        EvictionCandidate {
            slot_name: name.into(),
            last_used_ms,
            size_bytes: size_mb * 1024 * 1024,
        }
    }

    #[test]
    fn fits_returns_no_eviction_needed() {
        // current 5 GB + new 3 GB = 8 GB ≤ 12 GB budget → Fits.
        let c = vec![cand("warm", 1000, 5 * 1024)];
        let plan = pick_evictions(&c, 5 * 1024 * 1024 * 1024, 3 * 1024 * 1024 * 1024, 12 * 1024 * 1024 * 1024);
        assert_eq!(plan, EvictionPlan::Fits);
    }

    #[test]
    fn evicts_coldest_slot_first() {
        // Two candidates: "old" used at t=100, "fresh" used at t=999.
        // Need to free 1 MB. Algorithm picks the colder one ("old")
        // even though "fresh" alone would also free enough — the
        // ordering is the contract.
        let c = vec![
            cand("fresh", 999, 5),
            cand("old", 100, 5),
        ];
        // current 10 MB + new 4 MB = 14, budget 12 → need to free 2 MB
        let plan = pick_evictions(&c, 10 * 1024 * 1024, 4 * 1024 * 1024, 12 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["old".to_string()]);
            }
            other => panic!("expected Evict, got {other:?}"),
        }
    }

    #[test]
    fn evicts_multiple_when_one_isnt_enough() {
        // Three cold slots, all 1 MB. Need to free 3 MB. Walks LRU
        // order ("a" → "b" → "c"). Stops once enough freed.
        let c = vec![
            cand("c", 300, 1),
            cand("b", 200, 1),
            cand("a", 100, 1),
        ];
        // current 3 MB + new 5 MB = 8, budget 5 → need 3 MB
        let plan = pick_evictions(&c, 3 * 1024 * 1024, 5 * 1024 * 1024, 5 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
            }
            other => panic!("expected Evict, got {other:?}"),
        }
    }

    #[test]
    fn stops_evicting_once_enough_freed() {
        // Multiple candidates available, but only the coldest's
        // capacity is needed. Stops early to preserve the warmer
        // slots.
        let c = vec![
            cand("a", 100, 10), // coldest, 10 MB — alone enough
            cand("b", 200, 10),
            cand("c", 300, 10),
        ];
        // current 30 MB + new 5 MB = 35, budget 30 → need 5 MB
        let plan = pick_evictions(&c, 30 * 1024 * 1024, 5 * 1024 * 1024, 30 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["a".to_string()]);
            }
            other => panic!("expected single eviction, got {other:?}"),
        }
    }

    #[test]
    fn insufficient_when_cold_capacity_too_small() {
        // Only 1 MB of cold inventory, but need to free 5 MB.
        // Algorithm reports the shortfall so the caller can
        // surface an error.
        let c = vec![cand("a", 100, 1)];
        // current 1 MB + new 10 MB = 11, budget 5 → need 6 MB but
        // cold total is only 1 MB.
        let plan = pick_evictions(&c, 1024 * 1024, 10 * 1024 * 1024, 5 * 1024 * 1024);
        match plan {
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                assert_eq!(need_to_free, 6 * 1024 * 1024);
                assert_eq!(cold_total, 1024 * 1024);
            }
            other => panic!("expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn empty_candidates_with_overflow_returns_insufficient() {
        // Edge case: budget would be exceeded but no cold slots
        // available (everything is busy in-flight). Caller can't
        // load.
        let c = vec![];
        let plan = pick_evictions(&c, 10 * 1024 * 1024, 5 * 1024 * 1024, 8 * 1024 * 1024);
        match plan {
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                assert_eq!(need_to_free, 7 * 1024 * 1024);
                assert_eq!(cold_total, 0);
            }
            other => panic!("expected Insufficient on empty cold, got {other:?}"),
        }
    }

    #[test]
    fn ties_break_deterministically_by_input_order() {
        // Two candidates with the same last_used. The algorithm
        // sorts by last_used; for equal keys, sort_by_key is
        // stable, so input order wins. Lock the contract — a
        // future change to unstable sort would surface here.
        let c = vec![
            cand("first", 100, 1),
            cand("second", 100, 1),
        ];
        // current 2 MB + new 1 MB = 3, budget 1 → need 2 MB.
        let plan = pick_evictions(&c, 2 * 1024 * 1024, 1024 * 1024, 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["first".to_string(), "second".to_string()]);
            }
            other => panic!("expected Evict on tie, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod clamp_tests {
    use super::clamp_max_tokens;

    #[test]
    fn defaults_to_remaining_context_when_unspecified() {
        // Caller didn't ask for a cap — give them the whole window.
        assert_eq!(clamp_max_tokens(None, 1000, 8192).unwrap(), 7192);
    }

    #[test]
    fn passes_through_when_request_fits() {
        assert_eq!(
            clamp_max_tokens(Some(2000), 1000, 8192).unwrap(),
            2000
        );
    }

    #[test]
    fn clamps_when_request_exceeds_headroom() {
        // Coding-agent worst case: opencode asks for 4096 on a tight
        // window. Pre-fix: returned Err. Post-fix: emits as much as
        // fits.
        assert_eq!(
            clamp_max_tokens(Some(4096), 30000, 32768).unwrap(),
            2768
        );
    }

    #[test]
    fn errs_only_when_prompt_alone_exhausts_context() {
        let err = clamp_max_tokens(Some(100), 8192, 8192).unwrap_err();
        // The wording is the user-visible error string; lock the
        // hint phrasing so it stays actionable.
        let msg = format!("{err}");
        assert!(msg.contains("Prompt too long"), "{msg}");
    }

    #[test]
    fn errs_when_prompt_is_strictly_oversized() {
        assert!(clamp_max_tokens(None, 9000, 8192).is_err());
    }
}

#[cfg(test)]
mod tool_call_parser_tests {
    use super::{parse_tool_calls_from_text, parse_tool_calls_with_errors};

    #[test]
    fn happy_path_single_call() {
        let text = r#"Sure, I'll check the weather.
<tool_call>{"name": "get_weather", "arguments": {"city": "SF"}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        // Object arguments serialize with no spaces — lock the shape.
        assert_eq!(calls[0].arguments, r#"{"city":"SF"}"#);
    }

    #[test]
    fn multiple_calls_in_one_response() {
        let text = r#"<tool_call>{"name":"a","arguments":{"x":1}}</tool_call>
then <tool_call>{"name":"b","arguments":{"y":"hi"}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn string_arguments_preserved_as_is() {
        // Some models (including older Qwen variants) stringify the
        // arguments object. We round-trip that form verbatim.
        let text = r#"<tool_call>{"name":"f","arguments":"{\"x\":1}"}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, r#"{"x":1}"#);
    }

    #[test]
    fn missing_arguments_defaults_to_empty_object() {
        let text = r#"<tool_call>{"name":"noargs"}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "noargs");
        assert_eq!(calls[0].arguments, "{}");
    }

    #[test]
    fn malformed_json_is_skipped_silently() {
        let text = r#"<tool_call>{"name": "truncated", "arguments": {</tool_call>
and <tool_call>{"name":"ok","arguments":{}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ok");
    }

    #[test]
    fn unterminated_tag_stops_scanning() {
        // No closing </tool_call> → parser stops rather than munching
        // arbitrary text as JSON. Anything before the open tag is
        // already ignored.
        let text = r#"preamble <tool_call>{"name":"never_closed""#;
        assert!(parse_tool_calls_from_text(text).is_empty());
    }

    #[test]
    fn no_tool_call_tags_returns_empty() {
        let text = "plain reply, no tools here";
        assert!(parse_tool_calls_from_text(text).is_empty());
    }

    #[test]
    fn error_variant_reports_malformed_bodies() {
        let text = r#"<tool_call>{ not json }</tool_call>
<tool_call>{"name":"ok","arguments":{}}</tool_call>
<tool_call>{"missing_name":true}</tool_call>"#;
        let (calls, errors) = parse_tool_calls_with_errors(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ok");
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("not json"));
        assert!(errors[1].contains("missing_name"));
    }

    #[test]
    fn whitespace_inside_block_tolerated() {
        let text = "<tool_call>\n  {\"name\": \"spaced\", \"arguments\": {}}  \n</tool_call>";
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "spaced");
    }
}
