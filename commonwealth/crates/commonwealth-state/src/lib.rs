// SPDX-License-Identifier: AGPL-3.0-or-later
//! A little shared state, kept on every node and reconciled by gossip.
//!
//! # What this is for
//!
//! Some things a mesh does need a value every node can read and any node can
//! write: which shards of a corpus have already been ingested, what each peer
//! has contributed, what this daemon has been doing. Not a database — a
//! notice board, small enough that every machine keeps the whole thing.
//!
//! [`MeshStore`] is that board. It is a SQLite file in WAL mode with one row
//! per `(app_id, key)`, and it is the only type here you have to learn:
//!
//! ```text
//!   store.set(app_id, key, value, origin)   write locally
//!   store.get(app_id, key)                  read
//!   store.scan(app_id, prefix)              read a range
//!   store.all_entries_for_gossip()          what may leave this machine
//!   store.merge_entry(entry)                take a peer's row, if newer
//! ```
//!
//! A daemon polls `all_entries_for_gossip` on one side and hands what it
//! receives to `merge_entry` on the other. That is the whole replication
//! contract, and this crate implements neither end of the wire — see "not
//! here" below.
//!
//! # Three decisions worth knowing before reading the code
//!
//! **Last write wins, on a wall clock, and there is no vector clock coming.**
//! Every [`StoreEntry`] carries unix seconds and the [`NodeId`](commonwealth_core::ids::NodeId) that wrote it;
//! [`MeshStore::merge_entry`] keeps the higher timestamp. Two nodes writing
//! the same key in the same second means one write is lost, silently. That is
//! priced, not overlooked: the alternative costs a causality mechanism on
//! every row, and the shapes stored here are either single-writer or
//! append-only by key construction. Which brings us to —
//!
//! **Append-only is spelled in the key, not in the schema.** The ledgers
//! ([`ContributionEmitter`], [`ActivityEmitter`]) put origin, timestamp,
//! nanoseconds and a per-process counter in the key, so two events can never
//! collide and last-write-wins degenerates to "everything keeps". Reading a
//! ledger is a `scan` and a fold; there is no append table. Same trick in
//! [`processed_shards_key`], where the `:<node_id>` suffix gives each peer its
//! own slot so peers do not overwrite each other's progress.
//!
//! **Privacy is structural, not a convention.** Some app namespaces must never
//! leave the machine — your own token counts, and the private per-peer
//! preferences that let you quietly serve a peer less. They are not filtered
//! at the call site: [`GOSSIP_EXCLUDED_APP_IDS`] is a const list,
//! [`is_gossip_excluded`] is the one predicate, and
//! [`MeshStore::all_entries_for_gossip`] applies it — so a private namespace
//! is off the wire by construction rather than by every caller remembering
//! (ARCH §7). If you add a namespace that must stay local, the list is the
//! only place to say so.
//!
//! The same list carries a second, non-privacy class since cw-lift 2b:
//! namespaces every reader resolves against LOCAL state. Replicating those
//! bought nothing, and for `wikipedia-newsworthy:status` it actively broke
//! the reader — one unsuffixed `last_tick` key per mesh meant last-write-wins
//! handed you a peer's tick. Deciding what may leave the machine is one
//! decision with one implementation (ARCH §10.6), so it did not get a second
//! list.
//!
//! # The rest of the crate, by what it stores
//!
//! ```text
//!   contributions      what this node gave the mesh — gossiped
//!   activity           what this node did locally — never gossiped
//!   peer_preferences   privately serve a peer less; clamped to (0.0, 1.0]
//!   processed_shards   which corpus shards each peer has finished
//!   gc                 delete rows past a TTL — see the warning on RetentionGc
//! ```
//!
//! [`PeerPreference`] is worth one more line because the clamp is the design:
//! its constructor rejects anything above `1.0`, so the mechanism can only be
//! used to offer a peer less, never to build a favoured lane for one. Sanction,
//! not patronage.
//!
//! # What is deliberately not here
//!
//! No wire, no server, no scheduler. Something has to call
//! `all_entries_for_gossip`, put the rows on a socket, and call `merge_entry`
//! on the far side; this crate supplies both ends of that contract and neither
//! end of the transport. In the shipped daemon that caller is `sovereign-mesh`.
//!
//! [`RetentionGc`] is here and is **not spawned by anything that ships** —
//! read its docs before wiring it, because starting it begins deleting rows
//! from a live store and the scope decision has to be made on purpose.

pub mod activity;
mod backend;
pub mod contributions;
pub mod error;
pub mod gc;
pub mod peer_preferences;
pub mod processed_shards;
pub mod store;

pub use activity::{current_activity, served_for, ActivityEmitter, ACTIVITY_APP_ID};
pub use contributions::{current_contributions, ContributionEmitter, CONTRIBUTIONS_APP_ID};
pub use error::{Error, Result};
pub use gc::RetentionGc;
pub use peer_preferences::{
    is_gossip_excluded, PeerPreference, PeerPreferenceStore, GOSSIP_EXCLUDED_APP_IDS,
    PEER_PREFERENCES_APP_ID, PORTFOLIO_PRIVATE_APP_ID,
};
pub use processed_shards::{processed_shards_key, union_processed_shards, PROCESSED_SHARDS_APP_ID};
pub use store::{MeshStore, StoreEntry};
