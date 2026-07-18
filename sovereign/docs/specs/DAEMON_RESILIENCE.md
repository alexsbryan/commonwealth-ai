# Daemon Resilience — fault-tolerance audit & bulletproofing plan

**Status:** audit complete 2026-07-18 (four parallel code audits: process
lifecycle, desktop integration, internal failure domains, observability).
P0 in progress. Evidence citations are file:line as of the audit commit;
re-verify before relying on exact line numbers.

**Why this doc exists:** the daemon is the component everything else
depends on. At thousands of users, every unhandled daemon failure mode is
a support ticket. This doc is the single map of (a) what actually fails,
(b) what the code does about it today, and (c) the ordered plan to make
failure boring.

---

## 1. Reliability target

Crash-only design. We assume the process *will* die — llama.cpp/ggml
guarantees native crashes we cannot catch in-process — and engineer for
three user-visible guarantees:

1. **Contained or supervised.** Every failure is either contained at the
   task/request level, or it kills the process and a supervisor restores
   it within seconds with user-visible state preserved.
2. **Visible in plain language.** The user always sees *what* happened
   ("Sovereign restarted after a model crash"), never a dead window or a
   silent degradation.
3. **Diagnosable after the fact, locally.** Every crash leaves a
   structured artifact on the user's own disk that they can bundle in one
   action. There is **no telemetry** — this is a sovereignty-first
   distributed system; users run their own daemons and meshes, and
   diagnostics never leave the machine unless the user explicitly shares
   them. Every observability item below is local-first by construction.

A failure mode that satisfies none of the three is a P0 bug regardless of
how rare it is.

---

## 2. Empirical failure catalog

What has actually bitten, from session notes and git history — the plan
is prioritized against this list, not hypotheticals.

| Failure | Class | Status |
|---|---|---|
| tokio-rt-worker stack overflow → SIGABRT crash loop (77 aborts / 166 starts, 2026-05-12) | fatal | Mitigated (8 MiB stacks, `RUST_BACKTRACE=full`); root cause never confirmed |
| ggml/llama.cpp SIGSEGV/abort (Intel mac, ROCm A3B, decode-time) | fatal | Pre-load smoke test + arch refusal only; live-slot crash still kills the process |
| Kernel OOM SIGKILL at ~39.5 GB — defense existed but unarmed | fatal | RSS hard-limit self-SIGTERM (exit 102) exists; armed **only** by `scripts/daemon-supervised.sh` |
| Decode Error -3 / KV desync (qwen-MoE, MTP) | degraded | Self-healing: MTP quarantine + structural demote (`model_slot.rs:1333-1425`) |
| Mesh leave dropped `:9741` forever; failed join stranded daemon | degraded | Fixed: `leave_to_solo` in-process rebind + failed-join rollback |
| Founder iroh ingress stale after ~31h uptime — peers can't join until restart | degraded | Fixed: `iroh_watchdog.rs` staged escalation (2138bc28) |
| Lint/test watcher silently dead, stale results labeled fresh | silent | Fixed: heartbeat sidecar + `WatcherSupervisor` self-heal |
| Log spam (~1000 lines/h) evicting real history from the 10 MiB rotation window | forensics | Fixed 2026-07-18 (log-level demotions, collision latch) |
| Daemon "Running" with no `:9741` listener (best-effort bind swallowed) | silent | Open — see P0.5 |
| Double `daemon run` → model-loaded zombie + clobbered pidfile | silent | Open — see P0.5 |

## 3. Fault map by domain

Containment status of each subsystem inside the long-running daemon
(`sovereign-cli-daemon` hosting sovereign-core runtime + sovereign-mesh
`EmbeddedDaemon` + commonwealth-api listeners).

| Domain | Status | Recovery today | Key gap |
|---|---|---|---|
| Inference, Rust-level errors | contained | `catch_unwind` at every decode site; MTP slot quarantine/demote; prefix-cache gate on recurrent archs; preflight token clamp | panic path doesn't mark KV cache-state-unknown (only the `Err` path rewinds) |
| Inference, native ggml crash | **fatal** | subprocess smoke test at load (load + 1 token, verdict cached); GGUF arch refusal; VRAM fit gate | live-slot SEGV kills the daemon; no per-slot isolation; no generic wedged-slot rebuild (MTP-only) |
| Memory (RSS) | fatal-but-designed | `memory_watch.rs` 60s sampler; soft-warn; hard-limit self-SIGTERM → exit 102 → supervisor relaunch | **hard limit disabled unless env set — only `daemon-supervised.sh` sets it**; systemd/launchd units set no env |
| LanceDB / corpus indexes | contained | per-corpus skip+warn in `installed_indexes()` and retrieval fan-out (`corpus_search.rs:329-400`); mtime cache kills the reopen storm | no corrupt-index quarantine/repair; skipped silently forever after first warn |
| SQLite stores | fatal at boot, serialized at runtime | WAL + integrity_check + auto `wal_checkpoint(TRUNCATE)`; corruption → `NeedsUserDecision` | migration failure at boot is fatal, no rebuild path for derived stores; no `busy_timeout` |
| Mesh / iroh | contained + self-healing | `iroh_watchdog` (nudge → relay bounce → capped endpoint rebuild); `leave_to_solo` rebind; `HybridProvider::complete` failover + circuit breaker | `complete_stream` has **no** failover (the main chat path); watchdog covers founder self-reachability only |
| HTTP servers (:9741/:9742) | connection-level only | hyper per-connection isolation; poison-lock recovery in scheduler/admission; unwrap-deny ratchet in commonwealth-api | **no CatchPanicLayer, no request timeout, no concurrency/body limits**; both listeners share one `select!` (`sovereign-mesh/daemon.rs:~2437`) — one accept failure drops both; `:9741` bind is best-effort (phantom-Running) |
| Background loops | mixed | watcher coordinator, iroh watchdog, memory watch: heartbeat-supervised | reindexer/atlas/enrichment/notes loops (`daemon_cmd/bootstrap.rs` ~397-809) + log rotation are fire-and-forget bare spawns — a panic is silent and permanent; **the daemon binary installs no panic hook** (the CLI shim's hook is lost across `exec`) |
| Process lifecycle | partial | `daemon start` has bind-collision detector + stale-pid probe; systemd `Restart=on-failure` / launchd `KeepAlive.SuccessfulExit=false` do relaunch on crash | `daemon run` path has no single-instance guard and unconditionally clobbers the pidfile; `stop` gives up after 10s (no SIGKILL escalation); shell supervisor is debug-binary, flat 8s, no ceiling, nothing supervises it |
| Desktop (released app) | **fatal by architecture** | boot watchdog (45s stall → webview reload); pre-load smoke test; crash records + crash bundle | **daemon + ggml run in-process → any native crash kills the app**; W1 child-process supervisor is opt-in (`SOVEREIGN_USE_SUPERVISOR=1`), can't run packaged (no sidecar wired), silently falls back to in-process on child failure; ReconnectBanner never fires in release and its button calls nothing; Attach mode has no post-attach health poll |
| Observability | weakest layer | `sovereign doctor` (point-in-time); `/status` report; desktop crash records/bundle; shutdown-forensics log line | **no metrics at all**; no daemon `/healthz` gate; no daemon self-heartbeat; headless installs get no crash capture; no `doctor --bundle`; glassbox tracing allowlist has silently blinded new targets 3× |

Patterns worth copying when hardening elsewhere: the iroh watchdog's
staged escalation with cooldown + rebuild cap; the watcher heartbeat
(liveness = recent stamp, not a one-shot bool); MTP quarantine
(demote-in-place instead of restart); `supervised_task.rs`
(catch_unwind + backoff + restart ceiling).

---

## 4. The plan

### P0 — no user-visible process death (launch blockers)

- [ ] **P0.1 — Ship the W1 flip: daemon as supervised child in the
  released desktop.** Design decision (2026-07-18): **self-spawn, not a
  sidecar** — bundling `sovereign-cli-daemon` via `externalBin` would add
  ~241 MB to every installer for code the desktop binary already links.
  Instead the desktop binary gains a `--daemon-child` argv mode (same
  pattern as the existing `--smoketest` crash-isolated child) that runs
  the REAL daemon entry: `sovereign-cli-daemon` becomes bin+lib, exposing
  `daemon_child_main()`, and the supervisor spawns
  `current_exe() --daemon-child`. One daemon bootstrap, two entry points;
  zero installer growth; the child inherits every P0.3–P0.5 defense.
  Supervised mode becomes default-ON (`SOVEREIGN_USE_SUPERVISOR=0` is the
  kill-switch; `SOVEREIGN_FORCE_LOCAL=1` also skips it — its documented
  meaning is "this process runs the weights", and the desktop harnesses
  rely on that). Child-start failure *surfaces* (event + banner) instead
  of silently reverting to in-process (`supervisor_setup.rs:187-203`).
  Done when: a `kill -SEGV` of the child daemon mid-chat produces the
  reconnect flow, not a dead app, in a packaged build on macOS + Linux.

  *Code landed 2026-07-18:* `sovereign-cli-daemon` is bin+lib
  (`lib.rs::run_with_args` / `daemon_child_main`); desktop `main.rs`
  gained the `--daemon-child` arm; `supervisor_setup.rs` default-ON +
  self-spawn + `supervisor-fallback` event; `supervisor_reconnect` /
  `supervisor_active` commands; ReconnectBanner's Reconnect button is
  real and the fallback notice renders. **Remaining before checking
  this box:** (a) packaged-build verification on macOS + Linux incl.
  the kill -SEGV drill; (b) known softening — the first session right
  after the wizard runs in-process (`complete_setup` →
  `state::bootstrap` doesn't re-run mode detection); isolation engages
  from the second launch. The first session's riskiest moment (initial
  model load) is already covered by the subprocess smoke test; (c) UX
  edge — `svrn daemon stop` against a desktop-supervised child kills it
  and the supervisor restarts it ~1s later ("I stopped it but it came
  back"); the child should advertise it is desktop-supervised so the
  CLI can say so; (d) graceful SIGTERM-with-grace on app quit (today:
  `kill_on_drop` SIGKILL; tolerable — mesh.json writes are atomic —
  but drains nothing).
- [ ] **P0.2 — Make reconnect real.** Wire `Supervisor::request_reconnect`
  to a Tauri command; give the frontend a daemon-health poll and a global
  daemon-down surface (today: `MeshStatusIndicator` silently nulls,
  `ReconnectBanner` is dead code in release); Attach mode gets post-attach
  monitoring + auto-reattach (today explicitly fire-and-forget,
  `App.svelte:174-177`). Done when: killing an attached daemon shows a
  banner within ~5s and recovery is one click (or automatic).
- [x] **P0.3 — Arm the OOM defense by default, in-daemon.** *(Landed
  2026-07-18.)* `memory_watch.rs` derives defaults from total system RAM
  — hard 85% / soft 70% on Linux (cgroup-v2 `memory.max`-aware), 65% /
  50% on macOS (jetsam observed killing at ~69% of RAM); env overrides;
  `SOVEREIGN_RSS_HARD_LIMIT_MB=0`/`off` is the explicit kill-switch; RAM
  undetectable falls back to the legacy posture with a loud
  "hard limit DISABLED" boot warning. systemd unit gained
  `TimeoutStopSec=30` + `StartLimitIntervalSec=600`/`StartLimitBurst=5`;
  launchd plist gained `ExitTimeOut=30`.
- [x] **P0.4 — Panic hook + supervised background tasks.** *(Landed
  2026-07-18.)* `panic_hook.rs`: every Rust panic (including tokio-task
  panics that were silently swallowed) logs + writes a structured record
  to `~/.sovereign/crashes/daemon-panic-*.json` + `last-crash.json`
  marker, pruned to 50. `supervise.rs`: `spawn_supervised(name, make)` —
  restart-on-panic with 2s→300s backoff, ceiling 5, loud DEGRADED park.
  Converted: all seven `daemon_cmd/bootstrap.rs` fire-and-forget spawns
  (rpc_worker_discovery, slot_alias_push, notes_tier_backfill,
  notes_ttl_sweep, notes_ingest_poller, lazy_stamp_fingerprints,
  tier2_enrichment_resume) + log_rotation + memory_watch.
- [x] **P0.5 — Close the process-identity holes.** *(Landed 2026-07-18;
  the bind-policy item shipped as a listener watchdog — see below.)*
  flock(2) single-instance run lock (`~/.sovereign/daemon.lock`,
  kernel-released on any exit incl. SIGKILL; escape hatch
  `SOVEREIGN_ALLOW_MULTIPLE_DAEMONS=1`), taken before model load;
  `daemon stop` escalates to SIGKILL after the 10s SIGTERM grace
  (`await_exit_or_sigkill`); phantom-Running is closed by
  `listener_watch.rs` — probe the client port each minute, 3 consecutive
  refusals → self-SIGTERM → **exit 104** → service-manager relaunch —
  chosen over making the in-task bind fatal, which would re-break the
  default-port integration tests (the documented `leave_to_solo`
  landmine). Exit-code contract is now: 0 deliberate, 102 RSS limit,
  104 listener lost. Known limitation: the TCP probe verifies *a*
  listener answers, not *our* listener — a foreign process (or an
  old-binary daemon) holding the port satisfies it; P2.1's
  identity-stamped `/healthz` (pid in the response, compared by the
  watchdog) closes that.
### P1 — request-level containment

- [ ] **P1.1** `CatchPanicLayer` (panic → 500) + request-timeout layer +
  body-size limit + load-shed/concurrency cap on both routers; split the
  client/internal serve futures so one accept-loop failure can't drop the
  other listener.
- [ ] **P1.2** Streaming failover: `HybridProvider::complete_stream`
  retries the next healthy backend before first token (parity with
  `complete`).
- [ ] **P1.3** Generic wedged-slot recovery: N consecutive decode
  failures → rebuild the slot in place (generalize the MTP
  quarantine/demote); mark KV cache-state-unknown on the panic path too.

### P2 — see flakes before users report them

- [ ] **P2.1** Real `/healthz` that *gates* (listener bound, slots
  responsive, DBs pass integrity, mesh sane) — kills phantom-Running;
  desktop supervisor + doctor + Attach poll all consume it.
- [ ] **P2.2** Daemon self-heartbeat file (watcher pattern) so headless
  liveness doesn't depend solely on the service manager.
- [ ] **P2.3** Restart/decode-failure/request-error counters on
  `/status`; last-crash marker consumed at next boot → "recovered from a
  crash" surfaced in UI + status.
- [ ] **P2.4** `sovereign doctor --bundle`: one attachable artifact
  (logs + config-redacted + crash records + doctor --json).
- [ ] **P2.5** Local-first support flow at scale — **no telemetry, ever**
  (against the system's ethos; users run their own daemons/meshes in
  containment). Instead: make the local artifacts so good that a user can
  self-diagnose ("Sovereign restarted after a model crash — details +
  copy bundle" in the UI), and make sharing a deliberate act
  (`doctor --bundle` file the user attaches wherever they choose). Any
  future aggregation happens on the *user's own mesh* under their
  control, or not at all.

### P3 — prove it and keep it proven

- [ ] **P3.1** Chaos lane in CI: kill -9 mid-stream, RSS-limit trip,
  slot-wedge injection, join/leave cycles; assert recovery time and no
  stuck states (extend `mesh-soak.sh` / chaos runner).
- [ ] **P3.2** 48h+ soak gate before releases — founder-ingress and
  log-spam were long-uptime bugs no short test catches.
- [ ] **P3.3** Stack-overflow root cause: chase the now-armed backtraces;
  bound corpus-engine recursion depth independently of the 8 MiB ceiling.
- [ ] **P3.4** SQLite: `busy_timeout`; rebuild-from-scratch fallback for
  derived stores on migration failure (reserve `NeedsUserDecision` for
  user data).
- [ ] **P3.5** Retire `scripts/daemon-supervised.sh` once P0.3 makes its
  env-arming redundant (keep it only as the toolbox/GPU dev runner it was
  written to be).

---

## 5. Change log

- 2026-07-18 — audit completed; doc created. No-telemetry posture made
  explicit (P2.5, §1.3).
- 2026-07-18 — P0.3–P0.5 landed (RAM-derived OOM defaults; panic hook +
  crash records + supervised background tasks; flock single-instance
  lock; stop SIGKILL escalation; listener watchdog / exit 104).
  Verified: full-workspace lint PASS; full test suite 7,730 pass / 1
  fail — the one failure was this work's own racy dead-port test
  assertion, removed. Live-daemon deploy verification pending (the dev
  box daemon still runs the pre-P0 binary). P0.1 design decided:
  self-spawn `--daemon-child`, not a 241 MB sidecar.
- 2026-07-18 (later) — P0.1/P0.2 code landed: daemon crate bin+lib,
  desktop `--daemon-child` arm, supervisor default-ON with surfaced
  fallback, `supervisor_reconnect`/`supervisor_active` commands,
  ReconnectBanner Reconnect wired + fallback notice. Post-flip full
  workspace re-verified: lint PASS, tests 7,728 / 0 fail. Remaining
  for the P0.1/P0.2 checkboxes: packaged-build kill -SEGV drill
  (macOS + Linux), first-post-wizard-session engagement, attach-mode
  health poll / auto-reattach.
