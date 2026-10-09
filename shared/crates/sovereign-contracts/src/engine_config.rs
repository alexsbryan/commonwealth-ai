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

    /// Does this engine read its chat slots from `[models]`? Every engine
    /// but a remote one: its chat model is the vendor's, so its config may
    /// carry no `[models]` at all (`svrn setup --hosted` writes none).
    pub fn needs_models(&self) -> bool {
        !matches!(self, Self::Remote)
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

/// How a host is asked for output that matches a JSON Schema. A property of
/// the HOST, not of the request: the request carries the schema, and the
/// provider spells it the way its host accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StructuredOutputMode {
    /// `response_format: {type: "json_schema", json_schema: {...}}`. The host
    /// enforces the schema (a Commonwealth daemon, OpenAI).
    #[default]
    JsonSchema,
    /// `response_format: {type: "json_object"}`: valid JSON, schema not
    /// enforced. The OICP-compliant spelling for a host that advertises
    /// `constraint:json_object` and not `constraint:json_schema`.
    JsonObject,
    /// The schema as the one function's `parameters`, `tool_choice: "auto"`:
    /// the model may answer in text instead.
    ToolUseAuto,
    /// The schema as the one function's `parameters`, the call forced. On a
    /// host with no schema-enforcing `response_format` this is the strongest
    /// adherence there is: DeepSeek's chat API (2026-10-03) answers
    /// `json_schema` with 400, and under `json_object` 15 of 20 wessex-hoard
    /// Phase 1 chapters dropped the required `questions_raised`.
    ToolUseForced,
}

impl StructuredOutputMode {
    /// True when the answer comes back as a function call's arguments rather
    /// than as the message content.
    pub fn via_tool(self) -> bool {
        matches!(self, Self::ToolUseAuto | Self::ToolUseForced)
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
    /// A second model for fast turns (a cheaper or quicker one at the same
    /// endpoint). Unset, fast turns use `model_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_model_id: Option<String>,
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
    /// An embedding GGUF this process loads, so a hosted engine embeds on
    /// this machine with no second server: text is never sent to a third
    /// party to be embedded. Excludes `embed_endpoint`; the model id is the
    /// file stem, as under `llama`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_path: Option<std::path::PathBuf>,
    /// Fields merged last into every chat body `remote` sends: a vendor's own
    /// knobs, e.g. OpenRouter's `provider = { require_parameters = true }`,
    /// without which it may route a schema request to a backend that ignores
    /// the schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_params: Option<serde_json::Value>,
    /// How `remote` asks this host for schema-shaped output. Unset is
    /// `json-schema`, and a host that refuses that with a 400 is asked again
    /// as a forced function call and remembered (`oicp_client`'s
    /// `send_chat`). A host that IGNORES `response_format` answers 200 with
    /// unconstrained text, which nothing can learn from, so it must be told:
    /// Anthropic's OpenAI-compatible endpoint documents `response_format` as
    /// ignored, and `svrn setup --hosted anthropic` writes `tool-use-forced`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<StructuredOutputMode>,
    /// Who prepares `remote`'s embed inputs: the instruction prefix and the
    /// EOS an instruction-aware embedder was trained with. Unset is `server`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_inputs: Option<EmbedInputs>,
}

/// Who turns text into the input an embedding model expects. A property of
/// the embedding SERVER, like [`StructuredOutputMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedInputs {
    /// The server does: a Sovereign daemon's embed slot applies the family's
    /// instruction and EOS itself, so the client sends text as given.
    #[default]
    Server,
    /// The client does, because the server sends text to the model as
    /// given (llama-server, vLLM). Without it a Qwen3-Embedding query lands
    /// at cosine 0.84 from the daemon's vector for the same query
    /// (bench/lanes/engine-swap/embed_parity.py, 2026-10-09).
    Client,
}

fn default_engine_context_size() -> u32 {
    8192
}

impl EngineSection {
    /// The embed GGUF this engine loads in process when it holds no
    /// `[models]`: a remote engine's `embed_path`. `None` for every other
    /// engine, which embeds from `[models]`.
    pub fn own_embed_path(&self) -> Option<&std::path::Path> {
        match self.kind.needs_models() {
            true => None,
            false => self.embed_path.as_deref(),
        }
    }
}

impl Default for EngineSection {
    fn default() -> Self {
        Self {
            kind: EngineKind::default(),
            endpoint: None,
            api_key: None,
            model_id: None,
            fast_model_id: None,
            context_size: default_engine_context_size(),
            embed_model_id: None,
            embed_endpoint: None,
            embed_path: None,
            extra_params: None,
            structured_output: None,
            embed_inputs: None,
        }
    }
}
