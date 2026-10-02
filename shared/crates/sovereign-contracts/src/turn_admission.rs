// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who may carry a turn's admission id across the chat wire (phase-b-27).
//!
//! The id (`ChatCompletionRequest::turn_admission`) tells the slot queue to
//! park a call it would otherwise shed, so it is honoured only from a caller
//! on this host: the daemon's own runtime dialing `serve`, or a local client
//! dialing the daemon. The listener that parses the wire decides, because only
//! it sees the connection, and after the switch a mesh peer's request reaches
//! `serve` through the daemon's peer listener and then over loopback
//! (principle 12). Everyone else's id is cleared here, so the request arrives
//! as fresh load, as it did before the field existed.

use std::net::SocketAddr;

use crate::principal::Principal;
use oicp_types::openai_types::ChatCompletionRequest;

/// Is this caller on this host? A loopback connection, and, where the
/// listener resolves principals (the daemon), not a mesh member, a bearer
/// client, a guest, or an unverified node claim: a peer can reach the daemon
/// through an iroh bridge on 127.0.0.1, and it presents `X-Node-Id`.
pub fn from_this_host(peer: Option<SocketAddr>, principal: Option<&Principal>) -> bool {
    let loopback = peer.is_some_and(|p| p.ip().is_loopback());
    let local_principal = matches!(
        principal,
        None | Some(Principal::LocalOwner { .. }) | Some(Principal::Anonymous)
    );
    loopback && local_principal
}

/// Keep the request's admission id only when [`from_this_host`] said yes.
pub fn honour_turn_admission(
    request: &mut ChatCompletionRequest,
    from_this_host: bool,
    listener: &'static str,
) {
    let Some(turn) = request.turn_admission.as_deref() else {
        return;
    };
    if from_this_host {
        tracing::debug!(target: "turn_admission", listener, turn, "honoured: caller is on this host");
    } else {
        tracing::debug!(target: "turn_admission", listener, turn, "dropped: caller is not on this host, served as fresh load");
        request.turn_admission = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Option<SocketAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn only_a_loopback_caller_that_is_not_a_peer_is_on_this_host() {
        let member = Principal::Member {
            node_id: crate::principal::NodeId::from_u128(7),
        };
        assert!(from_this_host(addr("127.0.0.1:1"), None));
        assert!(from_this_host(addr("[::1]:1"), Some(&Principal::Anonymous)));
        assert!(!from_this_host(addr("10.0.0.2:1"), None));
        assert!(!from_this_host(None, None));
        assert!(!from_this_host(addr("127.0.0.1:1"), Some(&member)));
        assert!(!from_this_host(
            addr("127.0.0.1:1"),
            Some(&Principal::Unverified)
        ));
    }

    #[test]
    fn a_caller_off_this_host_loses_the_id() {
        let mut req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}],
            "turn_admission": "turn-1",
        }))
        .unwrap();
        honour_turn_admission(&mut req, true, "test");
        assert_eq!(req.turn_admission.as_deref(), Some("turn-1"));
        honour_turn_admission(&mut req, false, "test");
        assert_eq!(req.turn_admission, None);
    }
}
