// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests of the guest dialer's private helpers, moved with them from
//! commonwealth-transport iroh.rs, and the proof of pb-reach-guest: a
//! non-member's dial, and one bridge.

use super::*;

#[test]
fn parse_relay_mode_empty_is_default() {
    // Empty config = leave the caller on the preset (n0) relays.
    assert!(parse_relay_mode(&[]).is_none());
}

#[test]
fn parse_relay_mode_valid_urls_build_custom() {
    let mode = parse_relay_mode(&[
        "https://relay.corp.example:443".to_string(),
        "https://relay2.corp.example".to_string(),
    ]);
    assert!(
        matches!(mode, Some(iroh::RelayMode::Custom(_))),
        "valid relay URLs must build a custom relay map"
    );
}

#[test]
fn parse_relay_mode_all_invalid_falls_back_to_default() {
    // A fat-fingered relay URL must not abort — fall back to the
    // default relays rather than take the node offline.
    assert!(parse_relay_mode(&["not a url".to_string()]).is_none());
}

#[test]
fn parse_relay_mode_skips_bad_keeps_good() {
    let mode = parse_relay_mode(&[
        "://broken".to_string(),
        "https://relay.corp.example:443".to_string(),
    ]);
    assert!(
        matches!(mode, Some(iroh::RelayMode::Custom(_))),
        "one valid URL among bad ones still yields a custom map"
    );
}

#[test]
fn redact_userinfo_hides_credentials() {
    assert_eq!(
        redact_userinfo("https://user:secret@proxy.corp:443"),
        "https://***@proxy.corp:443"
    );
    // No userinfo → unchanged.
    assert_eq!(
        redact_userinfo("https://proxy.corp:443"),
        "https://proxy.corp:443"
    );
    // Non-URL → unchanged (don't mangle).
    assert_eq!(redact_userinfo("proxy.corp:443"), "proxy.corp:443");
}

#[tokio::test]
async fn build_relayed_endpoint_with_custom_relay_binds() {
    // A configured self-hosted relay must not break endpoint
    // construction: bind is a local operation, the relay connection
    // is a background task (so this succeeds offline). Proves the
    // relay_urls → custom RelayMode path threads through to a valid
    // endpoint. proxy_from_env is a no-op here (env unset).
    let ep = build_relayed_endpoint(
        SecretKey::from_bytes(&[77u8; 32]),
        vec![GUEST_ALPN.to_vec()],
        &RelayConfig {
            relay_urls: vec!["https://relay.corp.example:443".to_string()],
            n0_services: true,
        },
    )
    .await
    .expect("custom-relay endpoint must bind");
    assert!(
        !ep.bound_sockets().is_empty(),
        "endpoint must bind a socket"
    );
}

/// A lender: an iroh endpoint on `GUEST_ALPN` whose streams splice, through
/// the one [`pump`], into a loopback HTTP origin. Returns its dial string.
async fn spawn_lender() -> String {
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async { "lender-origin" }),
    );
    tokio::spawn(async move { axum::serve(origin, app).await });

    let endpoint = EndpointBuilder::empty()
        .crypto_provider(ring_crypto_provider())
        .secret_key(SecretKey::from_bytes(&[41; 32]))
        .alpns(vec![GUEST_ALPN.to_vec()])
        .bind()
        .await
        .expect("lender endpoint binds");
    // iroh binds the wildcard; loopback is what dials in-process.
    let targets: Vec<String> = endpoint
        .bound_sockets()
        .into_iter()
        .map(|mut a| {
            if a.ip().is_unspecified() {
                let ip = if a.is_ipv4() { "127.0.0.1" } else { "::1" };
                a.set_ip(ip.parse().unwrap());
            }
            a.to_string()
        })
        .collect();
    let dial = format!(
        "{}@{}",
        hex::encode(endpoint.addr().id.as_bytes()),
        targets.join(",")
    );
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                while let Ok((send, recv)) = conn.accept_bi().await {
                    let tcp = tokio::net::TcpStream::connect(origin_addr).await.unwrap();
                    tokio::spawn(pump(
                        tcp,
                        send,
                        recv,
                        PumpSide::Acceptor,
                        "guest".to_string(),
                        GUEST_ALPN,
                    ));
                }
            });
        }
    });
    dial
}

/// THE proof of pb-reach-guest: a NON-member (an ephemeral key, no roster)
/// dials a lender by dial string through this leaf's dialer alone, and a
/// request answers.
#[tokio::test]
async fn a_non_member_dials_a_lenders_guest_alpn_and_a_request_answers() {
    let dial = spawn_lender().await;
    let tunnel = GuestTunnel::open(&dial, Vec::new(), Some("none"))
        .await
        .expect("the guest tunnel opens");
    let body = reqwest::Client::new()
        .get(format!("{}/v1/models", tunnel.base_url()))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .expect("the request reaches the lender through the tunnel")
        .text()
        .await
        .unwrap();
    assert_eq!(body, "lender-origin");
}

/// There is ONE iroh HTTP bridge: `HttpBridge`'s accept loop is the only
/// place in this leaf that opens a stream to a peer, and `pump` is defined
/// once, so `GuestTunnel` rides the bridge instead of keeping a loop of its
/// own. The needles are split so this file does not match itself.
#[test]
fn there_is_one_bridge_loop_and_one_pump() {
    fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    rs_files(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let sites = |needle: &str| -> Vec<String> {
        files
            .iter()
            .flat_map(|f| {
                let text = std::fs::read_to_string(f).unwrap();
                let name = f.display().to_string();
                text.lines()
                    .enumerate()
                    .filter(|(_, l)| l.contains(needle))
                    .map(|(i, _)| format!("{name}:{}", i + 1))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let bridges = sites(concat!("open_bi", "()"));
    assert_eq!(bridges.len(), 1, "one bridge loop, found {bridges:?}");
    assert!(bridges[0].contains("guest.rs"), "{bridges:?}");
    let pumps = sites(concat!("fn ", "pump("));
    assert_eq!(pumps.len(), 1, "one pump, found {pumps:?}");
}
