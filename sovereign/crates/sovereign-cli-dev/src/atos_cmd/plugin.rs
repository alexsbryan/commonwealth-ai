//! `sovereign atos install-plugin` — (re)install the opencode plugin
//! shipped with this CLI into `<repo>/.opencode/plugins/sovereign-atos.ts`.
//!
//! Idempotent — a second invocation reports `up to date`. Upgrades
//! from a prior version report the transition explicitly so the
//! operator sees that a plugin bump happened.

use std::path::PathBuf;

pub(crate) async fn cmd_install_plugin(_args: &[String]) -> i32 {
    let repo_root = find_repo_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    match crate::atos_plugin::install_plugin(&repo_root) {
        Ok(crate::atos_plugin::InstallOutcome::Installed) => {
            println!(
                "\u{2713} Installed {} (v{}).",
                crate::atos_plugin::plugin_rel_path(),
                crate::atos_plugin::PLUGIN_VERSION
            );
            0
        }
        Ok(crate::atos_plugin::InstallOutcome::UpToDate) => {
            println!(
                "\u{2713} {} already at v{} — no change.",
                crate::atos_plugin::plugin_rel_path(),
                crate::atos_plugin::PLUGIN_VERSION
            );
            0
        }
        Ok(crate::atos_plugin::InstallOutcome::Replaced { prior_version }) => {
            println!(
                "\u{2713} Updated {} ({} → v{}).",
                crate::atos_plugin::plugin_rel_path(),
                prior_version
                    .as_deref()
                    .map(|v| format!("v{v}"))
                    .unwrap_or_else(|| "unversioned".into()),
                crate::atos_plugin::PLUGIN_VERSION
            );
            0
        }
        Err(e) => {
            eprintln!(
                "\u{2717} Could not install {}: {e}",
                crate::atos_plugin::plugin_rel_path()
            );
            1
        }
    }
}

pub(super) fn find_repo_root() -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}
