// SPDX-License-Identifier: AGPL-3.0-or-later
//! media_bridge_bench — can a Jellyfin-shaped video stream ride the existing
//! iroh HTTP bridge, and what can the splice carry?
//!
//! Sibling to [`tunnel_bench`], which characterizes the SAME tunnel for the
//! distributed-inference planes. That example is reused, not duplicated, for
//! everything it already does (dial strings, `--relay-only` pinning, the
//! `path=` line from `remote_info`). What it cannot answer, and this one
//! exists for, is the media customer's three questions:
//!
//! 1. Does an HTTP `Range` request and its `206 Partial Content` reply cross
//!    `HttpBridge`'s `tokio::io::copy` splice BYTE-EXACT? tunnel_bench speaks a
//!    private `[send_len][want_len]` framing, so it proves nothing about HTTP.
//! 2. What SUSTAINED rate does the splice hold, and — the metric that decides
//!    whether a stream is watchable — what is the STALL distribution? A mean
//!    passes a stream nobody can watch (WORK_PLANE.md, the pre-registered bar).
//! 3. What happens with N simultaneous viewers of one peer?
//!
//! Roles (one binary; same-box screening and two-machine both supported):
//!
//! ```sh
//! # 0. make a known file (deterministic contents, verifiable at any offset)
//! …--example media_bridge_bench -- gen --file /tmp/media.bin --size-mb 1024
//!
//! # 1. the origin — a minimal HTTP/1.1 server with real Range support.
//! #    --rate-mbit and --stall-after-mb are the INSTRUMENT VALIDATION knobs:
//! #    a harness that reports the same number whatever you do to the link is
//! #    measuring itself (ARCH §18.4).
//! …-- origin --port 9810 --file /tmp/media.bin [--rate-mbit 5] [--stall-after-mb 8 --stall-ms 3000]
//!
//! # 2a. same box: acceptor + bridge in one process, no n0 contact at all
//! …-- gateway --origin 127.0.0.1:9810          # prints bridge=127.0.0.1:NNNNN
//! # 2b. two machines: `serve` on the holder, `bridge` on the viewer
//! …-- serve  --origin 127.0.0.1:9810 [--relay-only]   # prints dial=…
//! …-- bridge --iroh '<dial>' [--relay-only]           # prints bridge=…
//!
//! # 3. measure through whatever local port step 2 printed
//! …-- pull --addr 127.0.0.1:NNNNN --path /media.bin --verify
//! …-- pull --addr 127.0.0.1:NNNNN --path /media.bin --viewers 4
//!
//! # 4. THE BAR, through the PRODUCT path rather than this binary's own bridge:
//! #    the URL `svrn mesh media <holder>` prints is a bridge the viewer's
//! #    daemon holds over cwth/media/0 to the holder's [iroh] media_origin.
//! #    --url takes it verbatim; --duration-secs re-pulls the same path back to
//! #    back until the clock runs out and judges the pre-registered bar
//! #    (BAR_MBIT / BAR_STALL_MS / BAR_SECS below). Record `svrn mesh media
//! #    --json`'s `path` + `relayed_reading` beside the line: only a relayed
//! #    reading is the bar's kind, and on one LAN the path reads `mixed`.
//! …-- pull --url http://127.0.0.1:NNNNN --path /media.bin --duration-secs 600 --label relayed-run1
//! ```
//!
//! Byte-exactness is ALSO checkable without trusting this binary at all:
//! `curl -r 1000-2999 http://127.0.0.1:NNNNN/media.bin | sha256sum` against
//! `dd`+`sha256sum` on the source file. That is the check the deliverable cites;
//! `--verify` here is the cheap continuous version of the same claim.

#[cfg(not(feature = "iroh"))]
fn main() {
    eprintln!("media_bridge_bench requires --features iroh");
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
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use commonwealth_transport::iroh::{
        build_relayed_endpoint, format_dial_string, parse_dial_string, Endpoint, HttpBridge,
        IrohAcceptor, RelayConfig, SecretKey, TransportAddr, TransportAddrUsage,
    };
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Dedicated ALPN — never collides with the mesh's `cwth/http/0` or with
    /// tunnel_bench's `cwth/bench/0`.
    const BENCH_ALPN: &[u8] = b"cwth/media/0";

    /// Read/write granularity. 256 KiB is large enough that the syscall rate
    /// is not the ceiling and small enough that a stall is still visible as a
    /// gap between arrivals rather than smeared inside one read.
    const CHUNK: usize = 256 * 1024;

    // ─── deterministic file contents ─────────────────────────────────────────

    /// Byte at absolute offset `i`. Position-dependent so ANY range can be
    /// verified without holding the file, and so a splice that reorders,
    /// duplicates or drops a window is caught at the first wrong byte rather
    /// than only in a whole-file digest.
    fn byte_at(i: u64) -> u8 {
        (i.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
            >> 33) as u8
    }

    // ─── entry ───────────────────────────────────────────────────────────────

    pub async fn run() {
        // Glassbox: `RUST_LOG=iroh=debug` on this bench is how the QUIC
        // connection count per viewer is WATCHED rather than inferred. Without
        // a subscriber the env var is a silent no-op and the count reads zero.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_writer(std::io::stderr)
            .try_init();
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("gen") => gen(&args[1..]).await,
            Some("origin") => origin(&args[1..]).await,
            Some("gateway") => gateway(&args[1..]).await,
            Some("serve") => serve(&args[1..]).await,
            Some("bridge") => bridge_role(&args[1..]).await,
            Some("pull") => pull(&args[1..]).await,
            _ => {
                eprintln!(
                    "usage:\n  \
                     gen --file F --size-mb N\n  \
                     origin --port P --file F [--rate-mbit R] [--stall-after-mb M --stall-ms S]\n  \
                     gateway --origin HOST:PORT\n  \
                     serve --origin HOST:PORT [--relay-only] [--no-n0]\n  \
                     bridge --iroh <dial-string> [--relay-only]\n  \
                     pull (--url http://H:P | --addr H:P) --path /P [--range a-b] [--viewers K] \
                     [--verify] [--duration-secs N] [--label L]"
                );
                std::process::exit(2);
            }
        }
    }

    fn flag(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    }
    fn flag_num<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
        flag(args, name).and_then(|v| v.parse().ok())
    }
    fn has(args: &[String], name: &str) -> bool {
        args.iter().any(|a| a == name)
    }

    // ─── gen ─────────────────────────────────────────────────────────────────

    async fn gen(args: &[String]) {
        let path = flag(args, "--file").expect("gen needs --file");
        let size_mb: u64 = flag_num(args, "--size-mb").expect("gen needs --size-mb");
        let total = size_mb * 1024 * 1024;
        let mut f = tokio::fs::File::create(&path).await.expect("create");
        let mut buf = vec![0u8; CHUNK];
        let mut off: u64 = 0;
        while off < total {
            let n = CHUNK.min((total - off) as usize);
            for (k, b) in buf[..n].iter_mut().enumerate() {
                *b = byte_at(off + k as u64);
            }
            f.write_all(&buf[..n]).await.expect("write");
            off += n as u64;
        }
        f.flush().await.expect("flush");
        println!("gen: {path} {total} bytes ({size_mb} MiB)");
    }

    // ─── origin: a minimal HTTP/1.1 server that really does Range ────────────

    #[derive(Clone, Copy)]
    struct Pacing {
        /// Target rate; `None` = as fast as the socket takes it.
        rate_mbit: Option<f64>,
        /// Sleep `stall_ms` once, after this many bytes of body.
        stall_after: Option<u64>,
        stall_ms: u64,
    }

    async fn origin(args: &[String]) {
        let port: u16 = flag_num(args, "--port").expect("origin needs --port");
        let path = flag(args, "--file").expect("origin needs --file");
        let pacing = Pacing {
            rate_mbit: flag_num(args, "--rate-mbit"),
            stall_after: flag_num::<u64>(args, "--stall-after-mb").map(|m| m * 1024 * 1024),
            stall_ms: flag_num(args, "--stall-ms").unwrap_or(0),
        };
        let total = tokio::fs::metadata(&path).await.expect("stat").len();
        let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
        println!(
            "origin: 127.0.0.1:{port} file={path} bytes={total} rate={:?} stall_after={:?}/{}ms",
            pacing.rate_mbit, pacing.stall_after, pacing.stall_ms
        );
        let path = Arc::new(path);
        loop {
            let Ok((sock, _peer)) = listener.accept().await else {
                break;
            };
            let path = Arc::clone(&path);
            tokio::spawn(async move {
                let _ = serve_http(sock, &path, total, pacing).await;
            });
        }
    }

    /// Parse `Range: bytes=a-b` / `bytes=a-` / `bytes=-n` into an inclusive
    /// byte range. `None` for an absent header; `Err` for one that is present
    /// and unsatisfiable — those are different answers and the caller must
    /// distinguish them (416 vs 200).
    fn parse_range(h: &str, total: u64) -> Option<Result<(u64, u64), ()>> {
        let spec = h.strip_prefix("bytes=")?.trim();
        let (a, b) = spec.split_once('-')?;
        let (start, end) = match (a.trim(), b.trim()) {
            ("", "") => return Some(Err(())),
            ("", n) => {
                let n: u64 = n.parse().ok()?;
                if n == 0 || total == 0 {
                    return Some(Err(()));
                }
                (total.saturating_sub(n), total - 1)
            }
            (s, "") => {
                let s: u64 = s.parse().ok()?;
                (s, total.saturating_sub(1))
            }
            (s, e) => {
                let s: u64 = s.parse().ok()?;
                let e: u64 = e.parse().ok()?;
                (s, e.min(total.saturating_sub(1)))
            }
        };
        if start > end || start >= total {
            return Some(Err(()));
        }
        Some(Ok((start, end)))
    }

    async fn serve_http(
        mut sock: TcpStream,
        file: &str,
        total: u64,
        pacing: Pacing,
    ) -> std::io::Result<()> {
        sock.set_nodelay(true).ok();
        // Read request head.
        let mut head = Vec::with_capacity(2048);
        let mut buf = [0u8; 1024];
        loop {
            let n = sock.read(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            head.extend_from_slice(&buf[..n]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if head.len() > 64 * 1024 {
                return Ok(());
            }
        }
        let text = String::from_utf8_lossy(&head).to_string();
        let range_hdr = text
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .map(|l| l[6..].trim().to_string());

        let (status, start, len) = match range_hdr.as_deref().and_then(|h| parse_range(h, total)) {
            Some(Ok((s, e))) => ("206 Partial Content", s, e - s + 1),
            Some(Err(())) => {
                let body = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                sock.write_all(body.as_bytes()).await?;
                return Ok(());
            }
            None => ("200 OK", 0, total),
        };

        let mut hdr = format!(
            "HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\n\
             Content-Length: {len}\r\n"
        );
        if status.starts_with("206") {
            hdr.push_str(&format!(
                "Content-Range: bytes {}-{}/{}\r\n",
                start,
                start + len - 1,
                total
            ));
        }
        hdr.push_str("Connection: close\r\n\r\n");
        sock.write_all(hdr.as_bytes()).await?;

        let mut f = tokio::fs::File::open(file).await?;
        f.seek(std::io::SeekFrom::Start(start)).await?;
        let mut buf = vec![0u8; CHUNK];
        let mut sent: u64 = 0;
        let mut stalled = false;
        let t0 = Instant::now();
        while sent < len {
            let want = CHUNK.min((len - sent) as usize);
            let n = f.read(&mut buf[..want]).await?;
            if n == 0 {
                break;
            }
            sock.write_all(&buf[..n]).await?;
            sent += n as u64;
            if let (false, Some(after)) = (stalled, pacing.stall_after) {
                if sent >= after {
                    stalled = true;
                    sock.flush().await?;
                    tokio::time::sleep(Duration::from_millis(pacing.stall_ms)).await;
                }
            }
            if let Some(mbit) = pacing.rate_mbit {
                let due = Duration::from_secs_f64(sent as f64 * 8.0 / (mbit * 1_000_000.0));
                let elapsed = t0.elapsed();
                if due > elapsed {
                    tokio::time::sleep(due - elapsed).await;
                }
            }
        }
        sock.flush().await?;
        let _ = sock.shutdown().await;
        Ok(())
    }

    // ─── iroh roles ──────────────────────────────────────────────────────────

    async fn endpoint_for(alpns: Vec<Vec<u8>>, cfg: &RelayConfig) -> Endpoint {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("getrandom");
        build_relayed_endpoint(SecretKey::from_bytes(&key), alpns, cfg)
            .await
            .expect("iroh endpoint bind")
    }

    /// Relay-pinned endpoint (feature `iroh-relay-only`). BOTH sides must use
    /// it — path selection is per-side, so a normal peer answers over the
    /// direct path and halves the measured relay tax.
    async fn relay_only_endpoint_for(alpns: Vec<Vec<u8>>, cfg: &RelayConfig) -> Endpoint {
        #[cfg(feature = "iroh-relay-only")]
        {
            let mut key = [0u8; 32];
            getrandom::fill(&mut key).expect("getrandom");
            commonwealth_transport::iroh::build_relay_only_endpoint(
                SecretKey::from_bytes(&key),
                alpns,
                cfg,
            )
            .await
            .expect("iroh endpoint bind (relay-only)")
        }
        #[cfg(not(feature = "iroh-relay-only"))]
        {
            let _ = (alpns, cfg);
            eprintln!(
                "--relay-only needs a build with --features iroh,iroh-relay-only \
                 (iroh's unstable path-selector API)"
            );
            std::process::exit(2);
        }
    }

    /// Drop every IP target from a dial string, keeping only relay URLs.
    /// Seeding direct addrs alongside a relay pin is a RACE: a direct path
    /// that validates first becomes current and the run silently measures the
    /// direct path (tunnel_bench records this happening on 2026-07-19).
    fn relay_targets_only(dial: &str) -> Option<String> {
        let (id, targets) = dial.split_once('@')?;
        let relays: Vec<&str> = targets
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty() && t.parse::<SocketAddr>().is_err())
            .collect();
        if relays.is_empty() {
            return None;
        }
        Some(format!("{id}@{}", relays.join(",")))
    }

    /// Same-box screening: BOTH iroh endpoints in one process, with n0 contact
    /// severed (`presets::Minimal`, relays disabled). Nothing but loopback/LAN
    /// is in the path, which is exactly what a screening test wants — and the
    /// `path=` line still states it rather than leaving it inferred.
    async fn gateway(args: &[String]) {
        let origin: SocketAddr = flag(args, "--origin")
            .expect("gateway needs --origin")
            .parse()
            .expect("bad --origin");
        // Two postures. DEFAULT severs n0 entirely (`presets::Minimal`, relays
        // disabled) so nothing but loopback is in the path — the screening
        // posture. `--relay-only` does the opposite: n0 relays ON and both
        // sides path-pinned to the relay, so the bytes really do cross the
        // WAN to the relay and back. That is NOT the pre-registered bar (one
        // host, one uplink, not two networks), but it bounds the relay leg the
        // bar depends on.
        let relay_only = has(args, "--relay-only");
        let cfg = if relay_only {
            RelayConfig::default()
        } else {
            RelayConfig {
                relay_urls: Vec::new(),
                n0_services: false,
            }
        };
        let mk = |alpns: Vec<Vec<u8>>| async move {
            if relay_only {
                relay_only_endpoint_for(alpns, &RelayConfig::default()).await
            } else {
                endpoint_for(
                    alpns,
                    &RelayConfig {
                        relay_urls: Vec::new(),
                        n0_services: false,
                    },
                )
                .await
            }
        };
        let _ = &cfg;
        let server = mk(vec![BENCH_ALPN.to_vec()]).await;
        let _acceptor = IrohAcceptor::spawn(server.clone(), origin);
        let server_id = server.id();
        // Wait for a dialable target: a relay URL under --relay-only (and
        // ONLY a relay URL), otherwise any direct address.
        let target = loop {
            let addr = server.addr();
            if relay_only {
                if let Some(dial) = format_dial_string(&addr)
                    .as_deref()
                    .and_then(relay_targets_only)
                {
                    println!("relay-only: seeding relay target only ({dial})");
                    break parse_dial_string(&dial).expect("relay-only dial string");
                }
            } else if addr.ip_addrs().next().is_some() {
                break addr;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let client = mk(Vec::new()).await;
        let bridge = HttpBridge::spawn(client.clone(), target, BENCH_ALPN)
            .await
            .expect("bridge spawn");
        println!(
            "bridge={} origin={origin} peer={server_id}",
            bridge.local_addr()
        );
        println!("ready");
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            report_path(&client, server_id).await;
        }
    }

    async fn serve(args: &[String]) {
        let origin: SocketAddr = flag(args, "--origin")
            .expect("serve needs --origin")
            .parse()
            .expect("bad --origin");
        let cfg = if has(args, "--no-n0") {
            RelayConfig {
                relay_urls: Vec::new(),
                n0_services: false,
            }
        } else {
            RelayConfig::default()
        };
        let ep = endpoint_for(vec![BENCH_ALPN.to_vec()], &cfg).await;
        let _acceptor = IrohAcceptor::spawn(ep.clone(), origin);
        println!("iroh acceptor: {} -> {origin}", ep.id());
        let mut last = String::new();
        loop {
            if let Some(dial) = format_dial_string(&ep.addr()) {
                if dial != last {
                    println!("dial={dial}");
                    last = dial;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn bridge_role(args: &[String]) {
        let relay_only = has(args, "--relay-only");
        let mut dial_str = flag(args, "--iroh").expect("bridge needs --iroh");
        if relay_only {
            // Same reasoning as tunnel_bench: seeding direct addrs makes the
            // pin a race, and the run silently measures the direct path.
            let (id, targets) = dial_str.split_once('@').expect("bad dial string");
            let relays: Vec<&str> = targets
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty() && t.parse::<SocketAddr>().is_err())
                .collect();
            assert!(
                !relays.is_empty(),
                "--relay-only: no relay URL in dial string"
            );
            println!(
                "relay-only: seeding relay target(s) only ({})",
                relays.join(",")
            );
            dial_str = format!("{id}@{}", relays.join(","));
        }
        let target = parse_dial_string(&dial_str).expect("bad dial string");
        let target_id = target.id;
        let ep = endpoint_for(Vec::new(), &RelayConfig::default()).await;
        let bridge = HttpBridge::spawn(ep.clone(), target, BENCH_ALPN)
            .await
            .expect("bridge spawn");
        println!("bridge={} peer={target_id}", bridge.local_addr());
        println!("ready");
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            report_path(&ep, target_id).await;
        }
    }

    /// Which path QUIC actually used — stated from `remote_info`, never
    /// inferred from the numbers. Same shape as tunnel_bench's.
    async fn report_path(ep: &Endpoint, id: commonwealth_transport::iroh::PublicKey) {
        let Some(info) = ep.remote_info(id).await else {
            println!("path=unknown (no remote_info)");
            return;
        };
        let (mut direct, mut relay) = (Vec::new(), Vec::new());
        for a in info.addrs() {
            if !matches!(a.usage(), TransportAddrUsage::Active) {
                continue;
            }
            match a.addr() {
                TransportAddr::Relay(url) => relay.push(url.to_string()),
                TransportAddr::Ip(sa) => direct.push(sa.to_string()),
                _ => {}
            }
        }
        let class = match (!direct.is_empty(), !relay.is_empty()) {
            (true, true) => "mixed",
            (true, false) => "direct",
            (false, true) => "relayed",
            (false, false) => "idle",
        };
        println!(
            "path={class} direct=[{}] relay=[{}]",
            direct.join(","),
            relay.join(",")
        );
    }

    // ─── pull: the measuring client ──────────────────────────────────────────

    struct Reading {
        status: u16,
        content_length: Option<u64>,
        content_range: Option<String>,
        body_bytes: u64,
        ttfb_ms: f64,
        secs: f64,
        /// Gap in ms between consecutive body-bearing reads.
        gaps_ms: Vec<f64>,
        /// Bytes arriving in each whole second of the transfer.
        per_sec: Vec<u64>,
        verify_fail_at: Option<u64>,
    }

    async fn fetch(
        addr: SocketAddr,
        path: &str,
        range: Option<(u64, u64)>,
        verify: bool,
    ) -> std::io::Result<Reading> {
        let mut sock = TcpStream::connect(addr).await?;
        sock.set_nodelay(true).ok();
        let mut req =
            format!("GET {path} HTTP/1.1\r\nHost: bench\r\nUser-Agent: media_bridge_bench\r\n");
        if let Some((a, b)) = range {
            req.push_str(&format!("Range: bytes={a}-{b}\r\n"));
        }
        req.push_str("Connection: close\r\n\r\n");
        let t_req = Instant::now();
        sock.write_all(req.as_bytes()).await?;
        sock.flush().await?;

        let mut buf = vec![0u8; CHUNK];
        let mut head = Vec::new();
        let mut head_done = None::<usize>;
        let mut body_bytes: u64 = 0;
        let mut gaps = Vec::new();
        let mut per_sec: Vec<u64> = Vec::new();
        let mut ttfb_ms = f64::NAN;
        let mut t_body0 = None::<Instant>;
        let mut t_last = None::<Instant>;
        let base = range.map(|(a, _)| a).unwrap_or(0);
        let mut verify_fail_at = None;

        loop {
            let n = sock.read(&mut buf).await?;
            let now = Instant::now();
            if n == 0 {
                break;
            }
            let mut body_slice: &[u8] = &[];
            if head_done.is_none() {
                head.extend_from_slice(&buf[..n]);
                if let Some(pos) = head
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4)
                {
                    head_done = Some(pos);
                    let carry = head.len() - pos;
                    if carry > 0 {
                        body_slice = &head[pos..];
                    }
                }
            } else {
                body_slice = &buf[..n];
            }
            if body_slice.is_empty() {
                continue;
            }
            if t_body0.is_none() {
                t_body0 = Some(now);
                ttfb_ms = (now - t_req).as_secs_f64() * 1000.0;
            }
            if let Some(prev) = t_last {
                gaps.push((now - prev).as_secs_f64() * 1000.0);
            }
            t_last = Some(now);
            if verify && verify_fail_at.is_none() {
                for (k, b) in body_slice.iter().enumerate() {
                    let off = base + body_bytes + k as u64;
                    if *b != byte_at(off) {
                        verify_fail_at = Some(off);
                        break;
                    }
                }
            }
            let bucket = (now - t_body0.unwrap()).as_secs() as usize;
            if per_sec.len() <= bucket {
                per_sec.resize(bucket + 1, 0);
            }
            per_sec[bucket] += body_slice.len() as u64;
            body_bytes += body_slice.len() as u64;
        }

        let text = String::from_utf8_lossy(&head[..head_done.unwrap_or(head.len())]).to_string();
        let status = text
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let hget = |name: &str| -> Option<String> {
            let want = format!("{}:", name.to_ascii_lowercase());
            text.lines()
                .find(|l| l.to_ascii_lowercase().starts_with(&want))
                .map(|l| l[want.len()..].trim().to_string())
        };
        Ok(Reading {
            status,
            content_length: hget("content-length").and_then(|v| v.parse().ok()),
            content_range: hget("content-range"),
            body_bytes,
            ttfb_ms,
            secs: t_body0.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0),
            gaps_ms: gaps,
            per_sec,
            verify_fail_at,
        })
    }

    fn pct(v: &[f64], p: f64) -> f64 {
        if v.is_empty() {
            return f64::NAN;
        }
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let i = ((p / 100.0) * (s.len() as f64 - 1.0)).round() as usize;
        s[i.min(s.len() - 1)]
    }

    /// The pre-registered bar (WORK_PLANE.md, federated media): one stream
    /// sustains this rate for this long with no stall over this gap, on the
    /// RELAYED path, over ≥3 runs. Held here so the verdict line and the doc
    /// cannot drift apart; the path KIND is not this binary's to know — it
    /// comes from `svrn mesh media --json` (`relayed_reading`).
    const BAR_MBIT: f64 = 25.0;
    const BAR_STALL_MS: f64 = 2000.0;
    const BAR_SECS: f64 = 600.0;

    /// Pull `path` from `addr` back to back until `deadline`, folding every
    /// fetch into one reading. Gaps are measured WITHIN fetches only — the
    /// reconnect between two is this harness's artefact, not the stream's —
    /// and the reconnect count is reported beside it so it is never hidden.
    async fn fetch_until(
        addr: SocketAddr,
        path: &str,
        range: Option<(u64, u64)>,
        verify: bool,
        deadline: Option<Instant>,
    ) -> std::io::Result<(Reading, usize)> {
        let t0 = Instant::now();
        let mut acc = fetch(addr, path, range, verify).await?;
        let mut reconnects = 0usize;
        while let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
            let r = fetch(addr, path, range, verify).await?;
            reconnects += 1;
            acc.body_bytes += r.body_bytes;
            acc.gaps_ms.extend(r.gaps_ms);
            // Each fetch's LAST bucket is a partial second by construction; the
            // report drops only the final one, so folding them in as whole
            // seconds read as stalls that never happened (watched: worst_1s
            // 2.4 Mbit/s on a 951 Mbit/s pull, 29 fetches in). Drop the
            // completed fetch's trailing bucket before appending the next.
            acc.per_sec.pop();
            acc.per_sec.extend(r.per_sec);
            acc.verify_fail_at = acc.verify_fail_at.or(r.verify_fail_at);
            acc.status = r.status;
        }
        if deadline.is_some() {
            acc.secs = t0.elapsed().as_secs_f64();
        }
        Ok((acc, reconnects))
    }

    async fn pull(args: &[String]) {
        // `--url` is the form the product prints (`svrn mesh media`), taken
        // verbatim so the number and the demo share one path; `--addr` is the
        // bench's own bridge line.
        let addr: SocketAddr = match (flag(args, "--url"), flag(args, "--addr")) {
            (Some(u), _) => u
                .trim_end_matches('/')
                .strip_prefix("http://")
                .expect("--url must be http://host:port, as `svrn mesh media` prints it")
                .parse()
                .expect("bad --url authority"),
            (None, Some(a)) => a.parse().expect("bad --addr"),
            (None, None) => panic!("pull needs --url (from `svrn mesh media`) or --addr"),
        };
        let path = flag(args, "--path").unwrap_or_else(|| "/".to_string());
        let duration_secs: Option<f64> = flag_num(args, "--duration-secs");
        let deadline = duration_secs.map(|d| Instant::now() + Duration::from_secs_f64(d));
        let range = flag(args, "--range").map(|r| {
            let (a, b) = r.split_once('-').expect("--range a-b");
            (
                a.parse().expect("range start"),
                b.parse().expect("range end"),
            )
        });
        let viewers: usize = flag_num(args, "--viewers").unwrap_or(1);
        let verify = has(args, "--verify");
        let label = flag(args, "--label").unwrap_or_else(|| "run".to_string());

        let mut tasks = Vec::new();
        let t0 = Instant::now();
        for v in 0..viewers {
            let path = path.clone();
            tasks.push(tokio::spawn(async move {
                (v, fetch_until(addr, &path, range, verify, deadline).await)
            }));
        }
        let mut oks = Vec::new();
        for t in tasks {
            let (v, r) = t.await.expect("join");
            match r {
                Ok((r, reconnects)) => oks.push((v, r, reconnects)),
                Err(e) => println!("viewer {v}: ERROR {e}"),
            }
        }
        let wall = t0.elapsed().as_secs_f64();
        oks.sort_by_key(|(v, _, _)| *v);
        let mut agg_bytes = 0u64;
        for (v, r, reconnects) in &oks {
            let mbit = if r.secs > 0.0 {
                r.body_bytes as f64 * 8.0 / r.secs / 1_000_000.0
            } else {
                f64::NAN
            };
            agg_bytes += r.body_bytes;
            let stall_2s = r.gaps_ms.iter().filter(|g| **g > 2000.0).count();
            let stall_1s = r.gaps_ms.iter().filter(|g| **g > 1000.0).count();
            let stall_500 = r.gaps_ms.iter().filter(|g| **g > 500.0).count();
            // The watchability metric: the WORST whole second of the transfer.
            // Ignore the final partial second, which is short by construction.
            let worst_sec = if r.per_sec.len() > 2 {
                r.per_sec[..r.per_sec.len() - 1]
                    .iter()
                    .copied()
                    .min()
                    .unwrap_or(0)
            } else {
                0
            };
            println!(
                "{label} viewer={v} status={} clen={:?} crange={:?} bytes={} secs={:.2} \
                 rate={:.1} Mbit/s ttfb={:.1}ms gaps: n={} p50={:.1} p99={:.1} max={:.1}ms \
                 stalls>500ms={} >1s={} >2s={} worst_1s={:.1} Mbit/s verify={} reconnects={}",
                r.status,
                r.content_length,
                r.content_range,
                r.body_bytes,
                r.secs,
                mbit,
                r.ttfb_ms,
                r.gaps_ms.len(),
                pct(&r.gaps_ms, 50.0),
                pct(&r.gaps_ms, 99.0),
                pct(&r.gaps_ms, 100.0),
                stall_500,
                stall_1s,
                stall_2s,
                worst_sec as f64 * 8.0 / 1_000_000.0,
                match r.verify_fail_at {
                    None if verify => "OK".to_string(),
                    None => "off".to_string(),
                    Some(off) => format!("MISMATCH@{off}"),
                },
                reconnects
            );
            if let Some(d) = duration_secs {
                // The bar, judged from the numbers above and nothing else. The
                // path KIND is deliberately absent: this binary cannot see the
                // daemon's endpoint, and a verdict that assumed one would be
                // the LAN-as-relay substitution the bar exists to refuse.
                let ran_the_bar = d >= BAR_SECS && r.secs >= BAR_SECS;
                let rate_ok = mbit >= BAR_MBIT;
                let over = r.gaps_ms.iter().filter(|g| **g > BAR_STALL_MS).count();
                let verdict = if !ran_the_bar {
                    format!(
                        "could-not-judge (ran {:.0}s of the bar's {BAR_SECS:.0}s)",
                        r.secs
                    )
                } else if rate_ok && over == 0 {
                    "met".to_string()
                } else {
                    "NOT met".to_string()
                };
                println!(
                    "{label} viewer={v} BAR rate>={BAR_MBIT} Mbit/s: {} ({mbit:.1}) · stalls>{BAR_STALL_MS:.0}ms: {over} · {:.0}s of {BAR_SECS:.0}s · verdict={verdict} · path kind: `svrn mesh media --json` .path.relayed_reading must be true for this to be the bar's reading",
                    if rate_ok { "yes" } else { "NO" },
                    r.secs,
                );
            }
        }
        if viewers > 1 {
            println!(
                "{label} AGGREGATE viewers={viewers} bytes={agg_bytes} wall={wall:.2}s \
                 rate={:.1} Mbit/s",
                agg_bytes as f64 * 8.0 / wall / 1_000_000.0
            );
        }
    }
}
