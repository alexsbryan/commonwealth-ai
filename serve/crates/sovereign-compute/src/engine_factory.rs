// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which inference engine serves this node — the one place that decides.
//!
//! Before this module, engine choice was not a decision the system could
//! express: five construction sites named `EmbeddedLlamaCpp` literally
//! (`sovereign-daemon/src/build/inference.rs`, `sovereign-server/src/main.rs` ×2,
//! the desktop's `builders/inference.rs`, `sovereign-compute`'s
//! `child_main.rs`), and `SetupConfig` had no key that could say
//! otherwise. The [`InferenceProvider`] seam was already real — the mesh
//! forwarder, the compute-child facade and `oicp-client`'s pure-HTTP
//! `RemoteApiProvider` all satisfy it without llama.cpp — but nothing
//! could *select* between them at run time.
//!
//! This module supplies the missing step and nothing else. It answers
//! "construct which engine?"; it does not answer "is this node allowed to
//! run that engine?" (admission belongs to the serving assembly,
//! `sovereign_compute::assembly`, with the `[compute]` containment rules)
//! and it does not start the router (the assembly does, before it asks this
//! module for the router's remote half).
//!
//! `kind = "llama"`, the in-process engine, was deleted (ENGINE_SWAP_PLAN
//! G2). The name still parses, so a config that names it is refused here
//! with the replacement, never read as an unknown custom engine.
//!
//! **Open set, so a registry — but the in-tree half stays an enum**
//! (ARCH §2, §4). The engines that ship here are a closed set and get
//! typed variants; an engine built out of tree cannot be, so
//! [`EngineKind::Custom`] resolves through [`register_engine`]. Rust has
//! no safe cross-crate ABI for `dyn Trait`, so a third-party engine is
//! always compiled into a binary that already depends on this crate —
//! which means registration at `main()` is the whole mechanism, not a
//! compromise. An unknown id names the ids that ARE registered rather
//! than falling back to a default (ARCH §18.3 — never silently
//! substitute).
//!
//! **Home.** `sovereign-compute`, which owns the serving assembly that
//! calls it (ENGINE_SWAP_PLAN G1); the remote engine is `oicp-client`'s.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use sovereign_contracts::embed_quirks::EmbedQuirks;
use sovereign_contracts::model_family::ModelFamily;
use sovereign_contracts::setup_config::{EmbedInputs, EngineSection, SetupConfig};
use sovereign_contracts::traits::InferenceProvider;

/// Re-exported so a host or an out-of-tree engine can name the selection
/// type without also depending on `sovereign-contracts` directly. The
/// definition lives one layer down, next to [`EngineSection`], because it
/// is config vocabulary — this crate cannot own it without making the
/// contract crate depend upward.
pub use sovereign_contracts::setup_config::EngineKind;

/// A constructed engine plus what its host still needs to know about it.
///
/// The `provider` is what every consumer above the seam receives.
pub struct BuiltEngine {
    /// The `dyn`-erased engine, installed and advertised by the host.
    pub provider: Arc<dyn InferenceProvider>,
    /// The manifest-resolved family of the embed slot, which drives
    /// app-side pooling and the document/query instruction prefixes, and
    /// is threaded into the mesh embed-model advertisement.
    /// [`ModelFamily::Unknown`] for engines that embed remotely.
    pub embed_family: ModelFamily,
}

impl std::fmt::Debug for BuiltEngine {
    /// Prints what the engine IS, not what it holds: the provider behind
    /// the `Arc` is not `Debug`, and its address would say nothing anyway.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltEngine")
            .field("embed_family", &self.embed_family)
            .field(
                "max_context_tokens",
                &self.provider.capabilities().max_context_tokens,
            )
            .finish()
    }
}

impl BuiltEngine {
    /// Wrap a provider that embeds remotely. The constructor an out-of-tree
    /// [`EngineBuilder`] uses.
    pub fn external(provider: Arc<dyn InferenceProvider>) -> Self {
        Self {
            provider,
            embed_family: ModelFamily::Unknown,
        }
    }
}

/// Constructs one out-of-tree engine from the operator's `[engine]`
/// section. Implement this in your own crate and hand it to
/// [`register_engine`] before the host builds its provider.
pub trait EngineBuilder: Send + Sync {
    /// Build the engine. The returned provider must satisfy every
    /// contract on [`InferenceProvider`] — in particular the terminal
    /// `Finish` frame on `complete_stream_with_finish`, which receivers
    /// rely on to distinguish a completed stream from a truncated one.
    ///
    /// Return `Err` with an operator-actionable message; the host prints
    /// it verbatim and refuses to start. Never substitute a different
    /// engine (ARCH §18.3).
    fn build(&self, section: &EngineSection) -> Result<BuiltEngine, String>;
}

type Registry = RwLock<BTreeMap<String, Arc<dyn EngineBuilder>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Register an out-of-tree engine under `name`, so `kind = "<name>"`
/// selects it. Call before the host constructs its provider — typically
/// first thing in `main()`.
///
/// A name already registered is REFUSED, not replaced: two builders
/// behind one id is two deciders for one question (ARCH §10.6), and the
/// loser would be chosen by registration order — which is startup
/// sequencing, not a decision anyone made.
///
/// `name` must not shadow a built-in (`llama`, `remote`, `llama-server`) — those parse to
/// typed variants before the registry is consulted, so a builder
/// registered under either name is dead code. This is rejected rather
/// than accepted-and-ignored.
pub fn register_engine(
    name: impl Into<String>,
    builder: Arc<dyn EngineBuilder>,
) -> Result<(), String> {
    let name = name.into();
    if EngineKind::from(name.clone()).is_builtin() {
        return Err(format!(
            "`{name}` is a built-in engine name and cannot be re-registered — a builder \
             under this name would never be reached, because the built-in variant is \
             resolved first. Pick a distinct name."
        ));
    }
    let mut guard = registry()
        .write()
        .map_err(|_| "engine registry poisoned".to_string())?;
    if guard.contains_key(&name) {
        return Err(format!(
            "engine `{name}` is already registered — refusing to replace it. Two builders \
             behind one id means the one that wins is decided by startup order."
        ));
    }
    guard.insert(name, builder);
    Ok(())
}

/// The engine ids this binary can serve — the two built-ins plus every
/// registered custom name, sorted. What an unknown-id error lists, and
/// what a host can show the operator. The retired `llama` is not one.
pub fn available_engines() -> Vec<String> {
    let mut names = vec!["llama-server".to_string(), "remote".to_string()];
    if let Ok(guard) = registry().read() {
        names.extend(guard.keys().cloned());
    }
    names.sort();
    names
}

/// The quirks family of the primary and fast slots. `Unknown`, so they
/// sample with the generic llama.cpp penalties (repeat 1.15, frequency 0.1,
/// DRY) whatever GGUF `[models]` names; the router sends them
/// (`llama_router::provider`). Whether a Qwen primary should carry them is
/// HUMAN-es-measure-sampler-pins' question.
pub const CHAT_SLOT_FAMILY: ModelFamily = ModelFamily::Unknown;

/// The code slot's family. The only code GGUF we ship is
/// Qwen3-Coder-30B-A3B-Instruct; pinning the family picks up Qwen's sampling
/// defaults and the SystemPromptToken thinking control.
pub const CODE_SLOT_FAMILY: ModelFamily = ModelFamily::Qwen3;

/// Construct the engine the config selects.
///
/// Pure construction: the caller has already decided this node may run
/// this engine. Errors are operator-facing strings, matching the
/// `build_engine` precedent in `sovereign-tools`' OCR module.
pub fn build_engine(config: &SetupConfig) -> Result<BuiltEngine, String> {
    let kind = config.engine.kind.clone();
    tracing::info!(
        target: "engine_factory",
        engine = %kind,
        "constructing the inference engine"
    );
    let built = match &kind {
        EngineKind::Llama => {
            tracing::error!(
                target: "engine_factory",
                "refused: kind = \"llama\", the in-process engine, was deleted"
            );
            Err(LLAMA_RETIRED.to_string())
        }
        EngineKind::Remote => build_remote(&config.engine),
        // Its endpoint exists only once its router runs, and the router is a
        // supervised child that must outlive every reload: serving assembly
        // starts it and builds the remote half here from the live router's
        // section (`sovereign_compute::llama_router`).
        EngineKind::LlamaServer => match unhostable_embed_path(&config.engine) {
            Some(refusal) => Err(refusal),
            None => Err(
                "[engine] kind = \"llama-server\" is assembled with its router by \
                 sovereign_compute's serving assembly; the factory builds only its remote half"
                    .to_string(),
            ),
        },
        EngineKind::Custom(name) => {
            let builder = {
                let guard = registry()
                    .read()
                    .map_err(|_| "engine registry poisoned".to_string())?;
                guard.get(name).cloned()
            };
            let Some(builder) = builder else {
                return Err(format!(
                    "[engine] kind = \"{name}\" is not a known engine. This binary can serve: \
                     {}. An out-of-tree engine must call \
                     `sovereign_compute::engine_factory::register_engine` before the \
                     provider is built.",
                    available_engines().join(", ")
                ));
            };
            builder.build(&config.engine)
        }
    }?;
    Ok(BuiltEngine {
        provider: sovereign_inference::engine_capture::wrap_if_requested(
            built.provider,
            config.engine.capture.as_deref(),
        ),
        ..built
    })
}

/// The refusal for `kind = "llama"`: what it was and what serves instead.
pub const LLAMA_RETIRED: &str = "[engine] kind = \"llama\" is retired: the in-process llama.cpp \
     engine was deleted (ENGINE_SWAP_PLAN G2). Set kind = \"llama-server\" (the default, which \
     serves the same `[models]` through a supervised router), or remove the key.";

/// Does a supervised compute child own this node's primary
/// (`[compute] enabled` + `distributed_primary`)? When it does, no path in
/// this process may load the primary GGUF.
pub fn child_owns_primary(config: &SetupConfig) -> bool {
    config.compute.enabled && config.compute.distributed_primary
}

/// The family an embed GGUF gets, from the models manifest by file name —
/// the one answer every embed load reads. It picks app-side pooling and the
/// document/query instruction prefixes, so an embed slot loaded as
/// [`ModelFamily::Unknown`] when the manifest knows its file embeds with the
/// wrong strategy. A file the manifest does not list is `Unknown`, as before.
pub fn embed_family_for(path: &std::path::Path) -> ModelFamily {
    let family = path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|name| {
            sovereign_contracts::models_manifest::DEFAULT_MANIFEST.embed_family_for_file(name)
        })
        .unwrap_or(ModelFamily::Unknown);
    tracing::debug!(
        target: "engine_factory",
        path = %path.display(),
        family = ?family,
        "embed family resolved from the models manifest"
    );
    family
}

/// An OpenAI-compatible HTTP endpoint.
///
/// Construction performs no I/O — the endpoint is not dialled here, so a
/// host can build this engine with the remote down and let the first
/// request report the failure. That is what makes the seam testable
/// without a server (see `tests/engine_factory.rs`).
/// The input preparation the remote embed half applies, per
/// `[engine] embed_inputs`. `client` names the family from the embed model id
/// through the one table (`EmbedQuirks::for_model_stem`) and refuses an id it
/// cannot place: preparing inputs for the wrong family puts vectors in a space
/// no corpus occupies, and sending them raw is what `client` was set to stop.
fn remote_embed_input_prep(
    section: &EngineSection,
    embed_model_id: &str,
) -> Result<Option<EmbedQuirks>, String> {
    if section.embed_inputs.unwrap_or_default() == EmbedInputs::Server {
        tracing::info!(
            target: "engine_factory",
            embed_model = %embed_model_id,
            "remote engine: embed inputs sent as given; the server prepares them"
        );
        return Ok(None);
    }
    let stem = embed_model_id.trim_end_matches(".gguf");
    match EmbedQuirks::for_model_stem(stem) {
        Some((family, quirks)) => {
            tracing::info!(
                target: "engine_factory",
                embed_model = %embed_model_id,
                family,
                "remote engine: embed inputs prepared here (instruction and EOS)"
            );
            Ok(Some(quirks))
        }
        None => Err(format!(
            "[engine] embed_inputs = \"client\", but embed model `{embed_model_id}` matches no \
             known embed family, so there is no preparation to apply. Name the model by its \
             family's id, or set embed_inputs = \"server\" if the server prepares inputs."
        )),
    }
}

/// The refusal for an `[engine]` section whose `embed_path` beside a hosted
/// chat model cannot be served on `kind = "llama-server"`.
///
/// A second SPELLING, not a second decider: the one implementation is
/// [`EngineSection::unhostable_on_router`] (sovereign-contracts), and this
/// only words its answer for an operator reading the boot error. A
/// `llama-server` section reaches it here, and
/// `SetupConfig::validate_class` asks the same decider before a daemon
/// starts. A hosted `kind = "remote"` pair never reaches it: it is MOVED to
/// the router by `EngineSection::hosted_router_section`.
fn unhostable_embed_path(section: &EngineSection) -> Option<String> {
    let excluded = section.unhostable_on_router()?;
    Some(format!(
        "[engine] `{excluded}` beside `embed_path`: the embedding GGUF runs in the router's \
         supervised process (ENGINE_SWAP_PLAN A6), and that preset is built from `[models]`, \
         which a node with a vendor chat does not name. Name `embed_endpoint` to embed on a \
         server, add `[models]`, or drop `{excluded}`."
    ))
}

fn build_remote(section: &EngineSection) -> Result<BuiltEngine, String> {
    use oicp_client::EngineEmbed;
    let endpoint = section.endpoint.as_deref().ok_or_else(|| {
        "[engine] kind = \"remote\" requires `endpoint` (e.g. \
         endpoint = \"http://localhost:8000/v1\")"
            .to_string()
    })?;
    // A hosted pair with no `model_id` sends NO model name (a placeholder),
    // so the vendor's default answers. It used to refuse, because a pair that
    // embedded here was a complete node; now the pair's embedder is the
    // router's and this half serves only the vendor chat, where "no id" is
    // "no preference" — an id invented here would be a routing bug (ARCH 6).
    let placeholder = section.model_id.is_none();
    let model_id = section
        .model_id
        .as_deref()
        .unwrap_or(oicp_client::HOSTED_DEFAULT_MODEL_ID);
    // Chat and embeddings are ONE model id on ONE endpoint unless the
    // operator says otherwise. That default is correct against a Sovereign
    // daemon, which routes embeddings to its own embed slot whatever id it
    // is handed, and wrong against vLLM / SGLang / TGI, which serve one
    // model per process and return a non-embedding shape (or an error) when
    // a chat model reaches `/embeddings`. So an embed endpoint must name its
    // model.
    if section.embed_path.is_some() && section.embed_endpoint.is_some() {
        return Err(
            "[engine] embed_path and embed_endpoint are both set. Embeddings come from one \
             place: a model this process loads (embed_path) or a server (embed_endpoint)."
                .to_string(),
        );
    }
    if section.embed_path.is_some() && section.embed_inputs == Some(EmbedInputs::Client) {
        return Err(
            "[engine] embed_inputs = \"client\" with embed_path: an embed model in this process \
             prepares its own inputs, so there is no server to prepare them for."
                .to_string(),
        );
    }
    if section.embed_endpoint.is_some() && section.embed_model_id.is_none() {
        return Err(
            "[engine] embed_endpoint is set but embed_model_id is not. The embedding \
             server still has to be told WHICH model to embed with, and guessing it \
             from the chat id is how a chat model ends up on the embeddings route."
                .to_string(),
        );
    }
    // Chat and embeddings are ONE model id on ONE endpoint unless the
    // operator says otherwise. That default is correct against a Sovereign
    // daemon, which routes embeddings to its own embed slot whatever id it
    // is handed, and wrong against vLLM / SGLang / TGI, which serve one
    // model per process and return a non-embedding shape (or an error) when
    // a chat model reaches `/embeddings`. So an embed endpoint must name its
    // model.
    //
    // `embed_path` is no longer a source here. A hosted engine used to load
    // that GGUF in this process (`EmbeddedLlamaCpp`'s embed slot); the router
    // owns every model process now (ENGINE_SWAP_PLAN A6), so a hosted pair is
    // moved to `kind = "llama-server"` by `hosted_router_section` before it
    // reaches this function. A `remote` build that still names one is refused
    // by name rather than embedding here (ARCH 6).
    if let Some(path) = section.embed_path.as_deref() {
        tracing::error!(
            target: "engine_factory",
            path = %path.display(),
            "refused: kind = \"remote\" cannot load embed_path in this process"
        );
        return Err(format!(
            "[engine] embed_path {} is not loaded by kind = \"remote\" any more: the router \
             owns every model process (ENGINE_SWAP_PLAN A6), so the hosted pair runs on \
             kind = \"llama-server\", where the embedder is its one preset section \
             (`hosted_router_section` moves it there). Name `embed_endpoint` to embed on a \
             server, or name `[models]` and run kind = \"llama-server\".",
            path.display()
        ));
    }
    // An embed SERVER is named; there is no embed model here to name. With
    // no id either, the pair's label is not an embed model (the trait's
    // `"unknown"` sentinel is what "nothing to say" answers).
    let embed_model_id =
        section
            .embed_model_id
            .as_deref()
            .unwrap_or(if placeholder { "" } else { model_id });
    let input_prep = remote_embed_input_prep(section, embed_model_id)?;
    let embed = EngineEmbed::Remote {
        endpoint_v1: section
            .embed_endpoint
            .as_deref()
            .unwrap_or(endpoint)
            .to_string(),
        model_id: embed_model_id.to_string(),
        input_prep,
    };
    let embed_family = ModelFamily::Unknown;
    // One shape for every remote engine. The pair decides each half's far
    // end (an endpoint off this machine is a third party, never sent texts)
    // and the locus the router reads.
    let provider = oicp_client::SplitInferenceProvider::engine(
        endpoint,
        embed,
        section.api_key.clone(),
        model_id.to_string(),
        section.fast_model_id.clone(),
        section.context_size,
        section.extra_params.clone(),
        section.structured_output.unwrap_or_default(),
        section.grammar.unwrap_or_default(),
    )
    .map_err(|e| format!("[engine] kind = \"remote\": {e}"))?;
    if placeholder {
        tracing::info!(
            target: "engine_factory",
            endpoint = %endpoint,
            "hosted engine: no `model_id`, so no model name goes on the wire; the vendor's default answers"
        );
    }
    tracing::info!(
        target: "engine_factory",
        endpoint = %endpoint,
        model_id = %model_id,
        locus = ?provider.serving_locus(),
        "remote engine constructed — this node holds no chat weights"
    );
    let provider: Arc<dyn InferenceProvider> = Arc::new(provider);
    // The family the embed slot loaded with, not `external`'s `Unknown`:
    // serve reports it and the mesh advertises pooling from it.
    Ok(BuiltEngine {
        embed_family,
        ..BuiltEngine::external(provider)
    })
}
// The tests live in a sibling file: keeping them here put this file in the
// 800-1200 approach band (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "engine_factory/tests.rs"]
mod tests;
