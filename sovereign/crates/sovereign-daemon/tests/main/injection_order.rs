// SPDX-License-Identifier: AGPL-3.0-or-later
//! The service-injection hazards that used to be *remembered* are now
//! structural.
//!
//! `AppState::with_local_inference` mutated `AppStateInner` through
//! `Arc::get_mut`, which returns `None` the moment any other code has cloned
//! `app_state.inner` — the installer then became a `tracing::error!` and a
//! quiet return, NOT a panic or an error result, so the daemon booted with no
//! local inference and 503'd every chat turn.
//!
//! The value is a constructor argument now (`ServingSeed::local_inference`;
//! DC §4.2 "Construction is staged, and parts are total"). The hazard is gone
//! structurally (ARCH 10): there is no installer to re-order, so this test
//! asserts the value is present the moment the state exists rather than
//! capturing a log line that a bad ordering would emit. (The mesh-mutation
//! hook this file also pinned left with the daemon's roster,
//! pb-mesh-exit-transport.)
use std::sync::Arc;

use kernel_types::NodeId;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_daemon::state::{AppState, LocalInferenceService, ServingSeed};

use crate::common::service_double::ProviderService;
use crate::common::TestProvider;

#[test]
fn local_inference_is_present_at_construction() {
    // The provider used to ride the `Arc::get_mut` installer; it is a
    // constructor argument now, so a future refactor cannot re-order a clone
    // ahead of it (ARCH 10 — structural, not remembered).
    let provider: Arc<dyn InferenceProvider> = Arc::new(TestProvider::new());
    let adapter: Arc<dyn LocalInferenceService> = ProviderService::new(provider);
    let app_state = AppState::new_with_serving(
        NodeId::from_u128(0xDEAD_BEEF_CAFE_F00D),
        ServingSeed {
            local_inference: Some(adapter),
            ..Default::default()
        },
    );

    assert!(
        app_state.inner.serving.local_inference.is_some(),
        "a provider passed at construction must be present"
    );
}
