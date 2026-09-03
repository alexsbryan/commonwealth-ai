// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

pub use crate::mesh_identity::{aliased_endpoint_keys, AliasedEndpointKey, EndpointClaim};

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

pub use crate::mesh_merge::MergeReport;
use crate::mesh_merge::{MemberOutcome, MergeArm, RefusalReason, SkipReason};

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
            return MergeReport::refused();
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

        let mut report =
            MergeReport::for_round(arm, arm != GossipAuthArm::Proof && !other.has_mesh_secret());
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

        for (id, incoming) in &other.members {
            let outcome = self.merge_one_member(*id, incoming, self_node_id, enforce_signed);
            report.record(*id, &incoming.name, outcome);
        }
        report
    }

    /// Decide what one incoming member record does to the local roster, and
    /// apply it. Returns the decision; [`MergeReport::record`] is what turns
    /// it into a number.
    ///
    /// Split out of the round loop so the four paths are four returns rather
    /// than five scattered counter increments and two `continue`s. The
    /// refusal returns like any other outcome, which is what lets one poisoned
    /// row be abandoned without costing the rest of the cycle.
    fn merge_one_member(
        &mut self,
        id: NodeId,
        incoming: &MemberRecord,
        self_node_id: NodeId,
        enforce_signed: bool,
    ) -> MemberOutcome {
        if id == self_node_id {
            // Authoritative-for-self: never accept an incoming record about
            // us, regardless of its `last_seen`. If a buggy peer has a stale
            // view of us, we correct them on our next push-pull reply.
            return MemberOutcome::NotApplicable(SkipReason::AuthoritativeForSelf);
        }
        match self.members.get(&id) {
            None => {
                // First sight: trust dial info only if validly signed, else
                // clear it — a new member can't be poisoned with
                // attacker-supplied reachability on first contact. The member
                // is still added.
                let mut record = incoming.clone();
                Self::reconcile_dial_info(&mut record, None, enforce_signed);
                let active = record.is_active();
                if let Some((held_by, held_by_name)) = self.alias_clash(&record, active) {
                    return MemberOutcome::Refused(RefusalReason::EndpointKeyHeldByActiveMember {
                        held_by,
                        held_by_name,
                        arm: MergeArm::FirstSight,
                    });
                }
                self.members.insert(id, record);
                // A tombstone we have never seen is still added (so it
                // converges mesh-wide), but it is not "observed alive".
                MemberOutcome::Added { observed: active }
            }
            Some(existing) if incoming.event_time() > existing.event_time() => {
                // Anti-downgrade: a newer record relayed by a pre-identity
                // build carries `node_pubkey: None`. Without this
                // preservation, ONE old peer in the gossip path strips every
                // node's pubkey on each LWW win. An identity key never changes
                // within a membership, so keeping the locally-known key while
                // taking the rest of the newer record is always correct.
                let preserved_pubkey = match incoming.node_pubkey {
                    Some(pk) => Some(pk),
                    None => existing.node_pubkey,
                };
                let mut record = incoming.clone();
                record.node_pubkey = preserved_pubkey;
                // Non-security fields take the LWW win, but dial info travels
                // only if signed + fresh; otherwise it is pinned to the value
                // we already trust. So a forged-newer record advances liveness
                // but cannot move a peer's reachability (WS-D).
                Self::reconcile_dial_info(&mut record, Some(existing), enforce_signed);
                let active = record.is_active();
                if let Some((held_by, held_by_name)) = self.alias_clash(&record, active) {
                    return MemberOutcome::Refused(RefusalReason::EndpointKeyHeldByActiveMember {
                        held_by,
                        held_by_name,
                        arm: MergeArm::LwwUpdate,
                    });
                }
                self.members.insert(id, record);
                MemberOutcome::Updated { observed: active }
            }
            // Existing is equal or newer — keep ours.
            Some(_) => MemberOutcome::NotApplicable(SkipReason::LocalRecordNotOlder),
        }
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
pub(crate) mod tests;
