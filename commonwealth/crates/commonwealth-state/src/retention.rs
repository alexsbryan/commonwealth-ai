// SPDX-License-Identifier: AGPL-3.0-or-later
//! How long a namespace's rows are worth holding — declared ONCE.
//!
//! # Why this is not a parameter of the sweep
//!
//! [`MeshStore`](crate::MeshStore) is a PROJECTION of the ring journal: every
//! round, [`MeshStore::apply_projection`](crate::MeshStore::apply_projection)
//! folds the whole admitted op set and upserts each winning row. A row a local
//! sweep deleted has no incumbent, so the very next fold re-inserts it from the
//! journal — and the fold runs every sixty seconds. Before this module,
//! `RetentionGc`'s thirty-day window on `contributions` was undone within a
//! minute, every minute, on any node with an online peer
//! (`a_retention_sweep_is_not_undone_by_the_next_projection`).
//!
//! So retention on a rail-backed namespace is not something a deleter can do
//! downstream of the fold. It has to be part of the fold's decision, and then
//! there can only be ONE cutoff or the two disagree every round (ARCH §10.6):
//! the sweep takes a row and the fold hands it back, or the fold withholds a
//! row the sweep would have kept. The window is declared here, and both the
//! projection and [`RetentionGc`](crate::RetentionGc) read it.
//!
//! # Why an expiry needs no tombstone
//!
//! A delete is a fact only its author knows, so it travels as an act on the
//! journal. An expiry is not: the floor is `now - window`, the window is this
//! table, and `t` is on every op — so every node in the ring derives the same
//! answer for the same row without being told. Publishing a tombstone per
//! retired row would put one ~594-byte journal line on every node in the mesh
//! for each row retention exists to remove, which is the wrong direction for
//! the only mechanism bounding the journal.
//!
//! What actually shortens the journal is the seal: `rail_kv_pump::snapshot`
//! re-appends this node's live set FROM THE STORE, so a row this module keeps
//! out of the store is a row the next snapshot does not carry above the floor,
//! and the compaction that follows deletes its line. The store bound and the
//! journal bound are the same decision reached twice.
//!
//! # Adding a namespace
//!
//! Add a row to [`RETENTION_WINDOW_DAYS`] — not a branch, and not a second
//! constant next to the caller. The value must be the window the namespace's
//! READERS use, because a row older than that is provably invisible to every
//! one of them; a retention shorter than the read window deletes rows a reader
//! still aggregates, and a longer one keeps rows nothing can see. A namespace
//! absent from the table declares no window and is never swept for age.

/// Per-namespace retention window, in days. The whole table.
///
/// `contributions` is an append-only event log: `ContributionEmitter::record`
/// writes one row per served request under a key carrying an origin+time+seq
/// suffix, so LWW never collapses two events and nothing ever overwrites one.
/// Every reader (`current_contributions`, `commonwealth balance`) aggregates
/// over [`DEFAULT_WINDOW_DAYS`](commonwealth_core::contributions::DEFAULT_WINDOW_DAYS),
/// so the retention IS that window rather than a second independently chosen
/// number — widen the window and the retention follows.
pub const RETENTION_WINDOW_DAYS: &[(&str, u32)] = &[(
    crate::contributions::CONTRIBUTIONS_APP_ID,
    commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
)];

/// The declared window for `app_id`, or `None` when it declares none.
///
/// `None` is "this namespace is not swept for age", which is a different fact
/// from "swept at zero days" and must never collapse into one (ARCH §18.3) —
/// a zero window would delete the whole namespace on the next round.
pub fn window_days(app_id: &str) -> Option<u32> {
    RETENTION_WINDOW_DAYS
        .iter()
        .find(|(id, _)| *id == app_id)
        .map(|(_, days)| *days)
}

/// [`window_days`] in seconds — the form a TTL wants.
pub fn window_secs(app_id: &str) -> Option<u64> {
    window_days(app_id).map(|d| u64::from(d).saturating_mul(86_400))
}

/// The oldest `t` this namespace still holds, given `now`.
///
/// A row is expired when its timestamp is STRICTLY below this, matching
/// `delete_older_than_in_app`'s `timestamp < cutoff` — one boundary, so a row
/// exactly on it is kept by the sweep and by the fold alike.
///
/// `now` is a parameter for the reason
/// [`MeshStore::gc_before`](crate::MeshStore::gc_before) exists: a caller that
/// planted a row against its own earlier clock read and then let this function
/// take a second one is racing the wall clock at the boundary.
pub fn floor_at(app_id: &str, now: u64) -> Option<u64> {
    window_secs(app_id).map(|w| now.saturating_sub(w))
}

/// [`floor_at`] against the wall clock. What the projection uses: it runs on a
/// timer with no caller-supplied instant, and every window in the table is
/// measured in days, so a tick between two reads cannot move which side of the
/// floor a row is on.
pub fn floor_now(app_id: &str) -> Option<u64> {
    floor_at(app_id, commonwealth_core::clock::unix_now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CONTRIBUTIONS_APP_ID;

    /// The declared window is the READ window, not a second number beside it.
    /// If these diverge, retention deletes rows a reader still aggregates (or
    /// keeps rows nothing can see) and neither side goes red on its own.
    #[test]
    fn the_ledgers_retention_is_its_aggregation_window() {
        assert_eq!(
            window_days(CONTRIBUTIONS_APP_ID),
            Some(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS)
        );
    }

    /// A namespace that declares nothing is never swept — and `None` must not
    /// read as a zero window, which would empty it on the next round.
    #[test]
    fn an_undeclared_namespace_has_no_floor_rather_than_a_zero_one() {
        assert_eq!(window_days("work-atlas"), None);
        assert_eq!(floor_at("work-atlas", 1_000_000), None);
    }

    /// The boundary, stated once: `floor_at` is the cutoff
    /// `delete_older_than_in_app` compares STRICTLY against, so a row exactly
    /// on it survives both the sweep and the fold. An off-by-one here moves how
    /// much history every reader sees.
    #[test]
    fn the_floor_is_now_minus_the_window_exactly() {
        let now = 2_000_000_000u64;
        let days = u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS);
        assert_eq!(
            floor_at(CONTRIBUTIONS_APP_ID, now),
            Some(now - days * 86_400)
        );
    }

    /// A clock before the window is a saturating floor of zero, not a wrapped
    /// enormous cutoff that would delete the whole namespace.
    #[test]
    fn a_clock_below_the_window_floors_at_zero() {
        assert_eq!(floor_at(CONTRIBUTIONS_APP_ID, 5), Some(0));
    }
}
