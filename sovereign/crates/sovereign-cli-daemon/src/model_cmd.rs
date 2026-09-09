//! `svrn model` — inspect and change which models the daemon loads, without
//! hand-editing `config.toml` and without the non-obvious restart.
//!
//! Two annoyances this removes:
//!   1. Editing `[models]` in config.toml by hand (easy to typo a path, and
//!      not discoverable). `svrn model set primary <file>` does it safely,
//!      validating the file exists first.
//!   2. "Why didn't my change take effect?" — model edits need the daemon to
//!      reload. This command applies the change LIVE via the admin reload
//!      endpoint (models hot-swap, no restart), and when the daemon isn't
//!      running it says plainly that the change lands on next start.
//!
//! Slots: `primary` (main responder), `fast` (optional speed slot), `embed`
//! (required), `code` (optional specialist), plus named `extra` slots.

use crate::setup_config::SetupConfig;
use std::path::{Path, PathBuf};

const DAEMON_MODELS_URL: &str = "http://127.0.0.1:9741/v1/models";

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(|s| s.as_str()).unwrap_or("") {
        "" | "list" | "show" | "status" => cmd_list().await,
        "set" => cmd_set(&args[1..]).await,
        "unset" => cmd_unset(&args[1..]).await,
        "set-extra" => cmd_set_extra(&args[1..]).await,
        "rm-extra" => cmd_rm_extra(&args[1..]).await,
        "context" => cmd_context(&args[1..]).await,
        "-h" | "--help" | "help" => {
            print_help();
            0
        }
        other => {
            eprintln!("svrn model: unknown subcommand '{other}'\n");
            print_help();
            2
        }
    }
}

fn print_help() {
    eprintln!(
        "svrn model — see and change the models the daemon loads\n\
         \n\
         USAGE:\n\
         \x20 svrn model [list]                 show configured slots + what's loaded now\n\
         \x20 svrn model set <slot> <file>      set primary|fast|embed|code to a model\n\
         \x20 svrn model unset <slot>           clear an optional slot (fast|code)\n\
         \x20 svrn model set-extra <name> <file>  add/replace a named extra slot\n\
         \x20 svrn model rm-extra <name>        remove a named extra slot\n\
         \x20 svrn model context <n|auto>       set the context window (n_ctx) for all slots\n\
         \n\
         <file> is an absolute path, or a filename in your models dir\n\
         (~/.svrnmesh/models). Changes apply live if the daemon is running,\n\
         otherwise on its next start — no manual config edit, no restart step."
    );
}

// ── slot mutations ───────────────────────────────────────────────────────────

async fn cmd_set(args: &[String]) -> i32 {
    let (slot, spec) = match (args.first(), args.get(1)) {
        (Some(s), Some(p)) => (s.as_str(), p.as_str()),
        _ => {
            eprintln!("usage: svrn model set <primary|fast|embed|code> <file>");
            return 2;
        }
    };
    if !matches!(slot, "primary" | "fast" | "embed" | "code") {
        eprintln!("svrn model set: unknown slot '{slot}' (primary|fast|embed|code)");
        return 2;
    }
    let mut cfg = match load_cfg() {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let models = match models_mut(&mut cfg) {
        Ok(m) => m,
        Err(rc) => return rc,
    };
    let path = match resolve_model_path(spec) {
        Ok(p) => p,
        Err(rc) => return rc,
    };
    match slot {
        "primary" => models.primary = path.clone(),
        "embed" => models.embed = path.clone(),
        "fast" => models.fast = Some(path.clone()),
        "code" => models.code = Some(path.clone()),
        _ => unreachable!(),
    }
    apply(
        cfg,
        &format!("models.{slot}"),
        &format!("{slot} → {}", path.display()),
    )
    .await
}

async fn cmd_unset(args: &[String]) -> i32 {
    let slot = match args.first().map(|s| s.as_str()) {
        Some(s) => s,
        None => {
            eprintln!("usage: svrn model unset <fast|code>");
            return 2;
        }
    };
    let mut cfg = match load_cfg() {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let models = match models_mut(&mut cfg) {
        Ok(m) => m,
        Err(rc) => return rc,
    };
    match slot {
        "fast" => models.fast = None,
        "code" => models.code = None,
        "primary" | "embed" => {
            eprintln!("svrn model unset: '{slot}' is required and cannot be cleared (use `set` to change it)");
            return 2;
        }
        other => {
            eprintln!("svrn model unset: unknown slot '{other}' (fast|code)");
            return 2;
        }
    }
    apply(
        cfg,
        &format!("models.{slot}"),
        &format!("{slot} cleared (subsumed by primary)"),
    )
    .await
}

async fn cmd_set_extra(args: &[String]) -> i32 {
    let (name, spec) = match (args.first(), args.get(1)) {
        (Some(n), Some(p)) => (n.as_str(), p.as_str()),
        _ => {
            eprintln!("usage: svrn model set-extra <name> <file>");
            return 2;
        }
    };
    let mut cfg = match load_cfg() {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let models = match models_mut(&mut cfg) {
        Ok(m) => m,
        Err(rc) => return rc,
    };
    let path = match resolve_model_path(spec) {
        Ok(p) => p,
        Err(rc) => return rc,
    };
    models.extra.insert(name.to_string(), path.clone());
    apply(
        cfg,
        "models.extra",
        &format!("extra '{name}' → {}", path.display()),
    )
    .await
}

async fn cmd_rm_extra(args: &[String]) -> i32 {
    let name = match args.first().map(|s| s.as_str()) {
        Some(n) => n,
        None => {
            eprintln!("usage: svrn model rm-extra <name>");
            return 2;
        }
    };
    let mut cfg = match load_cfg() {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let models = match models_mut(&mut cfg) {
        Ok(m) => m,
        Err(rc) => return rc,
    };
    if models.extra.remove(name).is_none() {
        eprintln!("svrn model rm-extra: no extra slot named '{name}'");
        return 2;
    }
    apply(cfg, "models.extra", &format!("extra '{name}' removed")).await
}

async fn cmd_context(args: &[String]) -> i32 {
    let raw = match args.first().map(|s| s.as_str()) {
        Some(v) => v,
        None => {
            eprintln!("usage: svrn model context <n|auto>");
            return 2;
        }
    };
    let mut cfg = match load_cfg() {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let models = match models_mut(&mut cfg) {
        Ok(m) => m,
        Err(rc) => return rc,
    };
    let desc = if raw.eq_ignore_ascii_case("auto") || raw == "0" {
        models.context_size = None;
        "context_size → auto (default)".to_string()
    } else {
        match raw.parse::<u32>() {
            Ok(n) if n >= 512 => {
                models.context_size = Some(n);
                format!("context_size → {n}")
            }
            _ => {
                eprintln!("svrn model context: expected a number ≥ 512 or 'auto' (got '{raw}')");
                return 2;
            }
        }
    };
    apply(cfg, "models.context_size", &desc).await
}

// ── list / status ────────────────────────────────────────────────────────────

async fn cmd_list() -> i32 {
    let cfg = match load_cfg() {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    let models = match models_of(&cfg) {
        Ok(m) => m,
        Err(rc) => return rc,
    };
    let resident = fetch_resident().await; // None when the daemon isn't reachable
                                           // Only annotate load state when we actually parsed a resident set; an
                                           // empty/unknown set (daemon up but schema not matched) stays unlabeled
                                           // rather than falsely claiming every slot is "not loaded".
    let mark = |p: &Path| -> &'static str {
        match &resident {
            Some(set) if !set.is_empty() => {
                if set.iter().any(|r| same_model(r, p)) {
                    " [loaded]"
                } else {
                    " [not loaded]"
                }
            }
            _ => "",
        }
    };

    println!("Models (from {})", SetupConfig::default_path().display());
    println!(
        "  primary  {}{}",
        models.primary.display(),
        mark(&models.primary)
    );
    match &models.fast {
        Some(p) => println!("  fast     {}{}", p.display(), mark(p)),
        None => println!("  fast     (subsumed by primary)"),
    }
    println!(
        "  embed    {}{}",
        models.embed.display(),
        mark(&models.embed)
    );
    match &models.code {
        Some(p) => println!("  code     {}{}", p.display(), mark(p)),
        None => println!("  code     (none; primary handles code)"),
    }
    for (name, p) in &models.extra {
        println!("  extra:{name}  {}{}", p.display(), mark(p));
    }
    println!(
        "  context  {}",
        models
            .context_size
            .map(|n| n.to_string())
            .unwrap_or_else(|| "auto".to_string())
    );
    match &resident {
        Some(_) => println!("\ndaemon: running — changes apply live."),
        None => println!("\ndaemon: not reachable — changes apply on next `svrn daemon start`."),
    }
    0
}

// ── shared: load, resolve, apply+reload ──────────────────────────────────────

/// The slot table, or a refusal with an exit code.
///
/// `svrn model` is entirely about local slots, so on a terminal every one of
/// its subcommands is meaningless rather than merely empty. Refusing by name
/// beats printing a table of blanks (§18.3) — a user who ran `svrn model list`
/// on a terminal and saw empty paths would reasonably conclude their config was
/// corrupt.
fn models_of(cfg: &SetupConfig) -> Result<&sovereign_core::setup_config::ModelsSection, i32> {
    cfg.models().map_err(|e| {
        eprintln!("svrn model: {e}");
        2
    })
}

/// As [`models_of`], for the subcommands that WRITE a slot.
fn models_mut(
    cfg: &mut SetupConfig,
) -> Result<&mut sovereign_core::setup_config::ModelsSection, i32> {
    cfg.models_mut().map_err(|e| {
        eprintln!("svrn model: {e}");
        2
    })
}

fn load_cfg() -> Result<SetupConfig, i32> {
    if !SetupConfig::exists() {
        eprintln!(
            "svrn model: no config yet at {}. Run `svrn setup` first.",
            SetupConfig::default_path().display()
        );
        return Err(2);
    }
    SetupConfig::load().map_err(|e| {
        eprintln!("svrn model: could not read config: {e}");
        1
    })
}

/// Resolve a user-supplied model spec to an existing file. Accepts an absolute
/// or CWD-relative path, or a bare filename looked up in the models dir (with
/// and without a `.gguf` suffix). Returns a canonical absolute path.
fn resolve_model_path(spec: &str) -> Result<PathBuf, i32> {
    let candidates = {
        let mut v = vec![PathBuf::from(spec)];
        let models_dir = models_dir();
        v.push(models_dir.join(spec));
        if !spec.ends_with(".gguf") {
            v.push(models_dir.join(format!("{spec}.gguf")));
        }
        v
    };
    for c in &candidates {
        if c.is_file() {
            return Ok(c.canonicalize().unwrap_or_else(|_| c.clone()));
        }
    }
    eprintln!("svrn model: no model file found for '{spec}'.");
    eprintln!(
        "  looked at: {}",
        candidates
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let md = models_dir();
    if let Ok(rd) = std::fs::read_dir(&md) {
        let ggufs: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".gguf"))
            .collect();
        if !ggufs.is_empty() {
            eprintln!("  available in {}:", md.display());
            for g in ggufs {
                eprintln!("    {g}");
            }
        }
    }
    Err(2)
}

fn models_dir() -> PathBuf {
    SetupConfig::default_path()
        .parent()
        .map(|d| d.join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}

/// Save the config, then apply it: hot-reload the running daemon (models swap
/// with no restart), or explain that the change lands on next start.
async fn apply(cfg: SetupConfig, field: &str, human: &str) -> i32 {
    match cfg.save() {
        Ok(path) => println!("updated {field}: {human}\n  wrote {}", path.display()),
        Err(e) => {
            eprintln!("svrn model: could not write config: {e}");
            return 1;
        }
    }
    if daemon_reachable().await {
        println!("applying to the running daemon…");
        // Reuse the daemon reload path (POST /v1/admin/reload). It compares the
        // running config to what we just wrote, hot-swaps changed model slots,
        // and reports which fields reloaded / whether a restart is still needed.
        // `reload` touches no launch-mode branch; naming the launch this
        // in-process call stands in for keeps `run` total.
        crate::daemon_cmd::run(
            &sovereign_contracts::launch::Launch::Daemon {
                args: vec!["reload".to_string()],
            },
            &["reload".to_string()],
        )
        .await
    } else {
        println!("daemon isn't running — this applies on the next `svrn daemon start`.");
        0
    }
}

// ── daemon status probes (best-effort; no hard dep on it being up) ────────────

async fn daemon_reachable() -> bool {
    fetch_resident().await.is_some()
}

/// Query `/status` for the set of currently-resident model paths/ids. Returns
/// `None` when the daemon isn't reachable (so callers can degrade gracefully).
/// Which models the daemon is serving, from THE wire decider —
/// `GET /v1/models`, the OpenAI-compatible listing every client of this
/// daemon routes by (sv-surface rung 2). Until 2026-09-08 this scraped
/// `/status`'s `inference.resident` with a permissive key scavenger
/// (`path`/`model`/`id`/`file`, any nesting) — a private second parser
/// beside the desktop's `/v1/models` parse, and the two could disagree
/// about the same daemon. `None` when the daemon isn't reachable.
async fn fetch_resident() -> Option<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(DAEMON_MODELS_URL).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    Some(parse_model_ids(&body))
}

/// The one parse of the `/v1/models` body shape: `{"data": [{"id": ...}]}`
/// (OpenAI-compatible — the same shape `sovereign-desktop`'s
/// `list_daemon_models` reads). Strict on purpose: a body that does not
/// speak the listing shape yields an empty set — reported as "daemon up,
/// nothing parsed" — rather than scavenging plausible keys out of an
/// unknown schema.
fn parse_model_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// True when `resident` (a path or model id) refers to the same model as the
/// config path `p` — compared by file stem so `/models/foo.gguf` matches a
/// resident id of `foo`.
fn same_model(resident: &str, p: &Path) -> bool {
    let stem = |s: &str| -> String {
        Path::new(s)
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or(s)
            .to_string()
    };
    let ps = p.file_stem().and_then(|x| x.to_str()).unwrap_or_default();
    !ps.is_empty() && stem(resident) == ps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_model_matches_by_stem() {
        assert!(same_model("/a/b/qwen.gguf", Path::new("/x/y/qwen.gguf")));
        assert!(same_model("qwen", Path::new("/x/y/qwen.gguf")));
        assert!(!same_model("other", Path::new("/x/y/qwen.gguf")));
    }

    #[test]
    fn parse_model_ids_reads_the_openai_listing_shape() {
        // THE wire decider's shape (sv-surface rung 2): the same
        // `{"data":[{"id": ...}]}` the desktop's list_daemon_models reads.
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "qwen3-1.7b", "object": "model"},
                {"id": "qwen3-embedding-0.6b", "object": "model"},
            ]
        });
        assert_eq!(
            parse_model_ids(&body),
            vec!["qwen3-1.7b", "qwen3-embedding-0.6b"]
        );
    }

    #[test]
    fn parse_model_ids_is_strict_about_unknown_shapes() {
        // The retired scavenger accepted this body (it would have found
        // "path" under any nesting); the one parser of the listing shape
        // does not — an unknown schema reads as "daemon up, nothing
        // parsed" instead of a guess (§18.3).
        let body = serde_json::json!({
            "inference": {"resident": [{"path": "/m/a.gguf"}]}
        });
        assert!(parse_model_ids(&body).is_empty());
        // Missing `data`, non-array `data`, non-string `id`: all empty,
        // never a panic, never a partial guess.
        assert!(parse_model_ids(&serde_json::json!({})).is_empty());
        assert!(parse_model_ids(&serde_json::json!({"data": 7})).is_empty());
        assert!(parse_model_ids(&serde_json::json!({"data": [{"id": 3}]})).is_empty());
    }

    #[test]
    fn models_dir_is_beside_config() {
        assert!(models_dir().ends_with("models"));
    }
}
