// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `serves_rerank` — see `assembly.rs`.

use super::*;
use sovereign_contracts::setup_config::ComputeSlotConfig;

/// Daemon turns get a cross-encoder exactly when this process holds one:
/// a `[models.kinds] rerank` slot, or a warm rerank compute child.
#[test]
fn a_process_serves_rerank_only_when_it_holds_one() {
    assert!(
        std::env::var_os("SOVEREIGN_RERANK_MODEL_PATH").is_none(),
        "this test reads the config key; the env var would override it"
    );
    let mut config = SetupConfig::unconfigured();
    assert!(
        !serves_rerank(&config),
        "no [models]: no weights, no rerank"
    );

    config.models = Some(ModelsSection {
        primary: "/m/primary.gguf".into(),
        embed: "/m/embed.gguf".into(),
        ..Default::default()
    });
    assert!(!serves_rerank(&config), "no rerank key: none");

    let mut with_key = config.clone();
    if let Some(m) = with_key.models.as_mut() {
        m.kinds.insert("rerank".into(), "/m/rerank.gguf".into());
    }
    assert!(
        serves_rerank(&with_key),
        "the key installs the in-process slot"
    );

    config.compute.enabled = true;
    config.compute.slot.push(ComputeSlotConfig {
        name: "reranker".into(),
        role: "rerank".into(),
        model: "/m/rerank.gguf".into(),
        context_size: None,
        n_gpu_layers: None,
        warm: true,
        capture_embed: false,
    });
    assert!(serves_rerank(&config), "a warm rerank child serves it");
}
