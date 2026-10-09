// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use oicp_types::{
    CapabilityClaim, CapabilityHint, IngestEndpoints, KnowledgeManifest, LatencyClass,
    ModelStatus, ProviderModel,
};

fn model(id: &str, ctx: u32, claims: Vec<CapabilityClaim>) -> ProviderModel {
    ProviderModel {
        id: id.into(),
        base_model: None,
        quantization: None,
        context_tokens: ctx,
        status: ModelStatus {
            available: true,
            loaded: true,
            estimated_tokens_per_sec: None,
            estimated_ttft_ms: None,
            estimated_load_time_sec: None,
        },
        size_gb: None,
        claims,
        fingerprint: None,
    }
}

fn claim(affinity: f32, max_context: u32) -> CapabilityClaim {
    CapabilityClaim {
        hint: CapabilityHint::general(),
        latency_class: LatencyClass::Normal,
        max_context,
        max_output: 512,
        affinity,
    }
}

#[test]
fn clean_manifest_has_no_invariant_failures() {
    let m = ProviderManifest::new(vec![model("m", 8192, vec![claim(0.9, 8000)])]);
    assert!(manifest_invariant_failures(&m).is_empty());
}

#[test]
fn affinity_out_of_range_is_flagged() {
    let m = ProviderManifest::new(vec![model("m", 8192, vec![claim(1.5, 8000)])]);
    assert_eq!(manifest_invariant_failures(&m).len(), 1);
}

#[test]
fn claim_context_exceeding_model_is_flagged() {
    let m = ProviderManifest::new(vec![model("m", 8192, vec![claim(0.5, 9000)])]);
    let f = manifest_invariant_failures(&m);
    assert_eq!(f.len(), 1);
    assert!(f[0].contains("exceeds context_tokens"));
}

#[test]
fn unknown_feature_is_flagged_but_x_prefix_is_ok() {
    let mut m = ProviderManifest::new(vec![model("m", 8192, vec![])]);
    m.features = vec!["x:custom-thing".into(), "not-a-real-feature".into()];
    let f = feature_failures(&m);
    assert_eq!(f.len(), 1);
    assert!(f[0].contains("not-a-real-feature"));
}

#[test]
fn ingest_feature_must_match_ingest_section() {
    // ingest:v1 advertised but no knowledge.ingest → failure.
    let mut m = ProviderManifest::new(vec![model("m", 8192, vec![])]);
    m.features = vec![features::INGEST_V1.into()];
    assert_eq!(feature_failures(&m).len(), 1);

    // Now add the section → consistent.
    m.knowledge = Some(KnowledgeManifest {
        corpora: vec![],
        search_endpoint: "/v1/knowledge/search".into(),
        embed_model: None,
        ingest: Some(IngestEndpoints {
            install_endpoint: "/oicp/v1/corpus/install".into(),
            progress_endpoint: "/oicp/v1/corpus/progress".into(),
            test_endpoint: None,
        }),
        evidence: None,
    });
    assert!(feature_failures(&m).is_empty());
}
