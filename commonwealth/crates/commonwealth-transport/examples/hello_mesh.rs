// SPDX-License-Identifier: AGPL-3.0-or-later
//! hello_mesh — the smallest complete exchange on the dial-by-key transport.
//!
//! One process serves, one dials, there is no server in the middle, and the
//! service behind the gate never writes a login page: the caller's Ed25519 key
//! arrives in a request header because the QUIC handshake proved it.
//!
//! ```sh
//! # Terminal 1 — a keyed node with a service on it. Prints a dial ticket.
//! cargo run -p commonwealth-transport --features iroh --example hello_mesh -- serve
//!
//! # Terminal 2 — any other node reaches that service by dialing the ticket.
//! cargo run -p commonwealth-transport --features iroh --example hello_mesh -- dial '<ticket>'
//! ```
//!
//! Four things the two commands show:
//!
//! 1. **A key is the whole identity.** `--key <64-hex>` pins one (repeat a run
//!    and nothing changes); omitted, a fresh one is minted per run. There is no
//!    account, no name, no certificate.
//! 2. **A ticket is `<key>@<relay-or-address>[,...]`.** `serve` prints one and
//!    keeps it current; `dial` takes it verbatim. Relay URLs and direct
//!    addresses can both be in it and the QUIC stack picks a path.
//! 3. **The gate is a comparison, and it happens once per connection, on the
//!    key the handshake proved.** `serve --admit <hex>` closes the door on
//!    every other key: the ticket is public, the key is the credential. The
//!    value is the DIALER's public key — what the other side prints as
//!    `identity`, and what the first half of its ticket would be.
//! 4. **The service sees who called, in a header it can trust.** The client
//!    sends a forged `X-Mesh-Pubkey`; the acceptor strips every client-supplied
//!    `x-mesh-*` header and appends the verified one. `serve` prints what
//!    actually arrived — the forgery never gets there.
//!
//! The service itself is ordinary HTTP on loopback: it knows nothing about
//! iroh, holds no keys, and only reads a header. That is the shape a house app
//! or a shim takes — the transport is the part you do not write.
//!
//! `--no-n0` on either side builds the endpoint with no n0 relay and no n0 DNS
//! (`RelayConfig::from_parts(.., "none")`): two nodes on one flat LAN, or a
//! mesh that runs its own infrastructure, need nothing else.
//!
//! This is the transport level. One level up — a port on a laptop that people
//! reach by name, with the caller's verified key already in a request header —
//! is `docs/PUBLISH_AN_APP.md` and `svrn run --as <name> -- python app.py`.

#[cfg(not(feature = "iroh"))]
fn main() {
    eprintln!("hello_mesh requires a build with --features iroh");
    std::process::exit(2);
}

#[cfg(feature = "iroh")]
#[tokio::main]
async fn main() {
    real_main::run().await;
}

#[cfg(feature = "iroh")]
mod real_main {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use commonwealth_transport::iroh::{
        build_relayed_endpoint, format_dial_string, parse_dial_string, Endpoint, Forward,
        HttpBridge, IrohAcceptor, PublicKey, RelayConfig, SecretKey, TransportAddr,
        TransportAddrUsage,
    };

    /// One protocol, version-suffixed, so a later hello cannot collide with it.
    const HELLO_ALPN: &[u8] = b"cwth/hello/0";

    pub async fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("serve") => serve(&args[1..]).await,
            Some("dial") => dial(&args[1..]).await,
            _ => {
                eprintln!(
                    "usage:\n  hello_mesh serve [--key <64-hex>] [--admit <dialer-pubkey-hex>] [--no-n0]\n  \
                     hello_mesh dial <ticket> [--key <64-hex>] [--no-n0]"
                );
                std::process::exit(2);
            }
        }
    }

    fn flag_value(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    }

    /// The key this process speaks as: `--key`, or a fresh one. A pinned key is
    /// what makes the `--admit` demo repeatable — the gate is ON the key, so a
    /// changing key would make every run a different test.
    fn identity(args: &[String]) -> SecretKey {
        match flag_value(args, "--key") {
            Some(hex_key) => SecretKey::from_bytes(&bytes32_of(&hex_key, "--key")),
            None => {
                let mut seed = [0u8; 32];
                getrandom::fill(&mut seed).expect("getrandom");
                SecretKey::from_bytes(&seed)
            }
        }
    }

    /// `--key` is a secret seed, `--admit` a public key; both arrive as 32
    /// bytes of hex and neither is guessed at.
    fn bytes32_of(hex_str: &str, flag: &str) -> [u8; 32] {
        let bytes = hex::decode(hex_str.trim()).unwrap_or_else(|e| {
            eprintln!("{flag} is not hex: {e}");
            std::process::exit(2);
        });
        bytes.as_slice().try_into().unwrap_or_else(|_| {
            eprintln!("{flag} must be 32 bytes of hex (64 characters)");
            std::process::exit(2);
        })
    }

    fn relay_config(args: &[String]) -> RelayConfig {
        if args.iter().any(|a| a == "--no-n0") {
            RelayConfig::from_parts(Vec::new(), Some("none"))
        } else {
            RelayConfig::default()
        }
    }

    // ─── serve ───────────────────────────────────────────────────────────────

    async fn serve(args: &[String]) {
        let key = identity(args);
        let admit = flag_value(args, "--admit").map(|h| bytes32_of(&h, "--admit"));

        // The service the gate fronts: plain HTTP on loopback, no iroh types.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind a loopback port for the service");
        let service = listener.local_addr().expect("service address");

        let endpoint = build_relayed_endpoint(key, vec![HELLO_ALPN.to_vec()], &relay_config(args))
            .await
            .unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
        let me = hex::encode(endpoint.id().as_bytes());

        let me_for_service = me.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(hello_service(sock, me_for_service.clone()));
            }
        });

        // The gate. `spawn_admitting_forward` reads the negotiated ALPN and the
        // dialer key the handshake verified ONCE per connection, then decides
        // what the connection may reach — here, an origin that is told the key.
        let _acceptor = IrohAcceptor::spawn_admitting_forward(
            endpoint.clone(),
            move |alpn, dialer| async move {
                let key_hex = hex::encode(dialer.as_bytes());
                if alpn != HELLO_ALPN {
                    println!("refused  {key_hex} — no such protocol");
                    return None;
                }
                if let Some(want) = admit {
                    if *dialer.as_bytes() != want {
                        println!("refused  {key_hex} — not {}", hex::encode(want));
                        return None;
                    }
                }
                println!("admitted {key_hex}");
                Some(Forward::Http {
                    origin: service,
                    headers: vec![("X-Mesh-Pubkey".to_string(), key_hex)],
                })
            },
        );

        let gate = match admit {
            None => "open to any key (--admit <dialer-pubkey> closes it)".to_string(),
            Some(want) => format!("only {}", hex::encode(want)),
        };
        println!("identity {me}");
        println!("gate     {gate}");
        println!("service  http://{service} (loopback only — the tunnel is the way in)");

        let mut last = String::new();
        loop {
            if let Some(ticket) = format_dial_string(&endpoint.addr()) {
                if ticket != last {
                    println!("dial={ticket}");
                    last = ticket;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// What the caller reaches: it reads the request head, prints the mesh
    /// headers the acceptor added, and answers with plain text. It does no auth
    /// of its own — reading `X-Mesh-Pubkey` is the whole check, and the value
    /// is trustworthy only because the gate in front strips client copies.
    async fn hello_service(mut sock: TcpStream, me: String) {
        sock.set_nodelay(true).ok();
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf).await {
                Ok(0) => return,
                Ok(n) => head.extend_from_slice(&buf[..n]),
                Err(_) => return,
            }
            if head.len() > 16 * 1024 {
                return;
            }
        }
        let text = String::from_utf8_lossy(&head);
        let mut arrived = Vec::new();
        for line in text.lines().skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.to_ascii_lowercase().starts_with("x-mesh-") {
                arrived.push(format!("  {}: {}", name.trim(), value.trim()));
            }
        }
        let body = format!(
            "hello from {me}\nmesh headers that actually arrived:\n{}\n",
            if arrived.is_empty() {
                "  (none)".to_string()
            } else {
                arrived.join("\n")
            }
        );
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(reply.as_bytes()).await;
        let _ = sock.shutdown().await;
    }

    // ─── dial ────────────────────────────────────────────────────────────────

    async fn dial(args: &[String]) {
        let Some(ticket) = args.first().filter(|a| !a.starts_with('-')) else {
            eprintln!("dial wants a ticket: hello_mesh dial '<key>@<relay-or-address>[,<...>]'");
            std::process::exit(2);
        };
        let target = parse_dial_string(ticket).unwrap_or_else(|e| {
            eprintln!("bad ticket: {e}");
            std::process::exit(2);
        });
        let target_id = target.id;

        let endpoint = build_relayed_endpoint(identity(args), Vec::new(), &relay_config(args))
            .await
            .unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
        println!("identity {}", hex::encode(endpoint.id().as_bytes()));
        println!("target   {}", hex::encode(target_id.as_bytes()));

        let bridge = HttpBridge::spawn(endpoint.clone(), target, HELLO_ALPN)
            .await
            .expect("open a local bridge to the ticket");
        let mut sock = TcpStream::connect(bridge.local_addr())
            .await
            .expect("connect the bridge");

        // The forged header is the point: the acceptor strips every
        // client-supplied `x-mesh-*` and appends the key the handshake proved,
        // so the service never sees this line.
        let request = "GET /hello HTTP/1.1\r\n\
                       Host: hello-mesh\r\n\
                       X-Mesh-Pubkey: forged-by-the-client\r\n\
                       Connection: close\r\n\r\n";
        sock.write_all(request.as_bytes()).await.expect("send");
        let mut response = Vec::new();
        let _ = sock.read_to_end(&mut response).await;

        if response.is_empty() {
            println!(
                "no answer — the far end closed the connection (its --admit gate refused \
                 this key, or the ticket's addresses do not reach it)"
            );
            report_path(&endpoint, target_id).await;
            std::process::exit(1);
        }
        let text = String::from_utf8_lossy(&response);
        let (reply_head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        println!(
            "{}",
            reply_head.lines().next().unwrap_or("(no status line)")
        );
        print!("{body}");
        report_path(&endpoint, target_id).await;
    }

    /// Which path the QUIC connection actually used — a reading from
    /// `remote_info`, not an inference from timing.
    async fn report_path(endpoint: &Endpoint, peer: PublicKey) {
        let Some(info) = endpoint.remote_info(peer).await else {
            println!("path     unknown (no remote_info for this peer)");
            return;
        };
        let (mut direct, mut relay) = (Vec::new(), Vec::new());
        for addr in info.addrs() {
            if matches!(addr.usage(), TransportAddrUsage::Active) {
                match addr.addr() {
                    TransportAddr::Relay(url) => relay.push(url.to_string()),
                    TransportAddr::Ip(sock) => direct.push(sock.to_string()),
                    _ => {}
                }
            }
        }
        let class = match (!direct.is_empty(), !relay.is_empty()) {
            (true, true) => "mixed",
            (true, false) => "direct",
            (false, true) => "relayed",
            (false, false) => "idle",
        };
        println!(
            "path     {class} (direct=[{}] relay=[{}])",
            direct.join(","),
            relay.join(",")
        );
    }
}
