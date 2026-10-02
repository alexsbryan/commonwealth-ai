// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ratchet behind "no decider reads the header" — a test, not a comment.
//!
//! `x-node-id` is what a peer TYPES about itself. Until 2026-09-20 nine
//! production sites under `sovereign/crates` read it and decided something on
//! it: ledger attribution, per-peer manifest affinity, the scheduler's
//! reciprocity key, the peer tally, and the three literal reads that routed a
//! request to the peer gate at all. Every one of them would have believed
//! `x-node-id: <somebody-else>` typed by any caller that could reach the port.
//!
//! They now read the [`Principal`](sovereign_contracts::principal::Principal)
//! a resolver attached, and the header itself is read in exactly ONE file:
//! [`sovereign_contracts::principal::claimed_node_id`], which produces a
//! *claim*, never an identity.
//!
//! ## Why a grep and not a lint
//!
//! ARCH principle 10: make it structural, not remembered. A comment saying
//! "don't read the header" is exactly what `admission.rs:306` used to say
//! ("the verified node id") while the code below it did the opposite. This
//! module fails the normal test run instead (the test is
//! `quality/xtask/tests/mesh_principal_gate.rs`, which a lifted svrn
//! does not carry: it scans the monorepo), and it greps for the LITERAL
//! header as well as the parser's name, so moving the read behind a fresh
//! helper does not satisfy it (bar `mp-no-decider-reads-the-header`'s named
//! goodhart).
//!
//! ## Three lists, because there are three different things to say
//!
//! [`READ_ALLOWED`] is the one file that may hold the literal header or the
//! parser: `sovereign-contracts`'s `principal`, where the wire form lives with
//! the key it produces. Two crates resolve requests to a principal —
//! `sovereign-daemon` (the client and internal surfaces) and `sovereign-server`
//! (its own auth layer) — so the form cannot live privately in either without
//! the two drifting.
//!
//! [`RESOLVERS`] are those two files, and they may CALL [`claimed_node_id`]
//! without holding the wire form. A resolver is not a decider: it turns a
//! claim into a `Principal` and decides nothing else. Nothing else may call it,
//! which is what stops a decider reaching the header behind a helper the grep
//! does not name (the bar's own goodhart).
//!
//! [`SENDERS`] STAMP the header on outbound requests and are unchanged by this
//! work. They stay for one release: a receiver on an older build routes on the
//! header's PRESENCE, so a sender that stopped would have its turns admitted
//! as that node's own local traffic with pause, foreground yield and
//! `max_peer_inflight` all dark — the 2026-08-06 failure recorded at
//! `sovereign-serving-host/src/peer_inference.rs`. Stamping is not deciding.
//!
//! Test code is exempt twice over: `tests/` trees are not walked, and inside a
//! production file everything from the first `#[cfg(test)]` on is skipped. A
//! test that types a forged header is precisely how the refusal is proved.

/// The one production path that may hold the literal header or the parser.
/// Repo-relative, matched as a suffix so the test runs from any cwd.
pub const READ_ALLOWED: &[&str] = &["sovereign/crates/sovereign-contracts/src/principal.rs"];

/// The two request-to-principal resolvers, which may call
/// [`claimed_node_id`](sovereign_contracts::principal::claimed_node_id) and
/// nothing more. One per HTTP surface that has one.
pub const RESOLVERS: &[&str] = &[
    "sovereign/crates/sovereign-daemon/src/client_principal.rs",
    "sovereign/crates/sovereign-server/src/auth.rs",
];

/// The five sites that STAMP the header outbound. Unchanged by this work and
/// kept for one release — see the module docs for the failure a silent stop
/// reproduces. `oicp-client` was always one; the list said four while the
/// walk covered only `sovereign/crates/`.
pub const SENDERS: &[&str] = &[
    "oicp-client/src/lib.rs",
    "sovereign/crates/sovereign-grants/src/shard_manager.rs",
    "sovereign/crates/sovereign-daemon/src/routes_knowledge.rs",
    "sovereign/crates/sovereign-daemon/src/server.rs",
    "sovereign/crates/sovereign-serving-host/src/peer_inference.rs",
];

/// The wire form itself, in both spellings, plus the retired parser's name so
/// that reviving it under the old name does not open a second door.
pub const WIRE_FORM: &[&str] = &["x-node-id", "X-Node-Id", "parse_x_node_id"];

/// The published reader. Allowed in [`RESOLVERS`] only — naming it anywhere
/// else is a decider reaching the header behind a helper.
pub const READER: &str = "claimed_node_id";
