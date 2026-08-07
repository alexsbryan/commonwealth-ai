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
//! `~/.sovereign/mesh-measurements.json`. `SOVEREIGN_MESH_MEASUREMENTS=<path>`
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
//! The mesh KV store is a wire buffer, not storage: it is in-memory in the
//! daemon and empties on restart. The durable file stays authoritative and the
//! daemon republishes it at boot. See the `Travel` section below.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

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

/// Stable hash of one machine's hardware.
///
/// Small on purpose — this needs equality against a previously recorded value,
/// not cryptographic uniqueness. `gpus` is `(name, vram_gb, backend)`, and the
/// backend string is part of the hash because the same silicon driven through
/// different backends is, for throughput purposes, different hardware.
///
/// Order-independent across GPUs: the same two cards enumerated in either order
/// hash identically, so a driver-order change does not invalidate a record.
pub fn hardware_fingerprint(
    cpu_cores: u32,
    system_ram_gb: u32,
    gpus: &[(String, u32, String)],
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Hash each GPU independently, then combine with a commutative fold so
    // enumeration order cannot change the result.
    let mut gpu_mix: u64 = 0;
    for (name, vram_gb, backend) in gpus {
        let mut g = DefaultHasher::new();
        name.hash(&mut g);
        vram_gb.hash(&mut g);
        backend.hash(&mut g);
        gpu_mix ^= g.finish();
    }

    let mut h = DefaultHasher::new();
    cpu_cores.hash(&mut h);
    system_ram_gb.hash(&mut h);
    gpus.len().hash(&mut h);
    gpu_mix.hash(&mut h);
    h.finish()
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
    /// [`hardware_fingerprint`]. It is still encoded distinctly from any `Some`
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
/// A [`hardware_fingerprint`] is deliberately a small opaque hash: it answers
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
    /// See [`hardware_fingerprint`], for the host.
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

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Whether a run is fit to be served back.
///
/// A run that tripped a validity guard is still *written* — a discarded failure
/// teaches nobody anything, and silently dropping them would turn the tool into
/// retry-until-lucky. It is simply never returned by [`lookup`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// Every guard passed; this number may be shown.
    Valid,
    /// At least one guard tripped. `problems` is operator-facing prose.
    Invalid {
        /// What went wrong, one entry per tripped guard.
        problems: Vec<String>,
    },
}

impl Verdict {
    /// Whether this run may be served back to a reader.
    pub fn is_valid(&self) -> bool {
        matches!(self, Verdict::Valid)
    }
}

/// What else was true of this machine while the run was taken.
///
/// [`PlacementWitness`] explains *what* a run measured. This explains the
/// *conditions it measured under* — the half that was missing when two runs
/// under one key came back 43% apart and nothing recorded could say why. Every
/// field here is something that can differ between two runs of an identical
/// configuration, which is exactly the class of thing the key cannot hold.
///
/// **Never hashed, never part of [`MeasurementKey`].** Conditions are not
/// identity. Keying on them would give every run a unique unmatched key and
/// destroy the ability to compare runs of the same configuration at all — which
/// is the entire point of the store. So a busy run and a quiet run land under
/// one key, both are kept, and the reader is told which was which.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunConditions {
    /// Slot roles reported resident alongside the measured primary, sorted.
    ///
    /// The primary itself is excluded — it is what was measured, not something
    /// competing with it. So `["embed", "fast"]` means two other models held
    /// memory and could take GPU time during the run.
    ///
    /// Recorded because slot co-residency was the leading suspect for the 43%
    /// spread and could not be checked: it had to be *recalled* rather than
    /// read, and recall could not distinguish "resident in both runs" (a
    /// constant, which cannot explain a difference) from "resident in one".
    pub co_resident_roles: Vec<String>,

    /// Daemon resident-set size in MB at the start of the run, when `/status`
    /// reported it.
    pub host_rss_mb_before: Option<u64>,
    /// The same at the end. A large climb across a short run means something
    /// else on this box was growing while the number was being taken.
    pub host_rss_mb_after: Option<u64>,

    /// Daemon uptime in seconds when the run started.
    ///
    /// Distinguishes a measurement taken on a long-settled daemon from one
    /// taken minutes after a restart, when caches are cold and the supervisor
    /// may still be reconciling.
    pub host_uptime_s: Option<u64>,

    /// Wall-clock seconds spanned by the whole run, first trial to last.
    ///
    /// Not a performance figure — a cross-check. Two runs of the same trial
    /// count whose spans differ sharply were not taken under the same load.
    pub run_span_s: Option<f64>,

    /// The `host:port` addresses ggml actually dialled for each remote worker
    /// carrying weight, in placement order.
    ///
    /// [`LinkClass`] answers only "was the authority loopback", so a `direct`
    /// record says nothing about *which* route the tensors took. A peer
    /// routinely advertises several — a LAN address and an overlay address, say
    /// — and those have different latency floors and degrade differently under
    /// load. Two runs of one configuration can therefore differ by route while
    /// keying identically, which is exactly the unexplainable-spread failure the
    /// rest of this struct exists to close.
    ///
    /// Empty both for a local load and for records written before this was
    /// captured. **Absence is not loopback**: a reader must not infer a route
    /// from an empty list, only that none was recorded.
    #[serde(default)]
    pub rpc_endpoints: Vec<String>,
}

impl RunConditions {
    /// Whether anything shared the GPU with the measured primary.
    pub fn had_co_residents(&self) -> bool {
        !self.co_resident_roles.is_empty()
    }

    /// Growth in daemon RSS across the run, when both ends were reported.
    ///
    /// Signed on purpose: a *drop* is as interesting as a climb, because it
    /// means a slot was evicted mid-run.
    pub fn rss_delta_mb(&self) -> Option<i64> {
        match (self.host_rss_mb_before, self.host_rss_mb_after) {
            (Some(a), Some(b)) => Some(b as i64 - a as i64),
            _ => None,
        }
    }

    /// One line an operator can read, or `None` when nothing was captured.
    ///
    /// Deliberately says "nothing else resident" rather than staying silent
    /// when the slot list is empty: absence of co-residents is a *finding*
    /// about the run, not absence of information about it.
    pub fn describe(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.co_resident_roles.is_empty() {
            parts.push("nothing else resident".to_string());
        } else {
            parts.push(format!(
                "also resident: {}",
                self.co_resident_roles.join(", ")
            ));
        }
        if let Some(rss) = self.host_rss_mb_before {
            match self.rss_delta_mb() {
                Some(d) if d != 0 => {
                    parts.push(format!("daemon rss {rss} MB ({d:+} MB over the run)"))
                }
                _ => parts.push(format!("daemon rss {rss} MB")),
            }
        }
        if let Some(up) = self.host_uptime_s {
            parts.push(format!("daemon up {}", human_duration(up)));
        }
        // Named, not counted: "2 workers" would not distinguish the LAN route
        // from the overlay route, which is the whole reason this is recorded.
        if !self.rpc_endpoints.is_empty() {
            parts.push(format!("rpc via {}", self.rpc_endpoints.join(", ")));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
}

/// Compact duration for operator-facing condition lines.
fn human_duration(secs: u64) -> String {
    match secs {
        s if s < 90 => format!("{s}s"),
        s if s < 5400 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// One completed measurement run.
///
/// Deliberately carries **no model size**. See the module docs: without a size
/// there is no ratio to extrapolate by, so the banned size-law estimate cannot
/// be reconstructed from this data without someone first adding a field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementRecord {
    /// The configuration this run measured.
    pub key: MeasurementKey,

    /// Median steady-state decode rate across the run's trials.
    pub decode_tok_s: f64,
    /// Slowest trial in the run.
    pub decode_tok_s_min: f64,
    /// Fastest trial in the run.
    pub decode_tok_s_max: f64,
    /// Median time to first content token.
    pub ttft_ms: f64,
    /// Median inter-token latency.
    pub itl_p50_ms: f64,
    /// 95th-percentile inter-token latency — where link jitter shows up.
    pub itl_p95_ms: f64,
    /// Prefill rate, present only when the server reported real prompt-token
    /// counts. `None` renders as "n/a", never as an estimate from string
    /// length.
    pub prefill_tok_s: Option<f64>,
    /// Seconds spent loading the model, when this run paid for a cold load.
    pub cold_load_s: Option<f64>,
    /// Timed trials contributing to this run (warm-up excluded).
    pub trials: u32,
    /// Content frames observed, summed across trials.
    pub content_frames: u32,

    /// Human-facing model name. Provenance only — never keyed on.
    pub model_name: String,
    /// Human-facing placement, e.g. `"36 local + 12 @beefymac"`.
    pub placement_human: String,
    /// Machines holding blocks.
    pub nodes: u32,
    /// Network hops per token (`nodes - 1` for a single-stream pipeline).
    pub hops: u32,
    /// Unix seconds at which the run completed.
    pub measured_at: u64,
    /// Build that took the measurement. A mismatch marks a lookup stale.
    pub build: String,
    /// Engine-reported backend, for display and for spotting a payload/key
    /// disagreement.
    pub backend: Option<String>,
    /// Measured round-trip time to the furthest worker, when distributed.
    pub link_rtt_ms: Option<f64>,

    /// Whether this run may be served back.
    pub verdict: Verdict,

    /// The inputs behind [`MeasurementKey::placement_digest`], so this record can
    /// explain itself to a reader who did not run it. See [`PlacementWitness`].
    ///
    /// `None` for a record written before 2026-07-30, when only the hash was
    /// kept. Those records still serve exact hits perfectly well — the witness is
    /// explanatory, not part of the identity — so unlike the v1→v2 change
    /// (see [`SCHEMA_VERSION`], where the missing field was a *key* field and
    /// keeping the rows would have kept rows that could never match) they are
    /// preserved rather than discarded. The cost of keeping them is that they
    /// cannot say what they measured beyond `placement_human`, and the surfaces
    /// that read a witness say so instead of guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<PlacementWitness>,

    /// What else was true of this machine while the run was taken. See
    /// [`RunConditions`].
    ///
    /// `None` for a record written before 2026-07-30. Kept rather than
    /// discarded for the same reason as a witness-less record: conditions are
    /// explanatory, not part of the identity, so an old row still serves an
    /// exact hit perfectly well. The cost of keeping it is that it cannot say
    /// what else was running — and the surfaces that read conditions say
    /// exactly that instead of implying the box was quiet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<RunConditions>,
}

/// The on-disk file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementFile {
    schema_version: u32,
    records: Vec<MeasurementRecord>,
}

impl Default for MeasurementFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

impl MeasurementFile {
    /// An empty file at the current schema.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every record, newest last. Includes invalid runs — they are glassbox
    /// material, and `svrn mesh bench --history` shows them.
    pub fn records(&self) -> &[MeasurementRecord] {
        &self.records
    }
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// What [`lookup`] serves back: the MEDIAN valid run for a key (by decode
/// rate), with the observed spread of run medians across every valid run
/// under it.
///
/// The headline policy is deliberate and was an explicit operator call
/// (2026-07-29, THE_NEXT_MONTH Item Three): "latest" let whichever run
/// happened most recently set the number a stranger is told, and a mean
/// would synthesise a run nobody ran. The median run is a run that actually
/// happened, and one outlier cannot set it — the case that forced the call
/// was a 122B split whose four runs sat at 7.75/8.38/8.53/11.08, where
/// "latest" plus a trial-extreme range quoted the band as 7.5–11.5.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementSummary {
    /// Headline decode rate — the median valid run's rate. Every other
    /// single-run field below comes from that same run, so the summary
    /// always describes one real run, never a composite.
    pub decode_tok_s: f64,
    /// Slowest valid run's rate under this key. A run's rate is already its
    /// median trial, so this is the observed floor across runs — NOT the
    /// slowest single trial, which would widen the band with within-run
    /// jitter the medians absorb.
    pub decode_tok_s_min: f64,
    /// Fastest valid run's rate under this key (observed ceiling, per above).
    pub decode_tok_s_max: f64,
    /// Median run's median time to first token.
    pub ttft_ms: f64,
    /// Median run's median inter-token latency.
    pub itl_p50_ms: f64,
    /// Median run's 95th-percentile inter-token latency.
    pub itl_p95_ms: f64,
    /// Median run's prefill rate, when the server reported one.
    pub prefill_tok_s: Option<f64>,
    /// Context length these numbers were taken at.
    pub n_ctx: u32,
    /// Backend these numbers were taken on.
    pub backend: Option<String>,
    /// Valid runs under this key.
    pub runs: u32,
    /// When the median run completed.
    pub measured_at: u64,
    /// Build that took the median run.
    pub measured_build: String,
    /// Whether that build differs from the one asking. Not a reason to hide the
    /// number — a reason to show it with a warning.
    pub stale: bool,
    /// Human-facing placement of the median run.
    pub placement_human: String,
    /// Human-facing model name of the median run.
    pub model_name: String,
}

/// The measured numbers for exactly this configuration, or `None`.
///
/// Exact match on the whole key, and invalid runs are skipped. There is no
/// nearest-neighbour fallback and no interpolation: a caller that gets `None`
/// must say "not measured", because any number it could synthesise here would
/// describe a configuration nobody ran.
///
/// `current_build` is passed in rather than read from the environment so this
/// stays a pure function — the whole module is testable without a filesystem,
/// a daemon, or a GPU.
///
/// A key whose [`link`](MeasurementKey::link) is [`LinkClass::Unknown`] never
/// matches, *including against a stored `Unknown`*. Two runs we could not
/// classify are not thereby the same run — that would be inferring an identity
/// from a shared absence of evidence, which is precisely the fabrication this
/// module exists to prevent. The caller reports "not measured" and
/// [`near_misses`] still names what *was* measured, so the operator sees the
/// number that exists and why it does not apply.
pub fn lookup(
    file: &MeasurementFile,
    key: &MeasurementKey,
    current_build: &str,
) -> Option<MeasurementSummary> {
    if key.link == LinkClass::Unknown {
        return None;
    }
    let mut valid: Vec<&MeasurementRecord> = file
        .records
        .iter()
        .filter(|r| &r.key == key && r.verdict.is_valid())
        .collect();
    if valid.is_empty() {
        return None;
    }

    // The median run by decode rate; ties break on recency so the pick is
    // deterministic. For an even count the LOWER middle is taken — the
    // conservative side, and still a run that actually happened (averaging
    // the two middles would quote a rate nobody measured). See the policy
    // note on [`MeasurementSummary`].
    valid.sort_by(|a, b| {
        a.decode_tok_s
            .partial_cmp(&b.decode_tok_s)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.measured_at.cmp(&b.measured_at))
    });
    let median = valid[(valid.len() - 1) / 2];

    Some(MeasurementSummary {
        decode_tok_s: median.decode_tok_s,
        decode_tok_s_min: valid.first().expect("non-empty").decode_tok_s,
        decode_tok_s_max: valid.last().expect("non-empty").decode_tok_s,
        ttft_ms: median.ttft_ms,
        itl_p50_ms: median.itl_p50_ms,
        itl_p95_ms: median.itl_p95_ms,
        prefill_tok_s: median.prefill_tok_s,
        n_ctx: median.key.n_ctx,
        backend: median.backend.clone(),
        runs: valid.len() as u32,
        measured_at: median.measured_at,
        measured_build: median.build.clone(),
        stale: median.build != current_build,
        placement_human: median.placement_human.clone(),
        model_name: median.model_name.clone(),
    })
}

/// One concrete way two configurations differ.
///
/// The facet vocabulary is shared with [`NearMiss::differs_by`], and a
/// `Difference` is strictly a *refinement* of it: the same facets, with the two
/// sides described where they can be. Nothing appears here that would not also
/// appear there, so a caller reading only the facet names is never told about a
/// difference the older surface hid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// Stable identifier: `"model"`, `"split"`, `"host-hardware"`,
    /// `"context"`, `"probe-version"`, `"link"`.
    pub facet: &'static str,
    /// What the other configuration had.
    ///
    /// `None` when that side kept no [`PlacementWitness`] — or kept one that
    /// does not account for its own key. The difference is real either way; what
    /// is missing is any honest description of it, and a placeholder here would
    /// be exactly the fabrication this module exists to prevent.
    pub theirs: Option<String>,
    /// What this configuration has, under the same rule.
    pub ours: Option<String>,
}

/// A configuration to compare: a key, plus the witness that explains it when one
/// was kept.
#[derive(Debug, Clone, Copy)]
pub struct Configuration<'a> {
    /// The identity.
    pub key: &'a MeasurementKey,
    /// The pre-image behind [`MeasurementKey::placement_digest`], when recorded.
    pub witness: Option<&'a PlacementWitness>,
}

impl<'a> Configuration<'a> {
    /// A configuration with no witness — all that a pre-2026-07-30 record, or a
    /// caller that has not built one, can offer.
    pub fn unwitnessed(key: &'a MeasurementKey) -> Self {
        Self { key, witness: None }
    }

    /// The witness, but only if it accounts for this key's digest.
    ///
    /// The faithfulness check is applied here, at the point of *use*, rather
    /// than left as something a caller may remember to call. A witness built
    /// from different inputs than the key beside it describes some other
    /// configuration; quoting it would be worse than saying nothing, so it is
    /// treated as absent.
    fn faithful(&self) -> Option<&'a PlacementWitness> {
        self.witness
            .filter(|w| w.explains(&self.key.placement_digest))
    }

    fn split_description(&self) -> Option<String> {
        Some(self.faithful()?.describe_split())
    }

    fn host_description(&self) -> Option<String> {
        Some(
            self.faithful()?
                .machine_with_hw(self.key.host_hw_fingerprint)?
                .describe(),
        )
    }
}

/// Every way `theirs` differs from `ours`, described where it can be.
///
/// The one primitive behind both surfaces that need it: [`near_misses`], which
/// compares a stored record against a plan, and any caller asking the question
/// the store could not answer before a witness existed — *why did these two runs
/// of mine land under different keys?*
///
/// Facet order is by how much it should change a reader's mind, not
/// alphabetical: what model, then how it was split, then on what, then the
/// settings.
pub fn differences(theirs: Configuration<'_>, ours: Configuration<'_>) -> Vec<Difference> {
    let mut out = Vec::new();
    let (t, o) = (theirs.key, ours.key);

    if t.model_fingerprint != o.model_fingerprint {
        out.push(Difference {
            facet: "model",
            theirs: Some(t.model_fingerprint.clone()),
            ours: Some(o.model_fingerprint.clone()),
        });
    }
    if t.placement_digest != o.placement_digest {
        out.push(Difference {
            facet: "split",
            theirs: theirs.split_description(),
            ours: ours.split_description(),
        });
    }
    if t.host_hw_fingerprint != o.host_hw_fingerprint {
        out.push(Difference {
            facet: "host-hardware",
            theirs: theirs.host_description(),
            ours: ours.host_description(),
        });
    }
    if t.n_ctx != o.n_ctx {
        out.push(Difference {
            facet: "context",
            theirs: Some(t.n_ctx.to_string()),
            ours: Some(o.n_ctx.to_string()),
        });
    }
    if t.probe_version != o.probe_version {
        out.push(Difference {
            facet: "probe-version",
            theirs: Some(t.probe_version.to_string()),
            ours: Some(o.probe_version.to_string()),
        });
    }
    if t.link != o.link {
        out.push(Difference {
            facet: "link",
            theirs: Some(t.link.as_str().to_string()),
            ours: Some(o.link.as_str().to_string()),
        });
    }
    out
}

/// A measurement of the same model in a *different* configuration.
///
/// This exists so the tool can say "the split you are proposing has not been
/// measured; the one you are running measured 14.1 tok/s" — which is exactly
/// how an operator decides whether to move the host role. It names the other
/// configuration and its number; it does **not** combine, scale, or interpolate
/// them toward the configuration that was asked about.
#[derive(Debug, Clone, PartialEq)]
pub struct NearMiss {
    /// Human-facing placement of the configuration that *was* measured.
    pub placement_human: String,
    /// Its measured decode rate.
    pub decode_tok_s: f64,
    /// When it was measured.
    pub measured_at: u64,
    /// Which parts of the key differ from the one asked about. Stable
    /// identifiers: `"split"`, `"host-hardware"`, `"context"`,
    /// `"probe-version"`, `"link"`.
    ///
    /// Derived from [`Self::detail`] rather than computed alongside it, so the
    /// two can never disagree about what differs.
    pub differs_by: Vec<&'static str>,
    /// The same differences, with both sides described wherever a
    /// [`PlacementWitness`] allows it.
    ///
    /// This is the surface that carries the weight once a measurement can come
    /// from a machine the reader has never seen: an exact key hit pins the
    /// silicon *and* the split, so a stranger will almost never get one, and
    /// "differs by: split, host-hardware" gives them nothing to judge with.
    /// One entry per element of `differs_by`, in the same order.
    pub detail: Vec<Difference>,
    /// Which machine took this, when it was not this one.
    ///
    /// `None` is the local store — the reader's own past run. `Some(name)` names
    /// the peer whose daemon gossiped it. The distinction is not cosmetic and it
    /// is not presentational: a local near miss is evidence about hardware the
    /// reader controls and can re-measure, a peer's is evidence about hardware
    /// they have never seen and cannot check. Rendering the two identically
    /// would let a stranger's number pass for something the reader had measured,
    /// which is the same failure as an extrapolation, just sourced differently.
    pub taken_by: Option<String>,

    /// One line describing what else was running when this was taken, from
    /// [`RunConditions::describe`]. `None` when the record predates conditions.
    ///
    /// Carried here because this is where a number the reader cannot check gets
    /// offered to them. A peer's rate on a box with three other models resident
    /// and its RSS climbing is a different claim from the same rate on a quiet
    /// one, and a reader shown only the rate is in exactly the position that
    /// produced a false 43% variance on this fleet: comparing two numbers
    /// without being told they were taken under different loads.
    pub conditions: Option<String>,
}

impl NearMiss {
    /// Whether this measured *exactly* the configuration that was asked about.
    ///
    /// Only reachable for a peer's record. A local one is filtered out of
    /// [`near_misses`] by construction, because an exact local hit is what
    /// [`lookup`] is for. A peer's is kept, because a key is a claim about a
    /// configuration and not about a filesystem: someone with the same silicon,
    /// split, link and context measured the thing being asked about, and
    /// discarding that because it arrived over the network would throw away the
    /// most informative record travel can deliver.
    ///
    /// It is still not served as the answer — [`lookup`] reads local records
    /// only, so `mesh plan` continues to say "not measured *here*" and offers
    /// this beside it, attributed. Named rather than left as an empty-vec test
    /// so the branch reads as a decision at the call site.
    pub fn is_exact(&self) -> bool {
        self.detail.is_empty()
    }
}

/// Valid measurements of the same model in other configurations, newest first,
/// drawn from the local store **and** from whatever peers have gossiped.
///
/// Restricted to the same `model_fingerprint`: a different model's number is
/// not a near miss, it is an unrelated fact.
///
/// The two sources are treated alike in every respect but one — each result
/// carries [`NearMiss::taken_by`], so a peer's number can never be read as the
/// reader's own. They are ranked together by recency rather than kept in
/// separate lists, because the question ("what is the closest thing anyone has
/// actually measured?") does not care which disk the answer sat on.
///
/// Local records matching `key` exactly are excluded — that is a hit, not a
/// near miss, and [`lookup`] serves it. Peer records matching exactly are
/// **kept**, with an empty `detail`; see [`NearMiss::is_exact`] for why.
///
/// `peers` may be empty, which is the whole behaviour on a solo node and the
/// behaviour on any node whose daemon is not reachable. Nothing here needs the
/// mesh to be up; a missing peer half is silently a smaller answer, never an
/// error, because the local half is the part the operator can act on today.
///
/// `ours` is the caller's own [`PlacementWitness`], when it has one. Passing
/// `None` costs nothing that was ever there — the facets are still named — but
/// it does mean every `split` and `host-hardware` difference comes back
/// undescribed, because describing a difference needs both sides. That cost
/// lands hardest on the peer half, where those two facets are exactly what
/// differs.
pub fn near_misses(
    file: &MeasurementFile,
    peers: &[ForeignRecord],
    key: &MeasurementKey,
    ours: Option<&PlacementWitness>,
) -> Vec<NearMiss> {
    let mine = Configuration { key, witness: ours };
    let describe = |r: &MeasurementRecord, taken_by: Option<String>| {
        let detail = differences(
            Configuration {
                key: &r.key,
                witness: r.witness.as_ref(),
            },
            mine,
        );
        NearMiss {
            placement_human: r.placement_human.clone(),
            decode_tok_s: r.decode_tok_s,
            measured_at: r.measured_at,
            differs_by: detail.iter().map(|d| d.facet).collect(),
            detail,
            taken_by,
            conditions: r.conditions.as_ref().and_then(|c| c.describe()),
        }
    };

    let mut out: Vec<NearMiss> = file
        .records
        .iter()
        .filter(|r| r.verdict.is_valid())
        .filter(|r| r.key.model_fingerprint == key.model_fingerprint)
        .filter(|r| &r.key != key)
        .map(|r| describe(r, None))
        .collect();

    out.extend(
        peers
            .iter()
            .filter(|f| f.record.verdict.is_valid())
            .filter(|f| f.record.key.model_fingerprint == key.model_fingerprint)
            .map(|f| describe(&f.record, Some(f.describe_origin()))),
    );

    // Recency first; an exact peer hit outranks an older near one at equal
    // timestamps, since it is strictly more informative.
    out.sort_by(|a, b| {
        b.measured_at
            .cmp(&a.measured_at)
            .then_with(|| a.detail.len().cmp(&b.detail.len()))
    });
    out
}

/// Append a run, evicting the oldest under the same key past
/// [`MAX_RUNS_PER_KEY`].
///
/// Runs accumulate rather than overwrite so that repeated measurement shows
/// spread. Eviction is per key, so a busy configuration cannot push another
/// configuration's history out.
pub fn record(file: &mut MeasurementFile, rec: MeasurementRecord) {
    let key = rec.key.clone();
    file.records.push(rec);

    let mut idx: Vec<usize> = file
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.key == key)
        .map(|(i, _)| i)
        .collect();

    while idx.len() > MAX_RUNS_PER_KEY {
        // Oldest by measured_at; ties break on insertion order.
        let victim_pos = idx
            .iter()
            .enumerate()
            .min_by_key(|(_, &i)| (file.records[i].measured_at, i))
            .map(|(pos, _)| pos)
            .expect("idx is non-empty inside the loop");
        let victim = idx.remove(victim_pos);
        file.records.remove(victim);
        for i in idx.iter_mut() {
            if *i > victim {
                *i -= 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Resolved store path, or `None` when disabled.
///
/// `SOVEREIGN_MESH_MEASUREMENTS=0` turns the whole mechanism off: lookups miss
/// and nothing is written, which is the escape hatch for a machine that should
/// not keep this history.
pub fn store_path() -> Option<PathBuf> {
    match std::env::var("SOVEREIGN_MESH_MEASUREMENTS") {
        Ok(v) if v == "0" => None,
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v)),
        _ => dirs::home_dir().map(|h| h.join(".sovereign").join("mesh-measurements.json")),
    }
}

/// Parse a store, discarding one written by an incompatible schema.
///
/// Never fails: an unreadable or superseded file is treated as an empty one,
/// because losing measurement history is an inconvenience while refusing to
/// plan is a broken command.
pub fn parse(contents: &str) -> MeasurementFile {
    match serde_json::from_str::<MeasurementFile>(contents) {
        Ok(f) if f.schema_version == SCHEMA_VERSION => f,
        Ok(f) => {
            tracing::debug!(
                found = f.schema_version,
                expected = SCHEMA_VERSION,
                "mesh-measurements: discarding store written by an incompatible schema"
            );
            MeasurementFile::new()
        }
        Err(e) => {
            tracing::debug!(error = %e, "mesh-measurements: unreadable store — starting empty");
            MeasurementFile::new()
        }
    }
}

/// Read the store from disk. Empty when disabled, absent, or unreadable.
pub fn load() -> MeasurementFile {
    let Some(path) = store_path() else {
        return MeasurementFile::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => parse(&s),
        Err(_) => MeasurementFile::new(),
    }
}

/// Write the store to disk. No-op when disabled.
pub fn save(file: &MeasurementFile) -> std::io::Result<()> {
    let Some(path) = store_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, body)
}

// ---------------------------------------------------------------------------
// Travel
// ---------------------------------------------------------------------------
//
// A measurement is worth most to the machine that did not take it. Locally it
// answers "what did this feel like last time"; on a peer it answers the question
// `mesh plan` exists for — "what will this feel like here" — for a configuration
// the reader has no way to try without buying hardware.
//
// The shape follows the notes precedent: the durable local file is
// authoritative, and the mesh KV store is a wire buffer. Concretely that means
// three things, and the third is the one that is easy to get wrong:
//
//  1. A record is written to disk first and published second. `mesh bench`
//     succeeds with the daemon down; the record simply has not travelled yet.
//  2. Publication is idempotent — `wire_key` is derived from the record, so
//     republishing the same record overwrites its own entry rather than
//     accumulating copies. LWW does the right thing without a sequence number.
//  3. The buffer is *lost on daemon restart*, which is why publication cannot be
//     a one-shot at measure time. The daemon republishes the local file at boot
//     (`bootstrap.rs`). Without that step every node's history would quietly
//     evaporate from the mesh one restart at a time, while still looking correct
//     on the node that owned it.
//
// Peer records are deliberately **not** merged into [`MeasurementFile`]. That
// keeps [`lookup`] meaning exactly what it has always meant — what this machine
// measured — so no peer's number can ever be served as the reader's own. They
// reach the operator through [`near_misses`], attributed, and nowhere else.

/// Gossip namespace for measurement envelopes in the mesh KV store.
///
/// Not in `GOSSIP_EXCLUDED_APP_IDS`, so entries replicate by default. That is
/// the intent: a measurement describes hardware capability, which is the same
/// class of fact the mesh already gossips in `NodeCapabilities`. It carries no
/// prompt text, no corpus content, and no model size — see the module docs.
pub const MEASUREMENTS_APP_ID: &str = "mesh-measurements";

/// A record on the wire, versioned so a future incompatible change is *dropped*
/// by an older reader rather than half-understood.
///
/// Private: the only way to produce these bytes is [`to_wire`] and the only way
/// to read them is [`from_wire`], so the version check cannot be skipped by a
/// caller who forgot it existed. This is the same discipline [`parse`] applies
/// to the file, applied to the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MeasurementEnvelope {
    schema_version: u32,
    record: MeasurementRecord,
}

/// Serialize a record for publication, or `None` if it must not travel.
///
/// Refuses an [`Verdict::Invalid`] run. A failed run is glassbox material *for
/// the operator who ran it* — it says their machine could not do the thing —
/// and `mesh bench --history` shows it. On a peer it is only noise, and worse,
/// it is noise a reader could mistake for a capability claim about hardware they
/// were considering. The local file keeps every run; the wire carries only the
/// ones that mean something to a stranger.
pub fn to_wire(record: &MeasurementRecord) -> Option<Vec<u8>> {
    if !record.verdict.is_valid() {
        return None;
    }
    serde_json::to_vec(&MeasurementEnvelope {
        schema_version: SCHEMA_VERSION,
        record: record.clone(),
    })
    .ok()
}

/// Read a record published by a peer, or `None` if it cannot be trusted as one.
///
/// Never fails loudly: a peer on a different schema, or a corrupt entry, yields
/// `None` and is skipped. One unreadable entry must not cost the reader every
/// other peer's measurements, and there is nothing an operator could do about a
/// remote node's version anyway.
pub fn from_wire(bytes: &[u8]) -> Option<MeasurementRecord> {
    let env: MeasurementEnvelope = serde_json::from_slice(bytes).ok()?;
    if env.schema_version != SCHEMA_VERSION {
        tracing::debug!(
            found = env.schema_version,
            expected = SCHEMA_VERSION,
            "mesh-measurements: dropping a peer record written by an incompatible schema"
        );
        return None;
    }
    Some(env.record)
}

/// The KV key a record publishes under.
///
/// Derived entirely from the record, so publishing the same record twice is a
/// no-op rather than a duplicate — which is what makes the boot republish safe
/// to run on every start.
///
/// `measured_at` leads, zero-padded, so that lexicographic order is
/// chronological order: a raw `scan` of the namespace reads oldest-to-newest
/// without decoding anything, and a date prefix is a usable scan filter. The
/// hash tail is over the key fields *and* the headline rate, so two runs of the
/// same configuration in the same second stay distinct instead of one silently
/// replacing the other.
///
/// The rate enters the hash **quantized to a thousandth of a token per second**,
/// and that is load-bearing rather than tidy. `serde_json` is built here without
/// its `float_roundtrip` feature, so an `f64` can come back from JSON one ULP
/// away from what went in — and a record passes through JSON twice, once to the
/// local file and once to the wire. Hashing the raw bits would therefore let the
/// same measurement compute two different keys depending on which copy you held,
/// and the boot republish would leave an orphan entry behind that LWW could
/// never overwrite. A thousandth of a token per second is far below anything a
/// reader could act on, so the tolerance costs nothing; two runs closer together
/// than that in the same second are the same measurement, and keeping one is
/// right.
pub fn wire_key(record: &MeasurementRecord) -> String {
    let k = &record.key;
    let mut h = Sha256::new();
    h.update(k.model_fingerprint.as_bytes());
    h.update([0u8]);
    h.update(k.placement_digest.as_bytes());
    h.update([0u8]);
    h.update(k.host_hw_fingerprint.to_le_bytes());
    h.update(k.n_ctx.to_le_bytes());
    h.update(k.probe_version.to_le_bytes());
    h.update(k.link.as_str().as_bytes());
    h.update([0u8]);
    h.update(record.measured_at.to_le_bytes());
    h.update(((record.decode_tok_s * 1000.0).round() as i64).to_le_bytes());
    format!("{:010}/{}", record.measured_at, hex16(&h.finalize()))
}

/// A measurement taken by another node, with the identity of the node that took
/// it.
///
/// The origin is *not* a field of [`MeasurementRecord`], and that is deliberate.
/// It comes from the KV entry's own `origin`, stamped by the publishing daemon
/// and carried by gossip — so a node cannot claim to be someone else by writing
/// a name into a payload it controls. A record says what was measured; the
/// envelope around it says who says so.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignRecord {
    /// Hex node id of the publisher. Always present, always the ground truth.
    pub origin_node: String,
    /// Friendly mesh name for that node, resolved against live membership at
    /// read time. `None` when the peer has since left, or was never named.
    pub origin_name: Option<String>,
    /// What they measured.
    pub record: MeasurementRecord,
}

impl ForeignRecord {
    /// How to name the publisher to a reader.
    ///
    /// Prefers the friendly name; falls back to a truncated node id, which is
    /// unlovely but is at least something the operator can match against
    /// `svrn mesh status`. Never falls back to a description of the *hardware* —
    /// that would read as an identity the mesh had verified, and it has not.
    pub fn describe_origin(&self) -> String {
        if let Some(name) = self.origin_name.as_deref().map(str::trim) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
        let short: String = self.origin_node.chars().take(16).collect();
        format!("node-{short}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes() -> Vec<(String, Option<u32>, u64)> {
        vec![
            ("blk.0.attn_q.weight".into(), Some(0), 1_000),
            ("blk.1.ffn_gate.weight".into(), Some(1), 2_000),
            ("output.weight".into(), None, 3_000),
        ]
    }

    fn shards() -> Vec<PlacementShard> {
        vec![
            PlacementShard {
                node_key: "beefymac".into(),
                hw: Some(0xBEEF),
                blocks: Some((0, 11)),
                holds_output: false,
            },
            PlacementShard {
                node_key: "ruggedfox".into(),
                hw: Some(0xF0F),
                blocks: Some((12, 47)),
                holds_output: true,
            },
        ]
    }

    fn key() -> MeasurementKey {
        MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
            LinkClass::Direct,
        )
    }

    /// [`key`] over a tunnel instead of a direct link. Everything else — model,
    /// split, host, context — is byte-identical.
    fn key_tunnelled() -> MeasurementKey {
        MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
            LinkClass::Tunnel,
        )
    }

    // --- link classification -------------------------------------------------

    #[test]
    fn loopback_endpoints_are_tunnels_and_routable_ones_are_direct() {
        for ep in [
            "127.0.0.1:50052",
            "127.0.0.53:50052",
            "localhost:50052",
            "LOCALHOST:50052",
            "[::1]:50052",
        ] {
            assert_eq!(
                link_class_of_endpoint(ep),
                LinkClass::Tunnel,
                "{ep} is a loopback proxy — the far end is a tunnel"
            );
        }
        for ep in [
            "192.168.1.2:50052",
            "100.104.36.28:50052",
            "beefymac.local:50052",
            "[fd7a:115c:a1e0::a3a:241c]:50052",
        ] {
            assert_eq!(
                link_class_of_endpoint(ep),
                LinkClass::Direct,
                "{ep} is routable — ggml dials it directly"
            );
        }
    }

    /// A bare IPv6 literal has no port, so it must not be truncated at its
    /// first colon. `::1` splitting to an empty host would classify the
    /// loopback address as `Unknown` instead of `Tunnel`.
    #[test]
    fn bare_ipv6_is_not_truncated_at_its_first_colon() {
        assert_eq!(link_class_of_endpoint("::1"), LinkClass::Tunnel);
        assert_eq!(
            link_class_of_endpoint("fd7a:115c:a1e0::a3a:241c"),
            LinkClass::Direct
        );
        assert_eq!(link_class_of_endpoint(""), LinkClass::Unknown);
        assert_eq!(link_class_of_endpoint(":50052"), LinkClass::Unknown);
    }

    #[test]
    fn summarize_takes_the_worst_link_and_local_means_no_workers() {
        use LinkClass::*;
        assert_eq!(LinkClass::summarize(&[]), Local);
        assert_eq!(LinkClass::summarize(&[Direct, Direct]), Direct);
        // One tunnelled hop gates the whole pipeline.
        assert_eq!(LinkClass::summarize(&[Direct, Tunnel]), Tunnel);
        // Unknown dominates even a tunnel: we cannot attribute the run at all.
        assert_eq!(LinkClass::summarize(&[Tunnel, Unknown]), Unknown);
        assert_eq!(LinkClass::summarize(&[Direct, Unknown]), Unknown);
    }

    // --- the link is part of the identity ------------------------------------

    /// The defect this field exists to prevent, stated as a test.
    ///
    /// Same model, same split, same host, same context — measured once over a
    /// tunnel. Asking about the direct-link configuration must NOT return that
    /// number. On this fleet the two differ by ~2.3×, so serving one for the
    /// other is not a rounding error, it is a wrong answer delivered
    /// confidently.
    #[test]
    fn a_tunnelled_measurement_is_never_served_for_a_direct_plan() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key_tunnelled(), 100, 17.35, Verdict::Valid));

        assert!(
            lookup(&f, &key(), "0.10.0").is_none(),
            "the direct-link plan must not be answered by a tunnelled run"
        );
        assert_eq!(
            lookup(&f, &key_tunnelled(), "0.10.0")
                .expect("the tunnelled configuration WAS measured")
                .decode_tok_s,
            17.35
        );
    }

    /// …and the operator is told why, rather than just "no data".
    #[test]
    fn a_link_mismatch_surfaces_as_a_near_miss_naming_the_link() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key_tunnelled(), 100, 17.35, Verdict::Valid));

        let near = near_misses(&f, &[], &key(), None);
        assert_eq!(near.len(), 1, "the tunnelled run is a near miss");
        assert_eq!(near[0].differs_by, vec!["link"]);
        assert_eq!(near[0].decode_tok_s, 17.35);
    }

    /// `Unknown` is an absence of evidence, not a value. Two runs nobody could
    /// classify are not thereby the same run.
    #[test]
    fn an_unknown_link_never_matches_even_another_unknown() {
        let unknown = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
            LinkClass::Unknown,
        );
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(unknown.clone(), 100, 14.1, Verdict::Valid));

        assert!(
            lookup(&f, &unknown, "0.10.0").is_none(),
            "an unclassifiable link cannot be answered, even by another one"
        );
        // But the record is still visible as a near miss, so nothing is hidden.
        assert!(!near_misses(&f, &[], &key(), None).is_empty());
    }

    fn rec_at(k: MeasurementKey, at: u64, tok_s: f64, verdict: Verdict) -> MeasurementRecord {
        MeasurementRecord {
            key: k,
            decode_tok_s: tok_s,
            decode_tok_s_min: tok_s - 0.2,
            decode_tok_s_max: tok_s + 0.1,
            ttft_ms: 910.0,
            itl_p50_ms: 71.0,
            itl_p95_ms: 79.0,
            prefill_tok_s: None,
            cold_load_s: Some(112.3),
            trials: 3,
            content_frames: 256,
            model_name: "Qwen3.5-122B".into(),
            placement_human: "36 local + 12 @beefymac".into(),
            nodes: 2,
            hops: 1,
            measured_at: at,
            build: "0.10.0".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: Some(0.4),
            verdict,
            witness: None,
            conditions: None,
        }
    }

    /// The witness that explains [`key`]'s placement digest.
    fn witness() -> PlacementWitness {
        PlacementWitness {
            mode: "distributed".into(),
            total_blocks: 48,
            shards: shards(),
            machines: vec![
                MachineWitness {
                    node_key: "beefymac".into(),
                    vram_gb: 51,
                    backend: Some("metal".into()),
                },
                MachineWitness {
                    node_key: "ruggedfox".into(),
                    vram_gb: 128,
                    backend: Some("vulkan".into()),
                },
            ],
        }
    }

    // --- key discrimination: too-coarse failures -------------------------

    #[test]
    fn key_changes_when_split_changes() {
        let a = placement_digest("distributed", 48, &shards());
        let mut moved = shards();
        moved[0].blocks = Some((0, 17));
        moved[1].blocks = Some((18, 47));
        let b = placement_digest("distributed", 48, &moved);
        assert_ne!(a, b, "a 36/12 split must not match a 30/18 one");
    }

    #[test]
    fn key_changes_when_worker_identity_changes() {
        let a = placement_digest("distributed", 48, &shards());
        let mut other = shards();
        other[0].node_key = "someone-elses-mac".into();
        let b = placement_digest("distributed", 48, &other);
        assert_ne!(
            a, b,
            "the same split on a different peer is a different measurement"
        );
    }

    /// The digest must announce the generation it was actually built with.
    ///
    /// Written because the `pd1`→`pd2` bump half-landed: the hash input changed
    /// and the printed label did not, so for one build every digest was new
    /// bytes wearing the old name — the exact confusion the prefix exists to
    /// prevent, and invisible to every other test here because they all compare
    /// digests to each other rather than to a literal. When the construction
    /// changes again, change this literal in the same commit.
    #[test]
    fn the_digest_label_matches_the_generation_that_produced_it() {
        let d = placement_digest("distributed", 48, &shards());
        assert!(
            d.starts_with("pd2:"),
            "hashing `hw` is the pd2 construction; a pd1 label on it would tell a \
             reader these digests are comparable with older ones: {d}"
        );
    }

    /// The blind spot this field exists to close.
    ///
    /// Same peer name, same split, same everything a `pd1` digest could see —
    /// different silicon. Before `hw` was part of the shard these two hashed
    /// identically, so the number measured on the old GPU answered for the new
    /// one. A name is not hardware.
    #[test]
    fn key_changes_when_a_peer_swaps_hardware_but_keeps_its_name() {
        let a = placement_digest("distributed", 48, &shards());
        let mut regunned = shards();
        regunned[0].hw = Some(0xDEAD);
        let b = placement_digest("distributed", 48, &regunned);
        assert_eq!(
            regunned[0].node_key,
            shards()[0].node_key,
            "precondition: the peer kept its name — only the silicon changed"
        );
        assert_ne!(
            a, b,
            "the same split on the same peer's NEW hardware is a different measurement"
        );
    }

    /// A machine that never said what it is must not be confused with one that
    /// did — in either direction. Both callers refuse to build a key from an
    /// unfingerprinted shard, so this is the backstop for that promise rather
    /// than a path production takes.
    #[test]
    fn an_unfingerprinted_shard_never_collides_with_a_fingerprinted_one() {
        let known = placement_digest("distributed", 48, &shards());
        let mut anonymous = shards();
        anonymous[0].hw = None;
        assert_ne!(
            known,
            placement_digest("distributed", 48, &anonymous),
            "absence of a fingerprint must not hash like the presence of one"
        );
    }

    #[test]
    fn key_changes_between_solo_and_distributed() {
        let solo = placement_digest("local", 48, &shards());
        let dist = placement_digest("distributed", 48, &shards());
        assert_ne!(solo, dist);
    }

    #[test]
    fn key_changes_when_host_hardware_changes() {
        let a = hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "vulkan".into())]);
        let b = hardware_fingerprint(32, 128, &[("RTX 4090".into(), 24, "cuda".into())]);
        assert_ne!(a, b);
    }

    #[test]
    fn key_changes_when_backend_changes_on_identical_silicon() {
        let vulkan =
            hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "vulkan".into())]);
        let rocm = hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "rocm".into())]);
        assert_ne!(
            vulkan, rocm,
            "same GPU under a different backend runs at a different rate — the key must break"
        );
    }

    #[test]
    fn key_changes_when_ctx_changes() {
        let small = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            8_192,
            LinkClass::Direct,
        );
        assert_ne!(small, key());
    }

    #[test]
    fn model_fingerprint_is_quant_sensitive() {
        let mut requantised = sizes();
        requantised[0].2 = 1_500;
        assert_ne!(
            model_fingerprint(&sizes(), 48),
            model_fingerprint(&requantised, 48)
        );
    }

    // --- key stability: too-fine failures --------------------------------

    #[test]
    fn model_fingerprint_is_order_independent() {
        let mut permuted = sizes();
        permuted.reverse();
        assert_eq!(
            model_fingerprint(&sizes(), 48),
            model_fingerprint(&permuted, 48)
        );
    }

    #[test]
    fn placement_digest_is_order_independent() {
        let mut permuted = shards();
        permuted.reverse();
        assert_eq!(
            placement_digest("distributed", 48, &shards()),
            placement_digest("distributed", 48, &permuted)
        );
    }

    #[test]
    fn hardware_fingerprint_is_gpu_order_independent() {
        let a = hardware_fingerprint(
            32,
            128,
            &[
                ("RTX 4090".into(), 24, "cuda".into()),
                ("Radeon 8060S".into(), 128, "vulkan".into()),
            ],
        );
        let b = hardware_fingerprint(
            32,
            128,
            &[
                ("Radeon 8060S".into(), 128, "vulkan".into()),
                ("RTX 4090".into(), 24, "cuda".into()),
            ],
        );
        assert_eq!(
            a, b,
            "driver enumeration order must not invalidate a record"
        );
    }

    #[test]
    fn model_fingerprint_is_stable_across_repeated_reads() {
        assert_eq!(
            model_fingerprint(&sizes(), 48),
            model_fingerprint(&sizes(), 48)
        );
    }

    // --- the hypothetical bar --------------------------------------------

    #[test]
    fn an_unidentified_host_yields_no_identity() {
        assert!(
            HostIdentity::from_live_mesh(None).is_none(),
            "without a host fingerprint there is no key, so `--devices` cannot match a record"
        );
    }

    // --- lookup ----------------------------------------------------------

    #[test]
    fn lookup_returns_none_for_an_unmeasured_key() {
        let f = MeasurementFile::new();
        assert!(lookup(&f, &key(), "0.10.0").is_none());
    }

    #[test]
    fn lookup_ignores_invalid_runs_under_the_exact_key() {
        let mut f = MeasurementFile::new();
        record(
            &mut f,
            rec_at(
                key(),
                100,
                14.1,
                Verdict::Invalid {
                    problems: vec!["peer went offline mid-run".into()],
                },
            ),
        );
        assert!(
            lookup(&f, &key(), "0.10.0").is_none(),
            "a run that tripped a guard is kept for glassbox but must never be served"
        );
    }

    #[test]
    fn lookup_reports_the_median_run_and_the_observed_spread_of_runs() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 13.0, Verdict::Valid));
        record(&mut f, rec_at(key(), 200, 14.1, Verdict::Valid));
        let s = lookup(&f, &key(), "0.10.0").expect("two valid runs");
        assert_eq!(s.runs, 2);
        assert!(
            (s.decode_tok_s - 13.0).abs() < 1e-9,
            "even count: the lower middle, a run that actually happened"
        );
        assert!(
            (s.decode_tok_s_min - 13.0).abs() < 1e-9,
            "min is the slowest RUN, not the slowest trial"
        );
        assert!(
            (s.decode_tok_s_max - 14.1).abs() < 1e-9,
            "max is the fastest RUN, not the fastest trial"
        );
        assert!(!s.stale);
    }

    #[test]
    fn one_outlier_run_cannot_set_the_headline_or_arrive_last_and_steal_it() {
        // The real store that forced the policy: 7.75/8.38/8.53/11.08, where
        // the 11.08 was one coherently-fast outlier and the OLD latest-run
        // headline would have quoted whatever ran most recently.
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 7.75, Verdict::Valid));
        record(&mut f, rec_at(key(), 200, 8.53, Verdict::Valid));
        record(&mut f, rec_at(key(), 300, 8.38, Verdict::Valid));
        record(&mut f, rec_at(key(), 400, 11.08, Verdict::Valid));
        let s = lookup(&f, &key(), "0.10.0").expect("four valid runs");
        assert_eq!(s.runs, 4);
        assert!(
            (s.decode_tok_s - 8.38).abs() < 1e-9,
            "median run headlines even though the outlier arrived last"
        );
        assert!((s.decode_tok_s_min - 7.75).abs() < 1e-9);
        assert!(
            (s.decode_tok_s_max - 11.08).abs() < 1e-9,
            "the outlier is still visible in the observed range, just not the headline"
        );
        assert!(
            (s.measured_at as i64 - 300).abs() < 1,
            "companion fields travel with the median run, not the latest"
        );
    }

    #[test]
    fn a_record_from_another_build_is_served_but_flagged_stale() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
        let s = lookup(&f, &key(), "0.11.0").expect("still served");
        assert!(
            s.stale,
            "a build change is a warning, not a reason to hide the number"
        );
        assert_eq!(s.measured_build, "0.10.0");
    }

    // --- near misses ------------------------------------------------------

    #[test]
    fn a_near_miss_names_the_other_config_and_carries_no_rate_for_ours() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));

        let mut moved = shards();
        moved[0].blocks = Some((0, 17));
        moved[1].blocks = Some((18, 47));
        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &moved),
            32_768,
            LinkClass::Direct,
        );

        assert!(
            lookup(&f, &asked, "0.10.0").is_none(),
            "the configuration asked about was never measured"
        );
        let misses = near_misses(&f, &[], &asked, None);
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].differs_by, vec!["split"]);
        assert!((misses[0].decode_tok_s - 14.1).abs() < 1e-9);
    }

    #[test]
    fn a_different_model_is_not_a_near_miss() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));

        let mut other_model = sizes();
        other_model[0].2 = 9_999;
        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&other_model, 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
            LinkClass::Direct,
        );
        assert!(near_misses(&f, &[], &asked, None).is_empty());
    }

    #[test]
    fn an_exact_hit_is_not_also_reported_as_a_near_miss() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
        assert!(near_misses(&f, &[], &key(), None).is_empty());
    }

    #[test]
    fn an_invalid_run_is_not_a_near_miss_either() {
        let mut f = MeasurementFile::new();
        record(
            &mut f,
            rec_at(
                key(),
                100,
                14.1,
                Verdict::Invalid {
                    problems: vec!["only 10 content frames".into()],
                },
            ),
        );
        let mut moved = shards();
        moved[0].blocks = Some((0, 17));
        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &moved),
            32_768,
            LinkClass::Direct,
        );
        assert!(near_misses(&f, &[], &asked, None).is_empty());
    }

    // --- record retention -------------------------------------------------

    #[test]
    fn runs_accumulate_then_evict_oldest_first_per_key() {
        let mut f = MeasurementFile::new();
        for i in 0..(MAX_RUNS_PER_KEY as u64 + 3) {
            record(&mut f, rec_at(key(), 100 + i, 14.0, Verdict::Valid));
        }
        assert_eq!(f.records().len(), MAX_RUNS_PER_KEY);
        let oldest = f.records().iter().map(|r| r.measured_at).min().unwrap();
        assert_eq!(oldest, 103, "the three oldest runs were evicted");
    }

    #[test]
    fn eviction_is_scoped_to_one_key() {
        let mut f = MeasurementFile::new();
        let other = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(42)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("local", 48, &shards()),
            32_768,
            LinkClass::Direct,
        );
        record(&mut f, rec_at(other.clone(), 1, 11.7, Verdict::Valid));
        for i in 0..(MAX_RUNS_PER_KEY as u64 + 3) {
            record(&mut f, rec_at(key(), 100 + i, 14.0, Verdict::Valid));
        }
        assert!(
            lookup(&f, &other, "0.10.0").is_some(),
            "a busy configuration must not push another one's history out"
        );
    }

    // --- persistence ------------------------------------------------------

    #[test]
    fn a_store_round_trips() {
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
        let json = serde_json::to_string(&f).unwrap();
        let back = parse(&json);
        assert_eq!(
            lookup(&back, &key(), "0.10.0"),
            lookup(&f, &key(), "0.10.0")
        );
    }

    #[test]
    fn a_store_from_an_incompatible_schema_is_discarded_not_misread() {
        let json = r#"{"schema_version":9999,"records":[]}"#;
        assert!(parse(json).records().is_empty());
    }

    #[test]
    fn an_unreadable_store_degrades_to_empty_rather_than_failing() {
        assert!(parse("{ this is not json").records().is_empty());
    }

    #[test]
    fn an_invalid_verdict_round_trips_with_its_problems() {
        let r = rec_at(
            key(),
            100,
            0.0,
            Verdict::Invalid {
                problems: vec!["served by the wrong model".into()],
            },
        );
        let back: MeasurementRecord =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.verdict, r.verdict);
    }

    // --- witness: a record that can explain itself -----------------------
    //
    // These guard the property the store lacked until 2026-07-30: a digest
    // change was unattributable, so two runs under different keys could not be
    // told apart by anything a reader could act on.

    /// [`key`] with the host fingerprint that [`shards`] actually contains, so
    /// the fixture matches what the live callers build — the host is always one
    /// of the machines in its own placement.
    fn witnessed_key() -> MeasurementKey {
        MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(0xF0F)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
            LinkClass::Direct,
        )
    }

    /// The same fleet, weight moved off the host: 24/24 instead of 12/36.
    fn moved_witness() -> PlacementWitness {
        let mut shards = shards();
        shards[0].blocks = Some((0, 23));
        shards[1].blocks = Some((24, 47));
        PlacementWitness {
            shards,
            ..witness()
        }
    }

    fn moved_key() -> MeasurementKey {
        MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(0xF0F)).unwrap(),
            model_fingerprint(&sizes(), 48),
            moved_witness().digest(),
            32_768,
            LinkClass::Direct,
        )
    }

    #[test]
    fn a_witness_accounts_for_the_digest_it_was_built_from() {
        assert!(witness().explains(&witnessed_key().placement_digest));
        assert!(!moved_witness().explains(&witnessed_key().placement_digest));
    }

    #[test]
    fn a_description_is_not_part_of_the_identity() {
        // Improving what a peer advertises about itself must not orphan every
        // record naming it, so `machines` is witnessed but never hashed.
        let mut relabelled = witness();
        relabelled.machines[0].vram_gb = 96;
        relabelled.machines[0].backend = Some("rocm".into());
        assert_eq!(relabelled.digest(), witness().digest());
    }

    #[test]
    fn describe_split_counts_blocks_and_marks_the_output_head() {
        assert_eq!(
            witness().describe_split(),
            "beefymac 12 · ruggedfox 36 +head"
        );
    }

    #[test]
    fn a_near_miss_describes_both_splits_when_both_sides_kept_a_witness() {
        // The headline: the reader is planning 24/24 and the store holds 12/36.
        let mut f = MeasurementFile::new();
        let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
        rec.witness = Some(witness());
        record(&mut f, rec);

        let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
        assert_eq!(near.len(), 1);
        let split = near[0]
            .detail
            .iter()
            .find(|d| d.facet == "split")
            .expect("the split differs");
        assert_eq!(
            split.theirs.as_deref(),
            Some("beefymac 12 · ruggedfox 36 +head")
        );
        assert_eq!(
            split.ours.as_deref(),
            Some("beefymac 24 · ruggedfox 24 +head")
        );
    }

    #[test]
    fn a_near_miss_names_the_machine_when_the_host_hardware_differs() {
        // A measurement that arrived from somewhere else: same model, same
        // split shape, different host silicon. Naming the two machines is the
        // whole value — `differs_by: ["host-hardware"]` is unactionable.
        let mut f = MeasurementFile::new();
        let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
        rec.witness = Some(witness());
        record(&mut f, rec);

        // Ours: the same placement measured with beefymac as the host.
        let mine = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(0xBEEF)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            32_768,
            LinkClass::Direct,
        );
        let near = near_misses(&f, &[], &mine, Some(&witness()));
        let hw = near[0]
            .detail
            .iter()
            .find(|d| d.facet == "host-hardware")
            .expect("the host hardware differs");
        assert_eq!(hw.theirs.as_deref(), Some("128 GB vulkan"));
        assert_eq!(hw.ours.as_deref(), Some("51 GB metal"));
    }

    #[test]
    fn an_unfaithful_witness_is_treated_as_absent_rather_than_quoted() {
        // A witness that does not account for its own key describes some other
        // configuration. Quoting it would be worse than saying nothing.
        let mut f = MeasurementFile::new();
        let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
        rec.witness = Some(moved_witness()); // explains a digest this key doesn't have
        record(&mut f, rec);

        let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
        let split = near[0]
            .detail
            .iter()
            .find(|d| d.facet == "split")
            .expect("the split differs");
        assert_eq!(split.theirs, None, "an unfaithful witness must not be read");
        assert!(split.ours.is_some(), "ours is faithful and still described");
    }

    #[test]
    fn a_witnessless_record_still_names_the_facet_it_cannot_describe() {
        // Every record written before 2026-07-30 is this case, and they are kept
        // rather than discarded — so the surface has to degrade honestly.
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(witnessed_key(), 500, 10.4, Verdict::Valid));

        let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
        assert_eq!(near[0].differs_by, vec!["split"]);
        assert_eq!(near[0].detail[0].theirs, None);
        assert_eq!(
            near[0].detail[0].ours.as_deref(),
            Some("beefymac 24 · ruggedfox 24 +head")
        );
    }

    #[test]
    fn the_settings_are_described_without_any_witness_at_all() {
        // n_ctx, link and probe_version live in the key itself, so even the
        // oldest record gains "32768 vs 8192" over a bare "context".
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(witnessed_key(), 500, 10.4, Verdict::Valid));

        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(0xF0F)).unwrap(),
            model_fingerprint(&sizes(), 48),
            placement_digest("distributed", 48, &shards()),
            8_192,
            LinkClass::Tunnel,
        );
        let near = near_misses(&f, &[], &asked, None);
        let d: Vec<(&str, Option<&str>, Option<&str>)> = near[0]
            .detail
            .iter()
            .map(|d| (d.facet, d.theirs.as_deref(), d.ours.as_deref()))
            .collect();
        assert_eq!(
            d,
            vec![
                ("context", Some("32768"), Some("8192")),
                ("link", Some("direct"), Some("tunnel")),
            ]
        );
    }

    #[test]
    fn differs_by_is_exactly_the_facets_of_detail_in_order() {
        let mut f = MeasurementFile::new();
        let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
        rec.witness = Some(witness());
        record(&mut f, rec);

        let asked = MeasurementKey::for_plan(
            HostIdentity::from_live_mesh(Some(0xBEEF)).unwrap(),
            model_fingerprint(&sizes(), 48),
            moved_witness().digest(),
            8_192,
            LinkClass::Tunnel,
        );
        let near = near_misses(&f, &[], &asked, Some(&moved_witness()));
        let facets: Vec<&str> = near[0].detail.iter().map(|d| d.facet).collect();
        assert_eq!(near[0].differs_by, facets);
        assert_eq!(
            facets,
            vec!["split", "host-hardware", "context", "link"],
            "facet order is by how much it should move a reader, not alphabetical"
        );
    }

    #[test]
    fn a_store_written_before_witnesses_still_loads() {
        // The v1 -> v2 change discarded old rows because the missing field was a
        // KEY field. A witness is explanatory, so these rows are kept and simply
        // cannot describe themselves.
        let mut f = MeasurementFile::new();
        record(&mut f, rec_at(witnessed_key(), 500, 10.4, Verdict::Valid));
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            !json.contains("\"witness\""),
            "a None witness must not be written as a field that looks recorded"
        );

        let back = parse(&json);
        assert_eq!(back.records().len(), 1, "the row survives");
        assert!(back.records()[0].witness.is_none());
    }

    #[test]
    fn a_witness_round_trips_through_the_store() {
        let mut f = MeasurementFile::new();
        let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
        rec.witness = Some(witness());
        record(&mut f, rec);

        let back = parse(&serde_json::to_string(&f).unwrap());
        let w = back.records()[0].witness.as_ref().expect("witness kept");
        assert!(
            w.explains(&back.records()[0].key.placement_digest),
            "a witness must still account for its key after a round trip"
        );
        assert_eq!(w.describe_split(), witness().describe_split());
    }

    // -- Travel -------------------------------------------------------------

    /// A record as a peer would publish it, with the origin the daemon stamps.
    fn from_peer(rec: MeasurementRecord, name: Option<&str>) -> ForeignRecord {
        ForeignRecord {
            origin_node: "b88252e4325bc3771122334455667788".into(),
            origin_name: name.map(str::to_string),
            record: rec,
        }
    }

    #[test]
    fn a_record_survives_the_wire_unchanged() {
        let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
        rec.witness = Some(witness());
        let bytes = to_wire(&rec).expect("a valid run travels");
        let back = from_wire(&bytes).expect("and arrives");

        // Everything identity-bearing must be bit-identical.
        assert_eq!(back.key, rec.key);
        assert_eq!(back.witness, rec.witness);
        assert_eq!(back.verdict, rec.verdict);
        assert_eq!(back.trials, rec.trials);
        assert_eq!(back.content_frames, rec.content_frames);
        assert_eq!(back.measured_at, rec.measured_at);
        assert_eq!(back.build, rec.build);
        assert_eq!(back.model_name, rec.model_name);
        assert_eq!(back.placement_human, rec.placement_human);
        assert_eq!(back.backend, rec.backend);
        assert!(
            (back.decode_tok_s - rec.decode_tok_s).abs() < 1e-9,
            "and the number must survive to well past any precision a reader \
             could act on"
        );
        assert!(
            back.witness
                .as_ref()
                .is_some_and(|w| w.explains(&back.key.placement_digest)),
            "the witness must still explain its own key on the far side — a \
             record that cannot account for itself is exactly what travel is for"
        );
    }

    #[test]
    fn the_wire_may_shift_a_rate_by_one_ulp_and_the_key_does_not_move() {
        // Not hypothetical: `serde_json` is built without `float_roundtrip`, so
        // this is what the pipe actually does. Documented as a test because the
        // consequence — an orphan KV entry LWW can never overwrite — is only
        // obvious once you know the cause.
        let mut rec = rec_at(key(), 500, 0.0, Verdict::Valid);
        rec.decode_tok_s = 10.4 - 0.2; // 10.200000000000001
        let before = wire_key(&rec);

        let back = from_wire(&to_wire(&rec).unwrap()).unwrap();
        assert_ne!(
            back.decode_tok_s.to_bits(),
            rec.decode_tok_s.to_bits(),
            "if this ever starts holding, `float_roundtrip` was enabled \
             somewhere and the quantization below is merely belt-and-braces"
        );
        assert_eq!(
            wire_key(&back),
            before,
            "the same measurement must not compute two different keys depending \
             on which copy of it you happen to be holding"
        );
    }

    #[test]
    fn an_invalid_run_never_travels() {
        let rec = rec_at(
            key(),
            500,
            10.4,
            Verdict::Invalid {
                problems: vec!["trial spread 41% exceeds 25%".into()],
            },
        );
        assert!(
            to_wire(&rec).is_none(),
            "a failed run is glassbox material at home and noise on a peer"
        );
    }

    #[test]
    fn from_wire_drops_a_record_written_by_another_schema() {
        let rec = rec_at(key(), 500, 10.4, Verdict::Valid);
        let mut env: serde_json::Value =
            serde_json::from_slice(&to_wire(&rec).unwrap()).expect("envelope is json");
        env["schema_version"] = serde_json::json!(SCHEMA_VERSION + 7);
        assert!(
            from_wire(serde_json::to_vec(&env).unwrap().as_slice()).is_none(),
            "an unrecognised schema must be dropped, not half-read"
        );
        assert!(
            from_wire(b"{not json").is_none(),
            "and a corrupt entry must not be an error the reader has to handle"
        );
    }

    #[test]
    fn republishing_a_record_overwrites_its_own_entry() {
        let rec = rec_at(key(), 500, 10.4, Verdict::Valid);
        assert_eq!(
            wire_key(&rec),
            wire_key(&rec.clone()),
            "the boot republish runs on every start; a key derived from the \
             record is what keeps that from accumulating copies"
        );

        // Same configuration, same second, different number: two real runs, so
        // two entries.
        let faster = rec_at(key(), 500, 11.9, Verdict::Valid);
        assert_ne!(
            wire_key(&rec),
            wire_key(&faster),
            "two runs must not collide just because they share a timestamp"
        );
    }

    #[test]
    fn wire_keys_sort_chronologically() {
        let early = wire_key(&rec_at(key(), 900, 10.4, Verdict::Valid));
        let late = wire_key(&rec_at(key(), 1_700_000_000, 10.4, Verdict::Valid));
        assert!(
            early < late,
            "zero-padding is what makes a raw `scan` of the namespace readable \
             oldest-first without decoding anything: {early} vs {late}"
        );
    }

    #[test]
    fn a_peer_measurement_is_offered_and_says_whose_it_is() {
        let f = MeasurementFile::new();
        let theirs = from_peer(
            rec_at(witnessed_key(), 500, 11.08, Verdict::Valid),
            Some("BeefyMac"),
        );
        let near = near_misses(&f, &[theirs], &moved_key(), Some(&moved_witness()));
        assert_eq!(near.len(), 1, "an empty local store is not an empty answer");
        assert_eq!(
            near[0].taken_by.as_deref(),
            Some("BeefyMac"),
            "a number from hardware the reader has never seen must be named as such"
        );
    }

    #[test]
    fn a_local_measurement_is_attributed_to_nobody() {
        let mut f = MeasurementFile::new();
        let mut rec = rec_at(witnessed_key(), 500, 10.4, Verdict::Valid);
        rec.witness = Some(witness());
        record(&mut f, rec);
        let near = near_misses(&f, &[], &moved_key(), Some(&moved_witness()));
        assert_eq!(near.len(), 1);
        assert!(
            near[0].taken_by.is_none(),
            "`None` is the reader's own run — the one thing they can re-measure"
        );
    }

    #[test]
    fn a_peer_who_measured_this_exact_configuration_is_kept_and_marked() {
        let f = MeasurementFile::new();
        let asked = witnessed_key();
        let theirs = from_peer(
            rec_at(asked.clone(), 500, 11.08, Verdict::Valid),
            Some("BeefyMac"),
        );
        let near = near_misses(&f, &[theirs], &asked, Some(&witness()));
        assert_eq!(
            near.len(),
            1,
            "an exact peer hit is the most informative thing travel delivers; \
             dropping it because the key matched would throw away the answer"
        );
        assert!(near[0].is_exact());
        assert!(near[0].differs_by.is_empty());
    }

    #[test]
    fn lookup_never_serves_a_peer_number() {
        // The property that lets `near_misses` merge the two sources safely:
        // there is no path by which a peer's record can reach `lookup`, so
        // `mesh plan` keeps saying "not measured **here**".
        let f = MeasurementFile::new();
        let asked = witnessed_key();
        let theirs = from_peer(
            rec_at(asked.clone(), 500, 11.08, Verdict::Valid),
            Some("BeefyMac"),
        );

        assert!(
            lookup(&f, &asked, "0.10.0").is_none(),
            "an exact peer hit must not become a local measurement"
        );
        assert_eq!(
            near_misses(&f, &[theirs], &asked, Some(&witness())).len(),
            1,
            "it is still offered — beside the miss, attributed, not as the answer"
        );
    }

    /// A peer's number is the one the reader cannot check, so the load it was
    /// taken under has to travel with it. Without this the reader is back in the
    /// position that produced a false 43% variance: two rates, no way to know
    /// they were taken on differently-loaded machines.
    #[test]
    fn a_peers_conditions_travel_with_their_number() {
        let f = MeasurementFile::new();
        let asked = witnessed_key();
        let mut busy = rec_at(asked.clone(), 500, 7.75, Verdict::Valid);
        busy.conditions = Some(RunConditions {
            co_resident_roles: vec!["embed".into(), "fast".into()],
            host_rss_mb_before: Some(4_100),
            host_rss_mb_after: Some(4_260),
            host_uptime_s: Some(2_320),
            run_span_s: Some(41.5),
            rpc_endpoints: Vec::new(),
        });
        let theirs = from_peer(busy, Some("BeefyMac"));

        let near = near_misses(&f, &[theirs], &asked, Some(&witness()));
        assert_eq!(near.len(), 1);
        let line = near[0]
            .conditions
            .as_deref()
            .expect("a peer's conditions must reach the surface that offers their number");
        assert!(line.contains("embed"), "{line}");
        assert!(line.contains("fast"), "{line}");
        assert_eq!(near[0].taken_by.as_deref(), Some("BeefyMac"));
    }

    /// An old record carries no conditions, and the surface must say nothing
    /// rather than imply the box was quiet.
    #[test]
    fn a_peer_record_without_conditions_offers_none() {
        let f = MeasurementFile::new();
        let asked = witnessed_key();
        let theirs = from_peer(
            rec_at(asked.clone(), 500, 11.08, Verdict::Valid),
            Some("BeefyMac"),
        );
        let near = near_misses(&f, &[theirs], &asked, Some(&witness()));
        assert_eq!(near.len(), 1);
        assert!(
            near[0].conditions.is_none(),
            "absent conditions must not be rendered as a claim about the box"
        );
    }

    #[test]
    fn an_invalid_peer_record_is_not_offered() {
        // `to_wire` refuses to publish these, so one can only arrive from a
        // peer on a build that did not yet refuse. Filter on read as well:
        // the wire is not a trust boundary we control.
        let f = MeasurementFile::new();
        let theirs = from_peer(
            rec_at(
                witnessed_key(),
                500,
                2.1,
                Verdict::Invalid {
                    problems: vec!["decode stalled".into()],
                },
            ),
            Some("BeefyMac"),
        );
        assert!(near_misses(&f, &[theirs], &moved_key(), Some(&moved_witness())).is_empty());
    }

    #[test]
    fn a_peer_measuring_a_different_model_is_not_a_near_miss() {
        let f = MeasurementFile::new();
        let mut other = witnessed_key();
        other.model_fingerprint = "mf1:something-else".into();
        let theirs = from_peer(rec_at(other, 500, 44.0, Verdict::Valid), Some("BeefyMac"));
        assert!(
            near_misses(&f, &[theirs], &moved_key(), Some(&moved_witness())).is_empty(),
            "a different model's number is an unrelated fact, not a weaker answer"
        );
    }

    #[test]
    fn local_and_peer_measurements_rank_together_by_recency() {
        let mut f = MeasurementFile::new();
        let mut older_local = rec_at(witnessed_key(), 100, 10.4, Verdict::Valid);
        older_local.witness = Some(witness());
        record(&mut f, older_local);

        let newer_peer = from_peer(
            rec_at(witnessed_key(), 900, 11.08, Verdict::Valid),
            Some("BeefyMac"),
        );
        let near = near_misses(&f, &[newer_peer], &moved_key(), Some(&moved_witness()));
        assert_eq!(near.len(), 2);
        assert_eq!(
            near[0].taken_by.as_deref(),
            Some("BeefyMac"),
            "the question is what is the closest thing anyone measured, so the \
             two sources rank in one list rather than local-always-first"
        );
        assert!(near[1].taken_by.is_none());
    }

    #[test]
    fn a_peer_falls_back_to_its_node_id_when_the_mesh_cannot_name_it() {
        let rec = rec_at(key(), 500, 10.4, Verdict::Valid);
        assert_eq!(
            from_peer(rec.clone(), Some("BeefyMac")).describe_origin(),
            "BeefyMac"
        );
        assert_eq!(
            from_peer(rec.clone(), None).describe_origin(),
            "node-b88252e4325bc377",
            "a peer that has left the mesh is still matchable against `mesh status`"
        );
        assert_eq!(
            from_peer(rec, Some("   ")).describe_origin(),
            "node-b88252e4325bc377",
            "a blank name is no name"
        );
    }

    // -----------------------------------------------------------------------
    // Run conditions — the half a witness does not explain
    // -----------------------------------------------------------------------

    fn conditions() -> RunConditions {
        RunConditions {
            co_resident_roles: vec!["embed".into(), "fast".into()],
            host_rss_mb_before: Some(4_100),
            host_rss_mb_after: Some(4_260),
            host_uptime_s: Some(2_320),
            run_span_s: Some(41.5),
            rpc_endpoints: Vec::new(),
        }
    }

    /// The load-bearing rule. Conditions are explanatory, never identity: if
    /// they reached the key, two runs of one configuration taken under any
    /// different load would file under different keys, `lookup` would never
    /// find more than one run, and the variance this field exists to expose
    /// would become structurally invisible.
    #[test]
    fn conditions_never_reach_the_key() {
        let quiet = RunConditions {
            co_resident_roles: vec![],
            host_rss_mb_before: Some(900),
            host_rss_mb_after: Some(905),
            host_uptime_s: Some(90_000),
            run_span_s: Some(38.0),
            rpc_endpoints: Vec::new(),
        };
        let busy = conditions();

        let a = MeasurementRecord {
            conditions: Some(quiet),
            ..rec_at(key(), 1_000, 11.08, Verdict::Valid)
        };
        let b = MeasurementRecord {
            conditions: Some(busy),
            ..rec_at(key(), 2_000, 7.75, Verdict::Valid)
        };

        assert_eq!(a.key, b.key, "conditions must not participate in identity");

        // And both survive in one file, under one key, which is the whole point.
        let mut f = MeasurementFile::new();
        record(&mut f, a);
        record(&mut f, b);
        let s = lookup(&f, &key(), "0.10.0").expect("both runs are one configuration");
        assert_eq!(
            s.runs, 2,
            "a quiet run and a busy run belong to the same configuration"
        );
        assert!(
            (s.decode_tok_s_min - 7.75).abs() < 1e-9 && (s.decode_tok_s_max - 11.08).abs() < 1e-9,
            "the spread of run medians stays visible across conditions, got {}–{}",
            s.decode_tok_s_min,
            s.decode_tok_s_max
        );
    }

    /// A record written before conditions existed must still load. The field is
    /// explanatory, so unlike the v1->v2 key change there is nothing to discard.
    #[test]
    fn a_record_with_no_conditions_still_loads() {
        let mut json = serde_json::to_value(rec_at(key(), 10, 9.0, Verdict::Valid)).unwrap();
        json.as_object_mut().unwrap().remove("conditions");
        assert!(
            !json.as_object().unwrap().contains_key("conditions"),
            "fixture must actually lack the field"
        );
        let back: MeasurementRecord = serde_json::from_value(json).unwrap();
        assert!(back.conditions.is_none(), "absent means not recorded");
    }

    /// Every record filed before routes were captured lacks the field entirely.
    /// It must load as "no route recorded" — and specifically NOT be mistaken
    /// for a local load, which is the one reading that would silently turn a
    /// missing measurement into a claim about the topology.
    #[test]
    fn a_record_with_no_rpc_endpoints_still_loads() {
        let r = MeasurementRecord {
            conditions: Some(conditions()),
            ..rec_at(key(), 10, 9.0, Verdict::Valid)
        };
        let mut json = serde_json::to_value(&r).unwrap();
        let c = json
            .get_mut("conditions")
            .and_then(|c| c.as_object_mut())
            .expect("fixture has conditions");
        c.remove("rpc_endpoints");
        assert!(
            !c.contains_key("rpc_endpoints"),
            "fixture must actually lack the field"
        );

        let back: MeasurementRecord = serde_json::from_value(json).unwrap();
        let back_c = back.conditions.expect("conditions survive");
        assert!(
            back_c.rpc_endpoints.is_empty(),
            "an unrecorded route reads as unrecorded"
        );
        assert!(
            back_c.describe().is_some_and(|d| !d.contains("rpc via")),
            "an unrecorded route must not be described as a route"
        );
    }

    /// The route is why two runs of one configuration can differ, so it has to
    /// be legible in the line an operator actually reads — and named, because
    /// a count cannot distinguish the LAN address from the overlay address.
    #[test]
    fn a_recorded_route_is_named_in_the_operator_line() {
        let c = RunConditions {
            rpc_endpoints: vec!["192.168.1.2:50052".into()],
            ..conditions()
        };
        let line = c.describe().expect("conditions render");
        assert!(
            line.contains("192.168.1.2:50052"),
            "the dialled address must appear verbatim, got {line}"
        );
    }

    #[test]
    fn conditions_round_trip_through_json() {
        let r = MeasurementRecord {
            conditions: Some(conditions()),
            ..rec_at(key(), 10, 9.0, Verdict::Valid)
        };
        let back: MeasurementRecord =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.conditions, r.conditions);
    }

    /// An empty slot list is a FINDING — "nothing else was resident" — not
    /// missing information. A reader who cannot tell those apart cannot use the
    /// field to compare two runs, which is its only purpose.
    #[test]
    fn no_co_residents_is_reported_as_a_finding_not_as_silence() {
        let c = RunConditions {
            co_resident_roles: vec![],
            host_rss_mb_before: None,
            host_rss_mb_after: None,
            host_uptime_s: None,
            run_span_s: None,
            rpc_endpoints: Vec::new(),
        };
        let line = c
            .describe()
            .expect("an empty slot list still says something");
        assert!(
            line.contains("nothing else resident"),
            "expected an explicit finding, got: {line}"
        );
        assert!(!c.had_co_residents());
    }

    #[test]
    fn describe_names_every_co_resident_and_the_rss_climb() {
        let line = conditions().describe().unwrap();
        assert!(line.contains("embed"), "{line}");
        assert!(line.contains("fast"), "{line}");
        assert!(line.contains("4100 MB"), "{line}");
        assert!(
            line.contains("+160 MB"),
            "expected a signed delta, got: {line}"
        );
        assert!(
            line.contains("38m"),
            "expected a compact uptime, got: {line}"
        );
    }

    /// A negative delta means a slot was evicted mid-run, which invalidates the
    /// comparison as surely as a climb does. Reporting it unsigned would hide
    /// the direction and make the two look alike.
    #[test]
    fn an_rss_drop_reports_as_negative() {
        let c = RunConditions {
            host_rss_mb_before: Some(90_000),
            host_rss_mb_after: Some(4_000),
            ..conditions()
        };
        assert_eq!(c.rss_delta_mb(), Some(-86_000));
        assert!(c.describe().unwrap().contains("-86000 MB"));
    }

    #[test]
    fn an_rss_delta_needs_both_ends() {
        let c = RunConditions {
            host_rss_mb_after: None,
            ..conditions()
        };
        assert_eq!(c.rss_delta_mb(), None, "one reading is not a delta");
        let line = c.describe().unwrap();
        assert!(line.contains("4100 MB"), "{line}");
        assert!(!line.contains("+"), "no delta should be claimed: {line}");
    }

    #[test]
    fn human_duration_stays_compact_across_the_ranges() {
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(89), "89s");
        assert_eq!(human_duration(90), "1m");
        assert_eq!(human_duration(2_320), "38m");
        assert_eq!(human_duration(5_400), "1h30m");
        assert_eq!(human_duration(90_061), "25h01m");
    }
}
