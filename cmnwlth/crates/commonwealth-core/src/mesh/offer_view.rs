// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a member's record says about the library it offers — and the two
//! lines that say when that answer changed.
//!
//! A viewer's media rail (`commonwealth_media::offers`) reads exactly three
//! fields off a member's capabilities: what it serves (`origins`), who may
//! reach it (`media_allow`), and whether it is free (`media_available`). An
//! offer becomes visible when that triple changes on the HOLDER's own record
//! (the gossip self-stamp) and again when it changes on a PEER's copy (the
//! merge). Those are the two moments, and until rr-2 neither was logged: a
//! withdrawal that a peer kept listing for 98.02 s while every gossip round
//! in between reported `reach ok` on both sides could not be attributed to a
//! side at all (room run 2, `target/ralph/rr2-room-run2.log`).
//!
//! One type and one log call per side, here rather than at either site, so
//! the holder's line and the peer's line are the same three fields in the
//! same order and a run can be read by subtracting two timestamps.

use crate::capabilities::OriginKind;
use crate::mesh::MemberRecord;

/// The offer-relevant triple, lifted off a record so two of them compare.
#[derive(Debug, Clone, PartialEq)]
pub struct OfferView {
    /// What this member serves (`Media` is the library).
    pub origins: Vec<OriginKind>,
    /// Who the holder admits; empty = every member.
    pub media_allow: Vec<String>,
    /// `0.0` the holder is watching it, `1.0` free, `None` no reading.
    pub media_available: Option<f32>,
}

impl OfferView {
    /// This record's triple as a viewer's rail would read it.
    pub fn of(record: &MemberRecord) -> Self {
        Self {
            origins: record.capabilities.origins.clone(),
            media_allow: record.capabilities.media_allow.clone(),
            media_available: record.capabilities.media_available,
        }
    }

    /// Put this view's offer back onto capabilities a gossip round has just
    /// replaced wholesale.
    ///
    /// The offer is the HOLDER's own state; dial info is transport, and only
    /// the dial addresses may be absent (ARCH 12). The round's `fresh_caps` is
    /// built from hardware and corpora and hard-codes `origins`/`media_allow`
    /// empty (`sovereign-mesh/src/capabilities.rs`), and the live acceptor read
    /// that fills them runs only when `self_iroh_dialinfo()` answers — so
    /// without this call the replace withdraws an accepted offer by omission on
    /// every round with no live dial info, and the peer's LWW keeps the empty
    /// triple until dial info returns: measured at 22 rounds (little stamped
    /// 03:27:04, peers flipped 03:30:45 — room run, A45).
    ///
    /// Only the two fields the replace blanks are carried. `media_available` is
    /// not: it rides the claims port, which is not transport and answers every
    /// round, and carrying it would let a reading outlive the offer it
    /// described — the case [`log_merged`]'s sibling rule in the stamp site
    /// clears. Refreshing the offer, including withdrawing it, stays the live
    /// read's job.
    pub fn restamp_onto(&self, caps: &mut crate::capabilities::NodeCapabilities) {
        caps.origins = self.origins.clone();
        caps.media_allow = self.media_allow.clone();
    }
}

/// The HOLDER's line: this node's OWN record now says something different
/// about the library it offers, so the round about to be sent carries it.
///
/// `stamped_from_dial_info` is the first suspect made visible: `origins` and
/// `media_allow` are REFRESHED only inside the round's `self_iroh_dialinfo()`
/// arm, so a node with no live dial info publishes the offer it already held
/// ([`OfferView::restamp_onto`]) rather than a fresh read. `false` also emits
/// [`log_self_stamp_without_dial_info`] every round — one entry point, so the
/// stamp site has one call and both lines cannot drift apart.
pub fn log_self_stamp(before: &OfferView, after: &OfferView, stamped_from_dial_info: bool) {
    if !stamped_from_dial_info {
        log_self_stamp_without_dial_info(after);
    }
    if before == after {
        return;
    }
    tracing::info!(
        target: "transport",
        origins = ?after.origins,
        media_allow = ?after.media_allow,
        media_available = ?after.media_available,
        was_origins = ?before.origins,
        was_media_available = ?before.media_available,
        stamped_from_dial_info,
        "gossip: this node's own offer view changed — this round carries it to peers"
    );
}

/// The HOLDER's line for the round that had NO live dial info: every round,
/// unconditionally, naming the triple it is about to publish.
///
/// [`log_self_stamp`] is silent when the triple did not change, and that is
/// exactly the round A45 could not read: with no dial info the stamp site had
/// no live acceptor to read the offer off, published the triple `fresh_caps`
/// left behind, and said nothing either way. This line makes the absence
/// itself an event, so a run shows how many consecutive rounds went out
/// without a live read instead of leaving it to be inferred from a gap.
pub fn log_self_stamp_without_dial_info(publishing: &OfferView) {
    tracing::info!(
        target: "transport",
        stamped_from_dial_info = false,
        origins = ?publishing.origins,
        media_allow = ?publishing.media_allow,
        media_available = ?publishing.media_available,
        "gossip: no live dial info this round — publishing the offer view this node already held"
    );
}

/// The PEER's line: an incoming record changed what we believe a member
/// offers. This is the moment `GET /v1/mesh/media` on this node starts (or
/// stops) listing that member — the other end of the holder's line above.
pub fn log_merged(existing: Option<&MemberRecord>, incoming: &MemberRecord, arm: &'static str) {
    let after = OfferView::of(incoming);
    let before = existing.map(OfferView::of);
    if before.as_ref() == Some(&after) {
        return;
    }
    if before.is_none() && after.origins.is_empty() {
        // First sight of a member that offers nothing is not an event.
        return;
    }
    tracing::info!(
        target: "transport",
        peer = %incoming.name,
        node_id = %incoming.node_id,
        arm,
        origins = ?after.origins,
        media_allow = ?after.media_allow,
        media_available = ?after.media_available,
        was_origins = ?before.as_ref().map(|b| b.origins.clone()),
        event_time = incoming.event_time(),
        "gossip: a member's offer view changed on merge — this node's media rail reads it now"
    );
}

/// The PEER's OTHER line: what the merge did with this member's record this
/// round, said for EVERY member on EVERY merge whether anything changed or
/// not.
///
/// [`log_merged`] is the event line and it returns early on an unchanged
/// triple; the refusal and not-older arms return before it is even called. So
/// a run where a peer never listed a change cannot say which of three things
/// happened — the triple never arrived in the bytes, it arrived and was equal
/// to what we already held, or the LWW arm never fired
/// (`Mesh::merge_one_member`'s `LocalRecordNotOlder`). Room run 3 hit exactly
/// that: little stamped `media_available=Some(0.0)` for ~12 consecutive
/// rounds, `log_sent_snapshot` never fired on any of the three nodes, and
/// both peers held two merge lines each for little in the whole run. This is
/// the line that separates the three, so it carries BOTH triples and BOTH
/// `event_time`s rather than a diff.
///
/// `debug` and not `info`: one line per member per round is the volume
/// `transport: resolved` already carries, and the room run captures
/// `transport` at debug.
pub fn log_merge_arm(
    existing: Option<&(OfferView, u64)>,
    incoming: &MemberRecord,
    arm: &'static str,
) {
    let incoming_view = OfferView::of(incoming);
    let existing_view = existing.map(|(view, _)| view);
    tracing::debug!(
        target: "transport",
        peer = %incoming.name,
        node_id = %incoming.node_id,
        arm,
        in_origins = ?incoming_view.origins,
        in_media_allow = ?incoming_view.media_allow,
        in_media_available = ?incoming_view.media_available,
        in_event_time = incoming.event_time(),
        have_origins = ?existing_view.map(|v| v.origins.clone()),
        have_media_allow = ?existing_view.map(|v| v.media_allow.clone()),
        have_media_available = ?existing_view.and_then(|v| v.media_available),
        have_event_time = existing.map(|(_, at)| *at),
        "gossip: merge read this member's offer triple"
    );
}

/// The WIRE's line: the snapshot this round is about to POST to every picked
/// peer does not say what the round stamped on our own record a moment ago.
///
/// The two lines above cover the holder's write and the peer's merge, and
/// between them sat an unread gap: the round stamps under the mesh write lock
/// and then, after releasing it and awaiting the peer selection, takes a
/// FRESH read of `fabric.mesh` and sends that clone. Beefy's 22 rounds were
/// silent on both existing lines while a peer listed no offer, which can only
/// mean the bytes disagreed with the stamp — and no site read the bytes. This
/// is that read, and it is the only one: a run where this line never appears
/// has proved the wire carries what the stamp wrote, which is what makes the
/// remaining suspect the merge.
///
/// `stamped` is `None` when this node holds no record of itself (the round
/// stamped nothing); `sent` is `None` when the snapshot has no self record.
pub fn log_sent_snapshot(stamped: Option<&OfferView>, sent: Option<&OfferView>) {
    if stamped == sent {
        return;
    }
    tracing::warn!(
        target: "transport",
        stamped_origins = ?stamped.map(|s| s.origins.clone()),
        stamped_media_allow = ?stamped.map(|s| s.media_allow.clone()),
        stamped_media_available = ?stamped.and_then(|s| s.media_available),
        sent_origins = ?sent.map(|s| s.origins.clone()),
        sent_media_allow = ?sent.map(|s| s.media_allow.clone()),
        sent_media_available = ?sent.and_then(|s| s.media_available),
        "gossip: the snapshot this round sends disagrees with what it stamped — a writer ran between the stamp and the snapshot"
    );
}
