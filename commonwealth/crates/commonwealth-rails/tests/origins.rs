// SPDX-License-Identifier: AGPL-3.0-or-later
//! cw-rails serves a program's registered loopback origin to members, from
//! outside (FIVE_PROGRAMS §4 rule 8; phase-b pb-rails-origins).
//!
//! The lift's shape: a mesh grown by cw-rails alone — one founds it, a second
//! joins by the invite — with a fixture origin on the founder that
//! authenticates nothing and echoes the request head it was sent, so the test
//! reads exactly what the origin was told. Hermetic like `found_and_join.rs`:
//! direct addresses only, no relay, no packet leaves this machine.
//!
//! The outbound half (pb-rails-reach) rides the same mesh of two: a local
//! caller on the joiner reaches the founder's registered origin through
//! cw-rails' reach door alone, with `mesh_reach::rails::RailsTransport`.

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
        work_offer: Default::default(),
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
        "inference_capable": true, "loaded_models": ["fixture-model"],
        "anchor": {"can_anchor": true, "vram_gb": 0, "rpc_port": 50052, "rpc_iroh": true}
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
        let port_b = free_port();
        let node_b = RailsNode::bind(joiner_dir.path().to_path_buf(), hermetic("joiner", port_b))
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
            // And the joiner's HTTP roster carries it, with the founder's dial
            // data, for a program's discovery to read (pb-serve-distributes).
            let status = format!("http://127.0.0.1:{port_b}/v1/mesh/status");
            let started = Instant::now();
            loop {
                let doc: serde_json::Value = http
                    .get(&status)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let row = doc["members"]
                    .as_array()
                    .and_then(|ms| ms.iter().find(|m| m["name"] == "founder"))
                    .cloned();
                let anchor = row
                    .as_ref()
                    .map(|r| r["capabilities"]["anchor"].clone())
                    .unwrap_or_default();
                let direct = row
                    .as_ref()
                    .and_then(|r| r["dial"]["iroh_direct_addrs"].as_array().cloned())
                    .unwrap_or_default();
                if anchor["rpc_port"] == 50052 && !direct.is_empty() {
                    assert_eq!(anchor["rpc_iroh"], true);
                    // The member's full id, which a roster reader keys on
                    // (`node_id` is the truncated display form), is the
                    // founder's own (pb-serve-distributes-standalone).
                    let founder: serde_json::Value = http
                        .get(format!("http://127.0.0.1:{port_a}/v1/mesh/status"))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    let full = founder["self"]["node_id_hex"].as_str().unwrap_or_default();
                    assert_eq!(full.len(), 32, "self.node_id_hex: {founder}");
                    assert_eq!(
                        row.as_ref().map(|r| r["node_id_hex"].clone()),
                        Some(full.into())
                    );
                    break;
                }
                assert!(
                    started.elapsed() < BUDGET,
                    "the joiner's /v1/mesh/status never carried the founder's anchor and direct addresses: {row:?}"
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

/// A fixture origin that streams a response in 32 chunked writes, the shape
/// of a streaming completion.
async fn streaming_origin() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                    .await;
                for i in 0..32 {
                    let data = format!("data: {i}\n\n");
                    let _ = sock
                        .write_all(format!("{:x}\r\n{data}\r\n", data.len()).as_bytes())
                        .await;
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                let _ = sock.write_all(b"0\r\n\r\n").await;
            });
        }
    });
    addr
}

/// A fixture media origin honouring `Range: bytes=a-b` over a 4 MiB body.
async fn range_origin() -> SocketAddr {
    const LEN: usize = 4 << 20;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let (a, b) = header(&head, "Range")
                    .first()
                    .and_then(|r| r.strip_prefix("bytes="))
                    .and_then(|r| r.split_once('-'))
                    .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
                    .unwrap_or((0, LEN - 1));
                let body = vec![b'm'; b + 1 - a];
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{LEN}\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = sock.write_all(&body).await;
            });
        }
    });
    addr
}

/// One raw GET timed: (first byte, whole reply, reply bytes).
async fn timed(base_url: &str, path: &str, extra: &str) -> (Duration, Duration, usize) {
    let authority = base_url.strip_prefix("http://").unwrap_or(base_url);
    let started = Instant::now();
    let mut sock = tokio::net::TcpStream::connect(authority).await.unwrap();
    sock.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: local\r\n{extra}Connection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut first = [0u8; 1];
    sock.read_exact(&mut first).await.unwrap();
    let ttfb = started.elapsed();
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(20), sock.read_to_end(&mut rest)).await;
    (ttfb, started.elapsed(), rest.len() + 1)
}

fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort();
    xs[xs.len() / 2]
}

/// **THE PROOF (outbound).** On the lift's mesh of two, the joiner's
/// `RailsTransport` resolves the founder's registered origin through cw-rails'
/// reach door, and a request through the bridge it returns answers with the
/// joiner's verified identity. For EVERY traffic class it returns exactly what
/// cw-rails' own transport yields — the class → ALPN map is cw-rails' table,
/// never a second copy in the client.
///
/// It also takes the hop reading the row asks for (it gates nothing; no bar
/// was pre-registered): a streamed completion and a Range media read, n = 5
/// each, through the joiner's own transport ("before", the daemon's way) and
/// through RailsTransport ("after": cold = with the door's resolve, warm =
/// cached).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rails_transport_reaches_a_peers_registered_origin_through_cw_rails() {
    use commonwealth_transport::{PeerTransport, TrafficClass};
    use mesh_reach::rails::RailsTransport;

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
    let a_id = node_a.self_id;
    let daemon_a = RailsDaemon::start_from_disk(node_a)
        .await
        .expect("the founder starts");
    let echo = echo_origin().await;
    let stream = streaming_origin().await;
    let media = range_origin().await;

    let test = async {
        let api = format!("http://127.0.0.1:{port_a}/v1/mesh/origins");
        let http = reqwest::Client::new();
        let invite = poll_join_link(port_a).await;
        for reg in [
            serde_json::json!({"alpn": "cwth/http/0", "prefixes": ["/internal/fixture"],
                               "port": echo.port(), "admit": {"members": []}}),
            serde_json::json!({"alpn": "cwth/client/0", "port": stream.port(),
                               "admit": {"members": []}}),
            serde_json::json!({"alpn": "cwth/media/0", "port": media.port(),
                               "admit": {"members": []}}),
        ] {
            let r = http.post(&api).json(&reg).send().await.unwrap();
            assert!(
                r.status().is_success(),
                "{reg}: {}",
                r.text().await.unwrap()
            );
        }

        let port_b = free_port();
        let node_b = RailsNode::bind(joiner_dir.path().to_path_buf(), hermetic("joiner", port_b))
            .await
            .expect("the joiner binds");
        join::join_and_persist(&node_b, &invite, joiner_dir.path())
            .await
            .expect("the founder admits the joiner");
        let b_hex = hex::encode(node_b.pubkey().0);
        let daemon_b = RailsDaemon::start_from_disk(node_b)
            .await
            .expect("the joiner starts");
        let own = daemon_b.transport();
        let roster_b = daemon_b.mesh.clone();

        let joiner_side = async {
            let rails = RailsTransport::new(format!("http://127.0.0.1:{port_b}"));
            // The founder as the joiner's roster has it, once it is live and
            // gossips an iroh path.
            let started = Instant::now();
            let contact = loop {
                let found = {
                    let mesh = roster_b.read().await;
                    commonwealth_media::roster_of(&mesh)
                        .into_iter()
                        .find(|(c, p)| {
                            c.node_id == a_id
                                && c.active
                                && (p.relay_url.is_some() || !p.iroh_direct_addrs.is_empty())
                        })
                        .map(|(_, p)| p)
                };
                if let Some(p) = found {
                    if !rails
                        .endpoints(&p, TrafficClass::ControlPlane)
                        .await
                        .is_empty()
                    {
                        break p;
                    }
                }
                assert!(
                    started.elapsed() < BUDGET,
                    "the joiner's reach door never resolved the founder"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            };

            // Every class: the client's answer IS cw-rails' answer.
            for class in TrafficClass::ALL {
                let direct = own.endpoints(&contact, class).await;
                let through = rails.endpoints(&contact, class).await;
                assert_eq!(
                    through, direct,
                    "{class:?}: RailsTransport and cw-rails' own transport disagree"
                );
            }

            // A request through the resolved bridge answers, with the
            // joiner's verified identity.
            let ep = &rails.endpoints(&contact, TrafficClass::ControlPlane).await[0];
            let bridge: SocketAddr = ep
                .base_url
                .strip_prefix("http://")
                .unwrap()
                .parse()
                .unwrap();
            let (status, head) = get(bridge, "/internal/fixture/x", "").await.unwrap();
            assert_eq!(status, 200, "{head}");
            assert_eq!(
                header(&head, "X-Mesh-Pubkey"),
                vec![b_hex.as_str()],
                "{head}"
            );

            // The hop reading. Five runs each; medians reported.
            let range = "Range: bytes=0-1048575\r\n";
            let mut rows = Vec::new();
            for (what, class, path, extra) in [
                (
                    "stream",
                    TrafficClass::Inference,
                    "/v1/chat/completions",
                    "",
                ),
                ("range", TrafficClass::Media, "/video.mkv", range),
            ] {
                let mut before = (Vec::new(), Vec::new());
                let mut cold = (Vec::new(), Vec::new());
                let mut warm = (Vec::new(), Vec::new());
                for _ in 0..5 {
                    let base = own.endpoints(&contact, class).await[0].base_url.clone();
                    let (f, t, n) = timed(&base, path, extra).await;
                    assert!(n > 1, "{what}: nothing came back");
                    before.0.push(f);
                    before.1.push(t);

                    let fresh = RailsTransport::new(format!("http://127.0.0.1:{port_b}"));
                    let started = Instant::now();
                    let base = fresh.endpoints(&contact, class).await[0].base_url.clone();
                    let resolve = started.elapsed();
                    let (f, t, _) = timed(&base, path, extra).await;
                    cold.0.push(resolve + f);
                    cold.1.push(resolve + t);

                    let started = Instant::now();
                    let base = fresh.endpoints(&contact, class).await[0].base_url.clone();
                    let lookup = started.elapsed();
                    let (f, t, _) = timed(&base, path, extra).await;
                    warm.0.push(lookup + f);
                    warm.1.push(lookup + t);
                }
                rows.push(format!(
                    "{what}: first byte before {:?} / after cold {:?} / after warm {:?}; \
                     whole reply before {:?} / after cold {:?} / after warm {:?}",
                    median(before.0),
                    median(cold.0),
                    median(warm.0),
                    median(before.1),
                    median(cold.1),
                    median(warm.1),
                ));
            }
            for row in rows {
                println!("reach hops (n=5, medians): {row}");
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
