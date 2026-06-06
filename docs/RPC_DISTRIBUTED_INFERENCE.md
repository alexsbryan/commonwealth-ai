# Distributed inference (llama.cpp RPC)

Run one model across several mesh nodes — each node contributes its GPU, and
the model's layers are split across them (llama.cpp pipeline-parallel RPC).
Lets a node serve a model larger than any single node's VRAM, or pool capacity.

## Two roles, both just the daemon + one env var

The daemon already links ggml with `GGML_RPC=ON`, so **no separate `rpc-server`
binary is built or run** — a node takes a role purely by environment:

| Role | Env var | What it does |
|---|---|---|
| **Worker** | `SOVEREIGN_RPC_SERVE=0.0.0.0:50052` | Daemon starts an in-process RPC server exposing this node's local GPU to peers. |
| Worker cache | `SOVEREIGN_RPC_CACHE_DIR=<dir>` | Optional. On-disk tensor cache (default `~/.sovereign/rpc-cache`); set `off`/`0` to disable. See "Transfer cost" below. |
| **Host (auto)** | `SOVEREIGN_RPC_DISCOVER=1` | **Auto-discovery + auto-reload** — the host scans peers' `/status` for advertised workers (no IP list). When the worker set **changes** — a worker joins *or dies* — and settles (~20s debounce), it **force-reloads the primary** so the model redistributes; a dead worker is pruned from the device set (ggml has no unregister, so the reload passes an explicit live-only device list). |
| Host (manual) | `SOVEREIGN_RPC_WORKERS=<ip>:50052,<ip2>:50052` | Explicit worker list (union'd with auto-discovery). Use when you want to pin specific peers. |
| Host (split) | `SOVEREIGN_RPC_TENSOR_SPLIT=0.7,0.3` | Optional. Per-device fractions, **device order = RPC workers first, then local GPU**. Omit to let llama.cpp split by advertised VRAM. |

A node may be both (set both vars). Unset = ordinary single-node local inference,
byte-for-byte unchanged.

> Mechanism: the worker calls `ggml_backend_rpc_start_server` on a thread; the
> host calls `ggml_backend_rpc_add_server` + `ggml_backend_register`, after which
> llama.cpp's loader treats each worker as a GPU device named `RPC` and splits
> layers across them. See `sovereign-inference/src/embedded/model_slot.rs`.

## Rung 1 — heterogeneous 2-node bring-up (Mac host + Strix worker)

**On the Strix box** (the GPU worker, Vulkan), inside the toolbox:

```bash
# Build the daemon with the RPC feature (already in Cargo.toml; Linux pulls Vulkan)
cargo build -p sovereign-server          # or your normal daemon build

# Run the daemon as a worker — serves the Vulkan GPU to peers
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 <daemon-binary> ...
# Confirm it is listening:
ss -ltnp | grep 50052           # (or: lsof -nP -iTCP:50052 -sTCP:LISTEN)
```

The daemon logs `starting in-process RPC worker (serving local GPU to mesh peers)`
and the RPC banner lists the Vulkan device + its free VRAM.

**On the Mac** (the host), point at the Strix over Tailscale:

```bash
STRIX_IP=<strix-tailscale-ip>
SOVEREIGN_RPC_WORKERS=$STRIX_IP:50052 \
SOVEREIGN_RPC_TENSOR_SPLIT=0.7,0.3 \
  <daemon-or-example> ...            # 0.7 to the Strix worker, 0.3 local Metal
```

(For a quick check without the full daemon, the `complete` example takes the same
env: `cargo run -p sovereign-inference --example complete -- --model <gguf> --prompt "hi"`.)

## Verify distribution

tok/s is **not** a reliable signal (a fast CPU/loopback hides offload). Use the
**worker's GPU/RSS**: during a host inference the Strix worker's VRAM usage jumps
by ~the split fraction of the model, and its daemon logs `Accepted client
connection` held for the duration. On the Mac, the same is observable as worker
process RSS (e.g. split `0.9` → worker holds ~90% of the model).

## Transfer cost (and the cache)

llama.cpp RPC is **host-loads-all**: the host opens the GGUF and streams each
worker's layer weights to it at load time. So a cold load transfers
`split_fraction × model_size` to each worker (e.g. 0.8 × 30GB = 24GB). Important:

- **Not per request.** Once loaded, the worker holds its shard resident; every
  subsequent inference reuses it. Per-token traffic is just layer-boundary
  **activations** (KB).
- **Not per reload, if cached.** The worker caches received weight tensors
  (>10MB) to local disk by content hash. On a warm reload the host sends hashes;
  the worker already has them ⇒ **zero weight transfer** (measured: a 9B model
  cached 98 tensors / 3.6GB cold, re-sent 0 on reload). The cache is **on by
  default** at `~/.sovereign/rpc-cache`; `SOVEREIGN_RPC_CACHE_DIR=off` disables it.

So the cold first-load is the only time the weights cross the wire. This is why
distribution earns its keep on **can't-fit-one-node** models (MiniMax-M2.7 140GB,
Kimi K2.6) — the one-time transfer amortizes over a long-lived load — and is a
*net loss* for a model that already fits one node (you've added a network hop).

## Offline cache pre-warming (no cold transfer at all)

If even the cold transfer is a problem — remote nodes on a **metered/throttled
ISP link** — you can eliminate it. The cache is just content-addressed files
(`<016x FNV-1a of the tensor bytes>`), and those hashes can be computed **directly
from the GGUF**, offline:

```bash
# hand the node the GGUF on a thumbdrive / over a direct cable, then:
sovereign mesh warm-cache <model.gguf>      # → ~/.sovereign/rpc-cache, no network, no GPU
```

This parses the GGUF and writes one cache file per weight tensor >10MB. When the
cluster runs, the host's tensor-hash requests are all cache hits and **zero
weight bytes cross the wire** — verified: a GGUF-warmed cache served a real load
re-sending 0 tensors. It's effectively "the worker reads its own GGUF" (which
mainline RPC can't do directly), achieved offline via the cache. Scales to any
size (streams tensor-by-tensor, no VRAM). Two distribution patterns:

- **Sneakernet the cache:** generate it once on any node, copy `~/.sovereign/rpc-cache/`
  to a thumbdrive, drop it on every node (content-addressed ⇒ one cache fits all).
- **Sneakernet the GGUF + warm locally:** ship the GGUF, run `warm-cache` on each
  node. Same result, smaller to carry if nodes already have the GGUF.

Note: LAN / direct-cable transfers between co-located machines never touch your
ISP — pre-warming matters only when workers are *remote* (the real 8-node case).

## Automatic shard warming (the host seeds the workers)

The cache above makes a distributed *load* cheap — but a **large** model can't
even take the cold streaming path: the host wedges in `send()` when it streams a
worker's share over a real network above ~800 MB (an upstream llama.cpp RPC
flow-control deadlock; `-dio` / `--no-mmap` don't fix it). So the daemon **never
streams a large shard**. Instead, before a distributed load, the host **auto-warms**
each worker's cache and then loads with explicit placement — no manual step:

1. **Never-wedge guard.** `ModelSlot::load` decides per model: a small model
   (≤ `SOVEREIGN_RPC_SAFE_STREAM_MB`, default 512) streams safely; a large model
   distributes **only** against warm caches; anything uncertain loads
   **local-only** — the load never wedges. Only the **primary** slot ever
   distributes (fast / embed / code stay local — distributing them is what
   crashed the worker under concurrent multi-slot load).
2. **One plan.** The host computes a contiguous block→device placement once
   (`plan_shards`, weighted by each worker's advertised VRAM) and derives BOTH
   the load-time `-ot` overrides AND each worker's warm assignment from that same
   plan — so warm-time and load-time placement can't diverge (every weight a hit).
3. **Seed each worker.** The host `POST`s `/internal/rpc-warm` to each worker
   (internal port, tailnet-only) with its `device_index` + the plan. The worker
   warms its shard:
   - **whole-GGUF** (default): warm from the model it already holds (or fetches
     once from the host), hashing only its own blocks; or
   - **byte-range** (`SOVEREIGN_RPC_SHARD_FETCH=ranges`): `Range`-GET only its
     tensors from the host's `serve_model_file` and verify each by hash —
     `O(model/N)` on disk, the 500 GB × N-node endgame.
4. **Load.** Once every worker reports warm, the host loads with the overrides —
   all `SET_TENSOR_HASH` cache hits, zero bulk send, no deadlock.

This **retires the manual `SOVEREIGN_RPC_ASSUME_WARMED`** for the common case (it
remains an operator escape hatch to skip the warm step and assert the shards are
already warm). The demo-natural topology now just works: join mesh → the host
distributes the big primary → workers seed their shards → tokens.

## Notes & limits

- **Protocol match:** host and worker must run the **same llama.cpp version**
  (pinned b9180). Both built from this workspace ⇒ automatically matched. Distro/
  Homebrew llama.cpp usually lacks `-DGGML_RPC=ON` and won't work.
- **Loopback on one machine:** a worker serving the *same physical GPU* the host
  uses (Metal↔Metal / Vulkan↔Vulkan on one box) aborts (buffer aliasing). Fine
  across two nodes; for a single-box test point the worker at a CPU device.
- **Interconnect** is the throughput variable: a direct TB4/USB4 link or 10GbE
  ≫ Tailscale-over-Wi-Fi for the per-token activation traffic.
- **Fallback (no daemon):** `scripts/build-rpc-worker.sh` builds a standalone,
  version-matched `rpc-server` if you need a worker without running the daemon.

## Tuning env vars (host side)

| Env | Default | Effect |
|---|---|---|
| `SOVEREIGN_RPC_SAFE_STREAM_MB` | `512` | Below this the model streams (safe); above it distributes only against warm caches, else loads local-only. |
| `SOVEREIGN_RPC_ASSUME_WARMED` | unset | Escape hatch: assert the workers' shards are already warm and skip auto-warm. |
| `SOVEREIGN_RPC_SHARD_FETCH` | `whole` | `ranges` → workers byte-range-fetch only their shard (`O(model/N)` disk) instead of holding the whole GGUF. |
| `SOVEREIGN_RPC_MODELS_DIR` | `~/.sovereign/models` | Where a worker fetches a whole GGUF into when it doesn't already hold the model. |
| `SOVEREIGN_RPC_WORKER_SETTLE_SECS` | `90` | A discovered worker must be continuously advertised this long before the host distributes to it (eligibility settle). |
| `SOVEREIGN_RPC_WORKER_FLAP_THRESHOLD` / `…_FLAP_WINDOW_SECS` | `3` / `600` | Appear↔disappear cycles within the window that quarantine a worker. |
| `SOVEREIGN_RPC_WORKER_COOLDOWN_SECS` / `…_MAX_COOLDOWN_SECS` | `60` / `600` | Quarantine cooldown — linear backoff `cooldown × count`, capped. |

## Robustness — worker eligibility + the supervision contract

Distributed inference couples the host's stability to the remote worker: **any
RPC touch of a worker that has died** makes ggml's RPC client `GGML_ABORT`
(`RPC_STATUS_ASSERT`) — upstream and **uncatchable in-process** — killing the
**whole host daemon**. This has *two* faces, both observed live cross-machine
(2026-06-06): **graph compute** against the dead worker (`ggml-rpc.cpp:491`; an
inference hit a worker that had died mid-session), **and** the buffer-free /
device-query during model **teardown on the next reload** (`ggml-rpc.cpp:379`) —
which fired with **no inference in flight**, when the prune-reload tore down a
model still sharded across the just-dead worker (and is nondeterministic: an
earlier identical prune got lucky). Any in-flight request is lost. The defences:

- **Eligibility gate** (`sovereign-mesh::worker_eligibility`). The host
  distributes only to PROVEN-STABLE workers. A freshly-discovered worker is
  *Probationary* until continuously advertised for `…_SETTLE_SECS`; a worker that
  flaps (`…_FLAP_THRESHOLD` cycles in `…_FLAP_WINDOW_SECS`) is *Quarantined* with
  linear backoff. Only eligible workers reach the reload decision **and** the
  `-ot` plan, so a flapping worker can neither thrash the reload loop nor get a
  shard pinned onto it. (Before this, one flapping worker drove **11 reloads in
  27 min** and aborted the host mid-benchmark.) Transitions log at INFO
  (`worker-eligibility: …`) and show in `sovereign mesh status`.
- **Host supervision is required.** Because the abort is uncatchable, run the
  daemon under a supervisor that restarts it — `sovereign install-service`
  installs exactly that (systemd `Restart=on-failure`; launchd `KeepAlive`). The
  eligibility gate minimizes how often it fires.
- **Stable shard plan.** The plan is cached per `(model, eligible worker set)`
  and VRAM is quantized to coarse buckets, so a reload across the same workers
  reuses the identical assignment — workers' warm caches stay valid (no re-warm
  churn that the live-VRAM re-plan used to cause).

## Status

- ✅ **Manual env** — `SOVEREIGN_RPC_WORKERS` + optional `SOVEREIGN_RPC_TENSOR_SPLIT`.
- ✅ **Auto-discovery** — the host scans peers' `/status` for advertised workers
  (`SOVEREIGN_RPC_DISCOVER=1`); no manual list, split derived from advertised VRAM.
- ✅ **Never-wedge guard + auto-warm** — large models never stream (no `send()`
  deadlock); the host seeds each worker's shard, then loads with owned `-ot`
  placement. Primary-slot-only. Retires the manual `SOVEREIGN_RPC_ASSUME_WARMED`.
- ✅ **Byte-range shard fetch** (`SOVEREIGN_RPC_SHARD_FETCH=ranges`) — a worker
  materializes only `O(model/N)` on disk; for models too big for one node.
- ✅ **Worker eligibility + plan stability** — distribute only to proven-stable
  workers (settle + flap-quarantine); reload + `-ot` plan exclude quarantined
  workers; shard plan cached per worker set. See *Robustness* above.

Open: per-tensor `SET_TENSOR_HASH` round-trips dominate load time on high-RTT
links (a perf note for large models, not a correctness issue); a worker that dies
while a model is sharded across it still aborts the host (upstream ggml) on the
next RPC touch — graph compute (`:491`) or the prune-reload's teardown (`:379`) —
mitigated by the eligibility gate + required host supervision, not eliminated. Two
deferred follow-ups would *narrow* (not close) this exposure: **(1)
shrink-fast-prune** — prune a disappeared worker immediately, skipping the
grow-debounce for shrinks only (quarantine still prevents re-add thrash), cutting
the inference-abort window from ~one discovery+debounce cycle to ~one discovery
tick; **(2) dead-backend teardown** (vendor `llama-cpp-4` / ggml) — mark a crashed
RPC backend so its buffer-frees no-op, the only thing that eliminates the `:379`
teardown-abort. Supervision remains the contract until then.
