use serde::{Deserialize, Serialize};

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
pub enum ModelFamily {
    Qwen3,
    Qwen35,
    Qwen3Embedding,
    Gemma3,
    Llama3,
    Phi4,
    /// Always-on thinking, cannot be disabled.
    Phi4Reasoning,
    SmolLM3,
    Unknown,
}

impl Default for ModelFamily {
    fn default() -> Self {
        ModelFamily::Unknown
    }
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

    /// Sampling defaults used when the caller does not specify overrides.
    /// The Fast slot always receives temperature=0.0, top_k=1 regardless
    /// of these defaults — those are slot-level, not family-level.
    pub default_temperature: f32,
    pub default_top_k: Option<u32>,
    pub default_top_p: f32,
    pub default_presence_penalty: f32,

    /// Embedding-specific configuration. None for generative-only families.
    /// Populated only when the slot is the Embed slot.
    pub embed: Option<EmbedQuirks>,
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
    /// Required for Qwen3-Embedding; must be false for nomic/mxbai.
    pub append_eos_token: bool,
    /// Output vector dimensionality. Used to validate index compatibility
    /// at open time and to reject mismatched BYOM swaps at startup.
    pub output_dimensions: usize,
}

/// How token embeddings are pooled into a sequence embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PoolingStrategy {
    /// Take the last non-padding token's hidden state.
    /// Required for Qwen3-Embedding.
    Last,
    /// Average all non-padding token hidden states.
    /// Used by nomic-embed-text and mxbai-embed-large.
    Mean,
    /// Take the [CLS] token hidden state.
    /// Used by BERT-style models.
    Cls,
}

/// How L2 normalisation is applied to the raw embedding vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NormalizationStrategy {
    /// llama-server handles L2 normalisation via --embd-normalize.
    /// Safe default for most models in remote/server mode.
    /// In in-process mode (EmbeddedLlamaCpp), the application normalises
    /// regardless of this setting.
    Server,
    /// The application must L2-normalise the raw vector before returning.
    /// Required for Qwen3-Embedding when llama-server cannot handle it.
    Application,
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
                default_temperature:      0.6,
                default_top_k:            Some(20),
                default_top_p:            0.95,
                default_presence_penalty: 1.5,
                embed: None,
            },

            // Qwen3.5 uses the same thinking tokens as Qwen3 but a
            // slightly higher temperature reflects community-observed
            // behaviour with the new Gated DeltaNet architecture.
            ModelFamily::Qwen35 => ModelQuirks {
                thinking: ThinkingControl::SystemPromptToken {
                    enable:  "/think".into(),
                    disable: "/no_think".into(),
                },
                default_temperature:      0.7,
                default_top_k:            Some(20),
                default_top_p:            0.95,
                default_presence_penalty: 1.5,
                embed: None,
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
            },

            ModelFamily::Gemma3 => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      1.0,
                default_top_k:            Some(64),
                default_top_p:            0.95,
                default_presence_penalty: 0.0,
                embed: None,
            },

            ModelFamily::Llama3 => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      0.6,
                default_top_k:            Option::None,
                default_top_p:            0.9,
                default_presence_penalty: 0.0,
                embed: None,
            },

            ModelFamily::Phi4 => ModelQuirks {
                thinking: ThinkingControl::None,
                default_temperature:      0.7,
                default_top_k:            Option::None,
                default_top_p:            1.0,
                default_presence_penalty: 0.0,
                embed: None,
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
                embed: None,
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
                embed: None,
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
                embed: None,
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
