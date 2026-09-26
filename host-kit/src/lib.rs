// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host kit: what each program's binary owns about itself (FIVE_PROGRAMS
//! §2c). Its first content is the single-writer lock on a data root.
//!
//! One writer per data root.
//!
//! # What it guards
//!
//! A sovereign data root is a single-writer store: `sovereign.db`, the mesh
//! identity (`node_id`, `mesh.json`, `join_key.secret`), the per-project SCIP
//! indexes, the pidfile. Two processes writing one root is corruption, a
//! pidfile that names the wrong process, and double model RAM. `flock(2)` is
//! the primitive built for exactly this: the kernel releases it on ANY exit
//! path including `SIGKILL`, so there is no stale-lock cleanup to get wrong.
//! The claim is std's `File::try_lock` — `flock(LOCK_EX | LOCK_NB)` on unix,
//! `LockFileEx` on Windows — so it is enforced on every platform std locks
//! files on, and a platform that cannot lock refuses rather than admits.
//!
//! # Why the key is the data root and not `$HOME`
//!
//! Until 2026-08-24 the lock lived at `$HOME/.svrnmesh/daemon.lock`,
//! derived from `rebrand::svrnmesh_root()` — which does **not** honour the
//! `SVRNMESH_DATA_DIR` / `SOVEREIGN_DATA_DIR` override and cannot see a
//! `--config` that names a different `[data] dir`. That key was wrong in both
//! directions, and both cost real time:
//!
//! * **False refusal.** `scripts/mesh-soak.sh` runs three nodes with three
//!   config files and three data dirs under one `$HOME`. Node 0 took the
//!   lock and every other node exited — and the surviving one-node mesh
//!   still passed the invariant pack, because convergence and pairwise
//!   liveness are trivially true over a single node. A 3-node soak was
//!   really a 1-node soak reporting green.
//! * **False admission.** Two processes pointed at one data root from
//!   different `$HOME`s (a fake-HOME harness, a service account, a container
//!   bind-mount) each took their own lock and both ran — which is the
//!   corruption the lock exists to prevent.
//!
//! The escape hatch that existed to paper over the first case
//! (`SOVEREIGN_ALLOW_MULTIPLE_DAEMONS=1`) is deleted with this change. Under
//! the corrected key it can only ever defeat a *correct* refusal.
//!
//! Identity comes from the inode, not the string (ARCH_PRINCIPLES §7.5):
//! `~/.sovereign` is a symlink to `~/.svrnmesh` on migrated machines, and
//! `flock` on a path that resolves through it lands on the same open file
//! description. Two spellings of one directory cannot get two locks.
//!
//! # Why it is not keyed on `sovereign_contracts::launch::Launch`
//!
//! Residency and data-root ownership are different questions, and the closed
//! set answers only the first. `Launch::Worker` binds a listener and owns no
//! persistent state at all (an ephemeral pod boots from a bootstrap blob and
//! exits), so it has nothing to lock. `Launch::Desktop` owns a data root in
//! Local mode and none in Attach mode — the same variant, two answers,
//! decided by a probe long after argv is parsed. And `Launch` deliberately
//! carries no configuration, so it could not name *which* root even where it
//! knows there is one.
//!
//! So the caller names the root it is about to write, and the lock is the
//! seam. The caller also names the lock FILE (the daemon's `daemon.lock`,
//! cw-rails' `rails.lock`), because this kit names no program's vocabulary;
//! the operator hint a refusal prints is the caller's to add for the same
//! reason.

#[cfg(feature = "mcp")]
pub mod mcp;

use std::path::{Path, PathBuf};

/// An exclusive claim on one data root, held for as long as the value lives.
///
/// Dropping it releases the lock, so the holder must keep it alive for the
/// process's lifetime — bind it to a `_run_lock` local in `main`, or park it
/// in the state root. It is deliberately not `Clone`: two owners of one claim
/// is the confusion this type exists to remove.
#[derive(Debug)]
pub struct RunLock {
    /// The lock file this claim is on, inside the data root it names.
    path: PathBuf,
    /// Held open because the lock lives on the open file description;
    /// closing it is what releases the claim. Read only for its metadata.
    file: std::fs::File,
}

/// Why a data root could not be claimed. Both arms name the path, because an
/// operator's first question is always "which lock?" (ARCH_PRINCIPLES §18.3 —
/// a refusal is reported, never collapsed into a success-shaped value).
#[derive(Debug)]
pub enum RunLockError {
    /// Another live process holds this root. The expected, correct refusal.
    Held {
        /// The lock file the other process holds.
        path: PathBuf,
    },
    /// The lock file itself could not be opened — a read-only mount, a
    /// permission problem, a data root that is not a directory. Distinct from
    /// `Held` because the operator action is completely different.
    Unopenable {
        /// The lock file that could not be opened.
        path: PathBuf,
        /// The underlying `open(2)` failure.
        source: std::io::Error,
    },
    /// The lock file opened but the lock call itself failed for a reason
    /// other than another holder — a filesystem without locks, a platform std
    /// cannot lock on. Never folded into `Held`: nobody holds it.
    Unlockable {
        /// The lock file that could not be locked.
        path: PathBuf,
        /// The underlying lock failure.
        source: std::io::Error,
    },
}

impl RunLockError {
    /// The lock path this refusal is about.
    pub fn path(&self) -> &Path {
        match self {
            Self::Held { path } | Self::Unopenable { path, .. } | Self::Unlockable { path, .. } => {
                path
            }
        }
    }
}

impl std::fmt::Display for RunLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held { path } => write!(
                f,
                "another process already holds the run lock on this data root \
                 ({}) — refusing to run a second writer.",
                path.display()
            ),
            Self::Unopenable { path, source } => {
                write!(f, "cannot open run lock {}: {source}", path.display())
            }
            Self::Unlockable { path, source } => {
                write!(f, "cannot lock run lock {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for RunLockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Held { .. } => None,
            Self::Unopenable { source, .. } | Self::Unlockable { source, .. } => Some(source),
        }
    }
}

impl RunLock {
    /// Where the lock named `name` for `data_root` lives. Public so a
    /// harness that must wait for a daemon to *actually exit* can watch the
    /// same file the daemon holds — the listener dies tens of seconds before
    /// the process does when an 18GB model is unloading, so waiting on the
    /// port races.
    pub fn path_for(data_root: &Path, name: &str) -> PathBuf {
        data_root.join(name)
    }

    /// Claim `data_root` for this process, on the lock file `name` inside it.
    ///
    /// Call it before anything heavy: a refused second instance must not have
    /// already loaded models. Creates the data root if it does not exist —
    /// the first daemon on a fresh machine locks before it writes.
    pub fn acquire(data_root: &Path, name: &str) -> Result<Self, RunLockError> {
        let path = Self::path_for(data_root, name);
        // Best-effort: a root we cannot create surfaces as `Unopenable`
        // below, with the real errno, rather than as a second error here.
        let _ = std::fs::create_dir_all(data_root);
        Self::acquire_at(path)
    }

    fn acquire_at(path: PathBuf) -> Result<Self, RunLockError> {
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(source) => return Err(RunLockError::Unopenable { path, source }),
        };
        // Non-blocking: a live holder is `WouldBlock`, never a wait.
        match file.try_lock() {
            Ok(()) => Ok(Self { path, file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(RunLockError::Held { path }),
            Err(std::fs::TryLockError::Error(source)) => {
                Err(RunLockError::Unlockable { path, source })
            }
        }
    }

    /// Whether this claim is enforced. Always `true`: since the re-base on
    /// std `File::try_lock` a claim exists only as a held lock, and a
    /// platform that cannot lock refuses with `Unlockable` instead of
    /// granting an empty claim. Until then this was `cfg!(unix)` and Windows
    /// got a claim that guaranteed nothing. Kept because callers log it at
    /// startup, and a guard they report must be one they have
    /// (ARCH_PRINCIPLES §18.3).
    pub const fn is_enforced(&self) -> bool {
        true
    }

    /// The lock file this claim is on.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Metadata of the file this claim holds open — not of whatever sits at
    /// [`Self::path`] now. A holder that must notice its root being deleted
    /// or replaced compares the two (cw-rails' `root_lost`).
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon's name for its lock; any name a caller passes behaves alike.
    const LOCK: &str = "daemon.lock";

    /// The second `daemon run` on one data root. The `Held` arm is the whole
    /// point of the type, and the re-acquire proves the kernel released it on
    /// drop — which is what makes `SIGKILL` safe and stale-lock cleanup
    /// unnecessary.
    #[test]
    fn a_second_claim_on_one_root_is_refused_until_the_first_drops() {
        let root = tempfile::tempdir().expect("tempdir");
        let first = RunLock::acquire(root.path(), LOCK).expect("first claim succeeds");

        let refusal =
            RunLock::acquire(root.path(), LOCK).expect_err("second claim must be refused");
        assert!(matches!(refusal, RunLockError::Held { .. }));
        assert_eq!(refusal.path(), first.path());

        drop(first);
        RunLock::acquire(root.path(), LOCK).expect("re-acquirable once the holder drops");
    }

    /// The false refusal that made a 3-node soak a 1-node soak: two data
    /// roots under one `$HOME` are two locks, and both must be grantable.
    /// This is the assertion the old `$HOME`-derived key failed.
    #[test]
    fn two_data_roots_under_one_home_do_not_collide() {
        let home = tempfile::tempdir().expect("tempdir");
        let node0 = home.path().join("node0");
        let node1 = home.path().join("node1");
        let _a = RunLock::acquire(&node0, LOCK).expect("node0 claims its own root");
        let _b = RunLock::acquire(&node1, LOCK).expect("node1 must not be refused node0's lock");
    }

    /// The false admission: identity is the inode, so a symlinked spelling of
    /// one root cannot be claimed twice. `~/.sovereign` → `~/.svrnmesh` is
    /// exactly this shape on a migrated machine.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_spelling_of_one_root_is_the_same_claim() {
        let base = tempfile::tempdir().expect("tempdir");
        let real = base.path().join("svrnmesh");
        std::fs::create_dir_all(&real).expect("create real root");
        let alias = base.path().join("sovereign");
        std::os::unix::fs::symlink(&real, &alias).expect("symlink");

        let _held = RunLock::acquire(&real, LOCK).expect("claim the real root");
        assert!(
            matches!(
                RunLock::acquire(&alias, LOCK),
                Err(RunLockError::Held { .. })
            ),
            "the alias resolves to the same inode and must be refused"
        );
    }

    #[test]
    fn the_lock_lives_inside_the_root_it_names() {
        let p = RunLock::path_for(Path::new("/srv/data"), LOCK);
        assert_eq!(p, Path::new("/srv/data/daemon.lock"));
    }

    /// A data root that cannot hold a file is `Unopenable`, never `Held` —
    /// the operator action for the two is completely different.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_root_refuses_with_the_errno_not_as_a_collision() {
        let base = tempfile::tempdir().expect("tempdir");
        // A *file* where a data root should be: `create_dir_all` fails and
        // the open lands on ENOTDIR.
        let not_a_dir = base.path().join("root");
        std::fs::write(&not_a_dir, b"").expect("write");
        let err = RunLock::acquire(&not_a_dir, LOCK).expect_err("must refuse");
        assert!(matches!(err, RunLockError::Unopenable { .. }), "{err}");
    }
}
