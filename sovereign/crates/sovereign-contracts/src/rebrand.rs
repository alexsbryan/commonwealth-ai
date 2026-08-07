// SPDX-License-Identifier: AGPL-3.0-or-later
//! Back-compat layer for the `sovereign` -> `svrnmesh` rebrand.
//!
//! The product, CLI command, on-disk data directory, environment-variable
//! prefix, the state-DB filename, and the service identifiers were renamed
//! from `sovereign` to `svrnmesh`. To keep existing installations working
//! across the rename we ship two non-destructive bridges plus a one-time
//! migrator. Together they make every later string rename non-load-bearing:
//! a site we miss still resolves correctly.
//!
//!   * **Path resolution falls back to the legacy name.** The data-dir
//!     getters ([`svrnmesh_root`], [`mesh_data_dir`], and the duplicate in
//!     `setup_config::default_data_dir`) prefer the rebranded dir but
//!     transparently use a *populated* legacy dir when the new one doesn't
//!     exist yet, so correctness never depends on migration having run.
//!
//!   * **Env vars are mirrored both ways.** [`promote_legacy_env`] copies any
//!     `SOVEREIGN_*` var to the matching `SVRNMESH_*` (and vice-versa) when the
//!     target is unset, so old scripts and not-yet-converted read sites both
//!     keep working. [`svrnmesh_env`] is the read-side complement for vars set
//!     after `main()`.
//!
//!   * **A one-time migrator** ([`run_startup_migration`]) atomically renames
//!     the legacy data dirs to the rebranded ones, leaving a transitional
//!     back-compat symlink. It is idempotent and never copies data.
//!
//! All of this is transitional: once the ecosystem has moved, the legacy
//! fallbacks, the symlink, and the env mirror are dropped (see the rename
//! plan, "Later release"). That end state is now COMPUTABLE rather than
//! aspirational: `cargo run -p xtask -- env-gate` censuses every env-var
//! read site in the workspace (canonicalizing both prefixes), so
//! [`promote_legacy_env`] can be deleted the day the census shows zero
//! sites reading the legacy `SOVEREIGN_` prefix only.

use std::ffi::OsString;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Legacy brand token (pre-rebrand) as it appears in data dirs, the state-DB
/// filename, and the env-var prefix.
const LEGACY: &str = "sovereign";
/// Current brand token.
const BRAND: &str = "svrnmesh";
/// Default API port; used only as a heuristic for "is a daemon live?".
const DEFAULT_API_PORT: u16 = 9741;

// ─── Env-var bridge ────────────────────────────────────────────────

/// Read an env var by suffix, preferring the `SVRNMESH_` prefix and falling
/// back to the legacy `SOVEREIGN_` prefix. Use for canonical reads in library
/// code and for any var that is `set_var`'d at runtime *after* `main()` (which
/// the startup mirror in [`promote_legacy_env`] cannot see).
pub fn svrnmesh_env(suffix: &str) -> Option<OsString> {
    std::env::var_os(format!("SVRNMESH_{suffix}"))
        .or_else(|| std::env::var_os(format!("SOVEREIGN_{suffix}")))
}

/// Mirror the legacy/new env-var prefixes in both directions so that neither
/// old scripts (setting `SOVEREIGN_*`) nor not-yet-converted read sites (still
/// reading `SOVEREIGN_*`, or already reading `SVRNMESH_*`) break during the
/// transition.
///
/// MUST be called from each binary's `main()` *before* the async runtime is
/// built — mutating the process environment is only sound single-threaded.
/// Idempotent: a var already present under the target prefix is never
/// overwritten, so re-running (e.g. the dispatcher exec'ing a sibling that
/// re-runs the shim) is a no-op.
pub fn promote_legacy_env() {
    // Snapshot first: we mutate the environment inside the loop, and iterating
    // `vars()` while calling `set_var` would otherwise be unsound.
    let snapshot: Vec<(String, String)> = std::env::vars().collect();
    let mut promoted = 0usize;
    for (key, val) in &snapshot {
        if let Some(suffix) = key.strip_prefix("SOVEREIGN_") {
            let new_key = format!("SVRNMESH_{suffix}");
            if std::env::var_os(&new_key).is_none() {
                std::env::set_var(&new_key, val);
                promoted += 1;
            }
        } else if let Some(suffix) = key.strip_prefix("SVRNMESH_") {
            let old_key = format!("SOVEREIGN_{suffix}");
            if std::env::var_os(&old_key).is_none() {
                std::env::set_var(&old_key, val);
            }
        }
    }
    if promoted > 0 {
        eprintln!(
            "svrnmesh: bridged {promoted} legacy SOVEREIGN_* env var(s) to SVRNMESH_* \
             (the SOVEREIGN_* prefix is deprecated — update your scripts)"
        );
    }
}

// ─── Path resolution (new-preferred, legacy-fallback) ──────────────

/// The per-user data root, preferring `~/.svrnmesh` and falling back to a
/// populated legacy `~/.sovereign`. Falls back to `.` if home is unknown
/// (matching the prior `default_data_dir` behaviour).
// The ONE place the home-dir → branded-root derivation is allowed to live
// (clippy.toml bans `dirs::home_dir` everywhere else for sovereign paths).
#[allow(clippy::disallowed_methods)]
pub fn svrnmesh_root() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => resolve_branded_dir(
            home.join(format!(".{BRAND}")),
            home.join(format!(".{LEGACY}")),
        ),
        None => PathBuf::from("."),
    }
}

/// Platform-native data dir for the embedded mesh's shared storage
/// (`dirs::data_dir()/svrnmesh`, e.g. `~/Library/Application Support/svrnmesh`
/// on macOS), with the same legacy fallback as [`svrnmesh_root`].
pub fn mesh_data_dir() -> PathBuf {
    match dirs::data_dir() {
        Some(data) => resolve_branded_dir(data.join(BRAND), data.join(LEGACY)),
        None => PathBuf::from("."),
    }
}

/// Prefer the rebranded dir when it actually holds data; fall back to a
/// populated legacy dir; otherwise (fresh install) use the rebranded dir.
/// Guarding on "populated" means an aborted prior migration that left an empty
/// `~/.svrnmesh` never shadows a populated `~/.sovereign`.
fn resolve_branded_dir(new: PathBuf, legacy: PathBuf) -> PathBuf {
    if dir_is_populated(&new) {
        new
    } else if legacy.exists() {
        legacy
    } else {
        new
    }
}

fn dir_is_populated(p: &Path) -> bool {
    std::fs::read_dir(p)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

// ─── Canonical per-user paths (SSOT accessors) ─────────────────────

/// The per-user data root, honoring the `SVRNMESH_DATA_DIR` /
/// `SOVEREIGN_DATA_DIR` override and falling back to [`svrnmesh_root`].
/// This is THE derivation for "where does per-user state live" — read
/// sites must not re-derive it. Behavior note (2026-07-30): the previous
/// hand-rolled chains fell back to a bare relative `.sovereign` when both
/// the override and HOME were unset, silently writing into the process
/// CWD; this accessor never returns a relative path on that edge (it
/// returns [`svrnmesh_root`]'s `.` only when home resolution itself
/// fails, matching every other getter here).
pub fn data_dir() -> PathBuf {
    match svrnmesh_env("DATA_DIR") {
        Some(v) => PathBuf::from(v),
        None => svrnmesh_root(),
    }
}

/// The project registry written by `sovereign project register`
/// (`sovereign-mesh/src/projects.rs`) and read by the daemon's startup
/// re-registration and code-tool path resolution. Deliberately rooted at
/// [`svrnmesh_root`] (home-based), NOT [`data_dir`] — the writer has
/// never honored the `DATA_DIR` override, and reader/writer must agree.
pub fn projects_json() -> PathBuf {
    svrnmesh_root().join("projects.json")
}

/// `<root>/work-atlas.toml` — per-node work-atlas config, read by the
/// daemon bootstrap, `svrn project serve`, and `sovereign claim`.
pub fn work_atlas_toml() -> PathBuf {
    svrnmesh_root().join("work-atlas.toml")
}

/// `<root>/drift/` — persisted drift reports (the `latest.md.json`
/// mirror `drift_posture`/`drift_findings`/`briefing` read).
pub fn drift_dir() -> PathBuf {
    svrnmesh_root().join("drift")
}

// ─── State-DB filename migration ───────────────────────────────────

/// Resolve the state-store DB path inside `data_dir`, preferring the
/// `svrnmesh.db` name and renaming a legacy `sovereign.db` (with its SQLite
/// `-wal`/`-shm`/`-journal` sidecars, as a set) on first access. The rename
/// only runs when no daemon is live on the API port — the sidecars must move
/// while the DB is closed, so moving them out from under a live daemon would
/// corrupt it. The daemon performs this rename itself at startup before it
/// binds the port; other processes defer to whatever name is currently live.
pub fn state_db_path(data_dir: &Path) -> PathBuf {
    let new_db = data_dir.join(format!("{BRAND}.db"));
    if new_db.exists() {
        return new_db;
    }
    let legacy_db = data_dir.join(format!("{LEGACY}.db"));
    if !legacy_db.exists() {
        return new_db; // fresh install
    }
    if daemon_is_live() {
        return legacy_db; // keep the name the live daemon already opened
    }
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let from = append_suffix(&legacy_db, suffix);
        if from.exists() {
            let to = append_suffix(&new_db, suffix);
            if let Err(e) = std::fs::rename(&from, &to) {
                eprintln!(
                    "svrnmesh: rename {} -> {} failed: {e}; keeping legacy DB name",
                    from.display(),
                    to.display()
                );
                return legacy_db;
            }
        }
    }
    eprintln!(
        "svrnmesh: migrated state DB {LEGACY}.db -> {BRAND}.db in {}",
        data_dir.display()
    );
    new_db
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return path.to_path_buf();
    }
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

// ─── One-time directory migration ──────────────────────────────────

/// One-time, idempotent migration of the on-disk `sovereign` data dirs to the
/// `svrnmesh` brand. Safe to call from every binary's `main()`: the directory
/// moves are atomic `rename(2)`s guarded by existence checks (so re-runs and
/// fresh installs are no-ops), and a transitional symlink bridges any
/// not-yet-converted hard-coded `~/.sovereign` path. The state-DB *filename*
/// rename is handled lazily and gated separately in [`state_db_path`].
#[allow(clippy::disallowed_methods)] // SSOT crate: migrates the raw home-dir layout itself
pub fn run_startup_migration() {
    // Defer to a running daemon: it migrates the data dirs itself at startup
    // (before it binds the API port), and renaming a dir out from under
    // arbitrary live processes is best avoided. CLI invocations simply rely on
    // the legacy-fallback getters until the next clean daemon start.
    if daemon_is_live() {
        return;
    }
    if let Some(home) = dirs::home_dir() {
        let legacy_root = home.join(format!(".{LEGACY}"));
        let new_root = home.join(format!(".{BRAND}"));
        if migrate_dir_between(&legacy_root, &new_root) {
            ensure_back_compat_symlink(&legacy_root, &new_root);
        }
    }
    if let Some(data) = dirs::data_dir() {
        let legacy_data = data.join(LEGACY);
        let new_data = data.join(BRAND);
        if migrate_dir_between(&legacy_data, &new_data) {
            ensure_back_compat_symlink(&legacy_data, &new_data);
        }
    }
}

/// Atomically move `legacy` -> `new_path` iff `new_path` doesn't already exist
/// and `legacy` does. A same-filesystem `rename(2)` moves arbitrarily large
/// trees in O(1) with no copy, which matters because the data dir can hold many
/// GB of models and indexes. Never copies across filesystems — on `EXDEV` it
/// logs and leaves the legacy dir in place so the fallback getters keep
/// working. Returns true iff a move happened.
fn migrate_dir_between(legacy: &Path, new_path: &Path) -> bool {
    if new_path.exists() || !legacy.exists() || legacy == new_path {
        return false;
    }
    if let Some(parent) = new_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "svrnmesh: migrate {}: create {}: {e}",
                legacy.display(),
                parent.display()
            );
            return false;
        }
    }
    match std::fs::rename(legacy, new_path) {
        Ok(()) => {
            eprintln!(
                "svrnmesh: migrated {} -> {}",
                legacy.display(),
                new_path.display()
            );
            true
        }
        Err(e) => {
            eprintln!(
                "svrnmesh: could not move {} -> {} ({e}); leaving data in place \
                 (if they are on different volumes, move it by hand)",
                legacy.display(),
                new_path.display()
            );
            false
        }
    }
}

/// Best-effort transitional symlink so any not-yet-converted hard-coded legacy
/// path still resolves after migration. Dropped in a later release once all
/// sites resolve through the getters.
#[cfg(unix)]
fn ensure_back_compat_symlink(legacy: &Path, new_path: &Path) {
    if legacy.exists() || !new_path.exists() {
        return; // legacy still present (not migrated), or nothing to point at
    }
    if let Err(e) = std::os::unix::fs::symlink(new_path, legacy) {
        eprintln!(
            "svrnmesh: back-compat symlink {} -> {} failed: {e}",
            legacy.display(),
            new_path.display()
        );
    }
}

#[cfg(not(unix))]
fn ensure_back_compat_symlink(_legacy: &Path, _new_path: &Path) {}

// ─── Daemon liveness heuristic ─────────────────────────────────────

/// Conservative "is a daemon already serving?" probe: a short TCP connect to
/// the API port. Used only to gate the state-DB filename rename; the directory
/// migration is safe regardless. Honors a `PORT` override via [`svrnmesh_env`].
fn daemon_is_live() -> bool {
    let port = svrnmesh_env("PORT")
        .and_then(|v| v.to_str().and_then(|s| s.parse::<u16>().ok()))
        .unwrap_or(DEFAULT_API_PORT);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_suffix_builds_sqlite_sidecars() {
        let db = PathBuf::from("/x/svrnmesh.db");
        assert_eq!(append_suffix(&db, ""), PathBuf::from("/x/svrnmesh.db"));
        assert_eq!(
            append_suffix(&db, "-wal"),
            PathBuf::from("/x/svrnmesh.db-wal")
        );
        assert_eq!(
            append_suffix(&db, "-shm"),
            PathBuf::from("/x/svrnmesh.db-shm")
        );
    }

    #[test]
    fn data_dir_honors_override_and_is_never_relative() {
        // Explicit override wins (legacy prefix).
        std::env::set_var("SOVEREIGN_DATA_DIR", "/tmp/svrnmesh-data-dir-test");
        // The branded prefix must win over the legacy one when both exist.
        std::env::set_var("SVRNMESH_DATA_DIR", "/tmp/svrnmesh-data-dir-test-new");
        assert_eq!(data_dir(), PathBuf::from("/tmp/svrnmesh-data-dir-test-new"));
        std::env::remove_var("SVRNMESH_DATA_DIR");
        assert_eq!(data_dir(), PathBuf::from("/tmp/svrnmesh-data-dir-test"));
        std::env::remove_var("SOVEREIGN_DATA_DIR");
        // Unset: the branded home root — NEVER the bare relative `.sovereign`
        // the pre-2026-07-30 hand-rolled chains fell back to (which wrote
        // into the process CWD).
        let d = data_dir();
        assert_ne!(d, PathBuf::from(".sovereign"));
        assert!(d.is_absolute() || d == PathBuf::from("."));
    }

    #[test]
    fn projects_json_prefers_populated_branded_home() {
        let tmp =
            std::env::temp_dir().join(format!("svrnmesh-projects-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in [".svrnmesh", ".sovereign"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
            std::fs::write(tmp.join(d).join("marker"), b"x").unwrap();
        }
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &tmp);
        let p = projects_json();
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(p, tmp.join(".svrnmesh").join("projects.json"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_prefers_populated_new_then_legacy() {
        let tmp = std::env::temp_dir().join(format!("svrnmesh-rebrand-{}", std::process::id()));
        let new = tmp.join("new");
        let legacy = tmp.join("legacy");
        let _ = std::fs::remove_dir_all(&tmp);

        // Neither exists -> returns the new dir (fresh install).
        assert_eq!(resolve_branded_dir(new.clone(), legacy.clone()), new);

        // Legacy populated, new absent -> returns legacy (unmigrated).
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("marker"), b"x").unwrap();
        assert_eq!(resolve_branded_dir(new.clone(), legacy.clone()), legacy);

        // New populated -> returns new even though legacy is present.
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("marker"), b"x").unwrap();
        assert_eq!(resolve_branded_dir(new.clone(), legacy.clone()), new);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
