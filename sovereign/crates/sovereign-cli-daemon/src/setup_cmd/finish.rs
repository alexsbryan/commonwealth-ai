// SPDX-License-Identifier: AGPL-3.0-or-later
//! Setup finish path — write config, (optionally) install the service,
//! bring the daemon up, run doctor, write opencode config. Extracted
//! from `setup_cmd` (§3.2).

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::service_install;
use crate::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};

use super::opencode::{install_opencode_config, opencode_config_snippet, OpencodeInstall};
use super::{ModelPaths, Opts};

pub(super) async fn finish_with_paths(paths: ModelPaths, opts: &Opts) -> i32 {
    // ── Write config ─────────────────────────────────────────────
    // `rebrand::data_dir()`, not a hand-rolled `home/.sovereign` — that
    // spelling ignored the `~/.svrnmesh` rebrand, so on a fresh install
    // setup wrote a `data.dir` no other surface resolves to. Same
    // path-SSOT rule the registration below follows (`clippy.toml`).
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(sovereign_core::rebrand::data_dir);

    let cfg = SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: ModelsSection {
            primary: paths.primary,
            // `svrn setup` always prompts for an explicit fast
            // GGUF (BYOM is committed; no blank-to-use-default).
            // Optional-fast is for non-interactive callers (pod
            // entrypoint, tests).
            fast: Some(paths.fast),
            embed: paths.embed,
            code: paths.code,
            context_size: None,
            fast_context_size: None,
            extra: std::collections::BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
            edit: None,
        },
        daemon: DaemonSection::default(),
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

    // ── opencode config — write the global file directly ─────────
    //
    // ABOVE the `wizard_only` early return, deliberately. Both entry
    // points into setup set that flag — `svrn daemon --setup-only`
    // prepends it (`daemon_cmd/mod.rs:122`) and direct `svrn setup`
    // force-appends it (`setup_cmd/mod.rs:104`) — so nothing below
    // the return runs in production. This write used to sit down
    // there, which is why `svrn doctor` could report a missing
    // opencode config and hint "Run `svrn setup` to write it": advice
    // no invocation of setup could satisfy. Writing a JSON file does
    // not compete with the daemon for `:9741`, which is the only
    // reason the branch below is gated, so it does not belong there.
    write_opencode_config(cfg.daemon.client_port);

    // ── Register the repo the user ran setup in ──────────────────
    //
    // ABOVE the `wizard_only` early return, for the same reason the
    // opencode write is: both entry points set that flag, so nothing
    // below it runs in production. Until this landed, `svrn setup`
    // finished with models installed and NOTHING watching any code —
    // doctor's `watcher_freshness` then reported "NO projects
    // registered" while every other surface looked green, which is a
    // day to rediscover.
    register_setup_repo();

    // Phase 4: when invoked from `svrn daemon` first-boot or
    // `svrn daemon --setup-only`, we stop here. The daemon's
    // own startup loads models from this freshly-written config; a
    // service-manager registration would just compete with us for
    // `:9741`. The legacy `svrn setup` runs in this mode too —
    // service install moved to the explicit `svrn install-service`.
    if opts.wizard_only {
        println!();
        println!("  \u{2713} Wizard complete.");
        println!();
        println!("  Next steps:");
        println!("    svrn daemon                   # start the daemon (foreground)");
        println!("    svrn install-service          # register as a launchd/systemd service");
        return 0;
    }

    // ── Install service ──────────────────────────────────────────
    let bin_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  warning: cannot resolve current binary path: {e}");
            eprintln!("  skipping service registration; run `svrn daemon run` manually.");
            return 0;
        }
    };
    match service_install::install_service(&bin_path) {
        Ok(()) => println!("    \u{2713} Service registered"),
        Err(e) => {
            eprintln!("  warning: service registration failed: {e}");
            eprintln!("  run `svrn daemon run` manually to start the daemon.");
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
        eprintln!(
            "  warning: daemon didn't respond on :{} within 30s.",
            cfg.daemon.client_port
        );
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
        println!("    Run `svrn doctor --fix` to attempt repairs.");
    }

    // ── Banner ───────────────────────────────────────────────────
    println!();
    println!("  \u{2713} Mesh running — 1 node (you)");
    println!(
        "  \u{2713} Endpoint: localhost:{}/v1",
        cfg.daemon.client_port
    );

    // ── Next steps — the two commands a new user most wants next ──
    println!();
    println!("  Next steps:");
    println!("    svrn chat session          # start talking");
    println!("    svrn model list            # see the models it loaded; `svrn model set <slot> <file>` to change one (applies live)");

    let _ = Arc::new(()); // placeholder; Arc usage removed post-refactor
    0
}

/// Write the global opencode config so a fresh setup is immediately
/// usable from opencode without copy-paste plumbing.
///
/// Real opencode reads `~/.config/opencode/opencode.json`; earlier
/// versions only printed a project-local `.opencode/config.json`
/// snippet and left users to discover the real path themselves.
///
/// Failure is non-fatal — setup has already written a working
/// `config.toml` by this point, and an editor integration is not worth
/// failing the install over. The user gets the snippet to paste.
fn write_opencode_config(client_port: u16) {
    match install_opencode_config(client_port) {
        Ok(OpencodeInstall::Created(path)) => {
            println!();
            println!("  \u{2713} Wrote opencode config — {}", path.display());
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
            eprintln!("  paste this into ~/.config/opencode/opencode.json yourself:");
            eprintln!("{}", opencode_config_snippet(client_port));
        }
    }
}

/// Register the repo the user ran `svrn setup` in, so the daemon indexes
/// and watches it from its very next start.
///
/// # Why the registry file rather than the register route
///
/// `svrn project register` POSTs `/v1/projects/register`, and there is
/// no daemon here yet — setup's whole job is to write the config the
/// daemon will boot from. So this writes the entry directly, through the
/// registry's OWN `load`/`upsert`/`save` (never a hand-rolled JSON
/// write), and the daemon adopts it: `daemon_cmd/bootstrap.rs` does
/// `Registry::load()` → `reindexer.register(entry)` for every entry at
/// startup, and `Reindexer::register` schedules a
/// `RebuildReason::Startup`. The index therefore exists after the `svrn
/// daemon` that setup prints as the next step — which is the honest
/// claim, and the one the console message makes.
///
/// # What it refuses, and says so
///
/// Nothing here is silent. Not in a repo, already registered, nested
/// under an existing registration, or rooted at `$HOME` — each prints
/// what it saw and what to do. `$HOME` is the one that matters: a
/// dotfiles repo in the home directory is a real shape, and registering
/// it would set the daemon to index everything the user owns.
fn register_setup_repo() {
    use sovereign_mesh::projects::{ProjectEntry, Registry};

    let root = sovereign_cli_shared::repo::find_repo_root().map(|r| r.canonicalize().unwrap_or(r));
    let home = sovereign_core::rebrand::user_home().map(|h| h.canonicalize().unwrap_or(h));

    // Read BEFORE deciding, and never write a registry we could not
    // read — an unreadable file overwritten with a one-entry registry
    // silently drops every other project the user had.
    let registry = match Registry::load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!();
            eprintln!("  warning: could not read the project registry ({e});");
            eprintln!(
                "  skipping registration. Run `svrn project register` once the daemon is up."
            );
            return;
        }
    };

    let decision = decide_registration(root.as_deref(), home.as_deref(), &registry);
    for line in decision.explain() {
        println!("{line}");
    }
    let Registration::Register { corpus_id, root } = decision else {
        return;
    };

    let mut registry = registry;
    registry.upsert(ProjectEntry::new(corpus_id.clone(), root.clone()));
    match registry.save() {
        Ok(()) => {
            println!(
                "  \u{2713} Registered \"{corpus_id}\" for indexing — {}",
                root.display()
            );
            println!("    The daemon builds its call graph and starts watching on its next start.");
        }
        Err(e) => {
            eprintln!();
            eprintln!("  warning: could not save the project registry: {e}");
            eprintln!("  run `svrn project register` once the daemon is up.");
        }
    }
}

/// What setup decided to do about the current directory, and why.
///
/// Separated from the IO so every refusal has a test. A registration
/// guard whose failing input nobody can name is not a guard
/// (ARCH §18.1), and the `$HOME` case in particular has a consequence
/// (the daemon indexing everything the user owns) that is too expensive
/// to discover in the field.
#[derive(Debug, PartialEq)]
enum Registration {
    NotARepo,
    IsHomeDirectory,
    AlreadyRegistered(String),
    NestsWith {
        existing_id: String,
        existing_root: PathBuf,
    },
    Register {
        corpus_id: String,
        root: PathBuf,
    },
}

impl Registration {
    /// The console lines for this outcome. Held here, beside the
    /// decision, so a new variant cannot ship silent.
    fn explain(&self) -> Vec<String> {
        let mut out = vec![String::new()];
        match self {
            Self::NotARepo => {
                out.push(
                    "  \u{2139} Not inside a git repo, so nothing was registered for indexing."
                        .into(),
                );
                out.push(
                    "    From a repo you want code intelligence on: svrn project register".into(),
                );
            }
            Self::IsHomeDirectory => {
                out.push("  \u{2139} Skipped indexing: this repo IS your home directory.".into());
                out.push(
                    "    Indexing all of $HOME is almost never what you want. Register a".into(),
                );
                out.push("    specific project instead: cd <repo> && svrn project register".into());
            }
            Self::AlreadyRegistered(id) => {
                out.push(format!(
                    "  \u{2713} Project \"{id}\" is already registered — leaving it as is."
                ));
            }
            Self::NestsWith {
                existing_id,
                existing_root,
            } => {
                out.push(format!(
                    "  \u{2139} Skipped indexing: \"{existing_id}\" is already registered at {}, which",
                    existing_root.display()
                ));
                out.push(
                    "    nests with this repo. Nested registrations collapse the rebuild queue."
                        .into(),
                );
                out.push("    If you really want both: svrn project register --force".into());
            }
            // The success line names the id the registry actually got,
            // so it is printed by the caller after the save succeeds —
            // announcing it here would claim a write that can still fail.
            Self::Register { .. } => {}
        }
        out
    }
}

fn decide_registration(
    root: Option<&Path>,
    home: Option<&Path>,
    registry: &sovereign_mesh::projects::Registry,
) -> Registration {
    let Some(root) = root else {
        return Registration::NotARepo;
    };
    if home == Some(root) {
        return Registration::IsHomeDirectory;
    }
    let corpus_id = sovereign_cli_shared::repo::derive_corpus_id(root);
    if registry.find(&corpus_id).is_some() {
        return Registration::AlreadyRegistered(corpus_id);
    }
    if let Some(conflict) = registry.nested_conflict(&corpus_id, root) {
        return Registration::NestsWith {
            existing_id: conflict.corpus_id.clone(),
            existing_root: conflict.root.clone(),
        };
    }
    Registration::Register {
        corpus_id,
        root: root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_mesh::projects::{ProjectEntry, Registry};

    fn registry_with(entries: &[(&str, &str)]) -> Registry {
        let mut r = Registry::default();
        for (id, root) in entries {
            r.upsert(ProjectEntry::new(*id, PathBuf::from(root)));
        }
        r
    }

    #[test]
    fn a_repo_outside_home_registers_under_its_directory_name() {
        let d = decide_registration(
            Some(Path::new("/Users/dev/code/my-service")),
            Some(Path::new("/Users/dev")),
            &Registry::default(),
        );
        assert_eq!(
            d,
            Registration::Register {
                corpus_id: "my-service".into(),
                root: PathBuf::from("/Users/dev/code/my-service"),
            }
        );
    }

    /// The expensive one. A dotfiles repo in $HOME is a real shape, and
    /// registering it points the reindexer at everything the user owns.
    #[test]
    fn the_home_directory_is_never_registered() {
        let d = decide_registration(
            Some(Path::new("/Users/dev")),
            Some(Path::new("/Users/dev")),
            &Registry::default(),
        );
        assert_eq!(d, Registration::IsHomeDirectory);
    }

    #[test]
    fn setup_run_outside_a_repo_registers_nothing() {
        assert_eq!(
            decide_registration(None, Some(Path::new("/Users/dev")), &Registry::default()),
            Registration::NotARepo
        );
    }

    /// Re-running setup must not disturb an existing registration (it
    /// would reset nothing today, but the intent is "leave it alone").
    #[test]
    fn an_existing_registration_is_left_alone() {
        let reg = registry_with(&[("my-service", "/Users/dev/code/my-service")]);
        assert_eq!(
            decide_registration(
                Some(Path::new("/Users/dev/code/my-service")),
                Some(Path::new("/Users/dev")),
                &reg
            ),
            Registration::AlreadyRegistered("my-service".into())
        );
    }

    #[test]
    fn a_repo_nested_under_a_registered_one_is_refused_not_forced() {
        let reg = registry_with(&[("monorepo", "/Users/dev/code/monorepo")]);
        let d = decide_registration(
            Some(Path::new("/Users/dev/code/monorepo/services/api")),
            Some(Path::new("/Users/dev")),
            &reg,
        );
        assert_eq!(
            d,
            Registration::NestsWith {
                existing_id: "monorepo".into(),
                existing_root: PathBuf::from("/Users/dev/code/monorepo"),
            }
        );
    }

    /// Every refusal must SAY something — a silent skip is how `svrn
    /// setup` came to finish with nothing watching any code in the
    /// first place.
    #[test]
    fn every_refusal_explains_itself() {
        for d in [
            Registration::NotARepo,
            Registration::IsHomeDirectory,
            Registration::AlreadyRegistered("x".into()),
            Registration::NestsWith {
                existing_id: "x".into(),
                existing_root: PathBuf::from("/a"),
            },
        ] {
            let lines = d.explain();
            assert!(
                lines.iter().any(|l| !l.trim().is_empty()),
                "{d:?} would skip registration silently"
            );
        }
    }
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
    eprintln!("    launchctl list | grep svrnmesh        # is the service loaded?");
    #[cfg(target_os = "linux")]
    eprintln!("    systemctl --user status svrnmesh      # is the unit active?");

    eprintln!(
        "    svrn daemon run                       # run in the foreground to see errors live"
    );
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
        eprintln!(
            "  No log at {} yet — service likely didn't start.",
            err_log.display()
        );
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
        if client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}
