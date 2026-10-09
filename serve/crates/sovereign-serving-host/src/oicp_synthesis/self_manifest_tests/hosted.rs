// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hosted engine's manifest: its vendor model, and only the features an
//! OpenAI-compatible backend honours.
use super::*;
use oicp_client::{EngineEmbed, SplitInferenceProvider};
use oicp_types::features::{CONSTRAINT_LARK, OPENAI_COMPATIBLE_FEATURES, X_FORCED_CHOICE};

/// THE FAILING INPUT: a remote engine reported no resident slots, so its node
/// advertised no models and no peer could route to its vendor model.
#[test]
fn a_hosted_engine_advertises_its_vendor_model_and_vendor_features() {
    let embed = EngineEmbed::Remote {
        endpoint_v1: "http://127.0.0.1:8001/v1".into(),
        model_id: "embed".into(),
        input_prep: None,
    };
    let engine = SplitInferenceProvider::engine(
        "https://api.example.com/v1",
        embed,
        None,
        "vendor-model".into(),
        None,
        65536,
        None,
        Default::default(),
    )
    .unwrap();
    let manifest = build_self_manifest(&engine, &NoManifest);
    let model = manifest
        .models
        .iter()
        .find(|m| m.id == "vendor-model")
        .unwrap_or_else(|| panic!("vendor model not advertised: {:?}", manifest.models));
    assert!(model.status.available && model.status.loaded, "{model:?}");
    assert_eq!(manifest.features, OPENAI_COMPATIBLE_FEATURES);
}

/// The other side: a node holding its own weights keeps the embedded
/// features, lark grammars and forced choice included.
#[test]
fn a_node_with_weights_keeps_the_embedded_features() {
    let stub = SlotStub {
        fast_id: "fast.Q4_0",
        primary_id: "primary.Q5_K_M",
        code_id: None,
        slots: WARM_FAST_AND_PRIMARY,
    };
    let manifest = build_self_manifest(&stub, &NoManifest);
    for feature in [CONSTRAINT_LARK, X_FORCED_CHOICE] {
        assert!(manifest.features.iter().any(|f| f == feature), "{feature}");
    }
}
