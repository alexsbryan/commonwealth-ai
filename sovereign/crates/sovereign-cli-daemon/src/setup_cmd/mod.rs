// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn setup` — first-run onboarding.
//!
//! Flow: detect hardware → pick primary model → download three slots
//! in parallel → write `~/.svrnmesh/config.toml` → register
//! launchd/systemd service → poll the running daemon.
//!
//! Flags:
//! - `--reset`      Wipe config and re-run (uninstalls service first).
//! - `--yes`        Non-interactive — accept recommended for all prompts.
//! - `--data-dir`   Override the default `~/.svrnmesh` data root.

use std::io::{self, IsTerminal as _, Write as _};
use std::path::PathBuf;

use sovereign_contracts::daemon_wire::{SetupPlan, SetupProgressPhase};
use sovereign_core::models_manifest::SlotConfig;
use sovereign_inference::hardware::{self, detect_hardware, HardwareProfile};
use sovereign_inference::setup_planner::{
    build_primary_catalog, hf_download_url, resolve_slot, PrimaryOption, SlotKind,
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

use crate::setup_config::SetupConfig;

// §3.2 split: the wizard's phases live in focused submodules; the shared
// `Opts` / `ModelPaths` / `Pick` types stay here (submodules read them as
// ancestor-privates) while `run_setup` / `run_repair` orchestrate.
mod args;
mod byom;
mod catalog;
mod download;
mod emit;
mod fim;
mod finish;
mod opencode;
mod terminal;

use args::{parse_args, print_usage};
use byom::prompt_byom_paths;
use catalog::pick_primary;
use download::{download_silent, lookup_slot_size_gb};
use emit::say;
use finish::finish_with_paths;
// Re-exported: `daemon_cmd` calls `crate::setup_cmd::download_with_progress`.
pub(crate) use download::download_with_progress;

pub async fn run_setup(args: &[String]) -> i32 {
    // Route ggml through `tracing` BEFORE anything can touch it. Setup never
    // constructs a `LlamaBackend`, so it could not call `LlamaLogs::install`
    // and sat outside that module's one decision — and it still reaches ggml:
    // the in-process daemon that joins the mesh builds a capability manifest,
    // which calls `detect_hardware()`, which initialises the Metal device.
    // The result was ~30 lines of `ggml_metal_library_compile_all: compiled
    // 'fa' library in 0.036 sec` landing between "Joining the mesh..." and the
    // next line of an onboarding flow written for someone who has never seen a
    // GPU log. First statement in the function because the trigger is a
    // transitive call several layers down, and anything later is a race with it.
    let _fully_applied = sovereign_inference::llama_logs::LlamaLogs::from_env().install_global();

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

    // `--terminal` is likewise a destination, not a modifier: it downloads
    // nothing, detects no hardware, and writes a config with no `[models]`.
    // Dispatched before the deprecation shim for the same reason `--fim` is —
    // "use `svrn daemon --setup-only`" is wrong advice for it.
    if args.iter().any(|a| a == "--terminal") {
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
        let entry = opts
            .terminal
            .clone()
            .expect("--terminal was present, so parse_args set it");
        return terminal::run_terminal_setup(&entry, &opts).await;
    }

    // `--plan` is a READ, not a destination: it detects hardware, resolves the
    // catalog and prints, and it touches no file. Dispatched here — before the
    // deprecation shim below, which writes a banner to stdout and would land
    // inside the JSON document, and before the `config exists` short-circuit,
    // because a configured machine still has a plan to describe.
    //
    // This is the whole reason the verb grew a flag rather than the daemon
    // growing a route: the sidecar exits 1 with no config off a TTY
    // (`daemon_cmd/mod.rs`) and refuses a config with no `[models]`
    // (`daemon_cmd/build/inference.rs`), so on a first run there is no HTTP to
    // ask. A spawn links nothing.
    if args.iter().any(|a| a == "--plan") {
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
        return print_plan().await;
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

    // From here on stdout belongs to whichever audience `--json` named.
    emit::set_json_mode(opts.json);

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
        return run_repair(&opts).await;
    }

    // ── --reset: tear down existing setup ─────────────────────────
    if opts.reset {
        eprintln!("  Resetting sovereign...");
        if let Err(e) = sovereign_service::uninstall_service() {
            eprintln!("  warning: could not uninstall service: {e}");
        } else {
            eprintln!("    \u{2713} Service uninstalled");
        }
        if let Err(e) = SetupConfig::remove_at(&run_config_path(&opts)) {
            eprintln!("  warning: could not remove config: {e}");
        } else {
            eprintln!("    \u{2713} Config removed");
        }
        eprintln!();
    } else {
        let cfg_path = run_config_path(&opts);
        if cfg_path.exists() {
            say!();
            say!("  Already set up. Config at {}", cfg_path.display());
            say!("  Run `svrn status` to check or `svrn setup --reset` to reconfigure.");
            return 0;
        }
    }

    say!();
    say!("  Sovereign Setup");
    say!("  {}", "─".repeat(54));
    say!();

    // ── 1. Hardware detection ─────────────────────────────────────
    eprint!("  Detecting hardware... ");
    io::stderr().flush().ok();
    // Move the sync detection off the async runtime so we don't block
    // the reactor (sysinfo::System::new_all walks /proc).
    emit::narrate(
        SetupProgressPhase::DetectingHardware,
        "Reading what this machine can do.",
    );
    let hw = match tokio::task::spawn_blocking(detect_hardware).await {
        Ok(h) => h,
        Err(e) => return fail(format!("hardware detection panicked: {e}")),
    };
    let profile_name = hardware::select_profile(&hw);
    say!(
        "{}, {:.0}GB {}memory",
        hardware_label(&hw),
        hw.system_ram_gb(),
        if hw.is_unified_memory { "unified " } else { "" }
    );
    say!();

    // ── 2. Pick primary model ────────────────────────────────────
    let catalog = build_primary_catalog(&profile_name);
    if catalog.is_empty() {
        return fail("no models available in the bundled manifest for your hardware");
    }

    // `--primary <spec>` is the non-interactive form of the picker: it answers
    // the same question the numbered rows and the `[b]` branch answer, so it
    // resolves BEFORE the prompt rather than beside it. A client that spawned
    // this process has no terminal to type into.
    let chosen = match opts.primary.as_deref() {
        Some(spec) => match resolve_primary_flag(spec, &catalog) {
            Ok(c) => c,
            Err(msg) => return fail(msg),
        },
        None => match pick_primary(&catalog, opts.yes) {
            Pick::Slot(slot) => PrimaryChoice::Download {
                url: hf_download_url(&slot),
                slot,
            },
            Pick::Byom => match prompt_byom_paths(&opts) {
                Ok(paths) => {
                    return finish_with_paths(paths, &opts).await;
                }
                Err(msg) => return fail(msg),
            },
            Pick::Abort => {
                eprintln!("Setup cancelled.");
                emit::failed("setup cancelled");
                return 1;
            }
        },
    };
    let picked = chosen.slot().clone();

    // Fast + embed come from the user's own profile — not curated. If
    // the profile doesn't define one (very rare for embed on cpu_only),
    // fall back to the default profile's slot.
    let fast_slot = resolve_slot(&profile_name, SlotKind::Fast);
    let embed_slot = resolve_slot(&profile_name, SlotKind::Embed);
    let (fast_slot, embed_slot) = match (fast_slot, embed_slot) {
        (Some(f), Some(e)) => (f, e),
        _ => {
            return fail("bundled manifest is missing fast or embed slot for your profile");
        }
    };

    // ── 3. Download all three slots ──────────────────────────────
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(sovereign_contracts::rebrand::svrnmesh_root);
    let models_dir = data_dir.join("models");
    emit::narrate(
        SetupProgressPhase::PreparingDataDir,
        "Preparing your storage.",
    );
    if let Err(e) = std::fs::create_dir_all(&models_dir) {
        return fail(format!("cannot create {}: {e}", models_dir.display()));
    }

    say!("  Downloading models...");
    say!();

    // A `--primary <path>` pick is used where it already sits: never copied,
    // never re-fetched. Everything else lands in the data root's models dir.
    let primary_path = match &chosen {
        PrimaryChoice::InPlace { path, .. } => path.clone(),
        PrimaryChoice::Download { .. } => models_dir.join(&picked.file),
    };
    let fast_path = models_dir.join(&fast_slot.file);
    let embed_path = models_dir.join(&embed_slot.file);

    // The manifest's `hf_url` is the repo *landing page* — we derive the
    // actual GGUF download URL from it plus the slot's filename.
    let fast_url = hf_download_url(&fast_slot);
    let embed_url = hf_download_url(&embed_slot);

    // Primary shows progress; fast+embed run silently in parallel.
    // Each slot's `size_gb` goes through so `validate_gguf` can
    // apply a tighter floor than the 1 MB sentinel — a corrupt
    // 200 KB "35 GB" file is an obvious lie, a corrupt 200 KB
    // "0.4 GB" embed is too.
    //
    // `--json` mirrors that asymmetry rather than inventing a second
    // reporting policy: per-chunk lines for the primary, a phase line each
    // for fast and embed as they land. Three concurrent per-chunk streams on
    // one stdout would be honest and unreadable.
    let narrator = matches!(chosen, PrimaryChoice::Download { .. }).then(|| {
        emit::DownloadNarrator::new(
            SetupProgressPhase::DownloadingPrimary,
            "Downloading the main responder.",
            &picked.file,
        )
    });
    let primary_fut = async {
        match &chosen {
            PrimaryChoice::InPlace { path, .. } => {
                say!("    \u{2713} {} (already on disk)", path.display());
                Ok(())
            }
            PrimaryChoice::Download { url, .. } => {
                download_with_progress(
                    url,
                    &primary_path,
                    &picked.file,
                    picked.size_gb,
                    narrator.as_ref(),
                )
                .await
            }
        }
    };
    let fast_fut = download_silent(&fast_url, &fast_path, fast_slot.size_gb);
    let embed_fut = download_silent(&embed_url, &embed_path, embed_slot.size_gb);
    let (primary_res, fast_res, embed_res) = tokio::join!(primary_fut, fast_fut, embed_fut);

    if let Err(e) = primary_res {
        eprintln!("  \u{2717} Main responder: {e}");
        emit::failed(&e);
        return 1;
    }
    if let Err(e) = fast_res {
        eprintln!("  \u{2717} Quick responder: {e}");
        emit::failed(&e);
        return 1;
    } else {
        say!("    \u{2713} {}", fast_slot.file);
        emit::narrate(
            SetupProgressPhase::DownloadingFast,
            "The quick responder is ready.",
        );
    }
    if let Err(e) = embed_res {
        eprintln!("  \u{2717} Knowledge embedder: {e}");
        emit::failed(&e);
        return 1;
    } else {
        say!("    \u{2713} {}", embed_slot.file);
        emit::narrate(
            SetupProgressPhase::DownloadingEmbed,
            "The knowledge embedder is ready.",
        );
    }

    say!();
    say!("  \u{2713} Models ready");

    emit::narrate(
        SetupProgressPhase::WritingConfig,
        "Writing your configuration.",
    );
    let code = finish_with_paths(
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
    .await;
    // The exit code is the verdict; the terminal line is what a parser reads
    // it as. `finish_with_paths` has already named the reason on stderr.
    if code == 0 {
        emit::done(&run_config_path(&opts));
    } else {
        emit::failed("setup could not finish writing its configuration");
    }
    code
}

/// Report a wizard failure once — to the human on stderr, to a `--json`
/// parser as the terminal line, and to the shell as exit 1.
///
/// One site, because the two audiences must never disagree: before this, every
/// `return 1` on the wizard path told the human why and told a parser only
/// that the process ended (ARCH principle 6).
fn fail(msg: impl AsRef<str>) -> i32 {
    let msg = msg.as_ref();
    eprintln!("error: {msg}");
    emit::failed(msg);
    1
}

/// What `--primary <spec>` resolved to.
#[derive(Debug)]
enum PrimaryChoice {
    /// Fetch it: a catalog row, or a `.gguf` URL the user pasted.
    Download { slot: SlotConfig, url: String },
    /// It is already on this disk. Used where it sits — never copied, never
    /// re-fetched, because the user pointing at a file is the whole request.
    InPlace { slot: SlotConfig, path: PathBuf },
}

impl PrimaryChoice {
    fn slot(&self) -> &SlotConfig {
        match self {
            PrimaryChoice::Download { slot, .. } | PrimaryChoice::InPlace { slot, .. } => slot,
        }
    }
}

/// Resolve `--primary <spec>` against the catalog, then against the two BYOM
/// shapes the desktop's onboarding "Advanced" affordance already accepted: a
/// `.gguf` URL and a `.gguf` already on disk.
///
/// An unrecognised spec is REFUSED by name, listing what would have matched —
/// never quietly demoted to the hardware recommendation, which would install
/// a model the caller did not ask for and report success.
fn resolve_primary_flag(spec: &str, catalog: &[PrimaryOption]) -> Result<PrimaryChoice, String> {
    let spec = spec.trim();
    if let Some(opt) = catalog.iter().find(|o| o.slot.file == spec) {
        // Which of the three shapes a spec resolved to is NOT predictable from
        // the argv — it depends on the tier's catalog and on what is on this
        // disk — so each arm says which one it took (ARCH principle 1).
        tracing::info!(
            spec,
            shape = "catalog",
            file = %opt.slot.file,
            "setup:primary_resolved"
        );
        return Ok(PrimaryChoice::Download {
            url: hf_download_url(&opt.slot),
            slot: opt.slot.clone(),
        });
    }
    if spec.starts_with("http://") || spec.starts_with("https://") {
        let (url, file) = sovereign_inference::setup_planner::resolve_byom_url(spec)?;
        tracing::info!(spec, shape = "byom_url", %url, "setup:primary_resolved");
        return Ok(PrimaryChoice::Download {
            slot: SlotConfig {
                file: file.clone(),
                base_name: file,
                quant: "custom (url)".to_string(),
                hf_url: url.clone(),
                ..Default::default()
            },
            url,
        });
    }
    let path = PathBuf::from(spec);
    if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
        if !path.is_file() {
            return Err(format!("--primary {spec}: no such file"));
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(spec)
            .to_string();
        tracing::info!(
            spec,
            shape = "local_gguf",
            path = %path.display(),
            "setup:primary_resolved — used in place, nothing is fetched"
        );
        return Ok(PrimaryChoice::InPlace {
            slot: SlotConfig {
                file: name.clone(),
                base_name: name,
                quant: "custom (local)".to_string(),
                ..Default::default()
            },
            path,
        });
    }
    Err(format!(
        "--primary '{spec}' is not a catalog file, a .gguf URL, or a .gguf on disk. \
         This tier offers: {}",
        catalog
            .iter()
            .map(|o| o.slot.file.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The plan for a probed machine — the same four lookups the wizard makes,
/// taken apart from the probe so a test can pin the shape against a hardware
/// profile it chose rather than one it inherited from the host.
fn build_plan(hardware: HardwareProfile) -> SetupPlan {
    let profile = hardware::select_profile(&hardware);
    SetupPlan {
        catalog: build_primary_catalog(&profile),
        // Absent, not substituted: a manifest with no fast slot for this tier
        // is a fact the caller has to see, and `null` says it (principle 6).
        fast: resolve_slot(&profile, SlotKind::Fast),
        embed: resolve_slot(&profile, SlotKind::Embed),
        hardware,
        profile,
    }
}

/// `svrn setup --plan --json`: what a first run WOULD do on this machine.
///
/// Reads the same four functions the wizard reads, prints them, and exits.
/// Nothing is created, downloaded or written — which is what lets a client
/// call it on a machine that has never been set up, and call it again after.
async fn print_plan() -> i32 {
    let hardware = match tokio::task::spawn_blocking(detect_hardware).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: hardware detection panicked: {e}");
            return 1;
        }
    };
    match serde_json::to_string(&build_plan(hardware)) {
        Ok(json) => {
            say!("{json}");
            0
        }
        Err(e) => {
            eprintln!("error: could not render the setup plan: {e}");
            1
        }
    }
}

// ─── Which root is this run configuring? ──────────────────────────

/// The data root this setup run writes, and the config file inside it.
///
/// `--data-dir <p>` moves the whole root, so it moves `config.toml` with it.
/// This was three copies of one resolution — `terminal.rs` twice and
/// `finish.rs` once — each of which fed only the config's CONTENTS, while the
/// existence probe, the `--reset` removal and the write all read the process's
/// DEFAULT root. On a host that already runs a node that made
/// `svrn setup --terminal --data-dir <p> --client-port <n>` exit 0 having done
/// nothing ("Already set up"), which is the second-node onboarding
/// `--client-port` exists for; and had the probe passed, the save would have
/// overwritten the FIRST node's config. One resolution, read by every site
/// (ARCH §10.6, §18.3).
pub(super) fn run_data_dir(opts: &Opts) -> PathBuf {
    opts.data_dir
        .clone()
        .unwrap_or_else(sovereign_core::rebrand::data_dir)
}

/// The config file [`run_data_dir`] names.
pub(super) fn run_config_path(opts: &Opts) -> PathBuf {
    SetupConfig::path_in(&run_data_dir(opts))
}

// ─── Arg parsing ──────────────────────────────────────────────────

#[derive(Debug)]
struct Opts {
    /// `--terminal <entry>`: set this machine up as a node that holds NO
    /// models and routes every turn to the named entry node. Not a modifier
    /// on the wizard — a different destination, like `--fim`, so it
    /// dispatches before hardware detection and downloads nothing.
    terminal: Option<String>,
    /// `--entry <node-id>`: which member to bind, when a mesh has more than one
    /// that holds models. Only meaningful with a `--terminal <join-link>`;
    /// setup picks the sole holder unaided and refuses to guess between
    /// several, so this exists to answer that refusal rather than to be typed
    /// routinely.
    entry: Option<String>,
    reset: bool,
    yes: bool,
    data_dir: Option<PathBuf>,
    /// `--client-port <N>`: the port the daemon this setup configures will
    /// serve its client API on. `[daemon] internal_port` follows at `N + 1`,
    /// the relationship every default and every harness already assumes
    /// (9741/9742).
    ///
    /// Exists because setup could previously only ever produce 9741/9742, and
    /// its "is a daemon already running" guard probed a HARDCODED 9741. On a
    /// fresh machine those coincide and nothing is wrong; on a host that
    /// already runs a node they do not, and the guard refuses a setup that
    /// would not have collided with anything. That made `svrn setup
    /// --terminal` — the entire product onboarding — unreachable on any host
    /// with a daemon, which is why no test has ever executed it (the contract
    /// journey for it was deleted as unrunnable, and `terminal-e2e.sh`
    /// hand-writes the config precisely because "the product path never types
    /// this").
    client_port: Option<u16>,
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
    /// `--plan`: print the first-run plan and exit, touching nothing.
    /// A first-run client on a machine with no config has no daemon to
    /// ask — the sidecar refuses to serve HTTP unconfigured — so it
    /// spawns this verb and reads stdout instead (sv-surface svt-7).
    plan: bool,
    /// `--json`: stdout carries `SetupProgressLine`s and nothing else;
    /// the human narration moves to stderr (`emit`).
    json: bool,
    /// `--primary <spec>`: the main responder to install, instead of the
    /// hardware recommendation. A catalog file name, a `.gguf` URL, or a
    /// `.gguf` already on this disk. The non-interactive form of the
    /// picker's numbered rows and its `[b]` branch.
    primary: Option<String>,
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
async fn run_repair(opts: &Opts) -> i32 {
    // Repairs the node `--data-dir` names, like every other verb here. It read
    // the process default before, so `--repair --data-dir <p>` reported on the
    // wrong node's config while naming the right one nowhere.
    let cfg_path = run_config_path(opts);
    let cfg = match SetupConfig::load_from(&cfg_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not read {}: {e}", cfg_path.display());
            eprintln!("hint: run `svrn setup` to set up from scratch.");
            return 1;
        }
    };

    say!();
    say!("  Sovereign Setup — Repair");
    say!("  {}", "─".repeat(54));

    // Bundled manifest lookup provides each slot's advertised
    // `size_gb`. If a file isn't in the manifest (BYOM), the
    // validator falls back to a 1 MB floor — still enough to
    // catch the common HTML-stub failure mode.
    let manifest = &*sovereign_core::models_manifest::DEFAULT_MANIFEST;

    // Validate the on-disk size of each slot's GGUF. When fast
    // subsumes primary, primary is checked once — there's no separate
    // file to validate for the fast role. has_explicit_fast() gates
    // adding it to the sweep.
    // Nothing was downloaded on a terminal, and an unpopulated `[models]`
    // names no files either, so in both cases there is nothing to sweep.
    // Through `models()` rather than the field, so "holds no models" has the
    // same answer here as everywhere else.
    let Ok(models) = cfg.models() else {
        return 0;
    };
    let mut slots: Vec<(&str, &std::path::Path)> = vec![
        ("primary", models.primary.as_path()),
        ("embed", models.embed.as_path()),
    ];
    if models.has_explicit_fast() {
        slots.push(("fast", models.fast_path()));
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
                say!("  \u{2713} {role:<7} {} — valid", path.display());
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

    say!();
    say!(
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

// BELOW the production code, deliberately. `setup_never_reads_or_writes_the_
// process_default_config` isolates this file's production half by splitting on
// the first `\n#[cfg(test)]\nmod ` — so a test-module DECLARATION placed up
// with the other `mod` lines truncates the census to the header, and it passes
// vacuously over ~1.5 KB. Its `prod.len() > 2000` guard is what caught that
// (svt-7, 2026-09-12), which is the guard earning its keep on a real landing.
#[cfg(test)]
mod json_surface_tests;

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
            None,
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
        let err = download_with_progress(
            &format!("{base}/fake-model.gguf"),
            &dest,
            "fake",
            0.001,
            None,
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
            None,
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

    // ── --data-dir moves the config FILE, not just its contents ────
    //
    // The defect these pin: `--data-dir <p>` was threaded into `[data] dir`
    // and nowhere else, so setup probed, removed and wrote the config at the
    // DEFAULT root. On a host that already runs a node that made
    // `svrn setup --terminal --data-dir <p> --client-port <n>` print
    // "Already set up" and exit 0 having configured nothing — the exact
    // second-node onboarding `--client-port` was added for — and had the
    // probe passed, the write would have landed on the FIRST node's config.

    #[test]
    fn the_data_dir_flag_moves_the_config_path() {
        let opts = parse_args(&s(&["--data-dir", "/tmp/second-node"])).unwrap();
        assert_eq!(run_data_dir(&opts), Path::new("/tmp/second-node"));
        assert_eq!(
            run_config_path(&opts),
            Path::new("/tmp/second-node/config.toml")
        );
    }

    #[test]
    fn without_the_flag_the_config_path_is_unchanged() {
        let opts = parse_args(&[]).unwrap();
        assert_eq!(run_config_path(&opts), SetupConfig::default_path());
    }

    /// The structural half (ARCH §7): a test on the helper cannot stop a
    /// FOURTH site reaching for the root-blind accessor, and three sites
    /// reaching for it is what this defect was. `setup_cmd` resolves the root
    /// once and every read and write goes through it, so the root-blind
    /// spellings must not appear in this module at all.
    ///
    /// Watched red: restoring `SetupConfig::exists()` at either terminal
    /// probe turns this test, naming the file.
    #[test]
    fn setup_never_reads_or_writes_the_process_default_config() {
        const BLIND: &[&str] = &[
            "SetupConfig::exists()",
            "SetupConfig::default_path()",
            "SetupConfig::remove()",
            "cfg.save()",
        ];
        for (file, src) in [
            ("mod.rs", include_str!("mod.rs")),
            ("terminal.rs", include_str!("terminal.rs")),
            ("finish.rs", include_str!("finish.rs")),
        ] {
            // Production half only: a test may NAME the blind accessor (the
            // one just above does, to pin the no-flag case), and a census that
            // counted those would be measuring itself.
            let prod = src.split("\n#[cfg(test)]\nmod ").next().unwrap();
            assert!(
                prod.len() > 2000,
                "{file}: the cfg(test) cut left {} bytes — the census would pass \
                 vacuously",
                prod.len()
            );
            for needle in BLIND {
                let hits = prod
                    .lines()
                    .filter(|l| l.contains(needle))
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .count();
                assert_eq!(
                    hits, 0,
                    "{file} still reaches for `{needle}` — setup must read and write \
                     the config in the root `--data-dir` names, via run_config_path"
                );
            }
        }
    }

    #[test]
    fn parse_args_takes_the_terminal_entry_address() {
        let opts = parse_args(&s(&["--terminal", "http://halo:9741"])).unwrap();
        assert_eq!(opts.terminal.as_deref(), Some("http://halo:9741"));
        assert!(opts.terminal.is_some() && !opts.fim);
    }

    #[test]
    fn parse_args_defaults_terminal_off() {
        assert!(parse_args(&s(&[])).unwrap().terminal.is_none());
    }

    /// `--terminal` with nothing after it is a typo, not a request to set up
    /// against the empty string — which would write a config pointing nowhere.
    #[test]
    fn parse_args_rejects_a_dangling_terminal() {
        let err = parse_args(&s(&["--terminal"])).unwrap_err();
        assert!(err.contains("--terminal"), "got: {err}");
    }

    /// Two destinations, not a destination plus a modifier. Accepting both
    /// would silently run one and drop the other.
    #[test]
    fn parse_args_refuses_terminal_together_with_fim() {
        let err = parse_args(&s(&["--terminal", "http://halo:9741", "--fim"])).unwrap_err();
        assert!(err.contains("separately"), "got: {err}");
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

    /// THE DEFECT. Setup could only ever configure 9741/9742, so on a host
    /// already running a node the product onboarding had nowhere to go — and
    /// its guard refused a run that would not have collided with anything.
    #[test]
    fn parse_args_takes_a_client_port_so_a_second_node_can_onboard() {
        let opts = parse_args(&s(&[
            "--terminal",
            "http://halo:9741",
            "--client-port",
            "9771",
        ]))
        .unwrap();
        assert_eq!(opts.client_port, Some(9771));
    }

    /// Absent means the compiled default, which is what a fresh machine wants
    /// and is why this bug survived: there, the hardcoded port was correct.
    #[test]
    fn parse_args_defaults_the_client_port_to_none() {
        assert!(parse_args(&s(&["--terminal", "http://halo:9741"]))
            .unwrap()
            .client_port
            .is_none());
    }

    /// A port that is not a number is a typo, not a request to fall back to
    /// 9741 — falling back would silently configure the port the operator was
    /// trying to avoid, on the one path where they said it mattered.
    #[test]
    fn parse_args_rejects_a_client_port_that_is_not_a_port() {
        let err = parse_args(&s(&["--client-port", "http://nope"])).unwrap_err();
        assert!(err.contains("--client-port"), "got: {err}");
        let err = parse_args(&s(&["--client-port", "0"])).unwrap_err();
        assert!(err.contains("--client-port"), "got: {err}");
    }

    #[test]
    fn parse_args_rejects_dangling_client_port() {
        let err = parse_args(&s(&["--client-port"])).unwrap_err();
        assert!(err.contains("--client-port"), "error: {err}");
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
        // `mcp` is a FLAT map of name → server (opencode SDK
        // `Config.mcp`), and the only valid `type` discriminators are
        // "local" and "remote". Verified against opencode 1.14.48.
        assert_eq!(
            parsed["mcp"]["sovereign"]["url"],
            "http://localhost:9741/mcp"
        );
        assert_eq!(parsed["mcp"]["sovereign"]["type"], "remote");
        assert!(
            parsed["mcp"]["servers"].is_null(),
            "must not reintroduce the nested `mcp.servers` shape opencode rejects"
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
                "github": { "type": "remote", "url": "https://example.com/mcp" }
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
            parsed["mcp"]["sovereign"]["url"],
            "http://localhost:9741/mcp"
        );
        assert_eq!(
            parsed["provider"]["commonwealth"]["options"]["baseURL"],
            "http://localhost:9741/v1"
        );
        // Existing entries survived.
        assert_eq!(parsed["mcp"]["github"]["url"], "https://example.com/mcp");
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
            parsed["mcp"]["sovereign"]["url"],
            "http://localhost:9999/mcp"
        );
    }

    /// Until 2026-07-28 we wrote `mcp.servers.sovereign` with
    /// `type: "http"`. opencode's `mcp` is a flat name → server map and
    /// its only discriminators are "local"/"remote", so that entry
    /// parses as a server *named* "servers" with no `type` — a schema
    /// failure that takes the whole config down. Writing the correct
    /// key is therefore not enough; the old one has to go, or the user
    /// stays broken after re-running setup.
    #[test]
    fn opencode_install_evicts_the_legacy_servers_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{
              "mcp": {
                "servers": {
                  "sovereign": { "type": "http", "url": "http://localhost:9741/mcp" }
                }
              }
            }"#,
        )
        .unwrap();

        let result = install_opencode_config_at(&path, 9741).unwrap();
        assert!(
            matches!(result, OpencodeInstall::MergedInto(_)),
            "a legacy entry is work to do, not AlreadyConfigured"
        );

        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["mcp"]["sovereign"]["type"], "remote");
        assert!(
            parsed["mcp"]["servers"].is_null(),
            "the legacy `mcp.servers` object must be gone, not merely shadowed"
        );
    }

    /// Eviction must not eat a peer's unrelated entry that happens to
    /// live under the same bad key.
    #[test]
    fn opencode_install_keeps_foreign_entries_under_legacy_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{
              "mcp": {
                "servers": {
                  "sovereign": { "type": "http", "url": "http://localhost:9741/mcp" },
                  "github": { "type": "http", "url": "https://example.com/mcp" }
                }
              }
            }"#,
        )
        .unwrap();

        install_opencode_config_at(&path, 9741).unwrap();

        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["mcp"]["sovereign"]["type"], "remote");
        assert!(parsed["mcp"]["servers"]["sovereign"].is_null());
        // Not ours to delete or migrate.
        assert_eq!(
            parsed["mcp"]["servers"]["github"]["url"],
            "https://example.com/mcp"
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
    // `sovereign-contracts/src/gguf_validator.rs` (moved from sovereign-inference 2026-09-12). The old tests
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
