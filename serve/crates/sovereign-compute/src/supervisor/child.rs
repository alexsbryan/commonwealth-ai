// SPDX-License-Identifier: AGPL-3.0-or-later
//! The child-process half of the [`Supervisor`]: spawning a generation,
//! resolving its health URL, waiting for it to exit, killing it, and
//! keeping its crash log. The restart policy that decides what happens next
//! stays in the parent module.

use super::*;

/// Fork `cmd` from the one thread that lives as long as the process.
///
/// `PR_SET_PDEATHSIG` fires when the THREAD that forked the child exits, not
/// the process. A tokio worker can retire (`block_in_place` hands its thread
/// to the blocking pool, which ends idle threads), and a child it forked is
/// then told its parent died while the parent lives on, and is told nothing
/// when the parent really dies. Measured 2026-10-11: the daemon's router got
/// SIGTERM three times 31s after spawn, when serve's assembly ended, and none
/// when the daemon was SIGKILLed later; it and its models were orphaned.
pub(super) async fn spawn_from_lifetime_thread(cmd: Command) -> std::io::Result<Child> {
    type Request = (
        Command,
        tokio::runtime::Handle,
        oneshot::Sender<std::io::Result<Child>>,
    );
    static SPAWNER: std::sync::OnceLock<std::sync::mpsc::Sender<Request>> =
        std::sync::OnceLock::new();
    let spawner = SPAWNER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("child-spawner".into())
            .spawn(move || {
                // The sender is a static, so this loop, and the thread every
                // child's parent-death signal is tied to, never ends.
                for (mut cmd, runtime, reply) in rx {
                    let _context = runtime.enter();
                    if let Err(unsent) = reply.send(cmd.spawn()) {
                        // The supervisor stopped waiting; a child it never
                        // received is killed on drop, not left running.
                        warn!(
                            spawned = unsent.is_ok(),
                            "child-spawner: the requester is gone; the child is dropped"
                        );
                    }
                }
            })
            .expect("starting the child-spawner thread");
        tx
    });
    let (reply, answer) = oneshot::channel();
    spawner
        .send((cmd, tokio::runtime::Handle::current(), reply))
        .map_err(|_| std::io::Error::other("the child-spawner thread is gone"))?;
    answer
        .await
        .map_err(|_| std::io::Error::other("the child-spawner thread dropped the request"))?
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Signal {
    Term,
    Kill,
}

/// Signal the process group a child leads (`process_group(0)` at spawn makes
/// its pgid its pid). ESRCH, an empty group, is the common case after a
/// clean exit and is not an error.
pub(super) fn signal_group(pid: u32, signal: Signal) {
    #[cfg(unix)]
    if pid != 0 {
        let sig = match signal {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: kill(2) on a negative pid signals that process group; a
        // stale group yields ESRCH.
        let delivered = unsafe { libc::kill(-(pid as libc::pid_t), sig) } == 0;
        tracing::debug!(
            pid,
            ?signal,
            delivered,
            "supervisor: signalled the child's process group"
        );
    }
    #[cfg(not(unix))]
    let _ = (pid, signal);
}

/// Internal reason a supervise-loop iteration ended. Used to decide
/// whether to count toward the crash-loop ceiling and what reason to
/// surface in the next `Restarting` event.
#[derive(Debug)]
pub(super) enum ExitOutcome {
    /// Child returned, regardless of status code. We restart for both
    /// successful exits (e.g. someone sent SIGTERM outside our
    /// control) and crashes — the crash-loop ceiling catches
    /// pathological repeats either way.
    ChildExited { code: Option<i32> },
    /// Calling `Child::wait()` itself errored. Rare; treated as a
    /// crash for restart accounting.
    WaitError(String),
    /// Heartbeat threshold exceeded. The supervisor killed the
    /// child; this counts as a crash.
    HeartbeatFailed { consecutive_failures: u32 },
    /// `request_reconnect()` was called while the child was alive.
    /// Not counted as a crash; clears backoff state.
    ManualReconnect,
    /// `terminate()` was called — the child was SIGTERM'd (then SIGKILL'd
    /// if it overran the grace). Not a crash; the run loop exits for good.
    Terminated,
    /// All references to the supervisor have been dropped (mpsc
    /// channel closed) — the integration layer wants the run loop to
    /// exit. The supervisor kills the child and returns; the outer
    /// `run()` method observes this and breaks out of the loop.
    Shutdown,
}

impl ExitOutcome {
    pub(super) fn reason(&self) -> String {
        match self {
            Self::ChildExited { code: Some(c) } => format!("daemon exited with code {c}"),
            Self::ChildExited { code: None } => "daemon exited by signal".into(),
            Self::WaitError(e) => format!("wait error: {e}"),
            Self::HeartbeatFailed {
                consecutive_failures,
            } => {
                format!("daemon stopped responding ({consecutive_failures} failed heartbeats)")
            }
            Self::ManualReconnect => "manual reconnect".into(),
            Self::Terminated => "graceful terminate".into(),
            Self::Shutdown => "supervisor shutdown".into(),
        }
    }
}

impl Supervisor {
    /// Spawn the child and set up stderr draining + stdout handling.
    /// Returns the child plus, in `StdoutHandshake` mode, a receiver that
    /// fires once the child announces its port (`None` in `Fixed` mode).
    pub(super) async fn spawn_child(
        &self,
    ) -> std::io::Result<(Child, Option<oneshot::Receiver<u16>>)> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.args(&self.config.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The child leads its own process group, so whatever it forks can
        // be signalled with it (`signal_group`), and it is told to die with
        // its parent here rather than trusted to ask for that itself: a
        // child we did not write (llama-server) never does.
        //
        // SIGKILL, not SIGTERM: the kernel sends the parent-death signal on
        // every reparenting, and a multi-threaded parent's exit hands the
        // child from one dying thread to the next, so it can arrive several
        // times. llama-server's handler calls exit() on the second one, from
        // inside the handler, and deadlocks in libc's exit lock (gdb on the
        // orphaned router, 2026-10-11). SIGKILL is idempotent; the router's
        // models then see EOF on stdin and exit. A child of ours that wants
        // a graceful SIGTERM sets it after exec (`child_main`).
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // SAFETY: the closure runs between fork and exec and calls only
            // async-signal-safe functions (prctl, getppid, _exit).
            let parent = unsafe { libc::getpid() };
            unsafe {
                cmd.pre_exec(move || {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    // The parent may have died before prctl took effect.
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
        if let Some(wd) = &self.config.working_dir {
            cmd.current_dir(wd);
        }
        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }
        let mut child = spawn_from_lifetime_thread(cmd).await?;

        // Drain stderr into the ring buffer. Without this the child
        // can block on a full stderr pipe — and we'd lose the crash
        // context anyway.
        if let Some(stderr) = child.stderr.take() {
            let ring = Arc::clone(&self.stderr_ring);
            let cap = self.config.stderr_ring_lines.max(1);
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let mut guard = ring.lock().expect("stderr_ring poisoned");
                    while guard.len() >= cap {
                        guard.pop_front();
                    }
                    guard.push_back(line);
                }
            });
        }

        // Stdout handling depends on the health target. `Fixed`: discard
        // (the daemon writes structured logs to stderr; an undrained
        // stdout pipe would backpressure the child). `StdoutHandshake`:
        // scan for the port announcement, forward it once, keep draining.
        let handshake = matches!(self.config.health, HealthTarget::StdoutHandshake { .. });
        let port_rx = if handshake {
            let (tx, rx) = oneshot::channel();
            if let Some(stdout) = child.stdout.take() {
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stdout).lines();
                    let mut tx = Some(tx);
                    while let Ok(Some(line)) = reader.next_line().await {
                        // Fire the port once, then keep draining so the
                        // child's stdout pipe never backpressures.
                        if tx.is_some() {
                            if let Some(port) = parse_handshake(&line) {
                                if let Some(tx) = tx.take() {
                                    let _ = tx.send(port);
                                }
                            }
                        }
                    }
                });
            }
            Some(rx)
        } else {
            if let Some(stdout) = child.stdout.take() {
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stdout).lines();
                    while matches!(reader.next_line().await, Ok(Some(_))) {}
                });
            }
            None
        };

        Ok((child, port_rx))
    }

    /// Resolve the child's health URL. `Fixed` returns immediately;
    /// `StdoutHandshake` awaits the port announcement (emitting
    /// [`SupervisorState::Warming`]) up to `handshake_deadline`, or
    /// returns `None` if the child dies or never announces (→ crash
    /// accounting + backoff in `run()`).
    pub(super) async fn resolve_health_url(
        &self,
        port_rx: Option<oneshot::Receiver<u16>>,
        pid: u32,
        child: &mut Child,
    ) -> Option<String> {
        match &self.config.health {
            HealthTarget::Fixed(url) => Some(url.clone()),
            HealthTarget::StdoutHandshake {
                health_path,
                handshake_deadline,
            } => {
                let rx = port_rx?;
                let port = tokio::select! {
                    r = rx => match r {
                        Ok(port) => port,
                        Err(_) => {
                            warn!(pid, "supervisor: handshake channel closed before a port arrived");
                            return None;
                        }
                    },
                    _ = tokio::time::sleep(*handshake_deadline) => {
                        warn!(
                            pid,
                            deadline_secs = handshake_deadline.as_secs(),
                            "supervisor: compute child did not announce a port in time; killing"
                        );
                        let _ = child.start_kill();
                        return None;
                    }
                    exit = child.wait() => {
                        warn!(pid, ?exit, "supervisor: compute child exited before announcing a port");
                        return None;
                    }
                };
                info!(
                    pid,
                    port, "supervisor: compute child announced port; warming"
                );
                self.broadcast(SupervisorState::Warming { pid, port });
                Some(format!("http://127.0.0.1:{port}{health_path}"))
            }
        }
    }

    /// Graceful terminate: best-effort SIGTERM (so a compute child can
    /// `fast_exit_skip_destructors`), then SIGKILL if it overruns the
    /// grace. Reaps the child so no zombie remains.
    pub(super) async fn graceful_kill(child: &mut Child, pid: u32, grace: Duration) {
        signal_group(pid, Signal::Term);
        match tokio::time::timeout(grace, child.wait()).await {
            Ok(_) => info!(pid, "supervisor: child exited gracefully after SIGTERM"),
            Err(_) => {
                warn!(
                    pid,
                    "supervisor: child overran terminate grace; sending SIGKILL"
                );
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
    }

    pub(super) async fn supervise_until_exit(
        &self,
        child: &mut Child,
        pid: u32,
        spawn_instant: Instant,
        health_url: &str,
        http: &reqwest::Client,
        reconnect_rx: &mut mpsc::UnboundedReceiver<()>,
        terminate_rx: &mut mpsc::UnboundedReceiver<()>,
    ) -> (ExitOutcome, GenerationCost) {
        let mut consecutive_failures: u32 = 0;
        let mut ticker = tokio::time::interval(self.config.heartbeat_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick fires immediately; we want to give the child a
        // moment to bind before probing, otherwise the very first
        // probe always reads as a failure.
        ticker.tick().await;
        let mut last_health: Option<bool> = None;
        // When the child answered its FIRST probe. `Some` doubles as
        // "ever healthy" — once set, the startup grace no longer applies
        // (a later stall is a real failure, not slow loading) — and it
        // is the origin for both halves of `GenerationCost`.
        let mut first_healthy: Option<Instant> = None;
        let cost = |first_healthy: Option<Instant>| GenerationCost {
            load: first_healthy.map(|t| t.duration_since(spawn_instant)),
            healthy_for: first_healthy.map(|t| t.elapsed()).unwrap_or_default(),
        };

        loop {
            tokio::select! {
                exit = child.wait() => {
                    let outcome = match exit {
                        Ok(status) => ExitOutcome::ChildExited { code: status.code() },
                        Err(e) => ExitOutcome::WaitError(e.to_string()),
                    };
                    return (outcome, cost(first_healthy));
                }
                _ = terminate_rx.recv() => {
                    info!(pid, "supervisor: graceful terminate requested");
                    Self::graceful_kill(child, pid, self.config.terminate_grace).await;
                    return (ExitOutcome::Terminated, cost(first_healthy));
                }
                msg = reconnect_rx.recv() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    if msg.is_none() {
                        // All `Supervisor` references dropped → the
                        // integration layer wants us to stop. Returning
                        // `Shutdown` (vs. `ManualReconnect`) is what
                        // keeps the outer loop from spinning on a
                        // closed channel — see `run()`.
                        info!(pid, "supervisor: reconnect channel closed; shutting down");
                        return (ExitOutcome::Shutdown, cost(first_healthy));
                    }
                    info!(pid, "supervisor: reconnect requested; killing child");
                    return (ExitOutcome::ManualReconnect, cost(first_healthy));
                }
                _ = ticker.tick() => {
                    let ok = http.get(health_url).send().await
                        .map(|r| r.status().is_success())
                        .unwrap_or(false);
                    if ok {
                        if last_health != Some(true) {
                            let since = sovereign_time::unix_now_u64();
                            self.broadcast(SupervisorState::Healthy { pid, since_unix: since });
                        }
                        consecutive_failures = 0;
                        last_health = Some(true);
                        if first_healthy.is_none() {
                            let now = Instant::now();
                            first_healthy = Some(now);
                            info!(
                                pid,
                                load_secs = now.duration_since(spawn_instant).as_secs_f32(),
                                "supervisor: child answered its first probe — load cost recorded"
                            );
                        }
                    } else if first_healthy.is_none()
                        && spawn_instant.elapsed() < self.config.ready_deadline
                    {
                        // Startup grace: the child is still loading its
                        // model. A failed probe here is expected — don't
                        // count it toward the crash threshold. `Warming`
                        // was already broadcast when the port arrived.
                        debug!(
                            pid,
                            elapsed_ms = spawn_instant.elapsed().as_millis(),
                            "supervisor: child still warming (startup grace)"
                        );
                        last_health = Some(false);
                    } else {
                        consecutive_failures += 1;
                        if consecutive_failures >= self.config.heartbeat_failure_threshold {
                            warn!(
                                pid,
                                consecutive_failures,
                                "supervisor: heartbeat threshold exceeded; killing child"
                            );
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            return (
                                ExitOutcome::HeartbeatFailed { consecutive_failures },
                                cost(first_healthy),
                            );
                        }
                        if last_health != Some(false) || consecutive_failures > 1 {
                            self.broadcast(SupervisorState::Unhealthy {
                                pid,
                                consecutive_failures,
                            });
                        }
                        last_health = Some(false);
                    }
                }
            }
        }
    }

    pub(super) async fn persist_crash_log(
        &self,
        pid: u32,
        outcome: &ExitOutcome,
    ) -> Option<PathBuf> {
        if matches!(
            outcome,
            ExitOutcome::ManualReconnect | ExitOutcome::Shutdown | ExitOutcome::Terminated
        ) {
            return None;
        }
        let ts = sovereign_time::unix_now_u64();
        if let Err(e) = tokio::fs::create_dir_all(&self.config.crash_log_dir).await {
            warn!(
                error = %e,
                path = %self.config.crash_log_dir.display(),
                "supervisor: cannot create crash log dir"
            );
            return None;
        }
        let path = self.config.crash_log_dir.join(format!("daemon-{ts}.log"));
        let lines = self.stderr_tail();
        let body = format!(
            "# sovereign daemon crash report\n\
             # pid: {pid}\n\
             # exit: {}\n\
             # captured: {} stderr lines\n\
             # captured_at_unix: {ts}\n\n{}\n",
            outcome.reason(),
            lines.len(),
            lines.join("\n")
        );
        match tokio::fs::write(&path, body).await {
            Ok(()) => {
                info!(path = %path.display(), "supervisor: crash log written");
                Some(path)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "supervisor: crash log write failed"
                );
                None
            }
        }
    }
}
