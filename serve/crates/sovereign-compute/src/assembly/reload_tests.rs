// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `ReloadFactory`'s refusal — see `assembly.rs`.

use super::*;
use sovereign_contracts::setup_config::{ComputeSlotConfig, EngineSection, ModelsSection};

/// A running compute generation with no process in it: a remote engine holds
/// no weights, and a cold slot spawns nothing.
fn remote_with_a_cold_child() -> SetupConfig {
    let mut config = SetupConfig::unconfigured();
    config.data.dir = std::env::temp_dir().join(format!("pb-reload-{}", std::process::id()));
    config.models = Some(ModelsSection {
        primary: "/m/big-primary.gguf".into(),
        fast: Some("/m/small-fast.gguf".into()),
        embed: "/m/embed.gguf".into(),
        ..Default::default()
    });
    config.engine = EngineSection {
        kind: EngineKind::Remote,
        endpoint: Some("http://127.0.0.1:1/v1".to_string()),
        model_id: Some("some-remote-model".to_string()),
        ..Default::default()
    };
    config.compute.enabled = true;
    config.compute.slot.push(ComputeSlotConfig {
        name: "crash".into(),
        role: "mock".into(),
        model: "/m/none.gguf".into(),
        context_size: None,
        n_gpu_layers: None,
        warm: false,
        capture_embed: false,
    });
    config
}

/// The refusal is an event at a target the daemon's filter admits, not only
/// the error string the admin route returns.
#[tokio::test]
async fn a_refused_reload_is_logged_where_the_daemon_filter_admits_it() {
    let config = remote_with_a_cold_child();
    let cold = assemble_serving(&config).expect("a remote engine needs no weights");
    let mut changed = config.clone();
    changed.compute.slot.push(ComputeSlotConfig {
        name: "another".into(),
        role: "mock".into(),
        model: "/m/small-fast.gguf".into(),
        context_size: None,
        n_gpu_layers: None,
        warm: false,
        capture_embed: false,
    });
    let (refused, log) = crate::logged_under("compute_child=info", || {
        cold.reload_factory.build(&changed).is_err()
    });
    assert!(refused, "different children must be refused");
    assert!(
        log.contains("reload refused"),
        "no refusal event at compute_child: {log:?}"
    );
}
