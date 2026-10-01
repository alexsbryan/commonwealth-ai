// SPDX-License-Identifier: AGPL-3.0-or-later
//! The registration tie: cw-rails forwarding to svrn's peer routes is
//! believed exactly as the in-process acceptor's hop is, and a tie that is
//! not the live claim's is no tie.

use super::*;
use tokio::sync::watch;

const TIE: &str = "0123456789abcdef";

/// A daemon whose roster names one member by `KEY`, registered with cw-rails
/// under the live tie [`TIE`].
fn registered(node_id: NodeId) -> (AppState, watch::Sender<Option<String>>) {
    let state = state_with_member(node_id);
    let (tx, rx) = watch::channel(Some(TIE.to_string()));
    state
        .inner
        .node
        .peer_origin_tie
        .install(rx)
        .expect("a fresh daemon holds no registration");
    (state, tx)
}

/// What cw-rails writes on a forward to a registered HTTP origin.
fn rails_headers(pubkey: &str, tie: &str) -> HeaderMap {
    headers(&[
        ("x-mesh-member", "LittleMac"),
        ("x-mesh-node", "node-0000000000000000"),
        ("x-mesh-pubkey", pubkey),
        (ORIGIN_TIE_HEADER, tie),
    ])
}

/// The flip's inbound path: a forward carrying the live tie resolves to the
/// member the roster names, and the tie is off the request afterwards.
#[tokio::test]
async fn a_forward_carrying_the_live_tie_resolves_to_the_member() {
    let id = NodeId::from_u128(0xBEEF);
    let (state, _tx) = registered(id);
    let mut h = rails_headers(&hex::encode(KEY), TIE);
    let p = state.resolve_internal(&mut h, loopback()).await;
    assert_eq!(member_of(&p), Some(id), "got {p:?}");
    assert!(
        h.get(ORIGIN_TIE_HEADER).is_none(),
        "the tie must not reach a handler"
    );
}

/// A tie that is not the live claim's — guessed, or from a claim that
/// lapsed — is a typed claim: stripped, and the caller is unverified.
#[tokio::test]
async fn a_wrong_or_lapsed_tie_is_not_a_tie() {
    let id = NodeId::from_u128(0xBEEF);
    let (state, tx) = registered(id);
    let mut h = rails_headers(&hex::encode(KEY), "guessed");
    let p = state.resolve_internal(&mut h, loopback()).await;
    assert_eq!(p, Principal::Unverified);
    assert!(h.get("x-mesh-pubkey").is_none(), "the claim is stripped");

    tx.send_replace(None);
    let mut h = rails_headers(&hex::encode(KEY), TIE);
    let p = state.resolve_internal(&mut h, loopback()).await;
    assert_eq!(p, Principal::Unverified, "a lapsed claim ties nothing");
}

/// cw-rails dials the registered port on loopback, so the live tie from
/// anywhere else is no tie.
#[tokio::test]
async fn the_live_tie_from_a_non_loopback_caller_is_not_a_tie() {
    let (state, _tx) = registered(NodeId::from_u128(0xBEEF));
    let mut h = rails_headers(&hex::encode(KEY), TIE);
    let p = state.resolve_internal(&mut h, lan()).await;
    assert_eq!(p, Principal::Unverified);
}

/// A request with no connect info is not loopback (the stricter reading), so
/// the live tie on it ties nothing. Successor of the pre-flip
/// `a_missing_connect_info_is_not_loopback`.
#[tokio::test]
async fn a_missing_connect_info_is_not_loopback_so_no_tie() {
    let (state, _tx) = registered(NodeId::from_u128(0xBEEF));
    let mut h = rails_headers(&hex::encode(KEY), TIE);
    let p = state.resolve_internal(&mut h, None).await;
    assert_eq!(p, Principal::Unverified);
}

/// A tied forward whose key does not parse names nobody. Successor of the
/// pre-flip `a_malformed_verified_key_resolves_unverified`.
#[tokio::test]
async fn a_malformed_tied_key_resolves_unverified() {
    let (state, _tx) = registered(NodeId::from_u128(0xBEEF));
    let mut h = rails_headers("not-a-key", TIE);
    let p = state.resolve_internal(&mut h, loopback()).await;
    assert_eq!(p, Principal::Unverified);
}

/// With no registration, the tie header alone is a typed claim.
#[tokio::test]
async fn an_unregistered_daemon_believes_no_tie() {
    let state = state_with_member(NodeId::from_u128(0xBEEF));
    let mut h = rails_headers(&hex::encode(KEY), TIE);
    let p = state.resolve_internal(&mut h, loopback()).await;
    assert_eq!(p, Principal::Unverified);
}

/// A tied forward whose key the daemon's roster does not name is a joiner,
/// not a member: before the flip, every key cw-rails' own roster forwards
/// is such a key, which is why the registration answers nothing until the
/// daemon reads cw-rails' roster (`crate::peer_origin`).
#[tokio::test]
async fn a_tied_key_the_roster_does_not_name_is_unverified() {
    let (state, _tx) = registered(NodeId::from_u128(0xBEEF));
    let mut h = rails_headers(&hex::encode([9u8; 32]), TIE);
    let p = state.resolve_internal(&mut h, loopback()).await;
    assert_eq!(p, Principal::Unverified);
}
