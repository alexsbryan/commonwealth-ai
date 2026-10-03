// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `serves_rerank` — see `assembly.rs`.

use super::*;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, ComputeChildStatus, Depth, Error, ProviderCapabilities,
    ResidentSlot, Result, Speed,
};

/// A provider reporting what it holds, and nothing else.
struct Holds {
    slots: Vec<ResidentSlot>,
    children: Vec<ComputeChildStatus>,
}

#[async_trait::async_trait]
impl InferenceProvider for Holds {
    async fn complete(&self, _: &CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::NotImplemented("status only".into()))
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("status only".into()))
    }
    async fn embed(&self, _: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("status only".into()))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 0,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.slots.clone()
    }
    fn compute_children(&self) -> Vec<ComputeChildStatus> {
        self.children.clone()
    }
}

fn slot(role: &str) -> ResidentSlot {
    ResidentSlot {
        role: role.into(),
        model_id: format!("{role}-model"),
        resident: true,
        size_bytes: None,
        transitioning: false,
        placement: None,
    }
}

fn child(role: &str) -> ComputeChildStatus {
    ComputeChildStatus {
        name: format!("{role}-child"),
        role: role.into(),
        model_id: format!("{role}-model"),
        lifecycle: "starting".into(),
        port: None,
        restarts: 0,
        last_transition_reason: String::new(),
        last_exit: None,
    }
}

/// Daemon turns get a cross-encoder exactly when this process holds one: a
/// rerank slot its engine installed, or a rerank compute child.
#[test]
fn a_process_serves_rerank_only_when_it_holds_one() {
    let with = |slots: &[&str], children: &[&str]| Holds {
        slots: slots.iter().map(|r| slot(r)).collect(),
        children: children.iter().map(|r| child(r)).collect(),
    };
    assert!(serves_rerank(&with(&["rerank"], &[])), "the engine's slot");
    assert!(serves_rerank(&with(&[], &["rerank"])), "a rerank child");
    assert!(
        !serves_rerank(&with(&["fast", "embed"], &["generate"])),
        "no rerank slot and no rerank child: none"
    );
}

/// The config asked for a reranker and the install failed (the engine logs
/// it and holds no slot). The lane must stay unarmed: before this, the plan
/// answered yes and every search paid the overfetch for NotImplemented.
#[test]
fn a_failed_rerank_install_does_not_arm_the_lane() {
    let mut config = SetupConfig::unconfigured();
    let mut models = ModelsSection {
        primary: "/m/primary.gguf".into(),
        embed: "/m/embed.gguf".into(),
        ..Default::default()
    };
    models
        .kinds
        .insert("rerank".into(), "/m/rerank.gguf".into());
    config.models = Some(models);
    assert!(
        std::env::var_os("SOVEREIGN_RERANK_MODEL_PATH").is_none(),
        "this test reads the config key; the env var would override it"
    );
    let planned = plan_serving(&config).expect("a holder plans");
    assert!(
        planned.in_process.contains(&PlannedSlot::Rerank),
        "the config asks for the rerank slot"
    );

    let installed = Holds {
        slots: vec![slot("fast"), slot("embed")],
        children: Vec::new(),
    };
    assert!(
        !serves_rerank(&installed),
        "nothing loaded, so nothing serves rerank"
    );
}
