// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::capabilities::NodeCapabilities;
use crate::ids::{MeshId, NodeId, NodePubkey};

/// Constant-time equality for a 32-byte secret.
///
/// A plain `==` short-circuits on the first differing byte, so a peer that can
/// time our gossip response learns how long a prefix it guessed correctly and
/// can walk the secret out a byte at a time. `membership::verify_join_key`
/// already pays this cost for the same reason (it compares through
/// `blake3::Hash`, whose `PartialEq` is constant-time); before the credential
/// split the gossip predicate did not, and that gap is not worth carrying
/// forward onto a value that never rotates.
///
/// Accumulate-then-compare rather than early return: the loop runs all 32
/// bytes regardless of input, so the timing carries no information.
/// Whether the operator has declared the fleet fully post-split, so
/// [`Mesh::gossip_authorized`] may refuse the legacy arm instead of falling
/// back to it. Off by default — see `sovereign/DEFAULTS_LEDGER.md`.
///
/// Read once: this sits in the gossip hot path, and a knob whose value can
/// change mid-run would make "which predicate authorized this round" depend on
/// when the round happened.
fn strict_gossip_auth() -> bool {
    static STRICT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STRICT.get_or_init(|| {
        std::env::var("SOVEREIGN_MESH_STRICT_AUTH")
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                v == "1" || v == "true" || v == "on"
            })
            .unwrap_or(false)
    })
}

/// What a gossip round proved about its sender, supplied alongside the payload.
///
/// Deliberately NOT a field on [`Mesh`]: a proof is bound to one sender in one
/// time window, so it describes a round, not the mesh. Carrying it on `Mesh`
/// would persist one round's credential into `mesh.json`.
#[derive(Debug, Default, Clone)]
pub struct GossipAuth {
    /// Who claims to be sending. `None` on a pre-proof peer, whose request
    /// carries no sender identity.
    pub sender: Option<NodeId>,
    /// Keyed-BLAKE3 proof of `mesh_secret` possession — see
    /// [`Mesh::mesh_proof`]. `None` on a pre-proof peer.
    pub proof: Option<String>,
    /// Receiver's clock, for the proof's time window.
    pub now_secs: u64,
}

impl GossipAuth {
    /// No evidence offered — the pre-proof path. Authorization falls through to
    /// comparing raw secrets and then to the legacy arm, exactly as before the
    /// proof existed. This is what keeps a mixed fleet converging.
    pub fn none() -> Self {
        Self::default()
    }
}

/// Which predicate authorized a gossip round.
///
/// One decider (ARCH §10.6). Before this existed, [`MergeReport::peer_pre_split`]
/// re-derived the sender's build generation from the payload — "its
/// `mesh_secret` was zero, so it must be pre-split" — which stopped being true
/// the moment an UPGRADED peer started withholding its secret on purpose. Two
/// upgraded nodes then reported each other pre-split, blocked each other's
/// invite rotation, and resumed putting the raw credential back on the wire.
/// The authorization decision and the generation it implies must come from the
/// same place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GossipAuthArm {
    /// Not authorized. Nothing was merged.
    #[default]
    Refused,
    /// The caller proved possession of `mesh_secret` without sending it.
    /// Definitively post-split, whatever its payload carried.
    Proof,
    /// Both sides sent matching raw secrets — post-split, pre-proof.
    RawSecret,
    /// Authorized on `invite_key_hash` because at least one side has no
    /// secret to compare. The compat arm.
    Legacy,
}

/// Time bucket a gossip proof is bound to, seconds. A captured proof is
/// replayable for at most two of these (see [`Mesh::verify_mesh_proof`], which
/// accepts the previous window for clock skew).
///
/// 30s against a 10s gossip cadence: long enough that a round never lands in a
/// window neither side accepts, short enough that a sniffed proof is stale
/// before it is useful.
pub const PROOF_WINDOW_SECS: u64 = 30;

/// The "no gossip credential" sentinel. `mesh_secret` is all-zero exactly when
/// it is ABSENT — a `mesh.json` or a gossip payload written before the
/// credential split has no such field and serde defaults it — and all-zero is
/// never a legitimate value.
///
/// Named because the same 32 zero bytes are also the sentinel for
/// `PersistedMesh::mesh_secret` in another crate, and a sentinel spelled by
/// hand in two crates is one rename away from disagreeing.
pub const MESH_SECRET_UNSET: [u8; 32] = [0u8; 32];

/// 32-byte adapter over [`crate::ct::constant_time_eq`]. Kept as a name because
/// every caller here compares fixed-width credentials and the slice form would
/// invite a length check at each site.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    crate::ct::constant_time_eq(a, b)
}

/// A Commonwealth mesh — a closed group of trusted nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mesh {
    pub id: MeshId,
    pub name: String,
    /// Gossip auth — "are we the same mesh". Minted once at creation and
    /// **never rotated**: there is no setter, no parameter that reaches it,
    /// and [`crate::mesh::Mesh::rotate_invite_key`] cannot name it. That is
    /// deliberate (ARCH §7.1) — the whole point of splitting this out of
    /// `invite_key_hash` is that rotating an invite must not be able to
    /// partition the mesh.
    ///
    /// `[0u8; 32]` means "not set", which happens two ways: a peer running a
    /// pre-split build (the field is absent from its wire payload and serde
    /// defaults it), or a `mesh.json` written before the split and not yet
    /// migrated. Both are handled by [`Mesh::gossip_authorized`].
    #[serde(default)]
    pub mesh_secret: [u8; 32],

    /// Join admission — "may this node in". BLAKE3 hash of the invite key;
    /// the raw key is never persisted. Rotates freely, and rotation is
    /// invisible to gossip because [`Mesh::gossip_authorized`] does not read
    /// it once both sides carry a `mesh_secret`.
    ///
    /// Serialized under its historical name so wire and `mesh.json` bytes stay
    /// identical for pre-split peers.
    #[serde(rename = "join_key_hash")]
    pub invite_key_hash: [u8; 32],

    /// When the current invite key stops being accepted, unix seconds.
    ///
    /// This lived in `AppState` as per-node RAM until the split, which meant
    /// it died on restart and was never set at all on any member that had not
    /// personally minted the invite — so a joiner aimed at any other member
    /// bypassed the TTL entirely. It is mesh state: it persists, it gossips,
    /// and every admitting member enforces the same value.
    ///
    /// `None` = no expiry (plaintext meshes set none).
    #[serde(default)]
    pub invite_expires_at: Option<u64>,

    /// Monotonic counter over the invite credential, bumped by
    /// [`Mesh::rotate_invite_key`] and by nothing else.
    ///
    /// Without it a rotation could not propagate. [`Mesh::merge_from`] merges
    /// members by per-record `last_seen`, but the invite is mesh-wide and has
    /// no such clock, so before this existed the merge simply skipped
    /// `invite_key_hash` and `invite_expires_at` entirely — a founder's rotate
    /// stayed node-local, every other member kept admitting joiners on the
    /// REVOKED key indefinitely, and the TTL was never enforced anywhere but
    /// on the node that minted it.
    ///
    /// Same anti-rollback rule as [`MemberRecord::dial_info_version`], which
    /// solves the identical problem for dial info one field over: a replayed
    /// older payload can never win. Reusing that rule rather than inventing a
    /// second ordering scheme is deliberate (ARCH §10.6).
    ///
    /// `0` = never rotated, which is also what a pre-`invite_version` peer's
    /// payload deserializes to. That is correct rather than merely convenient:
    /// such a peer cannot have rotated in a way this node needs to learn.
    #[serde(default)]
    pub invite_version: u64,
    /// Mesh-wide encryption policy, set by the founder at creation and
    /// inherited by every joiner (it rides the same live gossip + join
    /// snapshot as [`Self::invite_key_hash`]). When `true`, every member
    /// enforces dial-by-key iroh transport for all traffic classes with
    /// no plaintext fallback, closes its plaintext ingress, and requires
    /// an encrypted join. Founder-set and **monotonic**: [`Self::merge_from`]
    /// only ever turns this ON (stricter-wins), so no peer — stale or
    /// hostile — can demote an encrypted mesh to plaintext. `#[serde(default)]`
    /// keeps wire/persist bytes identical for pre-policy nodes (they
    /// read and write `false`).
    #[serde(default)]
    pub require_encryption: bool,
    pub members: HashMap<NodeId, MemberRecord>,
    pub peers: Vec<MeshPeering>,
}

/// Record of a member node in the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRecord {
    pub node_id: NodeId,
    pub name: String,
    pub invited_by: NodeId,
    pub joined_at: u64,
    pub last_seen: u64,
    pub status: NodeStatus,
    pub capabilities: NodeCapabilities,
    pub addresses: Vec<SocketAddr>,
    /// Ed25519 identity key (see [`NodePubkey`]). `None` for nodes
    /// running pre-identity builds. Serde-defaulted both directions:
    /// old nodes ignore the field on receive and new nodes read old
    /// payloads as `None`; `skip_serializing_if` keeps new→old wire
    /// bytes identical when no key exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_pubkey: Option<NodePubkey>,
    /// iroh relay URL for dial-by-key reachability (Track W2 of
    /// TRANSPORT_MIGRATION.md). `None` when this node isn't reachable
    /// over iroh (iroh disabled, or no relay connected yet). Together
    /// with [`Self::node_pubkey`] and [`Self::iroh_direct_addrs`] this
    /// is everything a peer needs to dial this node by key — the
    /// "membership = dialability" collapse. Unlike `node_pubkey` (an
    /// immutable identity, anti-downgrade-protected in `merge_from`),
    /// this is MUTABLE reachability: it rides normal last-seen LWW, so
    /// a node that gains/loses a relay updates peers within one round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// iroh direct (hole-punched / LAN) socket hints for dial-by-key.
    /// Empty when unknown. Mutable reachability — rides normal LWW like
    /// [`Self::relay_url`]. Lets a LAN peer dial without a relay round
    /// trip; iroh still verifies the key on connect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iroh_direct_addrs: Vec<SocketAddr>,
    /// Monotonic counter for this node's SIGNED dial info (relay_url +
    /// iroh_direct_addrs). Bumped by the OWNER each time its reachability
    /// changes; the anti-rollback key in [`Mesh::merge_from`] — a merge
    /// adopts dial info only if its version is `>=` the version we hold,
    /// so a replayed older signed record can't roll a node back. `0` =
    /// legacy / unsigned. See [`crate::dial_sig`].
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub dial_info_version: u64,
    /// Hex-encoded Ed25519 signature by this node's `node_pubkey` over the
    /// canonical dial-info message (`crate::dial_sig::dial_info_message`).
    /// When present and valid, the dial info is tamper-evident: only the
    /// key-holder can change its own reachability, so a gossip-strip
    /// attacker past the join-key gate cannot force a peer unreachable or
    /// (on a non-required class) downgrade it. `None` = legacy / unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dial_info_sig: Option<String>,
    /// Tombstone: unix-seconds at which this member was removed (graceful
    /// `leave` or `revoke_member`). `None` = active member. Gossiped
    /// (wire-compatible like `node_pubkey`): a tombstone propagates mesh-wide
    /// and, via the event-time LWW in [`Mesh::merge_from`], out-competes any
    /// stale *live* record so a departed node can't be resurrected by a peer
    /// still holding its old record — while a genuine rejoin (activity newer
    /// than the removal) still wins. Read paths filter `removed_at.is_some()`
    /// out of active / online views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<u64>,
}

/// `skip_serializing_if` predicate for the dial-info version, so an
/// unsigned/legacy record (version 0) stays byte-identical on the wire.
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl MemberRecord {
    /// Whether this member is active (not tombstoned). Read paths
    /// (online counts, gossip targets, knowledge fan-out roster) filter on
    /// this so a departed node is invisible to scheduling while its tombstone
    /// still circulates for convergence.
    pub fn is_active(&self) -> bool {
        self.removed_at.is_none()
    }

    /// Whether this member has ANY path we could dial — an IP address
    /// OR an iroh path (a pubkey plus a relay or a direct addr). Peer
    /// selection (inference roster, knowledge fan-out, RPC-worker
    /// discovery) filters on this so a node reachable ONLY over iroh
    /// (no gossiped IP — the no-VPN case) is not dropped before the
    /// `PeerTransport` seam is even consulted. The seam still makes the
    /// final per-class routing decision; this is only "is there any
    /// point offering this peer at all."
    pub fn is_dialable(&self) -> bool {
        !self.addresses.is_empty()
            || (self.node_pubkey.is_some()
                && (self.relay_url.is_some() || !self.iroh_direct_addrs.is_empty()))
    }

    /// The timestamp of this record's latest lifecycle event — the later of
    /// its self-stamped `last_seen` and its `removed_at` tombstone. This is the
    /// LWW key in [`Mesh::merge_from`]: a removal stamped after the node's last
    /// heartbeat out-competes stale live copies, but a rejoin whose `last_seen`
    /// post-dates the removal out-competes the tombstone.
    fn event_time(&self) -> u64 {
        self.last_seen.max(self.removed_at.unwrap_or(0))
    }
}

/// This node's current iroh dial info, pulled fresh each gossip round
/// from the live endpoint and stamped into its own [`MemberRecord`].
/// A plain struct (no iroh types) so it crosses the
/// `commonwealth-core` boundary — the `commonwealth-api` `AppState`
/// stores a type-erased provider yielding this, installed by the
/// daemon (which owns the iroh endpoint). Empty/`None` fields mean
/// "not reachable that way yet"; the values change over a node's
/// lifetime as iroh discovers a relay and hole-punches direct paths.
#[derive(Debug, Clone, Default)]
pub struct IrohDialInfo {
    pub relay_url: Option<String>,
    pub direct_addrs: Vec<SocketAddr>,
}

/// Current status of a node as observed by the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Online,
    /// Under heavy local load.
    Busy,
    /// Not responding but not formally departed.
    Away,
    /// Gracefully disconnected.
    Offline,
}

/// Trust relationship with a peer mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPeering {
    pub peer_mesh_id: MeshId,
    pub peer_mesh_name: String,
    pub trust_level: PeerTrustLevel,
    pub established_at: u64,
    pub contact_nodes: Vec<SocketAddr>,
}

/// Level of trust between peered meshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerTrustLevel {
    /// Share model files and corpus indexes only.
    ModelAndKnowledgeSharing,
    /// Share everything plus allow overflow inference routing.
    Full,
}

/// Summary of what a `Mesh::merge_from` call did. Used for tracing
/// ("we learned about 1 new member") and test assertions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Members that were absent locally and got added from `other`.
    pub added: usize,
    /// Members that existed locally but were replaced by a newer
    /// record from `other` (higher `last_seen`).
    pub updated: usize,
    /// Whether the peer we merged FROM is running a pre-split build — its
    /// payload carried no [`Mesh::mesh_secret`], so serde defaulted it to
    /// zero.
    ///
    /// This is the ONLY moment that fact is observable: it lives in the
    /// gossip payload, not in any member record, so a caller that does not
    /// capture it here cannot recover it later. `EmbeddedDaemon::rotate_invite`
    /// needs it, because rotating while such a peer is online partitions
    /// exactly that peer (it still authorizes gossip on `invite_key_hash`).
    ///
    /// Meaningful only when [`Self::rejected`] is false — a refused merge
    /// tells us nothing about the sender's build.
    ///
    /// Derived from [`Self::auth_arm`], never from the payload alone: an
    /// upgraded peer withholds its `mesh_secret` deliberately, so a zeroed
    /// field is no longer evidence of a pre-split build.
    pub peer_pre_split: bool,
    /// Which predicate authorized this merge. The caller's reply uses it to
    /// decide whether the raw `mesh_secret` still needs to be on the wire —
    /// see `routes_internal::gossip`.
    pub auth_arm: GossipAuthArm,
    /// True when the merge was refused outright because `other`
    /// described a different mesh (mismatching `id` or
    /// `invite_key_hash`). When set, nothing was mutated.
    pub rejected: bool,
    /// Node IDs whose records we just observed advance (added or
    /// LWW-updated) in this merge. The caller stamps these in its local
    /// liveness map (`AppState::observe_peer_contact`) so offline-decay
    /// measures *local observation staleness*, not the peer's own
    /// (possibly clock-skewed) `last_seen`. Empty on a rejected merge.
    pub observed: Vec<NodeId>,
}

impl Mesh {
    /// Merge another view of this mesh into `self`. Per-member
    /// `last_seen` acts as the Lamport-ish clock: the record with
    /// the higher timestamp wins. Own record (the caller's
    /// `self_node_id`) is *never* overwritten via gossip — we are
    /// always authoritative for our own liveness, capabilities,
    /// addresses, etc. Peers can learn about us from others, but
    /// they can't replace what we know about ourselves.
    ///
    /// Returns a [`MergeReport`] so callers can surface "added 1,
    /// updated 0" in tracing logs — useful for noticing when gossip
    /// is actually converging vs. spinning.
    ///
    /// Rejects outright when [`Mesh::gossip_authorized`] says no — that's the
    /// auth boundary. Anyone who knows our mesh_id (public via mDNS) but not
    /// the mesh secret shouldn't be able to inject members into our view.
    pub fn merge_from(&mut self, self_node_id: NodeId, other: &Mesh) -> MergeReport {
        self.merge_from_authenticated(self_node_id, other, &GossipAuth::none())
    }

    /// [`Mesh::merge_from`] with the round's authentication evidence attached.
    ///
    /// The evidence is per-ROUND, not per-`Mesh` — a proof is bound to one
    /// sender in one time window — so it is a parameter rather than a field.
    /// Putting it on `Mesh` would persist a single round's credential into
    /// `mesh.json` and invite exactly the confusion between "state" and
    /// "what this caller just proved" that the credential split exists to undo.
    ///
    /// `merge_from` delegates here with [`GossipAuth::none`], which is
    /// byte-for-byte today's behaviour: no proof, fall through to comparing
    /// raw secrets, then the legacy arm. That is what keeps a mixed fleet
    /// converging while the proof path rolls out.
    pub fn merge_from_authenticated(
        &mut self,
        self_node_id: NodeId,
        other: &Mesh,
        auth: &GossipAuth,
    ) -> MergeReport {
        let arm = self.gossip_authorized_with(other, auth);
        if arm == GossipAuthArm::Refused {
            return MergeReport {
                added: 0,
                updated: 0,
                rejected: true,
                observed: Vec::new(),
                peer_pre_split: false,
                auth_arm: GossipAuthArm::Refused,
            };
        }

        // Mesh-wide encryption policy is monotonic: stricter wins. A peer
        // (stale or hostile, but past the invite_key_hash auth boundary
        // above) advertising `require_encryption = false` can never relax
        // a local `true`. Once a node learns the mesh is encrypted, no
        // gossip round demotes it to plaintext — this only ever turns ON.
        if other.require_encryption {
            self.require_encryption = true;
        }

        // Carry a rotation. `rotate_invite`'s doc says it mutates the live mesh
        // and lets "the ordinary gossip round carry it" — this is the line that
        // makes that true. Without it the claim was false: the round carried
        // the new hash on the wire and the merge dropped it on the floor.
        if self.merge_invite_from(other) {
            tracing::info!(
                mesh = %self.name,
                invite_version = self.invite_version,
                "gossip: adopted a rotated invite from a peer"
            );
        }
        // Whether to REJECT unsigned dial info outright (WS-D). An
        // encrypted mesh trusts only signed reachability; a plaintext
        // mesh accepts unsigned dial info where there is no already-trusted
        // signed path to protect (legacy LWW). Captured after the
        // monotonic policy OR-in above.
        let enforce_signed = self.require_encryption;

        let mut report = MergeReport::default();
        // Captured here and nowhere else: the sender's build generation is a
        // property of THIS ROUND, and it is gone the moment the merge ends.
        //
        // A proof settles it outright — only a holder of the current secret
        // can produce one, so the sender is post-split no matter what its
        // payload carried. Reading the payload alone was correct only while
        // every post-split node still shipped the raw secret; an upgraded
        // peer now zeroes that field ON PURPOSE once it has confirmed us,
        // and calling that pre-split flips the pair back to sending the
        // credential and blocks rotation on both sides.
        report.auth_arm = arm;
        report.peer_pre_split = arm != GossipAuthArm::Proof && !other.has_mesh_secret();
        for (id, incoming) in &other.members {
            if *id == self_node_id {
                // Authoritative-for-self: never accept an incoming
                // record about us, regardless of its `last_seen`.
                // If a buggy peer has a stale view of us, we'll
                // correct them on our next gossip round when we
                // ship our current record in the push-pull reply.
                continue;
            }
            match self.members.get(id) {
                None => {
                    // First sight of this member: trust its dial info only
                    // if it is validly signed, else clear it (a new member
                    // can't be poisoned with attacker-supplied reachability
                    // on first contact). The member is still added.
                    let mut record = incoming.clone();
                    Self::reconcile_dial_info(&mut record, None, enforce_signed);
                    let active = record.is_active();
                    self.members.insert(*id, record);
                    report.added += 1;
                    // A tombstone we've never seen is still added (so it
                    // converges mesh-wide), but it is not "observed alive".
                    if active {
                        report.observed.push(*id);
                    }
                }
                Some(existing) if incoming.event_time() > existing.event_time() => {
                    // Anti-downgrade: a newer record relayed by a
                    // pre-identity build carries `node_pubkey: None`.
                    // Without this preservation, ONE old peer in the
                    // gossip path strips every node's pubkey on each
                    // LWW win. An identity key never changes within
                    // a membership, so keeping the locally-known key
                    // while taking the rest of the newer record is
                    // always correct.
                    let preserved_pubkey = match incoming.node_pubkey {
                        Some(pk) => Some(pk),
                        None => existing.node_pubkey,
                    };
                    let mut record = incoming.clone();
                    record.node_pubkey = preserved_pubkey;
                    // The non-security fields (last_seen, status, …) take
                    // the LWW win, but dial info travels only if signed +
                    // fresh; otherwise it is pinned to the value we already
                    // trust. So a forged-newer record advances liveness but
                    // cannot move a peer's reachability (WS-D).
                    Self::reconcile_dial_info(&mut record, Some(existing), enforce_signed);
                    let active = record.is_active();
                    self.members.insert(*id, record);
                    report.updated += 1;
                    if active {
                        report.observed.push(*id);
                    }
                }
                Some(_) => {
                    // Existing is equal or newer — keep ours.
                }
            }
        }
        report
    }

    /// Proof that we hold `mesh_secret`, without transmitting it.
    ///
    /// The secret used to ride every gossip round in cleartext on a
    /// `require_encryption = false` mesh. The field it replaced was a one-way
    /// hash precisely so nothing recoverable travelled, and this credential is
    /// worse to lose than that one was: it never rotates, and
    /// [`Mesh::rotate_invite_key`] structurally cannot change it, so a passive
    /// sniffer gained permanent gossip auth with no revocation path.
    ///
    /// Whether this mesh holds a gossip credential at all.
    ///
    /// One decider for a sentinel that was spelled two ways in this one file —
    /// a bare `[0u8; 32]` at three sites and a local `const UNSET` at four —
    /// and read at seven (ARCH §10.6). The sentinel itself is
    /// [`MESH_SECRET_UNSET`], which the one other crate that must spell it
    /// (`sovereign_mesh::persist`) now shares.
    pub fn has_mesh_secret(&self) -> bool {
        self.mesh_secret != MESH_SECRET_UNSET
    }

    /// Bound to two things, each closing a replay:
    /// - `sender` — a proof captured from one peer cannot be presented by
    ///   another, so an eavesdropper cannot borrow a member's identity.
    /// - a [`PROOF_WINDOW_SECS`] time bucket — a captured proof expires,
    ///   rather than being a bearer token forever.
    ///
    /// Keyed BLAKE3 rather than hash-of-concatenation: the secret is the key,
    /// so a length-extension or prefix game on the message cannot forge one.
    /// `None` when we hold no secret. A node that has not migrated MUST NOT
    /// offer a proof: the receiver would refuse it (it cannot verify against an
    /// unset secret), and because an offered-and-failed proof is a hard refusal
    /// rather than a downgrade, two un-migrated nodes would hard-refuse each
    /// other and partition. Caught by `gossip_integration`'s round-trip tests,
    /// which is exactly what they are for.
    pub fn mesh_proof(&self, sender: NodeId, now_secs: u64) -> Option<String> {
        if !self.has_mesh_secret() {
            return None;
        }
        Some(Self::proof_for(
            &self.mesh_secret,
            self.id,
            sender,
            now_secs / PROOF_WINDOW_SECS,
        ))
    }

    fn proof_for(secret: &[u8; 32], mesh_id: MeshId, sender: NodeId, window: u64) -> String {
        let mut hasher = blake3::Hasher::new_keyed(secret);
        hasher.update(mesh_id.as_bytes());
        hasher.update(sender.as_bytes());
        hasher.update(&window.to_be_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// Whether `proof` was produced by a holder of our `mesh_secret` acting as
    /// `sender`, in this window or the previous one.
    ///
    /// The previous window is accepted for clock skew and for a round that
    /// straddles a boundary — without it, one gossip in every
    /// [`PROOF_WINDOW_SECS`] would fail for no reason. It is a bounded
    /// concession: the replay horizon is two windows, never unbounded.
    ///
    /// Returns false when we have no secret: an unset secret would key every
    /// proof identically across every un-migrated mesh, which is worse than
    /// refusing.
    pub fn verify_mesh_proof(&self, proof: &str, sender: NodeId, now_secs: u64) -> bool {
        if !self.has_mesh_secret() {
            return false;
        }
        let window = now_secs / PROOF_WINDOW_SECS;
        // Constant-time compare on each candidate: a proof is a secret-derived
        // value, and `String == String` short-circuits.
        [window, window.saturating_sub(1)].iter().any(|w| {
            let expected = Self::proof_for(&self.mesh_secret, self.id, sender, *w);
            crate::ct::constant_time_eq(expected.as_bytes(), proof.as_bytes())
        })
    }

    /// Whether `other`'s gossip may merge into ours — the auth boundary.
    ///
    /// Before the credential split one field, `invite_key_hash`, answered both
    /// this question and "may this node join". That conflation is why rotating
    /// an invite partitioned the mesh: re-keying admission also re-keyed
    /// gossip, so every peer still holding the old hash rejected us and we
    /// rejected them, symmetrically. `mesh_secret` never rotates, so this
    /// predicate is stable across any number of invite rotations.
    ///
    /// The compat arm below is a **temporary second decider** (a deliberate
    /// ARCH §10.6 deviation, ledgered in `sovereign/DEFAULTS_LEDGER.md`): a
    /// pre-split peer sends a zeroed `mesh_secret`, and refusing it would
    /// partition the mesh on upgrade — exactly the failure this change
    /// removes. It falls back to the legacy predicate and says so at `warn`,
    /// naming the mesh, so "is this still load-bearing" is observable rather
    /// than remembered. Delete this arm, and the zero-checks with it, once no
    /// node reports it.
    fn gossip_authorized_with(&self, other: &Mesh, auth: &GossipAuth) -> GossipAuthArm {
        if self.id != other.id {
            return GossipAuthArm::Refused;
        }
        // PREFERRED: the caller proved it holds the secret without sending it.
        // Tried first so an upgraded pair never needs the raw credential on the
        // wire at all — that is the whole point of the proof.
        if let (Some(sender), Some(proof)) = (auth.sender, auth.proof.as_deref()) {
            if self.verify_mesh_proof(proof, sender, auth.now_secs) {
                return GossipAuthArm::Proof;
            }
            // A proof that was OFFERED and did not verify is a failure, not an
            // invitation to try a weaker predicate. Falling through here would
            // let an attacker strip the proof — or send a junk one — and be
            // handed the legacy arm, which is the downgrade this ordering
            // exists to prevent.
            tracing::warn!(
                mesh = %self.name,
                %sender,
                "gossip: REFUSED — a mesh_proof was offered and did not verify"
            );
            return GossipAuthArm::Refused;
        }
        if self.has_mesh_secret() && other.has_mesh_secret() {
            return if ct_eq(&self.mesh_secret, &other.mesh_secret) {
                GossipAuthArm::RawSecret
            } else {
                GossipAuthArm::Refused
            };
        }
        // Strict mode: refuse the legacy arm outright once the operator says
        // the fleet is upgraded. Only meaningful when WE have a secret — if
        // ours is unset we have not migrated and strict would refuse every
        // peer, self-partitioning the node it was meant to protect.
        if strict_gossip_auth() && self.has_mesh_secret() {
            tracing::warn!(
                mesh = %self.name,
                "gossip: REFUSED a legacy-auth peer — SOVEREIGN_MESH_STRICT_AUTH \
                 is on. Turn it off if any node is still pre-split."
            );
            return GossipAuthArm::Refused;
        }
        tracing::warn!(
            mesh = %self.name,
            self_has_secret = self.has_mesh_secret(),
            peer_has_secret = other.has_mesh_secret(),
            "gossip: legacy auth — a peer on a pre-split build authorized on \
             invite_key_hash. Invite rotation can still partition this mesh \
             until every node is upgraded."
        );
        if ct_eq(&self.invite_key_hash, &other.invite_key_hash) {
            GossipAuthArm::Legacy
        } else {
            GossipAuthArm::Refused
        }
    }

    /// Replace the invite credential. The **only** way to change admission,
    /// and structurally incapable of touching [`Mesh::mesh_secret`] — that is
    /// the invariant the whole split exists to hold (ARCH §7.1), so it is
    /// expressed as a method that cannot name the field rather than as a
    /// comment asking callers not to.
    pub fn rotate_invite_key(&mut self, new_hash: [u8; 32], expires_at: Option<u64>) {
        self.invite_key_hash = new_hash;
        self.invite_expires_at = expires_at;
        // Bump LAST and always: this is what makes the rotation travel. A
        // rotation that does not advance the version is invisible to every
        // other member, which is precisely the node-local rotate this counter
        // exists to end.
        self.invite_version = self.invite_version.saturating_add(1);
    }

    /// Adopt `other`'s invite credential if it is strictly newer than ours.
    ///
    /// Returns whether anything changed, so the caller can log a real rotation
    /// rather than every merge.
    ///
    /// The three invite fields move TOGETHER or not at all. Merging the hash
    /// without the expiry would admit joiners on a new key with a stale TTL,
    /// which is a worse state than either endpoint.
    ///
    /// Ties are broken by hash, not by "keep ours". Two nodes that rotate in
    /// the same round land on the same version with different hashes, and
    /// "keep ours" is not a decision — it is each node keeping a different
    /// answer forever. Comparing hashes is a total order every node computes
    /// identically, so the mesh converges on one invite instead of splitting
    /// into two admission regimes.
    fn merge_invite_from(&mut self, other: &Mesh) -> bool {
        let newer = other.invite_version > self.invite_version;
        let tie_break = other.invite_version == self.invite_version
            && other.invite_key_hash > self.invite_key_hash;
        if !(newer || tie_break) {
            return false;
        }
        self.invite_key_hash = other.invite_key_hash;
        self.invite_expires_at = other.invite_expires_at;
        self.invite_version = other.invite_version;
        true
    }

    /// Whether the current invite has lapsed at `now` (unix seconds).
    /// No expiry set means the invite does not lapse.
    pub fn invite_expired_at(&self, now: u64) -> bool {
        matches!(self.invite_expires_at, Some(exp) if now >= exp)
    }

    /// WS-D dial-info reconciliation. `record` already has its
    /// `node_pubkey` preserved by the caller. Trust `record`'s dial info
    /// (relay + addrs) only if it is signed, verifies under that pubkey,
    /// and — versus an `existing` record — is not a version rollback.
    /// Otherwise pin the dial info to `existing`'s already-trusted value,
    /// or clear it on first sight, so a gossip-strip attacker past the
    /// join-key gate cannot move a peer's reachability.
    ///
    /// `enforce_signed` (the mesh's `require_encryption`) rejects ALL
    /// unsigned dial info. A plaintext mesh accepts unsigned dial info
    /// only when there is no already-trusted signed path to protect —
    /// preserving legacy last-writer-wins for an all-unsigned fleet while
    /// a signed path, once learned, can never be downgraded by an
    /// unsigned record.
    fn reconcile_dial_info(
        record: &mut MemberRecord,
        existing: Option<&MemberRecord>,
        enforce_signed: bool,
    ) {
        let signed_ok = match (record.node_pubkey, record.dial_info_sig.as_deref()) {
            (Some(pk), Some(sig)) => {
                let version_ok = match existing {
                    Some(e) => record.dial_info_version >= e.dial_info_version,
                    None => true,
                };
                version_ok
                    && crate::dial_sig::verify_dial_info_hex(
                        &pk,
                        record.dial_info_version,
                        record.relay_url.as_deref(),
                        &record.iroh_direct_addrs,
                        sig,
                    )
            }
            _ => false,
        };
        if signed_ok {
            return;
        }
        let existing_unsigned = match existing {
            Some(e) => e.dial_info_sig.is_none(),
            None => true,
        };
        let accept_unsigned =
            !enforce_signed && record.dial_info_sig.is_none() && existing_unsigned;
        if accept_unsigned {
            return;
        }
        // Reject: pin to the dial info we already trust, or clear on first
        // sight. The member is still added/updated — it just isn't
        // dialable-by-key until a verified record arrives.
        match existing {
            Some(e) => {
                record.relay_url = e.relay_url.clone();
                record.iroh_direct_addrs = e.iroh_direct_addrs.clone();
                record.dial_info_version = e.dial_info_version;
                record.dial_info_sig = e.dial_info_sig.clone();
            }
            None => {
                record.relay_url = None;
                record.iroh_direct_addrs = Vec::new();
                record.dial_info_version = 0;
                record.dial_info_sig = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_status_serde_roundtrip() {
        for status in [
            NodeStatus::Online,
            NodeStatus::Busy,
            NodeStatus::Away,
            NodeStatus::Offline,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: NodeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn peer_trust_level_serde_roundtrip() {
        for level in [
            PeerTrustLevel::ModelAndKnowledgeSharing,
            PeerTrustLevel::Full,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let back: PeerTrustLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, back);
        }
    }

    #[test]
    fn mesh_peering_serializes_to_json() {
        let peering = MeshPeering {
            peer_mesh_id: MeshId::from_u128(42),
            peer_mesh_name: "Mission District Co-op".into(),
            trust_level: PeerTrustLevel::Full,
            established_at: 1700000000,
            contact_nodes: vec!["10.0.1.50:9742".parse().unwrap()],
        };
        let json = serde_json::to_string(&peering).unwrap();
        let back: MeshPeering = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peer_mesh_name, "Mission District Co-op");
        assert_eq!(back.trust_level, PeerTrustLevel::Full);
    }

    // ── Mesh::merge_from ──────────────────────────────────────

    use crate::capabilities::{AvailableResources, HardwareProfile};

    fn member(id: NodeId, name: &str, last_seen: u64) -> MemberRecord {
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id,
            name: name.into(),
            invited_by: id,
            joined_at: 0,
            last_seen,
            status: NodeStatus::Online,
            capabilities: NodeCapabilities {
                hardware: HardwareProfile {
                    gpus: vec![],
                    system_ram_gb: 0,
                    cpu_cores: 0,
                    total_storage_gb: 0,
                    free_storage_gb: 0,
                    network_bandwidth_mbps: None,
                },
                available: AvailableResources::default(),
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: last_seen,
                inference_availability: 1.0,
                inference_capable: false,
                loaded_models: vec![],

                embed_model: None,
                benchmark: None,
                current_in_flight: None,
                anchor: None,
            },
            addresses: vec![],
        }
    }

    fn mesh_with(members: Vec<MemberRecord>, id: MeshId, hash: [u8; 32]) -> Mesh {
        let mut map = HashMap::new();
        for m in members {
            map.insert(m.node_id, m);
        }
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id,
            name: "test".into(),
            invite_key_hash: hash,
            invite_version: 0,
            require_encryption: false,
            members: map,
            peers: vec![],
        }
    }

    #[test]
    fn is_dialable_accepts_ip_or_iroh_paths() {
        let node = NodeId::from_u128(7);

        // No IP, no iroh → not dialable (the record the filters drop).
        let bare = member(node, "bare", 1);
        assert!(!bare.is_dialable());

        // IP path.
        let mut ip = member(node, "ip", 1);
        ip.addresses = vec!["10.0.0.5:9742".parse().unwrap()];
        assert!(ip.is_dialable());

        // A pubkey ALONE is not a path — need a relay or a direct addr.
        let mut key_only = member(node, "key", 1);
        key_only.node_pubkey = Some(NodePubkey([9u8; 32]));
        assert!(!key_only.is_dialable());

        // pubkey + relay (the off-LAN no-VPN case).
        let mut relayed = key_only.clone();
        relayed.relay_url = Some("https://relay.example./".into());
        assert!(relayed.is_dialable());

        // pubkey + direct addr (the LAN-without-internet iroh case).
        let mut direct = key_only.clone();
        direct.iroh_direct_addrs = vec!["127.0.0.1:5000".parse().unwrap()];
        assert!(direct.is_dialable());

        // A relay/direct WITHOUT a pubkey is not dialable by key.
        let mut no_key = member(node, "nokey", 1);
        no_key.relay_url = Some("https://relay.example./".into());
        assert!(!no_key.is_dialable());
    }

    #[test]
    fn iroh_dial_fields_serde_back_compat_and_mutable_lww() {
        // Back-compat: a record with no iroh dial info serializes
        // WITHOUT the keys (skip_serializing_if), so a pre-W2 node sees
        // identical bytes — and such a payload reads back as None/empty.
        let bare = member(NodeId::from_u128(1), "a", 1);
        let json = serde_json::to_value(&bare).unwrap();
        assert!(
            json.get("relay_url").is_none(),
            "relay_url omitted when None"
        );
        assert!(
            json.get("iroh_direct_addrs").is_none(),
            "iroh_direct_addrs omitted when empty"
        );
        let back: MemberRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back.relay_url, None);
        assert!(back.iroh_direct_addrs.is_empty());

        // Round-trips with values.
        let mut keyed = member(NodeId::from_u128(3), "c", 5);
        keyed.relay_url = Some("https://relay.example./".into());
        keyed.iroh_direct_addrs = vec!["127.0.0.1:5000".parse().unwrap()];
        let rt: MemberRecord =
            serde_json::from_value(serde_json::to_value(&keyed).unwrap()).unwrap();
        assert_eq!(rt.relay_url.as_deref(), Some("https://relay.example./"));
        assert_eq!(rt.iroh_direct_addrs, keyed.iroh_direct_addrs);

        // The load-bearing distinction: relay_url/iroh_direct_addrs are
        // MUTABLE reachability and ride normal last-seen LWW — a newer
        // record replaces them (even to None when a node turns iroh
        // off). node_pubkey is IMMUTABLE identity and is
        // anti-downgrade-preserved when a relayer drops it.
        let mesh_id = MeshId::from_u128(9);
        let hash = [3u8; 32];
        let mut have = member(NodeId::from_u128(7), "p", 1);
        have.relay_url = Some("https://old.relay./".into());
        have.node_pubkey = Some(NodePubkey([0xAB; 32]));
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), have],
            mesh_id,
            hash,
        );

        let mut newer = member(NodeId::from_u128(7), "p", 2); // higher last_seen
        newer.relay_url = Some("https://new.relay./".into());
        newer.node_pubkey = None; // relayed by a peer that didn't carry the key
        let incoming = mesh_with(vec![newer], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&NodeId::from_u128(7)).unwrap();
        assert_eq!(
            merged.relay_url.as_deref(),
            Some("https://new.relay./"),
            "relay_url is mutable LWW — the newer record wins"
        );
        assert_eq!(
            merged.node_pubkey,
            Some(NodePubkey([0xAB; 32])),
            "node_pubkey anti-downgrade still preserves the known identity key"
        );
    }

    // ── WS-D: signed dial-info anti-downgrade ─────────────────

    fn signed_member(
        id: NodeId,
        last_seen: u64,
        key: &ed25519_dalek::SigningKey,
        version: u64,
        relay: Option<&str>,
        addrs: &[std::net::SocketAddr],
    ) -> MemberRecord {
        use ed25519_dalek::Signer;
        let pk = NodePubkey(key.verifying_key().to_bytes());
        let mut m = member(id, "signed", last_seen);
        m.node_pubkey = Some(pk);
        m.relay_url = relay.map(|s| s.to_string());
        m.iroh_direct_addrs = addrs.to_vec();
        m.dial_info_version = version;
        let sig = key.sign(&crate::dial_sig::dial_info_message(
            &pk, version, relay, addrs,
        ));
        m.dial_info_sig = Some(hex::encode(sig.to_bytes()));
        m
    }

    #[test]
    fn dial_info_strip_attack_is_rejected_and_pinned() {
        let mesh_id = MeshId::from_u128(11);
        let hash = [4u8; 32];
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let b = NodeId::from_u128(7);
        let addrs: Vec<std::net::SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];

        // Local holds B's SIGNED dial info at version 3.
        let trusted = signed_member(b, 100, &key, 3, Some("https://relay.example./"), &addrs);
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), trusted],
            mesh_id,
            hash,
        );

        // Attacker (past the join-key gate) publishes a forged-NEWER record
        // (higher last_seen) with the dial info STRIPPED and unsigned.
        let mut stripped = member(b, "signed", 200);
        stripped.node_pubkey = Some(NodePubkey(key.verifying_key().to_bytes()));
        let incoming = mesh_with(vec![stripped], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(
            merged.relay_url.as_deref(),
            Some("https://relay.example./"),
            "stripped dial info rejected — pinned to the signed value"
        );
        assert_eq!(merged.iroh_direct_addrs, addrs);
        assert_eq!(merged.dial_info_version, 3);
        assert_eq!(
            merged.last_seen, 200,
            "non-security fields (last_seen) still take the LWW win"
        );
    }

    #[test]
    fn dial_info_substitution_with_foreign_sig_is_rejected() {
        let mesh_id = MeshId::from_u128(12);
        let hash = [4u8; 32];
        let owner = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let attacker = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let b = NodeId::from_u128(7);
        let real: Vec<std::net::SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];

        let trusted = signed_member(b, 100, &owner, 3, Some("https://relay.example./"), &real);
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), trusted],
            mesh_id,
            hash,
        );

        // Attacker substitutes their OWN addrs, signed with the ATTACKER's
        // key (version bumped) — but B's preserved pubkey won't verify it.
        let evil: Vec<std::net::SocketAddr> = vec!["10.0.0.99:9742".parse().unwrap()];
        let mut sub = signed_member(b, 200, &attacker, 9, Some("https://evil./"), &evil);
        // Carry B's real pubkey so preserved_pubkey resolves to B (the sig
        // is the attacker's, so verification under B's key must fail).
        sub.node_pubkey = Some(NodePubkey(owner.verifying_key().to_bytes()));
        let incoming = mesh_with(vec![sub], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(
            merged.iroh_direct_addrs, real,
            "attacker-signed substitution rejected — pinned to B's real addrs"
        );
        assert_eq!(merged.dial_info_version, 3);
    }

    #[test]
    fn replayed_older_signed_dial_info_loses_version_check() {
        let mesh_id = MeshId::from_u128(13);
        let hash = [4u8; 32];
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let b = NodeId::from_u128(7);
        let a5: Vec<std::net::SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];

        let v5 = signed_member(b, 100, &key, 5, Some("relay-v5"), &a5);
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), v5],
            mesh_id,
            hash,
        );

        // A genuine OLDER signed record (version 2, valid sig) replayed with
        // a forged-newer last_seen must not roll the dial info back.
        let a2: Vec<std::net::SocketAddr> = vec!["10.0.0.2:9742".parse().unwrap()];
        let older = signed_member(b, 999, &key, 2, Some("relay-v2"), &a2);
        let incoming = mesh_with(vec![older], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(
            merged.dial_info_version, 5,
            "version rollback rejected — kept version 5"
        );
        assert_eq!(merged.relay_url.as_deref(), Some("relay-v5"));
        assert_eq!(merged.last_seen, 999, "liveness still advances");
    }

    #[test]
    fn encrypted_mesh_rejects_unsigned_dial_info() {
        let mesh_id = MeshId::from_u128(14);
        let hash = [4u8; 32];
        let b = NodeId::from_u128(7);

        let mut local = mesh_with(
            vec![
                member(NodeId::from_u128(1), "self", 100),
                member(b, "b", 100),
            ],
            mesh_id,
            hash,
        );
        local.require_encryption = true; // encrypted mesh enforces signed dial info

        // Newer record with UNSIGNED attacker dial info.
        let mut unsigned = member(b, "b", 200);
        unsigned.relay_url = Some("https://attacker./".into());
        let incoming = mesh_with(vec![unsigned], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert!(
            merged.relay_url.is_none(),
            "encrypted mesh must reject unsigned dial info (cleared), got {:?}",
            merged.relay_url
        );
    }

    #[test]
    fn merge_adds_missing_members() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(vec![member(a, "A", 10)], mesh_id, hash);
        let remote = mesh_with(vec![member(a, "A", 10), member(b, "B", 20)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 1);
        assert_eq!(report.updated, 0);
        assert!(!report.rejected);
        assert_eq!(report.observed, vec![b], "added member is observed");
        assert_eq!(local.members.len(), 2);
        assert!(local.members.contains_key(&b));
    }

    #[test]
    fn merge_updates_stale_records_via_last_seen() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), member(b, "B-stale", 5)],
            mesh_id,
            hash,
        );
        let remote = mesh_with(vec![member(b, "B-fresh", 50)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 1);
        assert_eq!(report.observed, vec![b], "LWW-updated member is observed");
        assert_eq!(local.members.get(&b).unwrap().name, "B-fresh");
        assert_eq!(local.members.get(&b).unwrap().last_seen, 50);
    }

    #[test]
    fn merge_keeps_newer_local_over_older_incoming() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), member(b, "B-fresh", 100)],
            mesh_id,
            hash,
        );
        let remote = mesh_with(vec![member(b, "B-stale", 20)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert!(report.observed.is_empty(), "no advance => nothing observed");
        assert_eq!(local.members.get(&b).unwrap().name, "B-fresh");
    }

    #[test]
    fn tombstone_is_not_resurrected_by_stale_live_record() {
        // The immortal-ghost fix: a tombstoned member must out-compete a stale
        // live copy that a lagging peer still gossips. B was removed at t=50;
        // a peer relays B's old live record (last_seen=20 < 50) → must NOT win.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let b_tombstone = {
            let mut m = member(b, "B", 10);
            m.removed_at = Some(50);
            m
        };
        let mut local = mesh_with(vec![member(a, "A", 10), b_tombstone], mesh_id, hash);
        let remote = mesh_with(vec![member(b, "B-live-stale", 20)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.updated, 0, "stale live record must not resurrect");
        assert!(
            report.observed.is_empty(),
            "a non-event must not stamp liveness"
        );
        let merged = local.members.get(&b).unwrap();
        assert_eq!(merged.removed_at, Some(50), "B stays tombstoned");
        assert!(!merged.is_active());
    }

    #[test]
    fn genuine_rejoin_resurrects_a_tombstone() {
        // A live record whose last_seen post-dates the removal IS a real
        // rejoin — event-time LWW lets it win and clear the tombstone.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let b_tombstone = {
            let mut m = member(b, "B", 10);
            m.removed_at = Some(50);
            m
        };
        let mut local = mesh_with(vec![member(a, "A", 10), b_tombstone], mesh_id, hash);
        let remote = mesh_with(vec![member(b, "B-rejoined", 100)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(
            report.updated, 1,
            "rejoin (last_seen 100 > removed_at 50) wins"
        );
        let merged = local.members.get(&b).unwrap();
        assert!(merged.is_active(), "rejoin clears the tombstone");
        assert_eq!(merged.last_seen, 100);
    }

    #[test]
    fn merge_preserves_pubkey_when_old_peer_relays_record_without_it() {
        // The mixed-version mesh scenario: B has an identity key we
        // already know. An OLD-build peer relays B's record with a
        // newer last_seen but no node_pubkey field (its build
        // predates the field, so it gossips None). The LWW win must
        // NOT strip the key we know.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), {
                let mut m = member(b, "B", 5);
                m.node_pubkey = Some(NodePubkey([0xAB; 32]));
                m
            }],
            mesh_id,
            hash,
        );
        let remote = mesh_with(vec![member(b, "B", 50)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.updated, 1);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(merged.last_seen, 50, "rest of the newer record adopted");
        assert_eq!(
            merged.node_pubkey,
            Some(NodePubkey([0xAB; 32])),
            "locally-known pubkey survives a None-bearing LWW win"
        );
    }

    #[test]
    fn merge_adopts_pubkey_from_newer_record_that_carries_one() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(vec![member(a, "A", 10), member(b, "B", 5)], mesh_id, hash);
        let remote = mesh_with(
            vec![{
                let mut m = member(b, "B", 50);
                m.node_pubkey = Some(NodePubkey([0xCD; 32]));
                m
            }],
            mesh_id,
            hash,
        );

        local.merge_from(a, &remote);
        assert_eq!(
            local.members.get(&b).unwrap().node_pubkey,
            Some(NodePubkey([0xCD; 32]))
        );
    }

    #[test]
    fn member_record_wire_compat_with_pre_identity_builds() {
        // New → old: a record without a key serializes WITHOUT the
        // node_pubkey field, byte-identical to the pre-identity wire.
        let m = member(NodeId::from_u128(1), "A", 10);
        let json = serde_json::to_value(&m).unwrap();
        assert!(
            json.get("node_pubkey").is_none(),
            "None must not appear on the wire"
        );

        // Old → new: pre-identity JSON (no node_pubkey key) parses
        // with node_pubkey = None.
        let back: MemberRecord = serde_json::from_value(json).unwrap();
        assert!(back.node_pubkey.is_none());

        // Round-trip with a key present.
        let mut keyed = member(NodeId::from_u128(2), "B", 10);
        keyed.node_pubkey = Some(NodePubkey([9u8; 32]));
        let json = serde_json::to_string(&keyed).unwrap();
        let back: MemberRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.node_pubkey, Some(NodePubkey([9u8; 32])));
    }

    #[test]
    fn merge_never_overwrites_self_even_if_incoming_is_newer() {
        // A peer could ship us an old view of ourselves that has a
        // stale address list, wrong name, or Offline status. We're
        // authoritative for self — ignore it.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let me = NodeId::from_u128(100);

        let mut local = mesh_with(vec![member(me, "Me-Real", 10)], mesh_id, hash);
        let remote = mesh_with(
            vec![{
                let mut m = member(me, "Me-Imposter", 9999);
                m.status = NodeStatus::Offline;
                m
            }],
            mesh_id,
            hash,
        );

        let report = local.merge_from(me, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(local.members.get(&me).unwrap().name, "Me-Real");
        assert_eq!(local.members.get(&me).unwrap().last_seen, 10);
        assert_eq!(local.members.get(&me).unwrap().status, NodeStatus::Online);
    }

    #[test]
    fn merge_rejects_different_mesh_id() {
        let me = NodeId::from_u128(1);
        let hash = [7u8; 32];
        let mut local = mesh_with(vec![member(me, "M", 10)], MeshId::from_u128(1), hash);
        let remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            MeshId::from_u128(2), // different!
            hash,
        );

        let report = local.merge_from(me, &remote);
        assert!(report.rejected);
        assert_eq!(report.added, 0);
        assert_eq!(local.members.len(), 1, "no mutation on reject");
    }

    #[test]
    fn merge_rejects_mismatched_invite_key_hash() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        let remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            mesh_id,
            [9u8; 32], // different hash!
        );

        let report = local.merge_from(me, &remote);
        assert!(report.rejected);
        assert_eq!(local.members.len(), 1);
    }

    /// THE point of the credential split. Two members whose invite hashes have
    /// diverged — one rotated, the other has not gossiped it yet — must still
    /// authorize each other, because gossip auth reads `mesh_secret`.
    ///
    /// Before the split this exact state was a symmetric partition: each side
    /// rejected the other and both reported `[1/N online]`.
    #[test]
    fn a_rotated_invite_does_not_partition_the_mesh() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let secret = [3u8; 32];

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = secret;
        let mut remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            mesh_id,
            [9u8; 32], // rotated out from under us
        );
        remote.mesh_secret = secret;

        let report = local.merge_from(me, &remote);
        assert!(
            !report.rejected,
            "same mesh_secret means same mesh, whatever the invite hash says"
        );
        assert_eq!(local.members.len(), 2);
    }

    /// A different mesh that happens to share an invite hash is still refused.
    #[test]
    fn a_shared_invite_hash_is_not_enough_once_secrets_are_set() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = [3u8; 32];
        let mut remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            mesh_id,
            [7u8; 32], // same invite hash...
        );
        remote.mesh_secret = [4u8; 32]; // ...different mesh

        assert!(local.merge_from(me, &remote).rejected);
        assert_eq!(local.members.len(), 1);
    }

    /// The compat arm: a pre-split peer sends a zeroed secret and must still be
    /// admitted on the legacy predicate, or upgrading the fleet partitions it.
    #[test]
    fn a_pre_split_peer_authorizes_on_the_legacy_predicate() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = [3u8; 32];
        let remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            mesh_id,
            [7u8; 32],
        ); // mesh_secret defaults to zeroed

        let report = local.merge_from(me, &remote);
        assert!(!report.rejected, "a peer mid-upgrade must not be dropped");
        assert_eq!(local.members.len(), 2);
        assert!(
            report.peer_pre_split,
            "the merge must REPORT that this peer is pre-split — it is the \
             only moment that fact is visible, and rotate_invite depends on it"
        );
    }

    /// The signal `rotate_invite`'s guard stands on. Reported per merge,
    /// because it describes the SENDER's build and nothing in any member
    /// record carries it.
    #[test]
    fn a_post_split_peer_is_reported_as_post_split() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let secret = [3u8; 32];
        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = secret;
        let mut remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            mesh_id,
            [7u8; 32],
        );
        remote.mesh_secret = secret;

        let report = local.merge_from(me, &remote);
        assert!(!report.rejected);
        assert!(
            !report.peer_pre_split,
            "a peer that sent a mesh_secret is post-split; flagging it would \
             block invite rotation on a fully-upgraded fleet forever"
        );
    }

    /// A REFUSED merge says nothing about the sender's build, so the flag must
    /// not be read as "post-split" on that path. Guards that treat an absent
    /// answer as a positive one are the §18.3 substitution.
    #[test]
    fn a_rejected_merge_reports_no_split_generation() {
        let me = NodeId::from_u128(1);
        let mut local = mesh_with(vec![member(me, "M", 10)], MeshId::from_u128(1), [7u8; 32]);
        local.mesh_secret = [3u8; 32];
        let mut remote = mesh_with(vec![], MeshId::from_u128(2), [7u8; 32]);
        remote.mesh_secret = [4u8; 32];

        let report = local.merge_from(me, &remote);
        assert!(report.rejected);
        assert!(!report.peer_pre_split);
    }

    fn proof_mesh(secret: [u8; 32]) -> Mesh {
        proof_mesh_with_id(secret, MeshId::from_u128(77))
    }

    /// The proof binds to `mesh_id`, so a test that verifies across two Mesh
    /// values must give them the SAME id or the proof legitimately fails.
    fn proof_mesh_with_id(secret: [u8; 32], id: MeshId) -> Mesh {
        let mut m = mesh_with(vec![], id, [7u8; 32]);
        m.mesh_secret = secret;
        m
    }

    /// The happy path: a holder of the secret proves it without sending it.
    #[test]
    fn a_proof_from_the_same_secret_verifies() {
        let sender = NodeId::from_u128(5);
        let a = proof_mesh([9u8; 32]);
        let b = proof_mesh([9u8; 32]);
        let now = 1_000_000;
        assert!(b.verify_mesh_proof(&a.mesh_proof(sender, now).unwrap(), sender, now));
    }

    /// The point of the exercise: a different secret cannot forge one.
    #[test]
    fn a_proof_from_a_different_secret_is_refused() {
        let sender = NodeId::from_u128(5);
        let a = proof_mesh([9u8; 32]);
        let b = proof_mesh([8u8; 32]);
        let now = 1_000_000;
        assert!(!b.verify_mesh_proof(&a.mesh_proof(sender, now).unwrap(), sender, now));
    }

    /// Bound to the sender: an eavesdropper who captures a member's proof
    /// cannot present it as themselves.
    #[test]
    fn a_proof_cannot_be_replayed_by_a_different_peer() {
        let real = NodeId::from_u128(5);
        let impostor = NodeId::from_u128(6);
        let a = proof_mesh([9u8; 32]);
        let b = proof_mesh([9u8; 32]);
        let now = 1_000_000;
        let stolen = a.mesh_proof(real, now).unwrap();
        assert!(b.verify_mesh_proof(&stolen, real, now));
        assert!(
            !b.verify_mesh_proof(&stolen, impostor, now),
            "a captured proof must not authorize a different node — otherwise it \
             is a bearer token for anyone who can sniff one packet"
        );
    }

    /// Bound to a time window: a captured proof goes stale rather than being
    /// a credential forever. Two windows of slack, never more.
    #[test]
    fn a_proof_expires_after_two_windows() {
        let sender = NodeId::from_u128(5);
        let a = proof_mesh([9u8; 32]);
        let b = proof_mesh([9u8; 32]);
        let now = 1_000_000;
        let proof = a.mesh_proof(sender, now).unwrap();

        assert!(b.verify_mesh_proof(&proof, sender, now));
        assert!(
            b.verify_mesh_proof(&proof, sender, now + PROOF_WINDOW_SECS),
            "one window of skew must be tolerated, or a round that straddles a \
             boundary fails for no reason"
        );
        assert!(
            !b.verify_mesh_proof(&proof, sender, now + PROOF_WINDOW_SECS * 3),
            "the replay horizon must be bounded"
        );
    }

    /// A node with no secret must refuse rather than key every proof
    /// identically across every un-migrated mesh.
    #[test]
    fn a_node_without_a_secret_verifies_nothing() {
        let sender = NodeId::from_u128(5);
        let holder = proof_mesh([9u8; 32]);
        let unset = proof_mesh([0u8; 32]);
        let now = 1_000_000;
        assert!(!unset.verify_mesh_proof(
            &holder.mesh_proof(sender, now).unwrap_or_default(),
            sender,
            now
        ));
    }

    /// The upgraded case: a peer proves possession and sends NO raw secret at
    /// all. This is what takes the credential off the wire.
    #[test]
    fn a_valid_proof_authorizes_without_any_raw_secret_on_the_wire() {
        let me = NodeId::from_u128(1);
        let sender = NodeId::from_u128(2);
        let mesh_id = MeshId::from_u128(1);
        let now = 1_000_000;

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = [9u8; 32];

        // The peer's payload carries a ZEROED secret — it sent nothing.
        let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
        remote.mesh_secret = [0u8; 32];

        let holder = proof_mesh_with_id([9u8; 32], mesh_id);
        let auth = GossipAuth {
            sender: Some(sender),
            proof: holder.mesh_proof(sender, now),
            now_secs: now,
        };

        let report = local.merge_from_authenticated(me, &remote, &auth);
        assert!(
            !report.rejected,
            "a proof of possession must authorize; otherwise the secret can \
             never leave the wire"
        );
        assert_eq!(local.members.len(), 2);
    }

    /// The mis-attribution this arm enum exists to prevent.
    ///
    /// Once the outbound path stops sending the raw secret to a CONFIRMED
    /// post-split peer, a zeroed `mesh_secret` on the wire stops meaning "old
    /// build" and starts meaning "upgraded peer, deliberately withholding".
    /// Reading the payload alone flips two upgraded nodes to pre-split, which
    /// (a) blocks `rotate_invite` on both sides forever and (b) makes each
    /// resume sending the credential it had just stopped sending. The proof
    /// settles it: only a holder of the current secret can produce one.
    #[test]
    fn a_peer_that_proves_possession_is_never_reported_pre_split() {
        let me = NodeId::from_u128(1);
        let sender = NodeId::from_u128(2);
        let mesh_id = MeshId::from_u128(1);
        let now = 1_000_000;

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = [9u8; 32];

        // An UPGRADED peer that has confirmed us and now withholds the secret.
        let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
        remote.mesh_secret = [0u8; 32];

        let holder = proof_mesh_with_id([9u8; 32], mesh_id);
        let auth = GossipAuth {
            sender: Some(sender),
            proof: holder.mesh_proof(sender, now),
            now_secs: now,
        };

        let report = local.merge_from_authenticated(me, &remote, &auth);
        assert_eq!(report.auth_arm, GossipAuthArm::Proof);
        assert!(
            !report.peer_pre_split,
            "a peer that PROVED possession of the current secret is post-split \
             by definition; calling it pre-split blocks rotation on both sides \
             and puts the credential back on the wire"
        );
    }

    /// The compat half, still intact: no proof and no secret really is a
    /// pre-split peer, and it must still be admitted.
    #[test]
    fn a_peer_with_neither_proof_nor_secret_is_still_pre_split() {
        let me = NodeId::from_u128(1);
        let sender = NodeId::from_u128(2);
        let mesh_id = MeshId::from_u128(1);

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = [9u8; 32];
        let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
        remote.mesh_secret = [0u8; 32];

        let report = local.merge_from_authenticated(me, &remote, &GossipAuth::none());
        assert!(!report.rejected, "the compat arm must still admit");
        assert_eq!(report.auth_arm, GossipAuthArm::Legacy);
        assert!(report.peer_pre_split);
    }

    /// Two post-split-but-pre-proof nodes: raw secrets match, and that arm is
    /// the one the reply may still answer with the credential on.
    #[test]
    fn matching_raw_secrets_report_the_raw_secret_arm() {
        let me = NodeId::from_u128(1);
        let sender = NodeId::from_u128(2);
        let mesh_id = MeshId::from_u128(1);

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = [9u8; 32];
        let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
        remote.mesh_secret = [9u8; 32];

        let report = local.merge_from_authenticated(me, &remote, &GossipAuth::none());
        assert_eq!(report.auth_arm, GossipAuthArm::RawSecret);
        assert!(!report.peer_pre_split);
    }

    /// Downgrade prevention. An OFFERED proof that does not verify is a
    /// failure, not an invitation to try the weaker predicate — otherwise an
    /// attacker sends junk and gets handed the legacy `invite_key_hash` arm.
    #[test]
    fn a_bad_proof_is_refused_and_does_not_fall_back_to_the_legacy_arm() {
        let me = NodeId::from_u128(1);
        let sender = NodeId::from_u128(2);
        let mesh_id = MeshId::from_u128(1);
        let now = 1_000_000;

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = [9u8; 32];

        // The attacker knows the invite hash — which rides every payload — and
        // would be admitted by the legacy arm if the bad proof fell through.
        let mut remote = mesh_with(vec![member(sender, "X", 100)], mesh_id, [7u8; 32]);
        remote.mesh_secret = [0u8; 32];

        let auth = GossipAuth {
            sender: Some(sender),
            proof: Some("not a real proof".into()),
            now_secs: now,
        };

        assert!(
            local.merge_from_authenticated(me, &remote, &auth).rejected,
            "a failed proof must REFUSE, not downgrade to invite_key_hash"
        );
        assert_eq!(local.members.len(), 1, "a refused merge must not mutate");
    }

    /// A rotation must TRAVEL. Before `invite_version` existed, `merge_from`
    /// merged only `require_encryption` and `members`, so a founder's rotate
    /// was node-local: every other member kept admitting joiners on the
    /// revoked key forever. This is the test that would have caught that.
    #[test]
    fn a_rotated_invite_propagates_to_a_peer() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let secret = [3u8; 32];

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = secret;

        // The founder rotates, then gossips at us.
        let mut founder = mesh_with(
            vec![member(NodeId::from_u128(2), "F", 100)],
            mesh_id,
            [7u8; 32],
        );
        founder.mesh_secret = secret;
        founder.rotate_invite_key([9u8; 32], Some(4242));

        assert_eq!(
            founder.invite_version, 1,
            "rotating must advance the version"
        );
        assert!(!local.merge_from(me, &founder).rejected);
        assert_eq!(
            local.invite_key_hash, [9u8; 32],
            "the peer must adopt the rotated invite, or it keeps admitting \
             joiners on the revoked key"
        );
        assert_eq!(
            local.invite_expires_at,
            Some(4242),
            "the TTL moves WITH the hash — a new key under a stale expiry is \
             worse than either endpoint"
        );
        assert_eq!(local.invite_version, 1);
    }

    /// Anti-rollback, same rule as `dial_info_version`: a replayed older
    /// payload never wins. Without this a stale peer re-arms a revoked invite.
    #[test]
    fn an_older_invite_never_overwrites_a_newer_one() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let secret = [3u8; 32];

        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        local.mesh_secret = secret;
        local.rotate_invite_key([9u8; 32], None); // version 1

        let mut stale = mesh_with(
            vec![member(NodeId::from_u128(2), "S", 100)],
            mesh_id,
            [7u8; 32],
        );
        stale.mesh_secret = secret; // version 0, old hash

        assert!(!local.merge_from(me, &stale).rejected);
        assert_eq!(
            local.invite_key_hash, [9u8; 32],
            "a version-0 peer must not roll our rotation back"
        );
        assert_eq!(local.invite_version, 1);
    }

    /// Two nodes rotating in the same round land on the same version with
    /// different hashes. "Keep ours" is not a decision — it is each node
    /// keeping a different answer forever, i.e. two admission regimes in one
    /// mesh. The hash comparison is a total order every node computes
    /// identically, so they converge.
    #[test]
    fn a_simultaneous_rotation_converges_rather_than_splitting() {
        let mesh_id = MeshId::from_u128(1);
        let secret = [3u8; 32];
        let a_id = NodeId::from_u128(1);
        let b_id = NodeId::from_u128(2);

        let mut a = mesh_with(vec![member(a_id, "A", 10)], mesh_id, [7u8; 32]);
        a.mesh_secret = secret;
        a.rotate_invite_key([1u8; 32], None); // version 1, LOWER hash

        let mut b = mesh_with(vec![member(b_id, "B", 10)], mesh_id, [7u8; 32]);
        b.mesh_secret = secret;
        b.rotate_invite_key([2u8; 32], None); // version 1, HIGHER hash

        // Gossip both directions.
        let (a_snapshot, b_snapshot) = (a.clone(), b.clone());
        a.merge_from(a_id, &b_snapshot);
        b.merge_from(b_id, &a_snapshot);

        assert_eq!(
            a.invite_key_hash, b.invite_key_hash,
            "both sides must land on ONE invite; a split here means two \
             admission regimes inside one mesh"
        );
        assert_eq!(
            a.invite_key_hash, [2u8; 32],
            "the higher hash is the tie-break"
        );
    }

    /// ARCH §7.1: the invariant is structural, not remembered. `rotate_invite_key`
    /// cannot name `mesh_secret`, and this pins that it stays that way.
    #[test]
    fn rotating_the_invite_never_touches_the_mesh_secret() {
        let mesh_id = MeshId::from_u128(1);
        let mut mesh = mesh_with(vec![], mesh_id, [7u8; 32]);
        mesh.mesh_secret = [3u8; 32];

        mesh.rotate_invite_key([8u8; 32], Some(1234));

        assert_eq!(mesh.invite_key_hash, [8u8; 32]);
        assert_eq!(mesh.invite_expires_at, Some(1234));
        assert_eq!(
            mesh.mesh_secret, [3u8; 32],
            "rotation must be structurally incapable of re-keying gossip"
        );
    }

    #[test]
    fn an_invite_with_no_expiry_never_lapses() {
        let mesh_id = MeshId::from_u128(1);
        let mut mesh = mesh_with(vec![], mesh_id, [7u8; 32]);
        assert!(!mesh.invite_expired_at(u64::MAX));

        mesh.invite_expires_at = Some(100);
        assert!(!mesh.invite_expired_at(99));
        assert!(mesh.invite_expired_at(100), "expiry is inclusive");
        assert!(mesh.invite_expired_at(101));
    }
}
