// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hosted engine's plan: no `[models]`, and an embed slot only when it
//! embeds in this process.
use super::*;
use sovereign_contracts::engine_config::EngineSection;

fn hosted(embed_path: Option<&str>) -> SetupConfig {
    let mut config = SetupConfig::unconfigured();
    config.models = None;
    config.engine = EngineSection {
        kind: EngineKind::Remote,
        endpoint: Some("https://api.example.com/v1".to_string()),
        model_id: Some("vendor-model".to_string()),
        embed_path: embed_path.map(Into::into),
        ..Default::default()
    };
    config
}

/// THE FAILING INPUT: `plan_serving` read `[models]` before looking at the
/// engine, so a hosted node had to carry placeholder model paths to start.
#[test]
fn a_hosted_engine_plans_without_models() {
    let plan = plan_serving(&hosted(Some("/models/Qwen3-Embedding-0.6B-Q8_0.gguf"))).unwrap();
    assert_eq!(plan.engine, EngineKind::Remote);
    assert_eq!(
        plan.in_process,
        BTreeSet::from([PlannedSlot::Embed]),
        "the local embed model is the one slot in this process"
    );

    let plan = plan_serving(&hosted(None)).unwrap();
    assert!(
        plan.in_process.is_empty(),
        "no embed_path, no slot: {plan:?}"
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
#[test]
fn a_hosted_router_plans_the_embedder_as_its_one_section() {
    let mut config = hosted(Some("/models/Qwen3-Embedding-0.6B-Q8_0.gguf"));
    config.engine.kind = EngineKind::LlamaServer;
    config.engine.endpoint = None;
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
fn a_hosted_router_without_an_embed_path_serves_nothing() {
    let mut config = hosted(None);
    config.engine.kind = EngineKind::LlamaServer;
    config.engine.endpoint = None;
    let plan = plan_serving(&config).unwrap();
    assert!(plan.in_process.is_empty(), "{plan:?}");
    assert!(plan.router.is_none(), "{plan:?}");
}

/// THE FAILING INPUT (ENGINE_SWAP_PLAN A6): the shape `svrn setup --hosted`
/// writes — a vendor chat and an embed_path — under the engine that
/// replaced the in-process loader. It was served by loading the embedder in
/// this process, which the router now owns; and the router cannot serve it
/// at all, because a hosted pair names no `[models]` to build a preset from.
/// Named at plan time, so the boot says why instead of reporting the node
/// unconfigured (`svrn setup`) or starting a daemon that answers no model.
#[test]
fn a_hosted_pair_cannot_be_assembled_on_the_router() {
    let mut config = hosted(Some("/models/Qwen3-Embedding-0.6B-Q8_0.gguf"));
    config.engine.kind = EngineKind::LlamaServer;
    let err = match assemble_serving(&config) {
        Ok(_) => panic!("nothing to serve must refuse"),
        Err(e) => e,
    };
    assert!(
        err.reason.contains("embed_path") && err.reason.contains("llama-server"),
        "the refusal names the key and the engine: {}",
        err.reason
    );
}
