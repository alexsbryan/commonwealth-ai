// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hot reload builds exactly what cold start builds (pb-serving-assembly) —
//! see `assembly.rs`. Moved from the svrn daemon's provider.rs by
//! pb-serve-distributes, where they drove the same `ReloadFactory` through the
//! daemon's in-process reload path; that path is gone and the factory is
//! serve's.
//!
//! Until 2026-09-26 the reload loaded GGUFs itself: it never read
//! `[engine] kind` or `[compute] distributed_primary`, so a remote-kind node
//! loaded weights on reload and a distributed-primary node loaded the
//! withheld primary in-process. The GGUFs here are files holding no model,
//! so any build that reaches llama.cpp fails on the header — after it has
//! planned. (A missing file would not do: the vendored loader
//! `debug_assert!`s that the path exists.) The plan is the slot set each path
//! asked for, compared whether or not it loaded.

use super::*;
use sovereign_contracts::setup_config::{ComputeSlotConfig, EngineSection};

/// A directory of three files named like GGUFs, holding no model.
fn models_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pb-serving-assembly-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp models dir");
    for name in [
        "big-primary.gguf",
        "small-fast.gguf",
        "Qwen3-Embedding-0.6B-Q8_0.gguf",
    ] {
        std::fs::write(dir.join(name), b"not a gguf").expect("write a non-model file");
    }
    dir
}

fn primary() -> PathBuf {
    models_dir().join("big-primary.gguf")
}

fn holder() -> SetupConfig {
    let dir = models_dir();
    let mut cfg = SetupConfig::unconfigured();
    cfg.data.dir = dir.join("data-never-written");
    cfg.models = Some(ModelsSection {
        primary: primary(),
        fast: Some(dir.join("small-fast.gguf")),
        embed: dir.join("Qwen3-Embedding-0.6B-Q8_0.gguf"),
        ..Default::default()
    });
    cfg
}

fn remote() -> SetupConfig {
    let mut cfg = holder();
    cfg.engine = EngineSection {
        kind: EngineKind::Remote,
        endpoint: Some("http://127.0.0.1:1/v1".to_string()),
        model_id: Some("some-remote-model".to_string()),
        ..Default::default()
    };
    cfg
}

fn distributed_primary() -> SetupConfig {
    let mut cfg = holder();
    cfg.compute.enabled = true;
    cfg.compute.distributed_primary = true;
    cfg
}

/// The reload path on a node whose cold start started no compute children.
fn reload_path() -> Arc<ReloadFactory> {
    Arc::default()
}

fn slot_set(built: Result<ServingParts, AssemblyError>) -> ServingPlan {
    match built {
        Ok(parts) => parts.plan,
        Err(e) => e
            .plan
            .unwrap_or_else(|| panic!("the build refused before planning: {}", e.reason)),
    }
}

#[test]
fn a_reload_builds_the_cold_start_slot_set() {
    for (label, cfg) in [
        ("llama", holder()),
        ("remote", remote()),
        ("distributed_primary", distributed_primary()),
    ] {
        let cold = slot_set(assemble_serving(&cfg));
        let reload = slot_set(reload_path().build(&cfg));
        assert_eq!(
            reload, cold,
            "{label}: the reload path must build the slot set cold start builds"
        );
    }
}

/// Equal is not enough: two paths can agree on the wrong set. Each
/// config's set is the one it asks for.
#[test]
fn each_config_plans_the_slots_it_asks_for() {
    let llama = slot_set(reload_path().build(&holder()));
    assert_eq!(llama.engine, EngineKind::Llama);
    assert!(
        llama.in_process.contains(&PlannedSlot::Primary),
        "{llama:?}"
    );
    assert_eq!(
        llama.embed_family,
        ModelFamily::Qwen3Embedding,
        "embed keeps its manifest family"
    );

    let remote = slot_set(reload_path().build(&remote()));
    assert_eq!(remote.engine, EngineKind::Remote);
    assert!(
        remote.in_process.is_empty(),
        "a remote-kind node holds no local slots: {remote:?}"
    );

    let dp = slot_set(reload_path().build(&distributed_primary()));
    assert!(
        !dp.in_process.contains(&PlannedSlot::Primary),
        "the primary is withheld from this process: {dp:?}"
    );
    assert!(
        dp.children.iter().any(|(_, model)| model == &primary()),
        "a compute child owns the primary: {dp:?}"
    );
}

/// A running compute generation with no process in it: a remote engine
/// holds no weights, and the distributed primary's child stays unspawned
/// until a warmed worker set exists.
fn remote_distributed_primary() -> SetupConfig {
    let mut cfg = remote();
    cfg.compute.enabled = true;
    cfg.compute.distributed_primary = true;
    cfg
}

/// What a reload installs, as the reload route reports it: the provider, or
/// the refusal prefixed as the route prefixes it.
fn install(reload: &Arc<ReloadFactory>, cfg: &SetupConfig) -> Result<ServingParts, String> {
    reload.build(cfg).map_err(|e| format!("reload: {e}"))
}

/// A reload against the compute generation cold start started. The same
/// children are re-wrapped and the provider it installs still holds the
/// child's primary; different children are refused by name. Asserted on
/// what installed, not the plan.
#[tokio::test]
async fn build_provider_rewraps_the_running_children_and_refuses_new_ones() {
    let cfg = remote_distributed_primary();
    let cold = assemble_serving(&cfg).expect("a remote engine needs no weights");
    let child = cold
        .distributed_primary
        .clone()
        .expect("cold start registers the distributed-primary child");
    let reload = Arc::clone(&cold.reload_factory);

    let installed = match install(&reload, &cfg) {
        Ok(parts) => parts.provider,
        Err(e) => panic!("the same children must reload: {e}"),
    };
    let primary = installed
        .resident_slots()
        .into_iter()
        .find(|s| s.role == "primary")
        .expect("the reloaded provider must still route the primary to its child");
    assert_eq!(primary.model_id, "big-primary");
    let rebuilt = reload.build(&cfg).expect("same children");
    assert!(
        rebuilt
            .distributed_primary
            .is_some_and(|slot| Arc::ptr_eq(&slot, &child)),
        "a reload wraps the running child; it never registers a second one"
    );

    let mut changed = cfg.clone();
    changed.compute.slot.push(ComputeSlotConfig {
        name: "another".into(),
        role: "generate".into(),
        model: models_dir().join("small-fast.gguf"),
        context_size: None,
        n_gpu_layers: None,
        warm: false,
        capture_embed: false,
    });
    match install(&reload, &changed) {
        Ok(_) => panic!("a reload that asks for different children must refuse"),
        Err(e) => assert!(e.contains("restart the daemon"), "got: {e}"),
    }
}

/// A remote-kind node reloads with no weights on disk, as it boots.
#[test]
fn a_remote_reload_needs_no_weights() {
    let cold = assemble_serving(&remote()).expect("cold start needs no weights");
    let reload = reload_path()
        .build(&remote())
        .expect("the reload needs no weights either");
    assert!(reload.llama.is_none() && cold.llama.is_none());
    assert_eq!(
        reload.provider.resident_slots().len(),
        cold.provider.resident_slots().len()
    );
}
