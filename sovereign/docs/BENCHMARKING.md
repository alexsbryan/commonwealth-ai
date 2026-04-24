# Embed benchmarking

Runbook for measuring `EmbedSlot` decode throughput across GPU
backends (Metal / Vulkan / ROCm). Started 2026-04-24 after the
"embed was CPU-pinned on Linux" regression — we now need a shared
methodology so future tuning runs produce apples-to-apples numbers.

Benchmark entry point: `crates/sovereign-inference/examples/bench_embed.rs`.
It stands up a raw llama.cpp context (bypassing `EmbedSlot`) with
configurable `n_threads_batch`, `n_ctx`, `n_seq_max`, and backend
flags, runs one warm iteration, then three timed ones.

## When to run

- Swapping GPU backends (ROCm ↔ Vulkan, Metal upgrade).
- Upgrading `llama-cpp-2` / llama.cpp.
- Changing embed-slot context params (`n_ctx`, `n_seq_max`, `n_ubatch`).
- Trying a new embed-model quant (Q8_0 → Q4_K_M, etc.).
- Any "ingest feels slow" investigation.

## How to run

```bash
cd ~/dev/commonwealth-ai/sovereign
cargo build --release -p sovereign-inference --example bench_embed

MODEL=~/path/to/qwen-embedding-0.6b.Q8_0.gguf
BENCH=target/release/examples/bench_embed

run() {
  echo "=== $1 ==="
  $BENCH --model $MODEL --backend metal --threads 8 $2 --iters 3 2>&1 | \
    grep -E "^ *[0-9]+ +[0-9]+ +[0-9]+|Failed to allocate|ggml_vulkan: [a-zA-Z]" | head -10
}

# Baseline: matches production sub-batch exactly (16 seqs × 400 tok)
run "base"           "--n-seq-max 16 --seqs 16 --tokens-per-seq 400 --n-ctx 16384"
# Half work: confirms per-token linearity
run "half-work"      "--n-seq-max 16 --seqs 16 --tokens-per-seq 200 --n-ctx 16384"
# 2× parallelism at same work: does the scheduler keep CUs busier?
run "2x-par-eq-work" "--n-seq-max 32 --seqs 32 --tokens-per-seq 200 --n-ctx 16384"
# Larger single batch: tests kernel amortisation
run "2x-work"        "--n-seq-max 16 --seqs 16 --tokens-per-seq 800 --n-ctx 16384"
# Larger n_ctx, more parallelism: room for more KV?
run "big-ctx"        "--n-seq-max 32 --seqs 32 --tokens-per-seq 400 --n-ctx 32768"
```

The `--backend metal` flag looks wrong for Vulkan/ROCm but isn't:
the enum predates multi-backend Linux and just toggles
`offload_kqv=true / op_offload=true / n_gpu_layers=999`. On a crate
compiled against llama-cpp-2's `rocm` or `vulkan` feature, those
flags route to that backend.

`--backend cpu` gives the CPU-fallback baseline (what Linux ran
until 2026-04-24's `gpu embed + kqv on linux` commit).

## Gotchas + pitfalls

Before interpreting numbers, check for these — they've all bitten us:

- **`LlamaBatch::new(n_ctx, n_seq_max)` caps total tokens at `n_ctx`.**
  `seqs × tokens_per_seq > n_ctx` panics with `batch.add:
  InsufficientSpace(<n_ctx>)`. If you want 64 seqs × 400 tokens,
  bump `--n-ctx` to at least 32768. (Memory cost: linear in n_ctx,
  negligible on Strix Halo's 124 GiB GTT.)

- **`ggml_vulkan: Failed to allocate pinned memory ... ErrorOutOfDeviceMemory`**
  fires above ~6,400 total tokens per batch on Mesa RADV. The
  backend falls back to non-pinned host memory and throughput drops
  ~20%. Upstream llama.cpp isn't UMA-aware for this code path yet —
  on unified-memory iGPUs it shouldn't be trying to pin host memory
  at all. Log the warning + the throughput delta; don't treat a
  config that hits this as a valid comparison.

- **Bench vs production rates don't match directly.** The bench's
  timed region includes `str_to_token` + `embeddings_seq_ith`
  readback; production overlaps those with the next batch's GPU
  work. Expect production tok/s to be ~1.5–2× higher than
  single-decode bench numbers for the same config.

- **The bench runs with `with_n_seq_max = n_seq_max` directly;
  `EmbedSlot` in production hardcodes `n_seq_max = 16` at load
  time** (see `embedded.rs:528`). A bench finding that e.g. 32-way
  parallelism helps doesn't automatically translate — you'd need
  to also change the EmbedSlot config.

- **Model load time is excluded** (the bench calls
  `LlamaModel::load_from_file` before `Instant::now()`). For
  Strix Halo a 603 MB Q8 model takes ~400 ms to mmap + warm; if
  you're ever timing the whole process, account for that.

## How to read results

Table columns from the bench:

```
 n_threads  iter  wall_ms  seqs/sec  tok/sec
```

Key questions to ask of any run:

1. **Does tok/s stay flat as you vary parallelism at constant total
   tokens?** If yes, the GPU is compute-bound and more parallelism
   won't help. If tok/s rises 16 → 32 → 64, we're under-utilising
   and `EmbedSlot::load`'s `n_seq_max` is worth raising.
2. **What's the total-token threshold before the pinned-mem
   warning fires?** That's your effective per-sub-batch ceiling.
3. **How far are you from `peak_fp16_tflops / flops_per_token`?**
   For Qwen3-Embed-0.6B (28 layers, 1024 dim) that's ~784M FLOPS
   per token — divide your GPU's fp16 TFLOPS by that for the
   theoretical ceiling. Real-world will be 10–30% of that on
   Mesa RADV for small models; more on ROCm; more still on Metal.

## Recorded baselines

**Strix Halo (Radeon 8060S, RADV GFX1151) — Vulkan, Mesa 25.3.6**
<br>*Model: Qwen3-Embedding-0.6B-Q8_0 (603 MB, 28 layers, 1024 dim)*
<br>*llama-cpp-2 0.1.145, sovereign commit 7bcea5d, 2026-04-24*

| config | seqs | tok/seq | wall ms | seq/s | tok/s | notes |
|---|---|---|---|---|---|---|
| base          | 16 | 400 |   820 | 19.5 | 8,500 | |
| half-work     | 16 | 200 |   410 | 39.0 | 8,475 | |
| 2x-par-eq     | 32 | 200 |   815 | 39.3 | 8,475 | |
| 2x-work       | 16 | 800 | 2,045 |  7.8 | 6,770 | pinned-mem fail |
| big-ctx       | 32 | 400 | 2,025 | 15.8 | 6,900 | pinned-mem fail |
| 4x-par        | 64 | 100 | 1,000 | 63.8 | 7,140 | pinned-mem fail |

Peak observed: **~8,500 tok/s** (any config that stays under the
~6,400-total-token pinned-mem cliff). About 11% of the theoretical
fp16 ceiling (59 TFLOPS / 784 MFLOPS/token = 75K tok/s). Low
utilisation is a known RADV characteristic for small-dim matmuls.

Production `wikipedia` ingest on this config: **~42 chunks/s,
effective 17K tok/s** (overlapped-batch amplification over the
bench's single-decode rate).

### Adding a new backend

When you bench on a new backend, add a section with:
1. Device name + driver version (`vulkaninfo --summary | grep
   deviceName` or `rocminfo | grep Name` or equivalent).
2. The same table columns — keep the config rows identical so
   cross-backend reading is easy.
3. Peak observed + utilisation % of theoretical fp16 TFLOPS.
4. The production ingest chunks/s with the same corpus if possible
   (Wikipedia is a reasonable reference; keep chunker settings
   identical).

## Open questions / things to try next

- **Q4_K_M quant** of the same embed model. Q8_0 → Q4_K_M halves
  weight bandwidth and dequant cost; expected 30–50% throughput
  win on compute-bound GPUs, ~1% quality loss for ANN cosine
  retrieval in the existing literature. We haven't run it.
- **Lower `n_ubatch`** (currently 2048 in both `EmbedSlot::load`
  and the bench). Dropping to 1024 or 512 might keep pinned-mem
  staging below the RADV limit for longer sub-batches.
- **ROCm vs Vulkan on the same Strix Halo hardware.** kyuz0's
  benchmarks suggest 20–40% compute-bound win for ROCm over
  Vulkan on small models. Rerun this bench in a ROCm toolbox and
  fill in the recorded-baselines section.
- **Unified-memory-aware pinned allocation in upstream llama.cpp.**
  The `Failed to allocate pinned memory` message is avoidable on
  iGPU; worth an upstream issue.
