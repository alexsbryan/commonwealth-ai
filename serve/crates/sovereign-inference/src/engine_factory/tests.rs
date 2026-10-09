// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use sovereign_contracts::setup_config::ModelsSection;

/// The default must stay `Llama`: an existing `config.toml` names no
/// engine, and `#[serde(default)]` on the section must therefore
/// reproduce today's behaviour exactly. A change here silently
/// re-points every deployed node.
/// `embed_inputs = "client"` names the family through the one table and
/// refuses an id it cannot place; `server` (and unset) prepares nothing.
#[test]
fn client_embed_inputs_name_a_family_or_refuse() {
    let section = |inputs: Option<EmbedInputs>| EngineSection {
        embed_inputs: inputs,
        ..Default::default()
    };
    let client = section(Some(EmbedInputs::Client));
    assert_eq!(
        remote_embed_input_prep(&client, "Qwen3-Embedding-0.6B-Q8_0.gguf"),
        Ok(Some(EmbedQuirks::qwen3_embedding()))
    );
    let refusal = remote_embed_input_prep(&client, "BAAI/bge-m3").unwrap_err();
    assert!(refusal.contains("BAAI/bge-m3"), "{refusal}");
    assert_eq!(
        remote_embed_input_prep(&section(None), "Qwen3-Embedding-0.6B-Q8_0"),
        Ok(None)
    );
    assert_eq!(
        remote_embed_input_prep(
            &section(Some(EmbedInputs::Server)),
            "Qwen3-Embedding-0.6B-Q8_0"
        ),
        Ok(None)
    );
}

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

/// A hosted engine that embeds in process reports the family it loaded
/// with: serve's self report carries it and the mesh advertises pooling
/// from it, so `Unknown` there is a Mean-pooling claim about a
/// last-token model. Loads a real GGUF, so ignored in the suite; run by
/// hand with `SOVEREIGN_MODELS_DIR` naming the directory that holds it.
#[test]
#[ignore]
fn a_hosted_engine_reports_the_embed_family_it_loaded() {
    let dir = std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(std::path::PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the directory holding the embed GGUF");
    let section = EngineSection {
        kind: EngineKind::Remote,
        endpoint: Some("https://api.example.com/v1".to_string()),
        model_id: Some("vendor-model".to_string()),
        embed_path: Some(dir.join("Qwen3-Embedding-0.6B-Q8_0.gguf")),
        ..Default::default()
    };
    let built = build_remote(&section).expect("a hosted engine with a local embed builds");
    assert_eq!(built.embed_family, ModelFamily::Qwen3Embedding);
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
        fast_model_id: None,
        context_size: 8192,
        embed_model_id: None,
        embed_endpoint: None,
        embed_path: None,
        extra_params: None,
        structured_output: None,
        embed_inputs: None,
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
        fast_model_id: None,
        context_size: 32768,
        embed_endpoint: Some("http://127.0.0.1:8001/v1".to_string()),
        embed_model_id: Some("BAAI/bge-m3".to_string()),
        embed_path: None,
        extra_params: None,
        structured_output: None,
        embed_inputs: None,
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
