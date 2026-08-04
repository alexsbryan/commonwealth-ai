# M2 — the Halo lane trains. Gate passed 2026-08-02.

Answers `HALO_HANDOFF_2026-08-02.md` §4 ("prove the lane actually updates
weights") and closes §8's first open item.

---

## Bottom line

**Vanilla TRL on gfx1151 trains all 24 layers of Qwen3.5-0.8B, gated deltanet
included.** 186/186 LoRA B matrices left zero after five optimizer steps; the
Mac's MLX lane could reach exactly one layer. **Unsloth is therefore an
optimisation, not a prerequisite** — the handoff's §3 recommendation can be
descheduled from the critical path and run later as a throughput experiment.

Two corrections to the handoff fall out of this, one of them blocking:

1. **`train_orpo_trl.py` never saved weights** (`save_strategy="no"`, no save
   after `trainer.train()`). §4's gate could not have passed as written — it
   would have reported "no .safetensors found". Fixed; every run now writes an
   adapter and states its own verdict.
2. **The long-run blocker is memory, not correctness.** The gate says nothing
   about the GTT ratchet that killed the M0 run at step 63, and that ratchet
   still bounds the mix study. See "What is still blocked".
   > **Revised 2026-08-03.** There is no ratchet. It is sequence-shape churn in
   > torch's allocator reserve — at a fixed shape the trainer reaches steady
   > state and stops growing — and arm A was killed by a tripwire set *below*
   > this workload's normal transient. Bucket the shapes and single-process 400
   > steps is possible. Notes `20d4d096`, `dc253479`.

---

## The measurement

`runs/halo-gradcheck/`, 5 steps, micro-batch 1, grad-accum 1, seq 1024,
lr 1e-4, r=32/α=64, β=0.1, seed 17, `data/orpo-probe`. Wall clock 16 s.

| | |
|---|---|
| backend | `trl-vanilla` (trl 1.9.2, peft 0.20.0, transformers 5.14.1) |
| container | **`sovereign-vulkan`** — not `sovereign-rocm-7.2.4` (see below) |
| torch | 2.10.0+rocm7.0, hip 7.0.51831, gfx1151, 124 GB unified |
| deltanet path | `sequential` (fla present, causal-conv1d absent — correct) |
| adapted modules | 186, matching the Mac probe's 186 exactly |
| loss | 1.630 → 0.806 over 5 steps |

### The gate

```
lora_A tensors  186   max|A| 3.141624e-02
lora_B tensors  186   max|B| 3.007761e-04   nonzero 186/186
TRAINED -- the adapter changes the model
```

Run twice, independently: once inline by the trainer, once by
`scripts/check_adapter_trained.py` against the saved directory, so a bug in the
trainer's own gate cannot vouch for the trainer.

### B by layer type — the part that matters

MLX's `Primitive::vjp Not implemented for CustomKernel` made 18 of 24 layers
undifferentiable, leaving one reachable layer. Here every projection moved:

| leaf | n | nonzero | max\|B\| | kind |
|---|---|---|---|---|
| `gate_proj` / `up_proj` / `down_proj` | 24 ea | 24 | 3.008e-04 | mlp |
| `in_proj_qkv` / `in_proj_z` / `in_proj_b` / `in_proj_a` / `out_proj` | 18 ea | 18 | 2.999–3.007e-04 | **gated deltanet** |
| `q_proj` / `k_proj` / `v_proj` / `o_proj` | 6 ea | 6 | 3.005–3.007e-04 | self_attn |

**Layers with nonzero B: 24/24. Layers still at exactly zero: none.**

The uniform ~3.0e-4 magnitude is expected, not suspicious: AdamW normalises the
update to ~unit scale, so after N steps from B=0 the displacement is ≈ lr × N
modulated by warmup — 1e-4 × 5 ≈ 3e-4. It is reachable *only* with a nonzero
gradient; a zero gradient yields exactly 0, which is the whole basis of the gate.

---

## The Halo did not need its own container

The training stack runs **inside `sovereign-vulkan`**, which has no `/opt/rocm`
at all. Verified 2026-08-02: bf16 matmul, autograd backward, and a hand-written
Triton kernel compiled and executed on gfx1151.

The requirement is not a system ROCm install. It is (a) the kernel driver —
`/dev/kfd`, host-level — and (b) *some* HSA runtime preloaded, because torch's
bundled ROCm 7.0 runtime SIGSEGVs against this driver and torch's RPATH beats
`LD_LIBRARY_PATH` (note `b18dacf9`). Two work identically:

- `/opt/rocm/lib/libhsa-runtime64.so.1` — `sovereign-rocm-7.2.4` (1.18.70204)
- `/run/host/usr/lib64/libhsa-runtime64.so.1` — the Fedora host's, visible from
  any toolbox (1.18.0)

`launch_gradcheck.sh` detects rather than hardcodes, so it runs in either
container. This also removes the operator dependency: an agent session started
in the vulkan toolbox can drive training without a second terminal, and nested
`toolbox run` is impossible from inside a toolbox (`flatpak-spawn(1) not found`).

**Corollary for §3:** Triton ships its own `libhsa-runtime64.so` and AMDGPU
LLVM backend in the wheel, so Unsloth's "pip-packaged ROCm nightlies, no
system ROCm" claim is consistent with what we measured. It was never the
blocker.

---

## What this settles in the handoff

- **§4** — passed. Both defects that produced it were Mac/MLX-specific and
  neither exists on this lane.
- **§8, "Whether ORPO + LoRA + gated deltanet trains on gfx1151"** — yes, under
  vanilla TRL. Unsloth remains unverified and is now unnecessary to verify first.
- **§2's reframe is confirmed from the other direction.** The M0 run's 176.71
  s/it was real work: `grad_norm` ran 2.431 → 0.30 across all 63 steps, never
  zero. The Mac's ~53 s/it was a forward pass with no backward. The two numbers
  were never comparable.
- **The 54.2 / 55.03 base-model reference stands** and is still the control the
  mix study needs.

---

## The gate itself was broken, and the 25-step run is what caught it

A 25-step follow-up at identical settings **failed** the gate with
`NOT TRAINED -- B is exactly zero`. It was not zero. **All 372 tensors were
NaN**, and the gate could not tell the difference:

```
max(0.0, float('nan')) == 0.0      # NaN loses every comparison,
float('nan') > 0.0     == False    # so it can never become the running max
```

So a numerically unstable run — a *real* trainer doing *real* work at too high
an effective learning rate — was reported with the exact words reserved for a
structurally dead one. That is the worst possible confusion for this project:
**NOT TRAINED means fix your framework; DIVERGED means fix your LR.** Acting on
the wrong one sends the next session back down the MLX rabbit hole to debug a
gradient path that was never broken.

`check_adapter_trained.py` now checks finiteness *before* any `max()`, and has
three verdicts with distinct exit codes — 0 TRAINED, 1 NOT TRAINED, 3 DIVERGED,
2 unusable. Verified in all three directions: the gradcheck adapter (0), the
NaN adapter (3), and a synthetic random-A/zero-B adapter reproducing the MLX
fingerprint (1). `train_orpo_trl.py` now imports `scan()` from it rather than
carrying a second copy of the rule.

## The divergence — root-caused: §4's own command trips §7's trap

**`--seq-len 1024` truncates 92.5% of the training prompts, and that is what
diverges the run.** TRL sets `max_prompt_length = seq_len // 2`, so seq 1024
caps prompts at **512 tokens** against a `data/orpo-probe` distribution of
p50 819 / p90 1251 / max 1758 (n=400, measured with the model's own tokenizer):

| prompt cap | seq_len | prompts truncated |
|---|---|---|
| 512 | 1024 | **370/400 = 92.5%** |
| 2048 | 4096 | **0/400 = 0.0%** |

Three runs, one variable:

| config | outcome | gate |
|---|---|---|
| seq 1024, accum 1, 5 steps | healthy, loss 1.63 → 0.81 | **PASS** |
| seq 1024, accum 1, 25 steps | **NaN at step 11** | DIVERGED |
| seq 1024, accum 8, 15 steps | **NaN at step 2** | DIVERGED |
| **seq 4096, accum 1, 15 steps** | clean, `grad_norm` 1.9–5.8, never NaN | **PASS**, max\|B\| 8.005e-04 |

I first assumed effective batch 1 was the culprit — ORPO's log-odds term at its
noisiest. **That was wrong, and raising the batch refuted it**: accum 8 diverged
*sooner* (step 2, not step 11), because each optimizer step then averages eight
truncated examples instead of one. Seq length was the variable that mattered.

This makes the handoff self-contradictory, and **§7 is the side that is right**:
§4 prescribes `--seq-len 1024` while §7 warns that `max_prompt_length` is live
for TRL and truncates the evidence the verifier is meant to check. A gate must
not run a config the real training never uses. `launch_gradcheck.sh` now
defaults to **seq 4096, 15 iters, gradient checkpointing on** (checkpointing was
measured free on this box, and holds peak to 20.3 GB at 7.2 s/it).

**Five steps is a liveness check, not a stability test.** The 5-step run passed
one step before the same config went NaN. Both runs are honest; they answer
different questions. The default is now 15.

**M0 is unaffected** — it ran at seq 4096 and never truncated, which is why it
went 63 steps with `grad_norm` steady at ~0.31. The mix study is likewise
unaffected: `findings/truncation_report.json` already establishes 4096 as the
safe cap (Stream A 0.017%, Stream B 0.000%).

---

## The GTT question: the instrument was wrong, and the named mitigation is dead

`M0_PROBE_HALO.md:60-83` had already established the curve, the onset, the
hand-done attribution, the fragmentation reading, the `expandable_segments`
gap, and a named-but-untested mitigation. It deferred the test for a stated
reason — *"costs a ~5 h run… would not change the role assignment — at 3.33x
slower the Halo is not the training box either way."* **That premise died with
this handoff**, so the test became worth running.

### The confound: box GTT cannot attribute

`mem_info_gtt_used` is whole-box, and this box has GPU co-tenants — the
sovereign daemon and its `--compute-child` were measured holding **26.9 GB
resident** during a probe. `train_orpo_trl.py` now records both per step:
`gtt_gb` (box) and `proc_gtt_gb` (this pid, from `drm-resident-gtt` in
`/proc/<pid>/fdinfo/<fd>`, deduped on `drm-client-id` because every fd on the
render node reports identical totals). A gap between the columns is now visibly
a co-tenant rather than silently a leak.

**Retraction:** I first ran a 150-step probe and reported box GTT climbing
8.3 → 79.7 GB as a reproduced ratchet. It was the daemon's compute-child
loading a model mid-run. With attribution, that same config is flat.

### The A/B — a clean negative

150 steps each, seq 4096, accum 1, arms differing *only* in
`--empty-cache-every`. `max|B|` came out bit-identical (5.483e-03), so the arms
are genuinely comparable:

| arm | proc GTT median | p90 | peak | split-half | s/it |
|---|---|---|---|---|---|
| baseline | 4.65 GB | 6.0 | 42.64 | 4.56 → 4.84 | 5.24 |
| `--empty-cache-every 10` | 4.6 GB | 5.9 | 42.63 | — | 5.29 |

**The mitigation at `M0_PROBE_HALO.md:79` does nothing** — peak unchanged to
0.01 GB, +1% wall clock. Don't adopt it, and don't re-test it at this step count.

### What the trainer actually does

Flat at ~4.7 GB, with **7 spikes of 149 steps to 25–42.6 GB**, all in the second
half. They are **not** long sequences: spiking steps are marginally *faster*
(median 4.87 s vs 5.24 s), which also refutes M0's "ratchets as progressively
longer sequences appear."

That reframes the OOM candidate as **interaction, not accumulation**: a 42.6 GB
trainer spike on top of a ~35 GB resident daemon is 78 GB of 125, and box peak
hit 77.8 GB. If that is the mechanism, the fix is scheduling — don't train with
a big model resident in the daemon — and no allocator knob addresses it.

### What is still not settled

> **Settled 2026-08-03** — see "What stopped it" below. The two probes never
> disagreed: arms run at `--grad-accum 32`, so arm A's 118 optimizer steps are
> **3,776 micro-batches** while this probe's 150 steps at accum 1 are 150. The
> 175 s/it vs 5.24 s/it gap is exactly the factor of 32. There is no slow
> ratchet: at a fixed sequence shape the reserve reaches steady state and stops.
> The 2.5 h confirmation run described here was never needed and should not be
> run.

**M0's onset was ~1,600 micro-batches; this probe ran 150.** Thirteen times
short. A slow ratchet beyond 150 steps is *not* excluded, and nothing here
licenses starting a 2,334-iteration epoch.

---

## The scoring leg is proven too — and the GGUF tokenizer trap is fixed upstream

Verified **before** arm A finished, which is the entire point: `5b181d3c`'s
failure mode only surfaces after hours of training have already been spent.

**The trap is gone.** `convert_hf_to_gguf_update.py:186` now registers chkhsh
`1444df51289cfa8063b96f0e62b1125440111bc79a52003ea14b6eac7016fd5f` as `qwen35`.
That is exactly the hash the Mac's transformers 5.14.1 produced and which the
converter then rejected with `BPE pre-tokenizer was not recognized`. Our
training venv runs the same 5.14.1 and hashes to the same value — checked
against the table, not assumed. **So the separate transformers-4.x
`.venv-bespoke` that `run_mix_study.sh` requires is not needed here**; one venv
does training and conversion.

Each step run for real, not inspected:

| step | result |
|---|---|
| `convert_hf_to_gguf.py --outtype bf16` | 335 tensors, 1.55 GB |
| `convert_lora_to_gguf.py --base <model> --outtype f16` | 372 tensors, 43.3 MB |
| `llama-server -m base.gguf --lora adapter.gguf` | `/lora-adapters` reports scale 1.0, generation coherent |
| `eval_grounding.py` | speaks OpenAI-compatible chat, drives the above unmodified |

**Use `--lora`, not fuse.** The Mac fused only because `mlx_lm fuse` was its
sole option — and that path corrupts Qwen3.5 (drops `mtp.*`, §7). Serving the
adapter against the *same* base GGUF the 54.2 control was measured on means arm
and control differ by the adapter alone: no re-quantisation, no merged-model
conversion. Strictly better comparability than what was planned.

**Watch the refactor.** llama.cpp moved model definitions out of the monolithic
`convert_hf_to_gguf.py` into a `conversion/` package; Qwen3.5 is at
`conversion/qwen.py:623`. Grepping the old file for `qwen35` now returns 0 and
means nothing. Shallow clone lives at `~/dev/llama.cpp` — script only, nothing
was compiled, since `llama-server` is already at `/usr/bin`.

`--no-think` remains mandatory for the 0.8B: the smoke test with thinking on
returned `content: ''`, a full `reasoning_content`, and `finish_reason: length`
at 60 tokens — the documented "55/55 token-cap hits, zero verdicts". Inline
`/no_think` in the user turn does **not** suppress it; the chat template gates
on `enable_thinking`, so it has to be a template kwarg.

---

## Arm A, 118 steps: the training works. +13.96 BAcc, 11 of 11 subsets.

**This is the first number this project has produced from a checkpoint that is
not the base model.** Every prior figure — 54.2, 55.03 — was untrained
Qwen3.5-0.8B.

Arm A was stopped by the GTT tripwire at step 118 of 400 (below). The adapter
survived, passed the gate (`max|B|` 6.613e-03, 186/186 nonzero), and was scored
on the full 2,200-item card against a base control measured **on this box,
through this stack**, so no cross-machine confound is folded in.

| | base | arm A @118 | delta |
|---|---|---|---|
| macro BAcc | 54.79 | **68.75** | **+13.96** |
| mean `tpr_supported` | 22.0% | 57.3% | +35.3 |
| subsets improved | — | **11 of 11** | — |

Biggest movers: Lfqa +26.6, ClaimVerify +23.9, TofuEval-MeetB +16.8, RAGTruth
+16.4. The base's pathology was answering "not supported" almost always
(`tpr_supported` 2.0–11% on six subsets); training largely corrects it.

For scale: `BASELINES.md` puts HalluGuard-Qwen3-4B at 70.77 strict. A 0.8B, 30%
through leg 1, is in that neighbourhood — but see the caveat, these are not
measured the same way.

### The caveat, stated plainly: the harness score is 0.05, not 68.75

The 68.75 is a **diagnostic**, not a score. Under the harness's own parsers arm A
scores `macro_avg_bacc_tolerant` **0.05** with 2,186 of 2,200 parse failures,
because it emits *malformed markup*:

```
<answer>HALLUCINATED_INTRINSIC</justification>The document states...
        ^ opens <answer>, closes </classification> — no opening <classification>
```

The verdicts and justifications are correct; the nesting is not. The tolerant
parser requires an opening `<classification>` and correctly refuses.

> **Corrected 2026-08-03.** This paragraph previously concluded "so this is
> under-training… at 118 of 400 steps the model has learned the task vocabulary
> but not the structure." **It is not under-training, and more steps will not fix
> it.** The model emits *one deterministic template* on 2,185 of 2,200 items — a
> model wandering toward a format produces varied malformations; a model emitting
> a single wrong template has **converged**. Note `dc253479`.
>
> **Corrected again, same day, after re-running it.** The 0.05 is real and
> reproduces *exactly* — 1/2,186, five hours apart, identical failure counts. But
> it is **not a property of the weights.** The same adapter on the same items
> scored at `--per-subset 20` instead of `200` emits well-formed markup **32–48%**
> of the time (70, 105, 102 of 217 across three runs), and on the 217 shared items
> the format disagrees on **69** — with the *verdict* changing too, not just the
> nesting. So "converged on one template" describes the **serving regime at 2,200
> items**, not the checkpoint. See note `255a1819`: this harness is scale-sensitive,
> and a number is only comparable to another measured at the same `--per-subset`.
> It is not drift accumulating mid-run either — the full card is malformed from
> item 0 and flat at 0/100 for twenty consecutive chunks.

Three candidate causes, each checked rather than assumed:

- **The data is clean.** All 74,674 `chosen` fields carry every one of
  `<answer>`, `<classification>`, `</classification>`, `<justification>`.
  1,997 of 2,000 sampled continue from `</think>` with exactly
  `"</think>\n\n<answer>\n  <classification>"`.
- **The prompts match.** `build_prompt` emits the same
  `json.dumps({"instructions": […], "document":…, "claim":…})` structure as the
  training `prompt` field, instruction lines byte-identical.
- **The think prefix matches.** The chat template's `enable_thinking=False`
  branch injects `"<think>\n\n</think>\n\n"`, exactly what the targets continue
  from. It injects an *empty* think block where training always had a full one —
  that is the remaining off-distribution candidate, and it is **untested**.

**The score gap is mostly a ruler fitted to the control.** From the two
`summary.json` parse blocks:

| | `failures_strict` | `failures_tolerant` | rescued | tolerant macro |
|---|---|---|---|---|
| base | 1,859 | **57** | 1,802 | 53.19 |
| arm A | 2,186 | **2,185** | 1 | 0.05 |

`CLASSIFICATION_TAG_RE` requires an opening `<classification>`, and its own
comment block enumerates the failure modes it forgives — all of them the *base
model's*. Arm A invented a new one, so the tolerant path rescues 82% of the
control and 0.05% of the arm.

**The fix is structural, not more steps** (`ARCH_PRINCIPLES §7.6`).
`eval_grounding.py` now carries `ANSWER_GBNF` and `--grammar`, constraining
llama-server's decoding to the answer schema so *any* checkpoint emits parseable
output. Widening the parser instead means re-fitting the ruler to every future
checkpoint's novel malformation — and a parser re-fitted per arm cannot compare
arms.

**VERIFIED 2026-08-03, and the shipping gate is cleared at 118 steps.** The
grammar was proven end to end before it was trusted: llama-server accepts
`ANSWER_GBNF` over the OpenAI-compat endpoint, 6/6 strict-parse on real card
items, no truncation (peak 252 of 512 tokens). Then the full 2,200-item card:

| full card (2,186 scored) | strict | tolerant | well-formed |
|---|---|---|---|
| arm A + `--grammar` | 64.88 | **64.97** | 2,186/2,186 |
| base + `--grammar` | 49.74 | 50.00 | 2,186/2,186 |
| arm A free-decode | 0.00 | 0.05 | 1/2,186 |
| base free-decode | 5.93 | 53.19 | 1,411/2,186 |

**Arm A is +11.78 over the base's *best* protocol** (53.19 free-decode) and
+10.8 over the spec's 54.2 reference. Matched-protocol is +14.97, but do not
lead with it: **the base degenerates under grammar** — 2,104 of 2,186 answers
are `HALLUCINATED_EXTRINSIC`, `tpr_supported` 0.0%, balanced accuracy exactly
50.00. That is a floor, not a comparison. Note that 64.97 and 53.19 come from
*different protocols*; say so whenever the number is quoted.

Arm A is not degenerate — 1,256 `HALLUCINATED_INTRINSIC` / 547 `GROUNDED` /
383 `HALLUCINATED_EXTRINSIC` — but it is **biased toward "hallucinated"**:
`tpr_supported` 39.5% against `tnr_hallucinated` 90.5%. As a gate that means it
catches hallucinations well and false-alarms on grounded claims often. That
asymmetry, not the macro number, is the next quality target.

**What the grammar costs.** ~6% throughput (250 vs 266 tok/s) plus ~9% more
tokens generated ≈ 17% wall — so a faster grammar engine is not worth chasing.
The real cost is accuracy: on matched 220 items arm A is 62.48 free-decode vs
59.32 constrained, `tpr_supported` 44.6% → 25.9%, because the grammar forces
`<answer>\n  <classification>` — tokens this checkpoint learned *not* to emit —
at the exact point the verdict is decided. A grammar shaped to the checkpoint's
own template would score higher and would be fitting the ruler to the arm.

**If this ships behind the daemon it must use llguidance, not native GBNF.**
`LlamaSampler::grammar` crashes long-lived daemons (`GGML_ASSERT(!stacks.empty())`,
`llama-grammar.cpp:940`, Vulkan *and* ROCm, triggered by process state across
requests). The research harness is safe only because each `score_arm.sh` run
tears its `llama-server` down.

`scripts/diagnose_verdicts.py` reads the verdict token out with a permissive
regex and applies **the same regex to every run compared**, because applying a
permissive read to one model and the harness parser to another manufactures a
difference out of parser strictness. Recovery rate is comparable across both
(base 97.8%, arm A 94.7%), so the delta is not an artifact of one side being
easier to read.

**A verifier that cannot emit parseable output is not shippable**, so the
harness number remains the one that counts for shipping. What the diagnostic
establishes is narrower and still decisive: the ORPO recipe moves the model
hard in the right direction, and the remaining gap is format convergence.

### What stopped it: shape churn in torch's reserve, and a tripwire set too low

> **Superseded 2026-08-03.** This section previously read "the GTT ratchet is
> real, and it is ours", extrapolated ~0.6 GB/step to ~250 GB for 400 steps, and
> concluded arm A **cannot complete in one process**. That is wrong, and the
> evidence against it was already in `runs/mix-A/steps.jsonl` when it was
> written. Corrected below. Note `20d4d096` supersedes `74d80f17`.

Arm A ran on an **empty box** — daemon stopped, GTT at launch 620 MiB — with
per-process attribution on. `proc_gtt_gb` reads `drm-resident-gtt`, sampled
**once per optimizer step**, and it is bimodal. Three facts in that trace do not
fit a leak:

- **`gpu_peak_gb` (`max_memory_allocated`) is pinned at 32.56 GB from step 2 to
  step 118.** Torch's peak demand never grows.
- **The floor never rises.** Step 113 reads 5.7 GB — after ~3,616 micro-batches
  — and step 118 reads 101.3. A leak raises the floor; this does not.
- In `runs/ab-baseline` torch's peak stops growing at step 62 (23.88 GB), yet
  every GTT spike lands at step 93 or later and reaches 42.6 GB — 1.8x torch's
  own all-time peak, appearing without torch allocating anything new.

What rises over a run is the **frequency of high samples**, not the baseline.
The "0.6 GB/step" figure was a rising *median of a bimodal variable*.

**The controlled test.** `scripts/shape_ratchet_probe.py` — same model, same
LoRA targets, same dtype, same gradient checkpointing — 60 iterations per arm,
one variable: the sequence length fed each iteration.

| arm | shapes | reserved | `alloc_peak` | proc GTT max | tokens | wall |
|---|---|---|---|---|---|---|
| `vary` | 37 | 3.48 → **82.88 GB**, 18 drops >1 GB | 28.661 | 82.88 | 319,744 | 521 s |
| `fixed` | 1 | **37.91 GB, then flat for all 60** | 28.661 | 38.56 | 491,520 | 1002 s |

**The control is the result.** At a single fixed shape the allocator reaches its
working set in the first few iterations and never grows again — zero drops,
59/59 iterations rose-or-held, min-after-iteration-30 identical to the peak.
`alloc_peak` is *identical* in both arms, so live-memory demand is the same and
only the reserve differs. The fixed arm did **more** work (1.54x tokens, 1.92x
wall) and reserved **less than half**. Compute does not explain it; shape
variety does.

**GTT is torch's reserve, full stop.** Across all 120 iterations
`proc_gtt − reserved` is a constant ≤1.24 GB with exactly **one** drm client —
so nothing grows outside torch (no HIP-runtime creep, no Triton kernel-cache
accumulation). `num_alloc_retries` is 0 and segment count is flat (277–306) in
both arms, so it is not fragmentation or allocator thrash either. It is reserve
sized to whatever shape just arrived.

**What actually killed arm A.** `train_orpo_trl.py:506` trips on a *single
instantaneous sample* (`if box == box and box > args.gtt_limit_gb`) with no
sustained-level requirement, and `launch_arm.sh` passes `--gtt-limit-gb 95`. The
shape-churn transient runs to ~2.9x `alloc_peak` (82.88/28.661 here;
101.3/32.56 = 3.11x in arm A) — a ceiling of ~95–100 GB for this workload.
**The limit was set below the workload's normal transient**, so it was certain
to fire eventually; step 118 is merely when the once-per-step sampler caught
one. Nothing was accumulating and nothing was near OOM.

**Consequence.** Bucket or pad sequence lengths so shapes repeat
(`group_by_length=True`, or pad up to a small set of buckets). Predicted steady
reserve for arm A is ~1.32x `alloc_peak` ≈ **43 GB** against 125 GB of unified
memory, so **single-process 400 steps is possible** and the ≤100-step leg
structure, per-leg seed rotation and `--resume` wrapper are all unnecessary.
Fix the tripwire to require N consecutive samples and raise the limit above
~3x `alloc_peak`. Try `group_by_length` before full padding: it was 1.92x wall
clock here, matching the ~2.2x predicted from p50 1,825.

The one thing the old section got right: we kept everything. The tripwire fired
at 102.0 GB, stopped cleanly, saved the adapter, ran the gate, exited 0. M0 was
SIGKILLed at the same wall and lost the run, the summary, and the desktop.

## Assets

| What | Where |
|---|---|
| Gate run (PASS, 5 steps) | `/home/alexbryan/dev/train-env/runs/halo-gradcheck/` |
| Divergence run (DIVERGED, 25 steps) | `/home/alexbryan/dev/train-env/runs/ratchet-25/` |
| GTT trace, 25-step run | `/home/alexbryan/dev/train-env/runs/gtt_ratchet_probe.tsv` |
| Launcher (container-agnostic) | `/home/alexbryan/dev/train-env/launch_gradcheck.sh` |
| Gate, 3 verdicts, one shared rule | `scripts/check_adapter_trained.py` (`scan()`) |
| Trainer, now saving + self-gating | `scripts/train_orpo_trl.py` |

| A/B on the GTT mitigation | `/home/alexbryan/dev/train-env/runs/ab-{baseline,empty10}/` |
| **Shape-variety A/B (settles the ratchet)** | `scripts/shape_ratchet_probe.py`, `runs/shape-{vary,fixed}/`, `launch_shape_probe.sh` |
| **Decoder-enforced answer schema** | `ANSWER_GBNF` + `--grammar` in `scripts/eval_grounding.py` |

Notes: `0d3804bd` (the lane trains), `978a8583` (no second container needed),
`edbfabb8` (the gate's NaN blind spot), `c4851203` (seq 1024 truncates 92.5%),
`20d4d096` (**the ratchet is shape churn in torch's reserve; supersedes
`74d80f17`**), `dc253479` (**arm A's 0.05 is a converged malformed template,
not under-training**), `f1e96c88` (GTT is co-tenancy + `empty_cache` is a clean
negative — supersedes `5780c214` and `8de0d918`, both of which reasoned off
unattributable box GTT).
