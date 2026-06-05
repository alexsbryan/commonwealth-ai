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
