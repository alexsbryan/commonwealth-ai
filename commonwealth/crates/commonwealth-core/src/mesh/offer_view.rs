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
}

/// The HOLDER's line: this node's OWN record now says something different
/// about the library it offers, so the round about to be sent carries it.
///
/// `stamped_from_dial_info` is the first suspect made visible: `origins` and
/// `media_allow` are stamped only inside the round's `self_iroh_dialinfo()`
/// arm, so a node with no live dial info publishes an offer it has accepted
/// on disk — the absence is named rather than inferred from a missing line.
pub fn log_self_stamp(before: &OfferView, after: &OfferView, stamped_from_dial_info: bool) {
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
