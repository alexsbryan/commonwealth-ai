// SPDX-License-Identifier: AGPL-3.0-or-later
//! cw-rails' boot unit: `svrn mesh up` installs and enables it, so a node that
//! brought cw-rails up comes back on the mesh after a reboot (operator,
//! phase-b-51). Until the flip the daemon was the mesh endpoint and
//! sovereign.service restarted it; after it cw-rails is, and nothing else
//! starts cw-rails at boot. The mesh program owns its own restart as it owns
//! its bring-up (phase-b-31): neither sovereign.service nor
//! `svrn install-service` installs this unit (principle 12).
//!
//! The writer is host-kit's (`host_kit::service`), the one svrn's own unit
//! goes through (§12 3a rung 4); this module supplies only cw-rails' name,
//! text and argv. The unit is ENABLED, not started: the bring-up already
//! started cw-rails, and a second one on the same root would lose its
//! `rails.lock` and burn the unit's start limit.

use std::path::{Path, PathBuf};

use host_kit::service::{escape_specifiers, SystemdUser, UserUnit};
use sovereign_turn_client::rails_kv::RAILS_UNIT;

/// The unit's text, with `{EXEC}`, `{PATH}` and `{RAILS_DIR}` to fill.
const TEMPLATE: &str = include_str!("../data/systemd/cw-rails.service");

/// Where a user unit named [`RAILS_UNIT`] lives: `~/.config/systemd/user/`.
pub fn unit_path() -> Result<PathBuf, String> {
    let config =
        dirs::config_dir().ok_or_else(|| "cannot resolve user config directory".to_string())?;
    Ok(config.join("systemd").join("user").join(RAILS_UNIT))
}

/// One ExecStart word: double-quoted, so a path with a space stays one
/// argument, and `%`-escaped, since `%` is a unit-file specifier.
fn exec_word(word: &str) -> String {
    let quoted = word.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escape_specifiers(&quoted))
}

/// The unit's text for `bin` run with `args` (the bring-up's
/// `rails_up::run_args`), the installing shell's `path`, and the data root
/// the bring-up resolved.
pub fn unit_text(bin: &Path, args: &[String], path: &str, rails_dir: &Path) -> String {
    let exec = std::iter::once(bin.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|w| exec_word(&w))
        .collect::<Vec<_>>()
        .join(" ");
    TEMPLATE
        .replace("{EXEC}", &exec)
        .replace("{PATH}", &escape_specifiers(path))
        .replace(
            "{RAILS_DIR}",
            &escape_specifiers(&rails_dir.to_string_lossy()),
        )
}

/// Write and enable the unit at `unit_path` through `systemd`.
pub fn install_with(
    systemd: &SystemdUser,
    unit_path: &Path,
    bin: &Path,
    args: &[String],
    rails_dir: &Path,
) -> Result<(), String> {
    let content = unit_text(bin, args, &host_kit::service::captured_path(), rails_dir);
    tracing::debug!(unit = %unit_path.display(), bin = %bin.display(), args = ?args,
                    rails_dir = %rails_dir.display(), "rails unit: installing");
    systemd.install(
        &UserUnit {
            name: RAILS_UNIT,
            path: unit_path,
            content: &content,
            prefix: "svrn mesh up",
            what: "cw-rails mesh endpoint",
        },
        false,
    )
}

/// `svrn mesh up`'s last step: install and enable the boot unit for the
/// cw-rails it just reached, and say what happened. A failure is reported and
/// does not fail the verb: cw-rails is up, and what the node loses is named.
pub fn install_after_bring_up(base: &str, local_only: bool, mdns: bool) {
    if !cfg!(target_os = "linux") {
        tracing::warn!(
            os = std::env::consts::OS,
            "rails unit: no boot unit on this platform"
        );
        eprintln!(
            "cw-rails boot unit: not installed — only systemd user units are written \
             (this is {}). After a reboot this node is off the mesh until `svrn mesh up` \
             runs again, or run `cw-rails run` under your own service manager.",
            std::env::consts::OS
        );
        return;
    }
    let installed = (|| -> Result<PathBuf, String> {
        let port = crate::rails_up::loopback_port(base)?;
        let bin = crate::rails_up::locate_rails().ok_or_else(|| {
            "no cw-rails binary (CW_RAILS_BIN, beside this program, or PATH)".to_string()
        })?;
        let bin = std::fs::canonicalize(&bin)
            .map_err(|e| format!("cannot resolve {}: {e}", bin.display()))?;
        let path = unit_path()?;
        install_with(
            &SystemdUser::system(),
            &path,
            &bin,
            &crate::rails_up::run_args(port, local_only, mdns),
            &commonwealth_media::rails_data_dir(),
        )?;
        Ok(path)
    })();
    match installed {
        Ok(path) => {
            tracing::info!(unit = %path.display(), "rails unit: installed and enabled");
            println!(
                "cw-rails boot unit: {RAILS_UNIT} enabled ({}); it starts cw-rails at boot",
                path.display()
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "rails unit: not installed");
            eprintln!(
                "cw-rails boot unit: NOT installed ({e}). cw-rails is up now, but after a \
                 reboot this node is off the mesh until `svrn mesh up` runs again."
            );
        }
    }
}

#[cfg(test)]
#[path = "rails_unit_tests.rs"]
mod tests;
