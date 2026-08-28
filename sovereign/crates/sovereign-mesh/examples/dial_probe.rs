// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dial a node by key on a chosen ALPN and report what it does with you.
//!
//! The instrument the two-machine claims need and nothing else provides. The
//! interesting assertions about `AcceptorRoutes::forward_for` are all of the
//! form "who you are decides where you land", and no shipped CLI lets you
//! choose who you are: `svrn mesh use` always dials `GUEST_ALPN`, and peer
//! traffic always dials from the node's own mesh identity.
//!
//! By default it dials from a FRESH RANDOM key — a stranger, which is the
//! subject of the security bars. `--key <64-hex>` dials as a specific
//! identity, so the member arm can be exercised too.
//!
//! ```text
//! cargo run -p sovereign-mesh --example dial_probe -- \
//!     --dial '<hex-id>@https://relay…,192.168.1.3:54187' \
//!     --alpn client --path /v1/models
//! ```
//!
//! Exit status is 0 whenever the probe RAN — the HTTP status is the result,
//! not the exit code, because "401" is a pass for some bars and a fail for
//! others. A non-zero exit means the probe itself could not be performed.

use std::time::Duration;

use commonwealth_transport::iroh::{
    build_relayed_endpoint, parse_dial_string, HttpBridge, RelayConfig, SecretKey, ALPN,
    CLIENT_ALPN, GUEST_ALPN, RPC_ALPN,
};

fn usage() -> ! {
    eprintln!(
        "usage: dial_probe --dial <string> [--alpn client|guest|rpc|internal] \\\n\
        \x20                 [--path /v1/models] [--bearer <token>] [--key <64-hex>]\n\
        \n\
        --alpn defaults to `client`. --key defaults to a fresh random identity\n\
        (a stranger). Relay posture comes from this host's [iroh] config."
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut dial = None;
    let mut alpn_name = "client".to_string();
    let mut path = "/v1/models".to_string();
    let mut bearer: Option<String> = None;
    let mut key_hex: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let take = |i: &mut usize| -> String {
            *i += 1;
            args.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match args[i].as_str() {
            "--dial" => dial = Some(take(&mut i)),
            "--alpn" => alpn_name = take(&mut i),
            "--path" => path = take(&mut i),
            "--bearer" => bearer = Some(take(&mut i)),
            "--key" => key_hex = Some(take(&mut i)),
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown arg: {other}");
                usage()
            }
        }
        i += 1;
    }
    let Some(dial) = dial else { usage() };

    let alpn: &'static [u8] = match alpn_name.as_str() {
        "client" => CLIENT_ALPN,
        "guest" => GUEST_ALPN,
        "rpc" => RPC_ALPN,
        "internal" => ALPN,
        other => {
            eprintln!("unknown --alpn '{other}' (client|guest|rpc|internal)");
            std::process::exit(2)
        }
    };

    // A fresh key is the default BECAUSE the default question is "what does a
    // stranger get". Naming the identity is the opt-in.
    let (secret, who) = match &key_hex {
        Some(h) => {
            let bytes = match hex::decode(h.trim()) {
                Ok(b) if b.len() == 32 => b,
                _ => {
                    eprintln!("--key must be 64 hex chars (32 bytes)");
                    std::process::exit(2)
                }
            };
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            (SecretKey::from_bytes(&arr), "named key")
        }
        None => (
            SecretKey::from_bytes(&rand::random::<[u8; 32]>()),
            "fresh random key (a stranger)",
        ),
    };
    println!("dialing as: {who}");
    println!("  pubkey:   {}", hex::encode(secret.public().as_bytes()));
    println!("  alpn:     {}", String::from_utf8_lossy(alpn));
    println!("  path:     {path}");
    println!("  bearer:   {}", if bearer.is_some() { "yes" } else { "no" });

    let target = match parse_dial_string(&dial) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bad --dial: {e}");
            std::process::exit(2)
        }
    };

    // This host's own iroh posture, so a probe from a sovereign-mode box does
    // not silently reach for n0 when the daemon beside it would not.
    let (relay_urls, discovery) = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| (c.iroh.relay_urls.clone(), c.iroh.discovery.clone()))
        .unwrap_or_default();
    let relay_cfg = RelayConfig::from_parts(relay_urls, discovery.as_deref());

    let endpoint = match build_relayed_endpoint(secret, vec![alpn.to_vec()], &relay_cfg).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("RESULT: could not bind a local endpoint: {e}");
            std::process::exit(1)
        }
    };
    let bridge = match HttpBridge::spawn(endpoint, target, alpn).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("RESULT: could not open the tunnel: {e}");
            std::process::exit(1)
        }
    };

    let mut req = reqwest::Client::new()
        .get(format!("http://{}{path}", bridge.local_addr()))
        .timeout(Duration::from_secs(20));
    if let Some(b) = &bearer {
        req = req.bearer_auth(b);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let body = body.trim();
            let shown: String = body.chars().take(300).collect();
            println!("\nRESULT: HTTP {status}");
            if !shown.is_empty() {
                println!("BODY:   {shown}");
            }
        }
        Err(e) => {
            // A dead connection IS the expected result for a refused dial, so
            // it is reported as a result rather than as a crash.
            println!("\nRESULT: NO RESPONSE — {e}");
            println!("(for a refused ALPN or a closed connection this is the expected shape)");
        }
    }
}
