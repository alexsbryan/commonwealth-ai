use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use sovereign_core::error::Error;
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
}

impl ModelSlot {
    fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        context_size: u32,
        n_gpu_layers: u32,
    ) -> Result<Self> {
        let model_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load model: {e}")))?;

        let model_id = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let model = Arc::new(model);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(context_size))
            .with_n_batch(512)
            .with_n_ubatch(512);

        let ctx = unsafe {
            let model_ref: &'static LlamaModel =
                &*(Arc::as_ptr(&model) as *const LlamaModel);
            model_ref
                .new_context(backend, ctx_params)
                .map_err(|e| Error::Inference(format!("Failed to create context: {e}")))?
        };

        eprintln!(
            "Slot loaded: {} ({} params, {} layers, {}MB)",
            model_id,
            model.n_params(),
            model.n_layer(),
            model.size() / (1024 * 1024),
        );

        Ok(Self {
            model: model.clone(),
            context: Mutex::new(SlotContext {
                ctx,
                _model: model,
            }),
            model_id,
        })
    }

    fn generate_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        request: &CompletionRequest,
    ) -> Result<(String, usize)> {
        let full_prompt = format_prompt(model, request)?;

        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let max_tokens = request.max_tokens.unwrap_or(1024);

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;

        // Guard: reject prompts that exceed the model's context or batch limits.
        if tokens.len() > n_batch {
            return Err(Error::Inference(format!(
                "Prompt too long: {} tokens exceeds batch size of {}. \
                 Try a shorter message or reduce conversation history.",
                tokens.len(),
                n_batch
            )));
        }
        if tokens.len() + max_tokens > n_ctx {
            return Err(Error::Inference(format!(
                "Prompt too long: {} tokens + {} max response tokens exceeds \
                 context window of {}. Try a shorter message.",
                tokens.len(),
                max_tokens,
                n_ctx
            )));
        }

        let mut batch = LlamaBatch::new(n_batch, 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;

        let mut sampler = build_sampler(request.temperature);
        let mut output = String::new();
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        while n_generated < max_tokens {
            let token = sampler.sample(ctx, -1);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                output.push_str(&piece);
            }

            n_generated += 1;

            batch.clear();
            let pos = (tokens.len() + n_generated - 1) as i32;
            batch
                .add(token, pos, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;
        }

        ctx.clear_kv_cache();
        let total_tokens = tokens.len() + n_generated;
        Ok((output, total_tokens))
    }

    fn generate_stream_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<Result<String>>,
    ) -> Result<()> {
        let full_prompt = format_prompt(model, request)?;

        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let max_tokens = request.max_tokens.unwrap_or(1024);

        let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;

        let mut sampler = build_sampler(request.temperature);
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        while n_generated < max_tokens {
            let token = sampler.sample(ctx, -1);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                if tx.blocking_send(Ok(piece)).is_err() {
                    break;
                }
            }

            n_generated += 1;

            batch.clear();
            batch
                .add(token, (tokens.len() + n_generated - 1) as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;
        }

        ctx.clear_kv_cache();
        Ok(())
    }
}

// ─── EmbeddedLlamaCpp (dual-slot) ──────────────────────────────

/// Configuration for loading the inference provider.
pub struct InferenceConfig {
    pub fast_model: PathBuf,
    pub primary_model: Option<PathBuf>,
    pub context_size: u32,
    pub gpu_layers: Option<u32>,
}

/// Dual-slot inference provider wrapping llama.cpp via FFI.
///
/// - **Fast slot**: always loaded, small model for classification and simple queries.
/// - **Primary slot**: loaded on-demand for deep reasoning, unloaded after idle timeout.
pub struct EmbeddedLlamaCpp {
    #[allow(dead_code)]
    backend: Arc<LlamaBackend>,
    fast: Arc<ModelSlot>,
    primary: Arc<Mutex<Option<ModelSlot>>>,
    primary_path: Option<PathBuf>,
    primary_ctx_size: u32,
    gpu_layers: u32,
    primary_backend: Arc<LlamaBackend>,
    last_primary_use: Arc<Mutex<Option<Instant>>>,
    hardware: HardwareProfile,
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

    /// Load with separate fast and primary models.
    pub fn load_dual(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        let hardware = HardwareProfile::detect();
        let n_gpu_layers = gpu_layers.unwrap_or(hardware.recommended_gpu_layers);

        let backend = LlamaBackend::init()
            .map_err(|e| Error::Inference(format!("Failed to init llama backend: {e}")))?;
        let backend = Arc::new(backend);

        eprintln!("Loading fast slot...");
        let fast = Arc::new(ModelSlot::load(
            &backend,
            fast_model_path,
            context_size,
            n_gpu_layers,
        )?);

        Ok(Self {
            backend: Arc::clone(&backend),
            fast,
            primary: Arc::new(Mutex::new(None)),
            primary_path: primary_model_path.map(|p| p.to_path_buf()),
            primary_ctx_size: context_size,
            gpu_layers: n_gpu_layers,
            primary_backend: backend,
            last_primary_use: Arc::new(Mutex::new(None)),
            hardware,
        })
    }

    /// Ensure the primary slot is loaded, returning a reference.
    /// If no separate primary model path was provided, falls back to the fast slot.
    async fn ensure_primary(&self) -> Result<Arc<ModelSlot>> {
        let primary_path = match &self.primary_path {
            Some(p) => p.clone(),
            None => return Ok(Arc::clone(&self.fast)), // Single-model mode.
        };

        let mut primary = self.primary.lock().await;
        if primary.is_none() {
            eprintln!("Loading primary slot...");
            let slot = ModelSlot::load(
                &self.primary_backend,
                &primary_path,
                self.primary_ctx_size,
                self.gpu_layers,
            )?;
            *primary = Some(slot);
        }

        *self.last_primary_use.lock().await = Some(Instant::now());

        // This is safe because we hold the mutex and the slot is Some.
        // We can't return a reference into the MutexGuard, so we'll
        // run generation inline while we have the lock. But for the
        // InferenceProvider trait, we need to dispatch differently.
        // Instead, fall back to the fast slot when primary has no separate path.
        // When primary IS loaded, we'll use it via the blocking task.
        Ok(Arc::clone(&self.fast)) // placeholder, actual dispatch below
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
                        eprintln!("Primary slot idle for {}s, unloading.", timeout_secs);
                        *primary = None;
                        *this.last_primary_use.lock().await = None;
                    }
                }
            }
        });
    }

    /// Select the appropriate slot for a request.
    fn select_slot_for_speed(&self, speed: Speed) -> bool {
        // Returns true if we should use the primary slot.
        match speed {
            Speed::Fast => false,
            Speed::Medium | Speed::Slow => self.primary_path.is_some(),
        }
    }
}

#[async_trait]
impl InferenceProvider for EmbeddedLlamaCpp {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let use_primary = self.select_slot_for_speed(request.preferred_speed);

        if use_primary {
            // Load primary if needed.
            let primary_path = self.primary_path.clone().unwrap();
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let request = request.clone();

            tokio::task::spawn_blocking(move || {
                let start = Instant::now();

                // Ensure primary is loaded.
                let mut primary = primary_lock.blocking_lock();
                if primary.is_none() {
                    eprintln!("Loading primary slot...");
                    let slot = ModelSlot::load(&backend, &primary_path, ctx_size, gpu_layers)?;
                    *primary = Some(slot);
                }

                let slot = primary.as_ref().unwrap();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &mut ctx_lock.ctx, &request)
                }));

                let (text, tokens_used) = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;

                *last_use.blocking_lock() = Some(Instant::now());

                Ok(CompletionResponse {
                    text,
                    tokens_used,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                })
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?
        } else {
            // Use fast slot.
            let slot = Arc::clone(&self.fast);
            let request = request.clone();

            tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &mut ctx_lock.ctx, &request)
                }));

                let (text, tokens_used) = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;

                Ok(CompletionResponse {
                    text,
                    tokens_used,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                })
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?
        }
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        // Streaming always uses the fast slot for simplicity in Phase 4.
        let slot = Arc::clone(&self.fast);
        let request = request.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(32);

        tokio::task::spawn_blocking(move || {
            let mut ctx_lock = slot.context.blocking_lock();
            let result =
                ModelSlot::generate_stream_sync(&slot.model, &mut ctx_lock.ctx, &request, &tx);
            if let Err(e) = result {
                let _ = tx.blocking_send(Err(e));
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented(
            "Embedding not available yet (Phase 6)".to_string(),
        ))
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
}

// ─── Shared helpers ────────────────────────────────────────────

fn format_prompt(model: &LlamaModel, request: &CompletionRequest) -> Result<String> {
    if let Ok(template) = model.chat_template(None) {
        let mut messages = Vec::new();
        if let Some(sys) = &request.system_message {
            messages.push(
                LlamaChatMessage::new("system".to_string(), sys.clone())
                    .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
            );
        }
        messages.push(
            LlamaChatMessage::new("user".to_string(), request.prompt.clone())
                .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
        );
        if let Ok(formatted) = model.apply_chat_template(&template, &messages, true) {
            return Ok(formatted);
        }
    }

    Ok(match &request.system_message {
        Some(sys) => format!("{sys}\n\n{}", request.prompt),
        None => request.prompt.clone(),
    })
}

fn build_sampler(temperature: Option<f32>) -> LlamaSampler {
    let temp = temperature.unwrap_or(0.7);
    if temp < 0.01 {
        LlamaSampler::chain_simple([
            LlamaSampler::penalties(256, 1.3, 0.1, 0.1),
            LlamaSampler::greedy(),
        ])
    } else {
        LlamaSampler::chain_simple([
            LlamaSampler::penalties(256, 1.3, 0.1, 0.1),
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::temp(temp),
            LlamaSampler::dist(rand_seed()),
        ])
    }
}

fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
}
