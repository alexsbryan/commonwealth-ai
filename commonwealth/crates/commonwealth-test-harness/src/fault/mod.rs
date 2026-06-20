// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fault-injection primitives for deterministic-simulation (DST) mesh tests.
//!
//! These are transport-layer building blocks with **no** dependency on
//! `sovereign-mesh`: a [`FaultTransport`] (a [`commonwealth_transport::PeerTransport`]
//! impl installed per node), a [`FaultProxy`] (localhost TCP forwarder for
//! wire faults), a shared [`FaultPolicy`] mutated between rounds, and a seeded
//! [`FaultSchedule`]. The gossip-driving `DstMesh` that wires them together
//! lives in `sovereign-mesh` (the only crate that can name `run_one_round`),
//! because the test-harness crate sits *below* `sovereign-mesh` in the
//! dependency graph.
//!
//! Faults split into two mechanisms by cost:
//! - **Endpoint-level** (partition, peer-down): [`FaultTransport`] returns an
//!   empty candidate list — indistinguishable to the caller from "peer has no
//!   usable address", which is exactly the production unreachable-peer path.
//! - **Wire-level** (latency, loss, mid-stream cut, slow-loris): the transport
//!   points the candidate at this observer's [`FaultProxy`] for the target, and
//!   the proxy enforces the [`WireFault`]. Clean edges bypass the proxy.

mod policy;
mod proxy;
mod schedule;
mod transport;

pub use policy::{shared_policy, FaultPolicy, SharedPolicy, WireFault};
pub use proxy::FaultProxy;
pub use schedule::{DetRng, FaultEvent, FaultSchedule};
pub use transport::FaultTransport;
