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

/// The entry node's embed slot id, read from its `/status`.
///
/// `Ok(None)` means the node answered but declares no embed slot — a real,
/// reportable state (its own retrieval is degraded), distinct from `Err`,
/// which means we could not ask at all.
async fn probe_embed_model(
    client: &reqwest::Client,
    origin: &str,
) -> Result<Option<String>, String> {
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
    Ok(embed)
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

pub(super) async fn run_terminal_setup(entry_raw: &str, opts: &Opts) -> i32 {
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
    println!("  member and routes every turn and every embedding to:");
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
    let embed_model = match probe_embed_model(&client, &origin).await {
        Ok(e) => e,
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
            entry_embed_model: embed_model.clone(),
        },
        compute: Default::default(),
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
