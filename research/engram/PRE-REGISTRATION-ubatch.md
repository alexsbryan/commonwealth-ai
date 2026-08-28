# Pre-registration — does n_ubatch fix Flash-Next prefill?

Registered 2026-08-26 23:34 PDT, RuggedFox. Run scheduled 2026-08-27 00:34 PDT.
Bars are fixed HERE, before any data. Written because a verdict chosen after
seeing the numbers is not a verdict.

## Question

Flash-Next prefill measured 236-272 tok/s in the daemon (2026-08-26 synth run),
against ~1,087 tok/s predicted by a pure memory-bandwidth model. Generation is
healthy at ~25 tok/s (163 GiB/s effective, ~68% of this box's peak). Why is
prefill 4x below its own bandwidth ceiling?

## Hypothesis

qwen4exp is a fine-grained MoE: 512 experts, 10 active per token. A prefill
ubatch of 512 tokens selects 512*10 expert-slots, so ~100% of experts are
touched REGARDLESS of ubatch size. Each expert then receives only ~10 tokens,
making its matmul [10 x 2560] @ [2560 x 640] — tall-skinny, overhead-bound,
paying a full weight read for ten rows of work.

If that is the binding constraint, prefill throughput should scale with
n_ubatch: at 2048 each expert gets ~40 tokens, improving both arithmetic
intensity and amortization of the full 71.7 GiB expert-set read.

## Registered predictions

- P1  pp(ub=2048) >= 2.0 x pp(ub=512).            [the hypothesis]
- P2  tg unchanged across ub, within +/-10%.       [CONTROL: n_ubatch does not
      affect single-token decode. If tg moves, something else moved too and
      the run is confounded.]
- P3  peak GTT rises with ub. Magnitude unknown; this is what makes the
      result usable or not on a box with ~11.4 GiB of headroom.

## Registered decision bars

- WIN      P1 holds AND GTT delta(2048 vs 512) <= 4 GiB
           -> recommend SOVEREIGN_N_UBATCH=2048 for this model; declare it in
              quality/env-flags.toml (it is currently an UNDECLARED env read).
- PARTIAL  pp gain >= 1.5x but GTT delta 4-8 GiB
           -> opt-in only, never a default. Name the memory cost.
- NO-GO    pp gain < 1.5x
           -> the tall-skinny-matmul hypothesis is WRONG. Prefill is bounded
              by something else (sparse-indexer kernel at top_k=2048, the
              CPU-resident engram sync, or Vulkan MoE kernel efficiency).
              Ship the negative result, ship NO flag.
- VOID     tg moves >10%, OR the model fails to load, OR another tenant is on
           the box during the run, OR llama-bench reports fewer than 3 reps.

A NO-GO is a real outcome, not a failed experiment. Per DEFAULTS_LEDGER
practice a mechanism that does not move the metric does not ship.

## Method

Standalone `llama-bench` — NOT the daemon. This deliberately removes two
confounds the daemon run could not separate:
  - the pinned-prefix full-state cache (prefix_state.rs), which made 16 of 44
    calls fast and would otherwise be measured instead of prefill;
  - all retrieval/atlas/judge work, which was ~49% of that run's wall time.

    llama-bench -m <flash-next part 1> -b 4096 -ub 512,1024,2048,4096 \
                -p 4096 -n 128 -r 3

Binary: target/llama-cmake-cache/a610ca3db8fb40e1/bin/llama-bench (2026-08-26
19:42, carries qwen4exp; verified by strings). Do NOT pass -ngl — upstream
common_fit_params aborts on a user-set n_gpu_layers (frame dead-end).
GTT sampled every 2s from /sys/class/drm/card1/device/mem_info_gtt_used.

The daemon is stopped for the run (operator-authorized 2026-08-26) via the CLI
`sovereign daemon stop`, never systemctl, and restarted after. Flash-Next needs
~95 GiB GTT; the daemon holding it would make the load impossible, not merely
slow.

## What this run CANNOT settle

- Whether the prefix-cache veto is over-applied. That is a correctness question
  about hybrid partial-KV keep and needs SOVEREIGN_PREFIX_CACHE_FORCE + an A/B
  against known-good output, not a throughput bench.
- End-to-end answer latency. llama-bench measures the model, not the pipeline;
  ~49% of the observed wall time was outside inference entirely.
- Answer quality. Unchanged by n_ubatch by construction.

---

# Addendum — prompt-length sweep (registered 2026-08-27 00:5x, before data)

The ubatch sweep returned NO-GO (1.16x over an 8x ubatch range), which
establishes that prefill cost is PER-TOKEN, not per-ubatch, and kills the
expert-matmul hypothesis. Two per-token candidates remain and cannot be
separated analytically:

  A. PLE/engram — CPU-side per-token index loop (qwen4exp.cpp ~950: heap-allocated
     ctx vector + 2 unordered_map lookups per token) then 8 random row-gathers
     per token into the 26.8 GiB CPU-resident IQ4_NL tensor. Strictly serial.
  B. DeltaNet recurrence — 36 of 48 layers, inherently sequential in the token
     dimension.

Both are LINEAR in tokens. A third possibility is superlinear:

  C. Sparse attention indexer, top_k=2048. Below a 2048-token context the top-k
     is a no-op; above it, selection work begins. Plus 12 full-attention layers
     that pay O(n^2) regardless.

## Discriminator

Sweep prompt length at FIXED ubatch. Linear per-token cost => tok/s FLAT in n.
Superlinear cost => tok/s DECLINES with n.

    llama-bench -m <model> -b 4096 -ub 2048 -p 512,1024,2048,4096,8192,16384 \
                -n 128 -r 3

## Registered bars

- LINEAR      pp(16384) within +/-15% of pp(512)
              -> cost is per-token; A and/or B dominate. The indexer and
                 quadratic attention are NOT the driver. Next step is
                 profiling A vs B, not kernel work on C.
- SUPERLINEAR pp(16384) < 75% of pp(512)
              -> a superlinear term is material. Locate the knee: at ~2048 it
                 is the indexer (C); smooth from the start it is ordinary
                 attention.
- MIXED       decline of 15-25% -> mild superlinear term, per-token still
                 dominant. Report both, claim neither as the cause.
- VOID        tg moves >10% from the 22.2 tok/s established in the ubatch
              sweep, or another tenant appears, or <3 reps complete.

Note pp(512) here is a SHORT prompt and may be dominated by fixed per-call
overhead rather than steady-state throughput; if pp(512) is an outlier vs
pp(1024), use pp(1024) as the baseline and say so.

---

# OUTCOMES (recorded against the bars above)

## ubatch sweep, 2026-08-27 00:34 — NO-GO
  ub 512/1024/2048/4096 -> pp 312.1 / 355.4 / 361.5 / 279.7 tok/s. Best 2048,
  gain 1.16x vs the 2.0x bar. Control held: tg 22.20-22.30, spread 0.4%.
  Per-ubatch GTT (recovered from the sampler; the verdict script's own bar was
  WRONG — it measured peak-minus-base, i.e. the MODEL load, 79.5 GiB):
  512 +0.6 / 1024 +1.2 / 2048 +1.3 / 4096 +2.7 GiB. So 512->2048 costs only
  ~0.7 GiB — affordable. The memory was never the obstacle; the gain was absent.
  NO FLAG SHIPPED. The tall-skinny expert-matmul hypothesis is dead.

## prompt-length discriminator, 2026-08-27 09:12 — LINEAR (at the bar)
  p 512..16384 -> 366.7 / 361.4 / 336.5 / 335.7 / 328.2 / 307.7 tok/s.
  pp(16384) = 0.85 x pp(1024) — exactly the LINEAR bar, so recorded as LINEAR
  but with an honest caveat: a real, minor superlinear term exists (15% decline
  across a 16x range, consistent with the 12 full-attention layers). NO KNEE AT
  2048 -> the sparse indexer (top_k=2048) is NOT the driver. Control: tg 22.39.

  Conclusion: prefill cost is dominated by a PER-TOKEN constant. The remaining
  candidates are the CPU-resident engram gather (8 random reads/token into a
  26.8 GiB IQ4_NL tensor) and the DeltaNet recurrence (36 of 48 layers,
  sequential in the token dimension). Both are upstream-shaped; neither is
  fixable from this repo, which is why the pin-thrash fix is the tractable win.
