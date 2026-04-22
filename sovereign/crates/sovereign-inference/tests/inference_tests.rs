use std::collections::HashMap;
use std::sync::Arc;

use sovereign_core::oicp::*;
use sovereign_core::types::*;
use sovereign_inference::health::HealthTracker;
use sovereign_inference::selector::*;

// ─── HealthTracker Tests ───────────────────────────────────────

#[test]
fn health_tracker_starts_healthy() {
    let ht = HealthTracker::new();
    assert!(ht.is_healthy());
    assert_eq!(ht.health_score(), 1.0);
    assert_eq!(ht.latency_ms(), 0);
}

#[test]
fn health_tracker_records_success() {
    let ht = HealthTracker::new();
    ht.record_success(100);
    assert!(ht.is_healthy());
    assert_eq!(ht.latency_ms(), 100); // First record sets directly.
    assert_eq!(ht.health_score(), 1.0);
}

#[test]
fn health_tracker_latency_ewma() {
    let ht = HealthTracker::new();
    ht.record_success(100);
    assert_eq!(ht.latency_ms(), 100);

    ht.record_success(200);
    // EWMA: 100 * 0.7 + 200 * 0.3 = 70 + 60 = 130
    assert_eq!(ht.latency_ms(), 130);

    ht.record_success(200);
    // EWMA: 130 * 0.7 + 200 * 0.3 = 91 + 60 = 151
    assert_eq!(ht.latency_ms(), 151);
}

#[test]
fn health_tracker_single_failure_stays_healthy() {
    let ht = HealthTracker::new();
    ht.record_success(50);
    ht.record_failure();
    assert!(ht.is_healthy());
    // 1 error out of 2 requests = 0.5 error rate → score 0.5
    assert!((ht.health_score() - 0.5).abs() < 0.01);
}

#[test]
fn health_tracker_three_failures_unhealthy() {
    let ht = HealthTracker::new();
    ht.record_failure();
    ht.record_failure();
    assert!(ht.is_healthy()); // Still healthy at 2.
    ht.record_failure();
    assert!(!ht.is_healthy()); // Unhealthy at 3.
    assert_eq!(ht.health_score(), 0.0);
}

#[test]
fn health_tracker_reset_errors() {
    let ht = HealthTracker::new();
    ht.record_failure();
    ht.record_failure();
    ht.record_failure();
    assert!(!ht.is_healthy());

    ht.reset_errors();
    assert!(ht.is_healthy());
}

#[test]
fn health_tracker_mixed_success_and_failure() {
    let ht = HealthTracker::new();
    for _ in 0..8 {
        ht.record_success(50);
    }
    ht.record_failure();
    ht.record_failure();
    assert!(ht.is_healthy());
    // 2 errors out of 10 requests = 0.2 error rate → score 0.8
    assert!((ht.health_score() - 0.8).abs() < 0.01);
}

// ─── BackendSelector Tests ─────────────────────────────────────

fn make_backend(name: &str, priority: u32, cost: Option<f64>, healthy: bool, latency: u64) -> BackendEntry {
    let health = Arc::new(HealthTracker::new());
    if latency > 0 {
        health.record_success(latency);
    }
    if !healthy {
        health.record_failure();
        health.record_failure();
        health.record_failure();
    }
    let is_local = cost.is_none();
    if is_local {
        BackendEntry::new_local(name, health, priority)
    } else {
        BackendEntry::new_remote(name, health, priority, cost)
    }
}

fn dummy_request() -> CompletionRequest {
    CompletionRequest::new("test")
}

// ── PrioritySelector ──

#[tokio::test]
async fn priority_selector_picks_lowest_priority() {
    let backends = vec![
        make_backend("high", 10, None, true, 50),
        make_backend("low", 1, None, true, 50),
        make_backend("mid", 5, None, true, 50),
    ];

    let idx = PrioritySelector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "low");
}

#[tokio::test]
async fn priority_selector_skips_unhealthy() {
    let backends = vec![
        make_backend("best", 1, None, false, 50),
        make_backend("second", 2, None, true, 50),
    ];

    let idx = PrioritySelector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "second");
}

#[tokio::test]
async fn priority_selector_all_unhealthy_errors() {
    let backends = vec![
        make_backend("a", 1, None, false, 0),
        make_backend("b", 2, None, false, 0),
    ];

    let result = PrioritySelector.select(&dummy_request(), &backends).await;
    assert!(result.is_err());
}

// ── CostMinimizingSelector ──

#[tokio::test]
async fn cost_selector_prefers_free() {
    let backends = vec![
        make_backend("paid", 1, Some(0.01), true, 50),
        make_backend("local", 2, None, true, 50),
    ];

    let idx = CostMinimizingSelector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "local");
}

#[tokio::test]
async fn cost_selector_picks_cheapest() {
    let backends = vec![
        make_backend("expensive", 1, Some(0.10), true, 50),
        make_backend("cheap", 2, Some(0.01), true, 50),
    ];

    let idx = CostMinimizingSelector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "cheap");
}

// ── LatencyMinimizingSelector ──

#[tokio::test]
async fn latency_selector_picks_fastest() {
    let backends = vec![
        make_backend("slow", 1, None, true, 200),
        make_backend("fast", 2, None, true, 10),
        make_backend("mid", 3, None, true, 100),
    ];

    let idx = LatencyMinimizingSelector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "fast");
}

// ── LocalFirstSelector ──

#[tokio::test]
async fn local_first_prefers_local() {
    let backends = vec![
        make_backend("remote", 1, Some(0.01), true, 10),
        make_backend("local", 2, None, true, 100),
    ];

    let idx = LocalFirstSelector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "local");
}

#[tokio::test]
async fn local_first_falls_back_to_remote() {
    let backends = vec![
        make_backend("remote", 1, Some(0.01), true, 10),
        make_backend("local", 2, None, false, 0),
    ];

    let idx = LocalFirstSelector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "remote");
}

#[tokio::test]
async fn local_first_all_down_errors() {
    let backends = vec![
        make_backend("remote", 1, Some(0.01), false, 0),
        make_backend("local", 2, None, false, 0),
    ];

    let result = LocalFirstSelector.select(&dummy_request(), &backends).await;
    assert!(result.is_err());
}

// ─── CapabilityAwareSelector Tests ────────────────────────────

fn make_manifest(models: Vec<ProviderModel>) -> ProviderManifest {
    ProviderManifest::new(models)
}

/// v0.3 model fixture: synthesizes a single claim from the v0.2
/// capability profile. `caps` stays in the signature so existing
/// call sites keep working until the test bodies migrate.
fn make_model(id: &str, caps: &[(Capability, u8)], context: u32) -> ProviderModel {
    let profile: CapabilityProfile = caps.iter().copied().collect();
    let hint = sovereign_core::oicp::infer_hint_from_profile(&profile);
    let best = [
        Capability::Code,
        Capability::General,
        Capability::Analysis,
        Capability::Instruction,
    ]
    .into_iter()
    .map(|c| sovereign_core::oicp::proficiency(&profile, c))
    .max()
    .unwrap_or(0);
    let affinity = (best as f32 / 4.0).clamp(0.0, 1.0);
    let claim = CapabilityClaim::new(
        hint,
        LatencyClass::Normal,
        context,
        context / 4,
        affinity,
    );
    ProviderModel {
        id: id.to_string(),
        base_model: None,
        quantization: None,
        context_tokens: context,
        status: ModelStatus {
            available: true,
            loaded: true,
            estimated_tokens_per_sec: None,
            estimated_ttft_ms: None,
            estimated_load_time_sec: None,
        },
        size_gb: None,
        claims: vec![claim],
    }
}

async fn make_oicp_backend(
    name: &str,
    priority: u32,
    manifest: Option<ProviderManifest>,
    is_local: bool,
) -> BackendEntry {
    let health = Arc::new(HealthTracker::new());
    health.record_success(50);
    let be = if is_local {
        BackendEntry::new_local(name, health, priority)
    } else {
        BackendEntry::new_remote(name, health, priority, Some(0.01))
    };
    if let Some(m) = manifest {
        *be.oicp_manifest.write().await = Some(m);
    }
    be
}

#[tokio::test]
async fn capability_selector_falls_back_without_oicp() {
    let backends = vec![
        make_backend("high", 10, None, true, 50),
        make_backend("low", 1, None, true, 50),
    ];

    let selector = CapabilityAwareSelector {
        fallback: Box::new(PrioritySelector),
    };

    // No OICP in request → falls back to priority.
    let idx = selector.select(&dummy_request(), &backends).await.unwrap();
    assert_eq!(backends[idx].name, "low");
}

#[tokio::test]
async fn capability_selector_respects_local_only() {
    let backends = vec![
        make_oicp_backend("remote", 1, None, false).await,
        make_oicp_backend("local", 2, None, true).await,
    ];

    let mut req = dummy_request();
    req.oicp = Some(InferenceRequirements::new().with_sharding(ShardingPrivacy::LocalOnly));

    let selector = CapabilityAwareSelector {
        fallback: Box::new(PrioritySelector),
    };

    let idx = selector.select(&req, &backends).await.unwrap();
    assert_eq!(backends[idx].name, "local");
}

#[tokio::test]
async fn capability_selector_picks_best_match() {
    let code_model = make_model("coder", &[(Capability::Code, 4), (Capability::General, 2)], 32768);
    let general_model = make_model("general", &[(Capability::General, 3), (Capability::Analysis, 3)], 32768);

    let backends = vec![
        make_oicp_backend("code-backend", 2, Some(make_manifest(vec![code_model])), false).await,
        make_oicp_backend("general-backend", 1, Some(make_manifest(vec![general_model])), false).await,
    ];

    // Request asks for general work → general-backend's claim
    // matches exactly (affinity 0.75), code-backend's code-specific
    // claim scores 0 on a general request and is eliminated.
    let mut req = dummy_request();
    req.oicp = Some(
        InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal)
            .with_sharding(ShardingPrivacy::MeshAllowed),
    );

    let selector = CapabilityAwareSelector {
        fallback: Box::new(PrioritySelector),
    };

    let idx = selector.select(&req, &backends).await.unwrap();
    assert_eq!(backends[idx].name, "general-backend");
}

#[tokio::test]
async fn capability_selector_filters_by_required() {
    let weak_model = make_model("weak", &[(Capability::Code, 1)], 32768);
    let strong_model = make_model("strong", &[(Capability::Code, 3)], 32768);

    let backends = vec![
        make_oicp_backend("weak-be", 1, Some(make_manifest(vec![weak_model])), false).await,
        make_oicp_backend("strong-be", 2, Some(make_manifest(vec![strong_model])), false).await,
    ];

    // Request asks for code hint. weak_model has Code:1 (below the
    // v0.3 code specialist threshold), so its synthesized hint is
    // general — general-vs-code scores 0.5 (fallback). strong_model
    // has Code:3 (still not the Code==4 threshold for code
    // specialist), so its hint is also general. They both get the
    // same 0.5 fallback — the one with the higher affinity wins on
    // tiebreak.
    let mut req = dummy_request();
    req.oicp = Some(
        InferenceRequirements::new()
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Normal)
            .with_sharding(ShardingPrivacy::MeshAllowed),
    );

    let selector = CapabilityAwareSelector {
        fallback: Box::new(PrioritySelector),
    };

    let idx = selector.select(&req, &backends).await.unwrap();
    assert_eq!(backends[idx].name, "strong-be");
}

#[tokio::test]
async fn capability_selector_falls_back_no_manifests() {
    let backends = vec![
        make_oicp_backend("a", 10, None, false).await,
        make_oicp_backend("b", 1, None, false).await,
    ];

    let mut req = dummy_request();
    req.oicp = Some(InferenceRequirements::new().with_sharding(ShardingPrivacy::MeshAllowed));

    let selector = CapabilityAwareSelector {
        fallback: Box::new(PrioritySelector),
    };

    // No manifests → falls back to priority.
    let idx = selector.select(&req, &backends).await.unwrap();
    assert_eq!(backends[idx].name, "b");
}
