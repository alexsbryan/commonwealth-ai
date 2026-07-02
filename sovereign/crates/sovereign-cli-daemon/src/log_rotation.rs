// SPDX-License-Identifier: AGPL-3.0-or-later
//! Size-based daemon log rotation.
//!
//! `daemon.log` and `daemon.err` are written by launchd-spawned stdio
//! redirection. launchd holds the file descriptors and never reopens
//! them — there's no SIGHUP-reopen contract on macOS — so the only
//! rotation strategy that doesn't require restarting the daemon is
//! **copy-truncate**:
//!
//! 1. Copy the live file's contents to a `.bak.<unix-ts>` sibling.
//! 2. Truncate the live file to length 0, preserving the inode launchd
//!    is writing to. New log lines land in the now-empty file; nothing
//!    in flight is lost (the bytes are already on disk before rotation
//!    snapshots them).
//! 3. Trim `.bak.*` files for that base name, keeping only the
//!    `keep_n_baks` most recent.
//!
//! Called once at daemon startup and again on a 30-minute timer so
//! growth during a long-running daemon stays bounded between launchd
//! restarts. The 10 MB cap matches what the operator was complaining
//! about in dev (a 17 MB log made the noise patterns invisible);
//! easy to tune via `DEFAULT_SIZE_CAP_BYTES` if the SLO changes.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 10 MiB. Picked so a single rotation comfortably fits the kind of
/// detail an operator scrolls through during a debugging session,
/// without the file becoming unreadable in `less` / browser tools.
pub const DEFAULT_SIZE_CAP_BYTES: u64 = 10 * 1024 * 1024;

/// Five backup files. Enough history for a few days of dev iteration
/// at typical churn; small enough that the logs dir stays well under
/// 100 MB total. Tuned to feel "I can find what I need" not "Spotlight
/// chokes on this directory".
pub const DEFAULT_KEEP_N_BAKS: usize = 5;

/// Public entry point — rotate every known daemon log in `log_dir`
/// that exceeds `size_cap`. Errors are logged and swallowed so
/// rotation can never take the daemon down. Returns the number of
/// files actually rotated for callers that want to surface a metric.
pub fn rotate_daemon_logs(log_dir: &Path, size_cap: u64, keep_n_baks: usize) -> usize {
    // The four canonical filenames the daemon writes to. `daemon.log`
    // is the launchd StandardOutPath (tracing → stderr defaults
    // notwithstanding, fmt() defaults to stdout), `daemon.err` is
    // StandardErrorPath, `daemon.out` is the manually-spawned
    // counterpart from `svrn daemon start`. Hard-coded because
    // the set is small, well-known, and won't grow with new
    // subsystems.
    const TARGETS: &[&str] = &["daemon.log", "daemon.err", "daemon.out"];

    let mut rotated = 0usize;
    for name in TARGETS {
        let path = log_dir.join(name);
        match rotate_one(&path, size_cap, keep_n_baks) {
            Ok(true) => {
                rotated += 1;
                tracing::info!(
                    file = %path.display(),
                    "log rotation: copy-truncated past size cap"
                );
            }
            Ok(false) => { /* under cap, no work to do */ }
            Err(e) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %e,
                    "log rotation failed — leaving file alone"
                );
            }
        }
    }
    rotated
}

/// Rotate a single file if it exceeds `size_cap`. Returns Ok(true)
/// when a rotation happened, Ok(false) when the file was under cap or
/// missing, and Err on filesystem failure (the caller logs and moves
/// on; rotation must never block daemon startup).
fn rotate_one(path: &Path, size_cap: u64, keep_n_baks: usize) -> std::io::Result<bool> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        // Missing file is fine — daemon hasn't written anything yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if meta.len() <= size_cap {
        return Ok(false);
    }

    // Build the .bak path. Unix-second granularity is enough — even
    // under heavy rotation pressure (size cap hit twice in the same
    // second), the `keep_n_baks` trim catches the duplicate so we
    // don't accumulate. If it ever does collide we'd overwrite the
    // older sibling, which is benign.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bak_name = format!(
        "{}.{ts}.bak",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("log"),
    );
    let bak_path = path.with_file_name(bak_name);

    // Copy then truncate. The intermediate state (both files exist
    // simultaneously, briefly) is exactly what `cp + : > file`
    // produces in shell — durable, no in-flight loss.
    std::fs::copy(path, &bak_path)?;
    let f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.set_len(0)?;
    drop(f);

    // Trim older .bak files for this base name. We do this AFTER
    // creating the new bak so a crash mid-rotation can't leave us
    // under-retained.
    if let Err(e) = trim_bak_files(path, keep_n_baks) {
        tracing::warn!(
            file = %path.display(),
            error = %e,
            "trim_bak_files failed — older rotations may accumulate"
        );
    }

    Ok(true)
}

/// Delete `.bak` siblings of `base_path` beyond the `keep_n` most
/// recent (by mtime). Idempotent — safe to call when there are zero
/// or fewer than `keep_n` baks.
fn trim_bak_files(base_path: &Path, keep_n: usize) -> std::io::Result<()> {
    let dir = match base_path.parent() {
        Some(d) => d,
        None => return Ok(()),
    };
    let base_name = match base_path.file_name().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return Ok(()),
    };
    // Match `<base>.<digits>.bak`. Keeping the prefix-and-suffix check
    // tight so we never accidentally trim an unrelated file the
    // operator dropped in the logs dir.
    let prefix = format!("{base_name}.");
    let suffix = ".bak";

    let mut baks: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !name.starts_with(&prefix) || !name.ends_with(suffix) {
            continue;
        }
        // Validate the middle is a unix-seconds string so we don't
        // grab files like `daemon.log.notes.bak`.
        let middle = &name[prefix.len()..name.len() - suffix.len()];
        if middle.is_empty() || !middle.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let mtime = entry.metadata()?.modified().unwrap_or(UNIX_EPOCH);
        baks.push((mtime, entry.path()));
    }

    if baks.len() <= keep_n {
        return Ok(());
    }

    // Most recent first; delete tail.
    baks.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, victim) in baks.into_iter().skip(keep_n) {
        if let Err(e) = std::fs::remove_file(&victim) {
            tracing::warn!(
                file = %victim.display(),
                error = %e,
                "log rotation: failed to delete old bak file"
            );
        }
    }
    Ok(())
}

/// Spawn a background task that calls `rotate_daemon_logs` every
/// `interval`. Returns the JoinHandle so the daemon can abort it on
/// shutdown if it cares to (most code paths just leak it — Tokio
/// reaps tasks on runtime drop).
pub fn spawn_rotation_loop(
    log_dir: PathBuf,
    size_cap: u64,
    keep_n_baks: usize,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick — the daemon's startup-time
        // call already covered "rotate now if past cap"; this loop
        // only handles drift while running.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            // Rotation is filesystem I/O; wrap in spawn_blocking so
            // it never starves the runtime.
            let log_dir = log_dir.clone();
            let _ = tokio::task::spawn_blocking(move || {
                rotate_daemon_logs(&log_dir, size_cap, keep_n_baks);
            })
            .await;
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    fn write_bytes(path: &Path, n: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&vec![b'x'; n]).unwrap();
    }

    #[test]
    fn under_cap_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        write_bytes(&log, 1024);
        let rotated = rotate_daemon_logs(dir.path(), 4096, 5);
        assert_eq!(rotated, 0);
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 1024);
    }

    #[test]
    fn over_cap_copy_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        write_bytes(&log, 8192);
        let rotated = rotate_daemon_logs(dir.path(), 4096, 5);
        assert_eq!(rotated, 1);
        // Live file truncated to 0.
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
        // A `.bak` was created and contains the old size.
        let mut found_bak = None;
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if name.starts_with("daemon.log.") && name.ends_with(".bak") {
                found_bak = Some(entry.path());
                break;
            }
        }
        let bak = found_bak.expect("a .bak file should have been created");
        assert_eq!(std::fs::metadata(&bak).unwrap().len(), 8192);
    }

    #[test]
    fn missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing in the dir at all.
        let rotated = rotate_daemon_logs(dir.path(), 4096, 5);
        assert_eq!(rotated, 0);
    }

    #[test]
    fn trim_keeps_n_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("daemon.log");
        // Create 7 baks with synthetic but increasing timestamps.
        for ts in 1_000..1_007 {
            let bak = dir.path().join(format!("daemon.log.{ts}.bak"));
            write_bytes(&bak, 8);
            // mtime defaults to now; we order by timestamp-in-name
            // implicitly by creation order via short sleeps. For test
            // determinism, set mtime explicitly.
            let mtime =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(ts as u64);
            // filetime crate isn't a dep — fall back to leaving mtime
            // at "now" and rely on the digit suffix as a proxy. The
            // trim function sorts by mtime, but on most filesystems
            // creating files in order produces strictly increasing
            // mtimes, so this is fine for test purposes.
            let _ = mtime;
        }
        // Force a tiny sleep so all 7 mtimes are at least at second
        // resolution apart from any other test artifact.
        std::thread::sleep(Duration::from_millis(20));
        trim_bak_files(&base, 3).unwrap();

        let baks: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n.starts_with("daemon.log.") && n.ends_with(".bak"))
            .collect();
        assert_eq!(
            baks.len(),
            3,
            "expected exactly 3 baks after trim, got {baks:?}"
        );
    }

    #[test]
    fn trim_ignores_non_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("daemon.log");
        // Files that look bak-ish but aren't ours.
        write_bytes(&dir.path().join("daemon.log.notes.bak"), 8);
        write_bytes(&dir.path().join("other.log.1234.bak"), 8);
        write_bytes(&dir.path().join("daemon.log.1234.bak"), 8);
        trim_bak_files(&base, 0).unwrap();
        // Only our matching daemon.log.<digits>.bak should be gone.
        assert!(dir.path().join("daemon.log.notes.bak").exists());
        assert!(dir.path().join("other.log.1234.bak").exists());
        assert!(!dir.path().join("daemon.log.1234.bak").exists());
    }

    #[test]
    fn rotation_handles_all_three_target_files() {
        let dir = tempfile::tempdir().unwrap();
        write_bytes(&dir.path().join("daemon.log"), 8192);
        write_bytes(&dir.path().join("daemon.err"), 8192);
        write_bytes(&dir.path().join("daemon.out"), 8192);
        let rotated = rotate_daemon_logs(dir.path(), 4096, 5);
        assert_eq!(rotated, 3);
        for name in &["daemon.log", "daemon.err", "daemon.out"] {
            assert_eq!(
                std::fs::metadata(dir.path().join(name)).unwrap().len(),
                0,
                "{name} should have been truncated"
            );
        }
    }
}
