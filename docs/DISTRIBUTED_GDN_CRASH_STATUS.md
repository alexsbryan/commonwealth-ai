# Distributed inference: host aborts at `ggml-rpc.cpp:498` on a hybrid (Gated DeltaNet) model

**Status:** RESOLVED — root cause confirmed from source, fix landed AND e2e-confirmed
against BeefyMac (2026-07-27 14:27 UTC, see §10). The canary that aborted the host 5×
now decodes; distributed 4B decode measured at 17.35 t/s median over the forced iroh tunnel.
**Date:** 2026-07-27. **Repo HEAD:** `bd6e3652`.
**Machines:** RuggedFox (host, Linux/Vulkan, Strix Halo) + BeefyMac (RPC worker, macOS/Metal).

---

## 1. The failure

Loading a model **distributed across an RPC boundary** succeeds, but the first decode
kills the host daemon:

```
WARN resolve_fused_ops: layer 8 is assigned to device RPC0 but fused Gated Delta Net
                        (autoregressive) is assigned to device Vulkan0 (usually due to
                        missing support)
WARN resolve_fused_ops: fused Gated Delta Net (autoregressive) not supported, set to disabled
     ... same pair for "(chunked)"
INFO reload_primary: slot reloaded across device set latency_ms=54657
<canary request>
ggml-rpc.cpp:498: Remote RPC server crashed or returned malformed response
=== host daemon dies ===
```

Reproduced **5×**, byte-identical, across two days and multiple daemon restarts.
Deterministic — not a race.

---

## 2. What is PROVEN (direct evidence, not inference)

### 2.1 The abort site is a *detector*, not the defect
`ggml-rpc.cpp:498` is `RPC_STATUS_ASSERT(status)` inside
`ggml_backend_rpc_buffer_get_tensor`. The macro (line 30) is:

```cpp
// macro for nicer error messages on server crash
#define RPC_STATUS_ASSERT(x) if (!(x)) GGML_ABORT("Remote RPC server crashed or returned malformed response")
```

It fires when the worker returns `status == false`. **The worker need not have crashed** —
a clean refusal produces the identical host abort. Do not read this message as "the worker
segfaulted".

### 2.2 The worker's guard is asymmetric — this is the load-bearing detail
`rpc_server::deserialize_tensor` (ggml-rpc.cpp:992-1020) nulls the buffer of any tensor
whose buffer handle this worker does not own:

```cpp
result->buffer = reinterpret_cast<ggml_backend_buffer_t>(tensor->buffer);
if (result->buffer && buffers.find(result->buffer) == buffers.end()) {
    result->buffer = nullptr;          // foreign buffer -> nulled
}
if (result->buffer) { /* bounds checks ONLY happen here */ }
result->data = reinterpret_cast<void *>(tensor->data);   // set UNCONDITIONALLY
```

`create_node` (1285) then guards:

```cpp
if (result->buffer == nullptr && result->data != nullptr) {
    GGML_LOG_ERROR("[%s] invalid data ptr", __func__);
    return nullptr;
}
```

This rejects `null buffer + non-null data`. It does **not** reject `null buffer + null data`.

### 2.3 The tensor arrives null-null (worker-side instrumentation, BeefyMac)
With `GGML_RPC_DEBUG=1` working (see §5), BeefyMac observed:
- Every `alloc_buffer` returned a non-zero `remote_ptr`. **No allocation failure anywhere.**
- **No** `invalid data ptr` and **no** `failed to create source node` — both are
  `GGML_LOG_ERROR` and would be visible regardless of the debug gate.
- Therefore the tensor passed the 1285 guard, which is only possible with
  `buffer == nullptr && data == nullptr`.
- Faulting frame: **`ggml_metal_op_set` dereferencing a null buffer on the dst tensor.**
- A 951-node prefill graph SURVIVED; a 23-node / 103-tensor graph killed it. Size is not
  the trigger.

`GGML_OP_SET` is produced by the Gated DeltaNet path (`ggml_set_inplace`,
delta-net-base.cpp:262) — and only there.

### 2.4 The model architecture
`Qwen3.5-4B.Q6_K` is a **3:1 hybrid**, 32 layers, read straight from the GGUF tensor table:
- **Full attention:** layers 3, 7, 11, 15, 19, 23, 27, 31 (`attn_q/k/v/output`, no SSM)
- **Gated DeltaNet:** all others (`ssm_a/alpha/beta/conv1d/dt/norm/out` + `attn_gate/attn_qkv`)

**Layer 8 — the layer in the warning — is a Gated DeltaNet layer.**

The real target `Qwen3.5-122B-A10B` is the **same architecture** (252 `ssm_` vs 24 `attn_k`
tensors). The 4B is a faithful proxy, not a bad test vehicle.

### 2.5 This is a REGRESSION — distributed decode used to work
Per notes from **2026-07-19**: a live daemon pair decoded distributed at **~40 tok/s**
("DECODE migrates to direct minutes later (~40 tok/s ≈ direct)"). Load, transport, and
decode all worked. Something between 2026-07-19 and 2026-07-27 broke it.

---

## 3. Hypotheses TESTED AND FALSIFIED

Recording these so the next person does not re-run them.

| # | Hypothesis | How it was tested | Result |
|---|---|---|---|
| H1 | The host abort at :498 means the worker crashed first | Timestamps: host aborted ~04:22:17-19, worker rc=139 at 04:22:22 | **FALSE** — worker died *after*. The abort is a refusal detector (§2.1) |
| H2 | Layer-device disagreement is VRAM-proportional rounding; pinning `tensor_split` to the shard plan fixes it | Added `tensor_split_from_plan`, set `[0.25, 0.75]` for the 8/24 cut, ran | **FALSE** — layer-8 warning byte-identical. llama.cpp's per-layer assignment does not track `tensor_split` linearly here. Code reverted |
| H3 | Regression is `4b0b0e93` (2026-07-20) "byte-mass-aware split", which landed one day after the last good decode | Forced the count-based split for hybrid models; ran | **FALSE** — count-based produces the **identical** 8/24 cut (`blocks: 8`), same warning, same crash. Both apportionment rules agree here |
| H4 | llama.cpp bump 0.3.1 → 0.4.2 introduced fused GDN | `f6da8067` is dated **2026-07-16**, *before* the 07-19 good decode. (`resolve_fused_ops` is new in 0.4.2, but fused GDN existed in 0.3.1) | **FALSE** as the regression, though the *reporting* is new |
| H5 | `override_patterns` regex is wrong (`^blk\.(0\|…\|7)\.` accidentally matching `blk.8.`) | Read the regex; std::regex backtracks correctly; `blk.8.` cannot match | **FALSE** — the override patterns are correct |
| H6 | `GGML_RPC_DEBUG=1` on the worker produced no output because no traffic arrived | Read the source: `llama_cpp` tracing target was absent from the daemon allowlist at every level | **FALSE** — dead instrument, our bug. Fixed (§5) |

---

## 4. The single narrowed open question

**Both of our placement rules give RPC0 layers 0..=7 (eight layers). llama.cpp reports
layer 8 as also being on RPC0. It is placing exactly one more layer than we are, and this
is independent of which apportionment rule we use.**

That one straddled layer has its **ops** on one device and its **weights** on the other.
Because it is a Gated DeltaNet layer, the fused kernel is disabled, the unfused path emits
`GGML_OP_SET`, and its dst reaches the worker null-null → §2.2 → §2.3.

**Unread and directly relevant:** `llama-context.cpp:500-560` — the function that emits the
warning. It defines what "layer N is assigned to device X" *means* and where that assignment
comes from. Nobody has read it yet. Every hypothesis above was formed without it.

A plausible-but-untested lead: llama.cpp historically splits over **`n_layer + 1`** units
(the output layer counts as one), so a 25% share of 33 units is 8.25 → layers 0..=8 on
device 0, while our `-ot` splits 32 units → 0..=7. That would produce exactly this
off-by-one, but it is **inference, not evidence** — confirm against the source before acting.

---

## 5. Instrumentation fix (landed, independently correct — KEEP)

`GGML_RPC_DEBUG=1` on the worker produced zero output. Three gates sat between it and the log:

1. `GGML_RPC_DEBUG` is a genuine runtime getenv (ggml-rpc.cpp:21) — this one was fine.
2. `sovereign-inference/src/llama.rs:339-343` — the daemon default
   `install_log_tracing_errors_only()` demotes `GGML_LOG_LEVEL_DEBUG` to `tracing::trace!`.
3. `DAEMON_TRACING_FILTER` (sovereign-cli-daemon/src/lib.rs) is an **allowlist with no
   default level**, and `llama_cpp` — the literal target every ggml line rides — was absent
   **at every level, including ERROR**.

Gate 3 was unwinnable and also silently killed the model-load-failure surface that
`install_log_tracing_errors_only` exists to provide.

Diagnostic tell: `printf("Accepted client connection")` (ggml-rpc.cpp:1756) is a raw printf
that bypasses the log callback, while every `LOG_DBG` goes through it. Seeing one family and
not the other indicts log routing, not traffic.

**Fixed:** `llama_cpp` added to the allowlist (`info` quiet / `debug` verbose);
`GGML_RPC_DEBUG` now implies full verbosity; test `daemon_filter_carries_llama_cpp_target`
pins it. This is what made BeefyMac's §2.3 evidence possible.

---

## 6. Current tree state

HEAD `bd6e3652`. Uncommitted in `sovereign-inference`:

- `rpc_distribution.rs` — hybrid detection routing to count-based split. **Proven a no-op
  (H3). Revert or keep as documentation of the falsified path.**
- `model_slot.rs` — `tensor_split` pin **reverted**, replaced by a comment recording H2's
  falsification.
- `rpc_warm_cache.rs` — `tensor_split_from_plan` removed; `parse_block_split` /
  `plan_shards_explicit` remain (feed `SOVEREIGN_RPC_BLOCK_SPLIT`, an env knob to pin
  per-device block counts; useful for aiming the boundary, unused so far).

Lint: 0 fail / 175 pre-existing warnings. Daemon builds clean.

---

## 7. Reproduction

Host (RuggedFox):
```bash
cp ~/.sovereign/config.toml.e2e-tunnel-staged ~/.sovereign/config.toml
# primary = Qwen3.5-4B.Q6_K.gguf, fast = Qwen3.5-0.8B, [iroh] enabled=true,
# [shared_model] role="host"
rm -f ~/.sovereign/daemon.lock
SOVEREIGN_RPC_TUNNEL=always ./target/debug/sovereign-cli-daemon daemon run
```
Worker (BeefyMac): daemon with `[iroh] enabled=true`, `role="anchor"`,
`GGML_RPC_DEBUG=1`, `sovereign-server` stopped.

Wait ~300s (anchor settle) → `reload_primary` → `mode=distributed 24/32 local`, then:
```bash
curl -s localhost:9741/v1/chat/completions -H 'content-type: application/json' \
  -d '{"model":"commonwealth/primary","max_tokens":16,"temperature":0,
       "messages":[{"role":"user","content":"Count from 1 to 10."}]}'
```
Host dies at `ggml-rpc.cpp:498`.

Restore afterwards: `cp ~/.sovereign/config.toml.bak-pre-boundary-expt-* ~/.sovereign/config.toml`.

---

## 8. Harness traps that cost real time

1. **`host_alive` false positives.** `measure-distributed-decode.sh` checks
   `pgrep -f 'target/debug/sovereign-cli-daemon daemon run'`, which matches *bash wrappers
   containing that string*. It reports "alive" when the daemon is dead. Resolve
   `/proc/<pid>/exe` instead. This produced at least two wrong readings, including one in
   this session.
2. **Orphaned daemon on a deleted inode.** `cargo build` replaces the binary under a running
   daemon; the old process keeps running and keeps `:50052`. The orphaned listening socket
   still **accepts** connections that nobody answers → HELLO fails → host aborts at
   `ggml-rpc.cpp:337`. Detection must handle `readlink /proc/<pid>/exe` returning
   `…sovereign-cli-daemon (deleted)`.
3. **`:337` is a known transient**, characterized 2026-07-19: session death mid-exchange
   during a ~60s ALPN-refusal window after a peer restart; self-heals. Not the same bug
   as `:498`.
4. **Stale `~/.sovereign/daemon.lock`** after a GGML_ABORT blocks the next launch.
5. **Do not lower `SOVEREIGN_RPC_WORKER_SETTLE_SECS`.** The 300s anchor settle exists to keep
   a still-stabilizing worker out of a load; shortening it to 20s admitted a flapping
   BeefyMac and produced a `:337` abort that looked like a new bug.
6. **Buffers accumulate across reloads** — BeefyMac saw no `free_buffer` calls at all, so
   attempt 1's ~1.2 GB stayed resident alongside attempt 2's. Independent leak worth filing.

---

## 9. Recommended next step

Read `llama-context.cpp:500-560` **before** writing any more code. It answers where
"layer N is assigned to device X" comes from, which is the one fact every falsified
hypothesis above was missing. If it confirms the `n_layer + 1` lead in §4, the fix is to make
our `-ot` cut agree with llama.cpp's unit count rather than to keep negotiating with it.

Independently of the root cause: a remote peer should not be able to abort the host process.
`resolve_placement` already claims "the default in every uncertain case is LocalOnly — the
load NEVER wedges". A hybrid model whose plan straddles a GDN layer currently violates that.
Failing closed to `LocalOnly` with a named reason turns a daemon-killing abort into a legible
decision, and is worth doing whatever §4 turns out to be.

## 10. RESOLUTION (2026-07-27) — §4 confirmed from source; fix landed

§9's read was done. `llama-context.cpp:473-540` (`resolve_fused_ops`) compares the
scheduler's placement of each fused-GDN node against **`model.dev_layer(node.il)`** —
llama.cpp's own per-layer device map. `llama-model.cpp:1259-1318` shows exactly how that
map is built:

```cpp
const int act_gpu_layers = ... std::min(n_gpu_layers, n_layer_all + 1);   // 33 units, not 32
const int layer_gpu = std::upper_bound(splits.begin(), ...,
                                       float(il - i_gpu_start)/act_gpu_layers) - splits.begin();
```

- `dev_layer` is computed from **`tensor_split`** (advertised free memory when unset) over
  **`n_layer + 1` units** — the output head is the extra unit. It **never consults `-ot`
  overrides.** Two placement systems, no communication: overrides place WEIGHTS, dev_layer
  claims LAYERS.
- We pass `n_gpu_layers = 999`, so `i_gpu_start = 0` and `act_gpu_layers = 33` on the 4B.
- **H2's falsification is explained, not contradicted:** `[0.25, 0.75]` puts the cut at
  `0.25 > 8/33 ≈ 0.2424`, so layer 8 (value `8/33`) stays on RPC0 — byte-identical warning
  *predicted* by the source. The mechanism (pin tensor_split) was right; the constant
  (block fraction over 32 units instead of 33) was wrong.
- **H3's no-op is explained:** which apportionment rule we use (count vs byte-mass) never
  mattered, because dev_layer follows tensor_split/free-VRAM regardless. Both rules crash;
  both would be fixed by the pin.
- **§2.5's "regression" needs no code change to explain:** with free-memory default splits,
  whether dev_layer's implied boundary straddles a *GDN* layer (vs a full-attention layer,
  where no fused-GDN node exists to mismatch) is luck of the advertised free bytes at load
  time. The 2026-07-19 success plausibly never straddled.
- `ggml_backend_rpc_device_supports_op` returns `true` unconditionally (ggml-rpc.cpp:1822)
  — the mismatch was never "RPC can't run fused GDN"; the scheduler simply followed the
  weights our `-ot` placed while dev_layer said otherwise.

**The fix (landed in `sovereign-inference`):**

1. `rpc_warm_cache.rs::dev_layer_tensor_split(plan, n_layer)` — computes the
   `tensor_split` whose implied dev_layer cut points sit **halfway between our block
   boundaries in `(n_layer+1)`-unit space** (a device whose last block is `b` cuts at
   `b + 0.5`; the output-holding device extends to `n_layer + 1`). Midpoints make float
   ties impossible. Unit tests replicate llama.cpp's `upper_bound` math verbatim and
   assert layer-by-layer agreement on the exact 8/24 repro cut, multi-device cuts, a
   blockless middle device, and every cut position of a small model; a negative test
   pins that the naive `[8, 24]` weights reproduce the off-by-one.
2. `rpc_distribution.rs` — `DistributionPlan` now carries `tensor_split`, computed from
   the final plan; the H3-era hybrid→count-based forcing is **reverted** (byte-mass is
   safe for hybrids once dev_layer is pinned, and the 122B genuinely needs byte balance
   to not OOM the 64 GB Mac).
3. `model_slot.rs` — the `OwnedOverrides` branch now applies
   `.with_tensor_split(&dist.tensor_split)` alongside the overrides, with the corrected
   account replacing the H2 falsification note.

Consequence chain after the fix: dev_layer agrees with the `-ot` cut for every layer and
the output head → `resolve_fused_ops` finds no mismatch → fused GDN stays **enabled** →
the unfused `GGML_OP_SET` path is never emitted → the null-null tensor of §2.2/§2.3 never
reaches the worker.

Gates: `sovereign-lint.sh` 0 fail / 175 pre-existing warnings (0 in new code);
`cargo test -p sovereign-inference --lib rpc_warm_cache` 17/17 pass.

**E2E CONFIRMED (2026-07-27 14:20–14:31 UTC, RuggedFox + BeefyMac, §7 recipe verbatim).**
An unusually clean A/B: the byte-mass plan happened to produce the IDENTICAL 8/24 cut as
all five crash runs (the 4B is byte-uniform enough), so BeefyMac's warm cache stayed hot
and the only changed variable was the pin — logged as
`plan_distribution: dev_layer tensor_split pinned to the shard plan split=[7.5, 25.5]`,
exactly the predicted midpoint values. Load 52.2 s, `mode=distributed 24/32 local`.
Results vs. every crash run:

| Signal | Crash runs (5×) | This run |
|---|---|---|
| `resolve_fused_ops` layer-8 WARN pair | always, byte-identical | **absent** |
| Canary (`Count from 1 to 10`, greedy, 16 tok) | `completion_tokens=0` + host abort at :498 | full 16-token completion |
| Host daemon after decode | dead | alive (verified via `/proc/<pid>/exe`) |
| Sustained decode | n/a | 3×128-tok trials, **17.35 t/s** median [16.24, 17.81], TTFT 651 ms, ITL p95 130 ms, greedy-identical across trials |

(The fused-GDN "enabled" INFO lines from llama.cpp are demoted below the daemon's
llama_cpp=debug surface, so their absence is expected; the WARN pair demonstrably
surfaces at this filter level — its absence is the verdict.)

Throughput note: 17.35 t/s is over `SOVEREIGN_RPC_TUNNEL=always` (forced iroh tunnel);
the 07-19 ~40 t/s reading rode direct-ip. Tunnel-vs-direct cost is a separate,
already-characterized variable (relay floor 5.5 t/s, NODELAY notes) — not part of this bug.

Still open (unchanged by this fix): the fail-closed-to-LocalOnly guard (§9, second
paragraph), the worker buffer leak across reloads (§8.6), and BeefyMac's free-memory
over-advertisement (frame note — Metal's `recommendedMaxWorkingSetSize` delta is a
ceiling, not free memory).
