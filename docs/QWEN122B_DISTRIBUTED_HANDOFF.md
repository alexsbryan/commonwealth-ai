# Distributed inference (Qwen-122B heterogeneous) — investigation handoff

**Date:** 2026-07-18 · **Author:** pairing session (Alex + Claude) · **Tree HEAD at handoff:** `b4c39cb2`
**Status at the time of writing:** 2 of 3 blockers fixed (uncommitted). **Superseded — read the
2026-07-26 block below first.** All three blockers are now fixed and committed; the cross-network
question §6 poses has been answered by a working tunnel, not by argument.

> ## ⚠️ SUPERSEDING UPDATE — 2026-07-26
>
> **Blocker #3 is CLOSED and the "LAN-only" framing throughout this document is obsolete.** Commit
> `5c9ccf86` landed the ggml-RPC-over-iroh path (`RPC_ALPN cwth/rpc/0` + `TrafficClass::RpcTensor`),
> and a forced-tunnel E2E (host RuggedFox, worker BeefyMac, `SOVEREIGN_RPC_TUNNEL=always`) has since
> demonstrated it end to end. Evidence, twice, in `~/.sovereign/logs/rpc-tunnel-e2e.log`
> (04:03:26 and 04:11:07):
>
> ```
> discovered mesh RPC worker peer=BeefyMac via=iroh-bridge:iroh:127.0.0.1:37187→86627fd5
> registered mesh RPC device free_gb=20.97 total_gb=55.66      <- Metal VRAM read THROUGH the tunnel
> rpc-warm: worker shard warm via=iroh:…→86627fd5 written=0 already=20
> primary slot placement decided mode=distributed total_blocks=32 local_blocks=27
> distributed load: explicit tensor placement via -ot overrides
> ```
>
> `rpc_tensor` is a required iroh class with `require_encryption=true` and fails closed, so a pass
> here cannot be the LAN path wearing a tunnel's clothes.
>
> **Still unmeasured: decode.** No completion has yet run to completion across the tunnel, so there
> is **no tok/s number for the distributed cross-network path**. The one attempt (04:11:56) prefilled
> 29 tokens and was killed 15 s later by a worker-eligibility quarantine — a discovery defect, since
> fixed (`daemon.rs::reaffirm_plan`, `HttpBridge::retarget`), that had nothing to do with the
> transport. Getting that number is the next step, and until it exists §6's question 2 ("can the
> per-layer hop live on this path?") is answered only for the *relay floor* measured in the
> 2026-07-18 characterization (5.5–7.1 tok/s between two NATed home boxes), which is the worst case,
> not the cloud case (public IP → direct hole-punch).
>
> Read §5/§6 below as the reasoning that produced the fix, not as open work.

> **UPDATE (later 2026-07-18, post-characterization):** the §6 questions were answered by
> measurement (`commonwealth-transport/examples/tunnel_bench.rs`, see the transport-characterization
> memory/notes: iroh direct ≈ +2.4 ms per 16 KB round-trip — green light; relay floor 5.5–7.1 tok/s
> network ceiling — single-digit, measured two-machine both-relay-pinned), and **blocker #3's warm
> half is FIXED (task 5, uncommitted)**: the warm plane now rides `PeerTransport` — an
> endpoint→NodeId directory recorded at discovery (`EmbeddedDaemon.rpc_endpoint_nodes`), host-side
> warm POSTs resolved via `transport.endpoints(contact, TrafficClass::ModelTransfer)` (iroh bridge
> first, raw IP fallback, per-attempt `via=` logging), `host_node_id` in the warm request so the
> worker resolves its fetch bases through **its own** transport (`host_transport_bases` /
> `merge_bases` in `rpc_warm_http.rs`). Wire back-compat: old bodies parse (`#[serde(default)]`).
> **Task 6 also landed (same day, uncommitted):** ggml RPC activation traffic can now ride iroh —
> `RPC_ALPN cwth/rpc/0` + `TrafficClass::RpcTensor`; the worker's acceptor routes the ALPN to its
> local rpc-server and advertises `rpc_worker.iroh: true` on `/status` (additive); host discovery
> falls back to a bridge-local `127.0.0.1:<port>` endpoint when no direct IP answers
> (`SOVEREIGN_RPC_TUNNEL={auto|always|never}`, `always` forces the tunnel for E2E). The task-5×6
> loopback self-warm hazard is guarded (`raw_warm_fallback_allowed`). **E2E pending**: LAN
> forced-tunnel with the Mac (4B, expect ~40 tok/s), then a relay-grade pass. Path-quality gating
> at placement (refuse/warn on relay) remains future work — observability first.

> **Second-set-of-eyes ask (ANSWERED — kept for the reasoning; the direction was built and works,
> see the 2026-07-26 block):** the RPC-distribution path is **LAN-only** — it reaches peers by raw
> TCP/HTTP to their direct IPs. The use case that matters is **machines on different networks**,
> which forces everything over **iroh**. The good news is a generic TCP-over-iroh tunnel already
> exists in-tree (`IrohAcceptor`/`TcpBridge`); it just isn't wired to the RPC paths. The core
> questions are in [§6](#6-open-questions-for-the-reviewer). Please sanity-check the diagnosis and
> the direction before we build the transport work.

---

## 1. Goal & the use case that actually matters

- Experiment: [`QWEN122B_HETEROGENEOUS_EXPERIMENT.md`](./QWEN122B_HETEROGENEOUS_EXPERIMENT.md) — split
  Qwen3.5-122B-A10B (UD-Q5_K_XL, ~92 GB across 3 shards) across **Strix Halo (Vulkan)** + **BeefyMac
  (Apple Metal)**. A0 solo baseline already recorded (~19.2 t/s decode). We were chasing **B1 =
  distributed**.
- **The real target use case (operator, verbatim): "machines on *different networks* — end of
  story."** LAN co-location (both boxes on `192.168.1.0/24`) is just the test rig. Any fix that only
  works same-LAN does **not** satisfy the goal.
- **Hard constraint:** BeefyMac is **64 GB RAM**. It can only hold *its shard* (`SHARD_FETCH=ranges`,
  ~its VRAM-proportional share), never the whole 92 GB model. So "ship the whole GGUF to the worker
  and pre-warm" is off the table.

## 2. The failure chain (all glassbox-confirmed, not inferred)

The first several hours were spent *inferring* state from `free` deltas and t/s signatures — which
was wrong and wasted effort. The turning point was building observability (§4) and reading a `gdb`
backtrace. Everything below is from logs/stacks, not guesses.

| # | Bug | Evidence | Status |
|---|---|---|---|
| 1 | **Discovery resolved the worker to `127.0.0.1:50052`** — `discover_rpc_workers` derived the RPC host from the `/status` probe URL, which under iroh is a loopback proxy. | log: `discovered mesh RPC worker endpoint=127.0.0.1:50052` | ✅ fixed (LAN-only — see caveat) |
| 2 | **`model_bytes` = first-shard size (~10 MB)** — placement read `metadata(model_path)` where `model_path` is shard `00001-of-00003` (~10 MB header shard), not the 92 GB total. `10 MB < SAFE_STREAM_MB (512)` → `classify_placement` picked **`StreamSplit`** (bulk weight stream) instead of `OwnedOverrides` (warm-cache). Bulk stream → `ggml_backend_rpc_buffer_set_tensor` → the >800 MB RPC `send()` deadlock. | placement log `mode=stream-split`; `gdb` stack blocked in `ggml_backend_rpc_buffer_set_tensor ← load_all_data ← reload_primary`; after fix: `auto-warming worker shards … model_mb=87669` | ✅ fixed (root cause of the "hang") |
| 3 | **Warm control + data plane use raw HTTP to the iroh-only internal port** — `orchestrate_warm` POSTs `http://{worker_ip}:9742/internal/rpc-warm` and hands the worker `http://{host}:9742/internal/v1/models/file/…` range URLs. `:9742` is iroh/QUIC (UDP); internal HTTP only travels *over* iroh. | log: `auto-warm failed … POST http://192.168.1.2:9742/internal/rpc-warm ← Connection refused (os error 111)` → `placement decided mode=local` (fell back) | ✅ fixed + demonstrated over iroh (`5c9ccf86`; see the 2026-07-26 block) |

**Net:** with #1 and #2 fixed, the code now correctly (a) finds the worker, (b) classifies the 92 GB
model as "too big to bulk-stream → warm-cache path", and (c) *attempts* the warm. It fails at the warm
transport (#3), falls back to LocalOnly, and no shard ever reaches BeefyMac (this is why the operator
"saw no GETs" the whole time — the code never reached a working transfer).

## 3. Fixes landed (uncommitted — `git diff`)

8 files, +295/−13. `cargo build -p sovereign-cli-daemon` is clean (debug).

- **Fix #1 — discovery direct-IP.** `sovereign-mesh/src/daemon.rs::discover_rpc_workers` — added
  `reachable_rpc_endpoint()`: pick a **direct member IP** (private-LAN first, then CGNAT/Tailscale),
  reachability-probe it, fall back to the probe host. **⚠️ Caveat: this is a LAN-only patch.** It
  hardcodes a directly-dialable IP, which is exactly what breaks cross-network. See §5/§6 — for the
  real use case this endpoint should be an iroh-tunneled local address, not a peer IP.
- **Fix #2 — total sharded size.** `sovereign-inference/src/embedded/model_slot.rs::total_model_bytes`
  sums all `…-NNNNN-of-NNNNN.gguf` shards; `ModelSlot::load` uses it for the placement decision.
  This is the real bug — a split GGUF was classified by its ~10 MB header shard. **Deserves a unit
  test (tempfile) before commit — not yet written.**
- **Observability (the "good handle"):** placement is now **stated, never inferred.**
  - `sovereign-contracts/src/traits.rs`: `SlotPlacement { mode, total_blocks, local_blocks, workers[] }`
    + `WorkerPlacement { endpoint, blocks, holds_output }` + `ResidentSlot.placement`.
  - `sovereign-inference/src/embedded/rpc_distribution.rs`: `resolve_placement` is now a wrapper that
    logs `target:"placement"` ("primary slot placement decided mode=… local_blocks=… workers=…") and
    stashes a queryable global (`last_primary_placement()`); real logic moved to `resolve_placement_inner`.
  - `engine.rs::resident_slots` populates the primary's `placement`; `commonwealth-api/src/state.rs`
    mirrors the types; `inference_adapter.rs` maps across the seam → **`/status` reports placement**.
  - `sovereign-cli-daemon/src/lib.rs`: added `placement` to `DAEMON_TRACING_FILTER` (+ pinned in the
    filter test). **The observability *hole* we hit:** the daemon's *default* filter already includes
    `sovereign_inference`/`sovereign_mesh` at info, but our launch env set an explicit `RUST_LOG` that
    **dropped `sovereign_inference`**, blinding every placement/warm log. Lesson for whoever runs this:
    don't override `RUST_LOG` at launch, or include `sovereign_inference=info,sovereign_mesh=info,placement=info`.

## 4. How to reproduce / verify

Config (`~/.sovereign/config.toml`): primary = the 122B shard-00001, **plus a distinct tiny `fast`**
(`Qwen3.5-0.8B…`), `context_size = 8192`. The tiny fast is load-bearing — see §5 (reload OOM).

```sh
# Strix (host), inside the vulkan toolbox. Do NOT override RUST_LOG away from the default,
# or add: RUST_LOG=…,sovereign_inference=info,sovereign_mesh=info,placement=info
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 \
SOVEREIGN_RPC_DISCOVER=1 \
SOVEREIGN_RPC_SHARD_FETCH=ranges \
SOVEREIGN_RPC_WORKER_SETTLE_SECS=20 \
SOVEREIGN_SHARED_MODEL_HOST_NODE_ID=<strix-node-id> \
  target/debug/sovereign-cli-daemon daemon run
# BeefyMac (worker): SOVEREIGN_RPC_SERVE=0.0.0.0:50052 target/debug/sovereign-cli-daemon daemon run
```

Then watch:
- `grep 'primary slot placement decided'` in the daemon log → should read `mode=distributed` (post-fix-#2).
- `grep 'auto-warm'` → `auto-warming worker shards … model_mb=87669`, then the **`auto-warm failed …
  Connection refused`** at `:9742` (blocker #3).
- `curl localhost:9741/status` → the primary slot's `placement` object (log-independent).
- To pin a hang: `gdb -p <pid> -batch -ex 'thread apply all bt'` and look for the one thread with
  app frames (`reload_primary`, `ggml_backend_rpc_*`) — the rest are idle parked tokio workers.

Notes: run the daemon **unsupervised** (`target/debug/… daemon run`, no `daemon-supervised.sh`) for a
clean single attempt — the supervisor auto-restarts and, on the pre-fix-#2 OOM path, produced a
crash loop. Kill daemons **by explicit PID** (`kill -9 <pid>`); a `pkill -f 'sovereign-cli-daemon
daemon run'` matches your own shell's argv and kills the shell.

## 5. Two more things the reviewer should know (side-findings)

- **Reload 2× memory transient / the tiny-fast workaround.** With no distinct `fast`, `fast_path() ==
  primary_path`, so the daemon **eager-loads the 122B *as the fast slot*** (`distributable=false` →
  100 % local), then `reload_primary` redistributes — and the old 100 % model + the new load overlap
  → OOM (peaked 123 GB on a 125 GB box). The distinct tiny-`fast` sidesteps it (primary never
  eager-loads 100 %; it loads distributed on the discovery reload). **This is a real bug for
  Experiment 2** (a model that can't fit one box could never survive the 100 % boot). Proper fix:
  don't eager-load a large distributable primary at all (or free the old slot before the reload).
  `engine.rs:997` (`primary_is_alias`) / `reload_primary` (`engine.rs:~1955`).
- **Host must be a shared-model *anchor* to be elected host.** `should_host` elects from
  `eligible_anchors`, and `can_anchor` is set only by `SOVEREIGN_RPC_SERVE` (`capabilities.rs:140`).
  So the host needs `SOVEREIGN_RPC_SERVE` *and* `SOVEREIGN_SHARED_MODEL_HOST_NODE_ID=<self>` to win.
  The experiment doc's "host = `SOVEREIGN_RPC_DISCOVER=1` only" is insufficient with the current
  host-election code.

## 6. Open questions for the reviewer (the crux)

The whole RPC-distribution path assumes **direct dialability**: ggml activation traffic is raw TCP to
`worker_ip:50052`; the warm is raw HTTP to `worker_ip:9742` and `host_ip:9742`. All of that is
**LAN-only**. For "different networks" it must ride iroh (NAT-traversal / relay).

1. **Is the existing iroh TCP tunnel the right vehicle? (partly answered — it already exists AND is
   wired for internal HTTP.)** `commonwealth-transport/src/iroh.rs` has `IrohAcceptor::spawn(endpoint,
   forward_to)` (forwards *raw* accepted bi-streams to a local listener — generic, not HTTP-specific)
   + a connect-side `TcpBridge` giving `127.0.0.1:<port>`. And it's **already wired for internal
   HTTP**: `sovereign-mesh/src/iroh_access.rs:229` (`IrohAcceptor::spawn_routed`) and
   `sovereign-server/src/iroh_access.rs:98` (`spawn(…, 127.0.0.1:http_port)`) — this is how gossip /
   control-plane reach peers over iroh today (the `http://127.0.0.1:<proxy>/internal/…` base URLs).
   So:
   - **Warm HTTP (blocker #3) is the smaller fix:** stop building `http://{ip}:9742/internal/rpc-warm`
     by hand; route it through the same transport gossip uses (`transport.endpoints(peer, class)` →
     the iroh-tunneled base). Same for the worker→host range-fetch base.
   - **ggml RPC (`:50052`) is the genuinely new piece:** it's raw TCP, not HTTP, and connects to
     `worker_ip:50052` directly (no tunnel). Cross-network it needs a **connect-side raw-TCP bridge**
     (a `127.0.0.1:<local>` that tunnels to the peer's RPC server over iroh) that ggml is pointed at.
     The acceptor side can already forward a raw stream (`IrohAcceptor::spawn`); the connect-side
     bridge + a dedicated ALPN for RPC is what's missing. **Fix #1's direct-IP should then be
     re-pointed at this bridge-local endpoint** rather than the peer IP.
2. **Transport cost — the operator's actual question.** Decode does a per-layer activation handoff;
   it is latency-bound on the hop. iroh direct (hole-punched) ≈ QUIC overhead + one bridge hop
   (probably tolerable); iroh **relay** fallback (common across strict NATs) ≈ relay RTT + a shared
   bandwidth bottleneck (probably murders decode t/s). **The cross-network "distribution tax" is
   largely the iroh path quality.** Can we (a) measure iroh direct-vs-relay RTT between two real
   cross-network nodes, and (b) decide whether the per-layer hop can live on it — before building?
3. **Best vs easiest diverge, and easiest is a non-starter.** Easiest = keep raw TCP/HTTP (works
   today same-LAN) — but it does **not** prove the cross-network use case, so it's out. Best =
   iroh-tunnel the warm (moderate: route via `transport.endpoints()`/a bridge) **and** the ggml RPC
   (the load-bearing piece: wire `IrohAcceptor`/`TcpBridge`). Is there a middle option we're missing
   (e.g. a scoped plaintext RPC port that's still reachable through some existing hole-punch)?
4. **Does routing ggml-RPC over iroh even hold under load?** The RPC client `GGML_ABORT`s (uncatchable)
   on any touch of a dead/stalled worker. A relay hiccup mid-decode could look like a dead worker.
   How does the tunnel surface iroh path loss to ggml, and does the eligibility/supervision story
   (`RPC_DISTRIBUTED_INFERENCE.md` §Robustness) cover a *flapping tunnel* vs a flapping worker?

## 7. Suggested next step

Before writing transport code: **characterize the iroh path between two genuinely cross-network nodes**
(direct-vs-relay, RTT, bandwidth) and confirm `IrohAcceptor`/`TcpBridge` can carry an arbitrary TCP
stream end-to-end with a smoke test (tunnel a plain `nc`/HTTP through it). If the direct path is
healthy and the tunnel is generic, the fix is "route warm + RPC through the bridge" and Fix #1 should
be re-pointed at bridge-local endpoints. If relay is the common case, the honest result may be that
cross-network distributed *decode* is bandwidth/latency-bound to single-digit t/s — which is itself a
publishable finding for the SHARED_MODEL gate.

## 8. Tree / environment state at handoff

- **Uncommitted:** the 8 files in §3. Build clean (`cargo build -p sovereign-cli-daemon`, debug).
- **Config:** 122B primary + tiny `fast` + ctx 8192 (the distributed test config). Original 35B-solo
  config is backed up at `~/.sovereign/config.toml.bak-exp-*`. **Box is currently idle** (last daemon
  killed after the LocalOnly fallback).
- **Wire-compat:** both nodes must build from the **same `vendor/llama-cpp-4` tree**
  (`git rev-parse HEAD:vendor/llama-cpp-4` matched during the session) — RPC is version-sensitive.
- **Scratch/evidence:** logs + `gdb-bt.txt` under the session scratchpad
  `…/scratchpad/hetero-exp/` (fixed-run.log has the definitive placement + warm-fail lines).
