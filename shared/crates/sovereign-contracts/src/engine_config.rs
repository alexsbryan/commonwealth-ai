// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[engine]` — WHICH inference engine serves this node.
//!
//! Its own module rather than another 135 lines on `setup_config.rs`,
//! which is already one of the workspace's oversized files (ARCH §3.1 —
//! a file that crossed its slack is a smell, and the fix is a split, not
//! a rebaseline).
//!
//! Lives in the contract crate because this is the vocabulary an
//! out-of-tree engine implementation lifts: it names an engine without
//! naming any engine's implementation. The factory that turns an
//! [`EngineKind`] into a running provider is
//! `sovereign_inference::engine_factory`, one layer up, because it has
//! to name `EmbeddedLlamaCpp` and this must not.

use serde::{Deserialize, Serialize};

/// Which inference engine serves this node.
///
/// The engines that ship in-tree are a closed set and get typed
/// variants; an engine built OUT of tree cannot be, so it is named as
/// [`EngineKind::Custom`] and resolved through
/// `sovereign_inference::engine_factory::register_engine` (ARCH §2, §4 —
/// closed sets are enums, open sets are registries).
///
/// Lives here rather than beside the factory because this is config
/// vocabulary, and `sovereign-contracts` is the dependency budget a
/// third-party engine implementation lifts. The factory is one layer up
/// (it must name `EmbeddedLlamaCpp`); this type must not be.
///
/// Serialized as a plain TOML string — `kind = "llama"` — so the config
/// surface reads naturally while the code side stays typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum EngineKind {
    /// In-process llama.cpp via `llama-cpp-4`. The default, and what
    /// every existing deployment runs.
    #[default]
    Llama,
    /// An OpenAI-compatible HTTP endpoint (`oicp-client`'s
    /// `RemoteApiProvider`). Holds no weights.
    Remote,
    /// An engine registered out of tree, selected by name.
    Custom(String),
}

impl EngineKind {
    /// The string that names this engine in `config.toml` and in
    /// diagnostics. Round-trips with `From<String>`.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Llama => "llama",
            Self::Remote => "remote",
            Self::Custom(name) => name,
        }
    }

    /// Is this one of the engines that ships in-tree? A `false` here
    /// means the name must be found in the registry, and refused if it
    /// is not — never defaulted.
    pub fn is_builtin(&self) -> bool {
        matches!(self, Self::Llama | Self::Remote)
    }
}

impl From<String> for EngineKind {
    fn from(s: String) -> Self {
        match s.as_str() {
            "llama" => Self::Llama,
            "remote" => Self::Remote,
            _ => Self::Custom(s),
        }
    }
}

impl From<EngineKind> for String {
    fn from(k: EngineKind) -> Self {
        k.as_str().to_string()
    }
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[engine]` — the engine selection, plus the fields a non-local engine
/// needs to reach its backend.
///
/// ```toml
/// [engine]
/// kind = "remote"
/// endpoint = "http://localhost:8000/v1"
/// model_id = "Qwen3.5-35B-A3B"
/// context_size = 32768
/// # Only when the embedding model is not the chat model:
/// embed_endpoint = "http://localhost:8001/v1"
/// embed_model_id = "BAAI/bge-m3"
/// ```
///
/// The `endpoint` / `api_key` / `model_id` / `context_size` fields are
/// inert under `kind = "llama"`, which takes its slots from `[models]`.
/// They are deliberately NOT defaulted for `remote`: a defaulted
/// endpoint makes a misconfigured node look healthy until its first
/// request (ARCH §18.3), so the factory refuses instead.
///
/// A custom engine receives this whole section verbatim and reads
/// whatever subset it needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSection {
    /// Which engine. Default [`EngineKind::Llama`].
    #[serde(default)]
    pub kind: EngineKind,
    /// Base URL including the `/v1` suffix. Required by `remote`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Bearer token, when the backend wants one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// The model name sent on the wire. Required by `remote` — it is
    /// routing, not a label, so a wrong value is a routing bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Context window to advertise for this engine. Only meaningful
    /// where the engine cannot report its own.
    #[serde(default = "default_engine_context_size")]
    pub context_size: u32,
    /// The model that serves EMBEDDINGS, when it is not the chat model.
    ///
    /// Unset, `remote` sends one model id to both `/chat/completions` and
    /// `/embeddings`. That is correct against a Sovereign daemon, which
    /// routes embeddings to its own embed slot whatever id it is handed.
    /// It is wrong against vLLM / SGLang / TGI, where a chat model on the
    /// embeddings route errors or returns a non-embedding shape — so a
    /// node that also retrieves must name its embedding model here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model_id: Option<String>,
    /// Base URL of the embedding server, when it is a DIFFERENT process
    /// from the chat server. Defaults to [`Self::endpoint`].
    ///
    /// vLLM, SGLang and TGI serve one model per process, so a node that
    /// chats and retrieves is usually talking to two ports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_endpoint: Option<String>,
}

fn default_engine_context_size() -> u32 {
    8192
}

impl Default for EngineSection {
    fn default() -> Self {
        Self {
            kind: EngineKind::default(),
            endpoint: None,
            api_key: None,
            model_id: None,
            context_size: default_engine_context_size(),
            embed_model_id: None,
            embed_endpoint: None,
        }
    }
}
