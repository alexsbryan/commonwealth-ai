# Run DeepSeek-V4-Flash on the mesh

This is the [Run a model bigger than your machine](./RUN_A_BIGGER_MODEL.md) path,
made concrete for DeepSeek-V4-Flash across two boxes: a **Strix Halo (128 GB,
Vulkan)** as host and **BeefyMac (64 GB, Metal)** as worker.

Unlike GLM-5.2, this is not yet a hand-it-to-your-pool runbook — it's a **bring-up
in progress**. DeepSeek-V4-Flash is a new architecture whose llama.cpp support
lives on a work-in-progress branch, written CUDA/Metal-first, and it is *not* in
our binding: we vendor the `llama-cpp-4` binding at `vendor/llama-cpp-4`, but
llama.cpp itself ships inside `llama-cpp-sys-4 0.3.1`, which we pull unmodified from
crates.io — and that bundles llama.cpp `94a220cd6` (GLM-5 era, no DeepSeek-V4). So
we **cannot** run it through `sovereign daemon` on day one. Stages 0–2 route around
this entirely by using standalone WIP llama.cpp binaries; only Stage 3 confronts the
binding. This page is the staged plan that gets us there, and the log we keep as we
go. Each stage retires one risk before we spend on the next.

## Status

_Last updated: 2026-07-08_

- [~] **Stage 0 — does the arch execute on our backends?** (blocked on download)
- [ ] **Stage 1 — solo Strix, quality + speed read**
- [ ] **Stage 2 — pooled over LAN, measure tok/s** ← the decision gate
- [ ] **Stage 3 — integrate into `sovereign daemon`**
- [ ] **Stage 4 — distributed throughput probe** (build during Stage 2)

**Right now:** the IQ3_XXS quant (~97 GB, 4 shards) is downloading to
`sovereign/models/DeepSeek-V4-Flash-GGUF/UD-IQ3_XXS/`. Nothing else can start
until it lands and we have a WIP llama.cpp build. Next action: Stage 0.

## The model

- **284B total / 13B active** mixture-of-experts. Only ~13B parameters do work on
  any given token, which is what makes it MoE-friendly to split — the weights are
  heavy, the per-token compute is light.
- **Native MXFP4 experts** (96% of parameters, quantization-aware trained). The
  experts stay MXFP4 in every unsloth quant; only the non-expert tensors change
  bit-width. This matters: MXFP4 kernel support is required on our backend
  regardless of which quant we pick.
- **Hybrid attention — CSA + HCA** (Compressed Sparse + Heavily Compressed).
  MLA-style: the KV cache stays small even at long context, which is how it reaches
  a **1M-token** window (384K for its "Think Max" reasoning mode). Good news for
  fitting long context in our memory budget.
- **Sampler (unsloth's recommendation):** `--temp 1.0 --top-p 1.0 --min-p 0.0`.
  Thinking toggles via `--chat-template-kwargs '{"enable_thinking":false}'` or
  `--reasoning on/off` / `"reasoning_effort":"high"|"max"`.

### What "bleeding edge" actually means here

Support is a moving target, not a released version:

- **PR #24162** (am17an) — initial DeepSeek-V4 support, on branch
  `wip/deepseek-v4-support` (PR #22378 lineage). The one public run at our memory
  envelope (2×96 GB Blackwell) used a commit around `b8942`/`ba173dd08`.
- **PR #25202** (merged 2026-07-07) — fixes multi-turn corruption when KV cache is
  quantized (`--cache-type-k/v q8_0`); took tool-calling from 4/15 to 15/15. Only
  needed if we quantize the KV cache. `antirez/llama.cpp-deepseek-v4-flash` is an
  alternative experimental fork.

That Blackwell precedent is the honest expectation-setter: on mature CUDA cards it
managed **PP 38 / TG 35.7 tok/s at 8K context**, with **Flash Attention
auto-disabled** ("custom attention architecture" unsupported), **30–40% GPU
utilization**, and an incomplete graph. That's the kernels' home backend. We're on
Vulkan and Metal, across a network hop. Budget for single digits and be pleased if
we beat it.

## The two risks we're actually retiring

1. **Does the arch run on Vulkan (and Metal) at all?** The CSA/HCA attention and
   MXFP4 experts are being written CUDA-first. Our Strix is **Vulkan-only** — ROCm
   SEGVs on gfx1151 MoE (upstream #20176; `sovereign-inference/Cargo.toml:50-58`),
   and there's no ROCm runtime toggle. If a kernel is missing, the op falls to CPU
   (fine on unified memory, slow) or aborts. This is the make-or-break, and it has
   nothing to do with splitting.
2. **Is the tok/s across a network hop tolerable?** The distribution machinery is
   built and the >800 MB weight-upload deadlock (upstream ggml #19745) is already
   worked around — but it's only ever been run at **4B across 2 nodes**. Real MoE
   scale across the mesh is unmeasured. That measurement is the gate below.

## Hardware and the memory math

Both machines are unified memory, so weights **and** KV must fit the pool, not just
some "VRAM" carve-out. After the OS and daemon, usable is roughly **Strix ~116 GB**
(standalone, no other slots loaded) and **Mac ~50 GB** (raise `iogpu.wired_limit_mb`).
Pool ≈ **166 GB**. The compressed attention keeps KV small, so weights dominate.

| Quant | Size | Solo Strix (~116 GB) | Pooled split (~2:1) | Use |
|---|---|---|---|---|
| Q2_K_XL | 96.8 GB | ✅ | — | skip — quality cliff, redundant |
| **UD-IQ3_XXS** | **96.95 GB** | ✅ ~64–128K ctx | ✅ 69/34, long ctx | **← our quant. One file, both paths.** |
| Q3_K_XL | 129 GB | ❌ | ✅ 86/43 | later, only if the gate clears |
| native FP4/FP8 | 146 GB | ❌ | ⚠️ tight on Mac | later stretch; proven-working GGUF |
| Q8_K_XL | 162 GB | ❌ | ❌ exceeds pool | no |

We picked **IQ3_XXS** because it's the one file that serves both the solo fallback
and the pooled target: it's the first genuinely-good quality tier (above the Q2
cliff), and its real on-disk size is **96.95 GB** — the same footprint as Q2, not
the 103 GB the model card's table claims. On the Strix that leaves ~20 GB for KV
and compute; solo you'll get tens of K of context (not the full 384K — that needs
the pool). The Strix has already run a 104 GB model host-alone, so this is a
comparable, not a projection.

## Getting the model

Already in flight (resumable — re-run the identical command after any drop):

```bash
hf download unsloth/DeepSeek-V4-Flash-GGUF \
  --include "UD-IQ3_XXS/*" \
  --local-dir sovereign/models/DeepSeek-V4-Flash-GGUF
```

Lands as four shards under `.../DeepSeek-V4-Flash-GGUF/UD-IQ3_XXS/`. **To load a
split GGUF, point at shard `00001` only** — llama.cpp reads the split metadata and
pulls `00002`–`00004` from the same directory. Don't merge them, don't list them:

```
.../UD-IQ3_XXS/DeepSeek-V4-Flash-UD-IQ3_XXS-00001-of-00004.gguf
```

## Stage 0 — does the arch execute on our backends?

Standalone WIP llama.cpp build, no Sovereign code. The question is binary: does
DeepSeek-V4-Flash emit a token on Vulkan (Strix) and on Metal (Mac)? Test both —
the Mac must run these kernels as an RPC worker, so its Metal coverage matters as
much as the Strix's Vulkan.

On the Strix, **inside the `sovereign-vulkan` toolbox** (see
[TOOLBOX_SETUP.md](../sovereign/docs/TOOLBOX_SETUP.md); glslc comes from the LunarG
SDK we already have):

```bash
git clone https://github.com/ggml-org/llama.cpp && cd llama.cpp
git checkout wip/deepseek-v4-support        # or the antirez fork
cmake -B build -DGGML_VULKAN=ON -DGGML_RPC=ON -DBUILD_SHARED_LIBS=OFF
cmake --build build --config Release -j \
  --target llama-cli llama-server rpc-server llama-gguf-split

./build/bin/llama-cli \
  --model /path/UD-IQ3_XXS/DeepSeek-V4-Flash-UD-IQ3_XXS-00001-of-00004.gguf \
  --n-gpu-layers 999 --ctx-size 8192 \
  --temp 1.0 --top-p 1.0 --min-p 0.0 \
  -p "Explain what a mixture-of-experts model is, briefly." -n 128
```

On the Mac, the same but Metal activates automatically — drop `-DGGML_VULKAN=ON`,
keep `-DGGML_RPC=ON`. (Fresh-Mac gotcha: `xcode-select --install`.)

**Pass:** coherent tokens on both. **If Vulkan aborts on a missing op:** that's the
risk landing. Options in order — force the offending op to CPU on the unified pool,
wait for the Vulkan kernel to land upstream, or flip roles (Mac/Metal hosts, Strix
workers). Log which op and decide here; don't push past this stage until the Strix
produces tokens.

## Stage 1 — solo Strix, quality + speed read

If Stage 0 passes, run IQ3_XXS whole on the Strix via `llama-server` and get a real
read: is this model worth the distributed effort, and what's the local tok/s
baseline the split has to justify?

```bash
./build/bin/llama-server \
  --model /path/UD-IQ3_XXS/DeepSeek-V4-Flash-UD-IQ3_XXS-00001-of-00004.gguf \
  --n-gpu-layers 999 --ctx-size 32768 --jinja \
  --host 0.0.0.0 --port 8080
```

llama.cpp prints eval timing (tok/s) at the end of each response. Note it, and
sanity-check quality on a few real prompts.

## Stage 2 — pooled over LAN — the decision gate

Bring in the Mac using llama.cpp's **native** `rpc-server`, over the LAN, with the
local tensor cache (`-c`, stored under `$HOME/.cache/llama.cpp/rpc`). Native
raw-TCP RPC isolates the model-and-backend variable from our own transport, and the
cache sidesteps the weight-upload cost. This is deliberately *before* any Sovereign
integration.

On the Mac (worker):

```bash
./build/bin/rpc-server -c -p 50052        # binds all interfaces; -c enables the cache
```

On the Strix (host) — `--tensor-split 2,1` gives the local Strix ~2/3 and the RPC
Mac ~1/3, matching 128:64:

```bash
./build/bin/llama-server \
  --model /path/UD-IQ3_XXS/DeepSeek-V4-Flash-UD-IQ3_XXS-00001-of-00004.gguf \
  --n-gpu-layers 999 --rpc <mac-ip>:50052 --tensor-split 2,1 \
  --ctx-size 32768 --jinja --host 0.0.0.0 --port 8080
```

Capture **decode tok/s, time-to-first-token, first-load time, and per-node shard
RSS** (watch the Mac's memory rise to ~its third and hold). Confirm the split is
real: the Mac's RSS should be roughly its slice, not the whole model.

**The gate** ([SHARED_MODEL_DESKTOP_PARKED.md](./internal/SHARED_MODEL_DESKTOP_PARKED.md)
names this exact measurement): the distributed core has only ever run at 4B/2 nodes.
If pooled decode is **below a few tok/s a user would tolerate, stop here** — the
model isn't practical on this hardware yet; revisit when the Vulkan/Metal kernels
mature. Only a passing number justifies Stage 3.

_Needs shared IP locality — LAN or Tailscale — because the tensor split talks raw
TCP between the GPUs._

## Stage 3 — get it usable in-product

Only if Stage 2 clears the bar. The binding is the wall here: we vendor the
`llama-cpp-4` *binding* at `vendor/llama-cpp-4` (a fork of eugenehp/llama-cpp-rs),
but the llama.cpp **C++ itself** ships inside `llama-cpp-sys-4 0.3.1`, which we pull
unmodified from crates.io — and that bundles llama.cpp `94a220cd6` (GLM-5 era, **no**
DeepSeek-V4). Two routes out, and for a moving WIP branch they are not equal:

**Route B — sidecar (recommended for the trial).** Run the WIP `llama-server` as an
external process and have the daemon proxy to its OpenAI-compatible endpoint. We
already treat an HTTP endpoint as an inference peer — mechanism B in the machinery
map (`worker_inference_proxy.rs`, `MeshInferenceProvider`, the pinned transport).
This keeps the volatile bleeding-edge llama.cpp **out** of the library every other
model links against, which is the correct posture while V4 support is a churning
branch. It's also continuous with Stage 2: the `llama-server` you stood up there
*becomes* the sidecar. Cost — you use llama.cpp's native RPC for the split, not our
content-addressed warm-cache path, and you skip our slot machinery.

**Route A — bump the binding (the eventual clean path, not the trial path).** Fork
`llama-cpp-sys-4` too (we don't today), replace its bundled llama.cpp with the WIP
branch — or cherry-pick #24162 + #25202 onto `94a220cd6` *if* the drift is small
enough to apply cleanly — fix `build.rs` (new attention/MXFP4 sources, Vulkan
shaders) and any bindgen/`llama.h` drift, then **regression-test GLM-5 and our
primary/fast/embed/code slots** so the bump doesn't break the models we already run.
Real FFI/build work, and volatile: redo it on every WIP rebase. This becomes a
normal version bump only once V4 support merges to llama.cpp master and eugenehp's
fork ships it in a `-sys-4` release — that's when Route A is worth it and Route B
retires. **Open question that decides A's cost:** how far `94a220cd6` sits from the
WIP branch's base commit — small drift → cherry-pick; large → full rebase. Check
before committing to A.

Once running (either route), point at the model the
[RUN_A_BIGGER_MODEL](./RUN_A_BIGGER_MODEL.md) way — `[models] primary =
".../UD-IQ3_XXS/...-00001-of-00004.gguf"`, Strix host `SOVEREIGN_RPC_DISCOVER=1`,
Mac worker `SOVEREIGN_RPC_SERVE=0.0.0.0:50052` — and run **both daemons under
`sovereign install-service`**: a worker dying mid-answer triggers an uncatchable
`GGML_ABORT` that can take the host down (upstream limitation, mitigated by the
eligibility gate + supervised restart).

## Stage 4 — distributed throughput probe (build during Stage 2)

The gate above needs a number we don't currently have a tool for. `mtp-probe.sh` is
MTP-local; `rpc-distributed-e2e.sh` proves the distribute chain but measures no
throughput. Build a small probe that streams a fixed completion against
`/v1/chat/completions` and reports **TTFT + steady-state decode tok/s + per-node
shard RSS**. It's the instrument the whole shared-fleet gate hangs on, and it's
useful whether we run standalone or integrated.

## Alternative engine — DwarfStar (`antirez/ds4`)

Kept in our back pocket as the **sidecar of choice** if the llama.cpp Vulkan path
disappoints. DwarfStar is a from-scratch inference engine (C + CUDA + Metal, **no
GGML**) purpose-built for DeepSeek-V4-Flash/PRO, with an `ds4-server` that speaks
OpenAI *and* Anthropic APIs — it drops straight into Route B. Why it tempts over the
WIP llama.cpp branch:

- **Fast, purpose-built kernels:** 26–35 tok/s generation on Metal (M3 Max 128GB q2:
  26.7; M3 Ultra q4: 35.5). The llama.cpp WIP path runs at 30–40% util with Flash
  Attention disabled even on CUDA.
- **No GGML** → our ggml-ROCm SEGV on gfx1151 (#20176) can't apply; ds4's ROCm path
  is its own code and might run where llama.cpp's can't. Strix Halo (Framework
  Desktop) is an explicit first-class target.
- **SSD streaming** (`--ssd-streaming --ssd-streaming-cache-experts NGB`) caches MoE
  experts on NVMe. With ~764 GB free on the Strix, this could run a *bigger* quant on
  the Strix **solo** — no mesh split, no worker-death aborts, no network hop. A
  materially simpler phase 1 if it holds up.

Two catches for us: (1) on the Strix it's **ROCm-only** (no Vulkan fallback) and no
Strix perf numbers are published — the make-or-break test is "does ds4-ROCm run clean
on gfx1151?"; (2) it runs **its own GGUF files** (`download_model.sh`, custom
asymmetric quants), so our unsloth IQ3_XXS won't load — going this way costs a
separate ~90 GB download. Maturity: days old, beta, single-author + GPT-5.5.

**Decision (2026-07-08):** probe the llama.cpp Vulkan path first (no new download);
fall back to ds4 only if Vulkan lacks the kernels or is too slow. Its make-or-break
(ROCm on gfx1151) is independent of llama.cpp's (Vulkan V4 kernels), so a failure on
one cleanly points to the other.

## Reference — the code this rides on

- **Distributed placement:** `sovereign/crates/sovereign-inference/src/embedded/rpc_distribution.rs`
  — `resolve_placement:706`, `classify_placement:670`, `plan_distribution:460`,
  `serve_rpc_worker_if_configured:821`. Threshold `SOVEREIGN_RPC_SAFE_STREAM_MB`
  (default 512).
- **Deadlock workaround:** `rpc_warm_cache.rs` — content-addressed shard cache
  (`Fnv1a`, `override_patterns`), so tensors load as `SET_TENSOR_HASH` cache hits
  with no bulk send. Validated Strix-Vulkan-host ↔ Mac-Metal-worker at 4B/2 nodes.
- **Slot load / CPU escape:** `model_slot.rs` — `ModelSlot::load:658`,
  `SOVEREIGN_FORCE_CPU_CHAT:681`.
- **Backend selection (compile-time):** `sovereign-inference/Cargo.toml:50-58`
  (Vulkan on Linux; #20176).
- **The binding (the Stage 3 wall):** `Cargo.toml:331-332` patches `llama-cpp-4` to
  the vendored fork `vendor/llama-cpp-4` (eugenehp/llama-cpp-rs); llama.cpp C++ comes
  from crates.io `llama-cpp-sys-4 0.3.1` (llama.cpp `94a220cd6`, per
  `sovereign-inference/Cargo.toml:60`). We do **not** fork `-sys-4` today.
- **The flow:** [RUN_A_BIGGER_MODEL.md](./RUN_A_BIGGER_MODEL.md), tuning knobs in
  [RPC_DISTRIBUTED_INFERENCE.md](./RPC_DISTRIBUTED_INFERENCE.md).

## Log

- **2026-07-08** — Picked IQ3_XXS (96.95 GB, actual) as the single download: fits
  solo on the Strix and is the comfortable pooled target. Download started. Plan
  staged; Stage 0 pending a WIP llama.cpp build. External refs: unsloth model card
  + docs, llama.cpp PR #24162 / #25202, the 2×96 GB Blackwell precedent.
- **2026-07-08** — Sized the binding blocker. We vendor `llama-cpp-4` at
  `vendor/llama-cpp-4` but consume `llama-cpp-sys-4 0.3.1` (llama.cpp `94a220cd6`)
  unmodified from crates.io — V4 isn't in it and the C++ lives in the crate we don't
  fork. Decided Stage 3 goes **Route B (sidecar)** for the trial, keeping the WIP
  llama.cpp out of the shared binding; Route A (fork `-sys-4`, bump/cherry-pick) is
  deferred until V4 merges to master. Stages 0–2 are unaffected (standalone binaries).
