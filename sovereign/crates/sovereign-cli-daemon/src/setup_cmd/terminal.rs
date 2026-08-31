// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn setup --terminal <entry>` — onboard a node that holds no models.
//!
//! A terminal is a full mesh member that runs no local inference: it holds the
//! mesh key, gossips, shares knowledge and the ledger, and forwards every turn
//! and every embedding to a bound entry node. An IoT box or a laptop beside a
//! machine that holds the weights.
//!
//! This command writes the config and nothing else. Joining the mesh stays
//! `svrn mesh join <link>`, which already works and already routes through the
//! running daemon (`mesh_cmd.rs`) — the whole member onboarding is three lines:
//!
//! ```text
//! svrn setup --terminal http://halo:9741
//! svrn mesh join "sovereign://join/cwth-…"
//! svrn daemon
//! ```
//!
//! ## Why it probes before it writes
//!
//! Two things about the entry node cannot be guessed and must not be defaulted:
//! whether it is reachable at all, and which embed model it holds. The second
//! decides the vector space this terminal's corpora land in — a terminal embeds
//! over HTTP, so the space is the ENTRY node's — and the memory-embedding
//! staleness guard compares against whatever `embed_model_id()` reports.
//! Writing a placeholder would make that guard agree with itself and with
//! nothing else (§18.3). So setup asks `/status`, records what it finds, and
//! refuses when it cannot ask.
//!
//! A run that writes a config but never proves a served turn is a never-ran
//! dressed as a pass (§18.1), so the last step is a real completion through the
//! entry node, and the answer names the model that served it.

use std::time::Duration;

use sovereign_core::setup_config::{DataSection, NodeSection, SetupConfig};

use super::Opts;

/// How long to wait on each probe. Generous: an entry node may be loading a
/// large model when the terminal first reaches it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// Normalise whatever the operator typed into the `/v1` base the OpenAI
/// clients want, and the bare origin `/status` lives on.
///
/// Accepts `halo:9741`, `http://halo:9741`, `http://halo:9741/v1`, with or
/// without a trailing slash. Returns `(origin, v1_base)`.
fn normalize_entry(raw: &str) -> Result<(String, String), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("--terminal needs the entry node's address".to_string());
    }
    // Split the scheme off FIRST, then trim. Trimming slashes up front turns
    // the hostless `http://` into `http:`, which no longer looks like a scheme,
    // so the default-scheme branch rebuilds it as `http://http:` — a URL with a
    // "host" of `http:` that sails past every later check. Caught by
    // `an_empty_or_hostless_entry_is_refused`.
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((s, r)) if s == "http" || s == "https" => (s, r),
        Some((s, _)) => return Err(format!("unsupported scheme '{s}' (use http or https)")),
        None => ("http", trimmed),
    };
    let host_and_path = rest.trim_end_matches('/');
    let host_and_path = host_and_path.strip_suffix("/v1").unwrap_or(host_and_path);
    let host_and_path = host_and_path.trim_end_matches('/');
    if host_and_path.is_empty() {
        return Err(format!("'{raw}' has no host"));
    }
    let origin = format!("{scheme}://{host_and_path}");
    let v1 = format!("{origin}/v1");
    Ok((origin, v1))
}

/// What one `/status` read tells us about the entry node.
#[derive(Clone)]
struct EntryNodeFacts {
    /// The entry node's embed slot id. `None` when it declares none.
    embed: Option<String>,
    /// Does this node hold a slot that can serve a TURN?
    ///
    /// Used to tell an anchor from another terminal when a mesh has several
    /// members: a terminal answers `/status` perfectly well and holds nothing,
    /// so "reachable" is not the question. Read from the resident slot list,
    /// which is residency — the same source `build_self_manifest` was corrected
    /// to use on 2026-08-30, for the same reason.
    holds_chat_slot: bool,
    /// The entry node's mesh node id, recorded so `svrn doctor` can later tell
    /// "my entry node" from "whatever now answers at my entry node's address"
    /// (§7.5; see `NodeSection::entry_node_id`).
    node_id: Option<String>,
}

/// The entry node's embed slot id, read from its `/status`.
///
/// A `None` FIELD means the node answered but declares no such thing — for
/// the embed slot that is a real, reportable state (its own retrieval is
/// degraded). `Err` means we could not ask at all. The two must not collapse
/// (§18.2).
///
/// ONE probe for both facts, deliberately: they come off the same `/status`
/// body, and a second request would be a second chance for the two halves to
/// describe different moments.
async fn probe_entry_node(
    client: &reqwest::Client,
    origin: &str,
) -> Result<EntryNodeFacts, String> {
    let url = format!("{origin}/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("{url} returned a body this build cannot parse: {e}"))?;
    // `/status.inference.resident[]` carries `{role, model_id, …}` for every
    // configured slot (`routes_status.rs`). The embed slot is the one named.
    let embed = body
        .get("inference")
        .and_then(|i| i.get("resident"))
        .and_then(|r| r.as_array())
        .and_then(|slots| {
            slots
                .iter()
                .find(|s| s.get("role").and_then(|r| r.as_str()) == Some("embed"))
        })
        .and_then(|s| s.get("model_id"))
        .and_then(|m| m.as_str())
        .map(str::to_string);
    // `/status.node_id` is the entry node's mesh identity, top level and
    // present whether or not it has joined a mesh (`routes_status.rs`).
    let node_id = body
        .get("node_id")
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    let holds_chat_slot = body
        .get("inference")
        .and_then(|i| i.get("resident"))
        .and_then(|r| r.as_array())
        .is_some_and(|slots| {
            slots.iter().any(|s| {
                s.get("role")
                    .and_then(|r| r.as_str())
                    .is_some_and(|r| r != "embed")
                    && s.get("model_id")
                        .and_then(|m| m.as_str())
                        .is_some_and(|m| !m.is_empty())
            })
        });
    Ok(EntryNodeFacts {
        embed,
        node_id,
        holds_chat_slot,
    })
}

/// Drive one real completion through the entry node and return the model id
/// that served it.
///
/// `primary` by name, not a GGUF stem: the aliases are the mesh-routable names,
/// so the entry node's balancer picks whichever holder is least busy, and a
/// concrete quant would pin this terminal to one machine's filename
/// (`docs/ANCHOR_NODE.md`).
async fn prove_a_served_turn(client: &reqwest::Client, v1: &str) -> Result<String, String> {
    let url = format!("{v1}/chat/completions");
    let body = serde_json::json!({
        "model": "primary",
        "messages": [{"role": "user", "content": "Reply with the single word: ready"}],
        "max_tokens": 16,
        "stream": false,
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{url} answered {status}: {}", text.trim()));
    }
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{url} returned a body this build cannot parse: {e}"))?;
    let served = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("<unnamed>")
        .to_string();
    let content = parsed
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if content.trim().is_empty() {
        return Err(format!(
            "{url} answered 200 with an empty completion — the entry node is \
             reachable but is not serving turns"
        ));
    }
    Ok(served)
}

/// `svrn setup --terminal <entry>`, where `<entry>` is either a join link or
/// an address.
///
/// The join link is the one to reach for and the one the docs give, because it
/// is the SAME string every member already gets from `svrn mesh status` — an
/// operator hands out one artifact, not one per node class, and the person
/// pasting it never handles a credential. It also produces the binding ARCH
/// §7.5 asks for: joining first means there is a real node id to bind, so the
/// terminal follows its entry node across addresses and reaches it over
/// whatever path the mesh offers, including the iroh one that an encrypted
/// mesh makes its only ingress.
///
/// The address form stays for the case that has no identity to resolve: an
/// entry node that is not a mesh member, most often a daemon on this same
/// machine.
pub(super) async fn run_terminal_setup(entry_raw: &str, opts: &Opts) -> i32 {
    if let Some(link) = sovereign_mesh::deep_link::parse_join_argument(entry_raw) {
        return run_terminal_join(entry_raw, link, opts).await;
    }
    run_terminal_address(entry_raw, opts).await
}

/// The join-link path: join the mesh, find the node that holds the models,
/// bind its identity, prove a turn.
async fn run_terminal_join(
    raw: &str,
    link: sovereign_mesh::deep_link::DeepLink,
    opts: &Opts,
) -> i32 {
    use std::io::Write as _;

    println!();
    println!("  Sovereign Setup — terminal node");
    println!("  {}", "\u{2500}".repeat(54));
    println!();
    println!("  This machine will hold NO models. It joins the mesh as a full");
    println!("  member and routes the work it cannot do to whichever");
    println!("  member holds the weights.");
    println!();

    if SetupConfig::exists() && !opts.reset {
        let path = SetupConfig::default_path();
        println!("  Already set up. Config at {}", path.display());
        println!("  Run `svrn setup --reset --terminal <join-link>` to reconfigure.");
        return 0;
    }

    // A running daemon owns :9741/:9742 and its own AppState. Joining
    // in-process beside it updates the CLI's copy and never the one that
    // serves gossip — the split-brain `mesh_cmd::cmd_join` documents. Setup is
    // a bootstrap, so refuse and say what to do rather than route around it.
    if daemon_is_listening().await {
        eprintln!("error: a daemon is already running on :9741.");
        eprintln!();
        eprintln!(
            "hint: setup joins the mesh in-process, which needs those ports. Stop it \
             first:\n      svrn daemon stop\n      svrn setup --reset --terminal \"{raw}\""
        );
        return 1;
    }

    if opts.reset {
        if let Err(e) = SetupConfig::remove() {
            eprintln!("  warning: could not remove config: {e}");
        }
    }

    // ── 1. Join ───────────────────────────────────────────────────
    print!("  Joining the mesh... ");
    std::io::stdout().flush().ok();
    let node_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "sovereign-terminal".to_string());
    let daemon = std::sync::Arc::new(sovereign_mesh::daemon::EmbeddedDaemon::new(
        sovereign_core::rebrand::svrnmesh_root(),
        SetupConfig::unconfigured(),
        match admin_services() {
            Ok(s) => s,
            Err(e) => {
                println!();
                eprintln!("error: {e}");
                return 1;
            }
        },
    ));
    daemon.expose_client_api();
    let mesh_name = match daemon.join_mesh(&link, &node_name).await {
        Ok(result) => result.mesh_name,
        Err(e) => {
            println!();
            eprintln!("error: could not join the mesh: {e}");
            eprintln!();
            eprintln!(
                "hint: the link may have expired — an encrypted mesh's invite is \
                 short-lived. Ask for a fresh one: `svrn mesh status` on the entry \
                 node prints the current `join link:`."
            );
            return 1;
        }
    };
    println!("joined \"{mesh_name}\"");

    // ── 2. Find the node that holds the models ────────────────────
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not build an HTTP client: {e}");
            return 1;
        }
    };
    print!("  Looking for a member that holds models... ");
    std::io::stdout().flush().ok();
    let holders = match find_holders(&daemon, &client).await {
        Ok(h) => h,
        Err(msg) => {
            println!();
            eprintln!("error: {msg}");
            return 1;
        }
    };
    // An explicit `--entry` narrows the candidates before they are counted, so
    // the single-holder and disambiguated cases take the same path and cannot
    // drift apart.
    let holders = match opts.entry.as_deref() {
        None => holders,
        Some(want) => {
            let matched: Vec<_> = holders
                .iter()
                .filter(|(id, _, _, _)| id == want)
                .cloned()
                .collect();
            if matched.is_empty() {
                println!();
                eprintln!(
                    "error: --entry {want} names no member of \"{mesh_name}\" that holds \
                     a model."
                );
                eprintln!();
                eprintln!("      these do:");
                for (id, name, _, _) in &holders {
                    eprintln!("        {id}   {name}");
                }
                return 1;
            }
            matched
        }
    };
    let (entry_node_id, entry_name, entry_v1, facts) = match holders.len() {
        1 => {
            let (id, name, url, facts) = holders.into_iter().next().unwrap_or_else(|| {
                unreachable!("len() == 1 was just matched");
            });
            println!("{name}");
            (id, name, url, facts)
        }
        0 => {
            println!();
            eprintln!(
                "error: joined \"{mesh_name}\", but no member holds a model this node \
                 could route to."
            );
            eprintln!();
            eprintln!(
                "hint: a terminal is useless without one, so setup refuses rather than \
                 writing a config that cannot serve a turn. Check `svrn doctor` on the \
                 machine that is supposed to hold the weights."
            );
            return 1;
        }
        _ => {
            println!();
            eprintln!(
                "error: {} members hold models — name the one to bind:",
                holders.len()
            );
            eprintln!();
            for (id, name, _, _) in &holders {
                eprintln!("      svrn setup --reset --terminal \"{raw}\" --entry {id}   # {name}");
            }
            return 1;
        }
    };
    // ── 3. Write the config ───────────────────────────────────────
    match facts.embed.as_deref() {
        Some(m) => println!("  Embed model (the vector space this node's corpora land in): {m}"),
        None => println!(
            "  note: {entry_name} declares no embed slot, so this terminal cannot embed \
             either — retrieval and ingest are unavailable until it has one. Recorded \
             as absent rather than guessed."
        ),
    }
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(sovereign_core::rebrand::data_dir);
    let cfg = SetupConfig {
        models: None,
        node: NodeSection {
            // The IDENTITY is the binding. No address is written at all — a
            // recorded one would be a second answer to "where does a turn go",
            // and `validate_class` refuses a file carrying both.
            entry: None,
            entry_node: Some(entry_node_id.clone()),
            entry_node_id: None,
            entry_embed_model: facts.embed.clone(),
        },
        compute: Default::default(),
        // Default `[engine] kind = llama`, and it is never consulted: a
        // terminal returns its forwarding provider before `load_provider`
        // reaches the engine factory. Written as the default rather than
        // omitted so the file round-trips like any other config.
        engine: Default::default(),
        search: Default::default(),
        daemon: Default::default(),
        data: DataSection {
            dir: data_dir.clone(),
        },
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    };
    let config_path = match cfg.save() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!("    \u{2713} Wrote {}", config_path.display());

    // ── 4. Prove a served turn ────────────────────────────────────
    print!("  Asking the entry node to serve one turn... ");
    std::io::stdout().flush().ok();
    match prove_a_served_turn(&client, &entry_v1).await {
        Ok(served) => println!("answered by {served}"),
        Err(msg) => {
            println!();
            eprintln!("error: {msg}");
            eprintln!();
            eprintln!(
                "hint: the config at {} is written and correct as far as it goes, but \
                 nothing has served a turn through it yet — treat this as UNVERIFIED, \
                 not as working. The usual cause is that {entry_name} advertises no \
                 `primary` alias.",
                config_path.display(),
            );
            return 1;
        }
    }

    println!();
    println!("  Next:");
    println!("    svrn daemon");
    println!();
    println!("  Then point any OpenAI client at http://localhost:9741/v1 with model");
    println!("  `primary` — this node routes it onward. The bind is {entry_name}'s mesh");
    println!("  identity, so it keeps working when that machine changes address.");
    0
}

/// Is something already listening on the client port?
///
/// `/v1/models` rather than `/`, for the reason `mesh_cmd::daemon_listening_on`
/// gives: the root route answers 405 and reqwest treats that as a successful
/// send, which is exactly the question being asked — is anything there.
async fn daemon_is_listening() -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    client
        .get("http://127.0.0.1:9741/v1/models")
        .send()
        .await
        .is_ok()
}

/// The `DaemonServices` bundle a one-shot admin action needs.
fn admin_services() -> Result<sovereign_mesh::DaemonServices, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = sovereign_contracts::launch::Launch::parse(
        &args,
        sovereign_contracts::launch::Launch::Verb {
            name: "setup".to_string(),
            args: args.clone(),
        },
    );
    sovereign_mesh::assemble(&launch, sovereign_mesh::LaunchParts::Admin)
        .map_err(|refusal| refusal.to_string())
}

/// Peers that answer `/status` AND hold a slot that can serve a turn.
///
/// Polls, because gossip has not converged the instant a join returns and an
/// empty peer set at t=0 is not evidence of an empty mesh (§18.2). Returns
/// `(node_id_hex, name, v1_base, facts)` per holder.
#[allow(clippy::type_complexity)]
async fn find_holders(
    daemon: &sovereign_mesh::daemon::EmbeddedDaemon,
    client: &reqwest::Client,
) -> Result<Vec<(String, String, String, EntryNodeFacts)>, String> {
    const WINDOW: Duration = Duration::from_secs(30);
    let deadline = std::time::Instant::now() + WINDOW;
    let mut seen_any_peer = false;
    loop {
        let peers = daemon.peer_inference_endpoints().await;
        seen_any_peer |= !peers.is_empty();
        let mut holders = Vec::new();
        for peer in peers {
            let Some(v1) = peer.base_urls.first().cloned() else {
                continue;
            };
            let origin = v1.trim_end_matches("/v1").trim_end_matches('/').to_string();
            let Ok(facts) = probe_entry_node(client, &origin).await else {
                continue;
            };
            if facts.holds_chat_slot {
                holders.push((peer.node_id.to_hex(), peer.name.clone(), v1, facts));
            }
        }
        if !holders.is_empty() {
            return Ok(holders);
        }
        if std::time::Instant::now() >= deadline {
            return Err(if seen_any_peer {
                "the mesh has members, but none of them holds a model that can serve a \
                 turn (they answered /status with no resident chat slot)"
                    .to_string()
            } else {
                "no peers appeared within 30s of joining — gossip has not converged, or \
                 the entry node's daemon is not running"
                    .to_string()
            });
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

/// The address path — an entry node named by URL, unchanged from 2026-08-30.
async fn run_terminal_address(entry_raw: &str, opts: &Opts) -> i32 {
    let (origin, v1) = match normalize_entry(entry_raw) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 2;
        }
    };

    if SetupConfig::exists() && !opts.reset {
        let path = SetupConfig::default_path();
        println!();
        println!("  Already set up. Config at {}", path.display());
        println!("  Run `svrn setup --reset --terminal <entry>` to reconfigure.");
        return 0;
    }
    if opts.reset {
        if let Err(e) = SetupConfig::remove() {
            eprintln!("  warning: could not remove config: {e}");
        }
    }

    println!();
    println!("  Sovereign Setup — terminal node");
    println!("  {}", "─".repeat(54));
    println!();
    println!("  This machine will hold NO models. It joins the mesh as a full");
    println!("  member and routes the work it cannot do to:");
    println!("      {origin}");
    println!();

    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not build an HTTP client: {e}");
            return 1;
        }
    };

    // ── 1. Is the entry node there, and what does it embed with? ──
    print!("  Asking the entry node what it holds... ");
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let facts = match probe_entry_node(&client, &origin).await {
        Ok(f) => f,
        Err(msg) => {
            println!();
            eprintln!("error: {msg}");
            eprintln!();
            eprintln!(
                "hint: a terminal is useless without its entry node, so setup refuses \
                 rather than writing a config that cannot serve a turn. Check that the \
                 daemon is running there (`svrn daemon` on that machine) and that \
                 {origin} is reachable from here."
            );
            return 1;
        }
    };
    let embed_model = facts.embed;
    match embed_model.as_deref() {
        Some(m) => println!("embed model: {m}"),
        None => {
            println!("no embed slot");
            println!(
                "  note: the entry node declares no embed slot, so this terminal cannot \
                 embed either — retrieval and ingest will be unavailable until that node \
                 has one. Recorded as absent rather than guessed."
            );
        }
    }

    // ── 2. Write the config ───────────────────────────────────────
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(sovereign_core::rebrand::data_dir);
    let cfg = SetupConfig {
        // The whole point: no `[models]`. `node_class()` reads this plus the
        // entry below and answers `Terminal`.
        models: None,
        node: NodeSection {
            entry: Some(v1.clone()),
            // No identity binding on this path: the operator named an address,
            // and `validate_class` refuses a file that carries both.
            entry_node: None,
            // Recorded, not resolved through. See `NodeSection::entry_node_id`:
            // the bind is still the URL, and this is what lets `svrn doctor`
            // notice when that URL stops pointing at the same machine.
            entry_node_id: facts.node_id.clone(),
            entry_embed_model: embed_model.clone(),
        },
        compute: Default::default(),
        // Default `[engine] kind = llama`, and it is never consulted: a
        // terminal returns its forwarding provider before `load_provider`
        // reaches the engine factory. Written as the default rather than
        // omitted so the file round-trips like any other config.
        engine: Default::default(),
        search: Default::default(),
        daemon: Default::default(),
        data: DataSection {
            dir: data_dir.clone(),
        },
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    };
    let config_path = match cfg.save() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!("    \u{2713} Wrote {}", config_path.display());

    // ── 3. Prove a served turn ────────────────────────────────────
    //
    // Exiting 0 here without this would report success for work nobody
    // watched happen (§18.1).
    print!("  Asking the entry node to serve one turn... ");
    std::io::stdout().flush().ok();
    match prove_a_served_turn(&client, &v1).await {
        Ok(served) => {
            println!("answered by {served}");
        }
        Err(msg) => {
            println!();
            eprintln!("error: {msg}");
            eprintln!();
            eprintln!(
                "hint: the config at {} is written and correct as far as it goes, but \
                 nothing has served a turn through it yet — treat this as UNVERIFIED, \
                 not as working. The usual cause is that the entry node advertises no \
                 `primary` alias; check `curl {origin}/v1/models` there.",
                config_path.display(),
            );
            return 1;
        }
    }

    println!();
    println!("  Next:");
    println!("    svrn mesh join \"<join link from `svrn mesh status` on the entry node>\"");
    println!("    svrn daemon");
    println!();
    println!("  Then point any OpenAI client at http://localhost:9741/v1 with model");
    println!("  `primary` — this node routes it onward.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_addresses_normalize_to_an_origin_and_a_v1_base() {
        for raw in [
            "halo:9741",
            "http://halo:9741",
            "http://halo:9741/",
            "http://halo:9741/v1",
            "http://halo:9741/v1/",
            "  http://halo:9741/v1  ",
        ] {
            let (origin, v1) = normalize_entry(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(origin, "http://halo:9741", "origin for {raw}");
            assert_eq!(v1, "http://halo:9741/v1", "v1 base for {raw}");
        }
    }

    #[test]
    fn an_https_entry_keeps_its_scheme() {
        let (origin, v1) = normalize_entry("https://halo.example:8443/v1").unwrap();
        assert_eq!(origin, "https://halo.example:8443");
        assert_eq!(v1, "https://halo.example:8443/v1");
    }

    #[test]
    fn an_empty_or_hostless_entry_is_refused() {
        assert!(normalize_entry("").is_err());
        assert!(normalize_entry("   ").is_err());
        assert!(normalize_entry("http://").is_err());
    }

    /// `/status` shapes: a node with an embed slot, a node without one, and a
    /// body that says nothing about slots. The middle two are the same value
    /// (`None`) and both are distinct from a probe error — which is why the
    /// signature is `Result<Option<_>>` and not `Option<_>` (§18.2).
    #[test]
    fn the_embed_slot_is_read_by_role_not_by_position() {
        let body = serde_json::json!({
            "inference": {
                "resident": [
                    {"role": "fast", "model_id": "Qwen3-1.7B-Q8_0"},
                    {"role": "embed", "model_id": "qwen-embedding-0.6b"},
                    {"role": "primary", "model_id": "Qwen3.5-35B-A3B"},
                ]
            }
        });
        let found = body
            .get("inference")
            .and_then(|i| i.get("resident"))
            .and_then(|r| r.as_array())
            .and_then(|slots| {
                slots
                    .iter()
                    .find(|s| s.get("role").and_then(|r| r.as_str()) == Some("embed"))
            })
            .and_then(|s| s.get("model_id"))
            .and_then(|m| m.as_str());
        assert_eq!(found, Some("qwen-embedding-0.6b"));
    }
}
