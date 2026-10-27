// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hot reload builds exactly what cold start builds (pb-serving-assembly) —
//! see `assembly.rs`. Moved from the svrn daemon's provider.rs by
//! pb-serve-distributes, where they drove the same `ReloadFactory` through the
//! daemon's in-process reload path; that path is gone and the factory is
//! serve's.
//!
//! The configs that held weights in this process (`kind = "llama"`, the
//! distributed primary's child) went with the embedded engine
//! (ENGINE_SWAP_PLAN G2); both paths now refuse them by name, which the last
//! three tests pin.

use super::*;
use sovereign_contracts::setup_config::{ComputeSlotConfig, EngineSection, ModelsSection};

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

/// A node naming `[models]` and the engine `kind` names.
fn holder(kind: EngineKind) -> SetupConfig {
    let dir = models_dir();
    let mut cfg = SetupConfig::unconfigured();
    cfg.engine.kind = kind;
    cfg.data.dir = dir.join("data-never-written");
    cfg.models = Some(ModelsSection {
        primary: dir.join("big-primary.gguf"),
        fast: Some(dir.join("small-fast.gguf")),
        embed: dir.join("Qwen3-Embedding-0.6B-Q8_0.gguf"),
        ..Default::default()
    });
    cfg
}

fn remote() -> SetupConfig {
    let mut cfg = holder(EngineKind::Remote);
    cfg.engine = EngineSection {
        kind: EngineKind::Remote,
        endpoint: Some("http://127.0.0.1:1/v1".to_string()),
        model_id: Some("some-remote-model".to_string()),
        ..Default::default()
    };
    cfg
}

fn slot(name: &str, role: &str) -> ComputeSlotConfig {
    ComputeSlotConfig {
        name: name.into(),
        role: role.into(),
        model: models_dir().join("small-fast.gguf"),
        context_size: None,
        n_gpu_layers: None,
        warm: false,
        capture_embed: false,
    }
}

/// A running compute generation with no process in it: a remote engine holds
/// no weights, and a cold slot spawns nothing.
fn remote_with_a_cold_child() -> SetupConfig {
    let mut cfg = remote();
    cfg.compute.enabled = true;
    cfg.compute.slot.push(slot("crash", "mock"));
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
        ("remote", remote()),
        ("remote_with_a_cold_child", remote_with_a_cold_child()),
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
    let remote = slot_set(reload_path().build(&remote()));
    assert_eq!(remote.engine, EngineKind::Remote);
    assert!(
        remote.in_process.is_empty(),
        "a remote-kind node holds no local slots: {remote:?}"
    );

    let child = slot_set(reload_path().build(&remote_with_a_cold_child()));
    assert!(
        child.children.iter().any(|(name, _)| name == "crash"),
        "the declared slot is planned as a child: {child:?}"
    );
}

/// What a reload installs, as the reload route reports it: the provider, or
/// the refusal prefixed as the route prefixes it.
fn install(reload: &Arc<ReloadFactory>, cfg: &SetupConfig) -> Result<ServingParts, String> {
    reload.build(cfg).map_err(|e| format!("reload: {e}"))
}

/// A reload against the compute generation cold start started: the same
/// children are re-wrapped; different children are refused by name.
#[tokio::test]
async fn build_provider_rewraps_the_running_children_and_refuses_new_ones() {
    let cfg = remote_with_a_cold_child();
    let cold = assemble_serving(&cfg).expect("a remote engine needs no weights");
    let reload = Arc::clone(&cold.reload_factory);
    if let Err(e) = install(&reload, &cfg) {
        panic!("the same children must reload: {e}");
    }

    let mut changed = cfg.clone();
    changed.compute.slot.push(slot("another", "mock"));
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
    assert_eq!(
        reload.provider.resident_slots().len(),
        cold.provider.resident_slots().len()
    );
}

/// THE FAILING INPUT: a config still naming the deleted in-process engine.
/// It must be refused with the replacement, at cold start and on reload,
/// and never be read as an unknown custom engine or load anything.
#[test]
fn kind_llama_is_refused_by_name_on_both_paths() {
    let cfg = holder(EngineKind::Llama);
    for (label, built) in [
        ("cold", assemble_serving(&cfg)),
        ("reload", reload_path().build(&cfg)),
    ] {
        let Err(e) = built else {
            panic!("{label}: kind = \"llama\" must be refused");
        };
        assert!(
            e.reason.contains("retired") && e.reason.contains("llama-server"),
            "{label}: the refusal names the replacement: {}",
            e.reason
        );
        let plan = e.plan.expect("the config planned before it was refused");
        assert!(plan.in_process.is_empty(), "{label}: {plan:?}");
    }
}

/// `[compute] distributed_primary` hosted its model in a child running the
/// embedded engine; with that engine gone it is refused on every engine,
/// never planned as a child that could not load.
#[test]
fn distributed_primary_is_refused_on_every_engine() {
    // `llama-server` refused it already, before the engine went; a router
    // config plans its preset from GGUF headers these files do not have.
    for kind in [EngineKind::Remote, EngineKind::Llama] {
        let mut cfg = if kind == EngineKind::Remote {
            remote()
        } else {
            holder(kind.clone())
        };
        cfg.compute.enabled = true;
        cfg.compute.distributed_primary = true;
        let Err(e) = reload_path().build(&cfg) else {
            panic!("{kind}: distributed_primary must be refused");
        };
        assert!(
            e.reason.contains("distributed_primary is retired"),
            "{kind}: {}",
            e.reason
        );
    }
}

/// The generate and embed roles are refused before anything spawns, on the
/// engine that used to run them.
#[test]
fn a_generate_or_embed_slot_is_refused_before_anything_spawns() {
    let mut cfg = remote();
    cfg.compute.enabled = true;
    cfg.compute.slot.push(slot("talker", "generate"));
    let Err(e) = assemble_serving(&cfg) else {
        panic!("a generate slot must be refused");
    };
    assert!(
        e.reason.contains("talker") && e.reason.contains("retired"),
        "{}",
        e.reason
    );
}
