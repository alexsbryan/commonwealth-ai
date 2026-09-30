// SPDX-License-Identifier: AGPL-3.0-or-later
//! serve ranks inference venues (pb-serve-ranks, phase-b-21, phase-b-23): the
//! ONE construction of this node's `InferenceRouter`, moved here from the svrn
//! daemon's bootstrap. A composition root hands it the provider to rank over
//! (serve's reload cell, or a terminal's forwarder) and the roster and
//! identity ports, and gets back the router with the OpenAI face, gauge and
//! alias sink over it. A reload swaps the cell under the router, so a node
//! builds it once, cold start and reload alike.
//!
//! The ports are contracts': until the flip, the stock distribution hands in
//! the svrn daemon's own roster (cw-rails' is solo on a daemon-founded mesh,
//! phase-b-33).

use std::collections::HashMap;
use std::sync::Arc;

use sovereign_contracts::traits::LocalInferenceService;
use sovereign_contracts::venue::VenueSource;
use sovereign_contracts::venue_host::VenueHost;
use sovereign_contracts::InferenceProvider;
use sovereign_serving_host::peer_inference::InferenceRouter;

/// The router one [`rank`] built, and what a host serves through it.
pub struct Ranking {
    /// The router itself, for the distribution whose self-manifest follows it.
    pub router: Arc<InferenceRouter>,
    /// Where every turn goes: the router, as a provider.
    pub provider: Arc<dyn InferenceProvider>,
    /// The OpenAI face over the router (`SovereignInferenceAdapter`).
    pub service: Arc<dyn LocalInferenceService>,
    /// The in-flight gauge the router's guards write, minted before the
    /// router, which the host's gossip reads.
    pub in_flight: sovereign_contracts::in_flight::LocalInFlightGauge,
    /// Pushes the host's slot-alias map into the router.
    pub slot_aliases: Arc<dyn Fn(HashMap<String, String>) + Send + Sync>,
}

/// Rank over `provider`: build the router over the peers `venues` names and
/// the pinned worker pods persisted on disk, identified by `host`.
pub async fn rank(
    provider: Arc<dyn InferenceProvider>,
    venues: Arc<dyn VenueSource>,
    host: Arc<dyn VenueHost>,
) -> Ranking {
    let (router, gauge) = build_mesh_provider(provider, venues, host).await;
    let service: Arc<dyn LocalInferenceService> = Arc::new(
        sovereign_serving_host::inference_adapter::SovereignInferenceAdapter::new(
            Arc::clone(&router) as Arc<dyn InferenceProvider>,
            Arc::new(sovereign_serving_host::slot_manifest::CoreSlotManifest),
        ),
    );
    let sink_router = Arc::clone(&router);
    Ranking {
        provider: Arc::clone(&router) as Arc<dyn InferenceProvider>,
        router,
        service,
        in_flight: gauge,
        slot_aliases: Arc::new(move |map| sink_router.set_slot_aliases(map)),
    }
}

/// Build the mesh-routed inference provider: the roster's peers composited
/// with pinned worker pods loaded from disk.
///
/// The host's handle to its own mesh arrives as the two ports, because the
/// wiring is genuinely cyclic (the host serves peers through a provider that
/// routes to peers): the svrn daemon hands its `DeferredDaemon`, which answers
/// as a commissioned-but-stopped daemon until it is bound (§10.6).
async fn build_mesh_provider(
    provider: Arc<dyn InferenceProvider>,
    venues: Arc<dyn VenueSource>,
    host: Arc<dyn VenueHost>,
) -> (
    Arc<InferenceRouter>,
    sovereign_contracts::in_flight::LocalInFlightGauge,
) {
    // Wrap the raw `EmbeddedLlamaCpp` in `InferenceRouter`
    // before installing it as the daemon's serving provider.
    //
    // Without this wrapper the daemon's HTTP `/v1/chat/completions`
    // path silently substitutes a local model whenever the request
    // names a model that's only advertised by a peer (e.g. asking
    // for `gemma-4-E4B-it-Q4_K_M` on a node that only loads
    // `Qwen3.5-9B` and `35B-Q6` would answer with 35B-Q6 and stamp
    // the response accordingly). The wrapper inspects
    // `request.model_id` and either:
    //   * serves locally when self_manifest advertises the id
    //     (the local provider's slot picker handles Fast/Primary/
    //     Code/extras matching by name), or
    //   * forwards the request over HTTP to the peer whose manifest
    //     advertises the id, or
    //   * returns `ModelNotLoaded` if no node serves it — instead
    //     of the previous silent substitution.
    //
    // Mirrors the desktop wiring in
    // `sovereign-desktop/src-tauri/src/state.rs:649` so a request
    // hitting either entrypoint follows the same routing rules.
    // Keep a typed handle to the mesh provider so we can push the
    // slot-alias map into it once `register_local_model_slots` has
    // populated `AppState.slot_aliases`. The trait-object form is
    // what the daemon needs; the typed form is what the alias
    // installer needs.
    // Compose the gossiped-mesh source with any pinned worker pod
    // snapshots persisted on disk. `pod up` writes one snapshot per
    // pod into `~/.svrnmesh/worker-pods/`; this loop loads them at
    // daemon startup and registers each with the inference scheduler
    // so subsequent `chat/completions` calls can route to them.
    // Empty when no pods are configured (the common case) —
    // pinned_source.peer_inference_endpoints() returns an empty Vec
    // and the composite degrades to mesh-only.
    // Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md.
    let pinned_source =
        Arc::new(sovereign_serving_host::pinned_worker_source::PinnedWorkerEndpointSource::new());
    if let Some(dir) = sovereign_serving_host::pinned_pod_snapshot::default_snapshot_dir() {
        let snapshots = sovereign_serving_host::pinned_pod_snapshot::load_all_snapshots(&dir);
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 2026-05-18: silently expired tokens caused a 6h SEP-on-Vast
        // outage. Registering an already-expired snapshot means every
        // routed inference call gets `token expired` from the pod and
        // is retried via mesh fallback — wasteful and confusing. Skip
        // expired snapshots loudly here so the operator sees the
        // problem at daemon start, not after burning a night of GPU.
        const NEAR_EXPIRY_WARN_SECS: u64 = 4 * 3600; // 4h
        for snap in snapshots {
            let expires_unix = snap.bootstrap_blob.expires_unix;
            if expires_unix <= now_unix {
                tracing::error!(
                    vast_id = %snap.vast_id,
                    expires_unix,
                    expired_secs_ago = now_unix.saturating_sub(expires_unix),
                    "daemon_cmd: pinned-pod snapshot token EXPIRED — \
                     skipping (tear down with `svrn mesh pod down {id}` \
                     or relaunch with `--ttl-hours <N>` to refresh)",
                    id = snap.vast_id,
                );
                continue;
            }
            let remaining = expires_unix.saturating_sub(now_unix);
            if remaining < NEAR_EXPIRY_WARN_SECS {
                tracing::warn!(
                    vast_id = %snap.vast_id,
                    expires_unix,
                    remaining_secs = remaining,
                    "daemon_cmd: pinned-pod snapshot token near expiry \
                     (<4h remaining) — plan a fresh `mesh pod up` if \
                     your run will outlast it"
                );
            }
            match snap.to_pinned_pod() {
                Ok(pod) => {
                    tracing::info!(
                        vast_id = %snap.vast_id,
                        host = %snap.host,
                        port = snap.port,
                        node_id = %pod.node_id,
                        expires_in_h = remaining as f64 / 3600.0,
                        "daemon_cmd: registered pinned worker pod with inference scheduler"
                    );
                    pinned_source.register(pod).await;
                }
                Err(e) => {
                    tracing::warn!(
                        vast_id = %snap.vast_id,
                        error = %e,
                        "daemon_cmd: pinned-pod snapshot rejected — skipping"
                    );
                }
            }
        }
    }
    let composite = Arc::new(
        sovereign_serving_host::pinned_worker_source::CompositeVenueSource::new(
            venues,
            Arc::clone(&pinned_source),
        ),
    );
    // The gauge exists BEFORE the provider: the node creates it here, hands
    // the same `Arc` to the router's guards (`.in_flight`) and returns it so
    // `ServingCore` can give it to `AppState`. There is no install afterwards
    // — the counter is never a slot filled once the provider is built
    // (`quality/DAEMON_CORE.md` §4.2 "Where an install slot breaks a cycle").
    let in_flight_gauge = sovereign_contracts::in_flight::LocalInFlightGauge::new();
    tracing::info!(
        "daemon_cmd: minted the in-flight gauge before the router — gossip \
         will advertise this node's actual load from the same counter the \
         provider's guards write"
    );
    let mesh_provider = Arc::new(
        sovereign_serving_host::peer_inference::InferenceRouter::builder(Arc::clone(&provider))
            .candidates(Arc::clone(&composite) as Arc<dyn sovereign_contracts::venue::VenueSource>)
            .host(host)
            .manifest(Arc::new(
                sovereign_serving_host::slot_manifest::CoreSlotManifest,
            ))
            .in_flight(in_flight_gauge.arc())
            .build(),
    );
    // The pinned pods' TLS handles do not travel with the venue (the scheduler
    // may not name `PinnedTransport`); the router resolves them by `node_id`
    // through this source.
    mesh_provider.set_pinned_transports(Arc::clone(&pinned_source)
        as Arc<dyn sovereign_serving_host::venue_host::PinnedTransportResolver>);
    // A guest link this node accepted lets a granted model id resolve to the
    // LENDING node while the turn stays here. Wired at the COLD-START
    // assembly point, which is the whole reason this function exists: the
    // hot-reload factory in `provider.rs` had it and this did not,
    // so a freshly started daemon kept `NoGuestLenders` and the guest route
    // was dead until something happened to trigger a provider reload.
    // Observed live 2026-08-28: zero `guest-lender` lines in a daemon whose
    // `guest.json` was present and valid.
    mesh_provider.set_guest_source(sovereign_serving_host::guest_source::stored_guest_source());
    // Route this node's primary turns into the mesh-hosted shared model, if
    // one is configured (SOVEREIGN_SHARED_MODEL_ID, from [shared_model]
    // model_id). Here, at the one construction: a reload keeps this router,
    // so the first primary turn after cold start already goes there
    // (pb-serve-ranks; it ran only on reload before).
    let shared = sovereign_contracts::launch::SharedModelFleet::from_env();
    tracing::info!(target: "serving_path", shared_model = ?shared.model_id(), "router: the shared model primary turns route to");
    if let Some(id) = shared.model_id() {
        mesh_provider.set_shared_model_id(Some(id.to_string()));
    }
    (mesh_provider, in_flight_gauge)
}
