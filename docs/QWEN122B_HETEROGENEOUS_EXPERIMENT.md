# Heterogeneous distributed inference — the Qwen-122B experiment

**Goal:** demonstrate, and honestly characterise, **distributed inference across
heterogeneous hardware** — one model split across a Strix Halo (AMD, Vulkan) and a
Mac (Apple Silicon, Metal), over the network, on our own mesh. Qwen3.5-122B is the
vehicle, chosen because it already runs on our bindings on *both* backends and fits
the Strix solo — which is precisely what lets us measure **one box vs. distributed**
on the identical model. This is [Run a model bigger than your
machine](./RUN_A_BIGGER_MODEL.md) turned into a measured result, and it closes rung 1
of the SHARED_MODEL ladder (`122B across 2 nodes`,
[SHARED_MODEL_DESKTOP_PARKED.md](./internal/SHARED_MODEL_DESKTOP_PARKED.md)).

It is the real target that the parked [DeepSeek-V4-Flash
exploration](./RUN_DEEPSEEK_V4_FLASH.md) was a detour from: the impressive thing here
is *our mesh doing the split across two different GPU backends*, not any one model.

## Objective & hypotheses

We are testing two distinct things — keep them separate:

- **Capability (does it work?):** one Qwen-122B model, its layers split across a
  Vulkan host and a Metal worker, produces correct tokens, with each node holding
  only its shard.
- **Cost (what does it cost?):** how much decode throughput the network hop takes,
  versus running the whole model on the Strix alone.

Hypotheses:

- **H1 — functional.** The model runs correctly split across Vulkan + Metal; the
  Mac's resident memory equals ~its shard, not the whole model.
- **H2 — distribution tax.** Distributed decode t/s < solo decode t/s (we've added a
  per-layer activation handoff over the wire, for no capacity we needed at this
  quant). We quantify the ratio.
- **H3 — offload curve.** As more layers move to the remote Metal node
  (`SOVEREIGN_RPC_TENSOR_SPLIT` sweep), aggregate decode t/s changes predictably —
  decode is latency-bound on the per-layer hop, so more remote layers ≈ more hops ≈
  slower; prefill *may* benefit from the Mac's bandwidth. Measure both.
- **H4 — load amortises.** Distributed adds a one-time shard-warm/seed cost at load;
  it does not recur per request.
- **Open question — cross-backend fidelity.** Under greedy decoding, does the
  distributed (Vulkan+Metal) run produce the *same tokens* as solo (Vulkan)? Divergence
  would reveal numerical differences between the two backends' kernels. Worth checking.

## Hardware & why "heterogeneous" is the point

| Node | Role | GPU / backend | Memory | Notes |
|---|---|---|---|---|
| **Strix Halo** | host | Radeon 8060S, **Vulkan (RADV)** | 128 GB unified (~116 GB usable) | holds the GGUF; coordinates |
| **BeefyMac** | worker | Apple Silicon, **Metal** | 64 GB unified (~50 GB usable) | lends memory + compute; no model on disk |

The split spans **two different compute backends over a network link** — that is the
demonstration. Note the Mac (64 GB) **cannot run Qwen-122B alone** (86–95 GB), so the
pool genuinely enables something one of the boxes can't do, even though the *Strix*
can hold this particular quant. Pool ≈ 166 GB usable.

## The model

Qwen3.5-122B-A10B — **10 B active** MoE, so decode is cheap per token and
memory-bandwidth/latency-bound (favourable for splitting). On disk today:

- **`Qwen3.5-122B-A10B-UD-Q5_K_XL` — 86 GB** (3 shards) — **primary** for the
  experiment: most headroom, matches the ~87.6 GB / ~14.8 tok/s prior solo reading.
- `Qwen3.5-122B-A10B-Q6_K` — 95 GB (4 shards) — higher-quality alternate; tighter
  solo headroom.

Point the daemon at shard `00001`; llama.cpp pulls the rest. Pin **one** quant for
the whole experiment so every number is comparable.

## How our system decides to distribute (grounding the method)

From `rpc_distribution.rs::classify_placement`: a **primary** slot distributes when a
worker is present and `model_bytes > SOVEREIGN_RPC_SAFE_STREAM_MB` (default 512 MB) —
it does **not** check whether the model fits locally. Since 86 GB ≫ 512 MB, a Qwen-122B
primary takes the `OwnedOverrides{auto_warm}` path (warm-cache seed + `-ot` overrides,
the deadlock-safe route) whenever the Mac worker is up and the daemon's orchestrator is
wired. Therefore:

- **Solo arm = no worker present** (or don't start discovery) → `LocalOnly` → whole
  model on the Strix.
- **Distributed arm = bring the Mac worker up** → auto-splits, no force flag needed.

Knobs we use: `SOVEREIGN_RPC_TENSOR_SPLIT` (Strix:Mac ratio), `SOVEREIGN_RPC_DISCOVER`
/ `SOVEREIGN_RPC_SERVE` (roles), `SOVEREIGN_RPC_WORKERS` (explicit worker list),
`SOVEREIGN_RPC_CACHE_DIR` (warm-cache location), `SOVEREIGN_RPC_ASSUME_WARMED`.

## Experiment 1 — the cost of the heterogeneous hop (zero new download)

Same model, same everything, worker on vs. off. This is the controlled A/B.

| Arm | Setup | Isolates |
|---|---|---|
| **A0 — Solo Strix** | Vulkan, no worker | baseline decode/prefill t/s (expect ~14.8 t/s) |
| **B1 — Distributed, auto-split** | Strix Vulkan host + Mac Metal worker, default VRAM-proportional split (~70/30) | the headline distributed number |
| **B2 — Split sweep** | B1 with `SOVEREIGN_RPC_TENSOR_SPLIT` set to Mac-share ≈ 10 % / 25 % / 40 % | H3: how t/s degrades as more layers go remote |
| **B3 — Native control** *(optional)* | upstream `llama-server --rpc … --tensor-split` (our pinned llama.cpp), same ratio as B1 | our transport overhead vs. llama.cpp's inherent RPC cost (B1 vs B3) |
| **A0′ — Solo Mac** | — | *n/a: 86 GB > 64 GB, won't load.* Record as "cannot run alone" — the motivation for pooling. |

**Comparisons that matter:** `B1/A0` = the distribution tax; the `B2` curve = the
offload cost function; `B1 vs B3` = Sovereign transport overhead; cold-load delta =
the one-time seed cost (H4).

## Experiment 2 — the value demo (one download, later)

Experiment 1 measures cost on a model that fits one box. Experiment 2 shows the
*point*: a model that **won't** fit either box, run only because the pool exists.

- **Model:** a mainline-llama.cpp model in the ~120–150 GB band (fits the ~166 GB
  pool, exceeds the Strix's ~116 GB) — e.g. Qwen-122B at **Q8** (~130 GB, one
  download) or Llama-3.1-405B at IQ2/IQ3. Selection criteria: mainline support, runs
  Vulkan+Metal, fits pool / not one box.
- **Arms:** Solo Strix → **fails/OOMs to load** (that's the result); Distributed →
  runs. No solo baseline exists, so the "comparison" is *capability* (runs vs. can't)
  plus absolute t/s.

Stage this after Experiment 1 clears, so we don't spend a download before the method
is proven.

## Metrics — defined precisely

All via the probe (below); greedy decoding (`temperature 0`) for reproducibility.

- **Cold load / first-ready (s)** — daemon start → model ready to serve. For
  distributed, *includes* the one-time shard warm-seed; report that seed sub-time
  separately.
- **TTFT (ms)** — request submit → first token.
- **Prefill throughput (tok/s)** — prompt tokens ingested / prompt time, at a **fixed
  prompt length** (use 512 and 2048 to see the trend).
- **Decode throughput (tok/s)** — the headline. Steady-state generation over a
  **fixed 256-token** completion, *excluding* TTFT.
- **Inter-token latency p50 / p95 (ms)** — network jitter surfaces here for the
  distributed arms.
- **Per-node resident memory** — Strix VRAM/RSS and Mac VRAM/RSS. Proves the split is
  real (Mac RSS ≈ its shard) and quantifies each share.
- **Output fidelity** — does the distributed completion match solo token-for-token
  under greedy? (the cross-backend numerical check).

Protocol: **N = 5** warm trials per condition; report **median [min, max]**. Discard
the first request (cache warm-up). Report cold-load once.

## Instrument — the throughput probe

Build the small tool the SHARED_MODEL gate named as missing (`mtp-probe.sh` is
MTP-local; `rpc-distributed-e2e.sh` proves the chain but measures no t/s). It streams a
fixed completion against `/v1/chat/completions`, parses the SSE stream, and emits
**TTFT + steady-state decode t/s + inter-token p50/p95**. One JSON line per run so the
results table is mechanical to fill. **Built & validated 2026-07-09** —
`scripts/throughput_probe.py`, stdlib-only, streaming SSE, endpoint-agnostic (point
`--url` at the daemon or a raw `llama-server`).

Per-node memory is read **out-of-band** (`free` / `rocm-smi` / Metal counters), not by
the probe — on Vulkan unified memory the model is GPU-allocated and does *not* show in
process RSS (see the A0 memory note). Known gap: prefill t/s needs the stream `usage`
block, which our server doesn't emit yet.

## Controls & confounds

- **Same llama.cpp/binding version on both nodes** — RPC is wire-version-sensitive.
  Both run our built daemon (Strix = vulkan feature, Mac = metal feature).
- **Network characterised** — record link type (wired Ethernet / Wi-Fi / Thunderbolt),
  `ping` RTT, and `iperf3` bandwidth between the boxes. Distributed decode is bounded
  by **RTT × per-layer hops**, so this is a first-order variable, not a footnote.
- **Thermal** — the Strix throttles under sustained generation; warm to steady state
  and note any decline across the 5 trials.
- **Determinism** — greedy (`temp 0`), fixed prompt + max_tokens, fixed ctx (e.g.
  8192). No competing GPU load; don't run other slots that would contend for memory.
- **Shared IP** — the tensor split talks raw TCP between GPUs; both nodes need LAN or
  Tailscale locality.

## Procedure

**Prereqs (once):** both boxes joined (`sovereign mesh join`), same daemon build, model
on the Strix, network characterised (log RTT + iperf3).

**A0 — solo Strix:**
```toml
# ~/.sovereign/config.toml
[models]
primary = ".../Qwen3.5-122B-A10B-UD-Q5_K_XL/Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003.gguf"
```
Start the daemon with **no** worker / no discovery → `LocalOnly`. Run the probe → A0 row.

**B1 — distributed, auto-split:**
```sh
# BeefyMac (worker)
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 sovereign daemon run
# Strix (host) — orchestrator auto-warms the Mac's shard, then splits
SOVEREIGN_RPC_DISCOVER=1 sovereign daemon run
```
Wait for warm-seed to finish (note the seed time). Confirm the split: Mac RSS rises to
~its share and holds. Run the probe → B1 row.

**B2 — sweep:** re-run B1 with, on the Strix,
`SOVEREIGN_RPC_TENSOR_SPLIT=<ratio>` for Mac-share ≈ 10/25/40 %. Probe each → B2 rows.

**B3 — native control (optional):** upstream `rpc-server -c -p 50052` on the Mac;
`llama-server --model … --rpc <mac-ip>:50052 --tensor-split 70,30 --host 0.0.0.0
--port 8080` on the Strix. Probe `:8080` → B3 row.

Run **both daemons under `sovereign install-service`** if leaving it unattended — a
worker dying mid-answer triggers an uncatchable `GGML_ABORT` that can take the host
down (see Risks).

## Results (fill in)

_Model: Qwen3.5-122B-A10B-UD-Q5_K_XL (86 GB) · ctx 32768 · greedy · N=5 median [min,max]_
_Link: ____ · RTT ____ ms · iperf3 ____ Gbit/s_
_Probe: `scripts/throughput_probe.py --trials 5 --warmup 1 --max-tokens 256`_

| Arm | Cold load (s) | Seed (s) | TTFT (ms) | Prefill t/s @512 | Decode t/s | ITL p95 (ms) | Strix RSS | Mac RSS | Out == solo? |
|---|---|---|---|---|---|---|---|---|---|
| A0 solo Strix | 110 | — | 895 | n/a | **19.34** [19.0, 19.9] | 53.7 | ~86 GB (sys 122 GiB) | — | (ref) |
| B1 dist ~70/30 | | | | | | | | | |
| B2 Mac 10 % | | | | | | | | | |
| B2 Mac 25 % | | | | | | | | | |
| B2 Mac 40 % | | | | | | | | | |
| B3 native ctrl | | | | | | | | | |

**A0 recorded 2026-07-09.** Notes: *Prefill t/s is n/a* — our SSE stream omits `usage`,
so live prompt-token count isn't available (fix: honour `include_usage`, or read tokens
from the daemon log). *Memory* — on Vulkan unified memory the weights are GPU-allocated,
**not** in process RSS (daemon RSS was 1.4 GB); residency shows in system `free`
(~122 GiB used with the 122B loaded — the box is near-full solo at this quant). For the
B arms, read each node's share via `free` / `rocm-smi` / Metal counters, **not** process
RSS. Cold load was ~110 s (the 86 GB); warm TTFT ~0.9 s.

## Success criteria & the decision it feeds

- **Demonstration (pass/fail):** B1 produces coherent, correct output **and** the Mac
  holds only its shard (H1). That alone is "distributed inference across heterogeneous
  hardware, demonstrated on our mesh." Any decode t/s is a *successful demonstration
  with an honest cost* — a low number does not fail the demo.
- **Productization gate (separate):** the SHARED_MODEL threshold — is distributed
  decode above "a few tok/s a user tolerates"? That decides whether we build the
  download/UX around it, per SHARED_MODEL_DESKTOP_PARKED.md. This experiment supplies
  that number; it doesn't have to *pass* it to be worth running.

## Risks & honest caveats

- **Worker-death is an uncatchable abort.** If the Mac drops mid-generation, ggml's
  RPC client `GGML_ABORT`s and can kill the host daemon. Run under `install-service`.
  Optionally characterise it: kill the worker mid-request and record the recovery
  behaviour — it's part of the honest resilience story.
- **Network variance dominates.** Wi-Fi will read far worse than wired; always report
  the link. A bad number on Wi-Fi is a link result, not a system result.
- **Thermal drift** on the Strix over sustained runs — watch the 5-trial trend.
- **Cross-backend numerical drift** may make B1 diverge from A0 token-for-token even
  under greedy; that's a finding (backend kernel differences), not necessarily a bug.
- **Expect distributed < solo here.** At this quant the model fits the Strix, so B1 is
  *expected* to be slower than A0 — Experiment 1 measures a tax we pay for capability
  we don't need at this size. The value case is Experiment 2.

## Reference — the code this rides on

- **Placement decision:** `rpc_distribution.rs::classify_placement:670`
  (`> SAFE_STREAM_MB` + worker + orchestrator → distribute), `resolve_placement:706`,
  `plan_distribution:460` (VRAM-proportional shards), `SOVEREIGN_RPC_TENSOR_SPLIT`
  (`:112`, `model_slot.rs:718`).
- **Deadlock-safe seed:** `rpc_warm_cache.rs` (content-addressed shard cache) — the
  path validated Strix-Vulkan ↔ Mac-Metal at 4B.
- **The flow / knobs:** [RUN_A_BIGGER_MODEL.md](./RUN_A_BIGGER_MODEL.md),
  [RPC_DISTRIBUTED_INFERENCE.md](./RPC_DISTRIBUTED_INFERENCE.md).
- **The gate this feeds:** [SHARED_MODEL_DESKTOP_PARKED.md](./internal/SHARED_MODEL_DESKTOP_PARKED.md).

## Log

- **2026-07-09** — Experiment designed. Confirmed our placement auto-distributes any
  >512 MB primary when a worker is present (no force-knob needed; solo = worker
  absent). Models on disk: UD-Q5_K_XL (86 GB, primary) / Q6_K (95 GB). Next: build the
  throughput probe, then run A0 → B1 → sweep. Experiment 2 (oversized model) deferred
  until Exp 1 clears.
- **2026-07-09** — Built `scripts/throughput_probe.py` (streaming TTFT + decode t/s +
  ITL p50/p95, JSON row); validated on the 9B, then swapped the daemon primary to
  Qwen-122B-Q5_K_XL and recorded **A0 = 19.34 t/s decode** [19.0, 19.9], TTFT 895 ms,
  ITL p95 53.7 ms, ctx 32768, greedy-deterministic. Cold load ~110 s; box ~122 GiB used
  (near-full solo). Config backed up (`config.toml.bak-preqwen122b-*`); 122B left as
  primary for B1. Learnings: process RSS ≠ residency on Vulkan unified mem (read `free`);
  server omits stream `usage` (prefill t/s blocked). Next: BeefyMac worker → B1.
