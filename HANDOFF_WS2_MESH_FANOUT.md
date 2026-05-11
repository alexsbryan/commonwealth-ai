# WS2 — Cloud-Peer Mesh-Join + 3-Way Fanout — Resume Notes

Session continued from the pre-monorepo repo at `~/dev/commonwealth-ai/`.
All file paths below are relative to this monorepo root unless stated otherwise.

## Where this picks up

We are building a production-grade, flexible mesh that uses SEP atlas
enrichment as the test workload. End goal: a friend can join the mesh,
contribute, and be productive in 5 minutes. SEP is the hardening
target, not the deliverable.

Five workstreams (named WS1–WS5). Status as of this handoff:

| WS  | Subject                                    | Status   |
|-----|--------------------------------------------|----------|
| WS1 | Routing reliability (IPv4 preference, named-model routing) | ✅ shipped |
| WS2 | Cloud-peer mesh-join validation (Vast.ai)  | ⚠️ in progress — mesh-join works, but advertise-IP bug blocks ongoing gossip |
| WS3 | Scheduler self-healing (peer quarantine)   | ✅ shipped |
| WS4 | Security hardening (5-min friend onboarding precondition) | not started |
| WS5 | 5-min friend onboarding flow               | not started |

## Sovereign daemon — routing changes shipped this session

All in `sovereign/crates/sovereign-mesh/src/peer_inference.rs`.

### 1. Load-aware named-model routing

`locate_named_model` no longer short-circuits to local on a self-manifest
hit. Instead it collects every node that advertises the requested model
(self + reachable, non-quarantined peers) and picks the one with the
lowest in-flight count for that model. Local wins ties.

- Per-model local counter: `local_inflight_by_model: Arc<std::sync::Mutex<HashMap<String, u32>>>`.
- `LocalInflightGuard` RAII type increments on entry, decrements on Drop —
  works across `?`, panic, stream-cancel.
- `InflightGuardedStream` adapter holds the guard for the lifetime of
  streamed responses.

Live verification (laptop + Mac, no Vast):

```
mesh-inference: serving complete() locally by explicit model name
mesh-inference: peer wins load-balance for explicit model
  model=FINAL-Bench_Darwin-36B-Opus-Q6_K local_inflight=1
  peer=Alexs-MacBook-Pro-2.local peer_inflight=0
mesh-inference: routing complete() to peer by explicit model name
```

4 concurrent probes → 1 local + 3 Mac, no queuing on a single primary slot.
Single-peer SEP slug latency dropped from ~1100 s to ~310 s (≈3.5×).

### 2. Peer in-flight observation (symmetric load tracking)

The explicit-model `Peer` non-streaming branch now calls
`record_dispatch(Some(&peer.name))` before HTTP dispatch and
`record_success` / `record_failure` after. Without this,
`peer_observations[peer].in_flight` stayed at 0 and every concurrent
request after the first one stampeded onto whichever peer sorted first
(Mac), causing transient 503s from Mac overload.

**Still TODO**: streaming peer branch and OICP-driven (non-explicit-model)
peer branches don't yet call `record_dispatch`. Same fix pattern applies;
just wasn't urgent for SEP.

## Container changes shipped this session

`sovereign/container/Containerfile.cuda`:
- NCCL disabled at compile time via sed-patch on llama.cpp's
  `GGML_CUDA_NCCL` CMake option. Vast.ai marketplace hosts have
  heterogeneous CUDA driver versions; NCCL's `ncclCommInitAll`
  hard-fails when driver/runtime/NCCL don't line up. Single-GPU pods
  don't need NCCL.
- `cuda-preflight` binary: tiny `cudaMalloc` probe burned at image
  build time and run from the entrypoint BEFORE the 38 GB R2 sync.
  Catches driver/runtime mismatch in 1 s instead of after 5+ min of
  wasted download.

`sovereign/container/entrypoint.sh`:
- Patient tailnet-reach beacon (12 × 5 s retry) confirming the founder
  is reachable before the heavy sync.
- Tailscale started with **both** outbound proxies:
  `--socks5-server=localhost:1055` AND
  `--outbound-http-proxy-listen=localhost:1080`.
- Env wiring:
  - `HTTP_PROXY=http://localhost:1080` / `HTTPS_PROXY=…` →
    reqwest uses tailscale's HTTP CONNECT proxy. Required because
    `sovereign/Cargo.toml` does **not** enable the `socks` feature on
    reqwest; with only `ALL_PROXY=socks5h://…`, reqwest treats the SOCKS
    port as an HTTP proxy and sends CONNECT bytes, which tailscaled
    correctly rejects with "incompatible SOCKS version".
  - `ALL_PROXY=socks5h://localhost:1055` → preserved for curl/rclone.
  - `NO_PROXY=localhost,127.0.0.1,0.0.0.0` → loopback stays direct.
- Mesh-join URL synthesis: when `MESH_JOIN_LINK` is a bare
  `cwth-…` key, the entrypoint rewrites it as
  `sovereign://join/<key>?relay=$MESH_SEED_ADDR` so the
  `try_single_peer(&http, &authority, &body)` path in
  `crates/sovereign-mesh/src/join.rs` has a direct hint instead of
  relying on mDNS (which can't see across NAT).

## Vast.ai validation results

**Pod 36497141** (L40S Virginia, machine 91131, $0.56/hr, 65 Gbps inet):

1. ✅ `cuda-preflight: OK (1 GPU, runtime functional)`
2. ✅ Tailscaled up with both proxy listeners.
3. ✅ Tailnet IP `100.112.195.45` assigned. Beacon green on attempt 1.
4. ✅ Model sync from R2 — 38 GB at ~60 MiB/s (≈10 min).
5. ✅ Sovereign daemon started. Gossip loop spinning.
6. ✅ **Mesh-join succeeded via HTTP proxy**:
   ```
   handshake_sent: direct-peer hint, POST /internal/join peer=100.115.12.21:9742
   handshake_accepted: joined mesh via direct hint
     peer=100.115.12.21:9742 assigned_node_id=node-cbb019881cefded3
   gossip: reach ok peer=node-44ae76142b0c3c72 (laptop)
   gossip: reach ok peer=node-b88252e4325bc377 (Mac)
   ```

That's the WS2 core hypothesis validated: a Vast.ai pod behind broken
NAT can join the laptop's mesh by tunneling reqwest through tailscale's
HTTP CONNECT proxy, which falls back to DERP relay internally.

### The remaining bug (where this stopped)

The Vast daemon advertises **the wrong self-address** to peers:

```
mesh-inference: peer manifest transport error — trying next
  peer=81a035eda1d4 url=http://172.17.0.3:9741/oicp/v1/capabilities
  error=error sending request for url …
gossip: peer marked Offline (stale last_seen)
  peer=node-cbb019881cefded3 addrs=[172.17.0.3:9742]
```

`172.17.0.3` is the Vast container's **Docker bridge IP**, not its
tailnet IP. The daemon's self-advertise logic picks the first usable
interface and prefers the docker bridge over the tailscale userspace
netstack. The laptop can't reach `172.17.0.3` (it's the container's
internal address), so manifest fetches fail and the peer is marked
Offline 60 s after joining.

The pod was destroyed before debugging this further. Sync work is gone;
WS2 needs another spin-up to land. Estimated cost: ~$0.10–0.15 per
launch ($0.56/hr × ~10 min sync).

## Next steps to resume WS2

In rough order:

1. **Fix self-advertise** so the daemon prefers the tailscale interface.
   Two options:
   a. Add `advertise_addr` to the daemon's `[mesh]` config section and
      have the entrypoint set it to `$(tailscale ip -4)` before
      starting the daemon. Cheapest, no Rust changes needed beyond
      reading the env or config field.
   b. Make the daemon's interface enumeration filter docker bridge
      addresses (`172.17.0.0/12`, `10.0.0.0/8`, etc.) when a
      `100.64/10` tailnet address is available.
   Option (a) is the lowest-risk, most explicit fix.

2. **Rebuild + push** `ghcr.io/alexsbryan/sovereign-cuda:latest`.

3. **Relaunch** a Vast pod (the prior offer 35866039 worked end-to-end
   modulo the advertise bug; machine_id 91131 is reliable).

4. **Verify** the laptop's daemon log shows
   `mesh-inference: fetched peer manifest peer=<vast-name>` with the
   `100.x.x.x` tailnet IP.

5. **Run the 3-way fanout test** — 6 concurrent probes; expect 2 each
   on laptop + Mac + Vast.

6. Land the **streaming-peer / OICP-peer `record_dispatch`** parity
   work so the load-balance signal stays symmetric across all four
   peer dispatch sites.

## Mesh credentials & runtime state to be aware of

- Laptop's tailnet IP: `100.115.12.21` (fedora hostname).
- Mac's tailnet IP: `100.104.36.28` (alexs-macbook-pro).
- Active join key when the prior Vast pod joined: `cwth-1459-5989-b961`.
  A new key (`cwth-743f-3d87-3ad1`) was rotated during this session;
  per `sovereign-cli mesh rotate` semantics it invalidates the previous
  key once the daemon reloads from disk. The currently-running daemon
  was launched *before* the rotate, so the old key was still accepted
  for the test pod. After the next daemon restart the new key takes
  over.
- `~/.sovereign/join_key.secret` is a credential — do not cat into
  logs/transcripts.

## SEP fanout state

- Queue: `/tmp/sep_mesh_queue.txt` — 1446 slugs pinned to
  `FINAL-Bench_Darwin-36B-Opus-Q6_K`.
- Control script: `/tmp/sep_mesh_control.sh start|pause|status|tail`.
- Atomic unit of completion is the per-slug `atoms.json`; re-running
  `start` skips already-done slugs. Mid-flight pauses lose the
  in-flight slugs' Phase 1 progress (cheap; ~5 min/slug).
- Before the routing fix landed, per-slug Phase 1 was ~18 min. After,
  it's ~5 min on a 2-node mesh, and would be ~3 min on a 3-node mesh
  with Vast online.

## Files changed this session

```
sovereign/container/Containerfile.cuda
sovereign/container/entrypoint.sh
sovereign/crates/sovereign-mesh/src/admin_http.rs       (primary_pool: None test fixture)
sovereign/crates/sovereign-mesh/src/daemon.rs           (primary_pool: None test fixture)
sovereign/crates/sovereign-mesh/src/peer_inference.rs   (load-balance routing + per-model in-flight + record_dispatch)
```

The two test-fixture changes (admin_http + daemon) were already at HEAD
in this monorepo so the diff carried over here only touches the three
substantive files.

## Models directory

`sovereign/models/` (261 GB, 13 GGUFs) was moved from the old repo via
`mv` (same filesystem, instant). No re-sync required.
