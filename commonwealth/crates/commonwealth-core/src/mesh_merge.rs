// SPDX-License-Identifier: AGPL-3.0-or-later
//! What one gossip merge round did, and the vocabulary that says so.
//!
//! Split out of `mesh.rs` beside `mesh_identity`, on the same seam: that
//! module owns the identity question, this one owns the ACCOUNTING of a
//! round. Merge is about time, identity is about who, and this is about what
//! happened.
//!
//! # Why an outcome type rather than counters
//!
//! `merge_from_authenticated` runs FOUR algebras at three different scopes —
//! a monotone join on a mesh-level field (`require_encryption`, stricter
//! wins), last-writer-wins between two records, anti-downgrade on a field
//! *within* the winning record (`node_pubkey`), and a refusal that vetoes a
//! whole record (`Mesh::alias_clash`). They are not four spellings of one
//! operation and a single `merge` trait would flatten three altitudes into
//! one.
//!
//! What they DID share was a defect: nothing named them, so the set was not
//! enumerable and a missing case was invisible. The per-record decision now
//! lands as a [`MemberOutcome`], which one crate-private fold on
//! [`MergeReport`] turns into a number — the ONE place that happens, against
//! five scattered increments and two duplicated `warn!` blocks before. A new
//! path cannot compile without deciding its outcome, and a new outcome cannot
//! compile without the fold handling it.
//!
//! **The fields are private, and that is the half that makes the rest true.**
//! An exhaustive fold stops a new OUTCOME going uncounted; it does nothing
//! about a new merge path that writes `report.added += 1` and skips the fold
//! entirely. Both doors are now shut by the compiler, and both were watched
//! shutting: a fifth variant the fold does not handle fails with E0004, and a
//! direct tally from the round loop fails with E0616. Reads go through
//! accessors on [`MergeReport`]; the only writers are its crate-private
//! `record` fold and its two constructors.
//!
//! Two properties the merge loop needs, which is why the variants are shaped
//! this way:
//!
//! * **A refusal must abandon a record and let the round finish.** One
//!   poisoned row must not cost a whole gossip cycle, so
//!   [`MemberOutcome::Refused`] is a match arm the fold counts, never an
//!   early return from the round.
//! * **"This algebra does not apply here" is a named variant, not a
//!   sentinel.** Self-records and already-fresh locals are
//!   [`MemberOutcome::NotApplicable`]; the endpoint-key guard is
//!   deliberately not applied to tombstones or keyless records, and a
//!   not-applicable that shared a spelling with a refusal would make every
//!   count downstream a lie in a direction nobody could predict (ARCH §18.3).

use crate::ids::NodeId;
use crate::mesh::GossipAuthArm;

/// Summary of what a `Mesh::merge_from` call did. Used for tracing
/// ("we learned about 1 new member") and test assertions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Members that were absent locally and got added from `other`.
    added: usize,
    /// Members that existed locally but were replaced by a newer
    /// record from `other` (higher `last_seen`).
    updated: usize,
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
    peer_pre_split: bool,
    /// Which predicate authorized this merge. The caller's reply uses it to
    /// decide whether the raw `mesh_secret` still needs to be on the wire —
    /// see `routes_internal::gossip`.
    auth_arm: GossipAuthArm,
    /// True when the merge was refused outright because `other`
    /// described a different mesh (mismatching `id` or
    /// `invite_key_hash`). When set, nothing was mutated.
    rejected: bool,
    /// Node IDs whose records we just observed advance (added or
    /// LWW-updated) in this merge. The caller stamps these in its local
    /// liveness map (`AppState::observe_peer_contact`) so offline-decay
    /// measures *local observation staleness*, not the peer's own
    /// (possibly clock-skewed) `last_seen`. Empty on a rejected merge.
    observed: Vec<NodeId>,
    /// Records this merge REFUSED because writing them would have left two
    /// ACTIVE members claiming one endpoint key.
    ///
    /// Non-zero means a peer is gossiping a roster we will not adopt, and the
    /// mesh needs an operator: `svrn mesh members --aliased` names the pair
    /// and `svrn mesh forget-member` retires the ghost. It is counted rather
    /// than folded into `rejected` because the rest of the round is still
    /// good — one poisoned row must not cost us a whole gossip cycle.
    aliased_refused: usize,
}

/// What the merge loop decided about ONE member record.
///
/// Exhaustive over the loop's paths on purpose — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberOutcome {
    /// Absent locally, admitted from the peer. `observed` is false for a
    /// tombstone: it still converges mesh-wide, but it is not seen alive.
    Added { observed: bool },
    /// Present locally and replaced by a strictly newer record.
    Updated { observed: bool },
    /// Not written, and the round continues. Counted, never silent.
    Refused(RefusalReason),
    /// No algebra applies to this record. Not a failure and not a zero.
    NotApplicable(SkipReason),
}

/// Why a record was refused admission.
///
/// A refusal is a REFUSAL rather than a resolution, deliberately: endpoint-key
/// last-writer-wins would be self-healing, and would also let a peer past the
/// auth boundary forge a newer record carrying a victim's key and tombstone
/// that victim mesh-wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// Writing this record would leave two ACTIVE members claiming one
    /// endpoint key, splitting one node's liveness across two rows.
    EndpointKeyHeldByActiveMember {
        held_by: NodeId,
        held_by_name: String,
        arm: MergeArm,
    },
}

/// Which arm of the merge produced a refusal. Carried so the operator-facing
/// warning can say what was actually attempted — admitting a new member reads
/// differently from moving a key onto an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeArm {
    /// First sight of this member id.
    FirstSight,
    /// A strictly newer record for a member we already hold.
    LwwUpdate,
}

/// Why no algebra applied to a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Authoritative-for-self: never accept an incoming record about us,
    /// however new. A buggy peer gets corrected on our next push-pull reply.
    AuthoritativeForSelf,
    /// The local record is equal or newer, so last-writer-wins keeps ours.
    LocalRecordNotOlder,
}

impl MergeReport {
    /// A round refused outright — `other` described a different mesh. Nothing
    /// was mutated, so every tally is zero by construction rather than by a
    /// caller remembering to zero it.
    pub fn refused() -> Self {
        Self {
            rejected: true,
            auth_arm: GossipAuthArm::Refused,
            ..Self::default()
        }
    }

    /// A round that will proceed. The tallies start at zero and ONLY
    /// [`Self::record`] moves them — that is what the private fields buy.
    pub(crate) fn for_round(auth_arm: GossipAuthArm, peer_pre_split: bool) -> Self {
        Self {
            auth_arm,
            peer_pre_split,
            ..Self::default()
        }
    }

    /// Members absent locally and admitted from the peer.
    pub fn added(&self) -> usize {
        self.added
    }

    /// Members replaced by a strictly newer record.
    pub fn updated(&self) -> usize {
        self.updated
    }

    /// Records refused because writing them would leave two ACTIVE members
    /// claiming one endpoint key. Non-zero means the mesh needs an operator.
    pub fn aliased_refused(&self) -> usize {
        self.aliased_refused
    }

    /// Node IDs observed to advance in this round. Empty on a refused merge.
    pub fn observed(&self) -> &[NodeId] {
        &self.observed
    }

    /// True when the merge was refused outright and nothing was mutated.
    pub fn rejected(&self) -> bool {
        self.rejected
    }

    /// Whether the peer we merged FROM runs a pre-split build. Meaningful
    /// only when [`Self::rejected`] is false.
    pub fn peer_pre_split(&self) -> bool {
        self.peer_pre_split
    }

    /// Which predicate authorized this merge.
    pub fn auth_arm(&self) -> GossipAuthArm {
        self.auth_arm
    }
}

impl MergeReport {
    /// Fold one member outcome into the round's totals.
    ///
    /// The ONE place a `MemberOutcome` becomes a number (ARCH §10.6). The
    /// match is exhaustive, so a new outcome variant fails to build here
    /// rather than going uncounted — which is the property the scattered
    /// increments could not offer.
    pub(crate) fn record(&mut self, id: NodeId, name: &str, outcome: MemberOutcome) {
        match outcome {
            MemberOutcome::Added { observed } => {
                self.added += 1;
                if observed {
                    self.observed.push(id);
                }
            }
            MemberOutcome::Updated { observed } => {
                self.updated += 1;
                if observed {
                    self.observed.push(id);
                }
            }
            MemberOutcome::Refused(RefusalReason::EndpointKeyHeldByActiveMember {
                held_by,
                held_by_name,
                arm,
            }) => {
                self.aliased_refused += 1;
                let what = match arm {
                    MergeArm::FirstSight => {
                        "REFUSED a new member claiming an endpoint key an active member \
                         already holds — admitting it would split one node's liveness \
                         across two rows; `svrn mesh forget-member` retires whichever \
                         is the ghost"
                    }
                    MergeArm::LwwUpdate => {
                        "REFUSED an LWW update that would move an endpoint key onto a \
                         second active member — the local record stands"
                    }
                };
                tracing::warn!(
                    candidate = ?id,
                    candidate_name = %name,
                    held_by = ?held_by,
                    held_by_name = %held_by_name,
                    "gossip: {what}"
                );
            }
            // Nothing to count. Named so the compiler proves it was considered.
            MemberOutcome::NotApplicable(_) => {}
        }
    }
}
