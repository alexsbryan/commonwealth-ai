// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! Cross-router loopback parity test.
//!
//! This test exercises both the mesh's loopback-only routers AND the
//! `project_http` / `reindexer` SCIP-graph routers; the latter live
//! behind the `treesitter` feature, so the entire test is gated to
//! match. `cargo test -p sovereign-mesh --features treesitter` runs
//! it; the default `cargo test -p sovereign-mesh` skips it.
//!
//! Every loopback-only router in this crate layers the same
//! `loopback_guard::loopback_only` middleware AND a per-handler
//! `enforce_localhost` call (ARCH §5 defense in depth). The unit
//! tests in `loopback_guard` pin the middleware in isolation; the
//! per-router tests pin the helper.
//!
//! # What this file proves, corrected (2026-09-10)
//!
//! Until now the paragraph above ended "a route added without the
//! middleware (or with a misordered layer stack) would slip past the
//! per-router tests but fail here." **That claim was false and a run
//! falsified it** (`scripts/twin-census.py`, families
//! `mesh-loopback-parity` and `mesh-loopback-spoof`, verdict
//! NOT-A-GATE, commit f6a633519). Re-watched on this tree with
//! `mesh_router`'s `.layer(from_fn(loopback_only))` deleted:
//!
//! ```text
//! mesh_http_rejects_non_loopback_via_mesh_status      ok
//! every_router_fails_closed_when_connect_info_absent  ok
//! loopback_caller_reaches_mesh_status                 ok
//! every_router_refuses_a_request_no_handler_…    FAILED  left: 405  right: 403
//! ```
//!
//! Defence in depth is what blinds the first three: `mesh_status`
//! extracts `ConnectInfo` and calls `enforce_localhost` itself, so the
//! spoofed LAN caller is refused by the HANDLER and the
//! ConnectInfo-less caller 500s in the extractor. Both assertions are
//! over-determined, and the two guards deliberately answer with the
//! same status AND the same body (`{"error":"local-only"}`, one
//! decider), so no body assertion separates them either.
//!
//! `every_router_refuses_a_request_no_handler_of_ours_can_refuse` is
//! the gate — it is the only test here whose red is evidence about the
//! LAYER. Read it before adding a router to this file; the spoof and
//! fail-closed families remain useful as the per-router and
//! fail-closed contracts, but neither is evidence the middleware is
//! mounted.
//!
//! Approach: build each router with minimal deps, wrap it with an
//! `outer` middleware that **spoofs** `ConnectInfo` to a non-loopback
//! socket address before the real `loopback_only` middleware runs.
//! Then hit a representative route and assert 403. This is more
//! reliable than the existing test in `loopback_guard.rs` that
//! depends on a routable interface being present on the host.
//!
//! Layer order: `.layer(outer)` runs *before* the inner router's
//! `.layer(loopback_only)` because axum applies layers in reverse-
//! addition order. So the spoofed `ConnectInfo` is in place by the
//! time `loopback_only` reads it.
//!
//! Split into topic parts under `loopback_parity/` for the §3.2 size
//! ceiling (behaviour-preserving move): `guard_families` (spoof,
//! fail-closed, method-fallback gate), `surface_parity` (corpus status,
//! conversation CRUD, search, memory), `insight_and_record` (insight
//! surface, record route).
#[path = "loopback_parity/guard_families.rs"]
mod guard_families;
#[path = "loopback_parity/insight_and_record.rs"]
mod insight_and_record;
#[path = "loopback_parity/surface_parity.rs"]
mod surface_parity;

use crate::common::TestProvider;

use std::sync::Arc;

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::traits::StateStore;
use sovereign_contracts::types::{Message, Role};
use sovereign_daemon::daemon::EmbeddedDaemon;

// ── sv-surface rung 3: the conversation CRUD routes serve the store's ────
// ── rows through the canonical projections ───────────────────────────────
//
// The parity instrument for the conversation family. sovereign-server's
// routes.rs is the CONTRACT (mobile already speaks it); these daemon routes
// mirror its envelopes field-for-field and call the SAME projection deciders
// the server calls (`sovereign_contracts::types::projection`). Parity here
// means: on the SAME fixture rows, the route's bytes equal the wire schema's
// serialization of those rows — the route adds nothing, drops nothing, and
// re-derives nothing. The desktop's in-process reads answer from the same
// `StateStore` rows through the same trait methods, so one-decider-on-the-row
// is what this pins; the desktop's HTTP conversion is rung 6.

/// A seeded conversation row: one user turn, one assistant turn whose metadata
/// projects to provenance + citations + an epistemic ledger. The assistant
/// metadata is the interesting half — it is what separates "serves the row"
/// from "serves the row's projection faithfully".
pub(crate) async fn seed_conversation(
    store: &Arc<dyn StateStore>,
    id: &str,
    title: Option<&str>,
    with_metadata: bool,
) {
    let m1 = Message {
        id: format!("{id}-m1"),
        conversation_id: id.to_string(),
        role: Role::User,
        content: "what is compatibilism?".to_string(),
        created_at: 100,
        metadata: None,
        version: 0,
    };
    let m2 = Message {
        id: format!("{id}-m2"),
        conversation_id: id.to_string(),
        role: Role::Assistant,
        content: "Compatibilism holds that free will is compatible with determinism.".to_string(),
        created_at: 101,
        metadata: with_metadata.then(|| {
            serde_json::json!({
                "provenance": {
                    "inference_backend": "test-provider",
                    "coarse_intent": "DeepQuery",
                    "total_latency_ms": 42,
                    "sources": [{ "origin": "sep", "count": 2 }]
                },
                "retrieved_chunks": [{
                    "corpus_id": "sep",
                    "chunk_id": 7,
                    "snippet": "Compatibilism holds that...",
                    "score": 0.9,
                    "title": "Free Will"
                }],
                "epistemic_state": {
                    "version": 1,
                    "demands": [],
                    "holdings": [],
                    "gaps": [],
                    "verdict": "grounded",
                    "citations": []
                }
            })
        }),
        version: 0,
    };
    store
        .insert_empty_conversation(id, 100, None)
        .await
        .unwrap();
    store.save_message(&m1).await.unwrap();
    store.save_message(&m2).await.unwrap();
    if let Some(t) = title {
        store.update_conversation_title(id, t).await.unwrap();
    }
}

/// The rung-3 fixture: a serving daemon over a store the test also holds —
/// the same shape `turn_surface.rs` uses, so the CRUD routes are exercised
/// against the exact wiring a turn already runs on.
pub(crate) async fn conversation_fixture(
    provider: TestProvider,
) -> (tempfile::TempDir, Arc<EmbeddedDaemon>, Arc<dyn StateStore>) {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let services =
        crate::common::desktop_services_with_store(engine, Arc::clone(&store), Arc::new(provider));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    );
    seed_conversation(&store, "alpha", Some("Free will"), true).await;
    seed_conversation(&store, "beta", None, false).await;
    // `alpha` is SCOPED and `beta` is not, so the get route's
    // `enabled_corpora` has both a present and an absent case to serve. The
    // pair is the gate: a route hard-coding `None` passes the second alone.
    store
        .set_conversation_enabled_corpora(
            "alpha",
            Some(vec!["sep".to_string(), "wikipedia".to_string()]),
        )
        .await
        .unwrap();
    (tmp, daemon, store)
}
