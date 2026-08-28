// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh grant` and `svrn mesh use` — the two ends of an ephemeral mesh
//! link.
//!
//! An invite makes someone a **member**: they receive `mesh_secret`, they
//! gossip, and they can mint further invites. A grant makes them a **guest**:
//! a short-lived bearer, exactly the routes its scope names, exactly the models
//! it lists, revocable in one call. They never enter `Mesh.members`, so "a
//! guest cannot invite people" is not a check anyone had to remember to write.
//!
//! These live outside `mesh_cmd.rs` because that file is long past the §3.1
//! split threshold and is the file a peer session is most likely to be editing.
//! The dispatch there is two arms; everything else is here.
//!
//! # The two honest failure modes, both caught at MINT time
//!
//! A link that cannot work should fail while the operator is still looking at
//! the terminal, not on the guest's first request an hour later:
//!
//! - **A loopback-bound daemon** publishes an address no guest can reach. We
//!   refuse rather than print a link that is inert by construction.
//! - **A model name nothing advertises** produces a grant that 403s on first
//!   use with a message about scope, which sends the guest hunting in the wrong
//!   place. The mint route validates against `dispatchable_ids` for us.

use sovereign_cli_shared::help::{Help, HelpSection};
use sovereign_mesh::deep_link::{build_guest_link, parse_deep_link, DeepLink};

use crate::guest_link::{self, GuestLink};

/// Read the daemon's client port from `SetupConfig` rather than hardcoding
/// 9741 — a sandbox pointed at its own daemon must not mint against the
/// operator's.
fn daemon_client_port() -> u16 {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741)
}

/// What `[daemon] client_bind` resolves to. Loopback here means no guest can
/// reach this node no matter what address the link carries.
fn client_bind() -> String {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_bind)
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

fn bind_is_loopback(bind: &str) -> bool {
    let b = bind.trim();
    if b.eq_ignore_ascii_case("localhost") {
        return true;
    }
    b.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// `2h`, `30m`, `90s`, `1d`, or a bare seconds count.
///
/// Bare digits mean SECONDS, matching the `ttl_secs` field on the wire — one
/// unit, so a `--ttl 30` cannot mean minutes here and seconds there.
fn parse_ttl(raw: &str) -> Result<u64, String> {
    let s = raw.trim();
    let (digits, mult) = if let Some(d) = s.strip_suffix('s') {
        (d, 1)
    } else if let Some(d) = s.strip_suffix('m') {
        (d, 60)
    } else if let Some(d) = s.strip_suffix('h') {
        (d, 3_600)
    } else if let Some(d) = s.strip_suffix('d') {
        (d, 86_400)
    } else {
        (s, 1)
    };
    let n: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("--ttl: expected a duration like 30m, 2h, 1d — got '{raw}'"))?;
    if n == 0 {
        return Err("--ttl must be greater than zero".to_string());
    }
    n.checked_mul(mult)
        .ok_or_else(|| format!("--ttl: '{raw}' overflows"))
}

/// Render a seconds count the way an operator reads a window.
fn human_duration(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3_600)
    } else if secs >= 3_600 {
        format!("{}h{}m", secs / 3_600, (secs % 3_600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Normalise an operator-supplied base URL: scheme optional, `/v1` and
/// trailing slashes trimmed. The link's `url` is a BASE — `RemoteApiProvider`
/// appends `/v1` itself, and a doubled `/v1/v1` is the shape of bug that only
/// shows up on the guest's machine.
fn normalise_base(raw: &str) -> String {
    let s = raw.trim();
    let with_scheme = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("http://{s}")
    };
    with_scheme
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .to_string()
}

/// Where a guest should send the bearer.
///
/// Three sources, most-specific first, and none of them invented here —
/// "what address are we reachable at" already has an owner, and an invite and
/// a guest link disagreeing about it would be the §10.6 failure:
///
/// 1. `--url`, the operator saying it outright.
/// 2. `SOVEREIGN_ADVERTISE_ADDR`, read through
///    `mesh_discovery::read_advertise_addr_override` — the SAME parse the
///    daemon uses to stamp `MemberRecord.addresses`. This is the cloud-peer
///    escape hatch: a containerised daemon's `if-addrs` table lists the Docker
///    bridge before the tailnet, so without it a guest link would carry
///    `172.17.0.3` and be inert.
/// 3. `mesh_discovery::relay_candidates`, which ranks Tailscale over LAN over
///    IPv6 — the same ordering the invite's relay picker uses.
fn guest_base_url(explicit: Option<&str>, client_port: u16) -> Result<String, String> {
    if let Some(u) = explicit {
        return Ok(normalise_base(u));
    }
    if let Some(addr) = sovereign_mesh::mesh_discovery::read_advertise_addr_override(client_port)
        .and_then(|addrs| addrs.into_iter().next())
    {
        return Ok(format!("http://{addr}"));
    }
    let candidates = sovereign_mesh::mesh_discovery::relay_candidates(client_port);
    let picked = candidates
        .iter()
        .find(|c| c.recommended)
        .or_else(|| candidates.first())
        .ok_or_else(|| {
            "this node has no routable address to publish, so a guest link would be inert.\n\
             Connect a network, set SOVEREIGN_ADVERTISE_ADDR, or pass --url <base>."
                .to_string()
        })?;
    Ok(format!("http://{}", picked.url_fragment))
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

/// Pull `error` out of an `ErrorBody` response, falling back to the raw text.
async fn error_text(resp: reqwest::Response) -> String {
    let status = resp.status();
    match resp.text().await {
        Ok(body) => serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| {
                if body.trim().is_empty() {
                    format!("daemon returned {status}")
                } else {
                    body
                }
            }),
        Err(e) => format!("daemon returned {status} and the body would not read: {e}"),
    }
}

// ───────────────────────────── grant (host side) ─────────────────────────────

pub(crate) const HELP_MESH_GRANT: Help = Help {
    command: "svrn mesh grant",
    summary: "Lend named models to someone who is NOT a mesh member, for a bounded window.",
    sections: &[
        HelpSection::Usage(
            "svrn mesh grant --model <id> [--model <id>…] [--ttl 2h] [--label <text>] [--url <base>]\n\
             svrn mesh grant --list\n\
             svrn mesh grant --revoke <token>",
        ),
        HelpSection::Flags(&[
            (
                "--model <id>",
                "A model this grant may dispatch. Repeatable. Exact ids from `/v1/models`.",
            ),
            (
                "--ttl <dur>",
                "Lifetime: 30m, 2h, 1d, or bare seconds. Default 2h, capped at 24h.",
            ),
            ("--label <text>", "Your own note, shown by --list. Never sent to the guest."),
            (
                "--url <base>",
                "Base URL the guest should reach you at. Default: this node's published address.",
            ),
            ("--list", "Show outstanding grants, including revoked and expired ones."),
            ("--revoke <token>", "Kill a link immediately. The token is the one in the link."),
        ]),
        HelpSection::Notes(
            "A guest is not a member: they never receive the mesh secret, never gossip, and\n\
             cannot mint invites or further grants. They may call exactly /v1/models and\n\
             /v1/chat/completions, and only for the models named here.\n\n\
             The daemon must be bound non-loopback for a guest to reach it — set\n\
             `[daemon] client_bind = \"0.0.0.0\"` in ~/.svrnmesh/config.toml.",
        ),
    ],
};

pub(crate) async fn cmd_grant(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_MESH_GRANT);
        return 0;
    }

    let mut models: Vec<String> = Vec::new();
    let mut ttl_secs: Option<u64> = None;
    let mut label: Option<String> = None;
    let mut url_override: Option<String> = None;
    let mut list = false;
    let mut revoke: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                match args.get(i) {
                    Some(v) => models.push(v.clone()),
                    None => {
                        eprintln!("--model needs a model id");
                        return 2;
                    }
                }
            }
            "--ttl" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("--ttl needs a duration");
                    return 2;
                };
                match parse_ttl(v) {
                    Ok(s) => ttl_secs = Some(s),
                    Err(e) => {
                        eprintln!("{e}");
                        return 2;
                    }
                }
            }
            // Both error on a missing value rather than falling back. A
            // trailing `--url` that silently reverted to auto-discovery would
            // publish a different address than the operator typed and say
            // nothing — the substitution this whole surface refuses.
            "--label" => {
                i += 1;
                label = match args.get(i) {
                    Some(v) => Some(v.clone()),
                    None => {
                        eprintln!("--label needs a value");
                        return 2;
                    }
                };
            }
            "--url" => {
                i += 1;
                url_override = match args.get(i) {
                    Some(v) => Some(v.clone()),
                    None => {
                        eprintln!("--url needs a base URL");
                        return 2;
                    }
                };
            }
            "--list" => list = true,
            "--revoke" => {
                i += 1;
                revoke = args.get(i).cloned();
                if revoke.is_none() {
                    eprintln!("--revoke needs the token from the link");
                    return 2;
                }
            }
            other => {
                eprintln!("Unknown arg: {other}");
                eprintln!("Run `svrn mesh grant --help`.");
                return 2;
            }
        }
        i += 1;
    }

    let port = daemon_client_port();
    if !crate::mesh_cmd::daemon_listening_on(port).await {
        eprintln!("No daemon detected on :{port} — minting a grant needs one.");
        eprintln!("Start it with `svrn daemon start`, then re-run.");
        return 1;
    }

    if list {
        return grant_list(port).await;
    }
    if let Some(token) = revoke {
        return grant_revoke(port, &token).await;
    }

    if models.is_empty() {
        eprintln!("A grant must name at least one model: --model <id>");
        eprintln!("`svrn mesh grant --list` shows what is already outstanding.");
        return 2;
    }

    // The inert-link check. A loopback-bound daemon cannot serve a guest at
    // all, so a link minted here would fail on the guest's first request with
    // a connection error that says nothing about the real cause. An explicit
    // --url is the escape hatch for an operator fronting the daemon with a
    // proxy or tunnel — that is a claim only they can make, so we warn rather
    // than refuse.
    let bind = client_bind();
    if bind_is_loopback(&bind) {
        if url_override.is_none() {
            eprintln!("This daemon binds {bind} — loopback only, so no guest can reach it.");
            eprintln!();
            eprintln!("Set `[daemon] client_bind = \"0.0.0.0\"` in ~/.svrnmesh/config.toml and");
            eprintln!("restart the daemon, or pass --url <base> if something else fronts it.");
            return 1;
        }
        eprintln!(
            "(warning: client_bind is {bind} — loopback. Trusting --url; the guest reaches you \
             only if something else forwards to this port.)"
        );
    }

    let base_url = match guest_base_url(url_override.as_deref(), port) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let client = match http_client(15) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let body = serde_json::json!({
        "scopes": { "models": models },
        "ttl_secs": ttl_secs,
        "label": label,
    });
    let url = format!("http://127.0.0.1:{port}/internal/guest/grant");
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach the daemon at {url}: {e}");
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!("Could not mint the grant: {}", error_text(resp).await);
        return 1;
    }
    let payload: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned a non-JSON grant response: {e}");
            return 1;
        }
    };
    let (Some(token), Some(expires_at_ms)) = (
        payload.get("token").and_then(|v| v.as_str()),
        payload.get("expires_at_ms").and_then(|v| v.as_u64()),
    ) else {
        eprintln!("Daemon's grant response was missing `token` or `expires_at_ms`.");
        return 1;
    };
    let summary = payload
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // The link carries SECONDS (`exp=`), the store milliseconds. One
    // conversion, here, so the two never disagree about when the window shuts.
    let expires_at_secs = expires_at_ms / 1_000;
    let link = build_guest_link(
        token,
        &base_url,
        expires_at_secs,
        (!summary.is_empty()).then_some(summary),
    );

    let now = guest_link::now_secs();
    println!();
    println!("Guest link minted.");
    println!();
    println!("  Grants:   {summary}");
    println!("  Reach at: {base_url}");
    println!(
        "  Expires:  in {} (unix {expires_at_secs})",
        human_duration(expires_at_secs.saturating_sub(now))
    );
    println!();
    println!("Send them this, and have them run:");
    println!();
    println!("  svrn mesh use '{link}'");
    println!();
    println!("Revoke at any time with:");
    println!();
    println!("  svrn mesh grant --revoke {token}");
    println!();
    0
}

async fn grant_list(port: u16) -> i32 {
    let client = match http_client(10) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let url = format!("http://127.0.0.1:{port}/internal/guest/grant/list");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach the daemon at {url}: {e}");
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!("Could not list grants: {}", error_text(resp).await);
        return 1;
    }
    let rows: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned a non-JSON grant list: {e}");
            return 1;
        }
    };
    if rows.is_empty() {
        println!("No guest grants outstanding.");
        return 0;
    }
    let now = guest_link::now_secs();
    println!();
    println!(
        "{:<10}  {:<8}  {:<10}  {:<28}  {}",
        "TOKEN", "STATE", "EXPIRES", "GRANTS", "LABEL"
    );
    for row in &rows {
        let prefix = row
            .get("token_prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let labelled = row.get("label").and_then(|v| v.as_str()).unwrap_or("");
        let exp_secs = row
            .get("expires_at_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            / 1_000;
        // Revoked and expired are DIFFERENT states and are reported as such —
        // "I revoked that, right?" is the question this surface answers, and a
        // list that collapses them into "dead" cannot.
        let state = if row.get("revoked").and_then(|v| v.as_bool()) == Some(true) {
            "revoked"
        } else if row.get("live").and_then(|v| v.as_bool()) == Some(true) {
            "live"
        } else {
            "expired"
        };
        let when = if state == "live" {
            human_duration(exp_secs.saturating_sub(now))
        } else {
            "-".to_string()
        };
        println!("{prefix:<10}  {state:<8}  {when:<10}  {summary:<28}  {labelled}");
    }
    println!();
    0
}

async fn grant_revoke(port: u16, token: &str) -> i32 {
    let client = match http_client(10) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let url = format!("http://127.0.0.1:{port}/internal/guest/grant/revoke");
    let resp = match client
        .post(&url)
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach the daemon at {url}: {e}");
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!("Could not revoke: {}", error_text(resp).await);
        return 1;
    }
    let payload: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    // Revoking something that was never there is reported as such rather than
    // as success — an operator who mistyped a token must not walk away
    // believing a live link is dead.
    if payload.get("revoked").and_then(|v| v.as_bool()) == Some(true) {
        println!("Revoked. The next request bearing that token is refused.");
        0
    } else {
        eprintln!("No grant with that token — nothing was revoked.");
        eprintln!("`svrn mesh grant --list` shows the outstanding ones by prefix.");
        1
    }
}

// ───────────────────────────── use (guest side) ──────────────────────────────

pub(crate) const HELP_MESH_USE: Help = Help {
    command: "svrn mesh use",
    summary: "Accept a guest link — `svrn chat` then routes to the issuing node.",
    sections: &[
        HelpSection::Usage(
            "svrn mesh use <sovereign://guest/…>\n\
             svrn mesh use --status\n\
             svrn mesh use --forget",
        ),
        HelpSection::Flags(&[
            ("--status", "Show the link currently in effect, if any."),
            (
                "--forget",
                "Drop the stored link and go back to your own daemon.",
            ),
            (
                "--no-verify",
                "Store the link without first checking that the issuing node answers.",
            ),
        ]),
        HelpSection::Notes(
            "This does NOT join a mesh. You stay outside it: no mesh secret, no gossip, no\n\
             membership. The link expires on its own, and the issuing node can revoke it at\n\
             any moment.\n\n\
             While a link is in effect, `svrn chat` sends its completions to the issuing\n\
             node instead of your local daemon and says so on stderr.",
        ),
    ],
};

pub(crate) async fn cmd_use(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_MESH_USE);
        return 0;
    }

    let mut link_arg: Option<String> = None;
    let mut forget = false;
    let mut status = false;
    let mut no_verify = false;
    for arg in args {
        match arg.as_str() {
            "--forget" => forget = true,
            "--status" => status = true,
            "--no-verify" => no_verify = true,
            other if other.starts_with('-') => {
                eprintln!("Unknown arg: {other}");
                eprintln!("Run `svrn mesh use --help`.");
                return 2;
            }
            other => link_arg = Some(other.to_string()),
        }
    }

    if forget {
        return match guest_link::forget() {
            Ok(true) => {
                println!("Guest link dropped. `svrn chat` is back on your own daemon.");
                0
            }
            Ok(false) => {
                println!("No guest link was stored.");
                0
            }
            Err(e) => {
                eprintln!("Could not remove {}: {e}", guest_link::path().display());
                1
            }
        };
    }

    if status || link_arg.is_none() {
        let now = guest_link::now_secs();
        // Deliberately `load`, not `load_live`: `--status` must be able to
        // SHOW an expired link. An accessor that hides the thing the operator
        // is asking about answers a different question.
        return match guest_link::load() {
            Some(l) => {
                println!();
                println!("  Host:     {}", l.url);
                println!(
                    "  Grants:   {}",
                    l.summary.as_deref().unwrap_or("(not stated)")
                );
                match l.remaining_secs(now) {
                    Some(rem) => println!("  Expires:  in {}", human_duration(rem)),
                    None => println!("  Expires:  EXPIRED — `svrn mesh use --forget` to clear it"),
                }
                println!("  Stored:   {}", guest_link::path().display());
                println!();
                0
            }
            None => {
                if status {
                    println!("No guest link in effect — `svrn chat` uses your own daemon.");
                    0
                } else {
                    eprintln!("Missing link.");
                    eprintln!("Usage: svrn mesh use <sovereign://guest/…>");
                    1
                }
            }
        };
    }

    let raw = link_arg.expect("checked above");
    // Matched exhaustively rather than with a `Guest`-or-bust `if let`: a JOIN
    // link arriving here is the mistake a first-time user actually makes, and
    // it deserves its own message. A third `DeepLink` variant added later is a
    // compile error at this arm rather than a silently-refused link.
    let stored = match parse_deep_link(&raw) {
        Some(DeepLink::Guest {
            token,
            url,
            expires_at,
            summary,
        }) => GuestLink {
            token,
            url,
            expires_at,
            summary,
        },
        Some(DeepLink::Join { .. }) => {
            eprintln!("That is a JOIN link, not a guest link.");
            eprintln!("It would make you a mesh MEMBER — run `svrn mesh join {raw}` if that is");
            eprintln!("what you meant. A guest link looks like sovereign://guest/<token>?url=…");
            return 1;
        }
        None => {
            eprintln!("Not a usable guest link: {raw}");
            eprintln!("Expected sovereign://guest/<token>?url=<base>&exp=<unix-seconds>");
            return 1;
        }
    };

    let now = guest_link::now_secs();
    if now >= stored.expires_at {
        eprintln!("That link has already expired. Ask for a fresh one.");
        return 1;
    }

    if !no_verify {
        // Prove the link works BEFORE it takes over `svrn chat`'s routing. A
        // stored-but-dead link is worse than no link: every subsequent chat
        // silently aims at an unreachable host, and the error surfaces three
        // layers down in bootstrap.
        match verify_link(&stored).await {
            Ok(models) => {
                println!();
                println!("Verified against {} — models in scope:", stored.url);
                for m in &models {
                    println!("  {m}");
                }
            }
            Err(e) => {
                eprintln!("The issuing node did not accept this link: {e}");
                eprintln!();
                eprintln!("Nothing was stored. Re-run with --no-verify to keep it anyway.");
                return 1;
            }
        }
    }

    if let Err(e) = guest_link::save(&stored) {
        eprintln!("Could not write {}: {e}", guest_link::path().display());
        return 1;
    }

    println!();
    println!("Guest link accepted.");
    println!(
        "  {} for the next {}",
        stored.summary.as_deref().unwrap_or("(scope not stated)"),
        human_duration(stored.expires_at.saturating_sub(now))
    );
    println!();
    println!(
        "  svrn chat ask \"hello\"      # now served by {}",
        stored.url
    );
    println!("  svrn mesh use --forget     # go back to your own daemon");
    println!();
    0
}

/// GET `<url>/v1/models` with the bearer. Returns the ids the grant exposes.
///
/// `/v1/models` is in `Scope::Models`'s own path set, so this needs no
/// privilege the link did not already carry — and the host filters the listing
/// to the granted ids, which is exactly what we want to print back.
async fn verify_link(link: &GuestLink) -> Result<Vec<String>, String> {
    let client = http_client(10)?;
    let url = format!("{}/v1/models", link.url);
    let resp = client
        .get(&url)
        .bearer_auth(&link.token)
        .send()
        .await
        .map_err(|e| format!("{url} is unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("{url} returned a non-JSON body: {e}"))?;
    let ids: Vec<String> = payload
        .get("data")
        .and_then(|d| d.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        // A grant whose listing is empty permits nothing dispatchable. Saying
        // "verified" here would be the substitution this feature exists to
        // refuse (§18.3).
        return Err("the node accepted the token but lists no models for it".to_string());
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_accepts_the_suffixes_the_help_advertises() {
        assert_eq!(parse_ttl("90"), Ok(90));
        assert_eq!(parse_ttl("90s"), Ok(90));
        assert_eq!(parse_ttl("30m"), Ok(1_800));
        assert_eq!(parse_ttl("2h"), Ok(7_200));
        assert_eq!(parse_ttl("1d"), Ok(86_400));
    }

    #[test]
    fn ttl_refuses_rather_than_defaulting() {
        // Each of these once had a "reasonable" reading. A grant is a security
        // object: an unparseable window must not become a default one.
        assert!(parse_ttl("soon").is_err());
        assert!(parse_ttl("2 hours").is_err());
        assert!(parse_ttl("0").is_err());
        assert!(parse_ttl("0h").is_err());
        assert!(parse_ttl("").is_err());
        assert!(parse_ttl("-1h").is_err());
    }

    #[test]
    fn loopback_binds_are_recognised_in_every_form_config_allows() {
        assert!(bind_is_loopback("127.0.0.1"));
        assert!(bind_is_loopback("127.0.0.53"));
        assert!(bind_is_loopback("::1"));
        assert!(bind_is_loopback("localhost"));
        assert!(bind_is_loopback("  127.0.0.1  "));
        assert!(!bind_is_loopback("0.0.0.0"));
        assert!(!bind_is_loopback("192.168.1.10"));
        assert!(!bind_is_loopback("::"));
    }

    /// The link carries a BASE; `RemoteApiProvider` appends `/v1`. An operator
    /// pasting the URL they use with curl must not produce `/v1/v1`.
    #[test]
    fn base_url_normalisation_strips_v1_and_adds_a_scheme() {
        assert_eq!(normalise_base("box:9741"), "http://box:9741");
        assert_eq!(normalise_base("http://box:9741/"), "http://box:9741");
        assert_eq!(normalise_base("http://box:9741/v1"), "http://box:9741");
        assert_eq!(normalise_base("http://box:9741/v1/"), "http://box:9741");
        assert_eq!(normalise_base("https://box/v1"), "https://box");
    }

    #[test]
    fn an_explicit_url_wins_over_discovery() {
        assert_eq!(
            guest_base_url(Some("10.0.0.7:9741"), 9741).unwrap(),
            "http://10.0.0.7:9741"
        );
    }

    /// The cloud-peer case. Reads the env var directly (rather than through a
    /// seam) because that is what the daemon does, and a guest link carrying a
    /// different address than `MemberRecord.addresses` would be the bug.
    ///
    /// Serialised with the other env-touching test by running them in one
    /// body: `cargo test` shares a process, and two tests mutating the same
    /// var race.
    #[test]
    fn the_advertise_override_beats_interface_enumeration_but_not_an_explicit_url() {
        // No other test in this module reads or writes this var, so the
        // shared-process mutation is contained.
        std::env::set_var("SOVEREIGN_ADVERTISE_ADDR", "100.112.195.45");
        assert_eq!(
            guest_base_url(None, 9741).unwrap(),
            "http://100.112.195.45:9741",
            "a containerised daemon publishes the tailnet IP, not the docker bridge"
        );
        assert_eq!(
            guest_base_url(Some("box.example:9741"), 9741).unwrap(),
            "http://box.example:9741",
            "--url is still the most specific instruction"
        );
        std::env::remove_var("SOVEREIGN_ADVERTISE_ADDR");
    }

    #[test]
    fn durations_render_at_the_scale_an_operator_reads() {
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(90), "1m");
        assert_eq!(human_duration(7_200), "2h0m");
        assert_eq!(human_duration(90_000), "1d1h");
    }
}
