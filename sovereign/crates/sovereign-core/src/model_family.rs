use serde::{Deserialize, Serialize};

// Re-export embed-strategy types from oicp-types so that all crates that
// depend on sovereign-core continue to find them at their original path.
pub use oicp_types::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};

/// Identifies the behavioural family of a loaded model.
///
/// A new family is added here when it has at least one default quirk that
/// differs from all existing families. If a new model fits an existing
/// family's defaults exactly, it does not need a new variant — it just
/// sets `family = "Qwen3"` (or whichever) in models.toml.
///
/// `Unknown` is the safe fallback: no thinking injection, conservative
/// sampling, server-side normalisation assumed for embedding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[derive(Default)]
pub enum ModelFamily {
    Qwen3,
    Qwen35,
    Qwen3Embedding,
    Gemma3,
    /// Gemma 4 (E2B / E4B / E27B). Same chat template family as
    /// Gemma 3 — `{%- macro format_parameters() -%}` Jinja2
    /// templates that require the minja renderer (tier-2 path in
    /// `build_chat_prompt`). Sampling defaults match Gemma 3.
    Gemma4,
    Llama3,
    Phi4,
    /// Always-on thinking, cannot be disabled.
    Phi4Reasoning,
    SmolLM3,
    /// Cross-encoder reranker — covers BERT-based BGE rerankers
    /// (bge-reranker-v2-m3) and Qwen3-based Jina rerankers
    /// (jina-reranker-v3). Both expose the same llama.cpp interface
    /// when loaded with `pooling_type = LLAMA_POOLING_TYPE_RANK`:
    /// feed a (query, doc) pair through `llama_decode`, read a
    /// single scalar relevance logit from the embedding output.
    /// Neither chat-capable nor embedding-capable — the scalar is
    /// the only output, consumed by `CorpusIndex::search_with_rerank`.
    Reranker,
    #[default]
    Unknown,
}


/// All family-specific runtime behaviour in one place.
///
/// Every field has a clear owner: the model manifest sets the base values
/// via `ModelFamily::default_quirks()`, and the optional `quirks_override`
/// section in models.toml can overwrite any field. Nothing outside this
/// struct should make family-conditional decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQuirks {
    /// How thinking mode is controlled for this family, if at all.
    pub thinking: ThinkingControl,

    /// Sampling defaults — **think** profile (thinking-general).
    /// Used when the caller has thinking enabled AND no tools are
    /// present in the request (no coding signal). The Fast slot
    /// always receives temperature=0.0, top_k=1 regardless of
    /// these defaults — those are slot-level, not family-level.
    pub default_temperature: f32,
    pub default_top_k: Option<u32>,
    pub default_top_p: f32,
    pub default_presence_penalty: f32,
    /// Min-p threshold. Qwen card recommends 0.0 (disabled). Older
    /// llama-cpp tradition was 0.05. Defaults to 0.05 for backwards
    /// compatibility on families that haven't published a value;
    /// per-family overrides land here.
    #[serde(default = "default_min_p_compat")]
    pub default_min_p: f32,
    /// Repetition penalty applied to the `LlamaSampler::penalties`
    /// stage. Qwen card recommends 1.0 (disabled — relies on DRY
    /// + presence_penalty instead). Older llama-cpp tradition was
    /// 1.15.
    #[serde(default = "default_repetition_penalty_compat")]
    pub default_repetition_penalty: f32,
    /// Frequency penalty applied to the `LlamaSampler::penalties`
    /// stage. Qwen card recommends 0.0. Older tradition was 0.1.
    #[serde(default = "default_frequency_penalty_compat")]
    pub default_frequency_penalty: f32,

    /// **Instruct** profile (non-thinking). Used when the request
    /// sets `enable_thinking: false` regardless of whether tools
    /// are present (codex CLI traffic, atlas Phase 1, etc). Each
    /// field falls back to its `default_*` sibling when `None`.
    ///
    /// Why a separate profile: model cards (Qwen 3.6) publish
    /// substantively different recommendations per mode — thinking
    /// uses higher temperature + wider top_p than instruct does.
    /// Forcing one profile across both wastes capability.
    #[serde(default)]
    pub instruct_temperature: Option<f32>,
    #[serde(default)]
    pub instruct_top_k: Option<u32>,
    #[serde(default)]
    pub instruct_top_p: Option<f32>,
    #[serde(default)]
    pub instruct_presence_penalty: Option<f32>,

    /// **Code** profile (thinking + tools). Used when the request
    /// has both `enable_thinking: true` AND tools present — a
    /// coding-with-reasoning task. Qwen 3.6 recommends a tighter
    /// temperature (0.6) and zero presence-penalty for this mode
    /// vs. thinking-general. Each field falls back to its
    /// `default_*` sibling when `None`.
    #[serde(default)]
    pub code_temperature: Option<f32>,
    #[serde(default)]
    pub code_top_k: Option<u32>,
    #[serde(default)]
    pub code_top_p: Option<f32>,
    #[serde(default)]
    pub code_presence_penalty: Option<f32>,

    /// Embedding-specific configuration. None for generative-only families.
    /// Populated only when the slot is the Embed slot.
    pub embed: Option<EmbedQuirks>,

    /// Reranker-specific configuration. None for everything except
    /// cross-encoder reranker families (`BgeReranker`). Populated
    /// only when the slot is a Rerank slot.
    pub rerank: Option<RerankQuirks>,

    /// True iff the family's gguf carries recurrent layers (Mamba /
    /// Gated DeltaNet / RWKV / SSM). Drives the prefix-cache safety
    /// gate in the inference path: `clear_kv_cache_seq` doesn't
    /// rewind recurrent hidden state, so partial-keep prefix caching
    /// is unsafe and the slot must always full-clear before a new
    /// prompt. Default `false` for attention-only families.
    ///
    /// 2026-05-20: replaces the `is_recurrent_arch_by_name` substring
    /// heuristic that pattern-matched "qwen3.5" / "qwen3.6" / "qwopus"
    /// in the gguf file name. The gguf `general.architecture`
    /// metadata is still the primary signal at slot load time (read
    /// by `is_recurrent_arch`); this quirks flag is the fallback the
    /// runtime consults when the metadata is empty.
    #[serde(default)]
    pub has_recurrent_layers: bool,
}

/// Cross-encoder rerank configuration. Sits alongside `EmbedQuirks`
/// — same shape (model-specific defaults overridable in `models.toml`),
/// different responsibilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankQuirks {
    /// Max input tokens the encoder will accept. bge-reranker-v2-m3
    /// supports 8192; older bge-reranker-large is 512. Sequences
    /// longer than this are truncated *before* tokenization to keep
    /// the doc tail rather than the query tail.
    pub max_context: usize,
    /// Hard cap on how many candidate docs the reranker batches in a
    /// single forward pass. Above this the runtime chunks the batch.
    /// Bounded by GPU memory pressure — 50 is a safe default for a
    /// 568M-param BGE reranker on Vulkan/ROCm at 8192 ctx.
    pub max_batch: usize,
}

/// Controls how thinking mode is enabled or disabled for a model family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThinkingControl {
    /// Append a token to the system prompt to enable or disable thinking.
    /// Qwen3 and Qwen3.5: enable = "/think", disable = "/no_think".
    /// SmolLM3 uses the same convention.
    SystemPromptToken { enable: String, disable: String },

    /// Thinking is structurally always on. Cannot be disabled.
    /// Used for Phi-4-reasoning variants.
    AlwaysOn,

    /// Model has no thinking mode. No token injection performed.
    /// Used for Gemma3, Llama3, base Phi-4.
    None,
}

/// Embedding-specific configuration for the Embed slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedQuirks {
    pub pooling: PoolingStrategy,
    pub normalize: NormalizationStrategy,
    /// Prepended to query-side inputs at inference time.
    /// Empty string = no instruction prefix.
    pub query_instruction: String,
    /// Prepended to document-side inputs at ingestion time.
    pub document_instruction: String,
    /// Whether to append the model's EOS token to every input.
    /// Required for Qwen3-Embedding; must be false for Qwen3-Embedding and similar embedders.
    pub append_eos_token: bool,
    /// Output vector dimensionality. Used to validate index compatibility
    /// at open time and to reject mismatched BYOM swaps at startup.
    pub output_dimensions: usize,
}

// Backwards-compatible defaults for the three sampler-stage params
// we just added to ModelQuirks. Used by serde when an existing
// `models.toml` quirks_override doesn't mention them.
fn default_min_p_compat() -> f32 {
    0.05
}
fn default_repetition_penalty_compat() -> f32 {
    1.15
}
fn default_frequency_penalty_compat() -> f32 {
    0.1
}

impl ModelFamily {
    /// Returns the canonical quirks for this family.
    /// Callers apply quirks_override fields on top of this after parsing.
    pub fn default_quirks(&self) -> ModelQuirks {
        match self {
            ModelFamily::Qwen3 => ModelQuirks {
                thinking: ThinkingControl::SystemPromptToken {
                    enable:  "/think".into(),
                    disable: "/no_think".into(),
                },
                // Think profile (thinking-general).
                default_temperature:      1.0,
                default_top_k:            Some(20),
                default_top_p:            0.95,
                default_presence_penalty: 1.5,
                // Qwen card recommends these across all three modes.
                default_min_p:             0.0,
                default_repetition_penalty: 1.0,
                default_frequency_penalty:  0.0,
                // Instruct profile (thinking off).
                instruct_temperature:      Some(0.7),
                instruct_top_k:            Some(20),
                instruct_top_p:            Some(0.80),
                instruct_presence_penalty: Some(1.5),
                // Code profile (thinking + tools).
                code_temperature:      Some(0.6),
                code_top_k:            Some(20),
                code_top_p:            Some(0.95),
                code_presence_penalty: Some(0.0),
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },

            // Qwen3.5 / Qwen3.6 / Qwopus3.5 use Gated DeltaNet — a
            // recurrent component in the attention path. Same thinking
            // tokens as Qwen3 but slightly higher temperature reflects
            // community-observed behaviour with the new architecture.
            // The `has_recurrent_layers` flag drives the prefix-cache
            // safety gate in `generate_sync` (partial-keep is unsafe
            // on recurrent layers; full-clear required).
            ModelFamily::Qwen35 => ModelQuirks {
                thinking: ThinkingControl::SystemPromptToken {
                    enable:  "/think".into(),
                    disable: "/no_think".into(),
                },
                default_temperature:      0.7,
                default_top_k:            Some(20),
                default_top_p:            0.95,
                default_presence_penalty: 1.5,
                default_min_p:              0.0,
                default_repetition_penalty: 1.0,
                default_frequency_penalty:  0.0,
                instruct_temperature:      Some(0.7),
                instruct_top_k:            Some(20),
                instruct_top_p:            Some(0.80),
                instruct_presence_penalty: Some(1.5),
                code_temperature:      Some(0.6),
                code_top_k:            Some(20),
                code_top_p:            Some(0.95),
                code_presence_penalty: Some(0.0),
                embed: None,
                rerank: None,
                has_recurrent_layers: true,
            },

            // Qwen3-Embedding uses last-token pooling and requires the
            // application to normalise. llama-server's --embd-normalize
            // flag is not supported for this family as of llama.cpp b5xxx.
            // output_dimensions is overridden per-size in models.toml:
            //   0.6B → 1024, 4B → 2560, 8B → 4096.
            ModelFamily::Qwen3Embedding => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      0.0,
                default_top_k:            Option::None,
                default_top_p:            1.0,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: Some(EmbedQuirks {
                    pooling:              PoolingStrategy::Last,
                    normalize:            NormalizationStrategy::Application,
                    query_instruction:    "Instruct: Given a search query, retrieve \
                                          relevant passages that answer the query\nQuery: "
                                          .into(),
                    document_instruction: String::new(),
                    append_eos_token:     true,
                    output_dimensions:    1024, // overridden in manifest for 4B (2560) / 8B (4096)
                }),
                rerank: None,
                has_recurrent_layers: false,
            },

            ModelFamily::Gemma3 => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      1.0,
                default_top_k:            Some(64),
                default_top_p:            0.95,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },

            // Gemma 4 inherits Gemma 3's quirks — same chat-template
            // family (Jinja2 macros), same sampling defaults
            // (Google's recommended decode params for instruct
            // tuning didn't change between 3 and 4).
            ModelFamily::Gemma4 => ModelQuirks {
                thinking: ThinkingControl::None,
                // Default profile uses the model card's universal
                // recommendation (T=1.0, top_p=0.95, top_k=64).
                default_temperature:      1.0,
                default_top_k:            Some(64),
                default_top_p:            0.95,
                default_presence_penalty: 0.0,
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                // Per-mode tuning beyond the card. The card publishes
                // one universal T=1.0 but in practice constrained
                // tool / instruct work benefits from a tighter
                // distribution. Observed 2026-05-19 on gemma-4-26B-A4B-it
                // at the cognitive bank: T=1.0 left positional bias
                // dominant on multi-choice and let whitespace tokens
                // win on calibration. Tighter T trades distribution
                // breadth for adherence to the schema/argument.
                //
                // Instruct = non-thinking general work (the cognitive
                // bank's mode). Tighten T enough that the model's
                // argument-content beats positional bias, but stay
                // higher than greedy so multi-modal choices have
                // sampling latitude.
                instruct_temperature:      Some(0.7),
                instruct_top_k:            Some(50),
                instruct_top_p:            Some(0.95),
                instruct_presence_penalty: Some(0.0),
                // Code = composing structured emission. Tightest T —
                // schema-constrained output benefits from low T to
                // avoid sampling drift inside string bodies, JSON
                // escape sequences, and TOML body lines.
                code_temperature:      Some(0.4),
                code_top_k:            Some(40),
                code_top_p:            Some(0.95),
                code_presence_penalty: Some(0.0),
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },

            ModelFamily::Llama3 => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      0.6,
                default_top_k:            Option::None,
                default_top_p:            0.9,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },

            ModelFamily::Phi4 => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      0.7,
                default_top_k:            Option::None,
                default_top_p:            1.0,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },

            // Phi-4-reasoning cannot have thinking disabled. Attempting
            // to suppress it produces degraded output rather than a clean
            // non-thinking response. The Planner and Router must account
            // for this: all Primary calls will include a thinking block.
            ModelFamily::Phi4Reasoning => ModelQuirks {
                thinking: ThinkingControl::AlwaysOn,
                default_temperature:      0.8,
                default_top_k:            Option::None,
                default_top_p:            0.95,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },

            // Cross-encoder reranker. Sampling defaults are placebos
            // — the rerank path never decodes generative tokens, it
            // reads the rank logit straight out of the encoder. The
            // shape that matters lives in `rerank.max_context` and
            // `rerank.max_batch`. Default 8192-token context matches
            // jina-reranker-v3 + bge-reranker-v2-m3; smaller bge
            // variants (512-ctx) override via `quirks_override`.
            ModelFamily::Reranker => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      0.0,
                default_top_k:            Option::None,
                default_top_p:            1.0,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: None,
                rerank: Some(RerankQuirks {
                    max_context: 8192,
                    max_batch:   50,
                }),
                has_recurrent_layers: false,
            },

            // SmolLM3 uses the same thinking token convention as Qwen3,
            // established by HuggingFace in its post-training alignment.
            ModelFamily::SmolLM3 => ModelQuirks {
                thinking: ThinkingControl::SystemPromptToken {
                    enable:  "/think".into(),
                    disable: "/no_think".into(),
                },
                default_temperature:      0.7,
                default_top_k:            Option::None,
                default_top_p:            0.9,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },

            // Safe conservative defaults. No thinking injection.
            // Users bringing an unknown family get this until they
            // specify a quirks_override in models.toml.
            ModelFamily::Unknown => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      0.7,
                default_top_k:            Option::None,
                default_top_p:            0.9,
                default_presence_penalty: 0.0,
                // llama-cpp tradition defaults for sampler-stage
                // params not on this family's card.
                default_min_p:              0.05,
                default_repetition_penalty: 1.15,
                default_frequency_penalty:  0.1,
                instruct_temperature:      None,
                instruct_top_k:            None,
                instruct_top_p:            None,
                instruct_presence_penalty: None,
                code_temperature:      None,
                code_top_k:            None,
                code_top_p:            None,
                code_presence_penalty: None,
                embed: None,
                rerank: None,
                has_recurrent_layers: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwen3_thinking_tokens() {
        let quirks = ModelFamily::Qwen3.default_quirks();
        match quirks.thinking {
            ThinkingControl::SystemPromptToken { ref enable, ref disable } => {
                assert_eq!(enable, "/think");
                assert_eq!(disable, "/no_think");
            }
            _ => panic!("Expected SystemPromptToken for Qwen3"),
        }
    }

    #[test]
    fn qwen35_thinking_tokens() {
        let quirks = ModelFamily::Qwen35.default_quirks();
        match quirks.thinking {
            ThinkingControl::SystemPromptToken { ref enable, ref disable } => {
                assert_eq!(enable, "/think");
                assert_eq!(disable, "/no_think");
            }
            _ => panic!("Expected SystemPromptToken for Qwen35"),
        }
    }

    #[test]
    fn qwen3_embedding_quirks() {
        let quirks = ModelFamily::Qwen3Embedding.default_quirks();
        let eq = quirks.embed.expect("Qwen3Embedding must have EmbedQuirks");
        assert!(matches!(eq.pooling, PoolingStrategy::Last));
        assert!(eq.append_eos_token);
        assert_eq!(eq.output_dimensions, 1024);
    }

    #[test]
    fn phi4_reasoning_always_on() {
        let quirks = ModelFamily::Phi4Reasoning.default_quirks();
        assert!(matches!(quirks.thinking, ThinkingControl::AlwaysOn));
    }

    #[test]
    fn unknown_no_thinking() {
        let quirks = ModelFamily::Unknown.default_quirks();
        assert!(matches!(quirks.thinking, ThinkingControl::None));
    }
}
