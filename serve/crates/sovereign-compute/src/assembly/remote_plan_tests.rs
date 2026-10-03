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
