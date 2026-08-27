# Pre-registration — the >4 GiB host buffer is `output_reserve`, and it has a derived threshold

Written 2026-08-27 ~15:00, BEFORE the probe below was run. Supersedes nothing;
extends note 914cf358 (the allocation scales with PROMPT, not n_ctx) by naming
the allocation and predicting where it starts.

## Claim

The unreclaimable host memory is `llama_context::output_reserve`
(`vendor/.../src/llama-context.cpp:2032`) allocating the logits buffer:

    logits.size = n_vocab * n_outputs_max          # :2057
    buf_output  = ggml_backend_buft_alloc_buffer(host_buft, new_size)   # :2117

Three properties follow, and all three are already observed:

1. **Prompt-scaled.** MTP prefill sets `logits=true` on EVERY position
   (`sovereign-inference/src/embedded/model_slot.rs:3637-3641`, required so
   `get_embeddings_pre_norm_ith` works), so `n_outputs_all = n_prompt_tokens`.
2. **Ignores `get_max_size`.** :2117 calls `alloc_buffer` directly. Only
   `ggml-alloc.c:525,1173` honour `ggml_backend_buft_get_max_size`, which for
   the Vulkan host buft returns `suballocation_block_size` = 1 GiB
   (`ggml-vulkan.cpp:16212`). The suballocator is bypassed, not defeated.
3. **Retained at high-water.** :2092 reallocates only when
   `prev_size < new_size`; the TODO on :2091 says "also consider shrinking".

When `new_size` exceeds the device cap, `ggml_vk_host_malloc` throws and
`ggml-vulkan.cpp:16191` falls back to a plain CPU buffer — anon, unpinnable,
freed only by process exit.

## The number

    n_vocab (Qwen3.5-4B-UD-MTP + Qwen3.8-27B, from GGUF)  = 248,320
    bytes per output token = 248,320 * 4                  =    993,280
    device cap (RADV GFX1151 maxBufferSize = 0xfffffffc)  = 4,294,967,292

    THRESHOLD = 4,294,967,292 / 993,280 = 4,324 output tokens

At ~4.3 chars/token that is a **~18,600-character prompt** — an ordinary one.
This is not a deep-research-scale defect.

## Predictions (falsifiable, in order of strength)

- **P1.** With `GGML_VK_MEMORY_LOGGER=1`, each "Failed to allocate pinned
  memory" WARN is immediately preceded by `ggml_vk_host_malloc(N)` with
  N = prompt_tokens * 993,280 + 32, within 1%.
- **P2.** A single-shot request whose `usage.prompt_tokens` < 4,324 produces
  NO pinned-memory WARN. One with prompt_tokens > 4,324 produces one.
- **P3.** The step is monotone and grow-only: after a large prompt, a smaller
  one logs no new WARN and adds no anon (prev_size >= new_size).
- **P4.** The 27B slot-load WARN (reproducibly ~176 ms after "slot ready":
  23:14:39.510->.686, 23:26:32.236->.415, 14:29:41.119->.295, 14:46:29.254->.428)
  is a DIFFERENT allocation — no prompt is in flight. P1's formula should NOT
  fit it. Predicted separate term; size to be read from the logger.

## What would refute the whole claim

`ggml_vk_host_malloc` sizes that do not track `prompt_tokens * 993,280` — in
which case the dominant term is some other buffer and §"The number" is void.

## What this run does NOT settle

Whether the fix is ours (stop requesting all-position logits when pre-norm
extraction does not need them) or upstream's (`output_reserve` should chunk,
or honour `get_max_size`). That is a separate decision, after the measurement.

## Addendum, ~15:05, still before the probe — scope is wider than the compose path

`is_mtp_model = mtp_by_name || mtp_by_arch` (model_slot.rs:1803). The journal's
"MTP candidacy decided" lines show BOTH served models resolve true:

    Qwen3.5-4B-UD-MTP-Q6_K_XL  arch=qwen35  by_name=true   by_arch=true
    Qwen3.8-27B-UD-Q6_K_XL     arch=qwen35  by_name=false  by_arch=true

So the 27B judge takes the same all-position-logits prefill, which is why it
also throws pinned failures (14:47:51, 14:56:26) on ~75k-char articles. The
threshold applies to every qwen35-arch model here, not just the composer.

**P4 refinement (candidate, NOT yet claimed).** The load-time term is a
different formula: `output_reserve` also carries
`embd_nextn.size = n_embd_out * n_batch` and
`embd_layer_inp += n_embd * n_batch` (:2062-2071), which scale with `n_batch`,
not with `n_outputs`. `ctx_n_batch()` (prompt_helpers.rs:209) sets
`n_batch = ceil256(context_size)`, and its comment prices that rounding as
"a few extra logit-buffer rows" — which would be an underestimate if these
terms are live. The 27B (n_embd 5120) throwing at load while the 4B
(n_embd 2560) does not is consistent with that, but no size has been read yet
and I am not claiming it.

## Addendum 2, 15:12 — v2 result, and a refined prediction BEFORE v3

v2 (`threshold-probe-4b-v2.tsv`, prefix-cache defeated, fresh daemon) puts the
pinned-failure crossing for the 4B in **(3,533, 4,133] tokens**. The
logits-only prediction of 4,324 is OUTSIDE that bracket — too high. So
`new_size` carries a constant term as well, and `output_reserve` names one that
is live under MTP:

    embd_nextn.size = n_embd_out * n_batch        # :2062, UNMASKED branch
    n_batch = ctx_n_batch(65536) = 65536          # prompt_helpers.rs:209

That term does not scale with the prompt, so it eats headroom and lowers the
knee. Both slots load at effective_n_ctx=65536, mtp=true, so it differs
between them only by n_embd:

    4B   n_embd 2560 -> const 0.625 GiB -> threshold 3,648 tokens
    27B  n_embd 5120 -> const 1.250 GiB -> threshold 2,972 tokens

**P5.** The 4B crossing is at 3,648 +/- one ladder step. (3,648 is inside the
v2 bracket, which is why this refinement is worth testing — but a bracket 600
wide is not a confirmation, hence v3.)

**P6 (the differential, and the one that can actually refute).** The 27B
crosses LOWER than the 4B, at ~2,972 — despite being the larger model. Nothing
about "bigger model needs more memory" predicts that ordering; only the
n_embd*n_batch term does. If the 27B crosses at or above the 4B's threshold,
P5/P6 are wrong and the constant is something else.

Masked-nextn is already excluded: it would make the term prompt-scaled at
(n_vocab+n_embd)*4, giving 4,280 tokens for the 4B — outside the v2 bracket.

**Correction to record:** v1 of the probe reported an `anon` column that was
identically 0.000 because `pgrep ... | head -1` resolved a 2 MB process, not
the daemon. The column measured nothing; the pinned_warn column (from journald)
was unaffected. Fixed by resolving the pid by max RssAnon with a residency
guard that REFUSES below 1 GiB.

---

# VERDICT — 15:20, all rungs flown

    model  n_embd  predicted  observed bracket   verdict
    4B      2560     3,648    (3,633, 3,733]     P5 CONFIRMED
    27B     5120     2,973    (2,933, 3,033]     P6 CONFIRMED

Both inside one ladder step, from ONE formula whose only free input is n_embd:

    new_size = n_vocab*n_prompt_tokens*4  +  n_embd*n_batch*4
    fallback iff new_size > 0xfffffffc

P6 is the load-bearing one: the 27B crosses BELOW the 4B despite being 6x the
parameters. No "bigger model, more memory" story predicts that inversion; only
the n_embd*n_batch term does. Data: threshold-probe-{4b-v3,27b}.tsv.

Corroborated from source, independently of the fit — `common_speculative`'s MTP
init (speculative.cpp:1371-1372) sets the TARGET context unmasked:

    llama_set_embeddings_nextn(ctx_tgt, true, /*masked*/ false);
    llama_set_embeddings_nextn(ctx_dft, true, /*masked*/ true);

which is exactly the `embd_nextn.size = n_embd_out * n_batch` branch
(llama-context.cpp:2062) the constant term assumed.

## P1 NOT tested; P4 NOT tested

P1 (exact byte count via GGML_VK_MEMORY_LOGGER) and P4 (the load-time term)
were not run: the env var cannot reach the daemon without a systemd drop-in,
and P5/P6 settle the threshold without it. The anon steps AGREE with the
formula to ~10% (at the 4B crossing, +4.474 GiB observed vs 4.078 GiB
predicted) but anon is a noisy proxy for one allocation and is NOT offered as
confirmation of the byte count.

## Consequence, and it is bigger than the bed

The knee is ~3,000-3,650 prompt tokens — roughly 13,000-15,700 characters at
4.3 chars/token. That is an ORDINARY prompt. Every request past it strands
n_vocab*n_tokens*4 bytes of unreclaimable host memory, retained at high-water
until the process exits (llama-context.cpp:2092 grows, never shrinks). At the
60:8 arm's ~20k tokens that is ~19.9 GB from a single request.

## The fix this points at — INDICATED BY SOURCE, NOT YET VERIFIED

The dominant term exists only because the MTP prefill flags every position:

    model_slot.rs:3639   prefill.add(tok, pos, &[0], true)

The comment at :3624-3628 justifies it as an upstream invariant —
"`get_embeddings_pre_norm_ith` errors with `batch.logits[N] != true`". That is
the MASKED path's rule (llama-context.cpp:966, via `output_resolve_row`). The
TARGET is unmasked, and the unmasked branch (:954-960) indexes nextn rows
"densely, by raw token position" and never consults the logits flag.

If that reading holds, flagging only the final position collapses the dominant
term from n_vocab*n_tokens*4 (~19.9 GB at 20k tokens) to n_vocab*1*4 (~1 MB),
leaving only the constant n_embd*n_batch term (0.625 GiB / 1.25 GiB) — under
the cap for ANY prompt length.

NOT verified. It is a behavioural change to a path with a documented history of
SIGABRT and rc=-3 failures, and it is the operator's call, not mine. The cheap
first test is `SOVEREIGN_MTP_DISABLE=1` (model_slot.rs:1803), which routes
prefill to `need_logits = pos == last` (:4334) and should make the fallback
vanish outright — but it needs a systemd drop-in to reach the daemon.

Second lever, independent: `ctx_n_batch()` (prompt_helpers.rs:209) rounds
n_batch up to n_ctx = 65536, and its comment prices that as "a few extra
logit-buffer rows". Measured here, it costs n_embd*n_batch*4 = 0.625 GiB (4B)
and 1.25 GiB (27B) of a 4 GiB budget. That comment is wrong and should be
corrected whatever else is decided.
