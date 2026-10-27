// SPDX-License-Identifier: AGPL-3.0-or-later
//! A placement measurement record, its local file, and the wire form it
//! travels in.
//!
//! The verbs that took and read these records (`svrn mesh bench`, `svrn mesh
//! plan`) were deleted with the mesh fabric (es-g4-delete-mesh-fabric-verbs).
//! What remains is the record schema, `~/.svrnmesh/mesh-measurements.json`
//! (`SOVEREIGN_MESH_MEASUREMENTS=<path>` relocates it, `=0` disables it), and
//! [`to_wire`]/[`from_wire`], which serve's reconcile
//! (`crate::measurements_rail::reconcile_loop`) uses to keep the file's records
//! on the `mesh-measurements` ring journal.
//!
//! A [`MeasurementKey`] is an identity: model fingerprint, placement digest,
//! host hardware fingerprint, context length, probe version and [`LinkClass`].
//! [`MeasurementRecord`] deliberately carries no model size, so no size ratio
//! can be computed from it, and these records must never reach
//! `NodeCapabilities.benchmark`, the scheduler's `throughput_factor` input
//! (`SCHEDULER_QUALITY.md` §4.5 filed that extrapolation DO-NOT-BUILD).
//! Invalid runs do not travel ([`to_wire`] refuses them), and a reader names a
//! publisher from the rail signature, never from the payload.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub use oicp_types::measurements::MEASUREMENTS_APP_ID;

/// Bumped when the probe protocol changes in any way that makes new numbers
/// incomparable with old ones: the prompt, the token budget, the timing
/// formula, or the set of validity guards. Old records then stop matching
/// rather than being silently compared against numbers taken differently.
pub const PROBE_VERSION: u32 = 1;

/// Bumped when the on-disk layout changes incompatibly. A file at a different
/// version is discarded wholesale.
///
/// v2 (2026-07-29) added [`MeasurementKey::link`]. Discarding rather than
/// migrating is the honest option: a v1 record does not say which link it was
/// taken over, and there is no way to recover that after the fact. Defaulting
/// them to any concrete [`LinkClass`] would assert something nobody measured,
/// and defaulting them to `Unknown` would keep rows that can never match. They
/// are dropped, and the operator re-measures.
const SCHEMA_VERSION: u32 = 2;

/// Per-key run cap. Keeps variance visible without unbounded growth.
pub const MAX_RUNS_PER_KEY: usize = 8;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Proof that a real, present host was identified — the token
/// [`MeasurementKey::for_plan`] requires.
///
/// This makes one rule structural: **a hypothetical mesh can never match a
/// measurement.** The key cannot be *constructed* without this value, and the
/// only way to obtain one is [`HostIdentity::from_live_mesh`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostIdentity(u64);

impl HostIdentity {
    /// Identify the host from a fingerprint read off the live mesh.
    ///
    /// `None` when the host advertises no fingerprint — an older peer, or a
    /// node whose hardware detection came up empty. A caller that gets `None`
    /// must report "not measured" rather than substituting a placeholder:
    /// a shared default would collide every unidentified host into one key.
    pub fn from_live_mesh(hw_fingerprint: Option<u64>) -> Option<Self> {
        hw_fingerprint.map(Self)
    }

    /// The underlying fingerprint, for display and JSON emission.
    pub fn fingerprint(self) -> u64 {
        self.0
    }
}

/// One device's share of a placement, as the digest sees it.
///
/// Serialisable because [`PlacementWitness`] stores these verbatim: a witness
/// that paraphrased the digest's inputs could not be checked against the digest,
/// which is the only thing that makes it worth trusting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementShard {
    /// Stable identity of the machine holding this share.
    ///
    /// Callers pass a mesh member *name* where the RPC endpoint resolves to a
    /// known peer, and an endpoint host with the port dropped otherwise. Ports
    /// must not appear: they churn across restarts and would make every lookup
    /// a miss.
    pub node_key: String,
    /// The fingerprint of the silicon behind `node_key`.
    ///
    /// A name is not hardware. A peer that swaps a GPU for a different one of
    /// the same capacity, or flips its backend between Vulkan and ROCm, keeps
    /// its mesh name and every name-only key it ever filed — so the old number
    /// answers for the new machine. This field is what makes the digest notice.
    /// It is deliberately *beside* `node_key` rather than concatenated into it,
    /// because `node_key` is also the mesh-member name used to look a peer up in
    /// the live mesh and to label it in human output; hashing hardware into that
    /// string would break both.
    ///
    /// `None` means the machine did not advertise one (a peer on an older
    /// daemon). Both callers refuse to build a key in that case rather than
    /// filing under a shard they cannot attribute — see
    /// [`hardware_fingerprint`](kernel_types::hardware_fingerprint). It is still encoded distinctly from any `Some`
    /// so the two can never collide in the digest.
    pub hw: Option<u64>,
    /// Inclusive block range this device holds, or `None` if it holds none.
    pub blocks: Option<(u32, u32)>,
    /// Whether this device carries the output head.
    pub holds_output: bool,
}

fn hex16(digest: &[u8]) -> String {
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Witness — the pre-image of a digest
// ---------------------------------------------------------------------------

/// What one machine in a placement *is*, in terms a reader can act on.
///
/// A [`hardware_fingerprint`](kernel_types::hardware_fingerprint) is deliberately a small opaque hash: it answers
/// "is this the same machine" and is not meant to be read. That is enough on the
/// machine that took the measurement, where the operator already knows what
/// their own hardware is. It is not enough anywhere else — a reader shown
/// `host_hw_fingerprint: 7602642063143971880` learns nothing they can weigh.
///
/// **Descriptive only, and deliberately not a rate.** `vram_gb` is a capacity
/// and `backend` is a label; neither is a throughput figure, so neither can be
/// divided by another machine's to scale a measured number onto it. That
/// restraint is the same one the module docs describe for model size: the banned
/// extrapolation of `SCHEDULER_QUALITY.md` §4.5 needs a rate or a size to divide
/// by, and adding one here would be a reviewable act rather than an accident.
/// Anything added to this struct should pass the same test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineWitness {
    /// The [`PlacementShard::node_key`] this describes.
    pub node_key: String,
    /// Total advertised GPU memory in GB, summed across the machine's cards.
    pub vram_gb: u32,
    /// Advertised GPU backend (`cuda` | `rocm` | `metal` | `vulkan`), when the
    /// machine said. Folded into the fingerprint, so it is also part of why two
    /// otherwise-identical machines key differently.
    pub backend: Option<String>,
}

/// The inputs a placement digest was computed from, kept beside the one-way
/// hash so a record can say what its number was *for*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementWitness {
    /// The `mode` string that was hashed — `"local"` or `"distributed"`.
    pub mode: String,
    /// The `total_blocks` that was hashed.
    pub total_blocks: u32,
    /// Exactly the shards that were hashed. Order is immaterial.
    pub shards: Vec<PlacementShard>,
    /// What each named machine is. Never hashed — a description changing must
    /// not change a configuration's identity, or improving what a peer
    /// advertises would silently orphan every record naming it.
    #[serde(default)]
    pub machines: Vec<MachineWitness>,
}

// ---------------------------------------------------------------------------
// Link
// ---------------------------------------------------------------------------

/// How the host reaches the machines holding the rest of the model.
///
/// The tensor stream is raw TCP to each worker's rpc-server, so this is a
/// property of *the endpoint ggml dials*, not of the peer's identity. The same
/// peer is [`Direct`](LinkClass::Direct) when discovery found a routable
/// address for it and [`Tunnel`](LinkClass::Tunnel) when it fell back to a
/// loopback proxy whose far end is an iroh tunnel. Which of those happens is
/// decided by network conditions on the day, not by configuration — which is
/// exactly why it has to be in the key rather than in a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkClass {
    /// No network hop at all: the whole model is on the host.
    Local,
    /// Raw TCP to a routable address — a LAN, or a WireGuard-style overlay.
    Direct,
    /// Relayed or hole-punched through an iroh loopback proxy.
    Tunnel,
    /// The link could not be determined. Never matches a record.
    Unknown,
}

impl LinkClass {
    /// Stable identifier for JSON and human output.
    pub fn as_str(self) -> &'static str {
        match self {
            LinkClass::Local => "local",
            LinkClass::Direct => "direct",
            LinkClass::Tunnel => "tunnel",
            LinkClass::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

/// The identity of a measurable configuration. Two runs share a key only if
/// they are genuinely the same thing measured twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasurementKey {
    /// See [`PROBE_VERSION`].
    pub probe_version: u32,
    /// `mf1:<16 hex>`, over the GGUF tensor table's names and byte counts.
    pub model_fingerprint: String,
    /// `pd2:<16 hex>`, over the placement's mode, block count and shards.
    pub placement_digest: String,
    /// See [`hardware_fingerprint`](kernel_types::hardware_fingerprint), for the host.
    pub host_hw_fingerprint: u64,
    /// Context length the measurement was taken at. Decode rate is a function
    /// of KV size, so 8k and 128k are not the same measurement.
    pub n_ctx: u32,
    /// See [`LinkClass`]. The path the tensor stream took between the machines
    /// in this placement.
    pub link: LinkClass,
}

impl MeasurementKey {
    /// Build the key for a plan against a real, present mesh.
    ///
    /// Requires a [`HostIdentity`], which is what bars a `--devices`
    /// hypothetical from ever matching a record. Always stamps the current
    /// [`PROBE_VERSION`]: a caller cannot ask for a number taken by a
    /// superseded method.
    pub fn for_plan(
        host: HostIdentity,
        model_fingerprint: String,
        placement_digest: String,
        n_ctx: u32,
        link: LinkClass,
    ) -> Self {
        Self {
            probe_version: PROBE_VERSION,
            model_fingerprint,
            placement_digest,
            host_hw_fingerprint: host.fingerprint(),
            n_ctx,
            link,
        }
    }
}

mod persistence;
mod records;

pub use persistence::*;
pub use records::*;

#[cfg(test)]
mod tests;
