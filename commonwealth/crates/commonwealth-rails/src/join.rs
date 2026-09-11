// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cw-rails join <invite>` — the one command between installing this and
//! being on somebody's mesh.
//!
//! The invite is whatever a person was handed: a `sovereign://join/…` deep
//! link, an `https://…/join/…` link, or the bare `cwth-xxxx-xxxx-xxxx` key.
//! `commonwealth_discovery::deep_link::parse_join_argument` accepts all three
//! — this crate does not parse invites, it reads one.
//!
//! # The handshake, and why it is exactly one shape
//!
//! `parse_dial_string` → an [`HttpBridge`] on THIS daemon's long-lived
//! endpoint → `POST http://<bridge>/internal/join`. The QUIC handshake IS the
//! key verification: it fails unless the responder holds the private key in
//! the dial string, so there is nothing to check afterwards. A throwaway
//! endpoint would work and is wrong — it would mint a second node key, hole
//! punch a second set of paths, and hand the founder reachability that dies
//! with the command.
//!
//! # The refusal that is a scope statement
//!
//! **An invite with no iroh dial is refused by name.** The legacy forms —
//! a `relay=` host:port POSTed directly, and mDNS polling for a founder on
//! the LAN — are how a pre-iroh mesh was joined, and they are out of scope
//! for this daemon: both mean speaking plaintext HTTP to an address, which is
//! the whole posture this process exists not to have. The refusal says so
//! rather than failing at a connect timeout that reads like a network fault.

use std::path::Path;
use std::time::Duration;

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::wire::{JoinRejection, JoinRequest, JoinResponse};
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::deep_link::{parse_join_argument, DeepLink};
use commonwealth_transport::identity::{node_pubkey, sign_join_proof};
use commonwealth_transport::iroh::{parse_dial_string, HttpBridge, ALPN};

use crate::{identity, RailsNode};

/// How long the founder has to answer. The handshake is one round trip
/// through an already-established tunnel; fifteen seconds is the inference
/// daemon's own bound for the same POST.
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum JoinRefusal {
    #[error(
        "`{0}` is not a join link — expected a `sovereign://join/…` link, an \
         `https://…/join/…` link, or a bare `cwth-xxxx-xxxx-xxxx` key. \
         A `sovereign://guest/…` link is a different thing: it lends you a \
         model, it does not make you a member."
    )]
    NotAJoinLink(String),
    #[error(
        "that invite carries no iroh dial string, so there is nobody to dial by key. \
         cw-rails joins over iroh only — a LAN/mDNS join means speaking plaintext HTTP \
         to an address, which is the posture this daemon exists not to have. Ask for an \
         invite from a daemon with iroh on (`svrn mesh invite` on an encrypted mesh)."
    )]
    NoIrohDial,
    #[error("the invite's dial string is malformed: {0}")]
    BadDial(String),
    #[error("could not open a tunnel to the founder: {0}")]
    NoTunnel(String),
    #[error("the founder did not answer within {0:?}: {1}")]
    NoAnswer(Duration, String),
    #[error("the founder refused the join: {0}")]
    Rejected(String),
    #[error("the founder answered {0}, which is not a join response")]
    BadResponse(String),
}

/// What a join produced, before it is persisted.
#[derive(Debug)]
pub struct Joined {
    pub self_id: NodeId,
    pub mesh: Mesh,
}

/// Parse an invite down to the dial string a join needs, refusing by name.
pub fn dial_of(invite: &str) -> Result<(String, String), JoinRefusal> {
    // A `let ... else` rather than a `match` with a catch-all arm: this reads
    // ONE variant of a link vocabulary that other kinds of link live in
    // (`guest`, and whatever is minted next), and a `Some(_)` arm would be an
    // unreachable pattern the day that vocabulary has only this one.
    let Some(DeepLink::Join {
        join_key,
        iroh_dial,
        ..
    }) = parse_join_argument(invite)
    else {
        return Err(JoinRefusal::NotAJoinLink(invite.to_string()));
    };
    iroh_dial
        .map(|dial| (join_key, dial))
        .ok_or(JoinRefusal::NoIrohDial)
}

/// Run the handshake on `node`'s endpoint and return what the founder said.
/// Nothing is written to disk here — see [`join_and_persist`].
pub async fn join(node: &RailsNode, invite: &str) -> Result<Joined, JoinRefusal> {
    let (join_key, dial) = dial_of(invite)?;
    let target = parse_dial_string(&dial).map_err(JoinRefusal::BadDial)?;
    let name = node.config.name.clone();
    let pubkey = node_pubkey(&node.key);

    let bridge = HttpBridge::spawn(node.endpoint.clone(), target, ALPN)
        .await
        .map_err(|e| JoinRefusal::NoTunnel(e.to_string()))?;
    let url = format!("http://{}/internal/join", bridge.local_addr());
    tracing::info!(
        target: "rails",
        mesh_dial = %dial,
        bridge = %bridge.local_addr(),
        name = %name,
        node_id = %node.self_id,
        "join: dialing the founder by key"
    );

    let body = JoinRequest {
        join_key,
        joining_node_name: name.clone(),
        // Deliberately empty. These are IP-overlay addresses for the
        // plaintext transport, and this daemon is reachable by key or not at
        // all — gossiping an address nothing serves would be an invitation to
        // dial a port that answers nothing.
        joining_node_addresses: Vec::new(),
        proposed_node_id: Some(node.self_id),
        node_pubkey: Some(pubkey),
        pubkey_proof: Some(sign_join_proof(&node.key, &node.self_id, &name)),
    };

    let http = reqwest::Client::builder()
        .timeout(JOIN_TIMEOUT)
        .build()
        .map_err(|e| JoinRefusal::NoTunnel(e.to_string()))?;
    let response = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| JoinRefusal::NoAnswer(JOIN_TIMEOUT, e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        // A 401 body is a `JoinRejection` carrying the founder's own reason —
        // an expired invite reads differently from a wrong key, and the
        // person typing the command is the one who can fix either.
        let reason = serde_json::from_str::<JoinRejection>(&text)
            .map(|r| r.reason)
            .unwrap_or_else(|_| format!("HTTP {status}: {text}"));
        return Err(JoinRefusal::Rejected(reason));
    }
    let parsed: JoinResponse = response
        .json()
        .await
        .map_err(|e| JoinRefusal::BadResponse(e.to_string()))?;
    let mesh = parsed.mesh.into_mesh();
    tracing::info!(
        target: "rails",
        mesh = %mesh.name,
        mesh_id = %mesh.id,
        assigned = %parsed.assigned_node_id,
        members = mesh.members.len(),
        "join: admitted"
    );
    Ok(Joined {
        self_id: parsed.assigned_node_id,
        mesh,
    })
}

/// Join and write both files. The assigned id is persisted even when it is
/// the one we proposed: a founder may assign a different one, and the file is
/// what every later round stamps.
pub async fn join_and_persist(
    node: &RailsNode,
    invite: &str,
    data_dir: &Path,
) -> Result<Joined, crate::Refusal> {
    let joined = join(node, invite).await?;
    identity::save_node_id(data_dir, &joined.self_id)?;
    identity::save_mesh(data_dir, &joined.mesh)?;
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The refusal that is this daemon's scope, watched failing.** A
    /// plaintext-mesh invite parses fine and carries no `iroh=` — joining it
    /// would mean POSTing a join key to an IP address in the clear. The
    /// failing input is `dial_of` returning `Ok` for it, which is what a
    /// version that fell through to the legacy path would do.
    #[test]
    fn an_invite_with_no_iroh_dial_is_refused_by_name() {
        let key = commonwealth_discovery::membership::generate_join_key();
        let err = dial_of(&format!("sovereign://join/{key}")).unwrap_err();
        assert!(matches!(err, JoinRefusal::NoIrohDial), "{err}");
        assert!(err.to_string().contains("iroh only"), "{err}");

        // The bare key is the same case: it is a valid invite with no dial.
        assert!(matches!(
            dial_of(&key).unwrap_err(),
            JoinRefusal::NoIrohDial
        ));

        // And a `relay=` hint is NOT a dial string — it is the legacy
        // plaintext path wearing a query parameter.
        let relayed = format!("sovereign://join/{key}?relay=192.168.1.100:9741");
        assert!(matches!(
            dial_of(&relayed).unwrap_err(),
            JoinRefusal::NoIrohDial
        ));
    }

    /// The happy path: an encrypted-mesh invite yields the key and the dial.
    #[test]
    fn an_iroh_invite_yields_the_key_and_the_dial_string() {
        let key = commonwealth_discovery::membership::generate_join_key();
        let dial = format!("{}@https://relay.example/", hex::encode([9u8; 32]));
        let (got_key, got_dial) =
            dial_of(&format!("sovereign://join/{key}?iroh={dial}")).expect("an iroh invite");
        assert_eq!(got_key, key);
        assert_eq!(got_dial, dial);
    }

    /// A plaintext mesh that ships `dial=` is still key-dialable, and this
    /// daemon takes that path — the posture difference is the founder's, and
    /// what matters here is that there IS a key to dial.
    #[test]
    fn a_plaintext_invite_carrying_a_dial_string_is_accepted() {
        let key = commonwealth_discovery::membership::generate_join_key();
        let dial = format!("{}@https://relay.example/", hex::encode([9u8; 32]));
        let (_, got) =
            dial_of(&format!("sovereign://join/{key}?dial={dial}")).expect("a dial invite");
        assert_eq!(got, dial);
    }

    /// Not everything that parses is a join. A guest link lends a model and
    /// never makes the holder a member; treating it as an invite would fail
    /// at the founder with a confusing rejection instead of here with a
    /// sentence that says what the link IS.
    #[test]
    fn a_guest_link_is_refused_as_the_wrong_kind_of_link() {
        let link = "sovereign://guest/tok123?url=http://10.0.0.2:9743&exp=99999999999";
        let err = dial_of(link).unwrap_err();
        assert!(matches!(err, JoinRefusal::NotAJoinLink(_)), "{err}");
        assert!(err.to_string().contains("lends you a model"), "{err}");
    }

    #[test]
    fn nonsense_is_not_a_join_link() {
        let err = dial_of("have a nice day").unwrap_err();
        assert!(matches!(err, JoinRefusal::NotAJoinLink(_)), "{err}");
    }
}
