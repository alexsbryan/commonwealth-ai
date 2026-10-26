// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hosted engine's plan: no `[models]`, and no slot in this process — an
//! embed server holds the embedder, and a hosted PAIR (`embed_path`) is the
//! router's one section.
use super::*;
use sovereign_contracts::engine_config::EngineSection;

/// A hosted node: a vendor chat and no `[models]`. `embed_server` names an
/// embed ENDPOINT; the pair's own `embed_path` is set by the test that wants
/// it, so the two routes are never confused.
fn hosted(embed_server: Option<&str>) -> SetupConfig {
    let mut config = SetupConfig::unconfigured();
    config.models = None;
    config.engine = EngineSection {
        // An embed SERVER, not `embed_path`: every client hosts its own
        // embedder, so `remote` + `embed_path` is a hosted PAIR now — the
        // router's one section (`a_hosted_pair_is_served_by_a_one_section_router`).
        kind: EngineKind::Remote,
        endpoint: Some("https://api.example.com/v1".to_string()),
        model_id: Some("vendor-model".to_string()),
        // 0 as `svrn setup --hosted` writes it: no chat model here, so no
        // chat context — which is also what marks the pair for the router.
        context_size: 0,
        embed_endpoint: embed_server.map(|_| "http://127.0.0.1:8001/v1".to_string()),
        embed_model_id: embed_server.map(|_| "Qwen3-Embedding-0.6B-Q8_0".to_string()),
        ..Default::default()
    };
    config
}

/// THE FAILING INPUT: `plan_serving` read `[models]` before looking at the
/// engine, so a hosted node had to carry placeholder model paths to start.
#[test]
fn a_hosted_engine_plans_without_models() {
    let plan = plan_serving(&hosted(Some("Qwen3-Embedding-0.6B-Q8_0"))).unwrap();
    assert_eq!(plan.engine, EngineKind::Remote);
    assert!(
        plan.in_process.is_empty(),
        "a hosted engine holds no weights: its embedder is the server named: {plan:?}"
    );

    let plan = plan_serving(&hosted(None)).unwrap();
    assert!(
        plan.in_process.is_empty(),
        "no embedder at all, no slot: {plan:?}"
    );
}

/// Llama still needs `[models]`: the exemption is the remote engine's alone.
#[test]
fn llama_without_models_still_refuses() {
    let mut config = hosted(None);
    config.engine = EngineSection::default();
    assert!(plan_serving(&config).is_err());
}

/// A HOSTED node on the router (ENGINE_SWAP_PLAN A6): `[engine] embed_path`
/// and no `[models]` at all, so its router serves the embedder as its one
/// section. Before this, `plan_serving` read `[models]` on every engine but
/// `remote`, so the shape `svrn setup --hosted` writes could not boot here.
/// The pair's own `remote` form of the same file takes the same route (see
/// `a_hosted_pair_is_served_by_a_one_section_router`).
#[test]
fn a_hosted_router_plans_the_embedder_as_its_one_section() {
    let mut config = hosted(None);
    config.engine.kind = EngineKind::LlamaServer;
    config.engine.embed_path = Some("/models/Qwen3-Embedding-0.6B-Q8_0.gguf".into());
    let plan = plan_serving(&config).unwrap();
    assert_eq!(plan.engine, EngineKind::LlamaServer);
    assert!(
        plan.in_process.is_empty(),
        "the router runs the embedder, not this process: {plan:?}"
    );
    let preset = plan.router.as_ref().expect("a router node plans a preset");
    assert_eq!(preset.primary(), None, "no chat model is served here");
    assert_eq!(
        preset.roles(),
        vec![("embed", "Qwen3-Embedding-0.6B-Q8_0")],
        "{preset:?}"
    );
    assert_eq!(plan.embed_family, ModelFamily::Qwen3Embedding);
}

/// A router node with no `[models]` and no `embed_path` serves nothing: no
/// preset, no slots, and a build that says so rather than loading weights.
#[test]
fn a_router_without_an_embed_path_serves_nothing() {
    let mut config = hosted(None);
    config.engine.kind = EngineKind::LlamaServer;
    let plan = plan_serving(&config).unwrap();
    assert!(plan.in_process.is_empty(), "{plan:?}");
    assert!(plan.router.is_none(), "{plan:?}");
}

/// THE FAILING INPUT (ENGINE_SWAP_PLAN A6): the shape `svrn setup --hosted`
/// writes — a vendor chat and an `embed_path` — used to be served by loading
/// the embedding GGUF in THIS process (`EngineEmbed::Local`), which is the
/// process A6 retires: every model is the router's. Before this the pair was
/// refused (the preset is built from `[models]`, and a hosted pair names
/// none); now it is served, by the one-section router the pair is moved to,
/// so the boot and the plan agree.
#[test]
fn a_hosted_pair_is_served_by_a_one_section_router() {
    let mut config = hosted(None);
    config.engine.embed_path = Some("/models/Qwen3-Embedding-0.6B-Q8_0.gguf".into());
    assert!(is_hosted_pair(&config), "the fixture must be the pair");
    let plan = plan_serving(&config).expect("the pair is served, not refused");
    assert_eq!(plan.engine, EngineKind::LlamaServer);
    assert!(
        plan.in_process.is_empty(),
        "the router runs the embedder, not this process: {plan:?}"
    );
    let preset = plan.router.as_ref().expect("a one-section preset");
    assert_eq!(preset.primary(), None, "the chat stays at the vendor");
    assert_eq!(
        preset.roles(),
        vec![("embed", "Qwen3-Embedding-0.6B-Q8_0")],
        "{preset:?}"
    );
}

/// The pair's boot follows its plan: `assemble_serving` moves once, before
/// anything is planned or spawned, so the build cannot load the embedder
/// here. The move is what the router then serves; a file with a missing GGUF
/// still fails, naming the path.
#[test]
fn a_hosted_pair_boots_toward_the_router_not_the_loader_it_replaced() {
    let mut config = hosted(None);
    config.engine.embed_path = Some("/models/Qwen3-Embedding-0.6B-Q8_0.gguf".into());
    assert!(is_hosted_pair(&config));
    let plan = plan_serving(&config).expect("the pair is served, not refused");
    assert_eq!(plan.engine, EngineKind::LlamaServer);
    assert!(
        plan.router.is_some() && plan.in_process.is_empty(),
        "the router runs the embedder, this process nothing: {plan:?}"
    );
    // And the boot answers the same way: it stops at the shipped-server
    // check of the router it planned, never at a `remote` loader or at the
    // class refusal the moved section no longer trips.
    let err = match assemble_serving(&config) {
        Ok(_) => panic!("a router with no server binary must refuse"),
        Err(e) => e.reason,
    };
    assert!(
        !err.contains("not served by kind")
            && !err.contains("names no route")
            && !err.contains("embed_path"),
        "the boot planned the pair's router, not a refusal: {err}"
    );
    // Said positively: the boot gets as far as starting the router. Until
    // 2026-10-24 it stopped earlier, at "no models are configured" (the moved
    // section's kind asked for `[models]`, which a pair never names), and
    // the negative checks above passed through it; the Mac's hosted daemon
    // crashlooped on exactly that.
    assert!(
        err.contains("llama-server router is started from a tokio runtime")
            || err.contains("needs llama-server beside the daemon"),
        "the boot stops where the router starts: {err}"
    );
}
