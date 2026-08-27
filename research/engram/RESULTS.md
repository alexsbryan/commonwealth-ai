# Engram hot-set experiment — results (2026-08-26, RuggedFox)

Corpora: 54.1M tokens, real Qwen3.8 vocab (max_id 248,069) via llama-tokenize
--vocab-only on Qwen3.8-27B-UD-Q6_K_XL.gguf.
  sep_mine 14,497,455 | sep_holdout 14,301,769 | repo_md 15,855,182 | rust_src 9,431,903

## 1. Static hot-set coverage (mined sep_mine, K = rows/head, mean of 2 seeds)
kind     eval          ceiling   @100k   @1M    @2M    @8M
bigram   sep_holdout   0.907     0.702   0.877  0.907  0.907
bigram   repo_md       0.467     0.285   0.417  0.467  0.467
bigram   rust_src      0.272     0.122   0.230  0.272  0.272
trigram  sep_holdout   0.706     0.322   0.513  0.580  0.706
trigram  repo_md       0.400     0.101   0.182  0.237  0.400
trigram  rust_src      0.317     0.029   0.087  0.143  0.317

Hot-set bytes = 16 heads x K x 170 B (Q8_0).  K=1M -> 2.72 GB.  K=2M -> 5.44 GB.
At 2.72 GB, held-out hit rate = (0.877+0.513)/2 = 0.695.

VERDICT vs pre-registered bars: RED. 95% held-out is unreachable at ANY K.

## 2. Ceiling is mining-budget-limited and grows logarithmically
mine toks   bigram ceil   trigram ceil   uniq trigram rows
   500,000        0.644          0.288             339,276
 1,000,000        0.709          0.354             619,095
 2,000,000        0.779          0.436           1,158,679
 4,000,000        0.830          0.520           1,995,067
 8,000,000        0.876          0.619           3,517,373
14,497,455        0.907          0.706           5,444,616

Heaps fit on trigram rows: beta ~= 0.824. Occupancy 1-exp(-distinct/20M).
5.44M occupied <=> ~6.35M distinct trigrams from 14.5M tokens.
90% occupancy (46M distinct) reached at ~1.6e8 tokens.
=> After ~160M tokens of ordinary text, 90% of the 20M trigram rows are live.
   The model saw trillions. The table is DENSE-in-use; there is no cold part.

## 3. NULL (pre-registered): shuffled tokens, unigrams preserved
kind      uniq rows mined   @1M rows   ceiling
bigram    4,336,904         0.692      0.808   (real: 1,765,635 / 0.877 / 0.907)
trigram   9,203,745         0.167      0.564   (real: 5,444,616 / 0.513 / 0.706)
=> Concentration is genuine n-gram structure, not a unigram artifact. PASS.

## 4. NVMe 16-way concurrent gather (the real access pattern), 85.6 GiB file
                    p50        p90        p99
COLD (fadvised)   376.3 us   533.5 us   710.8 us
WARM (cached)     284.0 us   445.6 us   526.1 us
Marginal I/O = 376-284 = ~92 us. The 284 us floor is Python pool dispatch,
not device time; a Rust/io_uring implementation pays less.

Against our own measured transport envelope (note qwen122b-iroh-transport-
characterization): iroh DIRECT 16KB rt p50 13.3 ms; RELAY 141-182 ms.
NVMe beats iroh-direct by ~35x on marginal cost, ~140x vs relay.

## 5. Quantization constraint (verified in ggml source)
Row width is 160. QK_K = 256 (ggml-common.h:89); legacy blocks are 32
(QK4_0/QK5_0/QK8_0). ggml.c:1341 asserts ne % ggml_blck_size(type) == 0.
160 = 5x32 but 160 % 256 != 0  =>  K-quants are STRUCTURALLY UNAVAILABLE for
the engram tensor. Only legacy 32-blocks (or a new row-aligned type) apply.
  row bytes: F16 320 | Q8_0 170 | Q5_0 110 | Q4_0 90
  table:     F16 102.4 GB | Q8_0 54.4 GB | Q5_0 35.2 GB | Q4_0 28.8 GB
  per-token gather (16 rows): F16 5,120 B | Q8_0 2,720 B | Q4_0 1,440 B
Disk free on this host: 580 GB. => the engram never needs quantizing at all.

## 6. Placement + throughput, measured on RuggedFox (2026-08-26)
Qwen3.8-Flash-Next-UD-Q4_K_XL, 103.7 GiB across 4 shards, all sha256-verified.
llama-completion, -ngl 99, -c 4096, -n 128, seed 42, daemon stopped.
GTT sampled 1 Hz from mem_info_gtt_used; faults from /proc/<pid>/stat.

run                       peak_gtt   peak_rss   majflt   tok/s
-ot per_layer=CPU (warm)  79.63      27.00      140      21.39
-ot per_layer=CPU (cold)  79.62      26.98       90      22.38
no override               79.68      26.99       30      22.45
--no-host                 79.05      27.62        0      22.42

Model 103.7 GiB = 79.6 GTT + 27.0 CPU RSS + ~3 compute/KV. The engram
(26.8 GiB) is NOT on the GPU: resident would be ~104 GiB, not ~80.

### 6.1 The -ot override is a NO-OP here; the default already does it
per_layer_token_embd is classified LLM_TENSOR_LAYER_INPUT / GGML_OP_GET_ROWS
(llama-arch.cpp:887), same class as token_embd (:696), so it is drawn from the
CPU buft list -- with or without -ot. The A/B above is flat within noise.

### 6.2 "=CPU" does NOT mean pageable CPU -- it means Vulkan_Host (PINNED)
make_cpu_buft_list (llama-model.cpp:1012) puts the device HOST buffer type
FIRST (:1033), ahead of plain CPU. Verified with -v on a forced override:
  tensor blk.0.ffn_down_exps.weight (600 MiB q5_1) buffer type overridden to Vulkan_Host
Pinned host memory is the OPPOSITE of the evictability this design wants.

### 6.3 The engram is evictable BY ACCIDENT OF ITS QUANT, not by design
  tensor 'per_layer_token_embd.weight' (iq4_nl) (and 30 others) cannot be used
  with preferred buffer type Vulkan_Host, using CPU instead
iq4_nl is unsupported by Vulkan_Host, so it falls back to plain pageable CPU.
Per RESULTS 5, iq4_nl was forced by row width 160 (160 % 256 != 0). The property
the design depends on is therefore load-bearing and unguarded.
=> --no-host makes it STRUCTURAL at zero throughput cost (22.42 vs 22.45).

### 6.4 It is NOT demand-paged; it is prefetched
llama-mmap.cpp:455 sets MAP_POPULATE and :463 posix_madvise(WILLNEED) whenever
prefetch != 0 (llama-model-loader.cpp:1369 passes -1 when use_mmap). So the
whole 26.8 GiB is pulled into page cache at load: 7.08M MINOR faults (= 27 GiB
/ 4 KiB) and ~0 major, even after fadvise(DONTNEED) on all four shards.
During generation proper: 9 major faults over 127 tokens. The 376 us cold-gather
cost from RESULTS 4 is real but is NOT being paid on this path.
CORRECTION (same day): the prefetch=0 path does NOT apply whole-file
MADV_RANDOM -- that call at llama-mmap.cpp:469 is gated on `numa`, not on
prefetch. prefetch=0 simply drops MAP_POPULATE and WILLNEED; the fd keeps
POSIX_FADV_SEQUENTIAL, so readahead for weights STREAMED to device buffers
survives. That is what made patch 0008 a one-liner instead of a rework.

### 6.5 Verdict vs the frame's bars
tok/s 22.4 > 19.34 (122B solo)  => worth continuing, +16%.
But the frame PROJECTED ~33.5 tok/s; measured is 33% under projection.
The projection should not be carried forward.


## 7. Serving it from the daemon (2026-08-26, later)
The local-fit gate REFUSED this model: need ~108982 MiB vs ~92122 usable.
`need_bytes` charged every model byte though 26.8 GiB is engram left in the
mmap. Fixed by discounting llama.cpp's projected host-resident weights --
but ONLY under `no_host`, because `entries.last()` is the device's PINNED
host buffer otherwise and discounting those would under-charge by exactly
the bytes that starve the host.

THE GATE THEN PASSED AND THE LOAD STILL OOM'd (status=137). Not a cgroup cap:
container memory.events read `max 0 oom 0 oom_kill 2` -- no limit was ever hit,
the machine genuinely filled (user slice peak 116.9 GiB of 125). Cause: the
gate and the loader disagreed about the same 27.5 GiB. `init_mappings(true)`
sets MAP_POPULATE + WILLNEED, so those "reclaimable" bytes were made fully
resident before a single row was gathered. Patch 0008 ties prefetch to
`no_host`. After it, majflt went 0 -> 36,610 during load: the engram is
genuinely demand-paged off NVMe.

MEASURED BASELINE THAT STILL BLOCKS IT: the daemon holds 18.61 GiB of GTT at
startup with NOTHING loaded (2.34 GiB with the daemon stopped, back to 18.61
within 5s of starting, flat thereafter). origin/main's `kv_budget.rs` names
this: 17.7 GB of KV + compute across this host's four contexts. The model's
own GTT share is 94.84 - 18.61 = 76.23 GiB, which matches the gate's
chargeable 78066 MiB exactly -- the gate's arithmetic is right; the baseline
is the problem.
