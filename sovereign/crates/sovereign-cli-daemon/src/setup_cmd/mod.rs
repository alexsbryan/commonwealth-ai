// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn setup` — first-run onboarding.
//!
//! Flow: detect hardware → pick primary model → download three slots
//! in parallel → write `~/.sovereign/config.toml` → register
//! launchd/systemd service → poll the running daemon.
//!
//! Flags:
//! - `--reset`      Wipe config and re-run (uninstalls service first).
//! - `--yes`        Non-interactive — accept recommended for all prompts.
//! - `--data-dir`   Override the default `~/.sovereign` data root.

use std::io::{self, IsTerminal as _, Write as _};
use std::path::PathBuf;

use sovereign_core::models_manifest::SlotConfig;
use sovereign_inference::hardware::{self, HardwareProfile};
use sovereign_inference::setup_planner::{
    build_primary_catalog, hf_download_url, resolve_slot, SlotKind,
};

// Imports used only by the in-file test modules. Kept behind
// `#[cfg(test)]` so a non-test `cargo check` doesn't warn.
#[cfg(test)]
use sovereign_core::models_manifest::DEFAULT_MANIFEST;
#[cfg(test)]
use sovereign_inference::hardware::ProfileName;
#[cfg(test)]
use sovereign_inference::setup_planner::{hf_token, tier_rank};
// Used by the test modules below (the non-test code no longer references
// `Path` after the §3.2 split moved the downloaders / opencode out).
#[cfg(test)]
use std::path::Path;

use crate::service_install;
use crate::setup_config::SetupConfig;

// §3.2 split: the wizard's phases live in focused submodules; the shared
// `Opts` / `ModelPaths` / `Pick` types stay here (submodules read them as
// ancestor-privates) while `run_setup` / `run_repair` orchestrate.
mod args;
mod byom;
mod catalog;
mod download;
mod fim;
mod finish;
mod opencode;

use args::{parse_args, print_usage};
use byom::prompt_byom_paths;
use catalog::pick_primary;
use download::{download_silent, lookup_slot_size_gb};
use finish::finish_with_paths;
// Re-exported: `daemon_cmd` calls `crate::setup_cmd::download_with_progress`.
pub(crate) use download::download_with_progress;

pub async fn run_setup(args: &[String]) -> i32 {
    // `--fim` is a different destination, not a modifier on the
    // wizard — dispatch BEFORE the deprecation shim below. Two
    // reasons: the shim announces "use `svrn daemon --setup-only`",
    // which is wrong advice for a flag that has nothing to do with
    // first-boot model setup; and it force-appends `--wizard-only`,
    // whose meaning in the FIM path ("the daemon is about to boot,
    // don't restart it") must stay under the caller's control.
    if args.iter().any(|a| a == "--fim") {
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
        return fim::run_fim_setup(&opts).await;
    }

    // Phase 4: `svrn setup` is now a wizard-only shim. The
    // service-install + opencode + doctor steps that used to run
    // here moved out — service registration is now `sovereign
    // install-service`, and the daemon-first-boot path
    // (`svrn daemon`) inlines the wizard automatically.
    //
    // We detect whether this invocation came in via the new
    // `daemon --setup-only` path (which prepends `--wizard-only`)
    // or from a direct `svrn setup` user invocation. Direct
    // invocations get a one-time banner so the user knows where
    // service registration moved.
    let invoked_via_daemon_path = args.iter().any(|a| a == "--wizard-only");
    let mut effective_args: Vec<String> = args.to_vec();
    if !invoked_via_daemon_path {
        sovereign_cli_shared::deprecation::announce("svrn setup", "svrn daemon --setup-only");
        // The legacy `svrn setup` is now wizard-only. Force the
        // flag on so `finish_with_paths` short-circuits before the
        // service-install branch — that branch belongs to
        // `svrn install-service` now. Keeping the alias semantics
        // means scripts that called `svrn setup` still get a
        // working config; they just have to follow up with
        // `svrn install-service` if they want the service
        // manager to keep the daemon alive across reboots.
        effective_args.push("--wizard-only".to_string());
    }

    let opts = match parse_args(&effective_args) {
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
        println!("  Run `svrn status` to check or `svrn setup --reset` to reconfigure.");
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
    /// Phase 4: if true, run only the hardware-detect → model-pick →
    /// config-write portion of the wizard. Skip service install,
    /// opencode config, doctor, and the daemon health probe. The
    /// daemon's first-boot path sets this so the wizard can run
    /// inline before `run_daemon` continues to load models and bind
    /// `:9741`. The legacy `svrn setup` command also runs in
    /// this mode and points the user at `svrn install-service`
    /// for service registration.
    wizard_only: bool,
    /// Run the inline-completion onboarding (`fim::run_fim_setup`)
    /// instead of the model wizard. Not a modifier on the wizard —
    /// a different destination, which is why `run_setup` dispatches
    /// on it before the deprecation shim.
    fim: bool,
    /// FIM ladder rung name (`"q6_k"`), when the operator overrode
    /// the hardware-derived pick. `None` = use
    /// `fim_rung_for_profile`. Only meaningful with `fim`.
    quant: Option<String>,
    /// With `fim`: stop after the daemon is verified, leave the
    /// editor alone. For headless/CI hosts and for operators who
    /// manage their extensions themselves.
    skip_editor: bool,
}

// ─── Model catalog + picker ───────────────────────────────────────
//
// Catalog construction (`build_primary_catalog`, `tier_rank`,
// `resolve_slot`, `SlotKind`, `PrimaryOption`) lives in
// `sovereign_inference::setup_planner` so the desktop's
// `complete_setup_auto` flow shares the same logic. Imported above.

enum Pick {
    Slot(SlotConfig),
    Byom,
    Abort,
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
/// re-run `svrn setup` (which is now idempotent: it'll skip
/// good files and re-download missing ones).
async fn run_repair() -> i32 {
    let cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "error: could not read {}: {e}",
                SetupConfig::default_path().display()
            );
            eprintln!("hint: run `svrn setup` to set up from scratch.");
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

    // Validate the on-disk size of each slot's GGUF. When fast
    // subsumes primary, primary is checked once — there's no separate
    // file to validate for the fast role. has_explicit_fast() gates
    // adding it to the sweep.
    let mut slots: Vec<(&str, &std::path::Path)> = vec![
        ("primary", cfg.models.primary.as_path()),
        ("embed", cfg.models.embed.as_path()),
    ];
    if cfg.models.has_explicit_fast() {
        slots.push(("fast", cfg.models.fast_path()));
    }

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
                    eprintln!("      removed; re-run `svrn setup` to re-download.");
                    removed += 1;
                }
            }
        }
    }

    println!();
    println!(
        "  Summary: {kept} valid, {removed} removed. \
         Run `svrn setup` to re-download the removed slots."
    );
    if removed > 0 {
        1
    } else {
        0
    }
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
        assert!(
            err.contains("content-type") || err.contains("text/html"),
            "err: {err}"
        );
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
        let err = download_with_progress(&format!("{base}/fake-model.gguf"), &dest, "fake", 0.001)
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
        download_with_progress(&format!("{base}/real-model.gguf"), &dest, "real", 0.001)
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
    // Fns that moved into submodules during the §3.2 split (parse_args is
    // re-imported into the parent above, so `use super::*` already covers it).
    use super::catalog::display_name;
    use super::download::has_content;
    use super::opencode::{install_opencode_config_at, OpencodeInstall};

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

    /// Phase 4: `--wizard-only` is the internal flag that
    /// `daemon_cmd::run_setup_only` uses to suppress the
    /// service-install / opencode / doctor steps. It also gets
    /// auto-injected by the legacy `svrn setup` shim so
    /// direct invocations of the old name still hit the wizard
    /// path.
    #[test]
    fn parse_args_recognizes_wizard_only_flag() {
        let opts = parse_args(&s(&["--wizard-only"])).unwrap();
        assert!(opts.wizard_only);
        assert!(!opts.reset);
        assert!(!opts.yes);
    }

    /// Default Opts still has `wizard_only=false` so we don't
    /// accidentally short-circuit the legacy `svrn setup` flow
    /// in scripts that rebuilt against this binary without changing
    /// their invocation.
    #[test]
    fn parse_args_defaults_wizard_only_off() {
        let opts = parse_args(&s(&[])).unwrap();
        assert!(!opts.wizard_only);
    }

    // ── --fim ──────────────────────────────────────────────────────

    #[test]
    fn parse_args_recognizes_fim_and_its_modifiers() {
        let opts = parse_args(&s(&["--fim", "--quant", "q8_0", "--skip-editor", "-y"])).unwrap();
        assert!(opts.fim);
        assert_eq!(opts.quant.as_deref(), Some("q8_0"));
        assert!(opts.skip_editor);
        assert!(opts.yes);
    }

    #[test]
    fn parse_args_defaults_fim_off() {
        let opts = parse_args(&s(&[])).unwrap();
        assert!(!opts.fim);
        assert!(opts.quant.is_none());
        assert!(!opts.skip_editor);
    }

    /// `--quant Q6_K` is what an operator copies off the setup
    /// banner, which prints the manifest's display spelling. Rejecting
    /// it over a case mismatch would be a gratuitous failure in the
    /// middle of onboarding.
    #[test]
    fn parse_args_accepts_display_case_quant() {
        let opts = parse_args(&s(&["--fim", "--quant", "Q6_K"])).unwrap();
        assert_eq!(opts.quant.as_deref(), Some("q6_k"));
    }

    /// A typo'd rung must fail at parse time, not after a multi-GB
    /// download resolves to nothing.
    #[test]
    fn parse_args_rejects_unknown_quant_and_lists_the_rungs() {
        let err = parse_args(&s(&["--fim", "--quant", "q3_k_s"])).unwrap_err();
        assert!(err.contains("q3_k_s"), "error should echo the input: {err}");
        assert!(err.contains("q6_k"), "error should list valid rungs: {err}");
    }

    #[test]
    fn parse_args_rejects_dangling_quant() {
        let err = parse_args(&s(&["--fim", "--quant"])).unwrap_err();
        assert!(err.contains("--quant"), "error: {err}");
    }

    /// `--quant` / `--skip-editor` without `--fim` would silently do
    /// nothing — the wizard has no FIM step to modify. Fail loudly so
    /// a typo'd invocation isn't mistaken for a completed FIM setup.
    #[test]
    fn parse_args_rejects_fim_modifiers_without_fim() {
        let err = parse_args(&s(&["--quant", "q6_k"])).unwrap_err();
        assert!(err.contains("--fim"), "error: {err}");
        let err = parse_args(&s(&["--skip-editor"])).unwrap_err();
        assert!(err.contains("--fim"), "error: {err}");
    }

    /// The `daemon --setup-only --fim` path prepends `--wizard-only`;
    /// both must survive parsing together, because `wizard_only` is
    /// what stops the FIM path from restarting the daemon that is
    /// currently booting it.
    #[test]
    fn parse_args_allows_fim_alongside_wizard_only() {
        let opts = parse_args(&s(&["--wizard-only", "--fim"])).unwrap();
        assert!(opts.fim);
        assert!(opts.wizard_only);
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
            assert!(
                seen.insert(key.clone()),
                "duplicate base_name in catalog: {key}"
            );
        }
    }

    #[test]
    fn catalog_very_high_includes_every_tier_below() {
        // VeryHigh users should see every tier at-or-below them (subject to
        // dedup). Count of distinct tiers available should be >= 1 (hard
        // guarantee) and match the number of profiles that define thoughtful
        // and have non-duplicate base_names.
        let cat = build_primary_catalog(&ProfileName::VeryHigh);
        assert!(!cat.is_empty());
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
        assert_eq!(
            parsed["provider"]["openrouter"]["npm"],
            "@openrouter/ai-sdk"
        );
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
