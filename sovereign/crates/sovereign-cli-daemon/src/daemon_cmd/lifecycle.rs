// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon process lifecycle — extracted from `daemon_cmd` (§3.2).
//! start / stop / restart / reload / status, the pidfile + port-probe
//! plumbing, and graceful-shutdown signal handling.

use super::home_dir_buf;

/// `svrn daemon restart` — hard-restart the registered service.
/// Most users reach for this when the daemon feels stuck or after a
/// change that isn't hot-reloadable (port, data_dir). For model-only
/// changes, prefer `svrn daemon reload` — no gap in availability.
pub(super) async fn stop_daemon() -> i32 {
    eprintln!("stopping the daemon …");

    // Prefer the PID file written by `daemon start` — that's the only
    // way we can reliably stop a daemon that wasn't launched via a
    // service manager. Fall back to launchctl / systemctl when the
    // PID file is absent (service-managed install) or stale.
    if let Some(pid) = read_daemon_pid() {
        #[cfg(unix)]
        {
            // SAFETY: POSIX kill is async-signal-safe; we're only sending
            // SIGTERM to a pid we own (we wrote the pidfile ourselves).
            let rc = unsafe {
                libc_kill(pid, 15 /* SIGTERM */)
            };
            if rc == 0 {
                return await_exit_or_sigkill(pid, "").await;
            }
            // kill() failed — pid likely stale. Clean up and fall
            // through to service_install so a service-managed instance
            // (if any) still gets stopped.
            let _ = std::fs::remove_file(daemon_pid_path());
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
        }
    }

    // No usable pidfile. Before forwarding to systemctl/launchctl,
    // try a port-based lookup: if something is listening on :9741, that
    // process IS the daemon and we can SIGTERM it directly. This catches:
    //   - daemons launched by an older binary that didn't write a pidfile
    //   - daemons started via `cargo run -- daemon run` from a dev shell
    //   - daemons whose pidfile was hand-deleted
    // Without this, `daemon stop` falls through to `systemctl stop` on
    // an inactive unit, which is a no-op returning exit 0 — i.e. silently
    // reports success while the actual daemon keeps serving.
    //
    // `SOVEREIGN_STOP_SANDBOXED=1` confines the stop chain to the
    // PIDFILE legs only — no :9741 port-lookup SIGTERM, no
    // service-manager forwarding. It exists for AUTOMATION that
    // exercises the stop path under an isolated HOME (the
    // phase3_serve_lifecycle test): HOME isolation only covers the
    // pidfile legs, and on 2026-06-10 the test's `stop` killed the
    // developer's live daemon TWICE — first via this port fallback,
    // then (after the port leg was gated) via `launchctl stop`, which
    // doesn't consult HOME at all. Operators should never set it.
    #[cfg(unix)]
    if !stop_sandboxed() {
        if let Some(pid) = find_daemon_pid_by_port(9741) {
            let rc = unsafe {
                libc_kill(pid, 15 /* SIGTERM */)
            };
            if rc == 0 {
                return await_exit_or_sigkill(pid, ", found by :9741 listener").await;
            }
            // kill() failed (most likely EPERM on a daemon owned by another
            // user). Fall through to the service-manager path; it might be
            // the only thing with permission.
        }
    }

    if stop_sandboxed() {
        eprintln!(
            "no running daemon found via pidfile (sandboxed stop: \
             port-lookup + service-manager legs skipped)"
        );
        return 1;
    }
    match crate::service_install::stop_service() {
        Ok(()) => {
            eprintln!("✓ stop signal sent — daemon will exit after draining in-flight requests");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Poll for `pid` to exit within the SIGTERM grace window; past it,
/// escalate to SIGKILL (DAEMON_RESILIENCE.md P0.5). The old behavior —
/// "didn't exit after 10s; leaving it alone" — left a wedged daemon
/// holding the models and `:9741`, and `daemon restart` then aborted
/// on the non-zero stop, stranding the operator with a zombie. A stop
/// verb must stop the daemon; a process wedged past the SIGTERM grace
/// has already forfeited its graceful drain.
#[cfg(unix)]
async fn await_exit_or_sigkill(pid: i32, found_via: &str) -> i32 {
    const TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
    const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

    let deadline = std::time::Instant::now() + TERM_GRACE;
    while std::time::Instant::now() < deadline {
        if unsafe { libc_kill(pid, 0) } != 0 {
            let _ = std::fs::remove_file(daemon_pid_path());
            eprintln!("✓ stopped (pid {pid}{found_via})");
            return 0;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    eprintln!(
        "⚠ pid {pid} didn't exit within {}s of SIGTERM — escalating to SIGKILL \
         (daemon is wedged; graceful drain forfeited)",
        TERM_GRACE.as_secs()
    );
    // SAFETY: same contract as the SIGTERM above — signalling a pid we
    // identified as the daemon (pidfile owner or :9741 listener).
    if unsafe {
        libc_kill(pid, 9 /* SIGKILL */)
    } != 0
    {
        // Either it exited between the poll and the kill (a win), or we
        // lack permission (EPERM — another user's daemon).
        if unsafe { libc_kill(pid, 0) } != 0 {
            let _ = std::fs::remove_file(daemon_pid_path());
            eprintln!("✓ stopped (pid {pid}{found_via}; exited during escalation)");
            return 0;
        }
        eprintln!("error: SIGKILL pid {pid} failed (permission?) — daemon left running");
        return 1;
    }
    let deadline = std::time::Instant::now() + KILL_GRACE;
    while std::time::Instant::now() < deadline {
        if unsafe { libc_kill(pid, 0) } != 0 {
            let _ = std::fs::remove_file(daemon_pid_path());
            eprintln!("✓ stopped (pid {pid}{found_via}; required SIGKILL)");
            return 0;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    eprintln!(
        "error: pid {pid} survived SIGKILL for {}s (uninterruptible I/O?) — inspect manually",
        KILL_GRACE.as_secs()
    );
    1
}

/// See the sandbox note in `stop_daemon` — automation-only escape
/// hatch confining stop to the pidfile legs.
fn stop_sandboxed() -> bool {
    std::env::var("SOVEREIGN_STOP_SANDBOXED").ok().as_deref() == Some("1")
}

/// Find the PID listening on `port` on localhost. Used by `daemon stop`
/// as a last-resort fallback when no pidfile is present and the service
/// manager has nothing to stop — see `stop_daemon` for the call site.
///
/// We try two probes in order, preferring `lsof` because its output is
/// stable and trivially parseable, then `ss` (iproute2) for hosts where
/// lsof isn't installed (notably minimal Linux containers). Both are
/// invoked with arguments that print one PID per line and nothing else.
#[cfg(unix)]
fn find_daemon_pid_by_port(port: u16) -> Option<i32> {
    use std::process::Command;

    // `lsof -t` → "terse" output: bare PIDs, one per line.
    // `-sTCP:LISTEN` filters out connected sockets (so we don't pick up
    // a client of :9741 if one happens to be alive).
    // `-i 4TCP:<port>` is more selective than `-i :<port>` — IPv4 only,
    // TCP only, avoiding UDP false positives.
    if let Ok(out) = Command::new("lsof")
        .args(["-t", "-sTCP:LISTEN", "-i", &format!("4TCP:{port}")])
        .output()
    {
        if out.status.success() {
            if let Some(pid) = parse_first_pid_line(&out.stdout) {
                return Some(pid);
            }
        }
    }

    // `ss -H -tlnp 'sport = :<port>'` prints one line per listener
    // with no header. The pid lives inside `users:(("name",pid=N,fd=...))`
    // so we have to extract it. -H suppresses the header line.
    if let Ok(out) = Command::new("ss")
        .args(["-H", "-tlnp", &format!("sport = :{port}")])
        .output()
    {
        if out.status.success() {
            if let Some(pid) = parse_ss_first_pid(&String::from_utf8_lossy(&out.stdout)) {
                return Some(pid);
            }
        }
    }

    None
}

#[cfg(unix)]
fn parse_first_pid_line(bytes: &[u8]) -> Option<i32> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.lines()
        .next()
        .and_then(|l| l.trim().parse::<i32>().ok())
}

/// Extract the first `pid=N` integer from one or more `ss -tlnp` lines.
/// Returns None when no line contains a `pid=` token or the digits don't
/// parse as i32. The pid token shape is iproute2-stable: the relevant
/// fragment is `users:(("name",pid=12345,fd=7))`.
#[cfg(unix)]
fn parse_ss_first_pid(text: &str) -> Option<i32> {
    for line in text.lines() {
        if let Some(rest) = line.split("pid=").nth(1) {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(p) = digits.parse::<i32>() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(all(test, unix))]
mod run_lock_tests {
    use super::*;

    #[test]
    fn second_flock_refused_while_first_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.lock");
        let first = try_flock_exclusive(&path)
            .expect("open lock file")
            .expect("first acquire succeeds");
        // A second open file description on the same path must be
        // refused while the first is held — this is exactly the
        // second-`daemon run` scenario.
        assert!(
            try_flock_exclusive(&path)
                .expect("open lock file")
                .is_none(),
            "second acquire must be refused while first is held"
        );
        drop(first);
        assert!(
            try_flock_exclusive(&path)
                .expect("open lock file")
                .is_some(),
            "lock must be re-acquirable after the holder drops (kernel release)"
        );
    }
}

#[cfg(all(test, unix))]
mod stop_daemon_tests {
    use super::*;

    #[test]
    fn parses_lsof_terse_single_pid() {
        assert_eq!(parse_first_pid_line(b"664307\n"), Some(664307));
    }

    #[test]
    fn parses_lsof_terse_first_of_multiple_pids() {
        // lsof -t prints one pid per line if multiple processes match.
        // Picking the first is fine — the bind would have prevented a
        // second daemon from coexisting; multiple lines means there is
        // a non-daemon listener masquerading on the port, and we'd
        // rather SIGTERM the obvious one than spray signals at every
        // pid in the list.
        assert_eq!(parse_first_pid_line(b"664307\n123\n"), Some(664307));
    }

    #[test]
    fn returns_none_on_empty_lsof_output() {
        assert_eq!(parse_first_pid_line(b""), None);
        assert_eq!(parse_first_pid_line(b"\n"), None);
    }

    #[test]
    fn parses_ss_listener_line_with_pid() {
        // Real shape of `ss -H -tlnp 'sport = :9741'` output:
        let sample = "LISTEN 0      4096       127.0.0.1:9741       0.0.0.0:* \
                      users:((\"sovereign-cli\",pid=664307,fd=12))";
        assert_eq!(parse_ss_first_pid(sample), Some(664307));
    }

    #[test]
    fn ignores_ss_lines_without_pid_field() {
        let sample = "LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\n\
                      LISTEN 0 4096 127.0.0.1:9741 0.0.0.0:* \
                        users:((\"sovereign-cli\",pid=664307,fd=12))";
        assert_eq!(parse_ss_first_pid(sample), Some(664307));
    }

    #[test]
    fn ss_no_pid_token_returns_none() {
        assert_eq!(parse_ss_first_pid(""), None);
        assert_eq!(
            parse_ss_first_pid("LISTEN 0 128 0.0.0.0:22 0.0.0.0:*"),
            None
        );
    }
}

/// `svrn daemon start` — spawn `daemon run` as a detached child
/// so it keeps running after this CLI exits. Writes the child pid to
/// `~/.sovereign/daemon.pid` and tails logs to `~/.sovereign/logs/`.
///
/// Idempotent: if `:9741` already answers, prints the running pid (if
/// we wrote it) and returns 0.
///
/// This is the dev-workflow counterpart to `svrn setup`'s
/// launchd/systemd registration — when you don't want a service
/// manager owning lifecycle, `start` gives you a one-liner.
pub(super) async fn start_daemon() -> i32 {
    if wait_for_ready(std::time::Duration::from_millis(200)).await {
        let pid_hint = read_daemon_pid()
            .map(|p| format!(" (pid {p})"))
            .unwrap_or_default();
        eprintln!("✓ daemon already running on :9741{pid_hint}");
        return 0;
    }

    // Bind-collision detector (added 2026-05-20).
    //
    // Failure mode this prevents: a stale process holds 127.0.0.1:9741
    // but doesn't answer our readiness probe (wrong build, half-shut
    // state, debug binary from an earlier session). `wait_for_ready`
    // returns false, we then spawn a fresh daemon which successfully
    // binds *:9741 — but the kernel routes loopback traffic to the
    // address-specific listener (the orphan), and every localhost
    // call silently goes to the dead listener. Confirmed 2026-05-20
    // with a stale `sovereign-cli serve` (debug build) holding
    // 127.0.0.1:9741 while a fresh `daemon start` bound *:9741. The
    // operator saw nothing in the new daemon's log because the new
    // daemon wasn't getting any traffic.
    //
    // Detect by asking lsof / ss who owns the port. If it's NOT our
    // pidfile owner, refuse loudly with a kill-the-orphan hint rather
    // than start a second listener.
    //
    // Unix-only: `find_daemon_pid_by_port` shells to lsof/ss (see its
    // `#[cfg(unix)]` gate). On Windows the detector is skipped — the
    // bind-collision failure mode it guards against is the Unix
    // loopback address-specific-listener routing quirk.
    #[cfg(unix)]
    if let Some(holder_pid) = find_daemon_pid_by_port(9741) {
        let our_pid = read_daemon_pid();
        if our_pid != Some(holder_pid) {
            eprintln!(
                "✗ :9741 is held by pid {holder_pid} but the readiness probe failed.\n  \
                 something else owns the port (stale debug build, half-shut daemon, foreign process).\n  \
                 Stop it first: `kill {holder_pid}` (or `svrn daemon stop` if it's a managed sovereign),\n  \
                 then re-run `svrn daemon start`."
            );
            return 1;
        }
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve current_exe: {e}");
            return 1;
        }
    };

    // Derive from the resolved runtime root (`~/.svrnmesh`, falling back to a
    // populated legacy `~/.sovereign`) — NOT a hardcoded `~/.sovereign`. This
    // keeps manual `svrn daemon start` writing to the same `logs/daemon.err`
    // the service path uses (service_install.rs) and every reader expects.
    // The bare `.sovereign` hardcode survived the `svrnmesh` rename and sent
    // manual-start logs to a stale dir, which is what made an alignment
    // ingest's progress invisible on 2026-07-24.
    let log_dir = sovereign_cli_shared::dirs::sovereign_root().join("logs");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!("error: cannot create {}: {e}", log_dir.display());
        return 1;
    }
    let out_path = log_dir.join("daemon.out");
    let err_path = log_dir.join("daemon.err");

    // Append so repeated start/stop cycles keep history rather than
    // truncating on each launch — matches launchd's default rotation
    // semantics (it also appends until an external log-rotator moves
    // the file aside, which is the `.bak` pattern seen in the logs
    // dir already).
    let out_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: open {}: {e}", out_path.display());
            return 1;
        }
    };
    let err_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&err_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: open {}: {e}", err_path.display());
            return 1;
        }
    };

    // Clean up a stale PID file before writing a new one so `stop`
    // can't target a recycled pid if the prior daemon crashed.
    if let Some(pid) = read_daemon_pid() {
        #[cfg(unix)]
        if unsafe { libc_kill(pid, 0) } != 0 {
            let _ = std::fs::remove_file(daemon_pid_path());
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
        }
    }

    eprintln!("starting the daemon (logs: {})…", log_dir.display());

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(err_file));

    // Diagnostics + defensive stack budget for the daemon process.
    //
    // - `RUST_BACKTRACE=full` makes the next stack overflow log a
    //   real frame trace to daemon.err — without it we see only
    //   "thread 'tokio-rt-worker' has overflowed its stack" with no
    //   symbol info, which is what made the 2026-05-11 fragility
    //   investigation slow.
    // - `RUST_MIN_STACK=8388608` (8 MiB) bumps the default thread
    //   stack size for every std::thread spawn the daemon makes.
    //   tokio's multi-thread runtime workers inherit this when they
    //   don't explicitly set `thread_stack_size`. Default is 2 MiB
    //   on macOS, which we've reproducibly overflowed under
    //   drift-detect load (77 overflows / 166 daemon starts this
    //   session). 8 MiB is the same headroom Cargo's build worker
    //   threads use and matches what corpus-engine's tree-sitter
    //   path needs on deeply-nested wikitext templates.
    //
    // Both vars are only set when the parent didn't already set
    // them — so a developer profiling with custom RUST_BACKTRACE
    // (e.g. =0, =1) or shrinking the stack to reproduce isn't
    // overridden.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        cmd.env("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        cmd.env("RUST_MIN_STACK", "8388608");
    }

    // Detach from the shell's process group so Ctrl-C in the invoking
    // terminal doesn't take the daemon down with it. Combined with
    // the /dev/null stdin + redirected stdio above, this is enough
    // for the common dev case (launch-from-shell, close shell). For
    // truly hostile environments (ssh disconnect on a flaky link),
    // use the launchd/systemd path via `svrn setup` instead.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: spawn daemon: {e}");
            return 1;
        }
    };
    let pid = child.id() as i32;

    // Drop the Child handle so we don't wait on it — the process is
    // now fully detached and owned by init (via process_group(0)).
    drop(child);

    let pid_path = daemon_pid_path();
    if let Err(e) = std::fs::write(&pid_path, format!("{pid}\n")) {
        eprintln!(
            "⚠ started pid {pid} but could not write {}: {e}",
            pid_path.display()
        );
        // Don't abort — the daemon is still coming up; caller can
        // stop via `pkill -f 'svrn daemon run'` if needed.
    }

    let timeout = ready_timeout();
    if wait_for_ready_with_progress(timeout, &err_path).await {
        eprintln!("✓ daemon ready at http://127.0.0.1:9741 (pid {pid})");
        // Supervision advisory: a pidfile-managed daemon has no
        // service manager behind it — a crash/jetsam leaves it down
        // until someone notices. One line, once, at the moment the
        // operator is already looking.
        if !crate::service_install::service_installed() {
            eprintln!(
                "note: this daemon is unsupervised (no restart on crash). \
                 For auto-restart: svrn install-service"
            );
        }
        return 0;
    }
    eprintln!(
        "⚠ pid {pid} started but :9741 didn't respond within {}s\n\
         the daemon may still be loading models — re-check with `svrn daemon status`\n\
         tail {} for details",
        timeout.as_secs(),
        err_path.display()
    );
    1
}

/// Readiness timeout for `daemon start`, env-overridable via
/// `SOVEREIGN_DAEMON_READY_TIMEOUT_SECS`. Default 120s: a cold boot
/// mmap-loads the primary 35B + fast + embed models, which takes
/// 30–60s on the canonical 64GB config — the old hardcoded 20s made
/// healthy cold boots report failure. Same respect-operator-override
/// posture as the RUST_BACKTRACE/RUST_MIN_STACK handling above.
fn ready_timeout() -> std::time::Duration {
    parse_ready_timeout(
        std::env::var("SOVEREIGN_DAEMON_READY_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

/// Pure parse so the policy is unit-testable without touching
/// process-global env. Unset/garbage/zero all fall back to the default.
fn parse_ready_timeout(raw: Option<&str>) -> std::time::Duration {
    const DEFAULT_SECS: u64 = 120;
    let secs = raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Path to the pidfile written by `daemon start`.
pub(super) fn daemon_pid_path() -> std::path::PathBuf {
    home_dir_buf().join(".sovereign").join("daemon.pid")
}

/// Single-instance run lock (DAEMON_RESILIENCE.md P0.5).
///
/// `daemon start` has always had a port-collision guard, but the
/// `daemon run` path (systemd, launchd, the shell supervisor, a stray
/// manual run) had NONE: a second `run` loaded every model (double
/// RAM), unconditionally overwrote the pidfile so `stop` targeted the
/// zombie, and — its bind being best-effort — parked forever with no
/// listener. flock(2) is the primitive built for this: the kernel
/// releases it on ANY exit path including SIGKILL, so there is no
/// stale-lock cleanup logic to get wrong.
///
/// The returned `File` must be held for the daemon's lifetime —
/// dropping it releases the lock. `SOVEREIGN_ALLOW_MULTIPLE_DAEMONS=1`
/// skips the guard for multi-daemon harnesses that drive `daemon run`
/// under a shared HOME (per-HOME harnesses like mesh-soak don't need
/// it — the lock file is per data dir).
#[cfg(unix)]
pub(super) fn acquire_run_lock() -> Result<Option<std::fs::File>, String> {
    if std::env::var("SOVEREIGN_ALLOW_MULTIPLE_DAEMONS")
        .ok()
        .as_deref()
        == Some("1")
    {
        return Ok(None);
    }
    let dir = home_dir_buf().join(".sovereign");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("daemon.lock");
    match try_flock_exclusive(&path) {
        Err(e) => Err(format!("cannot open run lock {}: {e}", path.display())),
        Ok(None) => Err(format!(
            "another daemon already holds the run lock ({}) — refusing to run a second \
             instance.\n  Check `svrn daemon status`; stop the running daemon with \
             `svrn daemon stop`.\n  (Multi-daemon test harnesses under one HOME: set \
             SOVEREIGN_ALLOW_MULTIPLE_DAEMONS=1.)",
            path.display()
        )),
        Ok(Some(file)) => Ok(Some(file)),
    }
}

/// The flock core, path-parameterized for unit tests. `Ok(None)` =
/// held by another open file description (i.e. another daemon).
#[cfg(unix)]
fn try_flock_exclusive(path: &std::path::Path) -> std::io::Result<Option<std::fs::File>> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)?;
    // SAFETY: flock on an fd we own; LOCK_NB means it never blocks.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    Ok(Some(file))
}

#[cfg(not(unix))]
pub(super) fn acquire_run_lock() -> Result<Option<std::fs::File>, String> {
    Ok(None)
}

/// Read the pidfile and return its pid if the process is still alive.
/// Returns None for a missing, empty, unparseable, or stale pidfile.
pub(crate) fn read_daemon_pid() -> Option<i32> {
    let raw = std::fs::read_to_string(daemon_pid_path()).ok()?;
    let pid: i32 = raw.trim().parse().ok()?;
    #[cfg(unix)]
    {
        // `kill(pid, 0)` returns 0 iff the process exists and we have
        // permission to signal it; any error (ESRCH, EPERM) means we
        // shouldn't trust the pidfile.
        if unsafe { libc_kill(pid, 0) } != 0 {
            return None;
        }
    }
    Some(pid)
}

// Minimal FFI shim for `kill(2)` so we can probe / signal the daemon
// pid without pulling in the `libc` crate as a dep. Signature matches
// POSIX: `int kill(pid_t pid, int sig)` where pid_t is i32 on every
// platform Sovereign targets.
#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
/// `svrn daemon restart` — stop the running daemon (whichever
/// lifecycle owns it: pidfile or launchd) and start a fresh one.
///
/// Earlier versions of this command went straight to `launchctl
/// kickstart -k gui/<uid>/com.svrnmesh.daemon`. That broke for every
/// user who started the daemon via `svrn daemon start` (the
/// pidfile-managed path), because the launchd service isn't loaded
/// in the gui domain — kickstart errors out with "Could not find
/// service in domain for user". The asymmetry was: `start` and
/// `stop` both fall back through pidfile → launchctl, but `restart`
/// only ever spoke to launchctl.
///
/// Fix: compose `restart` from `stop_daemon().await + start_daemon().await`,
/// so the same lifecycle inference (pidfile preferred, launchctl as
/// fallback) applies to all three commands. Cost: launchd-managed
/// installs that were previously kickstarted now end up under
/// `daemon start`'s detached-child path — consistent with what
/// `daemon stop && daemon start` already does, and which any user
/// who actually wants strict launchd accounting can run via
/// `launchctl kickstart -k gui/$(id -u)/com.svrnmesh.daemon`
/// directly.
pub(super) async fn restart_daemon() -> i32 {
    eprintln!("restarting the daemon …");
    let stop_rc = stop_daemon().await;
    if stop_rc != 0 {
        // stop_daemon already printed the failure reason. Don't try
        // to start on top of a daemon we couldn't confirm is gone —
        // we'd just race the old one for :9741.
        return stop_rc;
    }
    start_daemon().await
}

/// `svrn daemon reload` — POST /v1/admin/reload. Hot-reloads
/// changed model paths in place without dropping connections.
/// Reports which fields hot-reloaded and which require a full
/// restart; a subsequent `svrn daemon restart` picks those up.
pub(super) async fn reload_daemon() -> i32 {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: build http client: {e}");
            return 1;
        }
    };
    let resp = client
        .post("http://127.0.0.1:9741/v1/admin/reload")
        .json(&serde_json::json!({}))
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "error: could not reach daemon at :9741 ({e}).\n\
                 hint: is it running? try `svrn daemon status`."
            );
            return 1;
        }
    };
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({"error": "non-JSON response"}));
    if !status.is_success() {
        eprintln!("error: admin/reload returned {status}: {body}");
        return 1;
    }
    let reloaded = body
        .get("reloaded_fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let restart_required = body
        .get("restart_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if reloaded.is_empty() && !restart_required {
        eprintln!("✓ no config changes detected — nothing to reload");
        return 0;
    }
    if !reloaded.is_empty() {
        let names: Vec<String> = reloaded
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        eprintln!("✓ hot-reloaded: {}", names.join(", "));
    }
    if restart_required {
        let pending = body
            .get("restart_required_fields")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        eprintln!(
            "⚠ these changes need a full restart: {pending}\n\
             run `svrn daemon restart` to apply them."
        );
    }
    0
}

/// `svrn daemon status` — is the daemon alive and answering?
pub(super) async fn status_daemon() -> i32 {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: build http client: {e}");
            return 1;
        }
    };
    match client.get("http://127.0.0.1:9741/v1/models").send().await {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r
                .json()
                .await
                .unwrap_or_else(|_| serde_json::json!({"data": []}));
            let count = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            eprintln!("✓ daemon running at http://localhost:9741 ({count} models registered)");
            0
        }
        Ok(r) => {
            eprintln!(
                "⚠ something is answering on :9741 but returned {}. \
                 not a svrn daemon, or in a bad state.",
                r.status()
            );
            1
        }
        Err(_) => {
            eprintln!(
                "✗ daemon not reachable on :9741.\n\
                 start it with `svrn daemon restart` (if installed)\n\
                 or run `svrn setup` (if not yet configured)."
            );
            1
        }
    }
}

/// `wait_for_ready` plus operator feedback: a progress line every 10s
/// so a long (healthy) cold model load is distinguishable from a hang.
/// Used only by the `daemon start` readiness wait — the 200ms
/// idempotency probe at the top of `start_daemon` stays silent.
async fn wait_for_ready_with_progress(
    timeout: std::time::Duration,
    err_path: &std::path::Path,
) -> bool {
    const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    let mut next_progress = PROGRESS_EVERY;
    while start.elapsed() < timeout {
        let slice = PROGRESS_EVERY.min(timeout.saturating_sub(start.elapsed()));
        if wait_for_ready(slice).await {
            return true;
        }
        if start.elapsed() >= next_progress {
            eprintln!(
                "… still waiting ({}s/{}s) — cold model load takes 30-60s; tail {} to watch",
                start.elapsed().as_secs(),
                timeout.as_secs(),
                err_path.display()
            );
            next_progress += PROGRESS_EVERY;
        }
    }
    false
}

/// Poll `/v1/models` until it returns 2xx or `timeout` elapses.
/// Returns true when the daemon is answering, false on timeout.
async fn wait_for_ready(timeout: std::time::Duration) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(r) = client.get("http://127.0.0.1:9741/v1/models").send().await {
            if r.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    false
}
/// Wait for SIGINT (Ctrl-C) or SIGTERM (systemd/launchd shutdown) — the
/// only triggers that end the daemon's run loop. A user-initiated mesh
/// leave no longer exits the process: `POST /v1/mesh/leave` re-creates a
/// solo mesh in-process (`EmbeddedDaemon::leave_to_solo`, rebinding `:9741`)
/// so there's nothing to relaunch. Both signals are a deliberate stop →
/// exit 0; the RSS-hard-limit self-exit in `shutdown_daemon` is the only
/// path that asks the service manager for a relaunch.
pub(super) async fn wait_for_shutdown() {
    // Glassbox: shutdown forensics. A 2026-05-20 incident left the
    // daemon abort-crashing in ggml-metal's `__cxa_finalize_ranges`
    // path with no breadcrumb naming the trigger — was it SIGINT
    // from a stray Ctrl-C, SIGTERM from a peer `svrn daemon
    // stop`, launchd OOM, or something else? Without a log, the
    // post-mortem stalls. Emit the signal source + process context
    // (PID, PPID, peak RSS, jetsam hint) so the next incident
    // surfaces the trigger immediately.
    //
    // We can't get the sender PID — `tokio::signal::unix::signal`
    // abstracts away `SA_SIGINFO`, so siginfo.si_pid is unreachable
    // without dropping to raw `sigaction()`. Logging local context
    // is the practical second-best: if RSS is in the jetsam danger
    // zone (>~24 GiB on a 64 GiB host) and the signal was SIGTERM,
    // the operator gets a strong hint to inspect Console for a
    // "low memory" jetsam event.
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "sigterm handler install failed — falling back to SIGINT-only"
                    );
                    tokio::signal::ctrl_c().await.ok();
                    log_shutdown_context("SIGINT", "fallback");
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => log_shutdown_context("SIGINT", "primary"),
            _ = sigterm.recv() => log_shutdown_context("SIGTERM", "primary"),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!(signal = "ctrl_c", "daemon: shutdown signal received");
    }
}

/// One-line shutdown-receipt log with forensic context. Field names
/// are stable so `grep "daemon: shutdown signal received"` walks the
/// trail across runs.
#[cfg(unix)]
fn log_shutdown_context(signal: &'static str, path: &'static str) {
    let pid = std::process::id();
    let ppid: i64 = unsafe { libc::getppid() } as i64;
    let rss_mb = peak_rss_mb();
    let jetsam_risk = rss_mb.map(|mb| mb >= 24 * 1024).unwrap_or(false);
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if jetsam_risk && signal == "SIGTERM" {
        tracing::warn!(
            signal,
            path,
            pid,
            ppid,
            rss_mb = rss_mb.unwrap_or(0),
            at_unix = now_unix,
            "daemon: shutdown signal received — peak RSS suggests possible jetsam/OOM trigger; inspect Console.app for 'low memory' or 'memorystatus' around this timestamp"
        );
    } else {
        tracing::info!(
            signal,
            path,
            pid,
            ppid,
            rss_mb = rss_mb.unwrap_or(0),
            at_unix = now_unix,
            "daemon: shutdown signal received"
        );
    }
}

/// Peak resident-set size in MiB via `getrusage(RUSAGE_SELF)`.
/// On macOS, `ru_maxrss` is in *bytes*; on Linux it's in *kilobytes*.
/// `None` on platforms / failures.
#[cfg(unix)]
fn peak_rss_mb() -> Option<u64> {
    // SAFETY: getrusage with a properly-zeroed `rusage` struct is safe.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    if rc != 0 {
        return None;
    }
    let raw = ru.ru_maxrss as u64;
    #[cfg(target_os = "macos")]
    {
        Some(raw / (1024 * 1024)) // bytes → MiB
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(raw / 1024) // kilobytes → MiB
    }
}

#[cfg(test)]
mod ready_timeout_tests {
    use super::parse_ready_timeout;
    use std::time::Duration;

    #[test]
    fn unset_falls_back_to_default() {
        assert_eq!(parse_ready_timeout(None), Duration::from_secs(120));
    }

    #[test]
    fn garbage_and_zero_fall_back_to_default() {
        assert_eq!(parse_ready_timeout(Some("soon")), Duration::from_secs(120));
        assert_eq!(parse_ready_timeout(Some("")), Duration::from_secs(120));
        assert_eq!(parse_ready_timeout(Some("0")), Duration::from_secs(120));
        assert_eq!(parse_ready_timeout(Some("-5")), Duration::from_secs(120));
    }

    #[test]
    fn valid_override_is_honored() {
        assert_eq!(parse_ready_timeout(Some("30")), Duration::from_secs(30));
        assert_eq!(parse_ready_timeout(Some(" 300 ")), Duration::from_secs(300));
    }
}
