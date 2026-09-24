// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-time handover of the ring journals to the serving process.
//!
//! Until fp-54's flip the inference daemon's in-process rail was the ONLY
//! writer, so moving the journals to `cw-rails`' data root is atomic: this
//! runs once at daemon start, before any rail surface answers, and after it
//! the daemon never opens a journal again (§4 rule 1 — one data directory,
//! one owner; the daemon cannot write into the serving process's store as a
//! standing arrangement, and a one-time rename during a boot it owns is not
//! an arrangement).
//!
//! Both processes spell the layout the same way — `<data_dir>/rings/<ns>/` —
//! because `commonwealth-rail` is the ONE spelling of it (`rings_root`), so
//! a move is a directory rename and nothing inside changes. The target root
//! mirrors `cw-rails`' own data-dir resolution (`$CW_RAILS_DIR`, else
//! `~/.commonwealth-rails`), the same mirrored-convention move
//! `rails_client::DEFAULT_RAILS_BASE` makes for the port: the two programs
//! are built separately, so the convention is documented on both sides
//! rather than imported across the lift boundary.
//!
//! Never clobbers: a namespace already present at the target stays there and
//! the source is LEFT in place with a warning, so the worst case of a double
//! history is two copies, never a destroyed one. A namespace moved once is
//! gone from the source, so every later boot is a no-op.

use std::path::{Path, PathBuf};

/// Where the daemon's journals live today — the same layout
/// `commonwealth_rail` spells for both processes.
fn source_root(data_dir: &Path) -> PathBuf {
    data_dir.join("rings")
}

/// Where the serving process keeps journals, resolved the way `cw-rails`
/// resolves its data dir: the env var, else `~/.commonwealth-rails`.
fn rails_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CW_RAILS_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".commonwealth-rails")
}

/// Copy a directory tree. `std::fs::rename` is the path this module expects
/// to take; this is only the cross-device fallback, and journal directories
/// are small (a seal prunes them), so a plain recursion is enough.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Move every ring namespace's journal from the daemon's data dir to the
/// serving process's. Idempotent; logs what moved and what could not.
pub fn migrate_journals_to_rails(data_dir: &Path) {
    let source = source_root(data_dir);
    let entries = match std::fs::read_dir(&source) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(
                error = %e,
                dir = %source.display(),
                "rail migration: the daemon's ring directory could not be read; journals stay \
                 where they are and the serving process will not see them"
            );
            return;
        }
    };

    let target_root = rails_data_dir().join("rings");
    for entry in entries.flatten() {
        let from = entry.path();
        if !from.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let to = target_root.join(&name);
        if to.exists() {
            tracing::warn!(
                namespace = %name.to_string_lossy(),
                from = %from.display(),
                to = %to.display(),
                "rail migration: a journal with this name already exists at the serving \
                 process's root — the source copy is LEFT in place, nothing was overwritten"
            );
            continue;
        }
        if let Err(rename_err) = std::fs::rename(&from, &to) {
            // Different filesystems rename-refuse; fall back to copy+delete
            // rather than leaving the journal unseen.
            if let Err(e) = copy_dir(&from, &to) {
                tracing::error!(
                    error = %e,
                    rename_error = %rename_err,
                    namespace = %name.to_string_lossy(),
                    from = %from.display(),
                    to = %to.display(),
                    "rail migration: this journal could not be moved — it stays under the \
                     daemon's data dir and is invisible to the serving process until the \
                     operator moves it"
                );
                continue;
            }
            if let Err(e) = std::fs::remove_dir_all(&from) {
                // The copy landed: the serving process sees the journal. Only
                // the daemon-side original is left behind, and the next boot
                // leaves it alone (the target exists).
                tracing::warn!(
                    error = %e,
                    namespace = %name.to_string_lossy(),
                    from = %from.display(),
                    to = %to.display(),
                    "rail migration: the journal was copied to the serving process's root, \
                     but the daemon-side original could not be removed — it is now a stale copy"
                );
                continue;
            }
        }
        tracing::info!(
            namespace = %name.to_string_lossy(),
            to = %to.display(),
            "rail migration: the journal moved to the serving process's data root"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The handover, watched end to end: a journal directory moves whole
    /// (oplog AND its roster file), the source root is empty afterwards so a
    /// second boot is a no-op, and a namespace already at the target is
    /// NEVER overwritten — the worst case is two copies, not a destroyed one.
    #[test]
    fn a_journal_moves_whole_a_second_boot_is_a_no_op_and_nothing_is_clobbered() {
        let daemon_dir = tempfile::tempdir().unwrap();
        let rails_dir = tempfile::tempdir().unwrap();
        let ns = daemon_dir.path().join("rings").join("house-expenses");
        fs::create_dir_all(&ns).unwrap();
        fs::write(ns.join("oplog.jsonl"), "{\"seq\":0}").unwrap();
        fs::write(ns.join("roster.json"), "{\"members\":{}}").unwrap();
        // A second namespace already at the target: the daemon holds a stale
        // copy of a ring the serving process has since written.
        fs::create_dir_all(rails_dir.path().join("rings").join("work")).unwrap();
        fs::write(
            rails_dir.path().join("rings/work/oplog.jsonl"),
            "{\"seq\":9}",
        )
        .unwrap();
        fs::create_dir_all(daemon_dir.path().join("rings/work")).unwrap();

        // `CW_RAILS_DIR` is the resolution this module mirrors; scope the env
        // so the test is hermetic whatever the host shell carries.
        // SAFETY-hygiene: tests run single-threaded per process here.
        std::env::set_var("CW_RAILS_DIR", rails_dir.path());
        migrate_journals_to_rails(daemon_dir.path());

        // Moved whole, roster file included.
        let moved = rails_dir.path().join("rings/house-expenses");
        assert_eq!(
            fs::read_to_string(moved.join("oplog.jsonl")).unwrap(),
            "{\"seq\":0}"
        );
        assert!(moved.join("roster.json").exists(), "the roster file moved");
        assert!(
            !daemon_dir.path().join("rings/house-expenses").exists(),
            "the source is gone, so the next boot is a no-op"
        );

        // Never clobbered: the target's line survives the daemon's absence.
        assert_eq!(
            fs::read_to_string(rails_dir.path().join("rings/work/oplog.jsonl")).unwrap(),
            "{\"seq\":9}"
        );

        // The restart: everything is already at the target, nothing changes.
        migrate_journals_to_rails(daemon_dir.path());
        assert_eq!(
            fs::read_to_string(rails_dir.path().join("rings/work/oplog.jsonl")).unwrap(),
            "{\"seq\":9}"
        );
    }
}
