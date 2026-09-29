// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measured throughput for one model on one mesh split — recorded, never
//! estimated.
//!
//! `svrn mesh plan` answers "will this model fit on my machines" from a GGUF
//! header parse alone: offline, no GPU, instant even on a 400 GB split. The
//! question it could not answer is "what will it feel like." This module is the
//! memory that lets it answer — and, just as importantly, the structure that
//! keeps it from *making the answer up*.
//!
//! ## The design rule: measure, don't predict
//!
//! Nothing here estimates. A record is written only after a real run against a
//! real placement, and it is served back only for the *exact* configuration it
//! was taken on. Where there is no record, the honest output is "not measured"
//! plus the command that would measure it — never an interpolation between
//! records that do exist.
//!
//! That rule is not caution for its own sake. `SCHEDULER_QUALITY.md` §4.5
//! priced the alternative against the simulator: a throughput number
//! extrapolated from a baseline probe onto a differently-sized model reads
//! −56%, doubles declined upgrades, and is filed **DO-NOT-BUILD**. The live
//! extrapolator is `throughput_factor` (`oicp-types/src/scoring.rs:384`), and it
//! is already wired end to end — the only thing keeping it dark is that nothing
//! populates `NodeCapabilities.benchmark`. See the guard test
//! `gossip_never_advertises_a_benchmark` in `sovereign-mesh`.
//!
//! **These records must never reach that field.** They are a different thing
//! for a different consumer: §4.5 measured a number's worth to the scheduler's
//! automatic ranked dispatch, through a clamp that only carries information
//! about nodes slower than the reference rate. These records are read by a
//! *person* deciding whether to add a machine, move the host role, or buy
//! hardware. That person has no clamp, and a number worth 0% to a ranker can be
//! decisive for them.
//!
//! Note what [`MeasurementRecord`] deliberately does **not** carry: any model
//! size. There is nothing here from which a size ratio could be computed, so
//! re-deriving the banned extrapolation would require first *adding a field* —
//! a reviewable act, rather than a one-line temptation.
//!
//! ## Why the key has exactly these five parts
//!
//! A cache key that is too coarse fabricates; one that is too fine never hits.
//! Each field of [`MeasurementKey`] is here because dropping it would serve a
//! real number for a configuration it was not measured on:
//!
//! | Field | Drop it and… |
//! |---|---|
//! | `model_fingerprint` | a Q4 number is shown for a Q8 plan |
//! | `placement_digest` | a 36/12 split's number is shown for a 24/24 plan |
//! | `host_hw_fingerprint` | one machine's number is shown on another's |
//! | `n_ctx` | an 8k number is shown for a 128k plan (decode rate tracks KV size) |
//! | `probe_version` | numbers taken by different methods are compared as equals |
//! | `link` | a tunnelled number is shown for a direct-IP plan (a 2.3× error) |
//!
//! And what is deliberately *excluded*: RPC endpoint ports (DHCP churn would
//! make every lookup a miss), and the probe's prompt text and token counts
//! (they are protocol constants folded into `probe_version`, not key fields —
//! keying on them would drive the hit rate to zero).
//!
//! `link` is the newest of the six and was added after the other five were
//! already in service, so it is worth saying why it earns its place. It is the
//! only key field that can change *without anything on either machine
//! changing*: the same model, the same split, the same silicon, reached over a
//! different path. Measured on this fleet, the same 4B distributed decode read
//! 17.35 tok/s over a forced iroh tunnel and ~40 tok/s over direct IP — a 2.3×
//! spread from link choice alone, larger than most of what the other five
//! fields guard against. Before it existed those two runs shared a key and the
//! later one silently answered for both. See [`LinkClass`].
//!
//! The GPU backend is folded *inside* `host_hw_fingerprint` rather than sitting
//! beside it, because a ROCm↔Vulkan swap shifts throughput materially on
//! identical silicon without changing the GPU's name — so it has to break the
//! key, not merely annotate it.
//!
//! Peer hardware is covered the same way, one level down. `host_hw_fingerprint`
//! pins only the machine that *ran* the probe; the machines that held the rest
//! of the model are described by `placement_digest`, so each
//! [`PlacementShard`] carries its own [`hw`](PlacementShard::hw) fingerprint
//! and the digest changes when a peer's silicon does. Until 2026-07-29 a shard
//! was identified by mesh *name* alone, so a peer that swapped a GPU for a
//! different one of the same capacity — or merely flipped Vulkan↔ROCm — kept
//! every key it had ever filed, and the old number answered for the new
//! machine. A name is not hardware.
//!
//! Where a machine advertises no fingerprint (a peer on an older daemon), the
//! callers do not fall back to a name-only key: `mesh plan` reports "not
//! measured" and `mesh bench` refuses to file. An unattributable record is
//! worse than a missing one, because only the missing one admits what it does
//! not know.
//!
//! ## The key is an identity, not a description
//!
//! Both digests in the key are one-way. That is the right shape for [`lookup`],
//! which asks only "is this the same configuration" — but it means a record can
//! state a number without being able to say what the number was *for*. On
//! 2026-07-30 two runs of this fleet, four hours apart, filed under different
//! placement digests with identical human labels, and an exhaustive search over
//! every integer split of the model across both machines — both range orders,
//! either machine holding the output head, every known peer fingerprint — could
//! not reconstruct what the earlier one had described. Nothing was corrupt. The
//! pre-image had simply never been kept.
//!
//! So every hashed component of the key has a witness beside it:
//! [`PlacementWitness`] holds the exact inputs the digest was computed from, and
//! [`MachineWitness`] says what each named machine is in terms a person can
//! weigh. The witness is checkable against the hash it explains
//! ([`PlacementWitness::explains`]) and is ignored where it does not match, so
//! it can be trusted without being believed.
//!
//! This matters most for the case the key is worst at. A key pins the exact
//! split *and* the exact silicon, so two operators with genuinely comparable
//! machines will essentially never share one. Exactness is right — it is what
//! stops a number being served for a configuration nobody ran — but it makes
//! [`near_misses`] the surface that actually answers the question, and a near
//! miss is only worth reading if it can say *how* the other configuration
//! differed. [`Difference`] is that answer.
//!
//! ## Storage
//!
//! `~/.svrnmesh/mesh-measurements.json`. `SOVEREIGN_MESH_MEASUREMENTS=<path>`
//! relocates it; `SOVEREIGN_MESH_MEASUREMENTS=0` disables reads and writes
//! entirely. Records are append-only per key, capped at [`MAX_RUNS_PER_KEY`]
//! (FIFO) so repeated runs make variance *visible* rather than averaging it
//! away — a thermally-throttled or link-jittery machine should look unstable,
//! not merely slow.
//!
//! There is no time-based expiry. A measurement does not rot: the hardware that
//! produced it is pinned in the key. What can change underneath it is the
//! inference engine, so every record stamps the build that took it and
//! [`lookup`] flags a mismatch as stale rather than discarding it. Silently
//! dropping a record would cost the operator a re-measurement for nothing;
//! silently refreshing it would spend twenty minutes they did not ask for.
//! Showing the age and letting them judge is the whole premise of the tool.
//!
//! ## Travel
//!
//! A measurement is worth most to the machine that did not take it: locally it
//! recalls what a run felt like, on a peer it answers what a configuration
//! *would* feel like on hardware the reader cannot try. Records therefore
//! gossip, under [`MEASUREMENTS_APP_ID`], as versioned [`to_wire`] envelopes.
//!
//! Two rules keep that from undoing everything above. Peer records never enter
//! [`MeasurementFile`], so [`lookup`] still answers only "what did *this*
//! machine measure" and no peer's number can be served as the reader's own; and
//! every peer record reaches the operator through [`near_misses`] carrying
//! [`NearMiss::taken_by`], so it is named as someone else's. Invalid runs do not
//! travel at all ([`to_wire`] refuses them) — a failure is glassbox material for
//! the operator who caused it and noise to everyone else.
//!
//! **The transport is the ring rail, not the gossip KV store** (cw-lift 2d).
//! That store is `in_memory()` in the daemon — a wire buffer, not storage — so
//! everything a node had published evaporated from the mesh on every restart,
//! and a boot step re-uploaded the whole file to compensate. The rail is an
//! append-only journal on disk under `rings/mesh-measurements/`, replicated by
//! anti-entropy, so a record is written once and stays written; the boot step
//! is now a one-shot reconcile rather than a standing upload.
//!
//! Three consequences worth knowing here, all of them in
//! `sovereign_mesh::measurements_rail`:
//!
//! - A record rides as [`to_wire`]'s exact bytes inside a rail payload, as a
//!   JSON **string**. A rail payload may not contain a fractional number — two
//!   nodes must derive identical bytes from it and JSON does not promise that
//!   for fractions — and a [`MeasurementRecord`] is nine `f64`s. A string has
//!   one spelling and the bytes inside it are never re-serialized, so the
//!   hazard the rule guards against is absent rather than merely checked.
//! - Every line is signed by the publisher's node key, and a reader admits it
//!   only if the mesh's own membership claims that key. The publisher is named
//!   from the signature, never from anything inside the payload.
//! - A node that is not in a mesh is REFUSED at publish, because no roster
//!   could claim its signer. The file still holds the run and the reconcile
//!   carries it once membership exists — the refusal is not a loss.
//!
//! The durable file stays authoritative for [`lookup`] either way.

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
/// This exists to make one rule structural instead of conventional: **a
/// hypothetical mesh can never match a measurement.** `svrn mesh plan
/// --devices 64,32,32` describes hardware that is not here, so there is no host
/// to fingerprint and no measurement that could honestly apply to it. Rather
/// than rely on a runtime `if` that a later refactor could drop, the key simply
/// cannot be *constructed* without this value, and the only way to obtain one
/// is [`HostIdentity::from_live_mesh`].
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

/// Fingerprint a model from its GGUF tensor table — `"mf1:<16 hex>"`.
///
/// `sizes` is the `(tensor_name, layer, nbytes)` table that `mesh plan` already
/// parses; only name and byte count participate. Properties that matter:
///
/// - **Order-independent.** The table is sorted before hashing, so two reads of
///   the same file agree regardless of enumeration order.
/// - **Quantisation-sensitive.** Byte counts are hashed, so Q4 and Q8 of the
///   same model are different models here — which is the point.
/// - **Rename-proof.** Nothing about the file's path or name is included.
/// - **Free.** This is a header parse the caller has already done.
pub fn model_fingerprint(sizes: &[(String, Option<u32>, u64)], block_count: u32) -> String {
    let mut rows: Vec<(&str, u64)> = sizes.iter().map(|(n, _, b)| (n.as_str(), *b)).collect();
    rows.sort_unstable();

    let mut h = Sha256::new();
    h.update(b"mf1");
    h.update(block_count.to_le_bytes());
    h.update((rows.len() as u64).to_le_bytes());
    for (name, nbytes) in &rows {
        h.update(name.as_bytes());
        h.update([0u8]); // delimiter: "ab"+"c" must not collide with "a"+"bc"
        h.update(nbytes.to_le_bytes());
    }
    format!("mf1:{}", hex16(&h.finalize()))
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

/// Fingerprint a placement — `"pd2:<16 hex>"`.
///
/// `mode` distinguishes a single-machine load from a split one, so the same
/// model measured solo and measured distributed are never confused. Shards are
/// sorted by `node_key`, making the digest independent of the order the mesh
/// happened to enumerate its members.
///
/// The prefix is a *generation*, not decoration. `pd1` hashed a shard as
/// name + blocks + output-head; `pd2` also hashes [`PlacementShard::hw`], so
/// the same inputs produce a different digest under the two schemes. Bumping it
/// means a stored `pd1:` digest is visibly from the older construction instead
/// of being silently un-matchable bytes wearing the same label.
pub fn placement_digest(mode: &str, total_blocks: u32, shards: &[PlacementShard]) -> String {
    let mut sorted: Vec<&PlacementShard> = shards.iter().collect();
    sorted.sort_unstable_by(|a, b| a.node_key.cmp(&b.node_key));

    let mut h = Sha256::new();
    h.update(b"pd2");
    h.update(mode.as_bytes());
    h.update([0u8]);
    h.update(total_blocks.to_le_bytes());
    h.update((sorted.len() as u64).to_le_bytes());
    for s in sorted {
        h.update(s.node_key.as_bytes());
        h.update([0u8]);
        // Tagged, so "no fingerprint" cannot hash the same as any real one.
        match s.hw {
            Some(fp) => {
                h.update([1u8]);
                h.update(fp.to_le_bytes());
            }
            None => h.update([0u8]),
        }
        match s.blocks {
            Some((lo, hi)) => {
                h.update([1u8]);
                h.update(lo.to_le_bytes());
                h.update(hi.to_le_bytes());
            }
            None => h.update([0u8]),
        }
        h.update([u8::from(s.holds_output)]);
    }
    format!("pd2:{}", hex16(&h.finalize()))
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

impl MachineWitness {
    /// One-line description, e.g. `"51 GB vulkan"`.
    pub fn describe(&self) -> String {
        match &self.backend {
            Some(b) => format!("{} GB {b}", self.vram_gb),
            None => format!("{} GB", self.vram_gb),
        }
    }
}

/// The inputs a [`placement_digest`] was computed from.
///
/// A digest is a lossy projection. It answers "is this the same configuration"
/// and nothing else, which is exactly what [`lookup`] needs — an equality test
/// between keys written and read on the same machine. It is not enough for
/// anything that has to *explain* a configuration to a reader who did not run
/// it, and that is every other use these records have:
///
/// - Two of this machine's own records land under different keys, and the
///   operator asks which one describes what they are running now. Without the
///   pre-image this is unanswerable: on 2026-07-30 an exhaustive search over
///   every integer split of a 48-block model across both machines of this fleet,
///   in both range orders, with either machine holding the output head, and
///   every known peer fingerprint substituted, failed to reconstruct what a
///   digest recorded four hours earlier had described. The number was still
///   there; what it was a number *for* was gone.
/// - A [`NearMiss`] has to say how the measured configuration differs from the
///   one being planned, concretely enough for the reader to judge relevance.
///   `differs_by: ["split"]` does not clear that bar.
/// - A record that travelled from another machine, where an exact key hit is
///   vanishingly unlikely — the key pins the exact split *and* the exact
///   silicon — so the near miss is not a courtesy, it is the entire value.
///
/// The witness is therefore kept beside the hash rather than derived from it.
/// What makes it trustworthy is that it is *checkable*: [`Self::explains`]
/// re-runs [`placement_digest`] over these exact fields, so a witness that does
/// not account for the digest it sits next to can be detected rather than
/// believed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementWitness {
    /// The `mode` string that was hashed — `"local"` or `"distributed"`.
    pub mode: String,
    /// The `total_blocks` that was hashed.
    pub total_blocks: u32,
    /// Exactly the shards that were hashed. Order is immaterial:
    /// [`placement_digest`] sorts by `node_key`.
    pub shards: Vec<PlacementShard>,
    /// What each named machine is. Never hashed — a description changing must
    /// not change a configuration's identity, or improving what a peer
    /// advertises would silently orphan every record naming it.
    #[serde(default)]
    pub machines: Vec<MachineWitness>,
}

impl PlacementWitness {
    /// The digest these inputs produce.
    pub fn digest(&self) -> String {
        placement_digest(&self.mode, self.total_blocks, &self.shards)
    }

    /// Whether this witness accounts for `digest`.
    ///
    /// A `false` here means the producer built the witness and the key from
    /// different inputs — a bug in the writer, not in the reader, and one worth
    /// surfacing rather than papering over: a witness that explains the wrong
    /// configuration is more misleading than no witness at all.
    pub fn explains(&self, digest: &str) -> bool {
        self.digest() == digest
    }

    /// The description of one named machine, when it was recorded.
    pub fn machine(&self, node_key: &str) -> Option<&MachineWitness> {
        self.machines.iter().find(|m| m.node_key == node_key)
    }

    /// The machine carrying `hw`, by fingerprint rather than by name — how the
    /// host is located, since [`MeasurementKey::host_hw_fingerprint`] names
    /// silicon and not a mesh member.
    pub fn machine_with_hw(&self, hw: u64) -> Option<&MachineWitness> {
        let shard = self.shards.iter().find(|s| s.hw == Some(hw))?;
        self.machine(&shard.node_key)
    }

    /// The split, as a line a reader can compare against another —
    /// e.g. `"BeefyMac 12 · RuggedFox 36 +head"`.
    ///
    /// Block *counts*, not ranges: which end of the model a machine holds is
    /// part of the identity (and so part of the digest), but a reader deciding
    /// where to put weight is asking how much each machine carries.
    pub fn describe_split(&self) -> String {
        let mut sorted: Vec<&PlacementShard> = self.shards.iter().collect();
        sorted.sort_unstable_by(|a, b| a.node_key.cmp(&b.node_key));
        sorted
            .iter()
            .map(|s| {
                let held = match s.blocks {
                    Some((lo, hi)) => format!("{}", hi.saturating_sub(lo) + 1),
                    None => "idle".to_string(),
                };
                let head = if s.holds_output { " +head" } else { "" };
                format!("{} {held}{head}", s.node_key)
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
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
    /// The link could not be determined. Never matches a record: see [`lookup`].
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

    /// The link class of a whole placement, from its workers' individual links.
    ///
    /// Three rules, each chosen for a reason:
    ///
    /// - **No workers ⇒ [`Local`](LinkClass::Local).** There is no link to
    ///   classify, and a single-node run must not be keyed as though there
    ///   were.
    /// - **Any `Unknown` ⇒ `Unknown`.** One unclassifiable worker makes the
    ///   whole placement unattributable. Guessing the rest would be answering a
    ///   question we cannot see the answer to.
    /// - **Any `Tunnel` ⇒ `Tunnel`,** rather than a majority or an average. A
    ///   single-stream pipeline runs at the speed of its slowest hop, so one
    ///   tunnelled worker characterises the whole run even when every other
    ///   worker is direct.
    pub fn summarize(workers: &[LinkClass]) -> LinkClass {
        if workers.is_empty() {
            return LinkClass::Local;
        }
        if workers.contains(&LinkClass::Unknown) {
            return LinkClass::Unknown;
        }
        if workers.contains(&LinkClass::Tunnel) {
            return LinkClass::Tunnel;
        }
        LinkClass::Direct
    }
}

/// Classify the endpoint ggml dials for one worker.
///
/// A loopback authority is the tell. Worker discovery hands ggml either a
/// routable `host:port` it probed successfully, or `127.0.0.1:<port>` — a local
/// proxy socket whose far end is an iroh tunnel to the peer. Nothing else can
/// legitimately present as loopback: a worker genuinely on this machine is not
/// a worker, it is the host.
///
/// **This is the single decider, and it is shared deliberately.** `svrn mesh
/// bench` calls it on the endpoints in the daemon's live placement; `svrn mesh
/// plan` calls it on the endpoints in the mesh status' discovered-worker list.
/// Two implementations that disagreed by one edge case would file every record
/// under a key the reader can never reproduce — the store would grow forever
/// while the plan reported "not measured" for the configuration it just
/// measured. One function, two callers, is what makes that failure impossible
/// rather than merely unlikely.
pub fn link_class_of_endpoint(endpoint: &str) -> LinkClass {
    let e = endpoint.trim();
    // Only two forms carry a port unambiguously: `[<ipv6>]:port` and a single-
    // colon `<host>:port`. A bare IPv6 literal has no port (that is what the
    // brackets are for), so it is taken whole rather than truncated at its
    // first colon — `::1` must not parse as an empty host.
    let host = if let Some(rest) = e.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else if e.matches(':').count() > 1 {
        e
    } else {
        e.split(':').next().unwrap_or(e)
    }
    .trim();

    if host.is_empty() {
        return LinkClass::Unknown;
    }
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host == "0:0:0:0:0:0:0:1"
        // The whole 127.0.0.0/8 block, not just 127.0.0.1.
        || host
            .strip_prefix("127.")
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    if loopback {
        LinkClass::Tunnel
    } else {
        LinkClass::Direct
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
    /// See [`model_fingerprint`].
    pub model_fingerprint: String,
    /// See [`placement_digest`].
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
#[cfg(test)]
use records::human_duration;
pub use records::*;

#[cfg(test)]
mod tests;
