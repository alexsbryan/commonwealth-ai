// SPDX-License-Identifier: AGPL-3.0-or-later
//! Setup finish path — write config, (optionally) install the service,
//! bring the daemon up, run doctor, write opencode config. Extracted
//! from `setup_cmd` (§3.2).

use std::io::{self, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::service_install;
use crate::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};

use super::opencode::{install_opencode_config, opencode_config_snippet, OpencodeInstall};
use super::{ModelPaths, Opts};

pub(super) async fn finish_with_paths(paths: ModelPaths, opts: &Opts) -> i32 {
    // ── Write config ─────────────────────────────────────────────
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".sovereign"));

    let cfg = SetupConfig {
        compute: Default::default(),
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
            extra: std::collections::BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
            fim: None,
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

    // ── opencode config — write the global file directly ─────────
    //
    // Earlier the script just printed a snippet pointing at
    // `.opencode/config.json` (project-local). Real opencode reads
    // `~/.config/opencode/opencode.json`; users had to figure that
    // out themselves. Auto-write so a fresh `svrn setup` is
    // immediately usable from opencode without copy-paste plumbing.
    match install_opencode_config(cfg.daemon.client_port) {
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
            eprintln!("{}", opencode_config_snippet(cfg.daemon.client_port));
        }
    }

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
