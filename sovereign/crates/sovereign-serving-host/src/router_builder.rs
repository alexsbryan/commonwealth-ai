// SPDX-License-Identifier: AGPL-3.0-or-later
//! The builder for [`InferenceRouter`].
//!
//! Split out of `peer_inference.rs` so the router's own file stays inside
//! arch-gate's size slack (ARCH §3.1); the builder is a cohesive unit and the
//! router file only needs [`InferenceRouter::builder`] to construct one.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use sovereign_contracts::traits::InferenceProvider;
use sovereign_scheduler::venue::VenueSource;

use crate::peer_inference::InferenceRouter;
use crate::slot_select::SlotManifest;
use crate::venue_host::VenueHost;

/// Builder for [`InferenceRouter`].
///
/// Collapses the two historical constructors — `with_peer_source` (a private
/// in-flight publisher) and `with_peer_source_and_publisher` (an
/// externally-owned one) — into one shape (`sovereign/SERVING_BOUNDARY.md`
/// (e); `quality/DOMAINS.toml` `family = "serving-public-interface"`). The
/// pair existed only because a hot reload must not mint a fresh publisher:
/// live `LocalTotalGuard`s from the old router hold a clone of the shared
/// `Arc<AtomicU32>` and keep decrementing it as their requests drain, so
/// `in_flight` is an explicit, optional builder argument.
pub struct InferenceRouterBuilder {
    local: Arc<dyn InferenceProvider>,
    candidates: Option<Arc<dyn VenueSource>>,
    host: Option<Arc<dyn VenueHost>>,
    manifest: Option<Arc<dyn SlotManifest>>,
    in_flight: Option<Arc<AtomicU32>>,
}

impl InferenceRouterBuilder {
    /// A builder over `local` with no ports set yet.
    pub(crate) fn new(local: Arc<dyn InferenceProvider>) -> Self {
        Self {
            local,
            candidates: None,
            host: None,
            manifest: None,
            in_flight: None,
        }
    }

    /// The routable venues. Required.
    pub fn candidates(mut self, candidates: Arc<dyn VenueSource>) -> Self {
        self.candidates = Some(candidates);
        self
    }

    /// Fabric's identity reader and the contribution-ledger port. Required.
    pub fn host(mut self, host: Arc<dyn VenueHost>) -> Self {
        self.host = Some(host);
        self
    }

    /// The declared slot facts the self-manifest advertises. Required.
    pub fn manifest(mut self, manifest: Arc<dyn SlotManifest>) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// The shared in-flight publisher. Optional: absent, the router mints a
    /// private one — fine for tests and for any caller that does not share
    /// counter state with the gossip emitter.
    pub fn in_flight(mut self, publisher: Arc<AtomicU32>) -> Self {
        self.in_flight = Some(publisher);
        self
    }

    pub fn build(self) -> InferenceRouter {
        InferenceRouter::assemble(
            self.local,
            self.candidates
                .expect("InferenceRouter::builder: .candidates(..) is required"),
            self.host
                .expect("InferenceRouter::builder: .host(..) is required"),
            self.manifest
                .expect("InferenceRouter::builder: .manifest(..) is required"),
            self.in_flight
                .unwrap_or_else(|| Arc::new(AtomicU32::new(0))),
        )
    }
}
