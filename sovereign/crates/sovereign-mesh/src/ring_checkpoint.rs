// SPDX-License-Identifier: AGPL-3.0-or-later
//! The v1 checkpoint document (`docs/THE_LINK.md` §"The checkpoint,
//! specified"): one ring's record, frozen, in the shape a cold verifier reads.
//!
//! Composed HERE and nowhere else. The daemon's export route serves it and
//! `svrn ring checkpoint --verify` reads it; the verifier's tests build their
//! input with this same function, so the shape the host writes and the shape
//! the verifier is proven against cannot drift apart (ARCH principle 8). It
//! lives beside the rail port rather than in `commonwealth-rail-core` because
//! the checkpoint is built over that crate's shipped exports with no edit to
//! it (the-link's predicate, `quality/campaigns/the-link.toml`).

use commonwealth_rail_core::{digest, Op, Roster, SignedOp};

/// The document's version field.
pub const CHECKPOINT_VERSION: u64 = 1;

/// `ops` carried VERBATIM — each re-serialised to exactly the bytes the
/// journal's own writer produced — beside the roster they were admitted
/// under and the digest computed over those very ops. Nothing here is
/// trusted by a verifier: it re-parses the lines, re-runs admit and
/// recomputes the digest. `Err` only when an op cannot be serialised.
pub fn checkpoint_document(
    ns: &str,
    roster: &Roster,
    ops: &[Op<SignedOp>],
    created_unix: u64,
) -> Result<serde_json::Value, serde_json::Error> {
    let lines = ops
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::json!({
        "v": CHECKPOINT_VERSION,
        "ns": ns,
        "created_unix": created_unix,
        "roster": roster,
        "digest": digest(ops),
        "ops": lines,
    }))
}
