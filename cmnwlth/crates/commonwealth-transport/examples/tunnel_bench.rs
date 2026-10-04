// SPDX-License-Identifier: AGPL-3.0-or-later
//! tunnel-bench — characterize the iroh byte-tunnel for distributed-inference
//! traffic BEFORE wiring it to the ggml-RPC / rpc-warm paths.
//!
//! The distributed-inference planes this models (QWEN122B_DISTRIBUTED_HANDOFF §6):
//! - **Activation plane** (ggml RPC, latency-bound): per-token request/response.
//!   ~16 KB up (hidden state) with either ~16 KB back (worker returns hidden
//!   state) or ~600 KB back (worker holds the output head → logits).
//! - **Warm plane** (shard range-fetch, bandwidth-bound): bulk one-way streams.
//!
//! Roles (one binary, run on both machines):
//!
//! ```sh
//! # Worker box (e.g. BeefyMac):
//! cargo run --release -p commonwealth-transport --features iroh \
//!     --example tunnel_bench -- serve
//! # → prints  tcp=<lan-ip:port>  and  dial=<endpoint-id>@<relay,addrs…>
//!
//! # Host box (e.g. Strix), raw-TCP LAN baseline vs the tunnel:
//! … --example tunnel_bench -- dial --tcp 192.168.1.2:9799
//! … --example tunnel_bench -- dial --iroh '<dial-string>'
//! ```
//!
//! To measure the RELAY floor (the cross-network worst case), block direct UDP
//! between the two boxes (e.g. `nft add rule inet bench drop udp daddr <peer>`)
//! and re-run the iroh dial: the QUIC path falls back to the relay (TCP/443),
//! which the post-run `path=` line confirms from `remote_info` — stated, not
//! inferred. Add WAN latency to the direct path with `tc qdisc … netem delay`.
//!
//! Protocol (dead simple so it rides raw TCP and the tunnel identically):
//! per request the client writes `[u32 LE send_len][u32 LE want_len]` + payload,
//! the server reads it and writes back exactly `want_len` bytes. One TCP
//! connection for the whole suite — mirrors ggml's persistent RPC connection.

#[cfg(not(feature = "iroh"))]
fn main() {
    eprintln!("tunnel_bench requires --features iroh");
    std::process::exit(2);
}

#[cfg(feature = "iroh")]
#[tokio::main]
async fn main() {
    real_main::run().await;
}

#[cfg(feature = "iroh")]
mod real_main {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use commonwealth_transport::iroh::{
        build_relayed_endpoint, format_dial_string, parse_dial_string, Endpoint, HttpBridge,
        IrohAcceptor, RelayConfig, SecretKey, TransportAddr, TransportAddrUsage,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Dedicated bench ALPN — never collides with the mesh's `cwth/http/0`.
    const BENCH_ALPN: &[u8] = b"cwth/bench/0";

    const DEFAULT_TCP_PORT: u16 = 9799;

    /// Build the bench's iroh endpoint; `--relay-only` (feature
    /// `iroh-relay-only`, BOTH peers) pins data to the relay for the
    /// deterministic relay-floor measurement.
    async fn bench_endpoint(
        key_bytes: [u8; 32],
        alpns: Vec<Vec<u8>>,
        cfg: &RelayConfig,
        relay_only: bool,
    ) -> Endpoint {
        if !relay_only {
            return build_relayed_endpoint(SecretKey::from_bytes(&key_bytes), alpns, cfg)
                .await
                .expect("iroh endpoint bind");
        }
        #[cfg(feature = "iroh-relay-only")]
        {
            println!("relay-only: path selection pinned to the relay (both peers must run this)");
            commonwealth_transport::iroh::build_relay_only_endpoint(
                SecretKey::from_bytes(&key_bytes),
                alpns,
                cfg,
            )
            .await
            .expect("iroh endpoint bind (relay-only)")
        }
        #[cfg(not(feature = "iroh-relay-only"))]
        {
            eprintln!(
                "--relay-only needs a build with --features iroh,iroh-relay-only \
                 (iroh's unstable path-selector API)"
            );
            std::process::exit(2);
        }
    }

    pub async fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("serve") => serve(&args[1..]).await,
            Some("dial") => dial(&args[1..]).await,
            _ => {
                eprintln!(
                    "usage:\n  tunnel_bench serve [--tcp-port N] [--no-n0] [--relay-only]\n  \
                     tunnel_bench dial (--tcp host:port | --iroh <dial-string>) [--quick] [--relay-only]"
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

    // ─── serve ───────────────────────────────────────────────────────────────

    async fn serve(args: &[String]) {
        let tcp_port: u16 = flag_value(args, "--tcp-port")
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TCP_PORT);
        let relay_cfg = if args.iter().any(|a| a == "--no-n0") {
            RelayConfig {
                relay_urls: Vec::new(),
                n0_services: false,
            }
        } else {
            RelayConfig::default()
        };

        // The bench protocol server — the forward target for BOTH the raw-TCP
        // baseline (LAN dial) and the iroh acceptor (loopback forward).
        let listener = TcpListener::bind(("0.0.0.0", tcp_port))
            .await
            .unwrap_or_else(|e| panic!("bind 0.0.0.0:{tcp_port}: {e}"));
        println!("bench server: tcp port {tcp_port} (dial as tcp=<this-box-lan-ip>:{tcp_port})");
        tokio::spawn(async move {
            loop {
                let Ok((sock, peer)) = listener.accept().await else {
                    break;
                };
                println!("bench server: connection from {peer}");
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(sock).await {
                        println!("bench server: connection ended: {e}");
                    }
                });
            }
        });

        // Fresh identity per run — a bench peer, not a mesh member.
        let mut key_bytes = [0u8; 32];
        getrandom::fill(&mut key_bytes).expect("getrandom");
        let relay_only = args.iter().any(|a| a == "--relay-only");
        let endpoint =
            bench_endpoint(key_bytes, vec![BENCH_ALPN.to_vec()], &relay_cfg, relay_only).await;

        let forward: SocketAddr = ([127, 0, 0, 1], tcp_port).into();
        let _acceptor = IrohAcceptor::spawn(endpoint.clone(), forward);
        println!("iroh acceptor: forwarding {} -> {forward}", endpoint.id());

        // Print the dial string now (direct addrs appear at bind) and again
        // once the relay registration lands — the relay-bearing string is the
        // one that works cross-network.
        let mut last = String::new();
        loop {
            if let Some(dial) = format_dial_string(&endpoint.addr()) {
                if dial != last {
                    println!("dial={dial}");
                    last = dial;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// One bench connection: answer `[send_len][want_len]+payload` requests
    /// with `want_len` bytes until the peer closes.
    async fn serve_connection(mut sock: TcpStream) -> std::io::Result<()> {
        sock.set_nodelay(true).ok();
        let mut discard = vec![0u8; 1 << 20];
        let pattern = vec![0xA5u8; 1 << 20];
        loop {
            let mut header = [0u8; 8];
            match sock.read_exact(&mut header).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            }
            let send_len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
            let want_len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            let mut remaining = send_len;
            while remaining > 0 {
                let n = sock.read(&mut discard[..remaining.min(1 << 20)]).await?;
                if n == 0 {
                    return Ok(());
                }
                remaining -= n;
            }
            let mut to_send = want_len;
            while to_send > 0 {
                let n = to_send.min(1 << 20);
                sock.write_all(&pattern[..n]).await?;
                to_send -= n;
            }
            sock.flush().await?;
        }
    }

    // ─── dial ────────────────────────────────────────────────────────────────

    async fn dial(args: &[String]) {
        let quick = args.iter().any(|a| a == "--quick");
        if let Some(addr) = flag_value(args, "--tcp") {
            let sock = TcpStream::connect(&addr)
                .await
                .unwrap_or_else(|e| panic!("connect {addr}: {e}"));
            println!("mode=raw-tcp target={addr}");
            run_suite(sock, quick).await;
            return;
        }
        let Some(dial_str) = flag_value(args, "--iroh") else {
            eprintln!("dial needs --tcp host:port or --iroh <dial-string>");
            std::process::exit(2);
        };
        let relay_only = args.iter().any(|a| a == "--relay-only");
        // Relay-only: seed ONLY relay targets. Seeding direct addrs makes the
        // pin a race — a direct path that validates first (same box ~1ms)
        // becomes current and the selector's no-relay fallback keeps it, so
        // the run silently measures the direct path (observed 2026-07-19).
        let dial_str = if relay_only {
            let (id, targets) = dial_str
                .split_once('@')
                .unwrap_or_else(|| panic!("bad dial string: missing '@'"));
            let relays: Vec<&str> = targets
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty() && t.parse::<std::net::SocketAddr>().is_err())
                .collect();
            if relays.is_empty() {
                eprintln!("--relay-only: dial string has no relay URL target");
                std::process::exit(2);
            }
            println!(
                "relay-only: seeding relay target(s) only ({})",
                relays.join(",")
            );
            format!("{id}@{}", relays.join(","))
        } else {
            dial_str
        };
        let target =
            parse_dial_string(&dial_str).unwrap_or_else(|e| panic!("bad dial string: {e}"));
        let target_id = target.id;

        let mut key_bytes = [0u8; 32];
        getrandom::fill(&mut key_bytes).expect("getrandom");
        let endpoint = bench_endpoint(
            key_bytes,
            Vec::new(), // dial-only: no served ALPNs
            &RelayConfig::default(),
            relay_only,
        )
        .await;

        let bridge = HttpBridge::spawn(endpoint.clone(), target, BENCH_ALPN)
            .await
            .expect("bridge spawn");
        println!(
            "mode=iroh bridge={} target={}",
            bridge.local_addr(),
            target_id
        );
        let sock = TcpStream::connect(bridge.local_addr())
            .await
            .expect("connect to bridge");
        run_suite(sock, quick).await;
        report_path(&endpoint, target_id).await;
    }

    /// Which path QUIC actually used — stated from `remote_info`, not inferred
    /// from the numbers. THE line that distinguishes a direct-path run from a
    /// relay-floor run.
    async fn report_path(endpoint: &Endpoint, id: commonwealth_transport::iroh::PublicKey) {
        let Some(info) = endpoint.remote_info(id).await else {
            println!("path=unknown (no remote_info)");
            return;
        };
        let mut active_direct = Vec::new();
        let mut active_relay = Vec::new();
        for a in info.addrs() {
            let active = matches!(a.usage(), TransportAddrUsage::Active);
            match a.addr() {
                TransportAddr::Relay(url) if active => active_relay.push(url.to_string()),
                TransportAddr::Ip(sa) if active => active_direct.push(sa.to_string()),
                _ => {}
            }
        }
        let class = match (!active_direct.is_empty(), !active_relay.is_empty()) {
            (true, true) => "mixed",
            (true, false) => "direct",
            (false, true) => "relayed",
            (false, false) => "idle",
        };
        println!(
            "path={class} direct=[{}] relay=[{}]",
            active_direct.join(","),
            active_relay.join(",")
        );
    }

    // ─── the suite ───────────────────────────────────────────────────────────

    /// Activation-plane payloads: ~hidden-state up; hidden-state or logits back.
    const ACT: usize = 16 * 1024;
    const LOGITS: usize = 600 * 1024;

    async fn run_suite(mut sock: TcpStream, quick: bool) {
        sock.set_nodelay(true).ok();
        let (rtt_n, act_n, logits_n, bulk_mb, bulk_n) = if quick {
            (50, 30, 10, 16, 2)
        } else {
            (200, 100, 50, 32, 8)
        };

        // Warmup absorbs the bridge's lazy QUIC dial + handshake + slow start.
        for _ in 0..10 {
            request(&mut sock, 64, 64).await;
        }

        let rtt = timed_requests(&mut sock, 64, 64, rtt_n).await;
        print_lat("rtt-64B", &rtt);

        let act = timed_requests(&mut sock, ACT, ACT, act_n).await;
        print_lat("act-16KB/16KB", &act);

        let logits = timed_requests(&mut sock, ACT, LOGITS, logits_n).await;
        print_lat("logits-16KB/600KB", &logits);

        let bulk = bulk_mb * 1024 * 1024;
        let up = timed_requests(&mut sock, bulk, 8, bulk_n).await;
        print_bw("upstream", bulk, &up);
        let down = timed_requests(&mut sock, 8, bulk, bulk_n).await;
        print_bw("downstream", bulk, &down);

        // Decode-ceiling model: one request/response ≈ one per-token boundary
        // crossing pair. tokens/s ceiling = 1000 / p50-ms (network only — the
        // real rate also pays compute; this is the TRANSPORT tax in isolation).
        let p50_act = percentile(&act, 50.0);
        let p50_logits = percentile(&logits, 50.0);
        println!(
            "decode-ceiling: hidden-return={:.1} tok/s logits-return={:.1} tok/s",
            1000.0 / p50_act,
            1000.0 / p50_logits
        );
    }

    async fn request(sock: &mut TcpStream, send_len: usize, want_len: usize) {
        static PAYLOAD: [u8; 1 << 20] = [0x5Au8; 1 << 20];
        let mut header = [0u8; 8];
        header[0..4].copy_from_slice(&(send_len as u32).to_le_bytes());
        header[4..8].copy_from_slice(&(want_len as u32).to_le_bytes());
        sock.write_all(&header).await.expect("write header");
        let mut remaining = send_len;
        while remaining > 0 {
            let n = remaining.min(1 << 20);
            sock.write_all(&PAYLOAD[..n]).await.expect("write payload");
            remaining -= n;
        }
        sock.flush().await.expect("flush");
        let mut sink = vec![0u8; (1 << 20).min(want_len.max(1))];
        let mut to_read = want_len;
        while to_read > 0 {
            let chunk = to_read.min(sink.len());
            let n = sock.read(&mut sink[..chunk]).await.expect("read response");
            assert!(n > 0, "server closed mid-response");
            to_read -= n;
        }
    }

    async fn timed_requests(
        sock: &mut TcpStream,
        send_len: usize,
        want_len: usize,
        count: usize,
    ) -> Vec<f64> {
        let mut ms = Vec::with_capacity(count);
        for _ in 0..count {
            let t = Instant::now();
            request(sock, send_len, want_len).await;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ms
    }

    fn percentile(sorted_input: &[f64], p: f64) -> f64 {
        let mut v = sorted_input.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() {
            return f64::NAN;
        }
        let idx = ((p / 100.0) * (v.len() as f64 - 1.0)).round() as usize;
        v[idx.min(v.len() - 1)]
    }

    fn print_lat(name: &str, ms: &[f64]) {
        println!(
            "{name}: n={} min={:.2}ms p50={:.2}ms p90={:.2}ms p99={:.2}ms",
            ms.len(),
            percentile(ms, 0.0),
            percentile(ms, 50.0),
            percentile(ms, 90.0),
            percentile(ms, 99.0),
        );
    }

    fn print_bw(name: &str, bytes_per_req: usize, ms: &[f64]) {
        let total_bytes = bytes_per_req as f64 * ms.len() as f64;
        let total_secs: f64 = ms.iter().sum::<f64>() / 1000.0;
        println!(
            "{name}: n={} chunk={}MB rate={:.1} MB/s (p50 chunk {:.0}ms)",
            ms.len(),
            bytes_per_req / (1024 * 1024),
            total_bytes / (1024.0 * 1024.0) / total_secs,
            percentile(ms, 50.0),
        );
    }
}
