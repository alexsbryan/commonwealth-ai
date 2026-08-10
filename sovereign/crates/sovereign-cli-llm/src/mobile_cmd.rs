// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mobile` — opt-in serving of the phone-facing API.
//!
//! The capability is the headless-first sibling of the desktop app's "Mobile
//! access" toggle: it runs `sovereign-server` (the stateful
//! `/v1/conversations` + `/v1/corpora` + WS surface the Tauri mobile client
//! consumes), configured to **delegate all inference to the local daemon** so
//! it loads no models of its own. The shared logic — generating that config
//! from `~/.svrnmesh/config.toml`, persisting a bearer token, locating the
//! binary — lives in [`sovereign_core::mobile_host`]; this file is just the
//! CLI front-end (argument parsing, the pairing card, process launch).
//!
//! ```text
//! svrn mobile serve     # foreground; run on a server / under systemd
//! svrn mobile status    # is the daemon up? is the host listening?
//! svrn mobile pair      # print the pairing card (address + token)
//! ```

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use sovereign_core::mobile_host::{self, MobileHostConfig};
use sovereign_core::setup_config::SetupConfig;

/// Run a `mobile` subcommand. Returns the process exit code.
pub async fn run_mobile(args: &[String]) -> i32 {
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP_MOBILE);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP_MOBILE);
        return 0;
    }

    match args[0].as_str() {
        "serve" => cmd_serve(&args[1..]).await,
        "status" => cmd_status().await,
        "pair" => cmd_pair().await,
        other => {
            eprintln!("Unknown mobile subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP_MOBILE);
            1
        }
    }
}

/// `svrn mobile serve [--bind <addr>]`
///
/// Generate the remote-backed `sovereign-server` config and run it in the
/// foreground (so a service manager supervises one process). On Unix we
/// `exec()`-replace this process with `sovereign-server`, so `serve` *becomes*
/// the host — clean for systemd / launchd.
async fn cmd_serve(args: &[String]) -> i32 {
    // Optional --bind override, persisted so `pair`/`status` agree.
    let mut bind_override: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                match args.get(i) {
                    Some(b) => bind_override = Some(b.clone()),
                    None => {
                        eprintln!("--bind requires an address (e.g. 0.0.0.0:8080)");
                        return 2;
                    }
                }
            }
            other => {
                eprintln!("Unknown flag for `mobile serve`: {other}");
                return 2;
            }
        }
        i += 1;
    }

    let setup = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Mobile host needs a configured node. Run `svrn setup` first.\n  ({e})");
            return 1;
        }
    };

    let mut mh = match MobileHostConfig::load_or_create() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load/create mobile-host settings: {e}");
            return 1;
        }
    };
    if let Some(b) = bind_override {
        mh.bind = b;
        if let Err(e) = mh.save() {
            eprintln!("warning: could not persist --bind override: {e}");
        }
    }

    let config_path = match mobile_host::write_server_config(&setup, &mh) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to write server config: {e}");
            return 1;
        }
    };

    let binary = match mobile_host::resolve_server_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "Could not find the `sovereign-server` binary. Build it \
                 (`cargo build --release -p sovereign-server`) or set \
                 SOVEREIGN_SERVER_PATH."
            );
            return 1;
        }
    };

    // The host forwards every completion + embedding to the daemon; warn loudly
    // if it isn't up, but still launch (the operator may start it momentarily).
    if !daemon_reachable(setup.daemon.client_port) {
        eprintln!(
            "warning: no daemon answering on 127.0.0.1:{} — the mobile host \
             delegates inference to it, so chat/retrieval will fail until \
             `svrn daemon` is running.",
            setup.daemon.client_port
        );
    }

    // The no-VPN code is runtime state — the host hasn't bound yet at
    // this point, so the card says how to fetch it once it's up.
    print_pairing(&mh, resolve_tailnet_addr(&mh.bind), None);
    eprintln!(
        "Starting mobile host: {} --config {}",
        binary.display(),
        config_path.display()
    );
    eprintln!(
        "(inference delegated to daemon :{} — no models loaded here)\n",
        setup.daemon.client_port
    );

    let mut cmd = std::process::Command::new(&binary);
    cmd.arg("--config").arg(&config_path);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec replaces this process image; only returns on failure.
        let err = cmd.exec();
        eprintln!("Failed to exec sovereign-server: {err}");
        1
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("Failed to run sovereign-server: {e}");
                1
            }
        }
    }
}

/// `svrn mobile status` — is the daemon up, and is a host listening?
async fn cmd_status() -> i32 {
    let setup = SetupConfig::load().ok();
    let mh = MobileHostConfig::load_or_create().ok();

    println!("Mobile access");
    match &mh {
        Some(m) => {
            println!(
                "  settings:   {}",
                MobileHostConfig::default_path().display()
            );
            println!("  bind:       {}", m.bind);
            println!("  tenant:     {}", m.tenant);
            println!("  token:      {}", redact(&m.token));
            let listening = mh
                .as_ref()
                .and_then(|m| port_of(&m.bind))
                .map(host_listening)
                .unwrap_or(false);
            println!(
                "  host:       {}",
                if listening {
                    "listening ✓"
                } else {
                    "not running"
                }
            );
        }
        None => println!("  settings:   (none yet — run `svrn mobile pair` or `serve`)"),
    }
    match &setup {
        Some(s) => {
            let up = daemon_reachable(s.daemon.client_port);
            println!(
                "  daemon:     {} (127.0.0.1:{})",
                if up {
                    "up ✓ — inference will ride on it"
                } else {
                    "DOWN — host can't serve chat/retrieval"
                },
                s.daemon.client_port
            );
        }
        None => println!("  daemon:     (no ~/.svrnmesh/config.toml — run `svrn setup`)"),
    }
    0
}

/// `svrn mobile pair` — print the pairing card for the phone.
async fn cmd_pair() -> i32 {
    let mh = match MobileHostConfig::load_or_create() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load/create mobile-host settings: {e}");
            return 1;
        }
    };
    // Best-effort: the no-VPN code is runtime state on the running
    // host (`GET /status` → iroh.dial); absent when the host is down,
    // still reaching its relay, or `iroh_enabled = false`.
    let iroh_dial = if mh.iroh_enabled {
        fetch_iroh_dial(port_of(&mh.bind).unwrap_or(8080)).await
    } else {
        None
    };
    print_pairing(&mh, resolve_tailnet_addr(&mh.bind), iroh_dial);
    0
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Print the card a user types into the app's pairing screen.
fn print_pairing(mh: &MobileHostConfig, address: String, iroh_dial: Option<String>) {
    println!("\n  ┌─ Pair your phone ───────────────────────────────");
    match &iroh_dial {
        Some(dial) => println!("  │  No-VPN code       : {dial}"),
        None if mh.iroh_enabled => {
            println!(
                "  │  No-VPN code       : (host not up yet — re-run `svrn mobile pair` once it is)"
            )
        }
        None => {}
    }
    println!("  │  Address (tailnet) : {address}");
    println!("  │  Tenant            : {}", mh.tenant);
    println!("  │  Token             : {}", mh.token);
    println!("  └─────────────────────────────────────────────────");
    println!("  Enter ONE address in the Sovereign mobile app's \"Connect to your host\" screen.");
    println!("  The no-VPN code works from any network; the tailnet address needs both");
    println!("  phone + this node on the same Tailscale tailnet.\n");
}

/// Read `GET /status` → `iroh.dial` from the running mobile host.
async fn fetch_iroh_dial(port: u16) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let status: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    status
        .pointer("/iroh/dial")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Best-effort: turn the bind into something the phone can dial. Replaces a
/// wildcard host with this node's Tailscale IPv4 (`tailscale ip -4`) when
/// available, else returns the bind unchanged.
fn resolve_tailnet_addr(bind: &str) -> String {
    let port = port_of(bind).unwrap_or(8080);
    let host = bind.rsplit_once(':').map(|(h, _)| h).unwrap_or(bind);
    if host == "0.0.0.0" || host == "::" || host.is_empty() {
        if let Some(ip) = tailscale_ipv4() {
            return format!("{ip}:{port}");
        }
        return format!("<this-node-tailnet-ip>:{port}");
    }
    bind.to_string()
}

/// First IPv4 from `tailscale ip -4`, if the CLI is present and logged in.
fn tailscale_ipv4() -> Option<String> {
    let out = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn daemon_reachable(port: u16) -> bool {
    host_listening_at("127.0.0.1", port)
}

fn host_listening(port: u16) -> bool {
    host_listening_at("127.0.0.1", port)
}

fn host_listening_at(host: &str, port: u16) -> bool {
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(800)).is_ok())
}

fn port_of(bind: &str) -> Option<u16> {
    bind.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}

/// Show enough of the token to recognize it without printing the secret.
fn redact(token: &str) -> String {
    if token.len() <= 14 {
        return "•".repeat(token.len());
    }
    format!("{}…{}", &token[..12], &token[token.len() - 2..])
}

const HELP_MOBILE: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mobile",
    summary: "Serve the phone-facing API, riding on the daemon's resident models (no second load).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn mobile <subcommand> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            (
                "serve [--bind <addr>]",
                "Run the mobile host in the foreground (for a server / systemd). Delegates inference to the local daemon.",
            ),
            (
                "status",
                "Show whether the daemon is up and the host is listening",
            ),
            ("pair", "Print the pairing card (address + tenant + token)"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Requires a configured node (`svrn setup`) and a running `svrn daemon` \
             (the host forwards chat + embeddings to it). Settings + token live in \
             ~/.svrnmesh/mobile-host.toml.",
        ),
    ],
};
