// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh's membership, read as a ring roster — derived here, never
//! accepted over the wire.
//!
//! # Why this is a derivation and not a route
//!
//! `svrn ring`'s own docs state the property this module has to preserve:
//!
//! > **The roster is written from here and is not reachable from the rail at
//! > all.** There is no roster route, so a deployed app cannot add a key to
//! > the ring — including its own. That is a property of the route set rather
//! > than a check, which is the same move the rail itself makes (ARCH §7.1).
//!
//! A ring nobody narrowed — the daemon's own and every app's alike — still
//! needs a roster, and the obvious way to
//! get one is the one that must not exist: a peer handing us the membership it
//! thinks we have. Admitting a roster over the wire hands any peer the ability to
//! admit signers to a ring, which is exactly what §7.1 put out of reach.
//!
//! So the roster is computed from state this node **already holds** — its own
//! `Mesh` — and nothing about that is a route. No new endpoint, no new
//! acceptance, no new trust: a key is in the roster here only because a
//! member row already carried it, and member rows are governed by the join
//! and gossip paths that existed before the rail did.
//!
//! # The bridge is one equality, and it is load-bearing
//!
//! ```text
//!   MemberRecord.node_pubkey   NodePubkey(key.verifying_key().to_bytes())
//!   Op.actor                   hex(key.verifying_key().to_bytes())
//! ```
//!
//! `NodePubkey`'s `Display` is `hex::encode` of those same 32 bytes, and the
//! daemon hands `RingRail` the very key it derives its `node_pubkey` from
//! (`daemon.rs` — one `load_or_generate_node_key`, two consumers). So a
//! member's advertised pubkey and the `actor` on the journal line it signs
//! are the same 64 characters, with no mapping table in between.
//! `a_member_pubkey_and_a_rail_actor_are_the_same_spelling` pins it, and it
//! stops compiling if the two sides ever reach different `ed25519-dalek`s.
//!
//! # Deriving on every read is what makes an `UnknownSigner` gap heal
//!
//! A member on a pre-identity build has `node_pubkey: None`. It cannot be in
//! a roster, so every op it signed is an
//! [`UnknownSigner`](commonwealth_rail::RailGap::UnknownSigner) gap — and in
//! an append-only journal that is normally forever.
//!
//! It is not forever here, and the reason is that this roster is a
//! **parameter of the read** rather than a file. Nothing is dropped when a
//! signer cannot be placed: the op stays on the journal, and the next
//! admission runs against a roster derived from membership as it is *then*.
//! The moment that node's gossip round stamps its pubkey, its whole history
//! admits retroactively under the same actor it always signed with, because
//! the signing key is `load_or_generate` on disk and does not change when the
//! advertisement does.
//!
//! Writing this roster to `roster.json` would break exactly that, which is
//! why [`MeshRoster`] has no writer.
//!
//! # How the rail reaches it
//!
//! The rail's namespace-generic paths — the append and log routes, the
//! sync-side prune — hold a journal and a namespace and nothing of the mesh.
//! They read the roster through `RingRail::roster`, the rail's ONE reader,
//! and [`MeshRosterSource`] is what that reader calls for any ring without a
//! `roster.json`. It
//! is installed once, beside the rail itself, by [`MeshRosterSource::install`];
//! until it existed those paths read the file (empty, for these namespaces) and
//! the daemon's own rings refused its own key at the door.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{Person, RailError, RingRail, Roster, RosterSource};
use commonwealth_transport::PeerContact;
use sovereign_contracts::identity::IdentityReader;
use sovereign_contracts::membership::MembershipEntry;
use tokio::sync::RwLock;

/// A ring roster derived from mesh membership, plus the reverse lookup a
/// caller needs to name a signer's node.
///
/// One derivation, one filter, one set of rules — a second walk of
/// `mesh.members` somewhere else would be a second answer to "who is in this
/// ring" (ARCH §10.6), and the two would disagree on exactly the edge cases
/// this type exists to decide.
#[derive(Debug, Clone, Default)]
pub struct MeshRoster {
    roster: Roster,
    /// actor (lowercase hex pubkey) → the member row it came from.
    node_ids: BTreeMap<String, NodeId>,
}

impl MeshRoster {
    /// Read the membership this node holds as a ring roster.
    ///
    /// `self_pubkey` is this node's own identity key as the daemon installed
    /// it at startup, and it is passed in rather than read out of our own
    /// member row on purpose: the row's `node_pubkey` is stamped by the
    /// *gossip* loop, so a freshly booted daemon has a key and no stamp for
    /// the first round. Taking it from the row would make a node unable to
    /// author on its own journal for the first few seconds of every boot, and
    /// the refusal it would get ("nobody in the roster claims that key") does
    /// not describe that at all.
    pub fn derive(mesh: &Mesh, self_id: NodeId, self_pubkey: Option<NodePubkey>) -> Self {
        Self::derive_rows(
            mesh.members
                .values()
                .map(|r| (r.node_id, r.name.as_str(), r.node_pubkey)),
            self_id,
            self_pubkey,
        )
    }

    /// The one derivation, over `(node id, name, endpoint key)` rows —
    /// whether they come from the in-process `Mesh` ([`Self::derive`]) or the
    /// daemon's membership port ([`Self::from_membership`]).
    fn derive_rows<'a>(
        rows: impl Iterator<Item = (NodeId, &'a str, Option<NodePubkey>)>,
        self_id: NodeId,
        self_pubkey: Option<NodePubkey>,
    ) -> Self {
        let mut members: BTreeMap<Person, Vec<String>> = BTreeMap::new();
        let mut node_ids: BTreeMap<String, NodeId> = BTreeMap::new();
        let mut unidentified = 0usize;

        for (node_id, name, node_pubkey) in rows {
            // A tombstone is kept. It stops a member being dialled and stops
            // its gossip counting; it must not retire the journal lines it
            // already signed. Dropping departed members here would turn a
            // housemate's whole history into `UnknownSigner` gaps the day
            // they leave — the rail is deliberately author-blind about
            // replication for the same reason (`ops_missing_from`), and a
            // roster that forgets would undo it on the read side.
            //
            // Membership here decides whose acts COUNT. Who may reach this
            // node at all is the transport's question and is answered
            // elsewhere; keeping the two apart is what lets each be one
            // decider (ARCH §10.6).
            let pubkey = if node_id == self_id {
                self_pubkey.or(node_pubkey)
            } else {
                node_pubkey
            };
            // No placeholder, ever. A shared default for "this node has not
            // advertised a key" would collide every unidentified node into
            // one identity and admit their ops as one another's — the same
            // reasoning `HostIdentity::from_live_mesh` already applies to a
            // missing hardware fingerprint. Absent is reported, never
            // defaulted (ARCH §18.3); the report is the gap admission emits.
            let Some(pubkey) = pubkey else {
                unidentified += 1;
                continue;
            };
            let actor = pubkey.to_string();
            // A member whose name is blank is still a member: the roster
            // decides membership, and the name is only how an admitted op
            // renders. Falling back to the node id keeps the key in the ring
            // rather than trading a cosmetic problem for a permanent gap.
            let person = if name.trim().is_empty() {
                Person::from(node_id.to_string())
            } else {
                Person::from(name.trim())
            };
            let keys = members.entry(person).or_default();
            if !keys.contains(&actor) {
                keys.push(actor.clone());
            }
            node_ids.insert(actor, node_id);
        }
        // Two laptops under one name is two keys in one row, which is the
        // shape `Roster` documents. Sorted so the derivation is a function of
        // the membership set and not of hash-map iteration order.
        for keys in members.values_mut() {
            keys.sort();
        }

        tracing::debug!(
            people = members.len(),
            actors = node_ids.len(),
            unidentified,
            "ring roster: derived from mesh membership"
        );
        Self {
            roster: Roster::new(members),
            node_ids,
        }
    }

    /// The roster this daemon's membership makes, right now.
    ///
    /// The ONE call site shape for a rail namespace whose roster is derived
    /// (ARCH §10.6): the identity comes from what the daemon installed at
    /// startup and the membership from the live `Mesh`, and a caller that
    /// assembled those two itself would be free to assemble them differently.
    ///
    /// Takes the roster, identity and pubkey rather than the daemon host's
    /// `AppState` (domains `REVIEW-build-mesh-api-decouple`): Fabric observes
    /// membership through its own state, and `sovereign-mesh` may not name the
    /// host. This is [`MeshRoster::derive`] over the daemon's membership port
    /// (`sovereign_contracts::membership`, pb-mesh-exit-core) — the entries'
    /// dial handle carries the endpoint key the derivation reads.
    pub fn from_membership(
        members: &[MembershipEntry<PeerContact>],
        self_id: NodeId,
        self_pubkey: Option<NodePubkey>,
    ) -> Self {
        Self::derive_rows(
            members
                .iter()
                .map(|m| (m.node_id, m.name.as_str(), m.dial.node_pubkey)),
            self_id,
            self_pubkey,
        )
    }

    /// The roster to hand [`admit`](commonwealth_rail::admit) or
    /// [`RingJournal::append`](commonwealth_rail::RingJournal::append).
    pub fn roster(&self) -> &Roster {
        &self.roster
    }

    /// The roster alone, for a caller that does not need to name signers.
    pub fn into_roster(self) -> Roster {
        self.roster
    }

    /// Which node signed with this key, if the roster claims it. Lets a
    /// caller name a publisher by node id without reading anything the
    /// publisher supplied.
    pub fn node_id_of(&self, actor: &str) -> Option<NodeId> {
        self.node_ids.get(actor).copied()
    }

    /// Whether this ring claims a key at all.
    ///
    /// A DIFFERENT question from the one [`RingJournal::append`] asks, and
    /// that is why both exist. The rail's door asks "may this key author on
    /// this journal" and answers with a sentence naming
    /// `svrn ring roster add` — right for a ring whose roster is written by
    /// hand, and wrong here, where no such command applies. This asks "is
    /// this node in the mesh at all", which is the membership question the
    /// mesh itself owns, and lets the caller say so in the mesh's words.
    pub fn claims(&self, actor: &str) -> bool {
        self.node_ids.contains_key(actor)
    }

    /// How many keys this ring claims.
    pub fn len(&self) -> usize {
        self.node_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.node_ids.is_empty()
    }
}

/// Moved with `is_daemon_owned` to `sovereign_contracts::ring_namespaces`
/// (phase-b pb-mesh-exit-mesh); re-exported at their historical paths.
pub use sovereign_contracts::ring_namespaces::{is_daemon_owned, REGISTERED_NAMESPACES};

/// Moved with the round to `commonwealth_rails::ring_sync` (phase-b
/// pb-rails-parity); re-exported at its historical path.
pub use commonwealth_rails::ring_sync::roster_names;

/// [`MeshRoster::from_membership`] as the rail sees it.
///
/// Holds the daemon's membership WEAKLY: the rail lives beside it, so a strong
/// reference here would be a cycle that keeps a test's state alive after the
/// test, and a source that outlives the membership it derives from has nothing
/// true to say anyway — it reports that, rather than an empty ring. The
/// identity and pubkey are cheap owned handles (`IdentityReader` is an `Arc`
/// clone; the pubkey is a `Copy` value set at construction).
pub struct MeshRosterSource {
    mesh: Weak<RwLock<Mesh>>,
    identity: IdentityReader,
    self_pubkey: Option<NodePubkey>,
}

impl MeshRosterSource {
    /// Install `state`'s membership as `rail`'s DEFAULT roster — every ring
    /// nobody narrowed admits everyone in the mesh — and register it for
    /// [`REGISTERED_NAMESPACES`], which no file may narrow.
    ///
    /// The ONE place a namespace and its source meet, so the daemon and the
    /// tests that stand in for it cannot register different pairs.
    pub fn install(
        rail: &RingRail,
        mesh: &Arc<RwLock<Mesh>>,
        identity: &IdentityReader,
        self_pubkey: Option<NodePubkey>,
    ) -> Result<(), RailError> {
        let source: Arc<dyn RosterSource> = Arc::new(Self {
            mesh: Arc::downgrade(mesh),
            identity: identity.clone(),
            self_pubkey,
        });
        rail.default_roster(Arc::clone(&source));
        for namespace in REGISTERED_NAMESPACES {
            rail.derive_roster(namespace, Arc::clone(&source))?;
        }
        tracing::debug!(
            registered = REGISTERED_NAMESPACES.len(),
            "ring roster: membership is every ring's default roster"
        );
        Ok(())
    }
}

impl RosterSource for MeshRosterSource {
    fn roster(&self) -> Pin<Box<dyn Future<Output = Result<Roster, RailError>> + Send + '_>> {
        Box::pin(async move {
            let Some(mesh) = self.mesh.upgrade() else {
                return Err(RailError::Io(
                    "the mesh state this roster derives from is gone".into(),
                ));
            };
            let mesh = mesh.read().await;
            Ok(MeshRoster::derive(&mesh, self.identity.current(), self.self_pubkey).into_roster())
        })
    }
}

// Test fixtures shared across the crate boundary: `mesh_http`'s endpoint
// tests (now in `sovereign-daemon`) read the SAME roster fixtures this
// module's own tests do, or neither proves anything about the same thing
// (ARCH §10.6). `#[test]` fns are stripped from non-test builds, so this
// compiles into production as fixtures only.
#[doc(hidden)]
pub mod tests;
