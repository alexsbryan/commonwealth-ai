//! `sovereign setup` — first-run onboarding.
//!
//! Flow: detect hardware → pick primary model → download three slots
//! in parallel → write `~/.config/sovereign/config.toml` → register
//! launchd/systemd service → poll the running daemon.
//!
//! Flags:
//! - `--reset`      Wipe config and re-run (uninstalls service first).
//! - `--yes`        Non-interactive — accept recommended for all prompts.
//! - `--data-dir`   Override the default `~/.sovereign` data root.

use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use sovereign_core::models_manifest::{DEFAULT_MANIFEST, SlotConfig};
use sovereign_inference::hardware::{self, HardwareProfile, ProfileName};

use crate::service_install;
use crate::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};

pub async fn run_setup(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("error: {msg}");
            print_usage();
            return 2;
        }
    };

    if opts.help {
        print_usage();
        return 0;
    }

    // ── --reset: tear down existing setup ─────────────────────────
    if opts.reset {
        eprintln!("  Resetting sovereign...");
        if let Err(e) = service_install::uninstall_service() {
            eprintln!("  warning: could not uninstall service: {e}");
        } else {
            eprintln!("    \u{2713} Service uninstalled");
        }
        if let Err(e) = SetupConfig::remove() {
            eprintln!("  warning: could not remove config: {e}");
        } else {
            eprintln!("    \u{2713} Config removed");
        }
        eprintln!();
    } else if SetupConfig::exists() {
        let path = SetupConfig::default_path();
        println!();
        println!("  Already set up. Config at {}", path.display());
        println!("  Run `sovereign status` to check or `sovereign setup --reset` to reconfigure.");
        return 0;
    }

    println!();
    println!("  Sovereign Setup");
    println!("  {}", "─".repeat(54));
    println!();

    // ── 1. Hardware detection ─────────────────────────────────────
    eprint!("  Detecting hardware... ");
    io::stderr().flush().ok();
    // Move the sync detection off the async runtime so we don't block
    // the reactor (sysinfo::System::new_all walks /proc).
    let hw = match tokio::task::spawn_blocking(HardwareProfile::detect).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: hardware detection panicked: {e}");
            return 1;
        }
    };
    let profile_name = hardware::select_profile(&hw);
    println!(
        "{}, {:.0}GB {}memory",
        hardware_label(&hw),
        hw.system_ram_gb(),
        if hw.is_unified_memory { "unified " } else { "" }
    );
    println!();

    // ── 2. Pick primary model ────────────────────────────────────
    let catalog = build_primary_catalog(&profile_name);
    if catalog.is_empty() {
        eprintln!("error: no models available in the bundled manifest for your hardware");
        return 1;
    }

    let picked = match pick_primary(&catalog, opts.yes) {
        Pick::Slot(slot) => slot,
        Pick::Byom => match prompt_byom_paths(&opts) {
            Ok(paths) => {
                return finish_with_paths(paths, &opts).await;
            }
            Err(msg) => {
                eprintln!("error: {msg}");
                return 1;
            }
        },
        Pick::Abort => {
            eprintln!("Setup cancelled.");
            return 1;
        }
    };

    // Fast + embed come from the user's own profile — not curated. If
    // the profile doesn't define one (very rare for embed on cpu_only),
    // fall back to the default profile's slot.
    let fast_slot = resolve_slot(&profile_name, SlotKind::Fast);
    let embed_slot = resolve_slot(&profile_name, SlotKind::Embed);
    let (fast_slot, embed_slot) = match (fast_slot, embed_slot) {
        (Some(f), Some(e)) => (f, e),
        _ => {
            eprintln!("error: bundled manifest is missing fast or embed slot for your profile");
            return 1;
        }
    };

    // ── 3. Download all three slots ──────────────────────────────
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let models_dir = data_dir.join("models");
    if let Err(e) = std::fs::create_dir_all(&models_dir) {
        eprintln!("error: cannot create {}: {e}", models_dir.display());
        return 1;
    }

    println!("  Downloading models...");
    println!();

    let primary_path = models_dir.join(&picked.file);
    let fast_path = models_dir.join(&fast_slot.file);
    let embed_path = models_dir.join(&embed_slot.file);

    // Primary shows progress; fast+embed run silently in parallel.
    let primary_fut = download_with_progress(&picked.hf_url, &primary_path, &picked.file);
    let fast_fut = download_silent(&fast_slot.hf_url, &fast_path);
    let embed_fut = download_silent(&embed_slot.hf_url, &embed_path);

    let (primary_res, fast_res, embed_res) = tokio::join!(primary_fut, fast_fut, embed_fut);

    if let Err(e) = primary_res {
        eprintln!("  \u{2717} Primary: {e}");
        return 1;
    }
    if let Err(e) = fast_res {
        eprintln!("  \u{2717} Fast: {e}");
        return 1;
    } else {
        println!("    \u{2713} {}", fast_slot.file);
    }
    if let Err(e) = embed_res {
        eprintln!("  \u{2717} Embed: {e}");
        return 1;
    } else {
        println!("    \u{2713} {}", embed_slot.file);
    }

    println!();
    println!("  \u{2713} Models ready");

    finish_with_paths(
        ModelPaths { primary: primary_path, fast: fast_path, embed: embed_path },
        &opts,
    )
    .await
}

// ─── Arg parsing ──────────────────────────────────────────────────

struct Opts {
    reset: bool,
    yes: bool,
    data_dir: Option<PathBuf>,
    help: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts { reset: false, yes: false, data_dir: None, help: false };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reset" => opts.reset = true,
            "--yes" | "-y" => opts.yes = true,
            "--data-dir" => {
                i += 1;
                opts.data_dir = Some(PathBuf::from(
                    args.get(i).ok_or_else(|| "--data-dir needs a path".to_string())?,
                ));
            }
            "--help" | "-h" => opts.help = true,
            other => return Err(format!("unknown flag '{other}'")),
        }
        i += 1;
    }
    Ok(opts)
}

fn print_usage() {
    eprintln!(
        "sovereign setup — first-run onboarding\n\
         \n\
         Detects hardware, downloads models, writes ~/.config/sovereign/config.toml,\n\
         and registers the daemon with launchd (macOS) or systemd (Linux).\n\
         \n\
         Flags:\n  \
         --reset         Wipe config and re-run (uninstalls service first)\n  \
         --yes, -y       Non-interactive; accept recommended choices\n  \
         --data-dir <p>  Override the default data root (~/.sovereign)\n  \
         --help, -h      Show this message"
    );
}

// ─── Model catalog + picker ───────────────────────────────────────

#[derive(Clone)]
struct PrimaryOption {
    /// Profile this slot was drawn from — `Some("high")` etc. `None`
    /// means "recommended for your hardware" (the pick-by-default row).
    #[allow(dead_code)]
    profile: &'static str,
    slot: SlotConfig,
    recommended: bool,
}

impl std::ops::Deref for PrimaryOption {
    type Target = SlotConfig;
    fn deref(&self) -> &Self::Target { &self.slot }
}

enum Pick {
    Slot(SlotConfig),
    Byom,
    Abort,
}

enum SlotKind {
    Fast,
    Embed,
}

/// Build the curated list of primary-model options for the user's
/// profile tier: the profile's own `thoughtful` slot (marked
/// recommended), plus each smaller profile's thoughtful slot so the
/// user can opt into a faster / smaller model if they prefer.
fn build_primary_catalog(profile: &ProfileName) -> Vec<PrimaryOption> {
    // Walk from very_high down to cpu_only; include a tier iff it's
    // at-or-below the user's tier (so a "default" machine doesn't see
    // the 27B or 35B-A3B thoughtful slots).
    let order = [
        ("very_high", ProfileName::VeryHigh),
        ("high", ProfileName::High),
        ("default", ProfileName::Default),
        ("low_mem", ProfileName::LowMem),
        ("cpu_only", ProfileName::CpuOnly),
    ];
    let max_tier_rank = tier_rank(profile);

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (name, p) in order {
        if tier_rank(&p) > max_tier_rank {
            continue; // too big for this hardware
        }
        let Some(prof_cfg) = DEFAULT_MANIFEST.profiles.get(name) else {
            continue;
        };
        let Some(slot) = prof_cfg.thoughtful.clone() else {
            continue;
        };
        // Dedupe by base_name (or filename fallback) so the list is
        // compact when neighbouring tiers share a model.
        let key = if slot.base_name.is_empty() { slot.file.clone() } else { slot.base_name.clone() };
        if !seen.insert(key) {
            continue;
        }
        out.push(PrimaryOption {
            profile: name,
            recommended: &p == profile,
            slot,
        });
    }
    out
}

fn tier_rank(p: &ProfileName) -> u8 {
    match p {
        ProfileName::CpuOnly => 0,
        ProfileName::LowMem => 1,
        ProfileName::Default => 2,
        ProfileName::High => 3,
        ProfileName::VeryHigh => 4,
    }
}

fn resolve_slot(profile: &ProfileName, kind: SlotKind) -> Option<SlotConfig> {
    let profile_name = match *profile {
        ProfileName::CpuOnly => "cpu_only",
        ProfileName::LowMem => "low_mem",
        ProfileName::Default => "default",
        ProfileName::High => "high",
        ProfileName::VeryHigh => "very_high",
    };
    let prof_cfg = DEFAULT_MANIFEST.profiles.get(profile_name)?;
    let slot = match kind {
        SlotKind::Fast => prof_cfg.fast.clone(),
        SlotKind::Embed => prof_cfg.embed.clone(),
    };
    slot.or_else(|| {
        // Fallback: the `default` profile always has all three slots.
        let default = DEFAULT_MANIFEST.profiles.get("default")?;
        match kind {
            SlotKind::Fast => default.fast.clone(),
            SlotKind::Embed => default.embed.clone(),
        }
    })
}

/// Render the numbered picker, handle the `[b]` BYOM branch, and return
/// the chosen slot. In `--yes` mode, auto-picks the recommended row.
fn pick_primary(catalog: &[PrimaryOption], yes: bool) -> Pick {
    println!("  Pick your primary model:");
    println!();
    println!("    #   Model                          Size     Notes");
    for (i, opt) in catalog.iter().enumerate() {
        let tag = if opt.recommended { "← recommended" } else { "" };
        println!(
            "    {}   {:30}  {:>5.1} GB {tag}",
            i + 1,
            display_name(&opt.slot),
            opt.size_gb,
        );
    }
    println!();
    println!("    [b] Bring my own GGUF files");
    println!();

    if yes {
        let rec = catalog.iter().find(|o| o.recommended).or_else(|| catalog.first());
        return match rec {
            Some(o) => Pick::Slot(o.slot.clone()),
            None => Pick::Abort,
        };
    }

    loop {
        eprint!("  \u{276f} ");
        io::stderr().flush().ok();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
            return Pick::Abort;
        }
        let trimmed = line.trim().to_lowercase();

        if trimmed.is_empty() {
            // Enter = recommended
            if let Some(o) = catalog.iter().find(|o| o.recommended) {
                return Pick::Slot(o.slot.clone());
            }
            return Pick::Slot(catalog[0].slot.clone());
        }
        if trimmed == "b" {
            return Pick::Byom;
        }
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= catalog.len() {
                return Pick::Slot(catalog[n - 1].slot.clone());
            }
        }
        eprintln!("  (Enter a number 1..{}, 'b', or press enter for recommended.)", catalog.len());
    }
}

fn display_name(slot: &SlotConfig) -> String {
    if !slot.base_name.is_empty() {
        format!("{} {}", slot.base_name, slot.quant)
    } else {
        slot.file.trim_end_matches(".gguf").to_string()
    }
}

// ─── BYOM branch ───────────────────────────────────────────────────

struct ModelPaths {
    primary: PathBuf,
    fast: PathBuf,
    embed: PathBuf,
}

fn prompt_byom_paths(opts: &Opts) -> Result<ModelPaths, String> {
    if opts.yes {
        return Err("--yes cannot be combined with BYOM; choose a numbered option instead".into());
    }
    println!();
    println!("  Bring your own GGUF files. Leave blank to cancel.");

    let primary = prompt_path("  Primary (thoughtful) GGUF path: ")?;
    let fast = prompt_path("  Fast GGUF path (blank to use default recommended): ")?;
    let embed = prompt_path("  Embed GGUF path (blank to use default recommended): ")?;
    Ok(ModelPaths {
        primary: primary.ok_or_else(|| "primary path required".to_string())?,
        fast: fast.unwrap_or_else(|| PathBuf::from("TBD-fast")),
        embed: embed.unwrap_or_else(|| PathBuf::from("TBD-embed")),
    })
}

fn prompt_path(label: &str) -> Result<Option<PathBuf>, String> {
    eprint!("{label}");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let expanded = if trimmed.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(trimmed.trim_start_matches("~/")))
            .unwrap_or_else(|| PathBuf::from(trimmed))
    } else {
        PathBuf::from(trimmed)
    };
    if !expanded.exists() {
        return Err(format!("file not found: {}", expanded.display()));
    }
    Ok(Some(expanded))
}

// ─── Downloaders ───────────────────────────────────────────────────

/// Download `url` to `dest`, resuming a partial `.part` sibling if one
/// exists. Streams bytes through a pretty progress bar showing the
/// filename and percentage. Overwrite semantics: if `dest` already
/// exists and has non-zero length, treat it as complete (skip).
async fn download_with_progress(url: &str, dest: &Path, display: &str) -> Result<(), String> {
    if has_content(dest) {
        println!("    \u{2713} {display} (already present)");
        return Ok(());
    }
    let part = dest.with_extension("part");
    let resume_from = part.metadata().map(|m| m.len()).unwrap_or(0);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header("Range", format!("bytes={resume_from}-"));
    }
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(format!("GET {url}: {}", resp.status()));
    }
    let total = resp.content_length().map(|c| c + resume_from);

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(resume_from > 0)
        .write(true)
        .truncate(resume_from == 0)
        .open(&part)
        .map_err(|e| format!("open {}: {e}", part.display()))?;

    let mut stream = resp.bytes_stream();
    let mut downloaded = resume_from;
    let mut last_print = std::time::Instant::now();
    eprint!("    {display}  ");
    io::stderr().flush().ok();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("stream: {e}"))?;
        std::io::Write::write_all(&mut file, &bytes)
            .map_err(|e| format!("write {}: {e}", part.display()))?;
        downloaded += bytes.len() as u64;
        if last_print.elapsed() > Duration::from_millis(250) {
            print_progress(display, downloaded, total);
            last_print = std::time::Instant::now();
        }
    }
    print_progress(display, downloaded, total);
    eprintln!();
    drop(file);

    std::fs::rename(&part, dest)
        .map_err(|e| format!("rename {} -> {}: {e}", part.display(), dest.display()))?;
    Ok(())
}

/// Same as `download_with_progress` but doesn't print per-chunk
/// progress. Used for fast + embed (we show a single ✓ when done).
async fn download_silent(url: &str, dest: &Path) -> Result<(), String> {
    if has_content(dest) {
        return Ok(());
    }
    let part = dest.with_extension("part");
    let resume_from = part.metadata().map(|m| m.len()).unwrap_or(0);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header("Range", format!("bytes={resume_from}-"));
    }
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(format!("GET {url}: {}", resp.status()));
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(resume_from > 0)
        .write(true)
        .truncate(resume_from == 0)
        .open(&part)
        .map_err(|e| format!("open {}: {e}", part.display()))?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("stream: {e}"))?;
        std::io::Write::write_all(&mut file, &bytes)
            .map_err(|e| format!("write: {e}"))?;
    }
    drop(file);
    std::fs::rename(&part, dest).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

fn has_content(p: &Path) -> bool {
    p.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

fn print_progress(label: &str, done: u64, total: Option<u64>) {
    const BAR_WIDTH: usize = 20;
    let (pct_f, pct_s) = match total {
        Some(t) if t > 0 => {
            let p = (done as f64 / t as f64).clamp(0.0, 1.0);
            (p, format!("{:3.0}%", p * 100.0))
        }
        _ => (0.0, "--%".to_string()),
    };
    let filled = (pct_f * BAR_WIDTH as f64) as usize;
    let bar: String = (0..BAR_WIDTH)
        .map(|i| if i < filled { '\u{2588}' } else { '\u{2591}' })
        .collect();
    let done_mb = done as f64 / 1_048_576.0;
    let total_mb = total.map(|t| t as f64 / 1_048_576.0);
    let size = match total_mb {
        Some(t) => format!("{done_mb:>6.0}/{t:.0} MB"),
        None => format!("{done_mb:>6.0} MB"),
    };
    eprint!("\r    {label:<40}  [{bar}] {pct_s}  {size}");
    io::stderr().flush().ok();
}

// ─── Finish: write config, install service, bring daemon up ─────────

async fn finish_with_paths(paths: ModelPaths, opts: &Opts) -> i32 {
    // ── Write config ─────────────────────────────────────────────
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".sovereign"));

    let cfg = SetupConfig {
        models: ModelsSection {
            primary: paths.primary,
            fast: paths.fast,
            embed: paths.embed,
        },
        daemon: DaemonSection::default(),
        data: DataSection { dir: data_dir.clone() },
    };

    let config_path = match cfg.save() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!("    \u{2713} Wrote {}", config_path.display());

    // ── Install service ──────────────────────────────────────────
    let bin_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  warning: cannot resolve current binary path: {e}");
            eprintln!("  skipping service registration; run `sovereign daemon run` manually.");
            return 0;
        }
    };
    match service_install::install_service(&bin_path) {
        Ok(()) => println!("    \u{2713} Service registered"),
        Err(e) => {
            eprintln!("  warning: service registration failed: {e}");
            eprintln!("  run `sovereign daemon run` manually to start the daemon.");
            return 0;
        }
    }

    // ── Wait for daemon to come up ───────────────────────────────
    eprint!("  Waiting for daemon to come up...");
    io::stderr().flush().ok();
    if wait_for_daemon(cfg.daemon.client_port, Duration::from_secs(30)).await {
        println!(" ready");
    } else {
        println!();
        eprintln!("  warning: daemon didn't respond within 30s.");
        eprintln!("  check logs at {}/logs/daemon.err", data_dir.display());
        return 0;
    }

    // ── Banner ───────────────────────────────────────────────────
    println!();
    println!("  \u{2713} Mesh running — 1 node (you)");
    println!("  \u{2713} Endpoint: localhost:{}/v1", cfg.daemon.client_port);
    println!();
    println!("  Add to opencode — .opencode/config.json:");
    println!(r#"    {{
      "mcp": {{ "servers": {{ "sovereign": {{ "type": "http", "url": "http://localhost:{port}/mcp" }} }} }},
      "provider": {{
        "commonwealth": {{
          "npm": "@ai-sdk/openai-compatible",
          "options": {{ "baseURL": "http://localhost:{port}/v1" }}
        }}
      }}
    }}"#,
        port = cfg.daemon.client_port);

    let _ = Arc::new(()); // placeholder; Arc usage removed post-refactor
    0
}

async fn wait_for_daemon(port: u16, timeout: Duration) -> bool {
    let url = format!("http://localhost:{port}/v1/models");
    let deadline = std::time::Instant::now() + timeout;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    while std::time::Instant::now() < deadline {
        if client.get(&url).send().await.map(|r| r.status().is_success()).unwrap_or(false) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

fn hardware_label(hw: &HardwareProfile) -> String {
    match &hw.gpu_name {
        Some(name) => name.clone(),
        None if hw.is_unified_memory => "Apple Silicon".to_string(),
        None => "CPU-only system".to_string(),
    }
}

#[allow(dead_code)]
fn _tty_gate() -> bool {
    io::stdin().is_terminal()
}
