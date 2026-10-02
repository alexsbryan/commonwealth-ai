// SPDX-License-Identifier: AGPL-3.0-or-later
//! The published candidate record and the port that supplies it.
//!
//! `sovereign/SERVING_BOUNDARY.md` "The five entries" (a): the ranked thing
//! is a `Venue`, and the roster reaches the ranker through ONE port —
//! [`VenueSource::candidates`]. Two facts that used to ride this port do not:
//!
//! - the local node id, which is Fabric's identity READER over
//!   `kernel_types::NodeId` and is handed to the router's construction
//!   (`quality/DAEMON_CORE.md` §4.2 "Identity is a reader" — join adoption
//!   swaps the id inside a running daemon, so a cached value goes stale);
//! - the contribution-ledger emission, which the host mints from
//!   `RoutingOutcome` instead: Serving emits facts, Fabric prices them.
//!
//! A venue also carries only WHETHER it is a pinned worker pod
//! ([`InferenceVenue::pinned_transport`]), never the TLS-pinned transport
//! handle: the scheduler may not name `PinnedTransport`
//! (`quality/ARCH_LAYERS.toml` rule 2, layer `contract` vs host `runtime`) and
//! never reads it — the host resolves the handle by `node_id` through its own
//! resolver.
//!
//! The vocabulary itself — [`InferenceVenue`], [`VenueSource`] — lives in
//! `sovereign_contracts::venue` (fp-1, §12 decision 3) and is re-exported
//! here at its historical path. The ranker's impls stay in this crate.

pub use sovereign_contracts::venue::{InferenceVenue, VenueSource};
