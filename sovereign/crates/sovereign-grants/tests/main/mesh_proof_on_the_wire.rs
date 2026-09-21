// SPDX-License-Identifier: AGPL-3.0-or-later
//! `tg-2-strangers-are-refused`, this crate's half: the shard pull
//! `ShardManager` builds carries `x-mesh-proof` when a pair is handed in, and
//! carries nothing when it is not.
//!
//! **Why on the wire and not on the plan.** Asserting that `MergePlan` holds
//! the pair asserts that the test filled a struct. What a peer's internal-port
//! gate reads is the REQUEST HEAD, so the subject here is a socket that keeps
//! the head of every call it receives — something `merge_participants` cannot
//! author (ARCH principle 5).
//!
//! The same raw-socket shape `merge_participants_coverage::serve_tarball` uses,
//! and for the same reason: the pull path — reqwest, the header, the response —
//! stays inside the test rather than being stubbed around it. `merge_participants`
//! fails on this fixture (the recorder serves a 404), and that is fine:
//! `fetch_remote_shard` has already put the request on the wire by then, which
//! is the whole claim.

use std::sync::{Arc, Mutex};

use commonwealth_core::ids::{HandoffId, NodeId};
use commonwealth_state::MeshStore;
use corpus_engine::CorpusEngine;
use corpus_index::types::EmbedFn;
use sovereign_grants::shard_manager::MergePlan;
use sovereign_grants::ShardManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Heads = Arc<Mutex<Vec<String>>>;

/// A stand-in peer that keeps the head of every request and answers 404, so
/// the pull stops there.
async fn spawn_recorder() -> (String, Heads) {
    let heads: Heads = Default::default();
    let sink = heads.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let sink = sink.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            head.extend_from_slice(&buf[..n]);
                            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                sink.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&head).to_string());
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), heads)
}

/// Drive one shard pull and give back what the peer saw.
async fn pull_once(mesh_proof: Option<(&str, &str)>) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let (url, heads) = spawn_recorder().await;
    let embed: EmbedFn = Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0f32; 8]) }));
    let index_dir = tmp.path().join("indexes");
    let engine = Arc::new(CorpusEngine::new(
        tmp.path().join("recipes"),
        index_dir.clone(),
        embed,
    ));
    let manager = ShardManager::new(
        Arc::clone(&engine),
        index_dir,
        Arc::new(MeshStore::in_memory().unwrap()),
    );

    let local = NodeId::from_u128(1);
    let peer = NodeId::from_u128(2);
    let peer_urls = vec![(peer, url)];
    let participants = [local, peer];

    let _ = manager
        .merge_participants(MergePlan {
            handoff_id: HandoffId::from_u128(7),
            corpus_id: "wire",
            local_node_id: local,
            participants: &participants,
            peer_shard_base_urls: &peer_urls,
            mesh_proof,
            ephemeral: false,
            expected_partitions: None,
        })
        .await;

    let seen = heads.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "the shard pull must have reached the peer exactly once; got {seen:?}"
    );
    seen.into_iter().next().unwrap().to_ascii_lowercase()
}

#[tokio::test]
async fn the_shard_pull_carries_the_pair_it_was_given() {
    let head = pull_once(Some(("x-mesh-proof", "cafe.babe"))).await;
    assert!(
        head.contains("x-mesh-proof: cafe.babe"),
        "the shard pull must carry the proof it was handed:\n{head}"
    );
    // The label the route already had is untouched — the proof is beside it,
    // not instead of it.
    assert!(
        head.contains("x-node-id:"),
        "the peer label survives:\n{head}"
    );
}

/// THE failing input, and the state this crate shipped in before the pair
/// existed: no header at all, which a peer on the default `internal_auth`
/// answers 401 to over any non-loopback hop.
#[tokio::test]
async fn the_shard_pull_carries_nothing_when_this_mesh_holds_no_secret() {
    let head = pull_once(None).await;
    assert!(
        !head.contains("x-mesh-proof"),
        "a node with no credential must offer no proof — an offered-and-failed \
         proof is a refusal at the receiver, an absent one is merely unproved:\n{head}"
    );
}
