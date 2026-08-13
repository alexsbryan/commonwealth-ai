# SSD expert-streaming vs mesh-distributed model load

_2026-08-13 · RuggedFox · research status: hypothesis with measured brackets, no new runs_

This document has two jobs: (1) record the design of the community's
SSD expert-streaming systems we may lift from, and (2) interrogate the
question the operator asked — **is our mesh distributed-model-load approach
actually better than SSD streaming, which the community is converging on?**

Sources: `antirez/ds4` ("DwarfStar", read 2026-08-13: `ds4_ssd.c/h`,
`ds4_streaming_hotlist*.inc`, `rocm/ds4_rocm_moe.cuh`, `speed-bench/`,
`STRIXHALO.md`); Mference/TurboFieldfare (reviewed in note 482260be);
our own measurements in `docs/RUN_DEEPSEEK_V4_FLASH.md` and the notes rail.

## 1. What SSD streaming actually is (antirez/ds4's shipped design)

Three pieces, all in the **planner**, none in kernels:

1. **Byte-budgeted residency.** `ds4_ssd_auto_cache_plan(recommended,
   non_routed_bytes, per_expert_bytes, max_experts)`: resident =
   non-routed dense skeleton + as many routed experts as fit in
   `recommended × 80%`. The 80% cap is OOM-aware for unified-memory APUs
   ("transient spikes ride on top of the steady-state plan; 80% was
   measured safe"). One knob, `DS4_SSD_AUTO_CACHE_PCT` (50–95).
2. **A hotlist, not an LRU.** `ds4_streaming_hotlist*.inc` is a *generated
   static profile* of `(layer, expert)` pairs sorted by hits/weight, baked
   from profiling runs. Cache fill follows the hotlist; everything else
   streams cold. No online replacement policy — expert popularity is stable
   across workloads, so the profile *is* the policy. Shipped profiles exist
   for V4 Pro and GLM 5.2; a Flash profile was not in the tree at read time
   (their gguf-tools generator would produce one).
3. **The perf model, stated in their own code:** "decode on the streaming
   path is SSD-bandwidth bound — every routed expert miss is a random NVMe
   read, so a larger resident expert cache is the biggest decode-throughput
   lever."

Supporting numbers (their bench): M2 Ultra, DSv4-Flash q2, Metal —
prefill ~410 t/s @2k ctx decaying to ~270 t/s @65k; generation 23 → 20 t/s.
Mference's measured primitives: cold expert read 9.88 ms via mmap vs 2.79 ms
via pread; full streaming sim 0.50 t/s (mmap) vs 3.97 t/s (parallel pread).

## 2. The mapping onto our topology

We already own most of their substrate under different names:

| Theirs | Ours |
|---|---|
| resident expert cache (RAM) | warm blocks in VRAM/RAM |
| cold experts on local NVMe | **worker-side content-addressed tensor cache on disk** (already shipped; note b87bb8ea proves the bytes land there) |
| skeleton always resident | our "hot skeleton 6.9 GiB / 5%" vs "137.1 GiB cold routed experts / 95%" split, already computed in `docs/RUN_DEEPSEEK_V4_FLASH.md` |
| byte-budget planner | **absent** — our fit gate is all-bytes-resident or refuse |
| hotlist-prioritized fill | **absent** — our warm is all-or-nothing (measured 2.6 h of serialization over Wi-Fi) |
| pread into preallocated slots | llama.cpp knows per-tensor offsets, so a sparse expert→(file, offset, len) index is implementable without their repacked layout |

The one insight genuinely new to us: **warm priority order**. A half-warmed
worker that can already serve (hotlist-first warm, misses stream from its
own disk cache) converts warm from a gate into an amortization curve.

The hard topology difference: their miss path is a local random NVMe read
(~2.8 ms). Ours would be a peer fetch over iroh — at Wi-Fi ~40 Mbps a single
13 MB expert block is ~2.6 s. **Streaming from a peer's disk is dead on
arrival; streaming from the worker's own disk cache (post-warm) is their
topology verbatim.** Warm is therefore the only network path that matters,
and it must be idle-amortized, not per-token.

## 3. The interrogation: is mesh-distributed load better than SSD streaming?

The honest answer: **they solve different constraints, and the community
trend is real — the winner depends on where the model sits relative to one
box's RAM.** There is no universal ranking; there are regimes.

| Regime | Local resident | SSD streaming | Mesh-distributed (ours) |
|---|---|---|---|
| model ≤ one box's RAM | **best** — our own data: Qwen122B 19.3 t/s local vs 7.75–11.08 split (~25% of blocks remote) | slower (why stream what fits?) | strictly worse than local |
| model > one box, ≤ pool | — | runnable at miss-bound throughput | resident everywhere after warm; per-token cost = activation RPC traffic, not weight reads |
| model >> pool (1.6T PRO class) | — | runnable on 512 GB boxes | pool too small; the community's answer here is **pipeline-parallel halves loaded from each machine's own disk** (their `pro-q4-layers00-30` / `-output` split) — i.e., their file-level split, not ours |

Measured brackets we already hold (all cited in the notes rail):
- Distributed Qwen 122B: 7.75–11.08 t/s vs 19.34 local — **splitting cost ~half
  the throughput even on a LAN**, before any Wi-Fi penalty.
- Distributed DSv4 Q4: 505 ms/token (1.56 t/s), with the abort-class bugs
  since fixed upstream/pinned but not re-measured.
- Their streaming envelope (M2 Ultra, q2): 20–23 t/s decode.
- Fully-cold SSD streaming at Q4 arithmetic (ours, note 482260be): ~3.4 GB
  of expert reads/token ÷ ~5 GB/s NVMe (assumed, not measured) ≈ 680 ms/token
  floor — roughly where our *distributed* DSv4 already sits.

Reading the regime table: the community's SSD-streaming popularity is not
evidence against the mesh; it is evidence about **which constraint most users
have** — one machine, RAM < model, fast local NVMe, no fleet. Our product's
premise is a fleet. The two approaches are **complementary tiers**, not
competitors:

1. **RAM-resident** (primary box, or split across the pool) — highest
   throughput, highest residency cost.
2. **Local-NVMe streaming** (per machine, hotlist-biased) — the fallback that
   makes a too-big model runnable on one box; also the natural per-machine
   behavior *underneath* a file-level pipeline split.
3. **Peer disk / warm** (mesh) — bytes move only during idle windows, never
   on the token path.

The synthesis is a tier-selection planner: skeleton-gated fit, byte-budgeted
residency, hotlist-prioritized warm, miss path = local disk only. That is a
general mechanism (every MoE has the 95%-cold shape — Qwen3.6, GLM, DSv4),
lives in our planner code, and is llama.cpp-agnostic. It passes the
generalizable-product test; their kernels and repacked layouts do not.

What streaming does **not** buy us, honestly stated: throughput. Their own
numbers never claim streaming is *fast* — it makes sub-96 GB machines
*runnable*. At Q4, the fully-cold floor (~680 ms/token) is our current
distributed number. Streaming's value for us is fit-gate relaxation and
single-machine ops, not speed.

## 4. Falsifiable probes (none run yet)

1. **Expert-hit distribution from one real DSv4 run** — does the Pro hotlist
   correlate with Flash routing? (llama.cpp router stats / our dumps). If the
   skew holds, the whole mechanism rests on data one run provides.
2. **Single-box streaming DSv4 Q4 on RuggedFox** — hotlist-cache + local
   NVMe, vs our 505 ms/token distributed. This settles the regime question on
   our own hardware without any mesh.
3. **Warm-priority probe** — hotlist-first warm ordering; measure
   time-to-first-serve instead of time-to-full-warm (the 2.6 h serialization
   is the number to beat).
4. **Qwen3.6-35B-A3B head-to-head** — we ship it; Mference measured
   18.8–23.1 t/s streamed at ~1.45 GiB footprint. Same model, our resident
   path vs their streaming envelope brackets the tradeoff sharply.

## 5. Open items

- Flash hotlist profile (theirs ships Pro + GLM only) — either adopt theirs
  if/when published, or generate ours from probe 1.
- Our NVMe read bandwidth is **assumed** ~5 GB/s in the arithmetic above —
  measure before any planner work (§18.4).
- Whether `llama.cpp` ever grows expert streaming upstream remains on the
  watch list (still absent as of 2026-08-12); our planner layer does not
  depend on it.
