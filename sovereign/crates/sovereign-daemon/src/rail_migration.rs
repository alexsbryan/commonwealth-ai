// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-time handover of the ring journals to the rails daemon.
//!
//! Until fp-54's flip the inference daemon's in-process rail was the ONLY
//! writer, so moving the journals to `cw-rails`' data root is atomic: this
//! runs in `rails_client::ensure_rails`, before any bring-up and so before any
//! rail surface answers ([`hand_over`], phase-b-3), and after it
//! the daemon never opens a journal again (§4 rule 1 — one data directory,
//! one owner; the daemon cannot write into the rails daemon's store as a
//! standing arrangement, and a one-time rename during a boot it owns is not
//! an arrangement).
//!
//! Both processes spell the layout the same way — `<data_dir>/rings/<ns>/` —
//! because `commonwealth-rail` is the ONE spelling of it (`rings_root`), so
//! a move is a directory rename and nothing inside changes. The target root
//! is `cw-rails`' own default data dir, resolved by the ONE decider both
//! processes call, `commonwealth_media::rails_data_dir` (`$CW_RAILS_DIR`, else
//! `~/.commonwealth-rails`; fp-70 collapsed the mirror this module used to
//! carry).
//!
//! The media presence poll's inputs move the same way (fp-70): the house
//! credential `svrn mesh media offer` kept under this daemon's root, and the
//! viewer id it wrote into `[iroh] media_viewer_user`, both land in rails'
//! house store ([`migrate_media_to_rails`]).
//!
//! Never clobbers: a namespace already present at the target stays there and
//! the source is LEFT in place with a warning, so the worst case of a double
//! history is two copies, never a destroyed one. A namespace moved once is
//! gone from the source, so every later boot is a no-op.

use std::path::{Path, PathBuf};

/// The house credential's file in a house dir, and `[iroh]`'s viewer-id key:
/// one spelling for the handover's check ([`hand_over`]) and its move.
const HOUSE_CREDENTIAL: &str = "authorization";
const VIEWER_KEY: &str = "media_viewer_user";

/// Where the daemon's journals live today — the same layout
/// `commonwealth_rail` spells for both processes.
fn source_root(data_dir: &Path) -> PathBuf {
    data_dir.join("rings")
}

/// Where the rails daemon keeps journals: its default data dir.
fn rails_data_dir() -> PathBuf {
    commonwealth_media::rails_data_dir()
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

/// The handover as `ensure_rails` runs it, before it brings cw-rails up
/// (phase-b-3). cw-rails folds the journals on disk into its store once, at
/// start, so a namespace moved under a live one reads empty for that
/// cw-rails' lifetime. With one already answering nothing moves, journals or
/// media, and what waits is named (principle 6).
pub fn hand_over(data_dir: &Path, config_path: &Path, rails_answering: bool) {
    if !rails_answering {
        migrate_journals_to_rails(data_dir);
        migrate_media_to_rails(data_dir, config_path, &rails_data_dir());
        return;
    }
    let namespaces: Vec<String> = match std::fs::read_dir(source_root(data_dir)) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, dir = %source_root(data_dir).display(), "rail migration: cw-rails already answers, and the daemon's ring directory could not be read to name what waits");
            Vec::new()
        }
    };
    let house_credential = commonwealth_media::house_dir_under(data_dir)
        .join(HOUSE_CREDENTIAL)
        .exists();
    let viewer_id = std::fs::read_to_string(config_path)
        .ok()
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .is_some_and(|d| d.get("iroh").and_then(|i| i.get(VIEWER_KEY)).is_some());
    if namespaces.is_empty() && !house_credential && !viewer_id {
        tracing::debug!(dir = %data_dir.display(), "rail migration: cw-rails already answers and nothing waits to be handed over");
        return;
    }
    tracing::warn!(
        namespaces = ?namespaces,
        house_credential,
        viewer_id,
        dir = %data_dir.display(),
        "rail migration: cw-rails already answers, so nothing moved under it — these wait \
         under the daemon's data dir; cw-rails must restart to take them (stop it, and the \
         next daemon boot or `svrn portfolio` hands them over before bringing it up)"
    );
}

/// Move every ring namespace's journal from the daemon's data dir to the
/// rails daemon's. Idempotent; logs what moved and what could not.
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
                 where they are and the rails daemon will not see them"
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
                     daemon's data dir and is invisible to the rails daemon until the \
                     operator moves it"
                );
                continue;
            }
            if let Err(e) = std::fs::remove_dir_all(&from) {
                // The copy landed: the rails daemon sees the journal. Only
                // the daemon-side original is left behind, and the next boot
                // leaves it alone (the target exists).
                tracing::warn!(
                    error = %e,
                    namespace = %name.to_string_lossy(),
                    from = %from.display(),
                    to = %to.display(),
                    "rail migration: the journal was copied to the rails daemon's root, \
                     but the daemon-side original could not be removed — it is now a stale copy"
                );
                continue;
            }
        }
        tracing::info!(
            namespace = %name.to_string_lossy(),
            to = %to.display(),
            "rail migration: the journal moved to the rails daemon's data root"
        );
    }
}

/// Move the media presence poll's inputs into rails' house store, once: the
/// house credential under `data_dir` (never overwriting one already at the
/// target) and the viewer id in `config_path`'s `[iroh] media_viewer_user`
/// (written to the viewer file, then removed from the config). A node with
/// neither is a debug no-op, so every boot after the first is one.
pub fn migrate_media_to_rails(data_dir: &Path, config_path: &Path, rails_dir: &Path) {
    let target = commonwealth_media::house_dir_under(rails_dir);
    migrate_house_credential(data_dir, &target);
    migrate_viewer_key(config_path, &target);
}

fn migrate_house_credential(data_dir: &Path, target: &Path) {
    let from = commonwealth_media::house_dir_under(data_dir).join(HOUSE_CREDENTIAL);
    let to = target.join(HOUSE_CREDENTIAL);
    let value = match std::fs::read_to_string(&from) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(from = %from.display(), "media migration: no house credential under the daemon's root");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, from = %from.display(), "media migration: the house credential could not be read; it stays and the presence poll will not see it");
            return;
        }
    };
    if to.exists() {
        tracing::warn!(
            from = %from.display(),
            to = %to.display(),
            "media migration: rails already holds a house credential — the daemon-side copy is LEFT in place, nothing was overwritten"
        );
        return;
    }
    if let Err(e) = commonwealth_media::write_declared_in(target, HOUSE_CREDENTIAL, &value) {
        tracing::error!(error = %e, from = %from.display(), to = %to.display(), "media migration: the house credential could not be written to rails' store; it stays where it is");
        return;
    }
    if let Err(e) = std::fs::remove_file(&from) {
        tracing::warn!(error = %e, from = %from.display(), "media migration: the house credential was copied to rails' store, but the daemon-side original could not be removed");
    }
    tracing::info!(from = %from.display(), to = %to.display(), "media migration: the house credential moved to rails' store");
}

fn migrate_viewer_key(config_path: &Path, target: &Path) {
    let text = match std::fs::read_to_string(config_path) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(error = %e, config = %config_path.display(), "media migration: no config to read a viewer id from");
            return;
        }
    };
    let mut doc = match text.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, config = %config_path.display(), "media migration: config.toml did not parse; `[iroh] media_viewer_user` is left alone");
            return;
        }
    };
    let Some(iroh) = doc.get_mut("iroh").and_then(|i| i.as_table_mut()) else {
        tracing::debug!(config = %config_path.display(), "media migration: no [iroh] table, no viewer id to move");
        return;
    };
    let Some(id) = iroh
        .get(VIEWER_KEY)
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        tracing::debug!(config = %config_path.display(), "media migration: no `[iroh] media_viewer_user`");
        return;
    };
    let to = target.join(commonwealth_media::VIEWER_FILE);
    if to.exists() {
        tracing::warn!(to = %to.display(), "media migration: rails already holds a viewer id — kept; the config key is removed");
    } else if let Err(e) = commonwealth_media::write_viewer_in(target, &id) {
        tracing::error!(error = %e, to = %to.display(), "media migration: the viewer id could not be written to rails' store; the config key stays");
        return;
    }
    iroh.remove(VIEWER_KEY);
    if let Err(e) = std::fs::write(config_path, doc.to_string()) {
        tracing::warn!(error = %e, config = %config_path.display(), "media migration: the viewer id is in rails' store, but the config key could not be removed");
        return;
    }
    tracing::info!(from = %config_path.display(), to = %to.display(), "media migration: the viewer id moved from `[iroh] media_viewer_user` to rails' store");
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
        // copy of a ring the rails daemon has since written.
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

    /// fp-70: a node offered before the fix holds the house credential under
    /// the daemon's root and the viewer id in config.toml. Both land in rails'
    /// house store, the key leaves the config (comments kept), and a second
    /// boot is a no-op.
    #[test]
    fn media_inputs_move_to_rails_and_a_second_boot_is_a_no_op() {
        let daemon_dir = tempfile::tempdir().unwrap();
        let rails_dir = tempfile::tempdir().unwrap();
        let old_house = commonwealth_media::house_dir_under(daemon_dir.path());
        commonwealth_media::write_declared_in(&old_house, "authorization", "house-key").unwrap();
        let config = daemon_dir.path().join("config.toml");
        fs::write(
            &config,
            "# mine\n[iroh]\nmedia_origin = \"127.0.0.1:8096\"\nmedia_viewer_user = \"viewer-id-1\"\n",
        )
        .unwrap();

        migrate_media_to_rails(daemon_dir.path(), &config, rails_dir.path());
        let house = commonwealth_media::house_dir_under(rails_dir.path());
        let moved = (
            vec![("authorization".to_string(), "house-key".to_string())],
            Some("viewer-id-1".to_string()),
        );
        assert_eq!(commonwealth_media::read_house_in(&house), moved);
        assert!(!old_house.join("authorization").exists());
        let text = fs::read_to_string(&config).unwrap();
        assert!(!text.contains("media_viewer_user"), "{text}");
        assert!(text.contains("# mine") && text.contains("media_origin"));

        migrate_media_to_rails(daemon_dir.path(), &config, rails_dir.path());
        assert_eq!(commonwealth_media::read_house_in(&house), moved);
        assert_eq!(fs::read_to_string(&config).unwrap(), text);
    }

    /// Never clobbers: a house credential already in rails' store stays, and
    /// the daemon-side one is left in place.
    #[test]
    fn a_house_credential_already_at_rails_is_not_overwritten() {
        let daemon_dir = tempfile::tempdir().unwrap();
        let rails_dir = tempfile::tempdir().unwrap();
        let old_house = commonwealth_media::house_dir_under(daemon_dir.path());
        let house = commonwealth_media::house_dir_under(rails_dir.path());
        commonwealth_media::write_declared_in(&old_house, "authorization", "stale").unwrap();
        commonwealth_media::write_declared_in(&house, "authorization", "current").unwrap();
        migrate_media_to_rails(
            daemon_dir.path(),
            &daemon_dir.path().join("config.toml"),
            rails_dir.path(),
        );
        assert_eq!(
            commonwealth_media::read_house_in(&house).0,
            vec![("authorization".to_string(), "current".to_string())]
        );
        assert!(old_house.join("authorization").exists());
    }
}
