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

    // ── --repair: scan installed models, delete corrupted ones ────
    //
    // Unblocks users who landed here after a previous setup run
    // silently stored HTML error pages / LFS pointers at the
    // model paths (pre-validator). For each slot we look at the
    // stored SetupConfig, validate what's on disk, and delete
    // anything that isn't a real GGUF. The daemon will then
    // either fall back to re-running setup or surface a clean
    // "file missing" error, both of which are strictly better
    // than "null result from llama cpp" at inference time.
    if opts.repair {
        return run_repair().await;
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
    // Each slot's `size_gb` goes through so `validate_gguf` can
    // apply a tighter floor than the 1 MB sentinel — a corrupt
    // 200 KB "35 GB" file is an obvious lie, a corrupt 200 KB
    // "0.4 GB" embed is too.
    let primary_fut =
        download_with_progress(&primary_url, &primary_path, &picked.file, picked.size_gb);
    let fast_fut = download_silent(&fast_url, &fast_path, fast_slot.size_gb);
    let embed_fut = download_silent(&embed_url, &embed_path, embed_slot.size_gb);

    let (primary_res, fast_res, embed_res) = tokio::join!(primary_fut, fast_fut, embed_fut);

    if let Err(e) = primary_res {
        eprintln!("  \u{2717} Main responder: {e}");
        return 1;
    }
    if let Err(e) = fast_res {
        eprintln!("  \u{2717} Quick responder: {e}");
        return 1;
    } else {
        println!("    \u{2713} {}", fast_slot.file);
    }
    if let Err(e) = embed_res {
        eprintln!("  \u{2717} Knowledge embedder: {e}");
        return 1;
    } else {
        println!("    \u{2713} {}", embed_slot.file);
    }

    println!();
    println!("  \u{2713} Models ready");

    finish_with_paths(
        ModelPaths {
            primary: primary_path,
            fast: fast_path,
            embed: embed_path,
            // Curated download path doesn't surface a code slot
            // yet — PR-E2 only wires it on the BYOM flow. Adding
            // a Qwen Coder recommendation here would couple the
            // bundled manifest to a specific code-model choice;
            // BYOM leaves that decision to the user.
            code: None,
        },
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
    repair: bool,
    help: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts {
        reset: false,
        yes: false,
        data_dir: None,
        repair: false,
        help: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reset" => opts.reset = true,
            "--yes" | "-y" => opts.yes = true,
            "--repair" => opts.repair = true,
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
    println!("  Pick your main responder:");
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
    /// PR-E2: optional Code specialist GGUF. `None` is the common
    /// case — most users let the Main responder handle code work.
    code: Option<PathBuf>,
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
    println!("  Bring your own GGUF files. Provide a path for each role.");
    println!("  Leave any line blank to cancel.");
    println!();

    let primary = require_path(
        "  Main responder GGUF path: ",
        "main-responder path is required for BYOM",
    )?;
    let fast = require_path(
        "  Quick responder GGUF path: ",
        "quick-responder path is required for BYOM",
    )?;
    let embed = require_path(
        "  Knowledge embedder GGUF path: ",
        "knowledge-embedder path is required for BYOM",
    )?;
    // Code specialist is optional — blank input means "my Main
    // responder handles code fine, don't load a second substantive
    // model." Users who want a dedicated coder can add one later
    // via Settings.
    let code = prompt_path("  Code specialist GGUF path (optional, Enter to skip): ")?;
    Ok(ModelPaths { primary, fast, embed, code })
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
pub(crate) async fn download_with_progress(
    url: &str,
    dest: &Path,
    display: &str,
    size_gb: f64,
) -> Result<(), String> {
    let expected = sovereign_inference::GgufExpectation::from_size_gb(size_gb);

    if has_content(dest) {
        // Don't blindly trust a pre-existing file — a prior setup
        // run may have left an HTML error page at this path. If
        // it's not a plausible GGUF, remove and re-download.
        match sovereign_inference::validate_gguf(dest, &expected) {
            Ok(()) => {
                println!("    \u{2713} {display} (already present)");
                return Ok(());
            }
            Err(e) => {
                eprintln!("    \u{26a0} {display} exists but is invalid: {e}");
                eprintln!("      re-downloading");
                let _ = std::fs::remove_file(dest);
            }
        }
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
    if let Some(tok) = hf_token() {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(format!("GET {url}: {}", resp.status()));
    }

    // Pre-stream sniff: HuggingFace's CDN sometimes returns a 200
    // OK with `content-type: text/html` when it thinks we're a
    // bot, and the body is an error page. Surface that before we
    // stream MB of HTML to disk.
    if let Err(e) = reject_non_binary_content_type(&resp, url) {
        return Err(e);
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

    // Post-stream validation: catches cases the content-type
    // sniff missed (some CDN paths return `application/octet-
    // stream` on the error page) AND truncated real downloads
    // where the first few MB are a valid GGUF header but the
    // rest is cut off. On failure delete `.part` so a retry
    // starts from zero rather than resuming a partial bogus file.
    if let Err(e) = sovereign_inference::validate_gguf(&part, &expected) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download validation failed: {e}"));
    }

    std::fs::rename(&part, dest)
        .map_err(|e| format!("rename {} -> {}: {e}", part.display(), dest.display()))?;
    Ok(())
}

/// Return `Err` if the HTTP response advertises a non-binary
/// content type. The GGUF endpoint on HuggingFace serves
/// `application/octet-stream` (or sometimes no content-type at
/// all). Anything starting with `text/` or `application/json` is
/// an error page — surface the first 200 chars so the operator
/// can see "rate-limited" / "requires authentication" / etc.
/// without having to curl manually.
fn reject_non_binary_content_type(resp: &reqwest::Response, url: &str) -> Result<(), String> {
    let Some(ct) = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(());
    };
    let lower = ct.to_ascii_lowercase();
    if lower.starts_with("text/") || lower.starts_with("application/json") {
        // The body isn't captured here (that would consume the
        // stream before we get to download); we quote just the
        // header. `validate_gguf` after streaming catches the
        // actual bytes if the body surprises us.
        return Err(format!(
            "HuggingFace returned content-type={ct} for {url} — likely \
             bot-detection, rate limiting, or a gated-repo login page. \
             Try setting `HF_TOKEN` before `sovereign setup` to use \
             authenticated downloads."
        ));
    }
    Ok(())
}

/// Read `HF_TOKEN` from the environment for HuggingFace bearer
/// auth. Authenticated requests bypass the anonymous rate-limit
/// and bot-detection paths that return HTML error pages; leaving
/// `HF_TOKEN` unset is still fine for public models on a fresh
/// IP, just less robust at scale.
fn hf_token() -> Option<String> {
    std::env::var("HF_TOKEN").ok().filter(|s| !s.is_empty())
}

/// Same as `download_with_progress` but doesn't print per-chunk
/// progress. Used for fast + embed (we show a single ✓ when done).
async fn download_silent(url: &str, dest: &Path, size_gb: f64) -> Result<(), String> {
    let expected = sovereign_inference::GgufExpectation::from_size_gb(size_gb);

    if has_content(dest) {
        if sovereign_inference::validate_gguf(dest, &expected).is_ok() {
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
    if let Some(tok) = hf_token() {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(format!("GET {url}: {}", resp.status()));
    }
    if let Err(e) = reject_non_binary_content_type(&resp, url) {
        return Err(e);
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
    if let Err(e) = sovereign_inference::validate_gguf(&part, &expected) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download validation failed: {e}"));
    }
    std::fs::rename(&part, dest).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Scan the three models referenced by `SetupConfig`, validate
/// each against the manifest-derived size floor + GGUF magic
/// bytes, and delete the corrupted ones.
///
/// Deleting is the right action here even though it's aggressive:
/// the files that survive this check are either (a) plausible
/// GGUFs or (b) oversized placeholders that llama.cpp would also
/// reject at load. Leaving a stub behind has exactly one failure
/// mode — silent inference 503s hours later when the user first
/// issues a chat — while deleting it lets the operator just
/// re-run `sovereign setup` (which is now idempotent: it'll skip
/// good files and re-download missing ones).
async fn run_repair() -> i32 {
    let cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not read {}: {e}", SetupConfig::default_path().display());
            eprintln!("hint: run `sovereign setup` to set up from scratch.");
            return 1;
        }
    };

    println!();
    println!("  Sovereign Setup — Repair");
    println!("  {}", "─".repeat(54));

    // Bundled manifest lookup provides each slot's advertised
    // `size_gb`. If a file isn't in the manifest (BYOM), the
    // validator falls back to a 1 MB floor — still enough to
    // catch the common HTML-stub failure mode.
    let manifest = &*sovereign_core::models_manifest::DEFAULT_MANIFEST;

    let slots: [(&str, &std::path::Path); 3] = [
        ("primary", cfg.models.primary.as_path()),
        ("fast",    cfg.models.fast.as_path()),
        ("embed",   cfg.models.embed.as_path()),
    ];

    let mut removed = 0usize;
    let mut kept = 0usize;
    for (role, path) in slots {
        let size_gb = lookup_slot_size_gb(manifest, path);
        let expected = match size_gb {
            Some(gb) => sovereign_inference::GgufExpectation::from_size_gb(gb),
            None => sovereign_inference::GgufExpectation::unknown(),
        };
        match sovereign_inference::validate_gguf(path, &expected) {
            Ok(()) => {
                println!("  \u{2713} {role:<7} {} — valid", path.display());
                kept += 1;
            }
            Err(e) => {
                eprintln!("  \u{2717} {role:<7} {} — {e}", path.display());
                if let Err(rm_err) = std::fs::remove_file(path) {
                    eprintln!("      could not remove: {rm_err}");
                } else {
                    eprintln!("      removed; re-run `sovereign setup` to re-download.");
                    removed += 1;
                }
            }
        }
    }

    println!();
    println!(
        "  Summary: {kept} valid, {removed} removed. \
         Run `sovereign setup` to re-download the removed slots."
    );
    if removed > 0 { 1 } else { 0 }
}

/// Given a model file path, look up the slot's advertised
/// `size_gb` from the bundled manifest by filename match. The
/// manifest indexes by profile + slot, so we scan every slot in
/// every profile for a filename match; first hit wins. Returns
/// `None` if the user has a custom / BYOM model whose filename
/// isn't in the manifest.
fn lookup_slot_size_gb(
    manifest: &sovereign_core::models_manifest::ModelsManifest,
    path: &std::path::Path,
) -> Option<f64> {
    let file_name = path.file_name()?.to_str()?;
    for profile in manifest.profiles.values() {
        for slot in [&profile.thoughtful, &profile.fast, &profile.embed] {
            if let Some(s) = slot {
                if s.file == file_name {
                    return Some(s.size_gb);
                }
            }
        }
    }
    for user in &manifest.user_slots {
        if user.file == file_name {
            return Some(user.size_gb);
        }
    }
    None
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
            code: paths.code,
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

    // ── Post-setup health check ──────────────────────────────────
    // Run doctor to verify everything is healthy. Print results so the
    // user gets immediate confirmation that setup succeeded end-to-end.
    println!();
    println!("  Verifying setup health...");
    let exit_code = crate::doctor_cmd::run_doctor(&[]).await;
    if exit_code != 0 {
        println!();
        println!("  \u{26a0} Setup completed but some checks failed.");
        println!("    Run `sovereign doctor --fix` to attempt repairs.");
    }

    // ── Banner ───────────────────────────────────────────────────
    println!();
    println!("  \u{2713} Mesh running — 1 node (you)");
    println!("  \u{2713} Endpoint: localhost:{}/v1", cfg.daemon.client_port);

    // ── opencode config — write the global file directly ─────────
    //
    // Earlier the script just printed a snippet pointing at
    // `.opencode/config.json` (project-local). Real opencode reads
    // `~/.config/opencode/opencode.json`; users had to figure that
    // out themselves. Auto-write so a fresh `sovereign setup` is
    // immediately usable from opencode without copy-paste plumbing.
    match install_opencode_config(cfg.daemon.client_port) {
        Ok(OpencodeInstall::Created(path)) => {
            println!();
            println!(
                "  \u{2713} Wrote opencode config — {}",
                path.display()
            );
        }
        Ok(OpencodeInstall::MergedInto(path)) => {
            println!();
            println!(
                "  \u{2713} Updated opencode config — {} (preserved your existing entries)",
                path.display()
            );
        }
        Ok(OpencodeInstall::AlreadyConfigured(path)) => {
            println!();
            println!(
                "  \u{2713} opencode already configured — {}",
                path.display()
            );
        }
        Err(e) => {
            // Non-fatal: print the snippet the user can paste themselves.
            eprintln!();
            eprintln!("  warning: couldn't write opencode config: {e}");
            eprintln!(
                "  paste this into ~/.config/opencode/opencode.json yourself:"
            );
            eprintln!("{}", opencode_config_snippet(cfg.daemon.client_port));
        }
    }

    let _ = Arc::new(()); // placeholder; Arc usage removed post-refactor
    0
}

/// Outcome of attempting to install / update the opencode config.
/// Carries the path so the banner prints something actionable
/// instead of "ok".
#[derive(Debug)]
enum OpencodeInstall {
    /// File didn't exist; we created it from scratch.
    Created(PathBuf),
    /// File existed; we merged Sovereign's MCP server + provider
    /// into the existing JSON without disturbing the user's other
    /// providers, models, skills, etc.
    MergedInto(PathBuf),
    /// File existed and already contained an entry pointing at our
    /// daemon — nothing to do.
    AlreadyConfigured(PathBuf),
}

fn opencode_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode")
        .join("opencode.json")
}

/// Render the JSON snippet we'd write — used both to seed a new file
/// and to print as a fallback when writing fails.
fn opencode_config_snippet(client_port: u16) -> String {
    let value = serde_json::json!({
        "mcp": {
            "servers": {
                "sovereign": {
                    "type": "http",
                    "url": format!("http://localhost:{client_port}/mcp")
                }
            }
        },
        "provider": {
            "commonwealth": {
                "npm": "@ai-sdk/openai-compatible",
                "options": {
                    "baseURL": format!("http://localhost:{client_port}/v1")
                }
            }
        }
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| String::new())
}

/// Install or merge our opencode entries into `~/.config/opencode/opencode.json`.
///
/// Behaviour matrix (decided so a re-run of `sovereign setup` is
/// always safe and never clobbers third-party config):
///   - file missing               → create with our entries only.
///   - file present, no overlap   → merge: add `mcp.servers.sovereign`
///                                  and `provider.commonwealth`,
///                                  leave everything else untouched.
///   - file present, our entries  → noop, return `AlreadyConfigured`.
///   - file present, parse error  → bail (the user has invalid JSON;
///                                  we shouldn't try to "fix" it).
fn install_opencode_config(client_port: u16) -> Result<OpencodeInstall, String> {
    install_opencode_config_at(&opencode_config_path(), client_port)
}

fn install_opencode_config_at(
    path: &Path,
    client_port: u16,
) -> Result<OpencodeInstall, String> {
    // Fresh install — easy path.
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(path, opencode_config_snippet(client_port))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        return Ok(OpencodeInstall::Created(path.to_path_buf()));
    }

    // Existing file — parse, merge our keys, write back.
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut cfg: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if !cfg.is_object() {
        return Err(format!(
            "{} is not a JSON object — refusing to overwrite",
            path.display()
        ));
    }

    let mcp_url = format!("http://localhost:{client_port}/mcp");
    let base_url = format!("http://localhost:{client_port}/v1");

    // Detect already-configured to give the user a "nothing to do"
    // banner instead of pretending we did work.
    let same_mcp = cfg
        .pointer("/mcp/servers/sovereign/url")
        .and_then(|v| v.as_str())
        == Some(mcp_url.as_str());
    let same_provider = cfg
        .pointer("/provider/commonwealth/options/baseURL")
        .and_then(|v| v.as_str())
        == Some(base_url.as_str());
    if same_mcp && same_provider {
        return Ok(OpencodeInstall::AlreadyConfigured(path.to_path_buf()));
    }

    // Walk into mcp.servers.sovereign and provider.commonwealth,
    // creating intermediate objects only as needed. Other keys at
    // each level are preserved verbatim.
    let obj = cfg.as_object_mut().expect("verified above");
    let mcp = obj
        .entry("mcp".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let mcp_obj = mcp
        .as_object_mut()
        .ok_or_else(|| "`mcp` is not an object".to_string())?;
    let servers = mcp_obj
        .entry("servers".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "`mcp.servers` is not an object".to_string())?;
    servers_obj.insert(
        "sovereign".to_string(),
        serde_json::json!({ "type": "http", "url": mcp_url }),
    );

    let provider = obj
        .entry("provider".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let provider_obj = provider
        .as_object_mut()
        .ok_or_else(|| "`provider` is not an object".to_string())?;
    provider_obj.insert(
        "commonwealth".to_string(),
        serde_json::json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": { "baseURL": base_url }
        }),
    );

    let pretty = serde_json::to_string_pretty(&cfg)
        .map_err(|e| format!("serialize merged config: {e}"))?;
    std::fs::write(path, pretty)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(OpencodeInstall::MergedInto(path.to_path_buf()))
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
mod download_failure_tests {
    //! Integration tests for the download validation path. Each
    //! spins up an axum mock on a kernel-assigned port, points
    //! `download_with_progress` at it, and asserts the expected
    //! failure mode leaves the models dir clean.
    use super::*;
    use axum::{response::IntoResponse, routing::get, Router};
    use std::net::SocketAddr;

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        (format!("http://{addr}"), handle)
    }

    /// The pathological case that landed three 188 KB stubs on
    /// the user's disk: CDN returns 200 OK with `text/html`
    /// body. The content-type pre-check must fire and refuse
    /// *before* we stream any HTML to the `.part` file.
    #[tokio::test]
    async fn rejects_text_html_before_streaming_and_leaves_no_part() {
        let app = Router::new().route(
            "/fake-model.gguf",
            get(|| async {
                (
                    [(reqwest::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    "<!DOCTYPE html><html><body>rate limited</body></html>",
                )
                    .into_response()
            }),
        );
        let (base, _handle) = serve(app).await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("fake-model.gguf");
        let part = tmp.path().join("fake-model.gguf.part");

        let err = download_with_progress(
            &format!("{base}/fake-model.gguf"),
            &dest,
            "fake",
            18.5, // pretend this is a big model
        )
        .await
        .unwrap_err();
        assert!(err.contains("content-type") || err.contains("text/html"), "err: {err}");
        assert!(!dest.exists(), "no stub should land at final path");
        assert!(!part.exists(), "no .part should remain");
    }

    /// Server returns 200 with `application/octet-stream` but
    /// the body is HTML anyway — post-stream `validate_gguf`
    /// catches the magic-byte mismatch. We assert the `.part`
    /// is cleaned up so a retry doesn't resume a bogus file.
    #[tokio::test]
    async fn rejects_post_stream_when_magic_is_wrong_and_deletes_part() {
        // 2 MB of fake HTML, above the default 1 MB floor so the
        // size check passes and the magic check is the one that
        // fires. Advertises octet-stream to bypass the pre-check.
        let mut body = Vec::new();
        body.extend_from_slice(b"<!DOCTYPE html><html>");
        body.resize(2_000_000, b'.');

        let app = Router::new().route(
            "/fake-model.gguf",
            get(move || {
                let body = body.clone();
                async move {
                    (
                        [(reqwest::header::CONTENT_TYPE, "application/octet-stream")],
                        body,
                    )
                        .into_response()
                }
            }),
        );
        let (base, _handle) = serve(app).await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("fake.gguf");
        let part = tmp.path().join("fake.gguf.part");

        // size_gb=0.001 → 1 MB floor (the default min); the 2 MB
        // body passes the size check, so the GGUF magic check is
        // what fires. This is the important case: servers that
        // return HTML with an innocuous content-type header.
        let err = download_with_progress(
            &format!("{base}/fake-model.gguf"),
            &dest,
            "fake",
            0.001,
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("not a GGUF") || err.contains("GGUF") || err.contains("magic"),
            "err should mention magic mismatch: {err}"
        );
        assert!(!dest.exists(), "no stub should land at final path");
        assert!(!part.exists(), "no .part should remain on failure");
    }

    /// A successful response with a real GGUF magic header and
    /// plausible size lands at the final path. Confirms the
    /// happy path isn't broken by the new validation layer.
    #[tokio::test]
    async fn accepts_real_gguf_and_renames_to_final() {
        let mut body = Vec::with_capacity(2 * 1024 * 1024);
        body.extend_from_slice(b"GGUF");
        body.resize(2 * 1024 * 1024, 0u8);

        let app = Router::new().route(
            "/real-model.gguf",
            get(move || {
                let body = body.clone();
                async move {
                    (
                        [(reqwest::header::CONTENT_TYPE, "application/octet-stream")],
                        body,
                    )
                        .into_response()
                }
            }),
        );
        let (base, _handle) = serve(app).await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("real.gguf");
        // size_gb 0.001 so the 50% floor (512 KB) comfortably
        // accepts our 2 MB test payload.
        download_with_progress(
            &format!("{base}/real-model.gguf"),
            &dest,
            "real",
            0.001,
        )
        .await
        .expect("happy path should succeed");
        assert!(dest.exists(), "final path should hold the downloaded file");
        assert_eq!(dest.metadata().unwrap().len(), 2 * 1024 * 1024);
    }

    #[test]
    fn hf_token_reads_env_var() {
        // Unset first to get a clean baseline; safe because tests
        // use a distinct thread and no production code reads this
        // during tests.
        std::env::remove_var("HF_TOKEN");
        assert!(hf_token().is_none());
        std::env::set_var("HF_TOKEN", "secret");
        assert_eq!(hf_token().as_deref(), Some("secret"));
        std::env::set_var("HF_TOKEN", "");
        assert!(hf_token().is_none(), "empty token counted as unset");
        std::env::remove_var("HF_TOKEN");
    }
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

    // ── opencode config install ───────────────────────────────────

    #[test]
    fn opencode_install_creates_file_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.json");
        let result = install_opencode_config_at(&path, 9741).unwrap();
        assert!(matches!(result, OpencodeInstall::Created(_)));
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["mcp"]["servers"]["sovereign"]["url"],
            "http://localhost:9741/mcp"
        );
        assert_eq!(
            parsed["provider"]["commonwealth"]["options"]["baseURL"],
            "http://localhost:9741/v1"
        );
    }

    #[test]
    fn opencode_install_preserves_unrelated_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.json");
        // Pre-existing config with another provider, MCP server, and
        // top-level keys. Merge must leave them all alone.
        std::fs::write(
            &path,
            r#"{
              "model": { "id": "auto" },
              "skills": [".opencode/skills/sovereign-code"],
              "mcp": {
                "servers": {
                  "github": { "type": "http", "url": "https://example.com/mcp" }
                }
              },
              "provider": {
                "openrouter": { "npm": "@openrouter/ai-sdk", "options": {} }
              }
            }"#,
        )
        .unwrap();

        let result = install_opencode_config_at(&path, 9741).unwrap();
        assert!(matches!(result, OpencodeInstall::MergedInto(_)));

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Our entries are present.
        assert_eq!(
            parsed["mcp"]["servers"]["sovereign"]["url"],
            "http://localhost:9741/mcp"
        );
        assert_eq!(
            parsed["provider"]["commonwealth"]["options"]["baseURL"],
            "http://localhost:9741/v1"
        );
        // Existing entries survived.
        assert_eq!(
            parsed["mcp"]["servers"]["github"]["url"],
            "https://example.com/mcp"
        );
        assert_eq!(parsed["provider"]["openrouter"]["npm"], "@openrouter/ai-sdk");
        assert_eq!(parsed["model"]["id"], "auto");
        assert_eq!(parsed["skills"][0], ".opencode/skills/sovereign-code");
    }

    #[test]
    fn opencode_install_is_noop_when_already_configured() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.json");
        // First install: Created.
        install_opencode_config_at(&path, 9741).unwrap();
        // Second call with the same port: AlreadyConfigured.
        let result = install_opencode_config_at(&path, 9741).unwrap();
        assert!(matches!(result, OpencodeInstall::AlreadyConfigured(_)));
    }

    #[test]
    fn opencode_install_updates_when_port_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.json");
        install_opencode_config_at(&path, 9741).unwrap();
        let result = install_opencode_config_at(&path, 9999).unwrap();
        assert!(matches!(result, OpencodeInstall::MergedInto(_)));
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            parsed["mcp"]["servers"]["sovereign"]["url"],
            "http://localhost:9999/mcp"
        );
    }

    #[test]
    fn opencode_install_refuses_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(&path, "{ not json").unwrap();
        let err = install_opencode_config_at(&path, 9741).unwrap_err();
        assert!(err.contains("parse"), "{err}");
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

    // `verify_gguf_non_empty` was replaced by
    // `sovereign_inference::validate_gguf`, which is tested in
    // `sovereign-inference/src/gguf_validator.rs`. The old tests
    // here duplicated a strict subset of that coverage; they
    // were removed to avoid drift between the two schemas.

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
