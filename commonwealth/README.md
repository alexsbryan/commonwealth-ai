# Commonwealth

Commonwealth is the mesh layer underneath Sovereign. It handles the parts that let several machines work together as one mesh: discovery, gossip, scheduling inference across nodes, sharing knowledge indexes, and replicating a small amount of shared state.

Most people never run it on its own. Sovereign embeds it in-process through the `sovereign-mesh` crate, so you create and join meshes with `sovereign mesh create` and `sovereign mesh join`, and the daemon you already run becomes a node. There is also a standalone `commonwealth` binary, built from `commonwealth-daemon`, for a headless peer — a cloud GPU box that joins a mesh to lend compute with no desktop attached. That case is in [CLOUD_PEER_DEPLOY.md](../sovereign/docs/CLOUD_PEER_DEPLOY.md).

If what you want is to pool machines with people you trust, the Sovereign side is where to start: [Run a model bigger than your machine](../docs/RUN_A_BIGGER_MODEL.md).

## How it works

A mesh is symmetric — there is no master node, and every node runs the same code. Nodes find each other over mDNS on a local network, or through gossip across a Tailscale or WireGuard network. From there Commonwealth assigns model layers to machines by capacity, routes each request to a node that can serve it, shares knowledge indexes where the source license allows, and replicates shared state. When a machine sleeps or drops off, the remaining nodes notice and the mesh reforms around them. It serves an OpenAI-compatible API on `localhost:9741`, so a client that speaks that protocol can use the mesh as one local model. The capability-aware routing is described in [oicp-v0.4.md](docs/oicp-v0.4.md).

The trust model is social rather than cryptographic: you join a mesh because someone you know invited you. It does not try to defend against malicious nodes. There is no token, no blockchain, and no central registry — each node holds all the state there is.

## The crates

- `commonwealth-core` — shared types: ids, mesh state, capabilities, ledger, aliases.
- `commonwealth-transport` — the PeerTransport seam, mapping a (peer, traffic class) to endpoints; IP today, iroh-ready.
- `commonwealth-discovery` — mDNS, gossip, latency probing, hardware detection, TLS, mesh peering.
- `commonwealth-inference` — scheduling and orchestration: which model runs where, and the process lifecycle around it.
- `commonwealth-api` — the HTTP servers: client-facing on 9741 (bearer token for non-loopback callers), internal on 9742 (no per-request auth — perimeter-trusted, meant to ride a private network or WireGuard/Tailscale overlay).
- `commonwealth-knowledge` — corpus-engine integration: install, shard, and search corpora across the mesh.
- `commonwealth-app` — the mesh-app platform: manifests, lifecycle, registry, proxy.
- `commonwealth-state` — MeshStore, a gossip-replicated SQLite key-value store with TTL-based GC.
- `commonwealth-daemon` — the CLI entry point and the standalone `commonwealth` binary.
- `commonwealth-test-harness` — a simulated multi-node mesh and a mock llama-server for integration tests.

The full design is in [ARCHITECTURE.md](ARCHITECTURE.md); the system-wide map is [SYSTEM_OVERVIEW.md](../sovereign/SYSTEM_OVERVIEW.md).

## Working on it

From the repo root, `cargo test --workspace` runs the suite. The test harness simulates multi-node meshes, so it needs no real hardware or model weights.

## License

[AGPL-3.0-or-later](../LICENSE), one license across the monorepo. The network-use clause is the part that applies here, since Commonwealth is the network service.
