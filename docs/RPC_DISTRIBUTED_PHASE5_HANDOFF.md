# Handoff: Distributed-inference Phase 5 (auto-warm orchestration)

**For:** the next session picking up distributed-inference work.
**Date:** 2026-06-05.
**TL;DR:** The "Vulkan-host + Metal-worker hangs at model-load" bug is fully
root-caused and fixed; per-node sharded distribution via **owned placement (`-ot`
overrides) + warm cache** is built, lint-green, unit-tested, and **validated
cross-machine** (Strix Vulkan host + Mac Metal worker now generate tokens with
zero bulk weight transfer). What remains is **#5: auto-warm orchestration** — make
the host automatically seed each worker's shard so there's no manual cache-warm /
`SOVEREIGN_RPC_ASSUME_WARMED`. This doc is the complete map.

Read alongside:
- `docs/RPC_METAL_WORKER_INVESTIGATION.md` (the original briefing + my **RESOLUTION** section = the root cause).
- `docs/RPC_DISTRIBUTED_INFERENCE.md` (roles, env vars, the cache, transfer cost).
- Memory: `project_rpc_distributed_hang_root_cause.md`.

---

## ✅ #5 BUILT — 2026-06-05 (full #5a + #5b; lint-green; unit-tested)

Everything below §4 is now implemented. The sections remain as the design record;
this banner is the current state. Code spans 5 crates, all committed-ready:

- **§4.0 primary-only gate** — `ModelSlot::load(.., distributable: bool)` threaded
  through all 9 engine callsites; gated on `primary_path == target_path` (the lazy
  slot also holds the code model). Pure `classify_placement` decision split out +
  unit-tested (`classify_placement_is_the_never_wedge_gate`).
- **§4.1 plan agreement** — `build_buft_overrides_for_load` → `plan_distribution()`:
  ONE `plan` → both `-ot` overrides AND per-worker `assignments`. `NodeShard` gained
  serde.
- **#5a auto-warm seams** — two injected callbacks mirroring `set_rpc_worker_provider`:
  host `set_rpc_warm_orchestrator` (`model_slot.rs`, fired from `resolve_placement`),
  worker `RpcShardWarmer` trait on `commonwealth_api::state::AppState`
  (`with_rpc_shard_warmer`). Route `POST /internal/rpc-warm` on the **internal**
  router (`:9742` — `mesh_router` is loopback-only, would 403 the host). Worker impl
  `MeshRpcShardWarmer` + host orchestrator live in `sovereign-mesh/src/rpc_warm_http.rs`
  (only crate reaching both `fetch_model_to_dir` + `warm_cache_for_device`). Daemon
  wires both at startup (`daemon_cmd.rs` host + `daemon.rs` worker).
- **#5b byte-range** — `serve_model_file` honors `Range` (206, skips the whole-file
  sha); `warm_cache_from_ranges` range-GETs + hash-verifies each tensor; exposed
  `Fnv1a` + `cache_file_name` from `rpc_warm_cache`. Mode via
  `SOVEREIGN_RPC_SHARD_FETCH=ranges` (default whole-GGUF). Round-trip unit-tested.

**Status:** `sovereign-lint.sh` green; `sovereign-test.sh` = my new tests all pass,
0 new failures (the 4 failing tests are the 3 known pre-existing + an unrelated
`gliner_ner::models_root_honors_env_var` parallel env-var-race flake — see §6).

**Only remaining = §4.4 cross-machine live test** (needs the Mac worker, reachable
via the user as broker):
1. Rebuild the daemon **debug** + restart inside the `sovereign-vulkan` toolbox.
2. Mac: supervised in-process Metal worker on `:50052`, byte-identical GGUF present.
3. On the Strix: `SOVEREIGN_RPC_DISCOVER=1` (NO `SOVEREIGN_RPC_ASSUME_WARMED`), then
   trigger a primary load. Expect: `auto-warming worker shards…` → `auto-warm
   complete` → `explicit tensor placement via -ot overrides` → tokens, with Mac
   worker RSS = shard-only. The daemon (not the `complete` example) is required —
   only the daemon installs the orchestrator.

---

## 1. Root cause (settled — do not re-litigate)

The distributed-load hang is **NOT Metal**. It's a **host-side `send()` deadlock**
in llama.cpp's RPC weight-upload path: `llama_model_loader::load_all_data →
ggml_backend_rpc_buffer_set_tensor → socket send()` blocks when streaming a
worker's weight share over a real network above ~**800 MB**. Backend-agnostic — a
plain CPU worker reproduces it; localhost never wedges (loopback `send()` never
blocks). Matches upstream ggml-org/llama.cpp #19745. `-dio`/`--no-mmap` do **not**
fix it (empirically tested). Metal looked guilty only because the Mac's worker had
separately died (Bugs A/B below) so discovery kept skipping it.

**The strategy that fixes it:** don't stream weights at load. Instead each worker
holds its shard's bytes on disk in the RPC **cache** (content-addressed by
`fnv1a(tensor_bytes)`), so the host sends only `SET_TENSOR_HASH` (a hash) → the
worker hits → **zero bulk send → no deadlock**. We **own placement** via `-ot`
overrides (we do NOT predict llama.cpp's split → no divergence risk), and warm +
load read from the **same** plan, so they can't disagree.

Byte-identity is **inherent**, not a cache quirk: a worker serving blocks 18–35
must have those exact weights; the cache is keyed by their hash so it must be the
host's exact bytes. In the end state (#5) those bytes come **from the host** (fetch),
so byte-identity is automatic and there's no hand-maintained copy.

---

## 2. What's landed (all uncommitted; lint-green; unit-tested)

### 2a. Worker resilience — Bugs A+B (deployed on the Mac, live-verified)
- **Bug A (ggml, upstream):** rpc-server accept loop `return`s on one `accept()`
  failure — `ggml-rpc.cpp:1744` (vendored). One transient `ECONNABORTED` kills the
  worker forever.
- **Bug B (ours):** `serve_rpc_worker_if_configured` ran `start_server` once and
  never restarted; `/status` advertised a dead port.
- **Fix:** `model_slot.rs::serve_rpc_worker_if_configured` now **supervises** the
  worker (restart loop + `rpc_worker_restart_backoff`, capped 5 s, unit-tested) —
  compensates for Bug A. `commonwealth-api/routes_status.rs::rpc_worker_port` now
  gates the advertisement on a **TCP liveness probe** (`rpc_worker_listening`) —
  no cross-crate dep. Mac agent rebuilt + confirmed both live.

### 2b. The sharding brain — `sovereign-inference/src/embedded/rpc_warm_cache.rs`
Pure + unit-tested:
- `build_manifest(gguf)` → `Vec<TensorManifestEntry { name, layer, hash, nbytes, gguf_offset, cacheable }]`. **This is the #5b shard descriptor** (offsets + hashes per tensor).
- `tensor_layer(name)` — parse `blk.<N>`; `is_output_tensor` — `output.weight`.
- `plan_shards(n_layer, weights) -> Vec<NodeShard{ device_index, blocks:Option<(u32,u32)>, holds_output, fraction }]` — **OUR** largest-remainder contiguous policy (NOT a llama.cpp mirror). Output head → last block-holding device. token_embd → host CPU.
- `tensor_device(name, layer, plan)` — the single source of truth: warm + overrides both use it.
- `warm_cache_slice(gguf, dir, want(name,layer))` and `warm_cache_for_device(gguf, dir, plan, device_index)` — warm only a node's shard.
- `override_patterns(plan) -> Vec<(regex, device_index)]` — `^blk\.(L|…|M)\.` per device + `^output\.weight`. llama.cpp matches via `std::regex_search`.
- `gguf_block_count(gguf)` — read `<arch>.block_count` (n_layer) before load.
- `warm_cache_from_gguf` (whole-model) delegates to `warm_cache_slice(.., |_,_| true)`. Hash constants match ggml exactly (`FNV_OFFSET=0xcbf29ce484222325`, prime `0x100000001b3`, threshold 10 MB, filename `%016x`).

### 2c. The binding enabler — `vendor/llama-cpp-4/src/model/params.rs`
- `LlamaModelParams::with_tensor_buft_overrides(&[(CString, ggml_backend_buffer_type_t)])` — the `-ot` setter (mirrors `with_devices`; null-terminated; pattern CStrings kept alive). `vendor/llama-cpp-4` is already a `[patch.crates-io]` pure-Rust fork (no C++ rebuild; also has `with_n_seq_max`). **`LlamaModel.model` is `pub(crate)`** — you cannot wrap a raw sys load from our crate, which is why this setter lives in the fork.

### 2d. Never-wedge guard + override-application — `model_slot.rs`
In `ModelSlot::load`:
```
model_bytes = fs::metadata(model_path).len()
distribute = rpc_distribution_safe(model_bytes)   // small (<512MB) OR SOVEREIGN_RPC_ASSUME_WARMED
if !distribute            -> local_gpu_device_list() only        // NEVER WEDGES (the default fix)
else if assume_warmed     -> build_buft_overrides_for_load() -> with_devices + with_tensor_buft_overrides  // OWN placement
else (small)              -> live_device_list_if_pruning_needed() + rpc_tensor_split()  // stream (safe)
```
- `rpc_distribution_safe_decision(bytes, safe_bytes, assume_warmed)` (pure, tested). Threshold env `SOVEREIGN_RPC_SAFE_STREAM_MB` (default 512).
- `local_gpu_device_list()` — local GPU only, excludes RPC (robust even if a worker was registered by a prior load).
- `build_buft_overrides_for_load(model_path)` — enumerate RPC-first devices → weight by `ggml_backend_dev_memory` free VRAM → `plan_shards` → `override_patterns` → resolve `device_index` to `ggml_backend_dev_buffer_type(dev)` → `(CString, buft)`. Returns `None` (fall back to stream path) if no RPC device or `gguf_block_count` fails.

### Uncommitted files
`vendor/llama-cpp-4/src/model/params.rs`, `sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs`, `sovereign/crates/sovereign-inference/src/embedded/model_slot.rs`, `commonwealth/crates/commonwealth-api/src/routes_status.rs`, `docs/RPC_METAL_WORKER_INVESTIGATION.md` (+ this file). Suggested commits: (1) `fix(mesh): self-healing RPC worker + liveness-gated /status` (A+B), (2) `feat(inference): never-wedge guard + owned-placement (-ot) sharded distribution`.

---

## 3. Validation (done — cross-machine proven)

Harness: `cargo build -p sovereign-inference --example complete` →
```
SOVEREIGN_RPC_WORKERS=100.104.36.28:50052 SOVEREIGN_RPC_ASSUME_WARMED=1 RUST_LOG=sovereign_inference=info \
  target/debug/examples/complete --model sovereign/models/Qwen3.5-4B.Q6_K.gguf --prompt "..." --max-tokens 8
```
Result: `explicit tensor placement via -ot overrides devices=2 overrides=3` → `slot ready` (no wedge) → **24 tokens generated** across Strix Vulkan + Mac **Metal**, exit 0. Mac worker RSS: **+1.05 GB (its ~36% shard, NOT the full 2.93 GB)** → per-node sharding proven; smooth monotonic climb **through** the old ~800 MB wedge → deadlock gone. Metal-as-worker **compute works** once the load deadlock is sidestepped (the original open question — answered).

Localhost caveat (a TEST ARTIFACT): the `complete` example loads **3 slots** (primary/fast/FastShort) and I used the 4B for all → all >512 MB → all distribute onto one worker → the **unsupervised standalone** worker crashed mid concurrent compute (`ggml-rpc.cpp:491: Remote RPC server crashed`). Did **not** recur cross-machine (supervised Metal worker). → motivates **primary-slot-only distribution** (§4.0).

---

## 4. #5 — the remaining scope (build this)

### 4.0 Primary-slot-only distribution (do first; small, contained)
The multi-slot crash shows we must NOT auto-distribute fast/embed/code slots (they
must stay local even if >512 MB). Thread a `distributable: bool` through
`ModelSlot::load` (true only for the **primary** slot). Callers are in
`engine.rs` (search `ModelSlot::load(` — primary/fast/code/embed/`reload_primary`).
Guard becomes `distribute = distributable && rpc_distribution_safe(...)`. Unit-test
the gate. (Check callers with `callers("load")` / grep before the signature change.)

### 4.1 The plan-agreement INVARIANT (the crux)
Host and worker must compute the **identical** `plan_shards` — same `n_layer`, same
`weights` (per-device free VRAM via `ggml_backend_dev_memory`), same **device order
(RPC-first)**. The host owns the plan (`build_buft_overrides_for_load`); the
orchestration must hand each worker enough to reproduce ITS shard. Simplest: the
host computes the plan and tells worker W "warm device-index `i` of model M" plus
the plan inputs (or the explicit tensor list). If they diverge, some tensors miss →
bulk send → deadlock. The e2e all-hits check (RSS shard-only + completion) is how
you confirm agreement.

### 4.2 #5a — whole-GGUF fetch + sliced warm (tractable; existing primitives)
For nodes that can hold the GGUF on disk (covers 122B/MiniMax demo). Flow:
1. Host, on deciding to distribute primary M onto workers, computes the plan.
2. Host triggers each worker (new internal mesh op) to **ensure its shard warm**:
   worker `fetch_model_to_dir(host, M)` (whole GGUF; `sovereign-mesh/src/model_fetch.rs`, exists) → `warm_cache_for_device(gguf, SOVEREIGN_RPC_CACHE_DIR, plan, my_device_index)`.
3. Workers report warm-done; host sets the warm-state (replaces the manual
   `SOVEREIGN_RPC_ASSUME_WARMED`) and loads with overrides (existing override path).
Components: a worker-side handler (sovereign-mesh) + a host→worker trigger/route
(`commonwealth-api/src/routes_internal*.rs`; host already serves the GGUF via
`GET /internal/model/... serve_model_file`, allowlist `install_servable_model_files`)
+ wiring into the distribution decision (today: `daemon_cmd.rs` discovery loop →
`engine.rs::reload_primary` → `ModelSlot::load`). The warm must complete BEFORE the
load's override path runs.

### 4.3 #5b — byte-range shard fetch (the 500 GB × ~8-node endgame)
When no node can hold the whole GGUF. The worker fetches only its shard's bytes:
- `build_manifest(M)` (exists) gives every tensor's `gguf_offset`, `nbytes`, `hash`, `layer`. Select the worker's tensors via `tensor_device(.., plan) == my_index`.
- Add **HTTP Range** support to `serve_model_file` (`commonwealth-api/src/routes_internal*.rs`) — it currently streams whole-file; `fetch_model_to_dir` likewise has no Range.
- New `warm_cache_from_ranges(host_url, M, my_tensors:[{offset,nbytes,hash}], cache_dir)` — range-GET each tensor, write `<016x hash>` (verify the hash matches; the host can serve the precomputed manifest so the worker doesn't re-hash). This is the only path that keeps a worker at O(model/N) on disk.

### 4.4 Validate #5 cross-machine
Drop `SOVEREIGN_RPC_ASSUME_WARMED` (auto-warm should fire). Re-run the §3 harness
Strix→Mac; confirm the worker auto-fetches + warms ITS shard, then RSS shows
shard-only + completion. The Mac worker is supervised (A+B) so flaps self-heal.

---

## 5. Gotchas / environment (save yourself the rediscovery)

- **Sandbox exit-144:** Bash commands that `kill`/`pkill`/background processes get
  signaled (exit 144) and truncate — but the action usually happened. Launch
  long-running things via `setsid … & disown` (survives it) and verify in a
  separate command. Don't chain a kill with other steps.
- **Observability gap:** the daemon routes ggml logs through its tracing sink and
  **drops the RPC DEBUG** lines, so the in-process worker won't print
  `set_tensor_hash`/`graph_compute` counts. For hard histograms, point the host at
  a **standalone** `rpc-server -d CPU -c` (it prints to its own log with
  `GGML_RPC_DEBUG=1`). RSS-delta on the worker is the reliable shard-only proof.
- **Cache dir differs by worker kind:** in-process worker reads
  `SOVEREIGN_RPC_CACHE_DIR` (default `~/.sovereign/rpc-cache`) **directly**; the
  standalone `rpc-server -c` reads `<LLAMA_CACHE>/rpc/`. Warm into the right one.
- **Watcher is down** (`not_configured`): verify with the full scripts
  `./scripts/sovereign-lint.sh --human` + `./scripts/sovereign-test.sh --human`
  (NOT narrow `cargo -p`). Build the daemon's siblings per `.claude/CLAUDE.md`.
- **3 pre-existing failing tests** (NOT ours): corpus-engine `restore_refuses_embedding_model_mismatch` (stale after commit d65635bf), corpus-engine `uap_coalesces_variants_...`, sovereign-server `ws_streams_tokens_then_complete` (test router missing a `TurnNarration` broadcast extension).
- **The `complete` example loads 3 slots** — use §4.0 (primary-only) or a single-slot harness for clean distributed tests.
- **Models on disk (Strix):** `sovereign/models/Qwen3.5-4B.Q6_K.gguf` (sha256 `298cae8e619fcc323e63084a27bd1bc6d82e106e32d530e0dafaea5e12420604`); 35B primary `Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf`; two 122B quants; **no MiniMax on disk**. 122B (104 GB) fits the Strix's 128 GB alone (only >128 GB models strictly need distribution).
- **Env var contract:** `SOVEREIGN_RPC_SERVE` (be a worker), `SOVEREIGN_RPC_DISCOVER=1` (host auto-discovers workers via peer `/status`), `SOVEREIGN_RPC_WORKERS=ip:port` (host manual list), `SOVEREIGN_RPC_ASSUME_WARMED=1` (gate the override path; #5 replaces this with auto), `SOVEREIGN_RPC_SAFE_STREAM_MB` (guard threshold), `SOVEREIGN_RPC_CACHE_DIR`.

## 6. Live state / how to drive the Mac

- **Strix daemon:** currently the **pre-guard build** with discovery **off** (safe; loads local-only). To deploy the guard+overrides, rebuild `sovereign-cli-daemon` (debug) and restart inside the `sovereign-vulkan` toolbox (`sovereign daemon stop` via the debug binary, then start). Use the **debug** build (user's standing preference this session).
- **Mac:** supervised in-process **Metal** worker on **:50052** (A+B deployed + verified), cache `~/.sovereign/rpc-cache`, byte-identical 4B present + whole-warmed. There's also a standalone CPU worker pattern on :50056 (for clean `GGML_RPC_DEBUG` traces). SSH to the Mac is refused — coordinate **via the user as broker** (the Mac runs its own agent).
- **Tailscale:** Strix `100.115.12.21`, Mac `100.104.36.28`; direct LAN path (not DERP), ~134 ms Wi-Fi RTT (so per-tensor `SET_TENSOR_HASH` round-trips dominate load time — a perf note for large models, not a correctness issue).

## 7. Definition of done for #5
Both `lint_status` + `test_status` (or the full scripts) `fresh_passing`; new pure
logic unit-tested; the cross-machine harness (§4.4) shows the worker **auto-warms
its shard** (no manual cache-warm, no `ASSUME_WARMED`) and the distributed load
completes with worker RSS = shard-only. Then the demo-natural topology "just works":
join mesh → host distributes the big primary → workers seed their shards → tokens.
