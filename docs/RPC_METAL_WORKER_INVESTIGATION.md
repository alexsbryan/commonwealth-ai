# Investigation Briefing: distributed-inference load hangs with a **Metal-backed RPC worker**

**For:** an agent on the **Strix box** (fedora/toolbx, Tailscale `100.115.12.21`, the RPC *host*).
**From:** the Mac node (Apple M2 Max, Tailscale `100.104.36.28`, currently the RPC *worker*).
**Date:** 2026-06-05.

---

## Mission

A 2-node distributed inference run **hangs at model-load** in exactly one topology:
**Vulkan host (Strix) + Metal worker (Mac)**. The reverse topology and CPU-worker variants
all work. Find out *why* the Vulkan-host ↔ Metal-worker load stalls, and whether it's fixable
or a llama.cpp/ggml limitation we must design around.

This matters because the **demo-natural topology is the broken one**: the big node (Strix,
128 GB) should host, with the Mac contributing its Metal GPU as a worker — and that's also the
shape MiniMax-M2.7 (140 GB, file lives on the Strix) requires.

---

## Symptom (observed on the Strix)

- `POST http://100.115.12.21:9741/v1/chat/completions` with `model="commonwealth/primary"`:
  - empty body after a 300 s client timeout,
  - `HTTP 000` after 120 s (no response at all),
  - **zero tokens** streamed in a 45 s streaming request.
- `GET /status` answers in **0.09 s** — the daemon is alive; only the inference/load path is stuck.
- Strix daemon log shows **`reload_primary: redistributing across updated RPC device set`**
  (engine.rs:1677) **then silence** — stuck inside the reload's `ModelSlot::load`.
- Note: `commonwealth/primary` currently resolves to **`Qwen3.6-35B-A3B-MTP-UD-Q6_K` (the 35B)**,
  not the 122B. The hang reproduces on the 35B, so it is **not** model-specific. (Also: the 122B
  present on the Strix is `Qwen3.5-122B-A10B-UD-Q5_K_XL` 3-part, a different quant than the
  `UD-Q6_K` 4-part row in `models.toml` — reconcile later; irrelevant to this hang.)

---

## What is already CONFIRMED (from the Mac side this session)

1. **No code regression.** Mac as **Metal host** distributes correctly to a **CPU worker**:
   `SOVEREIGN_RPC_TENSOR_SPLIT=0.85,0.15` drove the worker's RSS to **1397 MB** (~85% of the
   distributable weight) with 2 sustained connections and a coherent 48-token completion.
   Registration and the tensor-split both fire (verified via logs). The host load path is healthy.

2. **The Strix never received bulk weights.** While the Strix completion was hung, the Mac worker
   process (the daemon serving `:50052`) sat at **4.9 GB RSS = its normal baseline** (fast + embed
   slots) with **no ESTABLISHED connection** from the Strix (only a CLOSED stub). If the Strix had
   streamed the 35B's share onto the Mac, the worker would be **tens of GB**. It wasn't.
   ⇒ **The load stalls at the setup/handshake stage, *before* weight transfer.**

3. **Topology is the only variable that changed:**

   | Run | Host | Worker | Result |
   |---|---|---|---|
   | Rung 1 (passed) | Mac **Metal** | Strix **Vulkan** | ✅ 80-tok completion |
   | Local tests (passed) | Mac **Metal** | **CPU** | ✅ 1397 MB distributed |
   | **Strix now (hangs)** | Strix **Vulkan** | Mac **Metal** | ❌ stalls at setup |

   The **only element never exercised in any passing run is Metal-as-an-RPC-*worker***.

---

## Architecture / code map (same repo on both nodes)

**Host side** (`sovereign-inference/src/embedded/model_slot.rs`):
- `ModelSlot::load` (`:955`) is the load path. In order it calls:
  - `register_rpc_workers()` (`:985` → defined `:667`) — for each endpoint (env
    `SOVEREIGN_RPC_WORKERS` ∪ the discovery provider) calls `ggml_backend_rpc_add_server()` then
    `ggml_backend_register()`, publishing the worker as a ggml **GPU device named "RPC"**.
  - `live_device_list_if_pruning_needed()` (`:992` → defined `:737`) — returns `None` when no
    registered RPC device is dead, keeping the NULL-`devices` auto-enumeration path. **In the hang
    case the worker is live, so this returns `None` — `with_devices` is NOT used.**
  - `rpc_tensor_split()` (`:1000`) — applies `SOVEREIGN_RPC_TENSOR_SPLIT` (device order: RPC first,
    then local GPU).
  - `LlamaModel::load_from_file()` (`:1008`) — **this is where it hangs.** With NULL `devices`,
    llama.cpp enumerates local Vulkan + the RPC device and splits layers; loading a layer onto the
    RPC device triggers `ALLOC_BUFFER` / `INIT_TENSOR` / `SET_TENSOR` RPC commands to the worker.

**Worker side** (same file):
- `serve_rpc_worker_if_configured()` (`:817`) — when `SOVEREIGN_RPC_SERVE` is set, starts an
  in-process `ggml_backend_rpc_start_server()` (`:913`) on a dedicated thread, serving **every
  local non-CPU device** (i.e. **Metal** on the Mac; it skips CPU/ACCEL/META at `:760`, `:834`).
  This is what the Mac is running now (`SOVEREIGN_RPC_SERVE=0.0.0.0:50052`).

**Reload + discovery** (host orchestration):
- `EmbeddedLlamaCpp::reload_primary()` (`engine.rs:1655`) — drops + reloads the primary so layers
  redistribute; logs `redistributing…` at `:1677`, then calls `ModelSlot::load`. Holds the
  `lazy_inflight` permit across the reload — **so if the load hangs, every completion blocks behind
  it** (explains the daemon-alive-but-completions-hang symptom).
- Discovery loop: `daemon_cmd.rs:1359` (gated on `SOVEREIGN_RPC_DISCOVER`) scans peers' `/status`
  every 15 s via `MeshDaemon::discover_rpc_workers()` (`sovereign-mesh/src/daemon.rs:1205`), builds
  `ip:port` from each peer's `rpc_worker.port`, and calls `reload_primary()` (`:1401`) on a
  worker-set change (debounced 20 s).

**Stack:** llama.cpp **b9180** (commit `64b38b561`), **RPC protocol v4.0.0**, via
`llama-cpp-sys-4` 0.2.57 with the `rpc` feature (`GGML_RPC=ON`). Sys symbols used directly:
`ggml_backend_rpc_add_server`, `ggml_backend_register`, `ggml_backend_rpc_start_server`,
`ggml_backend_rpc_get_device_memory`.

---

## Live config

- **Mac (worker):** `SOVEREIGN_RPC_SERVE=0.0.0.0:50052`, in-process Metal worker. `/status`
  advertises `rpc_worker:{"port":50052}`. node_id `node-b88252e4325bc377`, Tailscale
  `100.104.36.28`. ~54 GB usable Metal (`recommendedMaxWorkingSetSize` 55662 MB).
- **Strix (host):** `SOVEREIGN_RPC_DISCOVER=1`. Daemon runs inside a toolbx container. ~110 GB
  usable Vulkan. Tailscale `100.115.12.21`, daemon HTTP `:9741`.

---

## Hypotheses, most → least likely

1. **Vulkan-host ↔ Metal-worker handshake/buffer-alloc hangs.** The host requests a buffer/init on
   the remote Metal device; the Metal RPC server blocks (or the host blocks waiting on it) before
   any `SET_TENSOR`. Heterogeneous Metal↔Vulkan RPC is proven in the *other* direction (Metal host
   ↔ Vulkan worker, Rung 1) but never this way.
2. **Metal as an `rpc-server` backend device is itself problematic.** Metal residency sets /
   shared-buffer behavior (`use residency sets = true`, `use shared buffers = true` in the Mac
   worker init) may not survive being driven by a remote host. Loopback Metal↔Metal already
   *aborts* in `graph_compute` (GPU aliasing); a remote Vulkan host may *hang* at an earlier stage.
3. **A protocol/threading deadlock** in our in-process worker (`serve_rpc_worker_if_configured`)
   specific to the Metal backend (e.g. the blocking `start_server` thread vs. Metal's command-queue
   threading).

---

## Investigation plan (decisive-first)

**Step 0 — prove the daemon is otherwise healthy (isolate the distributed path).**
On the Strix, load the primary **local-only**: stop discovery (unset `SOVEREIGN_RPC_DISCOVER`, or
stop the Mac worker so discovery finds none) and restart the daemon. The 35B fits the Strix alone.
A normal completion confirms the hang is *only* in the distributed-onto-Metal-worker path, and
gives a working 35B immediately. **If local-only ALSO hangs, stop — the problem is not the worker.**

**Step 1 — capture WHERE the host is blocked.** With a completion hung, attach to the Strix daemon
(`gdb -p <pid>` inside the toolbx, or `RUST_BACKTRACE=full` + a SIGQUIT/`lldb` thread dump) and get
the loading thread's backtrace. Expectation if hypothesis 1 holds: blocked in a `recv()` on the RPC
client socket inside `ggml_backend_rpc_*` (host waiting on the worker). Confirm it's inside
`llama_model_load` → `ggml_backend_rpc` and note the **last RPC command** issued.

**Step 2 — capture WHERE the worker is stuck (the smoking gun).** Coordinated with the Mac:
trigger a hang, then thread-dump the **Mac worker** process (the Mac is reachable; a Mac-side agent
can `sample <pid>` / lldb the daemon at `:50052`). This shows whether the Metal worker is stuck in
`ALLOC_BUFFER`, `INIT_TENSOR`, `graph_compute`, or never received a command at all. **This single
artifact most directly settles hypotheses 1 vs 2.** (Ask the Mac-side agent — the daemon pid was
`51726` this session, but re-check.)

**Step 3 — turn on RPC command logging on both ends.** `ggml-rpc.cpp` can log each command. Build
the worker with verbose RPC (or run the standalone `target/rpc-worker-build/bin/rpc-server -d Metal
-p 50055` with verbosity on the Mac and point the Strix at it via
`SOVEREIGN_RPC_WORKERS=100.104.36.28:50055`) and watch the **last command before silence** on each
side. `ALLOC_BUFFER`/`GET_ALLOC_SIZE` = setup-stage hang (hypothesis 1/2); `GRAPH_COMPUTE` = compute
hang (closer to the loopback abort family).

**Step 4 — cheap differential: CPU worker instead of Metal.** Have the Mac serve a **CPU** worker
(standalone `target/rpc-worker-build/bin/rpc-server -d CPU -p 50056`) and point the Strix at it
(`SOVEREIGN_RPC_WORKERS=100.104.36.28:50056`). If the Strix-host load now **completes**, Metal-as-
worker is *definitively* the culprit (and you have a working-but-CPU-slow Strix-host path as a
fallback). If it **still hangs**, the fault is on the **Vulkan-host** side, not the worker backend.
(Note: our in-process worker only serves non-CPU devices, so use the standalone `rpc-server -d CPU`
for this, not `SOVEREIGN_RPC_SERVE`.)

**Step 5 — upstream check.** Does this llama.cpp (b9180) support **Metal** as an `rpc-server`
backend device at all? Search the llama.cpp issues/PRs for "rpc-server Metal" / "RPC Metal backend
hang". If it's a known gap, design around it (worker = CPU on the Mac, or move the host to the Mac).

---

## If the goal is just a *working* distributed run NOW (not the root-cause fix)

Flip to the **proven** topology: **Mac = Metal host, Strix = Vulkan worker** (Rung 1, validated).
For the 122B this needs the GGUF on the Mac — copy it Strix→Mac **over the Tailscale LAN** (local,
**not** ISP-metered; ~104 GB + ~104 GB free Mac disk). Then `SOVEREIGN_RPC_WORKERS=<strix>:50052`
on the Mac with a split like `0.7,0.3` (Strix gets the lion's share). This sidesteps Metal-as-worker
entirely. The cost is the one-time local copy; the benefit is a real distributed 122B today.

---

## Reset note

The Strix's primary slot is likely **wedged** (reload_primary holding `lazy_inflight` after the
hung load). A daemon restart on the Strix is the clean reset before re-testing. Per the toolbx
node-id volatility caveat, bind-mount `~/.sovereign/` so the mesh node_id survives a container
rebuild.

---

## RESOLUTION (2026-06-05, Strix-side investigation, with live Mac-agent co-debugging)

**The hang is NOT Metal-specific. Root cause: a host-side `send()` deadlock in
llama.cpp's RPC weight-upload path, triggered by upload volume (~800 MB), and
reproduced with a plain CPU worker.** Metal was a red herring.

### Evidence chain (each step isolates one variable)

1. **Strix Vulkan host → localhost CPU worker** (stock `llama-bench` *and* the daemon,
   full 28 GB 35B): ✅ loads + infers. → host side and the daemon load path are healthy.
2. **Strix Vulkan host → Mac CPU worker over Tailscale, 0.8 B (736 MB)**: ✅ loads split
   50/50 and runs inference (pp 26.6 / tg 5.74 t/s) over a **direct-LAN** path.
   → **cross-machine RPC genuinely works.**
3. **Same cross-machine path, 35B/4B with >~800 MB to the worker**: ❌ wedges. gdb host
   backtrace, identical across two dumps:
   `send() → socket_t::impl::send_data → ggml_backend_rpc_buffer_set_tensor →
   llama_model_loader::load_all_data → llama_model_load_from_file → main`.
   Host is blocked **writing weights to the socket during load**; Mac worker is
   ESTABLISHED, idle in `recv`, queues empty, **0 accept failures, healthy**.
4. **Flag sweep on the failing 4B/1.7 GB case**: baseline ❌, `--no-mmap` ❌, `-dio 1` ❌
   — **all wedge**. So this is **not** the #19745 mmap/UMA mechanism (`-dio` fixes that);
   it is a genuine RPC `set_tensor` transport flow-control deadlock. Tensors >10 MB take
   a `SET_TENSOR_HASH` round-trip then one full-data `send()` (`ggml-rpc.cpp:461-481`) —
   that send is where it blocks.

Why the original briefing pointed at Metal: the Mac's in-process **Metal worker (:50052)
had separately died** (Bug A/B below) and stopped listening, so discovery kept *skipping*
it — masking the real failure. When a worker WAS reachable, the host hit the same
`send()` deadlock regardless of backend.

### Secondary worker-resilience bugs (independent; worth fixing regardless)

- **Bug A (ggml, upstream):** the rpc-server accept loop `return`s on a single
  `accept()` failure — `ggml-rpc.cpp:1744` (vendored llama-cpp-sys-4 0.2.57, b9180).
  One transient `ECONNABORTED` permanently kills the worker. Should `continue`.
- **Bug B (ours):** `serve_rpc_worker_if_configured` (`model_slot.rs:899-925`) calls
  `ggml_backend_rpc_start_server` once, logs `"in-process RPC worker server loop exited"`,
  and never restarts — while `/status` keeps advertising `rpc_worker.port`. Should
  supervise/restart the server thread and tie the advertisement to actual liveness.

### What works today / what doesn't

- ✅ Strix hosts alone (35B; the 122B fits 128 GB).
- ✅ Small models distribute cross-machine (≤ ~800 MB uploaded to the worker).
- ❌ Large-model network distribution — the `send()` deadlock. Upstream llama.cpp issue.

### Fix directions for large-model distribution (in rough order of effort)

1. **Worker-side tensor cache pre-warmed from a local GGUF** — if the worker has the
   model file and warms its cache, every weight's `SET_TENSOR_HASH` *hits* and the host
   **skips the data send entirely** (`ggml-rpc.cpp:469-471`), sidestepping the deadlock.
   (`SOVEREIGN_RPC_CACHE_DIR` / commit 9336c381 "offline warm-from-GGUF" is the hook.)
2. **Vendored-ggml patch**: chunk/throttle `set_tensor` data sends, and/or enlarge
   `SO_SNDBUF`/`SO_RCVBUF`, and fix Bug A (`continue` on `accept()` failure).
3. **Upstream llama.cpp fix** for RPC large-tensor transport robustness.
4. **Avoid it**: host on the Strix alone (122B fits), or the proven flip topology
   (Mac Metal host + Strix Vulkan worker) — neither uses the broken large-upload path
   over the network from the Strix.

Flag-level remedies (`-dio`, `--no-mmap`) do **not** help.
