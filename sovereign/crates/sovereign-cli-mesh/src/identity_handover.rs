// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node's identity moves to cw-rails (pb-mesh-exit-transport, phase-b-11):
//! the daemon's `<data_dir>/node_key` becomes cw-rails' key, its `node_id` is
//! copied (svrn-side readers keep theirs, `sovereign_contracts::
//! node_identity`), and every mesh the daemon holds — the active one and the
//! parked ones, each with its invite key — lands in cw-rails' store through
//! cw-rails' own writers (`commonwealth_rails::identity`, `::known`).
//!
//! Runs where the journal handover runs (`rails_up::ensure_rails`), before
//! any bring-up, and only when no cw-rails answers: a live cw-rails holds its
//! key in memory and would keep signing with the old one. Nothing is deleted.
//! cw-rails' files are kept beside as `*.pre-handover`, and the daemon's key
//! is renamed `node_key.handed-over`, which is also what makes a second run
//! a no-op.

use std::path::Path;

use commonwealth_rails::identity;
use commonwealth_transport::identity::NODE_KEY_FILE;
use tracing::{info, warn};

const TARGET: &str = "rail_migration";

/// The daemon's key after the handover: kept, never read again.
pub const HANDED_OVER_KEY: &str = "node_key.handed-over";

/// What one handover did.
#[derive(Debug, PartialEq, Eq)]
pub enum Handover {
    /// The daemon holds no key: nothing to hand over (a fresh node, or one
    /// already handed over).
    NothingToMove,
    /// cw-rails answered, so nothing moved; the daemon's key waits.
    Deferred,
    /// Key, node id and this many meshes (active first) moved.
    Moved { meshes: usize },
}

/// Hand the daemon's identity in `svrn_dir` to cw-rails' store in
/// `rails_dir`. `rails_answering` is the probe `ensure_rails` already ran.
pub fn hand_over(
    svrn_dir: &Path,
    rails_dir: &Path,
    rails_answering: bool,
) -> Result<Handover, String> {
    use sovereign_contracts::node_identity::{mesh_handover_pending, PRE_HANDOVER_KEY_FILE};
    let svrn_key = svrn_dir.join(PRE_HANDOVER_KEY_FILE);
    if !mesh_handover_pending(svrn_dir) {
        return Ok(Handover::NothingToMove);
    }
    if rails_answering {
        warn!(target: TARGET, key = %svrn_key.display(),
              "identity handover: cw-rails is running, so the daemon's key was not handed over; \
               stop cw-rails and run `svrn mesh up` again");
        return Ok(Handover::Deferred);
    }
    std::fs::create_dir_all(rails_dir)
        .map_err(|e| format!("{} could not be created: {e}", rails_dir.display()))?;

    // Meshes first, read in full before anything is written: a daemon store
    // that does not read moves nothing, key included.
    let root = svrn_dir;
    let active = crate::daemon_store::load(root).map_err(|e| {
        format!(
            "the daemon's active mesh in {} does not read: {e}",
            root.display()
        )
    })?;
    let active_id = active.as_ref().map(|m| m.mesh_id);
    let active_key = crate::daemon_store::load_join_key(root)
        .map_err(|e| format!("the daemon's invite key does not read: {e}"))?;
    let parked: Vec<_> = crate::daemon_store::list_known(root)
        .into_iter()
        .filter(|m| Some(m.mesh_id) != active_id)
        .collect();

    keep_aside(&rails_dir.join(NODE_KEY_FILE))?;
    copy_private(&svrn_key, &rails_dir.join(NODE_KEY_FILE))?;
    match sovereign_contracts::node_identity::load_node_id(root)
        .map_err(|e| format!("the daemon's node_id does not read: {e}"))?
    {
        Some(id) => {
            keep_aside(&identity::node_id_file(rails_dir))?;
            identity::save_node_id(rails_dir, &id).map_err(|e| e.to_string())?;
        }
        None => {
            warn!(target: TARGET, "identity handover: the daemon holds a key but no node_id; cw-rails keeps its own")
        }
    }

    let mut meshes = 0;
    if let Some(p) = active {
        let (mesh, _) = p.into_live();
        keep_aside(&identity::mesh_file(rails_dir))?;
        keep_aside(&identity::join_key_file(rails_dir))?;
        identity::save_mesh(rails_dir, &mesh).map_err(|e| e.to_string())?;
        match &active_key {
            Some(k) => identity::save_join_key(rails_dir, k.trim()).map_err(|e| e.to_string())?,
            None => identity::clear_join_key(rails_dir).map_err(|e| e.to_string())?,
        }
        info!(target: TARGET, mesh = %mesh.name, members = mesh.members.len(),
              "identity handover: the daemon's active mesh is cw-rails' mesh");
        meshes += 1;
    }
    for p in parked {
        let key_file = crate::daemon_store::mesh_dir(root, &p.mesh_id)
            .join(crate::daemon_store::JOIN_KEY_FILE);
        let key = std::fs::read_to_string(&key_file).ok();
        let (mesh, _) = p.into_live();
        commonwealth_rails::known::park(rails_dir, &mesh, key.as_deref().map(str::trim))
            .map_err(|e| e.to_string())?;
        meshes += 1;
    }

    // A `node_key.handed-over` already there is the first key handed over: it
    // is kept, and this key, now copied to cw-rails, is removed instead.
    let retired = svrn_dir.join(HANDED_OVER_KEY);
    if retired.exists() {
        std::fs::remove_file(&svrn_key)
    } else {
        std::fs::rename(&svrn_key, &retired)
    }
    .map_err(|e| format!("the daemon's key was copied but could not be retired: {e}"))?;
    info!(target: TARGET, meshes, rails = %rails_dir.display(), retired = %retired.display(),
          "identity handover: the daemon's key, node id and meshes are cw-rails'");
    Ok(Handover::Moved { meshes })
}

/// Rename `path` to `<path>.pre-handover`, once (pb-distribution-f10): an
/// existing aside is the first original and is kept, and the caller then
/// overwrites `path`. A `path` that does not exist leaves an empty aside, the
/// marker the rollback reads as "there was no file" (RUNBOOK §9).
fn keep_aside(path: &Path) -> Result<(), String> {
    let mut aside = path.as_os_str().to_owned();
    aside.push(".pre-handover");
    let aside = std::path::PathBuf::from(aside);
    if aside.exists() {
        info!(target: TARGET, aside = %aside.display(), "identity handover: an older copy is kept aside; not overwritten");
        return Ok(());
    }
    let kept = if path.exists() {
        std::fs::rename(path, &aside)
    } else {
        std::fs::write(&aside, b"")
    };
    kept.map_err(|e| format!("{} could not be kept aside: {e}", path.display()))?;
    info!(target: TARGET, from = %path.display(), aside = %aside.display(), "identity handover: kept aside");
    Ok(())
}

/// Copy `from` to `to`, owner-only: a node key is a secret.
fn copy_private(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to).map_err(|e| format!("{} → {}: {e}", from.display(), to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "identity_handover_tests.rs"]
pub(crate) mod tests;
