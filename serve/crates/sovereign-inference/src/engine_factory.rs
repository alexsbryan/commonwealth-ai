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

use sovereign_contracts::model_family::ModelFamily;
use sovereign_contracts::setup_config::{EngineSection, SetupConfig};
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
    match &kind {
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
    }
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
    let embed = match section.embed_path.as_deref() {
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
            EngineEmbed::Local {
                provider: Arc::new(provider),
                model_id,
            }
        }
        None => EngineEmbed::Remote {
            endpoint_v1: section
                .embed_endpoint
                .as_deref()
                .unwrap_or(endpoint)
                .to_string(),
            model_id: section
                .embed_model_id
                .as_deref()
                .unwrap_or(model_id)
                .to_string(),
        },
    };
    // One shape for every remote engine. The pair decides each half's far
    // end (an endpoint off this machine is a third party, never sent texts)
    // and the locus the router reads.
    let provider = crate::remote::SplitInferenceProvider::engine(
        endpoint,
        embed,
        section.api_key.clone(),
        model_id.to_string(),
        section.context_size,
        section.extra_params.clone(),
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
    Ok(BuiltEngine::external(provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::setup_config::ModelsSection;

    /// The default must stay `Llama`: an existing `config.toml` names no
    /// engine, and `#[serde(default)]` on the section must therefore
    /// reproduce today's behaviour exactly. A change here silently
    /// re-points every deployed node.
    #[test]
    fn the_default_engine_is_llama() {
        assert_eq!(EngineKind::default(), EngineKind::Llama);
        assert_eq!(EngineSection::default().kind, EngineKind::Llama);
    }

    #[test]
    fn engine_names_round_trip_through_the_config_string() {
        for name in ["llama", "remote", "hypertuned-metal"] {
            let kind = EngineKind::from(name.to_string());
            assert_eq!(kind.as_str(), name, "`{name}` did not round-trip");
        }
        assert_eq!(EngineKind::from("llama".to_string()), EngineKind::Llama);
        assert_eq!(EngineKind::from("remote".to_string()), EngineKind::Remote);
        assert_eq!(
            EngineKind::from("hypertuned-metal".to_string()),
            EngineKind::Custom("hypertuned-metal".to_string())
        );
    }

    /// An unregistered engine must REFUSE and name what is available —
    /// not fall back to llama. A silent fallback here would load 30 GB of
    /// GGUF on a node whose operator asked for something else, and the
    /// only symptom would be the wrong model answering (ARCH §18.3).
    #[test]
    fn an_unknown_engine_refuses_and_lists_what_is_available() {
        let mut config = SetupConfig::unconfigured();
        config.engine.kind = EngineKind::from("no-such-engine".to_string());

        let err = build_engine(&config).expect_err("an unknown engine must not build");
        assert!(
            err.contains("no-such-engine"),
            "the error must name the id the operator typed; got: {err}"
        );
        assert!(
            err.contains("llama") && err.contains("remote"),
            "the error must list the engines this binary CAN serve; got: {err}"
        );
        assert!(
            err.contains("register_engine"),
            "the error must name the repair; got: {err}"
        );
    }

    /// A builder registered under a built-in name would never be reached,
    /// because the built-in variant is resolved before the registry is
    /// consulted. Accepting it would give one name two deciders.
    #[test]
    fn a_builtin_name_cannot_be_re_registered() {
        struct Never;
        impl EngineBuilder for Never {
            fn build(&self, _: &EngineSection) -> Result<BuiltEngine, String> {
                unreachable!("a built-in name must never reach the registry")
            }
        }
        for builtin in ["llama", "remote"] {
            let err = register_engine(builtin, Arc::new(Never))
                .expect_err("registering over a built-in must be refused");
            assert!(err.contains(builtin), "got: {err}");
        }
    }

    /// Every embed load reads this one answer, so a file the manifest lists
    /// must come back with its family, and the lookup is by file name wherever
    /// the file lives.
    #[test]
    fn a_manifest_embed_file_keeps_its_family() {
        let family = embed_family_for(std::path::Path::new(
            "/models/elsewhere/Qwen3-Embedding-0.6B-Q8_0.gguf",
        ));
        assert_eq!(family, ModelFamily::Qwen3Embedding);
        assert_eq!(
            embed_family_for(std::path::Path::new("/m/totally-unknown-embed-model.gguf")),
            ModelFamily::Unknown
        );
    }

    /// `remote` must refuse rather than invent a default endpoint. A
    /// defaulted `localhost:8000` would make a misconfigured node look
    /// healthy until the first request.
    #[test]
    fn remote_refuses_without_an_endpoint_or_model_id() {
        let mut section = EngineSection {
            kind: EngineKind::Remote,
            ..Default::default()
        };
        let err = build_remote(&section).expect_err("no endpoint must refuse");
        assert!(err.contains("endpoint"), "got: {err}");

        section.endpoint = Some("http://localhost:8000/v1".to_string());
        let err = build_remote(&section).expect_err("no model_id must refuse");
        assert!(err.contains("model_id"), "got: {err}");
    }

    /// Embeddings come from one place. A local embed model that cannot load
    /// is a refusal naming its path, never a quiet fall back to the vendor,
    /// which would be sent texts.
    #[test]
    fn a_local_embed_model_is_the_one_source_or_a_refusal() {
        let mut section = EngineSection {
            kind: EngineKind::Remote,
            endpoint: Some("https://api.example.com/v1".to_string()),
            model_id: Some("vendor-model".to_string()),
            embed_path: Some("/nonexistent/Qwen3-Embedding-0.6B-Q8_0.gguf".into()),
            embed_endpoint: Some("http://127.0.0.1:8001/v1".to_string()),
            embed_model_id: Some("e".to_string()),
            ..Default::default()
        };
        let err = build_remote(&section).expect_err("two embed sources must refuse");
        assert!(
            err.contains("embed_path") && err.contains("embed_endpoint"),
            "got: {err}"
        );

        section.embed_endpoint = None;
        let err = build_remote(&section).expect_err("an unloadable embed model must refuse");
        assert!(
            err.contains("/nonexistent/Qwen3-Embedding-0.6B-Q8_0.gguf"),
            "got: {err}"
        );
    }

    /// The whole point of the seam: an engine that is not llama.cpp
    /// builds, holds no llama handle, and needs no GGUF on disk. This is
    /// the test that would have been impossible before the factory —
    /// `load_provider` reached `EmbeddedLlamaCpp::load_full_with_families`
    /// unconditionally.
    #[test]
    fn the_remote_engine_builds_with_no_weights_and_no_llama_handle() {
        let mut config = SetupConfig::unconfigured();
        config.engine = EngineSection {
            kind: EngineKind::Remote,
            endpoint: Some("http://127.0.0.1:1/v1".to_string()),
            model_id: Some("some-model".to_string()),
            api_key: None,
            context_size: 8192,
            embed_model_id: None,
            embed_endpoint: None,
            embed_path: None,
            extra_params: None,
        };
        // Deliberately absent paths: if this engine touched a GGUF the
        // build would fail, and that failure is the assertion.
        //
        // SUPPLY the section rather than reaching through `models_mut()`.
        // `unconfigured()` carries `models: None` by contract, and the
        // accessor refuses a missing section on purpose (§18.3) — so
        // mutating through it here could only ever panic, which is what it
        // did. A fixture that wants a `[models]` table has to provide one.
        config.models = Some(ModelsSection {
            primary: "/nonexistent/primary.gguf".into(),
            embed: "/nonexistent/embed.gguf".into(),
            ..Default::default()
        });

        let built = build_engine(&config).expect("the remote engine needs no weights");
        assert!(
            built.llama.is_none(),
            "a non-llama engine must not carry a llama handle — a host that finds one \
             will call slot methods that cannot work"
        );
        assert_eq!(built.embed_family, ModelFamily::Unknown);
    }

    /// A third-party server serves ONE model per process, so a node that
    /// chats and retrieves points at two of them. Naming either embed key
    /// must route embeddings to the embed model rather than sending the
    /// chat id to `/embeddings`, which returns a non-embedding shape.
    #[test]
    fn a_separate_embedding_server_is_wired_to_its_own_model() {
        let mut config = SetupConfig::unconfigured();
        config.engine = EngineSection {
            kind: EngineKind::Remote,
            endpoint: Some("http://127.0.0.1:8000/v1".to_string()),
            model_id: Some("Qwen3.5-35B-A3B".to_string()),
            api_key: None,
            context_size: 32768,
            embed_endpoint: Some("http://127.0.0.1:8001/v1".to_string()),
            embed_model_id: Some("BAAI/bge-m3".to_string()),
            embed_path: None,
            extra_params: None,
        };
        let built = build_engine(&config).expect("split chat/embed builds without I/O");
        assert!(built.llama.is_none());
        assert_eq!(
            built.provider.embed_model_id(),
            "BAAI/bge-m3",
            "embeddings must be vouched for by the EMBED model — persisted vectors are \
             matched against this id, so reporting the chat model would silently \
             validate vectors it never produced"
        );
    }

    /// An embed endpoint with no embed model is refused rather than
    /// guessed. Falling back to the chat id here is exactly how a chat
    /// model ends up on the embeddings route (ARCH §18.3).
    #[test]
    fn an_embed_endpoint_without_a_model_id_is_refused() {
        let mut config = SetupConfig::unconfigured();
        config.engine = EngineSection {
            kind: EngineKind::Remote,
            endpoint: Some("http://127.0.0.1:8000/v1".to_string()),
            model_id: Some("chat-model".to_string()),
            embed_endpoint: Some("http://127.0.0.1:8001/v1".to_string()),
            embed_model_id: None,
            ..Default::default()
        };
        let err = build_engine(&config).expect_err("must refuse rather than guess");
        assert!(err.contains("embed_model_id"), "got: {err}");
    }

    /// The input the single-endpoint branch got wrong: it reported
    /// `OwnWeights`, so a `local_only` turn went to whatever the endpoint
    /// named. Off this machine is a third party in both config shapes; a
    /// loopback server is this machine.
    #[test]
    fn a_remote_engine_off_this_machine_is_a_third_party() {
        use sovereign_contracts::traits::ServingLocus;
        let locus = |endpoint: &str, embed_endpoint: Option<&str>| {
            let mut config = SetupConfig::unconfigured();
            config.engine = EngineSection {
                kind: EngineKind::Remote,
                endpoint: Some(endpoint.to_string()),
                model_id: Some("m".to_string()),
                embed_endpoint: embed_endpoint.map(str::to_string),
                embed_model_id: embed_endpoint.map(|_| "e".to_string()),
                ..Default::default()
            };
            build_engine(&config)
                .expect("builds without I/O")
                .provider
                .serving_locus()
        };
        let vendor = "https://api.deepseek.com/v1";
        assert_eq!(locus(vendor, None), ServingLocus::ForwardsToThirdParty);
        assert_eq!(
            locus(vendor, Some("http://127.0.0.1:9741/v1")),
            ServingLocus::ForwardsToThirdParty
        );
        assert_eq!(
            locus("http://127.0.0.1:8000/v1", None),
            ServingLocus::ForwardsOnBox
        );
    }

    /// An out-of-tree engine reaches the seam through the registry, and
    /// the config section is handed to it verbatim.
    #[test]
    fn a_registered_custom_engine_is_selected_and_receives_its_config() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static SAW_CONFIG: AtomicBool = AtomicBool::new(false);

        struct Hypertuned;
        impl EngineBuilder for Hypertuned {
            fn build(&self, section: &EngineSection) -> Result<BuiltEngine, String> {
                assert_eq!(section.endpoint.as_deref(), Some("vendor://gpu0"));
                SAW_CONFIG.store(true, Ordering::SeqCst);
                Err("constructed".to_string())
            }
        }

        register_engine("hypertuned-metal", Arc::new(Hypertuned))
            .expect("a fresh custom name registers");

        let mut config = SetupConfig::unconfigured();
        config.engine.kind = EngineKind::from("hypertuned-metal".to_string());
        config.engine.endpoint = Some("vendor://gpu0".to_string());

        let err = build_engine(&config).expect_err("the stub builder returns Err by design");
        assert_eq!(err, "constructed", "the registry must reach OUR builder");
        assert!(SAW_CONFIG.load(Ordering::SeqCst));
        assert!(available_engines().contains(&"hypertuned-metal".to_string()));
    }
}
