// SPDX-License-Identifier: AGPL-3.0-or-later
//! SLOT_POLICY §2.4 for the intent classify: the envelope it builds must
//! THREAD the session's sharding posture, not hardcode one.
//!
//! Its own file because the fixtures below build a real [`LlmRouter`] over a
//! capturing provider and an in-memory store, and that weight pushed
//! `router.rs` past its arch-gate slack (ARCH §3.1).

use super::*;
use crate::skills::SkillRegistry;
use std::sync::Arc;

/// Captures the `CompletionRequest` the router hands to inference, so a
/// test can read the envelope the classify actually built.
struct CapturingInference {
    seen: std::sync::Mutex<Vec<CompletionRequest>>,
}

#[async_trait::async_trait]
impl InferenceProvider for CapturingInference {
    async fn complete(
        &self,
        r: &sovereign_contracts::types::CompletionRequest,
    ) -> Result<sovereign_contracts::types::CompletionResponse> {
        self.seen.lock().unwrap().push(r.clone());
        Ok(sovereign_contracts::types::CompletionResponse {
            text: "A".to_string(),
            tokens_used: 1,
            prompt_tokens: 1,
            model_id: "capture".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _r: &sovereign_contracts::types::CompletionRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        Err(sovereign_contracts::error::Error::NotImplemented(
            "unused".into(),
        ))
    }
    async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 4])
    }
    async fn embed_query(&self, _q: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 4])
    }
    fn capabilities(&self) -> sovereign_contracts::types::ProviderCapabilities {
        sovereign_contracts::types::ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: sovereign_contracts::types::Speed::Fast,
            relative_reasoning: sovereign_contracts::types::Depth::Shallow,
        }
    }
}

fn router_with(skills: SkillRegistry) -> (LlmRouter, Arc<CapturingInference>) {
    let inference = Arc::new(CapturingInference {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let store = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let router = LlmRouter::new(
        Arc::clone(&inference) as Arc<dyn InferenceProvider>,
        store,
        Arc::new(skills),
    );
    (router, inference)
}

/// SLOT_POLICY §2.4, made a test rather than a sentence (principle 10).
///
/// The intent classify must THREAD the session's sharding posture. All
/// three classify calls used `Workload::request`, which hardcodes
/// `LocalOnly`, until 2026-09-19 — and `offload_verdict` refuses on
/// privacy BEFORE it ever looks at the latency class, so that one
/// constant is what pinned a 38.2-second one-letter classify to a CPU
/// node with an idle peer 37 ms away
/// (`target/ring-room-demo/a/daemon.err` 2026-09-19T20:18:44Z, the
/// `wl-route-05c1c73f` decision and its outcome).
///
/// Revert any of the three sites to `.request(prompt)` and this goes red.
#[tokio::test]
async fn every_intent_classify_threads_the_session_posture() {
    // A default session declares no skill, so the merge resolves
    // `MeshAllowed` — the same posture this turn's synthesis carries.
    let (router, inference) = router_with(SkillRegistry::new());

    router
        .classify_call("hello".into())
        .await
        .expect("classify");
    router
        .classify_call_json("hello".into(), 32)
        .await
        .expect("classify json");
    router
        .classify_call_tool_json("hello".into(), &["t".to_string()])
        .await
        .expect("tool select");

    let seen = inference.seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "all three classify paths ran");
    for (i, req) in seen.iter().enumerate() {
        let oicp = req
            .oicp
            .as_ref()
            .unwrap_or_else(|| panic!("classify {i} must carry a Workload::Route envelope"));
        assert_eq!(
            oicp.effective_latency_class(),
            sovereign_contracts::oicp::LatencyClass::Fast,
            "classify {i} stays SLOT_POLICY §3 Route class",
        );
        assert_eq!(
            oicp.sharding(),
            sovereign_contracts::oicp::ShardingPrivacy::MeshAllowed,
            "classify {i} hardcodes LocalOnly — `offload_verdict` refuses \
             it on privacy and the scheduler never sees the mesh",
        );
    }
}

/// The other direction, and the reason this threads a posture rather than
/// swapping one hardcoded constant for another: a session whose active
/// skill declares `privacy = local_only` keeps its classify at home.
#[tokio::test]
async fn a_local_only_skill_keeps_the_classify_off_the_mesh() {
    let mut skills = SkillRegistry::new();
    skills.register(
        sovereign_contracts::skills::parse_skill_toml(
            r#"
[skill]
id = "inner-work"
name = "inner-work"
version = "0.1.0"

[inference]
privacy = "local_only"
"#,
        )
        .expect("the fixture parses"),
    );
    skills.activate("inner-work");

    let (router, inference) = router_with(skills);
    router
        .classify_call("hello".into())
        .await
        .expect("classify");

    let seen = inference.seen.lock().unwrap();
    assert_eq!(
        seen[0].oicp.as_ref().expect("envelope").sharding(),
        sovereign_contracts::oicp::ShardingPrivacy::LocalOnly,
        "a local-only session must not put its classify on the wire",
    );
}
