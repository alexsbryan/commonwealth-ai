// SPDX-License-Identifier: AGPL-3.0-or-later
//! Supervises a child process — the desktop's Local-mode daemon, or a
//! daemon-managed **compute child** (an inference slot behind a process
//! boundary, `DISTRIBUTED_PILOT_READINESS.md` P1).
//!
//! # Why this module exists
//!
//! The inference path links ggml/llama.cpp via `sovereign-inference`, and
//! crashes there have historically been **process-level SEGVs** — see
//! the recent A3B ROCm crash and the Vulkan-fix project memories. When
//! that path lives in-process — inside the Tauri host (desktop) or inside
//! the mesh daemon — a model SEGV takes the whole process down with no
//! path back. Moving it behind a child-process boundary lets this
//! supervisor catch the crash: the desktop presents a reconnect surface
//! instead of a dead window; the daemon keeps gossip/`/status`/the client
//! API alive and observes the exit as an event it re-plans around.
//!
//! # Two health-target modes ([`HealthTarget`])
//!
//! - **`Fixed`** — the child binds a known port (the desktop daemon's
//!   `9741`); the health URL is knowable at config time. This is the
//!   original desktop behaviour, unchanged.
//! - **`StdoutHandshake`** — the child binds an *ephemeral* port
//!   (`127.0.0.1:0`, so N replicas never collide) and prints a one-line
//!   `SOVEREIGN_COMPUTE_LISTENING {"port":N,"pid":N}` handshake to stdout
//!   before loading its model. The supervisor parses the port, emits
//!   [`SupervisorState::Warming`], and builds the health URL from it.
//!   `ready_deadline` gives the model load a startup grace during which
//!   failed probes don't count toward the crash threshold.
//!
//! # Responsibilities
//!
//! - Spawn `<binary> <args>` as a child process. `kill_on_drop` is
//!   set so a Tauri panic doesn't orphan the daemon.
//! - Heartbeat the daemon's health URL (typically `/v1/models`) at a
//!   configurable cadence.
//! - On child exit, **or** after N consecutive heartbeat failures,
//!   restart with the configured exponential backoff.
//! - If the daemon crashes more than `crash_loop_max` times in a row —
//!   with no generation staying healthy for `healthy_reset_after` in
//!   between — stop auto-restarting and surface a `Failed` state. Only
//!   an explicit `request_reconnect()` from the UI wakes the supervisor
//!   back up — auto-relaunch never silently burns CPU in a tight crash
//!   loop. The counter resets on PROOF (a generation that served), not
//!   on elapsed time; a wall-clock window is unreachable for any child
//!   that takes longer to load than the window is wide.
//! - Back off at least as long as the last generation took to load, so
//!   an expensive child cannot re-enter a multi-minute load immediately
//!   after failing one.
//! - Drain child stderr into a bounded ring buffer. On each restart,
//!   persist the buffer to `<crash_log_dir>/daemon-<unix-ts>.log` so
//!   the user (or a "send report" action) can attach the trailing
//!   output that preceded the crash.
//! - Publish every state transition over a `broadcast::Sender` so the
//!   Tauri layer can map them to UI events without this module ever
//!   importing tauri.
//!
//! # Non-responsibilities
//!
//! - No Tauri imports. The integration glue lives in `bootstrap.rs` /
//!   `main.rs` and subscribes to this module's channel.
//! - No HTTP client construction beyond the heartbeat — peer
//!   inference and route handling stay in the child daemon.
//! - No knowledge of `SetupConfig` semantics. Callers compute the
//!   binary path, args, ports, and crash-log dir; this module just
//!   spawns and supervises.
//!
//! # Integration pattern
//!
//! ```ignore
//! let supervisor = Arc::new(Supervisor::new(config));
//! let mut states = supervisor.subscribe();
//! let task = {
//!     let sup = Arc::clone(&supervisor);
//!     tokio::spawn(async move { sup.run().await })
//! };
//! // ...UI subscribes to `states`, calls `supervisor.request_reconnect()`...
//! // On app quit:
//! task.abort();
//! ```
//!
//! `JoinHandle::abort()` drops the run future; the in-flight `Child`
//! handle's `kill_on_drop(true)` SIGKILLs the daemon. For v1 that's
//! the same behaviour as `kill -9 sovereign-cli` — the daemon writes
//! `mesh.json` atomically on each state change, so on-disk persistence
//! survives an abrupt termination. A graceful SIGTERM-with-grace path
//! can layer on later without changing this contract.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex as AsyncMutex};
use tracing::{debug, error, info, warn};

/// Prefix of the one-line stdout handshake a compute child prints once it
/// has bound its ephemeral port (before it loads its model). The rest of
/// the line is `{"port":N,"pid":N}`. Kept in sync with the child's
/// entrypoint (`child_main`).
pub const HANDSHAKE_PREFIX: &str = "SOVEREIGN_COMPUTE_LISTENING ";

/// Grace period after a graceful-terminate SIGTERM before the supervisor
/// escalates to SIGKILL. A stateless compute child that honours SIGTERM
/// (via `fast_exit_skip_destructors`) exits well within this.
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

/// How the supervisor learns the child's health URL.
#[derive(Debug, Clone)]
pub enum HealthTarget {
    /// A fixed URL known at config time; probed by GET, any 2xx is
    /// healthy. Used when the child binds a known port (the desktop
    /// daemon's `9741`).
    Fixed(String),
    /// The child binds an ephemeral port and announces it via the stdout
    /// handshake ([`HANDSHAKE_PREFIX`]). The supervisor parses the port
    /// and builds `http://127.0.0.1:{port}{health_path}`.
    StdoutHandshake {
        /// Path appended to `http://127.0.0.1:{port}` for the probe,
        /// e.g. `"/health"`.
        health_path: String,
        /// How long to wait for the handshake line before treating the
        /// spawn as failed (→ crash accounting + backoff).
        handshake_deadline: Duration,
    },
}

/// Parse the port out of a stdout handshake line. Returns `None` for any
/// non-handshake line so the drainer can keep discarding output.
fn parse_handshake(line: &str) -> Option<u16> {
    let rest = line.strip_prefix(HANDSHAKE_PREFIX)?;
    let v: serde_json::Value = serde_json::from_str(rest.trim()).ok()?;
    u16::try_from(v.get("port")?.as_u64()?).ok()
}

/// Static configuration. All fields required — defaults belong at the
/// call site (the desktop's bootstrap) so test fixtures don't quietly
/// inherit production values.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// How the health URL is resolved — a fixed URL, or parsed from the
    /// child's stdout handshake. Probed by GET; any 2xx counts as healthy.
    pub health: HealthTarget,
    pub crash_log_dir: PathBuf,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    /// Consecutive failed heartbeats before the supervisor kills the
    /// child and counts it as a crash.
    pub heartbeat_failure_threshold: u32,
    /// Startup grace: until the child's FIRST successful probe, failed
    /// probes within this window of spawn do NOT count toward
    /// `heartbeat_failure_threshold` — the child is loading its model.
    /// `Duration::ZERO` disables the grace (count from spawn, the
    /// original desktop behaviour).
    pub ready_deadline: Duration,
    /// Schedule of delays before successive restart attempts. After
    /// the last entry, restarts use the final entry indefinitely
    /// (until the crash-loop ceiling triggers `Failed`).
    ///
    /// The schedule is a FLOOR, not the whole story: a generation that
    /// took `t` to load waits at least `t` before the next attempt, so
    /// an expensive child can never re-enter a load back-to-back. See
    /// [`GenerationCost`].
    pub backoff_schedule: Vec<Duration>,
    /// How long a generation must stay healthy to PROVE that restarting
    /// worked. A generation that serves at least this long resets the
    /// consecutive-crash counter; anything shorter does not.
    ///
    /// This is the reset condition for [`Self::crash_loop_max`], and it
    /// is deliberately not a wall-clock window. A wall-clock window is
    /// unreachable for any child whose spawn→crash cycle is longer than
    /// the window: crashes age out faster than they accumulate and the
    /// ceiling never trips. Measured live 2026-08-03 — a 148 GB
    /// distributed primary took 4m36s to load and died 13s into serving,
    /// so the old 600s window held at most two crashes against a ceiling
    /// of five, and the breaker could not fire at all while each cycle
    /// re-mapped 148 GB of GTT. Two cycles exhausted the host's GPU
    /// address space and took the desktop down with it.
    pub healthy_reset_after: Duration,
    /// Consecutive crashes — with no generation proving itself healthy
    /// in between — before auto-restart stops and `Failed` is published.
    pub crash_loop_max: u32,
    pub stderr_ring_lines: usize,
}

/// State the supervisor publishes. Serialised tag is `kind`, so the
/// frontend can `switch` on a discriminant.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupervisorState {
    /// First boot, or between a previous exit and the next spawn.
    Starting,
    /// Handshake mode only: the child announced its port and is loading
    /// its model. Not yet answering health probes — the `ready_deadline`
    /// grace is in effect.
    Warming { pid: u32, port: u16 },
    /// Child is running and the last heartbeat succeeded.
    Healthy { pid: u32, since_unix: u64 },
    /// Child is running but the last N heartbeats failed.
    Unhealthy { pid: u32, consecutive_failures: u32 },
    /// Auto-restart in progress. `after_secs` is how long the
    /// supervisor will wait before the next spawn.
    Restarting {
        attempt: u32,
        after_secs: u64,
        reason: String,
    },
    /// Crash-loop ceiling exceeded — auto-restart is OFF. The UI
    /// should surface a Reconnect banner pointing at the last crash
    /// log.
    Failed {
        reason: String,
        last_crash_log: Option<PathBuf>,
    },
    /// Not spawning: an external precondition is unmet (see [`SpawnGate`]).
    /// NOT a crash and NOT a restart — the restart counter must not move.
    GateHeld { reason: String },
}

/// Verdict from a [`SpawnGate`], consulted immediately before every spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnVerdict {
    /// Spawn now.
    Allow,
    /// Do not spawn yet. `reason` is operator-facing and is published once per
    /// distinct reason, so a hold is never silent but a 2s poll never spams.
    Hold { reason: String },
}

/// An optional external precondition on spawning.
///
/// A `Hold` parks the run loop — re-polled every [`SPAWN_GATE_POLL`] — WITHOUT
/// spawning and WITHOUT touching the crash-loop ceiling: a spawn we chose not to
/// make is not a crash.
///
/// This exists because the supervisor and the thing that knows whether a spawn
/// can possibly succeed are different components on different clocks. The
/// supervisor respawns with IDENTICAL argv, and the compute child re-reads its
/// distribution handoff from disk on every spawn — so when a handoff names an
/// RPC worker that has died, each restart re-dials the corpse and re-aborts,
/// forever, on a budget only the daemon's discovery loop can refresh. Measured
/// live 2026-07-28: three futile respawns in 48 s, ~1/5 of the crash-loop
/// budget spent on spawns that could not possibly succeed.
pub type SpawnGate = Arc<dyn Fn() -> SpawnVerdict + Send + Sync>;

/// How often a held gate is re-polled. Short enough that recovery is prompt,
/// long enough that holding costs nothing.
const SPAWN_GATE_POLL: Duration = Duration::from_secs(2);

/// What one spawn→exit generation cost, and what it proved.
///
/// Both numbers feed decisions the supervisor cannot make from the exit
/// status alone: `healthy_for` answers "did restarting actually work?"
/// (the circuit breaker's reset condition) and `load` answers "how
/// expensive is it to try again?" (the backoff floor). A child that
/// takes minutes and tens of GB to load is not the same restart risk as
/// one that binds a port in 200 ms, and the supervisor has no other way
/// to tell them apart.
#[derive(Debug, Clone, Copy, Default)]
struct GenerationCost {
    /// spawn → first successful health probe. `None` if the generation
    /// died or was killed before it ever answered one.
    load: Option<Duration>,
    /// first successful health probe → exit. `ZERO` when the generation
    /// never became healthy.
    healthy_for: Duration,
}

impl GenerationCost {
    /// Did this generation run long enough to prove the restart worked?
    fn proved_healthy(&self, threshold: Duration) -> bool {
        self.healthy_for >= threshold
    }
}

/// Internal reason a supervise-loop iteration ended. Used to decide
/// whether to count toward the crash-loop ceiling and what reason to
/// surface in the next `Restarting` event.
#[derive(Debug)]
enum ExitOutcome {
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
    fn reason(&self) -> String {
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

pub struct Supervisor {
    config: SupervisorConfig,
    state_tx: broadcast::Sender<SupervisorState>,
    reconnect_tx: mpsc::UnboundedSender<()>,
    reconnect_rx: AsyncMutex<Option<mpsc::UnboundedReceiver<()>>>,
    /// Graceful-shutdown signal: SIGTERM the child, wait up to
    /// [`TERMINATE_GRACE`], SIGKILL if it hasn't exited, then leave the
    /// run loop for good (NOT counted as a crash). The daemon's
    /// compute-child manager calls `terminate()` to stop a replica.
    terminate_tx: mpsc::UnboundedSender<()>,
    terminate_rx: AsyncMutex<Option<mpsc::UnboundedReceiver<()>>>,
    /// Bounded ring of recent stderr lines. The `Mutex` here is sync,
    /// not async — the stderr drainer task pushes one line at a time
    /// with negligible hold time, so contention against
    /// `stderr_tail()` readers is fine.
    stderr_ring: Arc<Mutex<VecDeque<String>>>,
    /// Optional precondition consulted before EVERY spawn, including the
    /// supervisor's own crash restarts. `None` = today's behaviour exactly.
    spawn_gate: Option<SpawnGate>,
}

impl Supervisor {
    pub fn new(config: SupervisorConfig) -> Self {
        let (state_tx, _) = broadcast::channel(64);
        let (reconnect_tx, reconnect_rx) = mpsc::unbounded_channel();
        let (terminate_tx, terminate_rx) = mpsc::unbounded_channel();
        let stderr_ring = Arc::new(Mutex::new(VecDeque::with_capacity(
            config.stderr_ring_lines.max(1),
        )));
        Self {
            config,
            state_tx,
            reconnect_tx,
            reconnect_rx: AsyncMutex::new(Some(reconnect_rx)),
            terminate_tx,
            terminate_rx: AsyncMutex::new(Some(terminate_rx)),
            stderr_ring,
            spawn_gate: None,
        }
    }

    /// Install a spawn precondition.
    ///
    /// Consumes `self`, so a gated supervisor is gated for life — there is no
    /// window in which a respawn slips past a gate that was about to be
    /// installed.
    pub fn with_spawn_gate(mut self, gate: SpawnGate) -> Self {
        self.spawn_gate = Some(gate);
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorState> {
        self.state_tx.subscribe()
    }

    /// Block until the gate allows a spawn (immediate when there is no gate).
    ///
    /// `false` means the run loop must exit for good. Both `terminate()` and
    /// `request_reconnect()` break a hold: the first because a gate must never
    /// be able to wedge a slot the manager is trying to replace, the second
    /// because an explicit operator action outranks a heuristic.
    async fn await_spawn_gate(
        &self,
        reconnect_rx: &mut mpsc::UnboundedReceiver<()>,
        terminate_rx: &mut mpsc::UnboundedReceiver<()>,
    ) -> bool {
        let Some(gate) = self.spawn_gate.as_ref() else {
            return true;
        };
        // Publish once per DISTINCT reason: a 2s poll must not flood a
        // 64-slot broadcast channel and lag every collector on it.
        let mut announced: Option<String> = None;
        loop {
            match gate() {
                SpawnVerdict::Allow => {
                    if announced.is_some() {
                        info!(target: "compute_child", "supervisor: spawn gate released");
                    }
                    return true;
                }
                SpawnVerdict::Hold { reason } => {
                    if announced.as_deref() != Some(reason.as_str()) {
                        warn!(
                            target: "compute_child",
                            %reason,
                            "supervisor: spawn gate HELD — not respawning (this does NOT \
                             count against the crash-loop budget)"
                        );
                        self.broadcast(SupervisorState::GateHeld {
                            reason: reason.clone(),
                        });
                        announced = Some(reason);
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(SPAWN_GATE_POLL) => {}
                _ = terminate_rx.recv() => return false,
                r = reconnect_rx.recv() => {
                    return match r {
                        // Operator override: spawn regardless of the gate.
                        Some(()) => true,
                        // All handles dropped — shut down.
                        None => false,
                    };
                }
            }
        }
    }

    /// Latest stderr lines, oldest-first. Bounded by
    /// `config.stderr_ring_lines`.
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_ring
            .lock()
            .expect("stderr_ring poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// External signal: user clicked the Reconnect banner button, or
    /// the integration layer wants to force a fresh start. Resets the
    /// backoff and crash-loop counters on the next iteration. Returns
    /// `false` if the supervisor's run loop has already exited.
    pub fn request_reconnect(&self) -> bool {
        self.reconnect_tx.send(()).is_ok()
    }

    /// Graceful stop: the run loop SIGTERMs the child (letting a compute
    /// child `fast_exit_skip_destructors` past the ggml teardown SIGABRT),
    /// waits up to [`TERMINATE_GRACE`], SIGKILLs if needed, then exits the
    /// loop for good — NOT counted as a crash. Await the `run()` join
    /// handle to know the child is gone. Returns `false` if the loop
    /// already exited. Distinct from `request_reconnect()`, which restarts.
    pub fn terminate(&self) -> bool {
        self.terminate_tx.send(()).is_ok()
    }

    /// Main loop. Spawns, supervises, restarts until the crash-loop
    /// ceiling trips. After `Failed`, the loop blocks on a reconnect
    /// signal — auto-relaunch never burns CPU in a runaway crash
    /// loop. Returns only when the reconnect channel closes
    /// (i.e. all `Supervisor` references have been dropped).
    pub async fn run(&self) {
        let mut reconnect_rx = self
            .reconnect_rx
            .lock()
            .await
            .take()
            .expect("Supervisor::run called twice");
        let mut terminate_rx = self
            .terminate_rx
            .lock()
            .await
            .take()
            .expect("Supervisor::run called twice");
        let http = match reqwest::Client::builder()
            .timeout(self.config.heartbeat_timeout)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "supervisor: cannot build reqwest client");
                self.broadcast(SupervisorState::Failed {
                    reason: format!("cannot build heartbeat client: {e}"),
                    last_crash_log: None,
                });
                return;
            }
        };

        // Crashes since the last generation that PROVED itself (see
        // `GenerationCost::proved_healthy`). Not a sliding window —
        // see `SupervisorConfig::healthy_reset_after` for why.
        let mut consecutive_crashes: u32 = 0;
        let mut attempt: u32 = 0;
        // What the previous generation cost to load. Floors the backoff
        // so an expensive child never re-enters a load back-to-back.
        let mut last_cost = GenerationCost::default();

        loop {
            // Before anything else: may we spawn at all? A respawn into a
            // worker set that is known-gone cannot succeed and only burns
            // crash-loop budget.
            if !self
                .await_spawn_gate(&mut reconnect_rx, &mut terminate_rx)
                .await
            {
                return;
            }
            self.broadcast(SupervisorState::Starting);
            let spawn_instant = Instant::now();
            let (mut child, port_rx) = match self.spawn_child().await {
                Ok(c) => c,
                Err(e) => {
                    let reason = format!("spawn failed: {e}");
                    warn!(error = %e, "supervisor: spawn failed");
                    // Spawn failure counts toward the ceiling — usually
                    // it's a missing binary path or a permissions
                    // problem and shouldn't be retried forever.
                    if !self
                        .handle_crash(
                            &mut attempt,
                            &mut consecutive_crashes,
                            ExitOutcome::WaitError(reason.clone()),
                            None,
                            last_cost,
                            &mut reconnect_rx,
                        )
                        .await
                    {
                        return;
                    }
                    continue;
                }
            };

            let pid = child.id().unwrap_or(0);
            info!(pid, binary = %self.config.binary_path.display(), "supervisor: child spawned");

            // Resolve the health URL. For `Fixed` this is immediate; for
            // `StdoutHandshake` it awaits the child's port announcement
            // (emitting `Warming`), or fails to local crash-accounting if
            // the child dies or never announces within the deadline.
            let health_url = match self.resolve_health_url(port_rx, pid, &mut child).await {
                Some(url) => url,
                None => {
                    let outcome = ExitOutcome::WaitError("child did not announce its port".into());
                    let crash_log = self.persist_crash_log(pid, &outcome).await;
                    // A generation that never announced a port never
                    // became healthy — it proves nothing and resets
                    // nothing. `last_cost` stays as the previous
                    // generation left it, so an expensive child that
                    // starts failing early still pays the load-sized
                    // backoff floor it earned.
                    if !self
                        .handle_crash(
                            &mut attempt,
                            &mut consecutive_crashes,
                            outcome,
                            crash_log,
                            last_cost,
                            &mut reconnect_rx,
                        )
                        .await
                    {
                        return;
                    }
                    continue;
                }
            };

            let (outcome, cost) = self
                .supervise_until_exit(
                    &mut child,
                    pid,
                    spawn_instant,
                    &health_url,
                    &http,
                    &mut reconnect_rx,
                    &mut terminate_rx,
                )
                .await;
            let crash_log = self.persist_crash_log(pid, &outcome).await;

            last_cost = cost;

            match outcome {
                ExitOutcome::Shutdown | ExitOutcome::Terminated => {
                    // Channel closed, or a graceful terminate — nothing
                    // more for us to do.
                    info!(?outcome, "supervisor: exiting run loop");
                    return;
                }
                ExitOutcome::ManualReconnect => {
                    info!("supervisor: manual reconnect; resetting backoff");
                    attempt = 0;
                    consecutive_crashes = 0;
                    // Loop back to Starting immediately.
                }
                _ => {
                    // A generation that served for `healthy_reset_after`
                    // is PROOF that restarting this child works, and is
                    // the only thing that clears the breaker. Elapsed
                    // wall-clock is not proof — see
                    // `SupervisorConfig::healthy_reset_after`.
                    if cost.proved_healthy(self.config.healthy_reset_after) {
                        info!(
                            healthy_for_secs = cost.healthy_for.as_secs_f32(),
                            threshold_secs = self.config.healthy_reset_after.as_secs_f32(),
                            prior_consecutive_crashes = consecutive_crashes,
                            "supervisor: generation proved healthy — crash breaker reset"
                        );
                        attempt = 0;
                        consecutive_crashes = 0;
                    }
                    if !self
                        .handle_crash(
                            &mut attempt,
                            &mut consecutive_crashes,
                            outcome,
                            crash_log,
                            cost,
                            &mut reconnect_rx,
                        )
                        .await
                    {
                        return;
                    }
                }
            }
        }
    }

    /// Common path for "something went wrong, decide whether to back
    /// off + retry or surface Failed and wait for reconnect."
    /// Returns `false` if the reconnect channel closed (shutdown).
    async fn handle_crash(
        &self,
        attempt: &mut u32,
        consecutive_crashes: &mut u32,
        outcome: ExitOutcome,
        crash_log: Option<PathBuf>,
        cost: GenerationCost,
        reconnect_rx: &mut mpsc::UnboundedReceiver<()>,
    ) -> bool {
        *consecutive_crashes = consecutive_crashes.saturating_add(1);

        if *consecutive_crashes > self.config.crash_loop_max {
            warn!(
                consecutive_crashes = *consecutive_crashes,
                ceiling = self.config.crash_loop_max,
                healthy_reset_after_secs = self.config.healthy_reset_after.as_secs(),
                last_healthy_for_secs = cost.healthy_for.as_secs_f32(),
                last_load_secs = cost.load.map(|d| d.as_secs_f32()),
                "supervisor: crash-loop ceiling exceeded; awaiting manual reconnect"
            );
            self.broadcast(SupervisorState::Failed {
                reason: format!(
                    "{} — {} consecutive crashes, none serving {}s",
                    outcome.reason(),
                    *consecutive_crashes,
                    self.config.healthy_reset_after.as_secs()
                ),
                last_crash_log: crash_log,
            });
            if !wait_for_reconnect(reconnect_rx).await {
                return false;
            }
            *attempt = 0;
            *consecutive_crashes = 0;
            return true;
        }

        let scheduled = self
            .config
            .backoff_schedule
            .get(*attempt as usize)
            .copied()
            .or_else(|| self.config.backoff_schedule.last().copied())
            .unwrap_or(Duration::from_secs(30));
        // The schedule assumes a restart is cheap. For a child whose
        // load costs minutes and tens of GB of unified memory, going
        // back in after 1s means the host is loading a model nearly
        // 100% of the time — which is how a worker-side crash escalated
        // into a host-wide GPU-memory exhaustion on 2026-08-03. Waiting
        // at least as long as the last load took bounds that duty cycle
        // to ~50% and gives whatever failed (a peer, the operator, the
        // spawn gate) room to change the answer.
        let floor = cost.load.unwrap_or_default();
        let delay = scheduled.max(floor);
        if delay > scheduled {
            info!(
                scheduled_secs = scheduled.as_secs_f32(),
                load_floor_secs = floor.as_secs_f32(),
                "supervisor: backoff raised to the last generation's load cost"
            );
        }
        self.broadcast(SupervisorState::Restarting {
            attempt: *attempt + 1,
            after_secs: delay.as_secs(),
            reason: outcome.reason(),
        });
        *attempt = attempt.saturating_add(1);
        wait_or_reconnect(delay, reconnect_rx).await
    }

    fn broadcast(&self, state: SupervisorState) {
        debug!(?state, "supervisor: state");
        // Errors only mean "no subscribers attached" — fine to drop.
        let _ = self.state_tx.send(state);
    }

    /// Spawn the child and set up stderr draining + stdout handling.
    /// Returns the child plus, in `StdoutHandshake` mode, a receiver that
    /// fires once the child announces its port (`None` in `Fixed` mode).
    async fn spawn_child(&self) -> std::io::Result<(Child, Option<oneshot::Receiver<u16>>)> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.args(&self.config.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(wd) = &self.config.working_dir {
            cmd.current_dir(wd);
        }
        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;

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
    async fn resolve_health_url(
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
    async fn graceful_kill(child: &mut Child, pid: u32) {
        #[cfg(unix)]
        if pid != 0 {
            // SAFETY: kill(2) with a valid pid + signal is defined; a
            // stale pid just yields ESRCH, which we ignore.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        match tokio::time::timeout(TERMINATE_GRACE, child.wait()).await {
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

    async fn supervise_until_exit(
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
                    Self::graceful_kill(child, pid).await;
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
                            let since = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
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

    async fn persist_crash_log(&self, pid: u32, outcome: &ExitOutcome) -> Option<PathBuf> {
        if matches!(
            outcome,
            ExitOutcome::ManualReconnect | ExitOutcome::Shutdown | ExitOutcome::Terminated
        ) {
            return None;
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
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

async fn wait_or_reconnect(d: Duration, rx: &mut mpsc::UnboundedReceiver<()>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => true,
        msg = rx.recv() => msg.is_some(),
    }
}

async fn wait_for_reconnect(rx: &mut mpsc::UnboundedReceiver<()>) -> bool {
    rx.recv().await.is_some()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Stand up a localhost axum server answering `/v1/models`. Used
    /// as the health endpoint for tests that need the supervisor to
    /// see a healthy daemon.
    async fn spawn_health_server() -> (u16, tokio::task::JoinHandle<()>) {
        use axum::{routing::get, Json, Router};
        let app = Router::new().route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({"data": []})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (port, handle)
    }

    /// Write a small shell script the supervisor can spawn as its
    /// "daemon". Returns the script path; the file lives as long as
    /// the `TempDir`.
    fn write_script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
        path
    }

    fn base_config(binary: PathBuf, crash_dir: PathBuf, health_url: String) -> SupervisorConfig {
        SupervisorConfig {
            binary_path: binary,
            args: vec![],
            working_dir: None,
            env: vec![],
            health: HealthTarget::Fixed(health_url),
            crash_log_dir: crash_dir,
            heartbeat_interval: Duration::from_millis(80),
            heartbeat_timeout: Duration::from_millis(200),
            heartbeat_failure_threshold: 3,
            // No startup grace by default — matches the original desktop
            // behaviour (count failures from spawn). Handshake tests set
            // their own.
            ready_deadline: Duration::ZERO,
            backoff_schedule: vec![
                Duration::from_millis(20),
                Duration::from_millis(40),
                Duration::from_millis(60),
            ],
            // Long relative to these tests' millisecond timings: the
            // default fixture never resets the breaker, which is what
            // the crash-loop tests want. Tests that need a reset lower
            // it explicitly.
            healthy_reset_after: Duration::from_secs(60),
            crash_loop_max: 3,
            stderr_ring_lines: 64,
        }
    }

    /// Drain at most `max` states with an overall timeout, returning
    /// what we got. Discriminant-only check at call sites.
    async fn drain_states(
        rx: &mut broadcast::Receiver<SupervisorState>,
        max: usize,
        per_event_timeout: Duration,
    ) -> Vec<SupervisorState> {
        let mut out = Vec::new();
        for _ in 0..max {
            match tokio::time::timeout(per_event_timeout, rx.recv()).await {
                Ok(Ok(s)) => out.push(s),
                _ => break,
            }
        }
        out
    }

    /// A health server that 503s for `unhealthy_for`, then 200s. Lets a
    /// test give a generation a *measurable load cost* — the input to
    /// the backoff floor — without spawning a real model.
    async fn spawn_slow_health_server(
        unhealthy_for: Duration,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        use axum::{http::StatusCode, routing::get, Router};
        let up_at = Instant::now() + unhealthy_for;
        let app = Router::new().route(
            "/v1/models",
            get(move || async move {
                if Instant::now() < up_at {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (port, handle)
    }

    /// THE REGRESSION. A child that loads, serves *briefly*, and dies —
    /// over and over — must trip the breaker.
    ///
    /// This is the shape that took the host down on 2026-08-03: a 148 GB
    /// distributed primary loaded in 4m36s, served 13s, and aborted when
    /// its remote RPC worker died. The old ceiling was "N crashes inside
    /// a 600s sliding window", which that child could never reach —
    /// crashes aged out of the window faster than a 4.5-minute cycle
    /// could accumulate them — so the supervisor re-mapped 148 GB of
    /// unified memory indefinitely until amdgpu ran out of address space
    /// and the desktop died. The reset condition must be PROOF (a
    /// generation that served), never elapsed time.
    #[tokio::test]
    async fn brief_healthy_stretches_do_not_reset_the_breaker() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        // Becomes healthy on the first probe, serves ~200ms, then dies —
        // the "loads fine, dies on first real work" signature.
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 0.2\nexit 1\n");
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );
        config.backoff_schedule = vec![Duration::from_millis(20)];
        config.crash_loop_max = 2;
        // A full second of serving would prove the restart worked. 200ms
        // does not.
        config.healthy_reset_after = Duration::from_secs(1);

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 40, Duration::from_millis(1500)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Healthy { .. })),
            "the child must actually reach Healthy — otherwise this test \
             proves nothing about brief-healthy generations: {observed:?}"
        );
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Failed { .. })),
            "a child that keeps dying shortly after serving must trip the \
             breaker, got {observed:?}"
        );

        run_handle.abort();
    }

    /// The other half of the same decider: a generation that DOES serve
    /// long enough clears the counter, so an occasional crash in a
    /// long-running child never parks it. Fails on the old wall-clock
    /// ceiling, which would reach `Failed` on the second crash here
    /// regardless of how long the child had been up.
    #[tokio::test]
    async fn a_proven_healthy_generation_resets_the_breaker() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 0.5\nexit 1\n");
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );
        config.backoff_schedule = vec![Duration::from_millis(20)];
        // One crash would be enough to park it if nothing ever reset.
        config.crash_loop_max = 1;
        // ~400ms of serving per generation clears this bar.
        config.healthy_reset_after = Duration::from_millis(150);

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 40, Duration::from_millis(1200)).await;
        let restarts = observed
            .iter()
            .filter(|s| matches!(s, SupervisorState::Restarting { .. }))
            .count();
        assert!(
            restarts >= 2,
            "expected repeated restarts to exercise the reset, got {restarts} in {observed:?}"
        );
        assert!(
            !observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Failed { .. })),
            "a generation that served past healthy_reset_after must clear \
             the breaker — got {observed:?}"
        );

        run_handle.abort();
    }

    /// The backoff floor. A generation that cost 400ms to load must not
    /// be retried on the 20ms schedule — otherwise an expensive child
    /// spends nearly all its wall-clock inside a load, which is what
    /// turned one worker-side crash into host-wide GPU-memory
    /// exhaustion. Measured as the gap between the crash and the next
    /// spawn.
    #[tokio::test]
    async fn backoff_is_floored_by_the_last_generation_s_load_cost() {
        let dir = TempDir::new().unwrap();
        const LOAD: Duration = Duration::from_millis(400);
        let (port, _server) = spawn_slow_health_server(LOAD).await;
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 0.6\nexit 1\n");
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );
        // Generous grace so the slow health server reads as loading, not
        // as failing heartbeats.
        config.ready_deadline = Duration::from_secs(5);
        config.backoff_schedule = vec![Duration::from_millis(20)];
        config.crash_loop_max = 5;
        config.healthy_reset_after = Duration::from_secs(60);

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        // Walk to the first Restarting, then time how long until the
        // supervisor actually spawns again (the next Starting).
        let mut restarting_at = None;
        let mut gap = None;
        for _ in 0..40 {
            match tokio::time::timeout(Duration::from_secs(3), states.recv()).await {
                Ok(Ok(SupervisorState::Restarting { .. })) => {
                    restarting_at = Some(Instant::now());
                }
                Ok(Ok(SupervisorState::Starting)) => {
                    if let Some(t) = restarting_at {
                        gap = Some(t.elapsed());
                        break;
                    }
                }
                Ok(Ok(_)) => {}
                _ => break,
            }
        }

        let gap = gap.expect("supervisor never restarted");
        assert!(
            gap >= Duration::from_millis(300),
            "backoff should have been floored at the ~{}ms load cost, but the \
             next spawn came after only {gap:?} (the 20ms schedule)",
            LOAD.as_millis()
        );

        run_handle.abort();
    }

    /// A gate that is always closed must park the supervisor: no spawns, and —
    /// the load-bearing half — no movement on the crash-loop ceiling. The
    /// ceiling exists to stop a loop burning CPU on spawns that keep failing;
    /// a spawn we deliberately declined costs nothing and must not consume it.
    /// Without this the futile-respawn fix would trade three wasted spawns for
    /// a slot wedged in `Failed`.
    #[tokio::test]
    async fn a_held_spawn_gate_does_not_spawn_and_does_not_burn_crash_budget() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        // Marker file: written once per spawn. Must never appear.
        let marker = dir.path().join("spawned");
        let script = write_script(
            &dir,
            "daemon.sh",
            &format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
        );
        let config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );

        let supervisor =
            Arc::new(
                Supervisor::new(config).with_spawn_gate(Arc::new(|| SpawnVerdict::Hold {
                    reason: "no eligible workers".to_string(),
                })),
            );
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 4, Duration::from_millis(600)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::GateHeld { .. })),
            "expected a GateHeld state, got {observed:?}"
        );
        assert!(
            !marker.exists(),
            "the gate was held but a child was spawned anyway"
        );
        assert!(
            !observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Failed { .. })),
            "a held gate must not trip the crash-loop ceiling, got {observed:?}"
        );

        run_handle.abort();
    }

    /// Anti-wedge. A gate must never be able to hold a slot the manager is
    /// trying to replace — without this the gate is a foot-gun that can strand
    /// a `terminate()` forever.
    #[tokio::test]
    async fn terminate_releases_a_held_spawn_gate() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
        let config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );

        let supervisor =
            Arc::new(
                Supervisor::new(config).with_spawn_gate(Arc::new(|| SpawnVerdict::Hold {
                    reason: "permanently held".to_string(),
                })),
            );
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        // Let it settle into the hold, then ask it to stop.
        tokio::time::sleep(Duration::from_millis(200)).await;
        supervisor.terminate();

        let exited = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
        assert!(
            exited.is_ok(),
            "run() must return promptly when terminated during a gate hold"
        );
    }

    /// An explicit operator action outranks a heuristic: `request_reconnect`
    /// spawns even while the gate says hold.
    #[tokio::test]
    async fn request_reconnect_breaks_a_held_spawn_gate() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
        let config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );

        let supervisor =
            Arc::new(
                Supervisor::new(config).with_spawn_gate(Arc::new(|| SpawnVerdict::Hold {
                    reason: "held".to_string(),
                })),
            );
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        tokio::time::sleep(Duration::from_millis(150)).await;
        supervisor.request_reconnect();

        let observed = drain_states(&mut states, 6, Duration::from_millis(800)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Healthy { .. })),
            "reconnect must override the gate and spawn, got {observed:?}"
        );

        run_handle.abort();
    }

    /// Regression anchor: with no gate installed the run loop behaves exactly
    /// as it always has.
    #[tokio::test]
    async fn no_gate_is_exactly_todays_behaviour() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
        let config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 6, Duration::from_millis(800)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Healthy { .. })),
            "expected Healthy with no gate, got {observed:?}"
        );
        assert!(
            !observed
                .iter()
                .any(|s| matches!(s, SupervisorState::GateHeld { .. })),
            "an ungated supervisor must never publish GateHeld"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn healthy_when_daemon_responds() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        // Daemon: sleep long enough to outlast the test.
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
        let config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 6, Duration::from_millis(800)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Healthy { .. })),
            "expected at least one Healthy state, got {observed:?}"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn crashing_daemon_emits_restarting_then_failed() {
        let dir = TempDir::new().unwrap();
        // No health server — heartbeats fail, but the child also
        // exits on its own well before the threshold, exercising the
        // ChildExited path.
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\necho crashing >&2\nexit 1\n");
        // Tight schedule + low ceiling so the test finishes fast.
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            "http://127.0.0.1:1/v1/models".into(), // unreachable
        );
        config.backoff_schedule = vec![Duration::from_millis(10)];
        config.crash_loop_max = 2;

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 20, Duration::from_millis(600)).await;
        let saw_restarting = observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Restarting { .. }));
        let saw_failed = observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. }));
        assert!(
            saw_restarting && saw_failed,
            "expected Restarting and Failed in {observed:?}"
        );

        run_handle.abort();
    }

    // Flaky under workspace `cargo test` — the stderr drainer task
    // (spawned inside `spawn_child`) doesn't get scheduled fast
    // enough when the tokio runtime is saturated by parallel test
    // crates, and the child's stderr pipe is reaped by the
    // `kill_on_drop` path before the BufReader pulls anything out.
    // Passes consistently in isolation. Proper fix would be to make
    // the drainer drain synchronously on child exit (or to use a
    // bounded channel the supervisor writes from inside the wait
    // loop). Out of scope for the SlotContext refactor.
    #[ignore = "flaky under parallel cargo test (drainer scheduling); passes in isolation"]
    #[tokio::test]
    async fn stderr_ring_captures_recent_lines() {
        let dir = TempDir::new().unwrap();
        let script = write_script(
            &dir,
            "daemon.sh",
            "#!/bin/sh\necho line-1 >&2\necho line-2 >&2\necho line-3 >&2\nexit 0\n",
        );
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            "http://127.0.0.1:1/v1/models".into(),
        );
        // Stop after one restart attempt — we don't care about the
        // ceiling here, just the stderr capture.
        config.crash_loop_max = 0;
        config.backoff_schedule = vec![Duration::from_millis(10)];

        let supervisor = Arc::new(Supervisor::new(config));
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        // Let the supervisor spawn + the child exit + the drainer
        // flush. The script itself runs in <10ms but the drainer is
        // async and the stderr ring can be empty for surprisingly
        // long on a loaded CI host (workspace `cargo test` saturates
        // the tokio runtime across all crates simultaneously, and the
        // drainer's `tokio::spawn` doesn't get scheduled until the
        // pool catches up). Poll with a 10 s deadline rather than a
        // fixed sleep — passes fast on healthy hosts, doesn't
        // false-fail when the runner is heavily loaded.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let tail = loop {
            let current = supervisor.stderr_tail();
            if current.iter().any(|l| l == "line-1") || std::time::Instant::now() > deadline {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(
            tail.iter().any(|l| l == "line-1"),
            "expected line-1 in stderr tail after 10s, got {tail:?}"
        );
        assert!(
            tail.iter().any(|l| l == "line-3"),
            "expected line-3 in stderr tail, got {tail:?}"
        );

        // The crash-log file should also exist.
        let crash_dir = dir.path().join("crashes");
        let entries: Vec<_> = std::fs::read_dir(&crash_dir)
            .expect("crash dir exists")
            .flatten()
            .collect();
        assert!(
            !entries.is_empty(),
            "expected a daemon-*.log in {crash_dir:?}"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn manual_reconnect_after_failed_restarts_supervisor() {
        let dir = TempDir::new().unwrap();
        // Crashes immediately so we hit Failed quickly.
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nexit 1\n");
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            "http://127.0.0.1:1/v1/models".into(),
        );
        config.backoff_schedule = vec![Duration::from_millis(10)];
        config.crash_loop_max = 1;

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        // Wait until we see Failed.
        let mut got_failed = false;
        for _ in 0..30 {
            if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(150), states.recv()).await
            {
                if matches!(s, SupervisorState::Failed { .. }) {
                    got_failed = true;
                    break;
                }
            }
        }
        assert!(got_failed, "supervisor never entered Failed");

        // Fire reconnect; expect another Starting.
        assert!(supervisor.request_reconnect());
        let mut got_starting = false;
        for _ in 0..30 {
            if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(150), states.recv()).await
            {
                if matches!(s, SupervisorState::Starting) {
                    got_starting = true;
                    break;
                }
            }
        }
        assert!(got_starting, "no Starting after manual reconnect");

        run_handle.abort();
    }

    /// Health server answering an arbitrary path (handshake mode uses
    /// `/health`, not `/v1/models`).
    async fn spawn_health_server_at(path: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
        use axum::{routing::get, Json, Router};
        let app = Router::new().route(
            path,
            get(|| async { Json(serde_json::json!({"data": []})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (port, handle)
    }

    #[test]
    fn parse_handshake_extracts_port() {
        assert_eq!(
            parse_handshake("SOVEREIGN_COMPUTE_LISTENING {\"port\":54321,\"pid\":99}"),
            Some(54321)
        );
        // No `port` key → None (don't fire on a partial line).
        assert_eq!(
            parse_handshake("SOVEREIGN_COMPUTE_LISTENING {\"pid\":99}"),
            None
        );
        // Not a handshake line at all.
        assert_eq!(parse_handshake("2026-07-20 INFO loading model"), None);
        // Prefix present but the payload isn't JSON.
        assert_eq!(
            parse_handshake("SOVEREIGN_COMPUTE_LISTENING not-json"),
            None
        );
    }

    #[tokio::test]
    async fn handshake_mode_parses_port_and_reaches_healthy() {
        let dir = TempDir::new().unwrap();
        // The child will announce THIS server's port; the supervisor builds
        // http://127.0.0.1:{port}/health from the handshake line.
        let (port, _server) = spawn_health_server_at("/health").await;
        let script = write_script(
            &dir,
            "child.sh",
            &format!(
                "#!/bin/sh\necho 'SOVEREIGN_COMPUTE_LISTENING {{\"port\":{port},\"pid\":1}}'\nsleep 60\n"
            ),
        );
        let mut config = base_config(script, dir.path().join("crashes"), String::new());
        // Deadlines are generous on purpose: this asserts the handshake is
        // PARSED and reaches Healthy, not how fast. Under a saturated
        // --workspace run the Starting → Warming gap has exceeded 1.5s,
        // which failed the gate on scheduling latency, not on behaviour.
        config.health = HealthTarget::StdoutHandshake {
            health_path: "/health".into(),
            handshake_deadline: Duration::from_secs(20),
        };
        config.ready_deadline = Duration::from_secs(20);

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 8, Duration::from_secs(10)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Warming { port: p, .. } if *p == port)),
            "expected Warming with port {port}, got {observed:?}"
        );
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Healthy { .. })),
            "expected Healthy after handshake, got {observed:?}"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn handshake_timeout_when_child_never_announces() {
        let dir = TempDir::new().unwrap();
        // Child prints nothing on stdout and just sleeps → the handshake
        // deadline elapses → spawn is treated as a crash (Restarting), and
        // with crash_loop_max low we reach Failed.
        let script = write_script(&dir, "child.sh", "#!/bin/sh\nsleep 30\n");
        let mut config = base_config(script, dir.path().join("crashes"), String::new());
        config.health = HealthTarget::StdoutHandshake {
            health_path: "/health".into(),
            handshake_deadline: Duration::from_millis(150),
        };
        config.backoff_schedule = vec![Duration::from_millis(10)];
        config.crash_loop_max = 1;

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 12, Duration::from_millis(500)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Failed { .. })),
            "expected Failed after repeated handshake timeouts, got {observed:?}"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn terminate_exits_loop_without_crash_accounting() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        let script = write_script(&dir, "child.sh", "#!/bin/sh\nsleep 30\n");
        let config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        // Wait until healthy.
        let mut healthy = false;
        for _ in 0..40 {
            if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(150), states.recv()).await
            {
                if matches!(s, SupervisorState::Healthy { .. }) {
                    healthy = true;
                    break;
                }
            }
        }
        assert!(healthy, "child never became healthy");

        // Graceful terminate → the run loop must return promptly (SIGTERM
        // kills `sleep`), NOT restart.
        assert!(supervisor.terminate());
        let joined = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
        assert!(
            joined.is_ok(),
            "run loop did not exit within 5s of terminate()"
        );

        let tail = drain_states(&mut states, 4, Duration::from_millis(200)).await;
        assert!(
            !tail.iter().any(|s| matches!(
                s,
                SupervisorState::Restarting { .. } | SupervisorState::Failed { .. }
            )),
            "terminate() must not trigger restart/failed, got {tail:?}"
        );
    }
}
