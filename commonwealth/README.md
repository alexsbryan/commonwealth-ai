# Commonwealth

**Pool your computers. Share the intelligence.**

Commonwealth is a coordination daemon that lets a group of people combine their desktop machines into a single, powerful AI system. Five neighbors with ordinary hardware can collectively run models too large for any one machine and share knowledge bases too big for any one disk — while every participant's experience feels like they're using one seamless local system.

No tokens. No blockchain. No cloud service. No telemetry. Just people who trust each other, pooling what they have.

## The Problem

You have a decent computer. Your friend has a decent computer. Separately, each of you can run a 7B model — useful, but limited. Together, your combined VRAM could run a 70B model that's qualitatively better at code, analysis, and creative work. But "together" has always meant either (a) one of you hosts everything and the other freeloads, or (b) you both set up a complicated distributed system that neither of you wants to maintain.

Commonwealth makes "together" trivially easy.

## What It Does

```
$ commonwealth init --name "Sunset District Co-op"

Mesh created: Sunset District Co-op
Join key: cwth-7f3a-9b2e-4d1c

Share this key with people you want in the mesh.
They run: commonwealth join cwth-7f3a-9b2e-4d1c
```

Once your friends join, Commonwealth automatically:

- **Discovers** everyone's hardware (GPUs, VRAM, storage, network speed)
- **Schedules** model layers across machines proportional to each one's capability
- **Serves** an OpenAI-compatible API on `localhost:9741` that any client can talk to
- **Routes** requests to the right model based on what the request needs (coding, analysis, creative work)
- **Recovers** automatically when someone's machine goes to sleep or restarts
- **Tracks** who contributes and who consumes, making freeloading visible

From any client's perspective — Sovereign (releasing soon!), Open WebUI, a `curl` command — the mesh looks like a single local API endpoint. The distributed orchestration is invisible.

## A Concrete Example

Five people in a neighborhood:

| Person | GPU | VRAM | What they get alone | What they get with Commonwealth |
|--------|-----|------|--------------------|---------------------------------|
| Alice | Strix Halo | 32 GB | 14B model | 70B model, shared knowledge |
| Bob | RTX 4090 | 24 GB | 14B model | 70B model, shared knowledge |
| Carol | M3 Ultra | 192 GB | 70B model (slow) | 70B model (fast, sharded), knowledge host |
| Dave | 2× RTX 3090 | 48 GB | 30B model | 70B model, shared knowledge |
| Eve | MacBook Air | 16 GB | 7B model | 70B model, shared knowledge, 4 GB local footprint |

Pooled: **~312 GB VRAM**, **~4.3 TB storage**. Eve's MacBook Air stores 4 GB locally and accesses everything the mesh offers.

## Design Principles

Three constitutional constraints. A contribution that violates them is rejected regardless of technical merit.

**This is a commons, not a product.** Apache 2.0 license. No moat. Forking is trivially easy. There is nothing to capture — no central registry, no privileged node, no state that doesn't exist on every participant's machine.

**Social trust, not cryptographic verification.** You join a mesh because someone you know invited you. The threat model is "my neighbor's kid started a game and their node slowed down," not Byzantine fault tolerance. If your threat model requires adversarial resistance, Commonwealth is not your project.

**The daemon coordinates. It does not infer or index.** Commonwealth orchestrates [llama.cpp](https://github.com/ggml-org/llama.cpp) for inference and SQLite for knowledge search. When llama.cpp improves, Commonwealth benefits automatically. The daemon is the nervous system. The muscles are someone else's well-maintained project.

## How It Works

Commonwealth runs as a daemon on every participating machine. There is no master node — the mesh is symmetric.

```
Client (Sovereign, Open WebUI, curl)
         │
         │ HTTP :9741
         ▼
┌─────────────────────────────────────┐
│        Commonwealth Daemon          │
│                                     │
│  API Layer ──── Scheduler           │
│  (OpenAI-     (which model,         │
│  compatible)   which nodes)         │
│       │             │               │
│  Membership ── Orchestrator         │
│  & Discovery   (process mgmt)      │
│  (mDNS,             │              │
│  gossip)      llama-server          │
│               rpc-server            │
└─────────────────────────────────────┘
```

- **Discovery**: Nodes find each other via mDNS on the LAN, or transitively via gossip over a VPN (Tailscale/WireGuard)
- **Gossip**: Epidemic protocol, 10-second intervals. A 100-node mesh converges in under a minute
- **Scheduling**: Layers assigned proportional to VRAM, contiguous per node, topology-aware to minimize cross-node latency
- **Fault tolerance**: Node departs → mesh recovers within 15 seconds. Graceful pause → zero dropped requests
- **Fairness**: Append-only contribution ledger tracks compute, storage, and bandwidth. The group decides their policy

## Installation

```bash
curl -sSf https://commonwealth.dev/install.sh | sh
```

Or build from source:

```bash
git clone https://github.com/commonwealth-rs/commonwealth
cd commonwealth
cargo build --release
# Binary at target/release/commonwealth
```

### Prerequisites

- [llama.cpp](https://github.com/ggml-org/llama.cpp) — `llama-server` and `rpc-server` binaries
- A GPU supported by llama.cpp (NVIDIA CUDA, AMD ROCm, Apple Metal, or Vulkan)
- For cross-LAN meshes: [Tailscale](https://tailscale.com/) or WireGuard

## Quick Start

**Create a mesh:**
```bash
commonwealth init --name "Our Co-op"
# → Join key: cwth-7f3a-9b2e-4d1c
```

**Join from another machine:**
```bash
commonwealth join cwth-7f3a-9b2e-4d1c
```

**Check status:**
```bash
commonwealth status
```

**Start the daemon:**
```bash
commonwealth daemon start
```

**Use it** — point any OpenAI-compatible client at `http://localhost:9741/v1`:
```bash
curl http://localhost:9741/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"messages": [{"role": "user", "content": "Hello from the mesh!"}]}'
```

See the [Getting Started Guide](docs/getting-started.md) for a complete walkthrough including network setup.

## CLI Reference

```
commonwealth init --name "..."          Create a mesh, get a join key
commonwealth join <key>                 Join an existing mesh
commonwealth status                     Mesh state, members, models, capacity
commonwealth balance                    Contribution ledger
commonwealth models                     Available and loaded models
commonwealth corpora                    Hosted knowledge bases
commonwealth pause / resume             Graceful departure and return
commonwealth leave                      Permanent departure
commonwealth logs [--follow]            Daemon logs
commonwealth mesh members               List members with status
commonwealth mesh set <key> <value>     Propose config change
commonwealth mesh revoke <node>         Propose removing a member
commonwealth mesh peer <key>            Establish peering with another mesh
commonwealth daemon start/stop/status   Daemon lifecycle
```

## Client Integration

Commonwealth serves any client that speaks the OpenAI chat completions protocol.

### With Sovereign

```toml
# In Sovereign's config
[providers.remote]
type = "remote_api"
url = "http://localhost:9741/v1"
```

Sovereign sends OICP capability requirements per-request. Commonwealth routes to the best available model automatically.

### With Open WebUI / LiteLLM / Any OpenAI Client

Point the base URL at `http://localhost:9741/v1`. No OICP required — clients that don't send capability requirements get the default model.

### OICP (Open Inference Capabilities Protocol)

Commonwealth implements OICP for capability-aware routing. Clients can request specific model strengths:

```json
{
  "messages": [{"role": "user", "content": "Write a Rust function"}],
  "oicp": {
    "capabilities": {
      "required": {"code": 2},
      "preferred": {"code": 4, "instruction": 3}
    }
  }
}
```

The mesh routes this to a coding model if one is loaded, or the best available alternative. See [docs/oicp.md](docs/oicp.md) for the full specification.

## Architecture

Commonwealth is a Cargo workspace with seven crates:

| Crate | Purpose |
|-------|---------|
| `commonwealth-core` | Shared types, mesh state, shard plans, ledger, OICP |
| `commonwealth-discovery` | mDNS, gossip protocol, latency probing, hardware detection, TLS, mesh peering |
| `commonwealth-scheduler` | Layer assignment, knowledge sharding, model portfolio, OICP caching, leader election |
| `commonwealth-orchestrator` | llama.cpp process lifecycle, health monitoring, fault detection, graceful departure |
| `commonwealth-api` | Axum HTTP server, OpenAI-compatible + OICP endpoints, knowledge search |
| `commonwealth-daemon` | CLI entry point, config loading, signal handling |
| `commonwealth-test-harness` | Simulated multi-node meshes, mock llama-server, integration tests |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the complete technical design.

## Platform Support

| Platform | GPU Support | Notes |
|----------|-------------|-------|
| Linux x86_64 | CUDA, ROCm, Vulkan | Primary platform |
| Linux aarch64 | Vulkan | ARM servers |
| macOS ARM | Metal | Apple Silicon (M1-M4) |
| macOS x86_64 | — | Intel Macs (limited GPU) |
| Windows x86_64 | CUDA, Vulkan | Via WSL2 or native |

The daemon itself is pure Rust with no GPU dependencies — it manages processes that use GPUs, not GPU APIs directly.

## What This Doesn't Do (Intentionally)

- **No training or fine-tuning.** Inference and knowledge serving only.
- **No model hosting.** Models download from Hugging Face or transfer from mesh peers.
- **No Byzantine fault tolerance.** Social trust model.
- **No NAT traversal.** Use Tailscale or WireGuard.
- **No incentive token.** Constitutional constraint.
- **No centralized anything.** No master node, no registry, no cloud service.

## Contributing

Commonwealth is Apache 2.0 licensed. Contributions welcome.

```bash
cargo test --workspace          # Run all 239 tests
cargo clippy --workspace -- -D warnings  # Lint
cargo fmt --all --check         # Format check
```

## License

Apache 2.0. See [LICENSE](LICENSE) for details.
