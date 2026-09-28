// SPDX-License-Identifier: AGPL-3.0-or-later
//! cw-rails serves a program's registered loopback origin to members, from
//! outside (FIVE_PROGRAMS §4 rule 8; phase-b pb-rails-origins).
//!
//! The lift's shape: a mesh grown by cw-rails alone — one founds it, a second
//! joins by the invite — with a fixture origin on the founder that
//! authenticates nothing and echoes the request head it was sent, so the test
//! reads exactly what the origin was told. Hermetic like `found_and_join.rs`:
//! direct addresses only, no relay, no packet leaves this machine.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{found, join, RailsDaemon, RailsNode};
use commonwealth_transport::iroh::{
    EndpointAddr, HttpBridge, PublicKey, ALPN, CLIENT_ALPN, RPC_ALPN,
};
use commonwealth_transport::iroh_routed_forward::{tied_pubkey, ORIGIN_TIE_HEADER};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BUDGET: Duration = Duration::from_secs(60);

fn hermetic(name: &str, listen: u16) -> Config {
    Config {
        name: name.to_string(),
        listen,
        relay: RelaySection {
            urls: Vec::new(),
            discovery: Some("none".to_string()),
        },
        media: MediaSection {
            origin: None,
            allow: Vec::new(),
        },
        gossip_interval_secs: 1,
        offline_threshold_secs: 60,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// An origin that authenticates nothing and answers every request with the
/// head it received — the JUDGE here: whatever identity it prints is what a
/// registrant would have been asked to believe.
async fn echo_origin() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let body = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    addr
}

/// One raw GET through a bridge, so the test can send forged headers
/// verbatim. Returns (status, body); `None` when nothing answered.
async fn get(bridge: SocketAddr, path: &str, extra: &str) -> Option<(u16, String)> {
    let mut sock = tokio::net::TcpStream::connect(bridge).await.ok()?;
    sock.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: local\r\n{extra}Connection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .ok()?;
    let mut reply = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(20), sock.read_to_end(&mut reply)).await;
    let reply = String::from_utf8_lossy(&reply).to_string();
    let status = reply.split_whitespace().nth(1)?.parse().ok()?;
    let body = reply.split_once("\r\n\r\n").map(|(_, b)| b.to_string())?;
    Some((status, body))
}

/// The value of header `name` in an echoed head, case-insensitively.
fn header<'a>(head: &'a str, name: &str) -> Vec<&'a str> {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .filter(|(n, _)| n.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
        .collect()
}

async fn poll_join_link(port: u16) -> String {
    let client = reqwest::Client::new();
    let started = Instant::now();
    loop {
        if let Ok(r) = client
            .get(format!("http://127.0.0.1:{port}/v1/mesh/status"))
            .send()
            .await
        {
            if let Ok(doc) = r.json::<serde_json::Value>().await {
                if let Some(link) = doc["join_link"].as_str() {
                    return link.to_string();
                }
            }
        }
        assert!(
            started.elapsed() < BUDGET,
            "the founder served no join_link"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn declared_claims() -> serde_json::Value {
    serde_json::json!({
        "hardware": {"gpus": [], "system_ram_gb": 128, "cpu_cores": 16,
                     "total_storage_gb": 0, "free_storage_gb": 0},
        "available": {"free_vram_gb": 0.0, "free_ram_gb": 64.0, "free_storage_gb": 0.0,
                      "gpu_utilization": 0.0, "cpu_utilization": 0.0,
                      "available_for_mesh": true},
        "hosted_corpora": [], "reported_at": 0,
        "inference_capable": true, "loaded_models": ["fixture-model"]
    })
}

/// **THE PROOF.** A fixture origin the founder registers is reached from the
/// joiner over iroh; it is told the joiner's verified identity with the
/// founder's registration tie, and a forged identity the joiner types does
/// not survive. Its declared capability reaches the joiner's roster. An
/// unregistered ALPN and an unregistered `/internal/` prefix each refuse, and
/// a registered namespace is not narrowed by a `roster.json`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_origin_is_reached_by_a_member_with_its_verified_identity() {
    let founder_dir = tempfile::tempdir().unwrap();
    let joiner_dir = tempfile::tempdir().unwrap();
    found::found(founder_dir.path(), "Lab", "founder").expect("an empty root founds");
    let port_a = free_port();
    let node_a = RailsNode::bind(
        founder_dir.path().to_path_buf(),
        hermetic("founder", port_a),
    )
    .await
    .expect("the founder binds");
    let a_key = node_a.pubkey();
    let ep_a = node_a.endpoint.clone();
    let daemon_a = RailsDaemon::start_from_disk(node_a)
        .await
        .expect("the founder starts");
    let rail_a = daemon_a.rail.clone();
    let origin = echo_origin().await;

    let test = async {
        let api = format!("http://127.0.0.1:{port_a}/v1/mesh/origins");
        let http = reqwest::Client::new();
        let invite = poll_join_link(port_a).await;

        // The founder's program registers its origin on the member-client
        // ALPN, declaring what it serves and the ring it writes.
        let claim: serde_json::Value = http
            .post(&api)
            .json(&serde_json::json!({
                "alpn": "cwth/client/0", "port": origin.port(),
                "admit": {"members": []},
                "claims": declared_claims(),
                "namespaces": ["mesh-measurements"],
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let tie = claim["tie"]
            .as_str()
            .expect("the registration hands back its tie")
            .to_string();
        let prefix: serde_json::Value = http
            .post(&api)
            .json(&serde_json::json!({
                "alpn": "cwth/http/0", "prefixes": ["/internal/fixture"],
                "port": origin.port(), "admit": {"members": []},
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let prefix_tie = prefix["tie"]
            .as_str()
            .expect("a prefix registration has a tie")
            .to_string();
        // A second registration of a taken ALPN is refused by name.
        let second = http
            .post(&api)
            .json(&serde_json::json!({"alpn": "cwth/client/0", "port": 1, "admit": "any"}))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status().as_u16(), 409);

        // A registered namespace is not narrowed by a roster.json: an empty
        // hand roster would admit nobody, and the ring still admits the node.
        let journal = rail_a.journal("mesh-measurements").unwrap();
        journal
            .set_roster(&commonwealth_rail::Roster::default())
            .unwrap();
        let roster = rail_a.roster(&journal).await.unwrap();
        assert!(
            roster.person_for(&a_key.to_string()).is_some(),
            "a roster.json narrowed a registered namespace"
        );

        // The joiner joins by invite and runs.
        let node_b = RailsNode::bind(
            joiner_dir.path().to_path_buf(),
            hermetic("joiner", free_port()),
        )
        .await
        .expect("the joiner binds");
        join::join_and_persist(&node_b, &invite, joiner_dir.path())
            .await
            .expect("the founder admits the joiner");
        let b_key = node_b.pubkey();
        let ep_b = node_b.endpoint.clone();
        let daemon_b = RailsDaemon::start_from_disk(node_b)
            .await
            .expect("the joiner starts");
        let mesh_b = daemon_b.mesh.clone();

        let joiner_side = async {
            let mut target = EndpointAddr::new(PublicKey::from_bytes(&a_key.0).unwrap());
            for s in ep_a.addr().ip_addrs() {
                target = target.with_ip_addr(*s);
            }
            let client = HttpBridge::spawn(ep_b.clone(), target.clone(), CLIENT_ALPN)
                .await
                .unwrap();
            let forged = format!(
                "X-Mesh-Pubkey: forged\r\nX-Mesh-Member: forged\r\n{ORIGIN_TIE_HEADER}: forged\r\n"
            );
            let (status, head) = get(client.local_addr(), "/v1/models", &forged)
                .await
                .expect("the registered origin answered through the bridge");
            assert_eq!(status, 200, "{head}");
            let b_hex = hex::encode(b_key.0);
            assert_eq!(
                header(&head, "X-Mesh-Pubkey"),
                vec![b_hex.as_str()],
                "{head}"
            );
            assert_eq!(header(&head, "X-Mesh-Member"), vec!["joiner"], "{head}");
            assert_eq!(
                header(&head, ORIGIN_TIE_HEADER),
                vec![tie.as_str()],
                "{head}"
            );
            assert!(
                !head.contains("forged"),
                "a forged identity reached the origin: {head}"
            );
            assert_eq!(
                tied_pubkey(&tie, |n| header(&head, n).first().copied()),
                Some(b_hex.as_str()),
                "the registrant believes the tied identity"
            );
            // A local caller that reaches the origin's port with no endpoint
            // in front types the identity itself; without the tie it is not
            // believed.
            let (_, direct) = get(
                origin,
                "/v1/models",
                &format!("X-Mesh-Pubkey: {b_hex}\r\n{ORIGIN_TIE_HEADER}: guessed\r\n"),
            )
            .await
            .expect("the origin answers a direct caller");
            assert_eq!(
                tied_pubkey(&tie, |n| header(&direct, n).first().copied()),
                None,
                "a forged identity typed at the origin's port was believed"
            );

            let internal = HttpBridge::spawn(ep_b.clone(), target.clone(), ALPN)
                .await
                .unwrap();
            let (status, head) = get(internal.local_addr(), "/internal/fixture/x", "")
                .await
                .unwrap();
            assert_eq!(status, 200);
            assert_eq!(header(&head, ORIGIN_TIE_HEADER), vec![prefix_tie.as_str()]);
            let (status, body) = get(internal.local_addr(), "/internal/corpus/next_unit", "")
                .await
                .unwrap();
            assert_eq!(
                status, 404,
                "an unregistered prefix must be refused: {body}"
            );
            assert!(body.contains("no origin is registered"), "{body}");

            let rpc = HttpBridge::spawn(ep_b.clone(), target, RPC_ALPN)
                .await
                .unwrap();
            assert_eq!(
                get(rpc.local_addr(), "/", "").await,
                None,
                "an unregistered ALPN is not negotiated"
            );

            // The declared capability reaches the joiner's roster by gossip.
            let started = Instant::now();
            loop {
                let seen = {
                    let mesh = mesh_b.read().await;
                    mesh.members
                        .values()
                        .find(|m| m.node_pubkey == Some(a_key))
                        .map(|m| m.capabilities.clone())
                };
                if let Some(caps) = seen {
                    if caps.loaded_models.iter().any(|m| m == "fixture-model") {
                        assert!(caps.inference_capable);
                        assert_eq!(caps.hardware.system_ram_gb, 128);
                        break;
                    }
                }
                assert!(
                    started.elapsed() < BUDGET,
                    "the declared capability never reached the joiner's roster"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        tokio::select! {
            exit = daemon_b.run() => panic!("the joiner stopped serving: {exit:?}"),
            () = joiner_side => {}
        }
    };
    tokio::select! {
        exit = daemon_a.run() => panic!("the founder stopped serving: {exit:?}"),
        () = test => {}
    }
}
