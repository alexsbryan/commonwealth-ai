// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wizard's mesh join, run by the one daemon rather than a copy built here.
//!
//! `svrn setup --terminal <link>` used to construct the daemon in this process
//! to join. It now spawns the `sovereign-daemon` sibling's admin-join launch
//! (`Launch::AdminJoin`) over a provisional config, hands it the invite on
//! stdin (never argv: the link carries the join key and a process's cmdline is
//! world-readable), and reads its stdout for the one `joined` line. The join
//! and the node key are cw-rails' since pb-mesh-exit-transport, so the wizard
//! first runs the one bring-up, `svrn mesh up` ([`bring_up_rails`], an exec
//! of the cli-mesh sibling), and the child joins through that cw-rails.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use sovereign_contracts::launch::{ADMIN_JOIN_VERB, JOINED_LINE_PREFIX};
use sovereign_core::setup_config::{DataSection, SetupConfig};

/// A joined child. Dropping it stops the child (kill, then reap) and removes
/// the provisional config, so every exit path of the wizard stops it.
pub(super) struct JoinChild {
    child: Child,
    provisional: PathBuf,
    /// The child's client port; `GET /v1/mesh/venues` is read here.
    pub(super) client_port: u16,
}

impl JoinChild {
    pub(super) fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for JoinChild {
    fn drop(&mut self) {
        stop(&mut self.child, &self.provisional);
    }
}

/// Kill and reap. A child that already exited is the goal state, so the
/// kill's error is logged at debug and not raised.
fn stop(child: &mut Child, provisional: &Path) {
    if let Err(e) = child.kill() {
        tracing::debug!(pid = child.id(), error = %e, "setup join child: kill (already exited?)");
    }
    match child.wait() {
        Ok(status) => tracing::debug!(pid = child.id(), %status, "setup join child: stopped"),
        Err(e) => tracing::warn!(pid = child.id(), error = %e, "setup join child: reap failed"),
    }
    if let Err(e) = std::fs::remove_file(provisional) {
        tracing::debug!(path = %provisional.display(), error = %e, "setup join child: provisional config not removed");
    }
}

/// Why the join produced no joined child.
pub(super) enum JoinFailure {
    /// The child ran and exited without joining. Its stderr (inherited)
    /// already carries the reason.
    Refused,
    /// The child could not be started at all.
    Launch(String),
}

/// `svrn mesh up` over the run data dir: the cli-mesh sibling, exec'd, so the
/// wizard's node runs cw-rails through the one bring-up and never a copy
/// (phase-b-81 (1)). `SVRNMESH_DATA_DIR` points the verb at the run data dir,
/// whose config the wizard has not written yet, so it resolves the default
/// `[daemon] rails_base` exactly as [`provisional_config`] does with `None`.
/// `Err` carries the verb's own report.
pub(super) fn bring_up_rails(data_dir: &Path) -> Result<(), String> {
    let Some(bin) = sovereign_turn_client::reach::locate_sibling(
        "sovereign-cli-mesh",
        "SOVEREIGN_CLI_MESH_BIN",
    ) else {
        return Err(
            "cannot find the `sovereign-cli-mesh` binary that brings cw-rails up. Build it \
             with `cargo build -p sovereign-cli-mesh`, or set SOVEREIGN_CLI_MESH_BIN to its path."
                .into(),
        );
    };
    tracing::debug!(bin = %bin.display(), data_dir = %data_dir.display(), "setup join: bringing cw-rails up");
    let out = Command::new(&bin)
        .args(["mesh", "up"])
        .env("SVRNMESH_DATA_DIR", data_dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    let report = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        tracing::debug!(report, "setup join: cw-rails is up");
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    tracing::debug!(status = %out.status, report, "setup join: cw-rails bring-up refused");
    Err(format!(
        "`svrn mesh up` exited {}: {}",
        out.status,
        stderr.trim()
    ))
}

/// The provisional config: the ports and data dir the wizard will record, and
/// the cw-rails base the join goes through (`None` is the default base, the
/// one [`bring_up_rails`] brings up).
fn provisional_config(data_dir: &Path, client_port: u16, rails_base: Option<&str>) -> SetupConfig {
    SetupConfig {
        daemon: sovereign_contracts::setup_config::DaemonSection {
            client_port,
            internal_port: client_port + 1,
            rails_base: rails_base.map(str::to_string),
            ..Default::default()
        },
        data: DataSection {
            dir: data_dir.to_path_buf(),
        },
        ..SetupConfig::unconfigured()
    }
}

/// Spawn `daemon` (the sibling binary, env already set by the caller) as the
/// admin-join launch and wait for its `joined` line or its exit. Returns the
/// running child and the mesh name.
pub(super) async fn join(
    mut daemon: Command,
    data_dir: &Path,
    client_port: u16,
    rails_base: Option<&str>,
    link: &str,
    node_name: &str,
) -> Result<(JoinChild, String), JoinFailure> {
    let provisional = data_dir.join("setup-join.toml");
    provisional_config(data_dir, client_port, rails_base)
        .save_to(&provisional)
        .map_err(JoinFailure::Launch)?;
    let mut child = match daemon
        .args([ADMIN_JOIN_VERB, "--config"])
        .arg(&provisional)
        .args(["--node-name", node_name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            if let Err(rm) = std::fs::remove_file(&provisional) {
                tracing::debug!(error = %rm, "setup join child: provisional config not removed");
            }
            return Err(JoinFailure::Launch(format!(
                "could not start {}: {e}",
                daemon.get_program().to_string_lossy()
            )));
        }
    };
    tracing::debug!(pid = child.id(), config = %provisional.display(), "setup join child: spawned");
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let mut guard = JoinChild {
        child,
        provisional,
        client_port,
    };
    let (Some(mut stdin), Some(stdout)) = (stdin, stdout) else {
        return Err(JoinFailure::Launch(
            "the join child has no stdio pipes".into(),
        ));
    };
    if let Err(e) = writeln!(stdin, "{}", link.trim()) {
        return Err(JoinFailure::Launch(format!(
            "could not hand the invite to the join child: {e}"
        )));
    }
    drop(stdin);

    // Drains stdout to EOF so a later line can never fill the pipe; the first
    // `joined` line is the answer, and EOF without one means the child exited.
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut tx = Some(tx);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(rest) = line.strip_prefix(JOINED_LINE_PREFIX) {
                if let Some(tx) = tx.take() {
                    let name = rest.trim();
                    let name = name
                        .strip_prefix('"')
                        .and_then(|n| n.strip_suffix('"'))
                        .unwrap_or(name);
                    let _ = tx.send(name.to_string());
                }
            }
        }
    });
    match rx.await {
        Ok(mesh_name) => {
            tracing::debug!(pid = guard.id(), mesh_name, "setup join child: joined");
            Ok((guard, mesh_name))
        }
        Err(_) => {
            let status = guard.child.wait();
            tracing::debug!(?status, "setup join child: exited without joining");
            Err(JoinFailure::Refused)
        }
    }
}

#[cfg(test)]
mod tests;
