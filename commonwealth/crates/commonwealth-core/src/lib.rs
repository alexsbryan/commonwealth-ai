// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shared vocabulary of a mesh — the nouns every node agrees on, and the
//! pure functions that have to give every node the same answer.
//!
//! # What this is for
//!
//! A mesh is a handful of machines belonging to people who know each other,
//! agreeing to work as one. There is no server and no primary: each node
//! holds the whole roster, tells its neighbours what it knows, and reconciles
//! the two views. For that to settle, every node has to spell the roster the
//! same way and resolve a disagreement the same way. This crate is that
//! agreement, and nothing else.
//!
//! Reaching in for the first time you want [`mesh::Mesh`] — the roster, its
//! two credentials, and [`mesh::Mesh::merge_from_authenticated`], which is the
//! entire convergence rule in one function — and [`mesh::MemberRecord`], one
//! row of it.
//!
//! # Nothing here talks to anything
//!
//! This crate opens no socket, reads no file, and declares no `async fn`. It
//! is data and pure functions; the crates above it do the moving. That is why
//! its dependency closure is 55 crates where the daemons that use it are past
//! 600, and it is what makes the package liftable at all.
//!
//! ```text
//!   commonwealth-discovery   finding peers, minting and checking join keys
//!   commonwealth-transport   (peer, traffic class) -> a URL you can dial
//!   commonwealth-state       gossip-replicated key/value state
//!   ────────────────────────────────────────────────────────────────────
//!   commonwealth-core        ids, roster, capabilities, the merge rule  <-
//! ```
//!
//! The rail (`commonwealth-rail-core` / `commonwealth-rail`) is the package's
//! other half and does **not** sit on this crate — it shares no type with it.
//! A shared append-only log and a peer roster are separate problems here on
//! purpose; see `commonwealth/README.md`.
//!
//! # Three decisions worth knowing before reading the code
//!
//! **A mesh carries two credentials, and confusing them partitions it.**
//! [`mesh::Mesh::mesh_secret`] answers "are we the same mesh" and is minted
//! once and never rotated — there is no setter for it. [`mesh::Mesh::invite_key_hash`]
//! answers "may this node in" and rotates freely. They were one field, and
//! rotating an invite therefore cut off every peer that had not yet heard about
//! the rotation. Gossip authorizes on the secret, so rotation is now invisible
//! to it. What crosses the wire is a keyed-BLAKE3 proof of the secret rather
//! than the secret ([`mesh::Mesh::mesh_proof`]), and which predicate
//! authorized a round is recorded as a [`mesh::GossipAuthArm`] rather than
//! re-derived downstream.
//!
//! **Convergence has a direction.** Two views of a roster reconcile by
//! last-writer-wins on [`mesh::MemberRecord::event_time`] — the later of a
//! node's own heartbeat and its removal tombstone, so a departure out-competes
//! a stale live copy while a genuine rejoin out-competes the tombstone. Three
//! fields sit outside that rule because LWW is the wrong algebra for them:
//! `node_pubkey` can only be gained (a peer relaying an older record must not
//! strip an identity), `require_encryption` can only tighten, and
//! `invite_version` only counts up. A merge that could move any of those
//! backwards would let one stale peer undo the mesh's security posture.
//!
//! **Every gossiped type has to deserialize on a node that has never heard of
//! the field.** A mesh is upgraded one machine at a time, and a peer whose
//! serde fails drops the whole payload — reading, from the other side, as a
//! node with no capabilities at all rather than as a version skew. So new
//! fields land `#[serde(default)]`, removals stay declared until the fleet has
//! moved, and a rename carries `#[serde(rename)]` for as long as any peer might
//! still send the old spelling. [`mesh::MeshWire`] exists for the narrower
//! reason that `NodeId` is a byte array and JSON cannot key an object with one.
//!
//! # The modules, by the question they answer
//!
//! ```text
//!   Who is in this mesh, and who wins a disagreement?
//!     mesh            Mesh, MemberRecord, merge_from_authenticated
//!     mesh_merge      what one merge round did, and why
//!     mesh_identity   one dialable endpoint belongs to one member
//!     ids             MeshId, NodeId, ModelId, ...; NodePubkey
//!
//!   What can this machine do, and what has it done?
//!     capabilities    GPUs, RAM, cores, free disk, what is free right now
//!     contributions   what this node gave the mesh — gossiped, never scored
//!     activity        what this node did locally — never leaves the machine
//!     latency         the pairwise latency matrix
//!     peer_health     quarantine a peer that keeps failing, then retry it
//!
//!   How do I reach a peer, and is it really that peer?
//!     peer_addr       which of a peer's addresses to try first
//!     dial_sig        signed reachability: only a node changes its own
//!     ct              constant-time compare, in one place
//!
//!   Who does this piece of work?
//!     partition       leader election and rendezvous hashing over the roster
//!
//!   Vocabulary shared with things built on top
//!     knowledge       corpus shard plans and ingestion handoffs
//!     model           model metadata and the model-file route strings
//!     config          the shape of config.toml — the shape only, no loader
//!     clock           injectable time, so clock skew is testable
//!     error, oicp     one Error type; the OICP wire types, re-exported
//! ```
//!
//! # What is deliberately not here
//!
//! No transport, no gossip loop, no persistence, no scheduler. The rule that
//! keeps this crate small is that anything needing a runtime, a socket, or a
//! disk belongs one layer up — `commonwealth/BOUNDARY.md` is the contract and
//! `cargo xtask boundary-gate` enforces it. [`config::DaemonConfig`] is the
//! sharpest illustration: the struct is here, the TOML parser that fills it is
//! not, and `toml` is a dev-dependency for exactly that reason.

pub mod activity;
pub mod capabilities;
pub mod clock;
pub mod config;
pub mod contributions;
pub mod ct;
pub mod dial_sig;
pub mod error;
pub mod ids;
pub mod knowledge;
pub mod latency;
pub mod mesh;
pub mod mesh_identity;
pub mod mesh_merge;
pub mod model;
pub mod peer_addr;
pub mod peer_health;
pub use oicp_types as oicp;
pub mod partition;

pub use clock::{Clock, SystemClock, TestClock};
pub use error::{Error, Result};
pub use ids::{HandoffId, MeshId, ModelId, NodeId, PlanId, ProcessId};
