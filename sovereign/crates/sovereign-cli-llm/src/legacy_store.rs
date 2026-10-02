// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-shot migration of a legacy SQLite store into cw-rails' mesh store
//! (five-programs fp-87, §12 D4), and the one dial `svrn portfolio` and
//! `svrn newsworthy` read and write through.
//!
//! The READ half runs in `sovereign-cli-mesh kv-export` — commonwealth-state's
//! own backend, in its own package, so there is no SQL here. The WRITE half is
//! the daemon's one `RailsKv` client. A file that is migrated gets a
//! `<file>.migrated` marker beside it, so a second run migrates nothing. A
//! failed migration is a named refusal, writes no marker and leaves the legacy
//! file untouched — never a silent empty store (principles 6, 10).

use std::path::{Path, PathBuf};

use sovereign_contracts::peer::{ReplicatedKv, ReplicatedKvEntry};
use sovereign_daemon::rails_client::kv::RailsKv;

/// The `RailsKv` at the base `[daemon] rails_base` names, through THE one
/// reader of that key. It only dials: cw-rails is brought up by `svrn mesh
/// up` (pb-rails-untether), and an absent one is reported by the first call
/// on the store, naming that verb.
pub(crate) fn rails_kv() -> RailsKv {
    use sovereign_core::setup_config::SetupConfig;
    let daemon = match SetupConfig::load() {
        Ok(c) => c.daemon,
        Err(e) => {
            tracing::warn!(error = %e, "legacy store: no setup config; the rails base is the default");
            SetupConfig::unconfigured().daemon
        }
    };
    let base = sovereign_daemon::rails_client::resolve_rails_base(&daemon);
    tracing::debug!(rails_base = %base, "legacy store: dialing cw-rails; nothing is brought up");
    RailsKv::new(base)
}

fn marker(path: &Path) -> PathBuf {
    let mut m = path.as_os_str().to_owned();
    m.push(".migrated");
    PathBuf::from(m)
}

/// Migrate `path` into `kv` unless it is absent or already migrated. Each
/// `(legacy, current)` pair names an app_id read from the file and the one it
/// is written under. Returns the number of rows written.
pub(crate) fn migrate_if_needed(
    path: &Path,
    app_ids: &[(&str, &str)],
    kv: &dyn ReplicatedKv,
    read: impl FnOnce(&Path, &[String]) -> Result<Vec<ReplicatedKvEntry>, String>,
) -> Result<usize, String> {
    if !path.is_file() {
        tracing::debug!(path = %path.display(), "legacy store: none to migrate");
        return Ok(0);
    }
    let done = marker(path);
    if done.exists() {
        tracing::debug!(path = %path.display(), "legacy store: already migrated");
        return Ok(0);
    }
    let refuse = |what: String| {
        tracing::warn!(path = %path.display(), error = %what, "legacy store: migration refused");
        format!(
            "migrating the legacy store {} refused: {what}; the file is untouched",
            path.display()
        )
    };
    let legacy: Vec<String> = app_ids.iter().map(|(l, _)| l.to_string()).collect();
    let rows = read(path, &legacy).map_err(refuse)?;
    for row in &rows {
        let Some((_, current)) = app_ids.iter().find(|(l, _)| *l == row.app_id) else {
            return Err(refuse(format!("the export named app_id `{}`", row.app_id)));
        };
        kv.set(current, &row.key, row.value.clone(), row.origin)
            .map_err(|e| refuse(e.to_string()))?;
    }
    std::fs::write(&done, format!("{} rows\n", rows.len()))
        .map_err(|e| refuse(format!("write {}: {e}", done.display())))?;
    tracing::info!(path = %path.display(), rows = rows.len(), "legacy store: migrated into cw-rails");
    Ok(rows.len())
}

/// The READ half: `sovereign-cli-mesh kv-export`, found beside this binary
/// (or at `SOVEREIGN_CLI_MESH_BIN`, the dispatcher's override).
pub(crate) fn export_via_cli_mesh(
    path: &Path,
    app_ids: &[String],
) -> Result<Vec<ReplicatedKvEntry>, String> {
    let bin = std::env::var_os("SOVEREIGN_CLI_MESH_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            Some(
                std::fs::canonicalize(exe)
                    .ok()?
                    .parent()?
                    .join("sovereign-cli-mesh"),
            )
        })
        .filter(|p| p.is_file())
        .ok_or_else(|| "cannot find the sovereign-cli-mesh sibling that reads it".to_string())?;
    let out = std::process::Command::new(&bin)
        .arg("kv-export")
        .arg(path)
        .args(app_ids)
        .output()
        .map_err(|e| format!("run {}: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("read kv-export's answer: {e}"))
}

#[cfg(test)]
#[path = "legacy_store/tests.rs"]
mod tests;
