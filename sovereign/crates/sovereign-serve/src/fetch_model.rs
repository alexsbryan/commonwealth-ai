// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-serve fetch-model`, spelled `svrn mesh fetch-model` through the
//! dispatcher. Moved from sovereign-cli-mesh (phase-b-22): model transfer is
//! serve's (phase-b-19). The mesh.json read and the direct peer dial below are
//! residue; pb-serve-placement moves them onto cw-rails' reach door.

use std::path::PathBuf;

use sovereign_cli_base::dirs::sovereign_root;

/// `svrn mesh fetch-model <name> [--peer <peer-tailnet-addr>] [--out <dir>]`
///
/// Pulls a GGUF from a mesh peer over the tailnet. Used by the
/// friend-onboarding flow (WS5) so a new node doesn't need R2 /
/// S3 credentials of its own — it joins the mesh first, then
/// pulls model files from whoever already has them.
///
/// Discovery order:
///   1. If `--peer host:port` is given, use that directly.
///   2. Otherwise, read the local daemon's mesh.json to find peer
///      `addresses`, try each peer's `/internal/v1/models/list`
///      in turn, return the first peer that advertises `<name>`.
///
/// Destination: defaults to the parent dir of the local `[models]
/// .primary` path (the conventional models dir). `--out <dir>`
/// overrides.
///
/// Integrity: the peer's listing carries a SHA-256; the response
/// stream is hashed on the fly and the file is rejected on
/// mismatch. See `sovereign_serving_host::model_fetch::fetch_model_to_dir`.
pub(crate) async fn cmd_fetch_model(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) {
        eprintln!("Usage: svrn mesh fetch-model <name> [--peer <host:port>] [--out <dir>]");
        eprintln!();
        eprintln!("Pulls a GGUF from a mesh peer over the tailnet. No R2 credentials required.");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  svrn mesh fetch-model Darwin-9B-Opus.Q4_K_M.gguf");
        eprintln!("  svrn mesh fetch-model Qwen3-Embedding-0.6B-Q8_0.gguf --out ~/models");
        return 0;
    }

    let Some(name) = args.first().cloned() else {
        eprintln!("Missing model file name.");
        eprintln!("Usage: svrn mesh fetch-model <name> [--peer <host:port>] [--out <dir>]");
        return 1;
    };

    // Tiny manual flag parser — sticks with the existing CLI
    // convention here (no clap on this subcommand).
    let mut peer_override: Option<String> = None;
    let mut out_override: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--peer" => {
                i += 1;
                peer_override = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                out_override = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
        i += 1;
    }

    // Default dest is the dir holding `cfg.models.primary`. We
    // read SetupConfig from disk rather than the running daemon
    // so this command works even when the daemon's down — handy
    // during friend-onboarding where the daemon might be in its
    // first-boot loop.
    let dest_dir = match out_override {
        Some(p) => p,
        None => match sovereign_contracts::setup_config::SetupConfig::load() {
            Ok(cfg) => cfg
                .models
                .as_ref()
                .and_then(|m| m.primary.parent())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            Err(e) => {
                eprintln!("error: could not load setup config to choose default --out dir: {e}");
                eprintln!(
                    "hint: pass --out <dir> explicitly, or run `svrn daemon --setup-only` first."
                );
                return 1;
            }
        },
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 60)) // 1h cap for very slow links
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: build http client: {e}");
            return 1;
        }
    };

    // Decide which peers to try. Explicit override wins.
    let peer_candidates: Vec<String> = if let Some(p) = peer_override {
        // Accept `host:port` or `http://host:port` — normalise to URL.
        let url = if p.starts_with("http://") || p.starts_with("https://") {
            p
        } else {
            format!("http://{p}")
        };
        vec![url]
    } else {
        match collect_peer_internal_urls().await {
            Ok(urls) if urls.is_empty() => {
                eprintln!("No mesh peers known. Run `svrn mesh join <link>` first,");
                eprintln!("or pass --peer <host:port> to target a specific node.");
                return 1;
            }
            Ok(urls) => urls,
            Err(e) => {
                eprintln!("error: discovering peers: {e}");
                return 1;
            }
        }
    };

    println!();
    println!(
        "Searching {} peer(s) for '{}'…",
        peer_candidates.len(),
        name
    );

    for peer_url in &peer_candidates {
        // Probe the peer's listing first so we can pick the one
        // that actually advertises `name` before committing to
        // the download.
        let listing =
            match sovereign_serving_host::model_fetch::list_peer_files(&client, peer_url, None)
                .await
            {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("  ✗ {peer_url}: list failed ({e})");
                    continue;
                }
            };
        let Some(info) = listing.files.into_iter().find(|f| f.name == name) else {
            println!("  · {peer_url}: doesn't have it");
            continue;
        };
        println!(
            "  → {peer_url}: streaming {} ({} MiB, sha256={}…)",
            info.name,
            info.size_bytes / (1024 * 1024),
            &info.sha256[..16],
        );
        let started = std::time::Instant::now();
        let progress = |downloaded: u64, total: u64| {
            let pct = if total > 0 {
                100 * downloaded / total
            } else {
                0
            };
            eprint!(
                "\r  {} / {} MiB ({}%)…   ",
                downloaded / (1024 * 1024),
                total / (1024 * 1024),
                pct,
            );
        };
        match sovereign_serving_host::model_fetch::fetch_model_to_dir(
            // Unstamped: the CLI holds no `Mesh`, so it has nothing to mint
            // a proof from. Against a peer on the default `internal_auth` this
            // reaches only a loopback daemon; the fix is for the CLI to ask its
            // own daemon to fetch, not to forge a credential it does not hold.
            &client, peer_url, &info, &dest_dir, None, progress,
        )
        .await
        {
            Ok(path) => {
                let secs = started.elapsed().as_secs_f64();
                let mb_per_s = if secs > 0.0 {
                    (info.size_bytes as f64 / 1_048_576.0) / secs
                } else {
                    0.0
                };
                eprintln!();
                println!();
                println!("✓ saved to {}", path.display());
                println!(
                    "  {} MiB in {:.1}s ({:.1} MiB/s)",
                    info.size_bytes / (1024 * 1024),
                    secs,
                    mb_per_s,
                );
                return 0;
            }
            Err(e) => {
                eprintln!();
                eprintln!("  ✗ {peer_url}: fetch failed ({e})");
                // fall through to next peer
            }
        }
    }

    eprintln!();
    eprintln!("No peer could serve '{name}'.");
    1
}

/// Discover peer internal-port URLs by reading the local daemon's
/// persisted mesh.json. `MemberRecord.addresses` for each peer are
/// the gossip-port endpoints (`:9742`), which is exactly what we
/// want — the model-files routes live on the internal port.
async fn collect_peer_internal_urls() -> std::io::Result<Vec<String>> {
    let mesh_path = sovereign_root().join("mesh.json");
    let bytes = std::fs::read(&mesh_path)?;
    // Parse loosely — we only need the addresses array of each
    // non-self member. Using serde_json::Value avoids dragging in
    // the full Mesh deserialiser, which would force a tight
    // coupling on the on-disk schema this command only inspects.
    let v: serde_json::Value = serde_json::from_slice(&bytes)?;
    let self_id = v.get("self_node_id").and_then(|x| x.as_str()).unwrap_or("");
    let mut urls = Vec::new();
    if let Some(members) = v
        .get("mesh")
        .and_then(|m| m.get("members"))
        .and_then(|m| m.as_object())
    {
        for (nid, member) in members {
            if nid == self_id {
                continue;
            }
            if let Some(addrs) = member.get("addresses").and_then(|a| a.as_array()) {
                for a in addrs {
                    if let Some(s) = a.as_str() {
                        urls.push(format!("http://{}", s));
                    }
                }
            }
        }
    }
    Ok(urls)
}
