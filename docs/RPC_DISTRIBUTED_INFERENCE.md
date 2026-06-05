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
| **Host** | `SOVEREIGN_RPC_WORKERS=<ip>:50052,<ip2>:50052` | Daemon registers those workers and splits the model's layers across local GPU + workers. |
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

## Roadmap

This is the manual-env increment. Next: advertise the worker endpoint + GPU
memory through mesh `NodeCapabilities` gossip so the host **auto-discovers**
workers (no `SOVEREIGN_RPC_WORKERS`) and derives the split from advertised VRAM —
fully zero-config (join mesh → share GPU → cluster uses it).
