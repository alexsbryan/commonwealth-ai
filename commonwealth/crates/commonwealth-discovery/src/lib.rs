// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a mesh starts, and how a machine gets into one.
//!
//! # What this is for
//!
//! Before there is a roster to gossip, two problems have to be solved: a
//! machine has to find the others, and the others have to decide it may in.
//! This crate is both, plus the survey of what the local machine has to offer
//! once it is in.
//!
//! Founding a mesh is [`membership::init_mesh`] — it returns a
//! [`Mesh`](commonwealth_core::mesh::Mesh) with one member and the invite key
//! to hand a second person. Admitting that person is
//! [`membership::accept_join`] on the founder's side. Both live here rather
//! than in `commonwealth-core` because they mint secrets, and a crate that
//! mints secrets is not a crate of plain data.
//!
//! # The trust model is social, and the crate is shaped by that
//!
//! You join a mesh because someone you know sent you an invite. There is no
//! certificate authority, no registry, and no defence against a member who
//! turns hostile — a mesh is people, and it is scoped to a size where you know
//! all of them. What the code does defend is narrower and worth being precise
//! about, because the difference decides how much you can lean on it:
//!
//! **The invite key is never stored.** [`membership::generate_join_key`] mints
//! a human-readable `cwth-xxxx-xxxx-xxxx`; only its BLAKE3 hash is persisted,
//! and [`membership::verify_join_key`] compares through `blake3::Hash`, whose
//! `PartialEq` runs in constant time. Reading a founder's `mesh.json` does not
//! give you a working invite.
//!
//! **An invite is not the gossip credential.** Admission and "are we the same
//! mesh" are two different questions with two different secrets — see
//! `commonwealth-core`'s crate docs — so rotating an invite cannot partition a
//! running mesh. [`membership::generate_mesh_secret`] mints the gossip one;
//! [`membership::derive_legacy_mesh_secret`] exists only so a mesh created
//! before that split can reach the same value on every node without a
//! coordination round, and new meshes never call it.
//!
//! **Removing a member is a proposal, not a command.**
//! [`membership::RevocationProposal`] collects votes and
//! [`membership::RevocationProposal::has_majority`] decides; a single node
//! cannot evict a peer on its own.
//!
//! # Discovery is a hint, never an authorization
//!
//! [`mdns::MdnsDiscovery`] advertises this node on `_commonwealth._tcp.local.`
//! and browses for others. Everything it returns is a
//! [`mdns::DiscoveredPeer`] — an unauthenticated claim from the local network
//! about a name and an address. It tells a joiner which host to try; the join
//! key is what decides whether that try succeeds. Nothing here admits anyone.
//!
//! mDNS also carries the app layer: [`mdns::MdnsDiscovery::advertise_app`] and
//! [`mdns::MdnsDiscovery::browse_apps`] let a process on this node announce a
//! service other nodes can find, on the same daemon and the same wire.
//!
//! # What the machine can offer
//!
//! [`hardware::detect_hardware`] reads GPUs, RAM, cores and free disk into the
//! [`HardwareProfile`](commonwealth_core::capabilities::HardwareProfile) that
//! rides in every gossip round, and the `read_*_state` helpers sample the live
//! numbers a scheduler wants between rounds. It is best-effort by design: a
//! probe that cannot answer reports zero and logs, because a node that refuses
//! to join over an unreadable disk counter is worse than a node that joins
//! with an incomplete profile.
//!
//! # What is deliberately not here
//!
//! No gossip loop and no HTTP. This crate mints and checks credentials and
//! finds hosts; carrying a merge between two live daemons is the daemon's job,
//! over `commonwealth-transport`, using `commonwealth-core`'s merge rule. The
//! gossip, latency-probe and TLS modules that used to live here are gone —
//! the crate is three modules now, and its `Cargo.toml` description was
//! updated to match on 2026-09-04.

pub mod hardware;
pub mod mdns;
pub mod membership;

pub use commonwealth_core::{Error, Result};
