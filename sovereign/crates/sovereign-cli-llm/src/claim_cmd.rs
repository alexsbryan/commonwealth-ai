// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn claim` — CLI surface for the work atlas.
//!
//! DAEMON-FIRST: the daemon's work-atlas store is the one peers,
//! gossip, and CodeWatcher observations share, so every subcommand
//! calls the daemon's MCP tools when it answers. The in-process
//! repo-local `.sovereign/mesh.db` is a FALLBACK for daemon-down
//! operation only, and says so loudly — a claim written there is
//! invisible to every other process. (The previous header claimed the
//! daemon and CLI share that file; they never did — the daemon's
//! store is in-memory. Root-caused 2026-07-31.)
//!
//! Output is human-readable by default; `--format json` mirrors
//! `svrn tools` for scripting.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use commonwealth_state::MeshStore;
use uuid::Uuid;

use sovereign_cli_shared::mcp_client::{daemon_tool_call, DaemonCallError};
use sovereign_work_atlas::{
    model::{AgentKind, Privacy},
    resolve_repo_id, ClaimRecord, ScopeMatch, SessionIdentity, WorkAtlasConfig, WorkAtlasStore,
};

/// Outcome of the daemon-first attempt for one subcommand.
enum DaemonFirst {
    /// The daemon answered; here is the tool's JSON payload.
    Payload(serde_json::Value),
    /// No daemon — caller proceeds against the repo-local store.
    Fallback,
    /// The daemon answered and REJECTED the call. Do not fall back:
    /// retrying a rejected write against a store nobody reads would
    /// manufacture a success.
    Fail(i32),
}

async fn daemon_first(tool: &str, args: serde_json::Value) -> DaemonFirst {
    match daemon_tool_call(tool, args).await {
        Ok(v) => DaemonFirst::Payload(v),
        Err(DaemonCallError::Tool(msg)) => {
            eprintln!("claim: daemon rejected {tool}: {msg}");
            DaemonFirst::Fail(1)
        }
        Err(DaemonCallError::Unreachable(_)) => {
            eprintln!(
                "warning: daemon unreachable — using the repo-local store. Records here are \
                 NOT visible to the daemon, MCP peers, or the mesh; re-declare once the \
                 daemon is back if coordination matters."
            );
            DaemonFirst::Fallback
        }
    }
}

pub async fn run(args: &[String]) -> i32 {
    let Some((sub, rest)) = args.split_first() else {
        print_help();
        return 2;
    };
    match sub.as_str() {
        "help" | "--help" | "-h" => {
            print_help();
            0
        }
        "check" => run_check(rest).await,
        "list" => run_list(rest).await,
        "release" => run_release(rest).await,
        // `svrn claim <symbol> --intent <text>` — the bare form.
        // No leading subcommand keyword.
        scope => run_declare(scope, rest).await,
    }
}

fn print_help() {
    eprintln!(
        "svrn claim — coordinate work with other agents on this mesh\n\
         \n\
         Usage:\n  \
           sovereign claim <symbol-or-path> --intent <text> [--ttl <seconds>]\n  \
           sovereign claim check <symbol-or-path>\n  \
           sovereign claim list [--mine|--all]\n  \
           sovereign claim release <claim-id>\n  \
         \n\
         Flags:\n  \
           --format json   Emit JSON instead of human-readable output.\n  \
         \n\
         See sovereign/docs/WORK_ATLAS.md for the full model.\n"
    );
}

async fn run_declare(scope: &str, rest: &[String]) -> i32 {
    let mut intent: Option<String> = None;
    let mut ttl_seconds: Option<u64> = None;
    let mut format_json = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--intent" => {
                if i + 1 >= rest.len() {
                    eprintln!("claim: --intent requires a value");
                    return 2;
                }
                intent = Some(rest[i + 1].clone());
                i += 2;
            }
            "--ttl" => {
                if i + 1 >= rest.len() {
                    eprintln!("claim: --ttl requires a seconds value");
                    return 2;
                }
                ttl_seconds = rest[i + 1].parse().ok();
                if ttl_seconds.is_none() {
                    eprintln!("claim: --ttl must be a positive integer");
                    return 2;
                }
                i += 2;
            }
            "--format" => {
                format_json = i + 1 < rest.len() && rest[i + 1] == "json";
                i += 2;
            }
            other => {
                eprintln!("claim: unknown flag '{other}'");
                return 2;
            }
        }
    }
    let Some(intent_s) = intent else {
        eprintln!("claim: --intent <text> is required");
        return 2;
    };
    if intent_s.trim().is_empty() {
        eprintln!("claim: intent must not be empty");
        return 2;
    }

    let mut args = serde_json::json!({ "symbols": [scope], "intent": intent_s });
    if let Some(ttl) = ttl_seconds {
        args["ttl_seconds"] = ttl.into();
    }
    match daemon_first("declare_scope", args).await {
        DaemonFirst::Payload(p) => {
            if format_json {
                println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default());
            } else {
                let claim_id = p["claim_id"].as_str().unwrap_or("?");
                println!(
                    "claimed {scope}\n  id:       {}\n  intent:   {}\n  expires:  {}\n  release with: sovereign claim release {}",
                    claim_id,
                    p["intent"].as_str().unwrap_or(&intent_s),
                    p["ttl_expires_at"].as_u64().unwrap_or(0),
                    claim_id,
                );
            }
            return 0;
        }
        DaemonFirst::Fail(code) => return code,
        DaemonFirst::Fallback => {}
    }

    let ctx = match open_atlas() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let ttl = ctx.config.clamp_ttl(ttl_seconds);
    let identity = SessionIdentity {
        node_id: ctx.store.node_id(),
        // CLI synthetic — one ambient human session per workstation
        // for Phase 1 (Phase 2 supersedes with CodeWatcher idle-gap).
        agent_session_token: Some(format!("cli:{}", ctx.store.node_id())),
        repo_id: ctx.repo_id.clone(),
    };
    let session = match ctx.store.ensure_session(
        identity,
        ctx.config.node.default_privacy_enum(),
        AgentKind::Human,
        ctx.repo_root.clone(),
        ctx.current_branch.clone(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("claim: open session: {e}");
            return 1;
        }
    };
    let now = now_secs();
    let claim = ClaimRecord {
        claim_id: Uuid::new_v4(),
        session_id: session.session_id,
        intent: intent_s.trim().to_string(),
        symbol_refs: vec![sovereign_work_atlas::SymbolRef {
            scip_symbol: None,
            file_path: PathBuf::from(scope),
            scip_was_fresh: false,
        }],
        declared_at: now,
        ttl_expires_at: now.saturating_add(ttl),
    };
    if let Err(e) = ctx.store.put_claim(session.privacy, &claim) {
        eprintln!("claim: write: {e}");
        return 1;
    }

    if format_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "claim_id":       claim.claim_id.to_string(),
                "session_id":     claim.session_id.to_string(),
                "ttl_expires_at": claim.ttl_expires_at,
                "intent":         claim.intent,
                "scope":          scope,
            }))
            .unwrap()
        );
    } else {
        println!(
            "claimed {scope}\n  id:       {}\n  intent:   {}\n  ttl:      {}s (expires {})\n  session:  {}\n  release with: sovereign claim release {}",
            claim.claim_id,
            claim.intent,
            ttl,
            claim.ttl_expires_at,
            claim.session_id,
            claim.claim_id,
        );
    }
    0
}

async fn run_check(rest: &[String]) -> i32 {
    let mut format_json = false;
    let mut scope: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--format" => {
                format_json = i + 1 < rest.len() && rest[i + 1] == "json";
                i += 2;
            }
            other if !other.starts_with("--") && scope.is_none() => {
                scope = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("claim check: unknown argument '{other}'");
                return 2;
            }
        }
    }
    let Some(scope) = scope else {
        eprintln!("claim check: scope required");
        return 2;
    };

    match daemon_first(
        "work_in_flight",
        serde_json::json!({ "scope": scope, "match_mode": "symbol", "include_self": true }),
    )
    .await
    {
        DaemonFirst::Payload(p) => {
            let claims = p["claims"].as_array().cloned().unwrap_or_default();
            if format_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "scope": scope, "live": claims })
                    )
                    .unwrap_or_default()
                );
            } else if claims.is_empty() {
                println!("no live claims on {scope}");
            } else {
                let now = now_secs();
                println!("live claims on {scope}:");
                for c in &claims {
                    println!(
                        "  {}  intent: {}  expires-in: {}s",
                        c["claim_id"].as_str().unwrap_or("?"),
                        c["intent"].as_str().unwrap_or("?"),
                        c["ttl_expires_at"].as_u64().unwrap_or(0).saturating_sub(now),
                    );
                }
            }
            return 0;
        }
        DaemonFirst::Fail(code) => return code,
        DaemonFirst::Fallback => {}
    }

    let ctx = match open_atlas() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let claims = match ctx.store.list_claims_for_scope(&scope, ScopeMatch::Symbol) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("claim check: {e}");
            return 1;
        }
    };
    let now = now_secs();
    let live: Vec<&ClaimRecord> = claims.iter().filter(|c| c.ttl_expires_at >= now).collect();

    if format_json {
        let claims_json: Vec<_> = live
            .iter()
            .map(|c| {
                serde_json::json!({
                    "claim_id":       c.claim_id.to_string(),
                    "session_id":     c.session_id.to_string(),
                    "intent":         c.intent,
                    "ttl_expires_at": c.ttl_expires_at,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "scope":  scope,
                "live":   claims_json,
            }))
            .unwrap()
        );
    } else if live.is_empty() {
        println!("no live claims on {scope}");
    } else {
        println!("live claims on {scope}:");
        for c in &live {
            println!(
                "  {}  intent: {}  expires-in: {}s",
                c.claim_id,
                c.intent,
                c.ttl_expires_at.saturating_sub(now)
            );
        }
    }
    // Historical-prior: git co-evolution. Clearly labeled per §9 of
    // the spec — never confused with live activity.
    if !format_json {
        println!(
            "\n(historical-prior — not live activity; git co-evolution lookup deferred to Phase 2)"
        );
    }
    0
}

async fn run_list(rest: &[String]) -> i32 {
    let mut mine = false;
    let mut format_json = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--mine" => {
                mine = true;
                i += 1;
            }
            "--all" => {
                mine = false;
                i += 1;
            }
            "--format" => {
                format_json = i + 1 < rest.len() && rest[i + 1] == "json";
                i += 2;
            }
            other => {
                eprintln!("claim list: unknown argument '{other}'");
                return 2;
            }
        }
    }
    match daemon_first(
        "work_in_flight",
        serde_json::json!({ "scope": "", "match_mode": "file", "include_self": true }),
    )
    .await
    {
        DaemonFirst::Payload(p) => {
            let now = now_secs();
            let my_node = sovereign_mesh::persist::resolve_self_node_id(
                &sovereign_cli_shared::dirs::sovereign_root(),
            )
            .to_string();
            let claims: Vec<serde_json::Value> = p["claims"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|c| !mine || c["node_id"].as_str() == Some(my_node.as_str()))
                .collect();
            if format_json {
                println!("{}", serde_json::to_string_pretty(&claims).unwrap_or_default());
            } else if claims.is_empty() {
                println!("no live claims{}", if mine { " (yours)" } else { "" });
            } else {
                for c in &claims {
                    let target = c["scopes"][0].as_str().unwrap_or("<no-scope>");
                    println!(
                        "{}  {}  intent: {}  expires-in: {}s  node: {}",
                        c["claim_id"].as_str().unwrap_or("?"),
                        target,
                        c["intent"].as_str().unwrap_or("?"),
                        c["ttl_expires_at"].as_u64().unwrap_or(0).saturating_sub(now),
                        c["node_id"].as_str().unwrap_or("?"),
                    );
                }
            }
            return 0;
        }
        DaemonFirst::Fail(code) => return code,
        DaemonFirst::Fallback => {}
    }

    let ctx = match open_atlas() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let now = now_secs();
    // "Mine" = this workstation's node. The old token-string match
    // (`cli:<node>`) missed sessions created by any other surface
    // (tools-path sessions carry no token), which made `--mine`
    // return empty against your own live claims.
    let me_node = ctx.store.node_id();

    let mut rows: Vec<(ClaimRecord, Option<String>)> = Vec::new();
    for privacy in [Privacy::Public, Privacy::Private] {
        let claims = match ctx.store.scan_claims(privacy) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("claim list: scan {}: {e}", privacy.id());
                return 1;
            }
        };
        for c in claims {
            if c.ttl_expires_at < now {
                continue;
            }
            let session = ctx.store.get_session(c.session_id).ok().flatten();
            if mine {
                let is_mine = session
                    .as_ref()
                    .map(|s| s.node_id == me_node)
                    .unwrap_or(false);
                if !is_mine {
                    continue;
                }
            }
            let node = session.as_ref().map(|s| s.node_id.to_string());
            rows.push((c, node));
        }
    }
    rows.sort_by_key(|(c, _)| std::cmp::Reverse(c.declared_at));

    if format_json {
        let json: Vec<_> = rows
            .iter()
            .map(|(c, node)| {
                serde_json::json!({
                    "claim_id":       c.claim_id.to_string(),
                    "session_id":     c.session_id.to_string(),
                    "intent":         c.intent,
                    "ttl_expires_at": c.ttl_expires_at,
                    "node_id":        node,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    } else if rows.is_empty() {
        println!("no live claims{}", if mine { " (yours)" } else { "" });
    } else {
        for (c, node) in &rows {
            let target = c
                .symbol_refs
                .first()
                .map(|s| s.file_path.to_string_lossy().to_string())
                .unwrap_or_else(|| "<no-scope>".into());
            println!(
                "{}  {}  intent: {}  expires-in: {}s  node: {}",
                c.claim_id,
                target,
                c.intent,
                c.ttl_expires_at.saturating_sub(now),
                node.as_deref().unwrap_or("?")
            );
        }
    }
    0
}

async fn run_release(rest: &[String]) -> i32 {
    let mut format_json = false;
    let mut claim_id_str: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--format" => {
                format_json = i + 1 < rest.len() && rest[i + 1] == "json";
                i += 2;
            }
            other if !other.starts_with("--") && claim_id_str.is_none() => {
                claim_id_str = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("claim release: unknown argument '{other}'");
                return 2;
            }
        }
    }
    let Some(s) = claim_id_str else {
        eprintln!("claim release: claim-id required");
        return 2;
    };
    let claim_id = match Uuid::parse_str(&s) {
        Ok(id) => id,
        Err(_) => {
            eprintln!("claim release: invalid uuid '{s}'");
            return 2;
        }
    };

    match daemon_first(
        "release_scope",
        serde_json::json!({ "claim_id": claim_id.to_string() }),
    )
    .await
    {
        DaemonFirst::Payload(p) => {
            let released = p["released"].as_bool().unwrap_or(true);
            if format_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "released": released }))
                        .unwrap_or_default()
                );
            } else if released {
                println!("released {claim_id}");
            } else {
                println!("no claim {claim_id} (already released or expired)");
            }
            return 0;
        }
        DaemonFirst::Fail(code) => return code,
        DaemonFirst::Fallback => {}
    }

    let ctx = match open_atlas() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let released = match ctx.store.release_claim(claim_id) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("claim release: {e}");
            return 1;
        }
    };
    if format_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "released": released })).unwrap()
        );
    } else if released {
        println!("released {claim_id}");
    } else {
        println!("no claim {claim_id} (already released or expired)");
    }
    0
}

// ── Atlas open / context ────────────────────────────────────────────────

struct CliCtx {
    store: Arc<WorkAtlasStore>,
    config: WorkAtlasConfig,
    repo_root: PathBuf,
    repo_id: String,
    current_branch: Option<String>,
}

fn open_atlas() -> Result<CliCtx, i32> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("claim: cwd: {e}");
            return Err(1);
        }
    };
    let (repo_root, repo_id) = match resolve_repo_id(&cwd) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("claim: {e}");
            return Err(1);
        }
    };
    let sovereign_dir = repo_root.join(".sovereign");
    let _ = std::fs::create_dir_all(&sovereign_dir);
    let mesh_path = sovereign_dir.join("mesh.db");
    let mesh = match MeshStore::open(&mesh_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("claim: mesh open {}: {e}", mesh_path.display());
            return Err(1);
        }
    };
    // Identity from the ROOT data dir with the daemon's precedence
    // (node_id file → mesh.json → generate). The previous hardcoded
    // `~/.svrnmesh/indexes` minted a SECOND node id for this
    // workstation (2026-07-31).
    let node_id = sovereign_mesh::persist::resolve_self_node_id(
        &sovereign_cli_shared::dirs::sovereign_root(),
    );
    let store = Arc::new(WorkAtlasStore::new(Arc::new(mesh), node_id));

    let cfg_path = sovereign_contracts::rebrand::work_atlas_toml();
    let config = WorkAtlasConfig::load_or_default(&cfg_path).unwrap_or_else(|e| {
        eprintln!("claim: config load failed ({e}); using defaults");
        WorkAtlasConfig::defaults()
    });

    let current_branch = git_current_branch(&repo_root);
    Ok(CliCtx {
        store,
        config,
        repo_root,
        repo_id,
        current_branch,
    })
}

fn git_current_branch(repo_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" {
        None
    } else {
        Some(s)
    }
}

use sovereign_core::time::unix_now_u64 as now_secs;
