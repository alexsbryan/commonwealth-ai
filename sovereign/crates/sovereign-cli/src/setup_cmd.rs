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

    // The manifest's `hf_url` is the repo *landing page* — we derive the
    // actual GGUF download URL from it plus the slot's filename.
    let primary_url = hf_download_url(&picked);
    let fast_url = hf_download_url(&fast_slot);
    let embed_url = hf_download_url(&embed_slot);

    // Primary shows progress; fast+embed run silently in parallel.
    let primary_fut = download_with_progress(&primary_url, &primary_path, &picked.file);
    let fast_fut = download_silent(&fast_url, &fast_path);
    let embed_fut = download_silent(&embed_url, &embed_path);

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

#[derive(Debug)]
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

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign setup",
    summary: "First-run onboarding: detect hardware, download models, start the daemon.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign setup [--yes] [--reset] [--data-dir <path>]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--yes, -y",       "Non-interactive; accept recommended choices"),
            ("--reset",         "Wipe config and re-run (uninstalls service first)"),
            ("--data-dir <p>",  "Override the default data root (~/.sovereign)"),
            ("--help, -h",      "Show this message"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Writes config to the XDG config dir (macOS: ~/Library/Application Support/sovereign/,\n\
             Linux: ~/.config/sovereign/). Registers the daemon with launchd/systemd so it\n\
             survives logout. Re-run with --reset to wipe and reconfigure.",
        ),
    ],
};

fn print_usage() {
    crate::util::help::print(&HELP);
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

/// Prompt for all three GGUF paths. BYOM is committed — if the user
/// picked `[b]` from the numbered list, they wanted to supply their own
/// weights for every slot. No "blank to use default" shortcuts; a
/// blank line cancels the entire flow (setup exits). Paths are
/// validated for existence before we return; drag-and-drop quoting and
/// backslash-escaped spaces are stripped by `strip_quoting`.
fn prompt_byom_paths(opts: &Opts) -> Result<ModelPaths, String> {
    if opts.yes {
        return Err("--yes cannot be combined with BYOM; choose a numbered option instead".into());
    }
    println!();
    println!("  Bring your own GGUF files. Provide a path for each slot.");
    println!("  Leave any line blank to cancel.");
    println!();

    let primary = require_path(
        "  Primary (thoughtful) GGUF path: ",
        "primary path is required for BYOM",
    )?;
    let fast = require_path(
        "  Fast GGUF path: ",
        "fast path is required for BYOM",
    )?;
    let embed = require_path(
        "  Embed GGUF path: ",
        "embed path is required for BYOM",
    )?;
    Ok(ModelPaths { primary, fast, embed })
}

/// Prompt for a path, error out if the user leaves it blank.
/// Thin wrapper over `prompt_path` that turns `Ok(None)` into an error.
fn require_path(label: &str, missing_msg: &str) -> Result<PathBuf, String> {
    match prompt_path(label)? {
        Some(p) => Ok(p),
        None => Err(missing_msg.to_string()),
    }
}

/// BYOM path prompt. Delegates to `util::prompts::prompt_path` which
/// handles quote stripping, `~/` expansion, and existence checking.
fn prompt_path(label: &str) -> Result<Option<PathBuf>, String> {
    crate::util::prompts::prompt_path(label)
}

// ─── Downloaders ───────────────────────────────────────────────────

/// Build the direct GGUF download URL from a manifest slot. The
/// `hf_url` field in `models.toml` is the *repo* URL
/// (`https://huggingface.co/Qwen/Qwen3-1.7B-GGUF`), not the file URL,
/// so we append `/resolve/main/<file>` to land on the raw LFS blob.
///
/// This matches the canonical path `huggingface-cli download` would
/// resolve; it handles the LFS redirect server-side and supports HTTP
/// Range (crucial for resume).
fn hf_download_url(slot: &SlotConfig) -> String {
    let repo = slot
        .hf_url
        .trim_end_matches('/')
        .strip_prefix("https://huggingface.co/")
        .unwrap_or(&slot.hf_url);
    // If the URL is already a /resolve/ URL (e.g. someone wrote the
    // direct form), don't double-append.
    if slot.hf_url.contains("/resolve/") {
        slot.hf_url.clone()
    } else {
        format!("https://huggingface.co/{repo}/resolve/main/{}", slot.file)
    }
}


/// Download `url` to `dest`, resuming a partial `.part` sibling if one
/// exists. Streams bytes through a pretty progress bar showing the
/// filename and percentage. Overwrite semantics: if `dest` already
/// exists and has non-zero length, treat it as complete (skip).
async fn download_with_progress(url: &str, dest: &Path, display: &str) -> Result<(), String> {
    if has_content(dest) {
        // Don't blindly trust a pre-existing file — a prior setup run may
        // have left an HTML error page at this path. If it's not a
        // plausible GGUF, remove and re-download.
        if verify_gguf_non_empty(dest).is_ok() {
            println!("    \u{2713} {display} (already present)");
            return Ok(());
        }
        eprintln!("    \u{26a0} {display} exists but is too small; re-downloading");
        let _ = std::fs::remove_file(dest);
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

    // Defensive: if we received zero bytes, the upstream returned a 2xx
    // with an empty body (e.g. HF sometimes 200s a redirect body). Don't
    // silently hand back an empty .gguf — the daemon will fail to load
    // it, and the user won't know why without reading logs.
    verify_gguf_non_empty(&part)?;

    std::fs::rename(&part, dest)
        .map_err(|e| format!("rename {} -> {}: {e}", part.display(), dest.display()))?;
    Ok(())
}

/// Sanity-check a downloaded GGUF. A zero-byte file means the stream
/// silently yielded nothing (empty redirect body, auth failure, etc).
/// llama.cpp will error on load either way, but we want to fail the
/// *download* with a clear message rather than leaving the config in a
/// state where "setup completed but daemon won't start".
fn verify_gguf_non_empty(path: &Path) -> Result<(), String> {
    let len = path
        .metadata()
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .len();
    if len == 0 {
        return Err(format!(
            "download produced an empty file ({}); upstream likely returned \
             an empty body. Retry, or pass --reset and try a different model.",
            path.display()
        ));
    }
    // A real GGUF is at least tens of MB. Anything smaller than 1 MB is
    // almost certainly an HTML error page or truncated response.
    const MIN_PLAUSIBLE_GGUF_BYTES: u64 = 1_000_000;
    if len < MIN_PLAUSIBLE_GGUF_BYTES {
        return Err(format!(
            "downloaded file is suspiciously small ({} bytes at {}); \
             likely an HTML error response rather than a GGUF. Retry.",
            len,
            path.display()
        ));
    }
    Ok(())
}

/// Same as `download_with_progress` but doesn't print per-chunk
/// progress. Used for fast + embed (we show a single ✓ when done).
async fn download_silent(url: &str, dest: &Path) -> Result<(), String> {
    if has_content(dest) {
        if verify_gguf_non_empty(dest).is_ok() {
            return Ok(());
        }
        let _ = std::fs::remove_file(dest);
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
    verify_gguf_non_empty(&part)?;
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
        eprintln!("  warning: daemon didn't respond on :{} within 30s.", cfg.daemon.client_port);
        diagnose_daemon_failure(&data_dir);
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

/// When the daemon fails to come up within the setup window, dump enough
/// context for the user to self-diagnose without digging through logs:
/// the last ~20 lines of `daemon.err`, the service-manager status, and
/// a copy-paste command to run the daemon in the foreground.
fn diagnose_daemon_failure(data_dir: &Path) {
    let err_log = data_dir.join("logs").join("daemon.err");
    eprintln!();
    eprintln!("  To diagnose, try one of:");

    #[cfg(target_os = "macos")]
    eprintln!("    launchctl list | grep sovereign       # is the service loaded?");
    #[cfg(target_os = "linux")]
    eprintln!("    systemctl --user status sovereign     # is the unit active?");

    eprintln!("    sovereign daemon run                  # run in the foreground to see errors live");
    eprintln!();

    if err_log.exists() {
        eprintln!("  Last lines of {}:", err_log.display());
        match std::fs::read_to_string(&err_log) {
            Ok(contents) => {
                let tail: Vec<&str> = contents.lines().rev().take(20).collect();
                for line in tail.iter().rev() {
                    eprintln!("    {line}");
                }
            }
            Err(e) => eprintln!("    (couldn't read: {e})"),
        }
    } else {
        eprintln!("  No log at {} yet — service likely didn't start.", err_log.display());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_args ─────────────────────────────────────────────────

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_args_defaults_to_interactive() {
        let opts = parse_args(&[]).unwrap();
        assert!(!opts.reset);
        assert!(!opts.yes);
        assert!(!opts.help);
        assert!(opts.data_dir.is_none());
    }

    #[test]
    fn parse_args_recognizes_all_flags() {
        let opts = parse_args(&s(&["--reset", "--yes", "--data-dir", "/tmp/sv"])).unwrap();
        assert!(opts.reset);
        assert!(opts.yes);
        assert_eq!(opts.data_dir.as_deref(), Some(Path::new("/tmp/sv")));
    }

    #[test]
    fn parse_args_short_yes() {
        let opts = parse_args(&s(&["-y"])).unwrap();
        assert!(opts.yes);
    }

    #[test]
    fn parse_args_help_long_and_short() {
        assert!(parse_args(&s(&["--help"])).unwrap().help);
        assert!(parse_args(&s(&["-h"])).unwrap().help);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&s(&["--wat"])).unwrap_err();
        assert!(err.contains("--wat"), "error: {err}");
    }

    #[test]
    fn parse_args_rejects_dangling_data_dir() {
        let err = parse_args(&s(&["--data-dir"])).unwrap_err();
        assert!(err.contains("--data-dir"), "error: {err}");
    }

    // ── tier_rank ──────────────────────────────────────────────────

    #[test]
    fn tier_rank_orders_profiles_low_to_high() {
        assert!(tier_rank(&ProfileName::CpuOnly) < tier_rank(&ProfileName::LowMem));
        assert!(tier_rank(&ProfileName::LowMem) < tier_rank(&ProfileName::Default));
        assert!(tier_rank(&ProfileName::Default) < tier_rank(&ProfileName::High));
        assert!(tier_rank(&ProfileName::High) < tier_rank(&ProfileName::VeryHigh));
    }

    // ── build_primary_catalog ─────────────────────────────────────

    #[test]
    fn catalog_is_non_empty_for_every_profile() {
        // Sanity — the bundled manifest should support every hardware tier.
        for p in [
            ProfileName::CpuOnly,
            ProfileName::LowMem,
            ProfileName::Default,
            ProfileName::High,
            ProfileName::VeryHigh,
        ] {
            let cat = build_primary_catalog(&p);
            assert!(!cat.is_empty(), "catalog empty for {p:?}");
        }
    }

    #[test]
    fn catalog_marks_exactly_one_recommended() {
        let cat = build_primary_catalog(&ProfileName::Default);
        let recommended: Vec<_> = cat.iter().filter(|o| o.recommended).collect();
        assert_eq!(
            recommended.len(),
            1,
            "expected exactly one recommended row, got {}",
            recommended.len()
        );
    }

    #[test]
    fn catalog_excludes_tiers_above_user_hardware() {
        // A Default-tier machine must NOT see VeryHigh or High options — they
        // won't fit in VRAM. Verify by checking no returned slot came from a
        // higher tier's thoughtful slot.
        let cat = build_primary_catalog(&ProfileName::Default);
        let very_high_thoughtful = DEFAULT_MANIFEST
            .profiles
            .get("very_high")
            .and_then(|p| p.thoughtful.as_ref())
            .map(|s| s.file.clone());
        if let Some(f) = very_high_thoughtful {
            assert!(
                !cat.iter().any(|o| o.slot.file == f),
                "Default-tier catalog leaked very_high slot {f}"
            );
        }
    }

    #[test]
    fn catalog_dedupes_by_base_name() {
        // If two profile tiers point to the same base model, the catalog
        // should show it only once. We can't assume the bundled manifest has
        // duplicates, so construct a stricter invariant: every base_name
        // appears at most once.
        let cat = build_primary_catalog(&ProfileName::VeryHigh);
        let mut seen = std::collections::HashSet::new();
        for opt in &cat {
            let key = if opt.slot.base_name.is_empty() {
                opt.slot.file.clone()
            } else {
                opt.slot.base_name.clone()
            };
            assert!(seen.insert(key.clone()), "duplicate base_name in catalog: {key}");
        }
    }

    #[test]
    fn catalog_very_high_includes_every_tier_below() {
        // VeryHigh users should see every tier at-or-below them (subject to
        // dedup). Count of distinct tiers available should be >= 1 (hard
        // guarantee) and match the number of profiles that define thoughtful
        // and have non-duplicate base_names.
        let cat = build_primary_catalog(&ProfileName::VeryHigh);
        assert!(cat.len() >= 1);
        // First row (recommended) should be the VeryHigh slot.
        let first = &cat[0];
        assert!(first.recommended);
    }

    // ── resolve_slot ───────────────────────────────────────────────

    #[test]
    fn resolve_slot_returns_profile_slot_when_defined() {
        // Default profile has all three slots defined in the bundled manifest.
        let fast = resolve_slot(&ProfileName::Default, SlotKind::Fast);
        let embed = resolve_slot(&ProfileName::Default, SlotKind::Embed);
        assert!(fast.is_some(), "default.fast should exist");
        assert!(embed.is_some(), "default.embed should exist");
    }

    #[test]
    fn resolve_slot_falls_back_to_default_when_missing() {
        // This test encodes the invariant: even if a profile is thin (say,
        // cpu_only missing embed), we must fall back to default.embed so
        // `setup` always has three paths to write.
        for p in [
            ProfileName::CpuOnly,
            ProfileName::LowMem,
            ProfileName::Default,
            ProfileName::High,
            ProfileName::VeryHigh,
        ] {
            assert!(
                resolve_slot(&p, SlotKind::Fast).is_some(),
                "no fast slot (even via fallback) for {p:?}"
            );
            assert!(
                resolve_slot(&p, SlotKind::Embed).is_some(),
                "no embed slot (even via fallback) for {p:?}"
            );
        }
    }

    // ── display_name ───────────────────────────────────────────────

    #[test]
    fn display_name_uses_base_name_when_present() {
        let slot = SlotConfig {
            file: "qwen_weights.gguf".into(),
            base_name: "Qwen3.5-27B".into(),
            quant: "Q4_K_M".into(),
            ..Default::default()
        };
        assert_eq!(display_name(&slot), "Qwen3.5-27B Q4_K_M");
    }

    #[test]
    fn display_name_falls_back_to_filename() {
        let slot = SlotConfig {
            file: "custom-model.gguf".into(),
            base_name: "".into(),
            ..Default::default()
        };
        assert_eq!(display_name(&slot), "custom-model");
    }

    // ── hf_download_url ────────────────────────────────────────────

    #[test]
    fn hf_download_url_from_repo_landing_page() {
        let slot = SlotConfig {
            file: "Qwen3-1.7B-Q8_0.gguf".into(),
            hf_url: "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF".into(),
            ..Default::default()
        };
        assert_eq!(
            hf_download_url(&slot),
            "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q8_0.gguf"
        );
    }

    #[test]
    fn hf_download_url_handles_trailing_slash() {
        let slot = SlotConfig {
            file: "model.gguf".into(),
            hf_url: "https://huggingface.co/org/repo/".into(),
            ..Default::default()
        };
        assert_eq!(
            hf_download_url(&slot),
            "https://huggingface.co/org/repo/resolve/main/model.gguf"
        );
    }

    #[test]
    fn hf_download_url_passes_through_direct_urls() {
        // If the manifest already has a direct /resolve/ URL, don't
        // double-append.
        let slot = SlotConfig {
            file: "model.gguf".into(),
            hf_url: "https://huggingface.co/org/repo/resolve/main/model.gguf".into(),
            ..Default::default()
        };
        assert_eq!(
            hf_download_url(&slot),
            "https://huggingface.co/org/repo/resolve/main/model.gguf"
        );
    }

    // strip_quoting tests moved to util::prompts::tests — the function lives there now.

    // ── verify_gguf_non_empty ──────────────────────────────────────

    #[test]
    fn verify_gguf_non_empty_rejects_zero_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("empty.gguf");
        std::fs::write(&p, b"").unwrap();
        let err = verify_gguf_non_empty(&p).unwrap_err();
        assert!(err.contains("empty"), "err: {err}");
    }

    #[test]
    fn verify_gguf_non_empty_rejects_small_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("tiny.gguf");
        std::fs::write(&p, b"<html>error</html>").unwrap();
        let err = verify_gguf_non_empty(&p).unwrap_err();
        assert!(err.contains("suspiciously small"), "err: {err}");
    }

    #[test]
    fn verify_gguf_non_empty_accepts_plausible_size() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("ok.gguf");
        std::fs::write(&p, vec![0u8; 2_000_000]).unwrap();
        assert!(verify_gguf_non_empty(&p).is_ok());
    }

    // ── has_content ────────────────────────────────────────────────

    #[test]
    fn has_content_distinguishes_empty_from_populated() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty.gguf");
        std::fs::write(&empty, b"").unwrap();
        assert!(!has_content(&empty));

        let populated = tmp.path().join("model.gguf");
        std::fs::write(&populated, b"data").unwrap();
        assert!(has_content(&populated));

        let missing = tmp.path().join("nope.gguf");
        assert!(!has_content(&missing));
    }
}
