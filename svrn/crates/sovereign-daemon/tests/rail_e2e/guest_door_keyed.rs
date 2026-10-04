// SPDX-License-Identifier: AGPL-3.0-or-later
//! **On a keyed daemon the guest door's own bind is no bypass.** Ship gate F6
//! (pb-distribution-f6-guest-door-seal): the door's TCP bind serves the router
//! `guest_door::door_router` builds, and that router is sealed where it is
//! built, so a key reaches the same scope there as on the client listener.
//!
//! A real socket for both, because the door only listens while a rail grant
//! is live and the seal reads the peer address. The named failing input:
//! drop `api_keys::seal` from `door_router`, and the non-admin key reaches
//! `/v1/models` at the door while the client listener refuses it.

use std::net::SocketAddr;
use std::time::Duration;

use sovereign_daemon::api_keys;
use sovereign_daemon::client_tokens::{client_tokens_dir, keys, ClientTokenStore, KEY_ADMIN_GROUP};
use sovereign_daemon::guest_door::{serve, GuestPages};

use super::*;

const ALICE: &str = "alice-key-0000000000000000000000000000000000000000000000000000";
const IT: &str = "it-key-000000000000000000000000000000000000000000000000000000000";

/// A daemon holding two API keys — `alice` outside the admin group, `it` in
/// it — and a live wall grant, so the door is open.
fn keyed_state(data_dir: &std::path::Path) -> AppState {
    let dir = client_tokens_dir(data_dir);
    keys::add_key(&dir, "alice", &[], ALICE).unwrap();
    keys::add_key(&dir, "it", &[KEY_ADMIN_GROUP.to_string()], IT).unwrap();
    let node = NodeId::from_u128(1);
    let state = AppState::new_with_seeds(
        node,
        None,
        None,
        sovereign_daemon::state::FabricSeed::default(),
        sovereign_daemon::state::ServingSeed::default(),
        sovereign_daemon::state::NodeSeed {
            client_token: Some(Arc::<str>::from(TOKEN)),
            named_client_tokens: Arc::new(ClientTokenStore::load(Some(dir))),
            ..Default::default()
        },
        Arc::new(ledger_double::RecordingLedger::new(node)).seed(),
    );
    assert!(state.inner.node.named_client_tokens.is_keyed());
    with_guest(state, vec![Scope::Rails(NS.into())])
}

fn free_addr() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

/// The client listener, sealed the way `start_daemon` seals it.
async fn client_listener(state: &AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = api_keys::seal(client_router(state.clone()), state)
        .into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, service).await;
    });
    addr
}

async fn get(addr: SocketAddr, path: &str, key: &str) -> (u16, String) {
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .bearer_auth(key)
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

#[tokio::test]
async fn a_key_at_the_doors_own_bind_reaches_only_its_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let state = keyed_state(tmp.path());
    let client = client_listener(&state).await;
    let door = free_addr();
    tokio::spawn(serve(
        state.clone(),
        Some(door.to_string()),
        Arc::new(GuestPages::default()),
        None,
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while reqwest::get(format!("http://{door}/status")).await.is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the door never opened with a live rail grant"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Outside alice's scope: the door refuses with the client listener's
    // answer, byte for byte.
    let at_client = get(client, "/v1/models", ALICE).await;
    let at_door = get(door, "/v1/models", ALICE).await;
    assert_eq!(at_client.0, 403, "the client listener: {}", at_client.1);
    assert_eq!(
        at_door, at_client,
        "the door's own bind is a bypass of alice's key scope"
    );

    // A key whose scope covers the route still gets the same answer there.
    let at_client = get(client, "/v1/models", IT).await;
    let at_door = get(door, "/v1/models", IT).await;
    assert!(
        ![401, 403].contains(&at_door.0),
        "the door refused an admin key in scope: {at_door:?}"
    );
    assert_eq!(at_door, at_client);
}
