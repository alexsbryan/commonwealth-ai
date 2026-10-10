// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which inference engine serves this node — the one place that decides.
//!
//! Before this module, engine choice was not a decision the system could
//! express: five construction sites named [`EmbeddedLlamaCpp`] literally
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
//! and it does not configure llama's optional slots (extras, edit, rerank,
//! idle monitors — all concrete `EmbeddedLlamaCpp` methods the assembly
//! calls on [`BuiltEngine::llama`] when it is `Some`).
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
//! **Home.** This crate, because it is the only one that can already name
//! both engines: `EmbeddedLlamaCpp` is its own, and `oicp-client` has been
//! a dependency since the `remote.rs` extraction (re-exported at
//! `crate::remote`). Zero new dependency edges, no change to
//! `quality/ARCH_LAYERS.toml`.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use sovereign_contracts::embed_quirks::EmbedQuirks;
use sovereign_contracts::model_family::ModelFamily;
use sovereign_contracts::setup_config::{EmbedInputs, EngineSection, SetupConfig};
use sovereign_contracts::traits::InferenceProvider;

use crate::embedded::{EmbeddedLlamaCpp, SlotWindows};

/// Re-exported so a host or an out-of-tree engine can name the selection
/// type without also depending on `sovereign-contracts` directly. The
/// definition lives one layer down, next to [`EngineSection`], because it
/// is config vocabulary — this crate cannot own it without making the
/// contract crate depend upward.
pub use sovereign_contracts::setup_config::EngineKind;

/// A constructed engine plus the concrete handles its host still needs.
///
/// The `provider` is what every consumer above the seam receives. The
/// `llama` handle is the honest, contained form of a leak that used to be
/// implicit: the daemon holds it for the RPC-worker auto-reload path and
/// for llama's optional slot installs, and it is `None` for every other
/// engine. A host must treat `None` as "this engine has no local slots" —
/// never as a reason to fall back to constructing one.
pub struct BuiltEngine {
    /// The `dyn`-erased engine, installed and advertised by the host.
    pub provider: Arc<dyn InferenceProvider>,
    /// `Some` only for [`EngineKind::Llama`]. Carries the concrete API
    /// the trait deliberately does not expose: `install_extras`,
    /// `install_edit_slot`, `install_rerank`, `start_idle_monitor`,
    /// and the primary reload the mesh worker-discovery task fires.
    pub llama: Option<Arc<EmbeddedLlamaCpp>>,
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
            .field("holds_llama_slots", &self.llama.is_some())
            .field("embed_family", &self.embed_family)
            .field(
                "max_context_tokens",
                &self.provider.capabilities().max_context_tokens,
            )
            .finish()
    }
}

impl BuiltEngine {
    /// Wrap a provider that owns no local llama slots. The constructor an
    /// out-of-tree [`EngineBuilder`] uses — it cannot produce an
    /// `EmbeddedLlamaCpp`, and should not pretend to.
    pub fn external(provider: Arc<dyn InferenceProvider>) -> Self {
        Self {
            provider,
            llama: None,
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
/// `name` must not shadow a built-in (`llama`, `remote`) — those parse to
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
/// what a host can show the operator.
pub fn available_engines() -> Vec<String> {
    let mut names = vec!["llama".to_string(), "remote".to_string()];
    if let Ok(guard) = registry().read() {
        names.extend(guard.keys().cloned());
    }
    names.sort();
    names
}

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
        EngineKind::Llama => build_llama(config),
        EngineKind::Remote => build_remote(&config.engine),
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
                     `sovereign_inference::engine_factory::register_engine` before the \
                     provider is built.",
                    available_engines().join(", ")
                ));
            };
            builder.build(&config.engine)
        }
    }?;
    Ok(BuiltEngine {
        provider: crate::engine_capture::wrap_if_requested(
            built.provider,
            config.engine.capture.as_deref(),
        ),
        ..built
    })
}

/// The in-process llama.cpp engine — the three-to-four GGUF slots.
///
/// Everything llama-specific that was inline in the daemon's
/// `load_provider` and is *construction* lives here; everything that is
/// *post-construction configuration* (extras, edit slot, rerank, idle
/// monitors) stays with the serving assembly, which calls it on
/// [`BuiltEngine::llama`].
fn build_llama(config: &SetupConfig) -> Result<BuiltEngine, String> {
    // `[models]` is an `Option` since the `terminal` node class landed
    // (2026-08-30): a node can legitimately hold none. Read it through the
    // accessor rather than a field, so the absence arrives as the refusal that
    // names WHY there are no slots — "this node is a terminal, route instead"
    // vs "run `svrn setup`" — instead of an empty `PathBuf` reaching
    // llama.cpp and coming back as whatever it says about `""` (§18.3).
    //
    // A terminal never actually gets here; the daemon returns its forwarding
    // provider before calling the factory. This is the guard for the case that
    // is NOT a deliberate terminal — a half-written or hand-edited config.
    let models = config.models()?;
    let embed_family = embed_family_for(&models.embed);

    // `[compute] distributed_primary` — the primary lives in a supervised
    // child, so the daemon must NOT also hold it. Withholding the path is
    // what makes that true: every lazy in-process primary load reads
    // `primary_path`, so `None` means no code path in this process can pull
    // the distributed model in behind our back. The *guards* around this
    // mode (containment armed? is `fast` distinct from `primary`?) are
    // admission and live in the serving assembly (sovereign-compute
    // `assembly.rs`, `containment.rs`); this is only the derivation.
    let child_owns_primary = child_owns_primary(config);

    let engine = EmbeddedLlamaCpp::load_full_with_families(
        models.fast_path(),
        if child_owns_primary {
            None
        } else {
            Some(&models.primary)
        },
        Some(&models.embed),
        models.code.as_deref(),
        SlotWindows::from_models(models),
        None, // gpu_layers — auto-detect
        ModelFamily::Unknown,
        ModelFamily::Unknown,
        embed_family.clone(),
        // The only code GGUF we ship is Qwen3-Coder-30B-A3B-Instruct;
        // pinning the family picks up Qwen's sampling defaults and the
        // SystemPromptToken thinking control.
        ModelFamily::Qwen3,
    )
    .map_err(|e| format!("failed to load models: {e}"))?;

    let arc = Arc::new(engine);
    Ok(BuiltEngine {
        provider: Arc::clone(&arc) as Arc<dyn InferenceProvider>,
        llama: Some(arc),
        embed_family,
    })
}

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

fn build_remote(section: &EngineSection) -> Result<BuiltEngine, String> {
    use crate::remote::EngineEmbed;
    let endpoint = section.endpoint.as_deref().ok_or_else(|| {
        "[engine] kind = \"remote\" requires `endpoint` (e.g. \
         endpoint = \"http://localhost:8000/v1\")"
            .to_string()
    })?;
    let model_id = section.model_id.as_deref().ok_or_else(|| {
        "[engine] kind = \"remote\" requires `model_id` — the name the remote serves. \
         It is sent on the wire, so a wrong value is a routing bug, not a label."
            .to_string()
    })?;
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
    // model. A vendor is never sent texts at all, so a hosted engine names
    // `embed_path` instead: the small embedding GGUF, in this process.
    let (embed, embed_family) = match section.embed_path.as_deref() {
        Some(path) => {
            let family = embed_family_for(path);
            let model_id = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .ok_or_else(|| format!("[engine] embed_path {} names no file", path.display()))?;
            // llama-cpp-4 panics on a missing file (`model.rs`: "does not
            // exist"); refuse first, naming the path, so a typo is a message
            // and not a dead daemon.
            if !path.is_file() {
                return Err(format!(
                    "[engine] embed_path {} is not a file. Point it at the embedding GGUF \
                     (`svrn setup --hosted` downloads one).",
                    path.display()
                ));
            }
            let provider = crate::embedded::EmbedOnlyProvider::load(path, family.clone())
                .map_err(|e| format!("[engine] embed_path {}: {e}", path.display()))?;
            tracing::info!(
                target: "engine_factory",
                path = %path.display(),
                ?family,
                %model_id,
                "remote engine: embeddings run in this process"
            );
            let embed = EngineEmbed::Local {
                provider: Arc::new(provider),
                model_id,
            };
            (embed, family)
        }
        None => {
            let embed_model_id = section.embed_model_id.as_deref().unwrap_or(model_id);
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
            (embed, ModelFamily::Unknown)
        }
    };
    // One shape for every remote engine. The pair decides each half's far
    // end (an endpoint off this machine is a third party, never sent texts)
    // and the locus the router reads.
    let provider = crate::remote::SplitInferenceProvider::engine(
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
