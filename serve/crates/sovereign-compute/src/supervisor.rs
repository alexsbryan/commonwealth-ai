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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex as AsyncMutex};
use tracing::{debug, error, info, warn};

mod child;
use child::{signal_group, ExitOutcome, Signal};

/// Prefix of the one-line stdout handshake a compute child prints once it
/// has bound its ephemeral port (before it loads its model). The rest of
/// the line is `{"port":N,"pid":N}`. Kept in sync with the child's
/// entrypoint (`child_main`).
pub const HANDSHAKE_PREFIX: &str = "SOVEREIGN_COMPUTE_LISTENING ";

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
    /// The child serves raw TCP at a known address (ggml's `rpc-server`), so
    /// health is a connect, and only until the first one succeeds; after
    /// that the child's exit is the only signal. rpc-server serves one client
    /// at a time on a listen backlog of 1 (llama.cpp `transport.cpp:677`), so
    /// probes sent while a head is connected queue behind it, fill the
    /// backlog and read as failures: probing on would kill a peer for serving.
    TcpListening(std::net::SocketAddr),
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
    /// How health is probed: a GET of a fixed URL or of one built from the
    /// child's stdout handshake (any 2xx is healthy), or a connect.
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
    /// How long `terminate()` waits after SIGTERM before SIGKILL. The
    /// signal goes to the child's whole process group, so a child that
    /// tears down children of its own (llama-server's router stops one
    /// process per model) needs longer than a single process does.
    pub terminate_grace: Duration,
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

pub struct Supervisor {
    config: SupervisorConfig,
    state_tx: broadcast::Sender<SupervisorState>,
    reconnect_tx: mpsc::UnboundedSender<()>,
    reconnect_rx: AsyncMutex<Option<mpsc::UnboundedReceiver<()>>>,
    /// Graceful-shutdown signal: SIGTERM the child, wait up to
    /// [`SupervisorConfig::terminate_grace`], SIGKILL if it hasn't exited, then leave the
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
    /// Monotonic count of restarts this supervisor has ORDERED, incremented
    /// at the same point it broadcasts [`SupervisorState::Restarting`].
    ///
    /// The authoritative source for "how many times has this child come
    /// back" (ARCH_PRINCIPLES §10.6 — one decider). Subscribers must NOT
    /// derive it by counting `Restarting` broadcasts: `broadcast` is a
    /// bounded ring, a slow consumer gets `RecvError::Lagged`, and the
    /// dropped messages are gone. `collect_lifecycle` in the manager did
    /// exactly that and under-reported — a child that had crash-looped
    /// showed `restarts: 0` to the operator while still reaching Serving,
    /// which is the one number that would have told them it was looping.
    /// Caught 2026-08-10 by `sigkill_child_midstream_recovers` failing
    /// only under full-workspace load.
    total_restarts: Arc<AtomicU32>,
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
            total_restarts: Arc::new(AtomicU32::new(0)),
        }
    }

    /// How many restarts this supervisor has ORDERED since construction.
    ///
    /// Read this instead of counting `SupervisorState::Restarting`
    /// broadcasts — see the field's doc for why counting the broadcast
    /// under-reports under load.
    pub fn total_restarts(&self) -> u32 {
        self.total_restarts.load(Ordering::Relaxed)
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
    /// child exit on its own),
    /// waits up to [`SupervisorConfig::terminate_grace`], SIGKILLs if needed, then exits the
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

            // Resolve the probe. For `Fixed` and `TcpListening` this is
            // immediate; for `StdoutHandshake` it awaits the child's port announcement
            // (emitting `Warming`), or fails to local crash-accounting if
            // the child dies or never announces within the deadline.
            let probe = match self.resolve_probe(port_rx, pid, &mut child).await {
                Some(probe) => probe,
                None => {
                    signal_group(pid, Signal::Kill);
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
                    &probe,
                    &http,
                    &mut reconnect_rx,
                    &mut terminate_rx,
                )
                .await;
            // The leader is gone however it went; anything it forked must
            // not outlive the generation and hold its port or memory.
            signal_group(pid, Signal::Kill);
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
        // Bump BEFORE the broadcast: a subscriber woken by the broadcast
        // must never read a counter that has not yet caught up.
        self.total_restarts.fetch_add(1, Ordering::Relaxed);
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
#[path = "supervisor_tests.rs"]
mod tests;
