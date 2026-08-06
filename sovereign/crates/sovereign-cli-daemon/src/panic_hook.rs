// SPDX-License-Identifier: AGPL-3.0-or-later
//! Process-wide panic hook for the daemon (DAEMON_RESILIENCE.md P0.4).
//!
//! Before this module the daemon binary installed NO panic hook — the
//! CLI shim's hook is lost the moment it `exec()`s into
//! `sovereign-cli-daemon`, so a panic in a tokio worker task was
//! swallowed by tokio with no log line and no artifact, and a panic on
//! the main path printed only the default stderr message. Headless
//! installs (launchd/systemd/CLI) got none of the desktop's crash
//! forensics (`sovereign-desktop` `crash_report.rs`).
//!
//! This hook gives the headless daemon parity:
//!
//! 1. always writes a one-line summary to fd 2 (lands in `daemon.err`
//!    under every supervision topology) — via a raw, non-panicking
//!    write, NOT `eprintln!`; see [`eprint_best_effort`],
//! 2. writes a structured JSON crash record to
//!    `<data_dir>/crashes/daemon-panic-<ts>-<n>.json`,
//! 3. overwrites `<data_dir>/crashes/last-crash.json` — the marker a
//!    later boot / doctor / desktop surface reads to say "the daemon
//!    recovered from a crash" (P2.3 consumes this),
//! 4. prunes the crashes dir to the newest [`KEEP_RECORDS`] records.
//!
//! Known limitation (documented in the resilience spec): SIGABRT from
//! a stack overflow and native ggml SIGSEGVs bypass Rust's panic
//! machinery entirely — those still leave only the service manager's
//! exit status. This hook covers every *Rust* panic, including ones in
//! background tasks that tokio would otherwise swallow silently.
//!
//! Everything here is local-first: records stay on the user's disk;
//! nothing is uploaded anywhere (no-telemetry posture, spec §1.3).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Cap on retained crash records — enough history to see a pattern,
/// bounded so a crash loop can't fill the disk.
const KEEP_RECORDS: usize = 50;

/// Monotonic suffix so two panics in the same second (multi-thread)
/// can't clobber each other's record file.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Install the daemon panic hook. Chains the previously-installed hook
/// (the std default prints the message + backtrace to stderr — we keep
/// that contract for operators tailing `daemon.err`).
pub(crate) fn install(data_dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Nothing in here may panic: a panic inside the hook aborts the
        // process with no record at all. Every step is best-effort.
        // The two ways that invariant has actually been broken are the
        // print macros (`eprintln!`/`println!` panic on a failed write —
        // use `eprint_best_effort`) and `tracing` dispatch (a subscriber
        // layer can panic on the caller's thread). Neither appears below;
        // keep it that way.
        let message = panic_message(info);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let thread = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();

        eprint_best_effort(&format!(
            "daemon: PANIC in thread '{thread}' at {location}: {message} \
             (crash record: {})\n",
            data_dir.join("crashes").display()
        ));
        // NO `tracing::error!` here. A tracing event is dispatched into
        // whatever layers the process installed, and `tracing_subscriber`'s
        // fmt layer reports a failed write to its own writer with
        // `eprintln!` (fmt_layer.rs:1053) — which panics when fd 2 is also
        // gone. That is a panic raised from inside the panic hook, i.e. the
        // abort this module exists to prevent. The structured record
        // written below carries the same fields (thread, location,
        // message, backtrace) and is durable, so nothing is lost.

        let _ = write_crash_record(&data_dir, &message, &location, &thread, &backtrace);

        // Keep the std default's stderr output (message + backtrace).
        previous(info);
    }));
}

/// Write to fd 2 without the `eprintln!` panic.
///
/// `eprintln!` routes through `std::io::stdio::_eprint`, which turns a
/// failed write into `panic!("failed printing to stderr: {e}")`. A panic
/// raised while the panic hook is running is a nested panic: std marks
/// the thread as already-panicking and `abort()`s *before* re-entering
/// the hook, so the process dies with no record — precisely the outcome
/// this module was built to prevent.
///
/// That is not a theoretical edge. Under the desktop's supervised
/// topology the daemon child is spawned with `Stdio::piped()` for both
/// stdout and stderr (`sovereign_compute::supervisor`), so both fds are
/// pipes to the parent UI process. When the user quits, the parent exits
/// and every subsequent write from the child fails with EPIPE. Any log
/// line emitted during that window panicked, and this hook's own
/// `eprintln!` then panicked again — a guaranteed SIGABRT and a macOS
/// crash report on every quit (diagnosed 2026-08-05).
///
/// Best-effort by construction: if the write fails there is nowhere left
/// to report it, so the error is dropped and the caller continues on to
/// the durable JSON record.
fn eprint_best_effort(msg: &str) {
    write_best_effort(&mut std::io::stderr().lock(), msg);
}

/// The testable core of [`eprint_best_effort`]: swallow every write
/// error rather than let one become a panic. Split out so the
/// "a failing sink must not panic" invariant has an actual failing
/// input to assert against (see `failing_sink_does_not_panic`) instead
/// of living only in a comment.
fn write_best_effort(sink: &mut impl std::io::Write, msg: &str) {
    let _ = sink.write_all(msg.as_bytes());
    let _ = sink.flush();
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn write_crash_record(
    data_dir: &Path,
    message: &str,
    location: &str,
    thread: &str,
    backtrace: &str,
) -> std::io::Result<()> {
    let crashes = data_dir.join("crashes");
    std::fs::create_dir_all(&crashes)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let record = serde_json::json!({
        "kind": "panic",
        "timestamp_unix": ts,
        "pid": std::process::id(),
        "binary": "sovereign-cli-daemon",
        "version": env!("CARGO_PKG_VERSION"),
        "thread": thread,
        "location": location,
        "message": message,
        "backtrace": backtrace,
    });
    let body = serde_json::to_vec_pretty(&record).unwrap_or_default();
    let path = crashes.join(format!("daemon-panic-{ts}-{seq}.json"));
    std::fs::write(&path, &body)?;
    // Marker read at next boot / by doctor: "the last thing that
    // happened to a daemon on this machine was this crash."
    std::fs::write(crashes.join("last-crash.json"), &body)?;
    prune_records(&crashes);
    Ok(())
}

/// Keep the newest [`KEEP_RECORDS`] `daemon-panic-*.json` files.
fn prune_records(crashes: &Path) {
    let Ok(entries) = std::fs::read_dir(crashes) else {
        return;
    };
    let mut records: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("daemon-panic-") && n.ends_with(".json"))
        })
        .collect();
    if records.len() <= KEEP_RECORDS {
        return;
    }
    // Filename embeds the unix timestamp — lexicographic sort on the
    // zero-unpadded secs is wrong across digit-count boundaries, so
    // sort on mtime instead.
    records.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    let excess = records.len() - KEEP_RECORDS;
    for stale in records.into_iter().take(excess) {
        let _ = std::fs::remove_file(stale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink whose every operation fails with `BrokenPipe` — the exact
    /// state of the daemon child's fd 1 and fd 2 once the desktop parent
    /// has exited and closed the read ends of the pipes it spawned the
    /// child with.
    struct BrokenPipe;

    impl std::io::Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    /// The hook's stderr write must survive a dead sink.
    ///
    /// This is the regression guard for the every-quit SIGABRT: the hook
    /// used `eprintln!`, which turns a failed write into a panic, and a
    /// panic inside the panic hook aborts the process before any crash
    /// record is written. `write_best_effort` must swallow it instead.
    #[test]
    fn failing_sink_does_not_panic() {
        write_best_effort(&mut BrokenPipe, "daemon: PANIC in thread 'x'\n");
    }

    /// The contrast case, kept as executable documentation of *why*
    /// `write_best_effort` exists: routing the same message through the
    /// `write!` macro's unwrap-shaped contract does panic on this sink.
    /// If this ever stops panicking, the helper above is no longer
    /// earning its place.
    #[test]
    fn the_panicking_alternative_really_does_panic() {
        let attempt = std::panic::catch_unwind(|| {
            use std::io::Write;
            // `eprintln!` cannot be pointed at a test sink, but it fails
            // exactly this way: format, write, `.unwrap()` the result.
            write!(BrokenPipe, "boom").unwrap();
        });
        assert!(
            attempt.is_err(),
            "a failing sink must panic through the unwrap path — that is the \
             hazard `write_best_effort` exists to avoid"
        );
    }

    #[test]
    fn crash_record_roundtrip_and_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_crash_record(dir.path(), "boom", "src/x.rs:1:1", "test-thread", "bt")
            .expect("record written");
        let crashes = dir.path().join("crashes");
        let marker = std::fs::read_to_string(crashes.join("last-crash.json")).expect("marker");
        let parsed: serde_json::Value = serde_json::from_str(&marker).expect("valid json");
        assert_eq!(parsed["kind"], "panic");
        assert_eq!(parsed["message"], "boom");
        assert_eq!(parsed["thread"], "test-thread");
        let records = std::fs::read_dir(&crashes)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("daemon-panic-"))
            })
            .count();
        assert_eq!(records, 1);
    }

    #[test]
    fn prune_keeps_newest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        for i in 0..(KEEP_RECORDS + 7) {
            let p = crashes.join(format!("daemon-panic-1000-{i}.json"));
            std::fs::write(&p, b"{}").unwrap();
            // Distinct mtimes matter for the sort — stamp them
            // explicitly rather than trusting filesystem timestamp
            // granularity.
            let t = std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(1_000_000 + i as u64);
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(t))
                .unwrap();
        }
        prune_records(&crashes);
        let mut left: Vec<String> = std::fs::read_dir(&crashes)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP_RECORDS);
        // The oldest 7 are the ones pruned.
        assert!(!left.contains(&"daemon-panic-1000-0.json".to_string()));
        assert!(!left.contains(&"daemon-panic-1000-6.json".to_string()));
        assert!(left.contains(&format!("daemon-panic-1000-{}.json", KEEP_RECORDS + 6)));
    }
}
