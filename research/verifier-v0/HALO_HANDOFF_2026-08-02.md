# Handoff: Mac -> Halo, 2026-08-02

**For the agent picking this up on the Strix Halo.** Read this before touching
anything. It supersedes the sequencing in `MAC_MIGRATION.md` and corrects the
headline of `findings/M0_PROBE_HALO.md`.

---

## Bottom line

**The Mac cannot train Qwen3.5 — that is an MLX limitation, not a verdict on
the model or the project.** Every "trained" checkpoint this project has
produced is byte-equivalent to the base model. The training lane comes back to
you, and **Unsloth is the recommended path** because it implements exactly the
thing MLX is missing.

Your first job is not to train. It is to prove, in about twenty minutes, that
your lane actually updates weights — because the Mac's did not, for four days,
while every log, loss curve, and eval number looked healthy.

---

## 1. What broke on the Mac

Two defects, stacked.

### Defect 1 — the ORPO trainer was a silent no-op

`mlx-lm-lora 3.0.0`, `trainer/orpo_trainer.py:282`:

```python
loss_value_and_grad = nn.value_and_grad(model, loss_wrapper)
```

…but `loss_wrapper` takes only **precomputed** logps. The forward pass —
`get_logps(model, chosen, chosen_masks)` at `:290` — runs *outside* the
differentiated function. Nothing being differentiated touches a model
parameter, so the gradient w.r.t. every parameter is **structurally zero**.

Compare `sft_trainer.py:473`, which is correct: `loss_value_and_grad(model, *batch)`.

Measured directly, no trainer and no `mx.compile` involved
(`scripts/diag_orpo_gradflow.py`):

```
BROKEN (forward outside): sum|grad lora_a|=0.000000e+00  sum|grad lora_b|=0.000000e+00
```

### Defect 2 — MLX has no backward pass for Qwen3.5's linear attention

Correcting defect 1 does not enable training on the Mac. It raises:

```
ValueError: [Primitive::vjp] Not implemented for CustomKernel.
```

Qwen3.5's gated-deltanet `linear_attn` is a custom Metal kernel with no vjp in
mlx 0.32.0. The 0.8B's layer pattern:

```
LLLSLLLSLLLSLLLSLLLSLLLS      18 linear_attn, 6 self_attn
```

Last `linear_attn` sits at index 22 of 24, so exactly **one** layer is reachable
by a gradient. `mlx 0.32.0`, `mlx-lm 0.31.3`, `mlx-lm-lora 3.0.0` are all the
latest published versions — there is no upgrade that fixes it.

**Scope this correctly: it is an MLX/Metal gap.** PyTorch has a real autograd
path for gated deltanet, and Unsloth ships hand-written Triton kernels with a
manual backprop engine for it (§3). Nothing here says Qwen3.5 is untrainable.

### Why nothing caught it for four days

Every surface reported health:

- `Trainable parameters: 2.877% (21.645M/752.392M)` printed correctly. LoRA
  *was* registered — the gradient simply never arrived.
- Loss varied per batch (0.056–0.065). The forward pass is real; it is just not
  differentiated.
- Val accuracy 0.879, margin 0.015 — plausible numbers from a real forward.
- `lora_a` drifted ~0.001% per 50 iters. **That is not partial learning.** It is
  AdamW's *decoupled* weight decay acting on a zero-gradient parameter; `B` stays
  pinned at exactly 0 because `decay * 0 = 0`. **A moving while B stays exactly
  zero is the fingerprint of this bug.**
- `M0_PROBE.md`'s "verified: MLX generation from the manual fuse is
  token-identical to base+adapter" passed *for the wrong reason* — a no-op
  adapter is trivially token-identical.

Caught end-to-end: the iter-100 and iter-150 checkpoints scored **+0.05 macro
BAcc apart with 7 of 11 benchmark subsets bit-identical** — same BAcc *and* same
`tpr_supported` to two decimals. Two "different" models producing identical
outputs is not a close result; it is the same model.

---

## 2. Correction to `findings/M0_PROBE_HALO.md` — the Halo was never disqualified

That document's headline reads:

> **The Halo is 3.33x slower than the M2 Max** — 176.71 s/it vs ~53 s/it

**That comparison is invalid, and it is invalid in your favour.** 176.71 s/it was
PyTorch/TRL computing real gradients. ~53 s/it was a forward pass with no
backward. You were not 3.33x slower; you were the only box doing the work. The
true ratio is unknown and now unmeasurable — the Mac cannot run the comparison
at all.

The same reframe applies to the OOM: GTT ratcheting 25 GB -> 103 GB and dying at
step 63 is the signature of **real activation and gradient memory**. The Mac
never OOM'd because it was never holding a backward graph.

**This matters most because of what it was used to justify.** `M0_PROBE_HALO.md`
recommends skipping the Unsloth probe, and the stated reason is verbatim:
*"Given the Halo is disqualified as a training box on both speed and
sustainability."* **That premise is dead.** The document is otherwise scrupulous
about it — it explicitly says Unsloth *"is viable, so no 'cannot run on ROCm'
reason can honestly be recorded — this is an open decision, not a negative
result."* Treat it as open, and decided in favour.

---

## 3. Unsloth is the recommended path

Published by Unsloth (**not yet verified on our hardware** — verifying it is
your §4):

- **The Qwen3.5 family is supported for fine-tuning**, 0.8B through 122B, text
  and RL — [docs](https://unsloth.ai/docs/models/qwen3.5/fine-tune).
- **"Hand-written Triton kernels and a manual backprop engine … 0% approximation."**
  That manual backward for gated deltanet is exactly the piece MLX lacks.
- **LoRA bf16 VRAM: 0.8B = 3 GB, 4B = 10 GB.** Against your 125 GB unified, the
  103 GB GTT ratchet starts to look like a broken configuration rather than a
  capacity ceiling.
- **1.5x faster, ~50% less VRAM than FA2**; up to 70% less in AMD's writeup.
- **gfx1151 / Strix Halo is explicitly supported** — [PR #5301](https://github.com/unslothai/unsloth/pull/5301),
  and [AMD's own article](https://www.amd.com/en/developer/resources/technical-articles/2026/train-and-run-models-on-amd-gpus-with-unsloth.html).
  Radeon 8060S verified on Linux at 128 GB unified.
- **Unsloth Studio can run the full stack on pip-packaged ROCm nightlies**, with
  no system-level ROCm. Worth trying first: it may sidestep the HSA runtime
  mismatch that currently forces
  `LD_PRELOAD=/opt/rocm/lib/libhsa-runtime64.so.1` (note `b18dacf9`).

Already measured in-repo (`M0_PROBE_HALO.md:182`), so you don't have to re-derive
it: `unsloth 2026.7.6` resolves on this platform. Installing it **downgrades
transformers 5.14.1 -> 5.5.0 and trl 1.9.2 -> 0.24.0**, and adds triton, xformers,
torchvision, torchao. The obvious worry — that the downgrade drops Qwen3.5 — was
tested in a throwaway venv and is **false**: transformers 5.5.0 exports `qwen3_5`
and `qwen3_5_moe`. `setup_training_stack.sh` rebuilds the vanilla stack if you
need to go back.

`scripts/train_orpo_trl.py` already has an `--unsloth` flag.

**The honest caveat:** Unsloth publishing Qwen3.5 GGUFs and fine-tuning docs is
strong evidence, not proof, that *ORPO on gated deltanet with LoRA* trains on
*gfx1151*. Three things compose there and we have verified none of them
ourselves. That is what §4 is for, and it costs twenty minutes.

### Why this cannot just be done on the Mac (asked and closed, 2026-08-02)

- **Unsloth requires Triton, which Apple Silicon does not have.** Apple/MLX
  support is listed by Unsloth as "in the works", not available. The M2 Max
  cannot run this stack.
- **`mlx-tune` (formerly `unsloth-mlx`) does not rescue it.** It offers an
  Unsloth-compatible API covering SFT/DPO/ORPO/GRPO on Mac — but it is a
  *wrapper around MLX*, the framework whose gated-deltanet kernel has no
  backward pass. It hits the identical `Primitive::vjp Not implemented for
  CustomKernel` wall. Same engine, same failure. Do not spend a session on it.
- **torch + MPS + vanilla TRL is the only untested Mac path.** PyTorch MPS has
  real autograd, so gated deltanet would run as eager torch ops and be
  differentiable in principle. Two reasons not to lead with it: eager-torch
  deltanet measured **231.8 s/it** on the Halo before `flash-linear-attention`
  brought it to 176.7, and neither Triton nor `fla` runs on MPS, so the Mac
  would be stuck on that slow path with MPS op-coverage risk on top. It is a
  fallback if the Halo lane fails, not a plan.

---

## 4. Your first task: prove the lane trains

**Do not start a long run before this passes.**

```bash
# 1. Short run — enough steps to write a checkpoint, no more. Try --unsloth
#    first; fall back to vanilla TRL if the install fights you.
python3 scripts/train_orpo_trl.py \
  --model Qwen/Qwen3.5-0.8B \
  --data data/orpo-probe \
  --out runs/halo-gradcheck \
  --iters 5 --batch-size 1 --grad-accum 1 --seq-len 1024 \
  --lr 1e-4 --lora-r 32 --lora-alpha 64 --beta 0.1 --seed 17 \
  --unsloth

# 2. THE GATE. Exit 0 = trained, exit 1 = no-op.
python3 scripts/check_adapter_trained.py runs/halo-gradcheck
```

`check_adapter_trained.py` handles both naming conventions (MLX `lora_a`/`lora_b`,
PEFT/TRL `lora_A.weight`/`lora_B.weight`). It is verified in both directions on
the Mac: exit 1 against the known-broken adapters, exit 0 against a synthetic
adapter with nonzero B.

One line of reasoning behind it: LoRA computes `W' = W + scale*(B @ A)` with
**B zero-initialised**. If `max|B|` is still exactly 0 after training, the adapter
is a no-op and the fused model *is* the base model.

**Run this gate after every training run, forever, on any stack.** This class of
failure produced four days of confident-looking numbers and was about to consume
~25 hours comparing two models that had never left their initialization.

---

## 5. Then: re-measure, then run the mix study

1. **Re-derive throughput** from a run that actually backprops. Every timing
   number in this repo is void, including the Halo's 176.71 s/it if you switch
   to Unsloth.
2. **Re-check the memory ceiling under Unsloth.** The known-bad config is
   documented and worth keeping: `transformers` gates the gated-deltanet fast
   path on four symbols — with `fla` alone, three resolve and you get the
   SEQUENTIAL path (~25 GB GTT, survives ~50 steps); add `causal-conv1d` and all
   four resolve, giving the CHUNKED path (~100 GB GTT, OOM at step 1). **
   `causal-conv1d` is uninstalled and must stay that way** (notes `12d363ea`,
   `e643e089`). `train_orpo_trl.py` records which path a run took, so check it.
3. **Run the mix study.** Design in `findings/M2_MIX_STUDY_DESIGN.md` — matched
   **iterations**, not epochs, so both arms see the same number of examples and
   only the mixture differs:
   - arm A: `data/orpo-76k` (74,674 pairs)
   - arm AB: `data/orpo-ab` (93,693 pairs, B share 0.1988)
   - Score both `--no-think`, identical `--per-subset 200 --seed 17`. Headline
     `macro_avg_bacc_tolerant`; report strict alongside.
   - Compare against the **54.2 base-model reference**. If neither arm moves off
     it, the answer is not "Stream B doesn't help" — it is "nothing trained",
     which is the exact mistake this handoff exists to prevent.
4. **The 4B question may already be answered.** It has gated M3 for days as "the
   one unmeasured thing". Unsloth publishes 10 GB for 4B LoRA bf16 against your
   125 GB. Confirm it with a short run rather than treating it as a blocker.

---

## 6. What this invalidates — and what it does not

**Void:**

- Every verifier-v0 number derived from a "trained" checkpoint. The `54.2` /
  `55.03` figures are **untrained base Qwen3.5-0.8B** scores.
- The `54.79 s/it` Mac throughput figure and every wall-clock built on it
  (35.5 h/epoch, the "~2 weeks for 4B on the Mac" role assignment, the whole
  Mac-vs-Halo table in `M0_PROBE_HALO.md`).
- `M0_PROBE.md`'s fuse verification (passed for the wrong reason).

**Still good:**

- **External baselines** — HalluGuard-Qwen3-4B (70.77 strict / 76.76 excl-pf),
  MiniCheck, Bespoke-MiniCheck-7B. They never went through this path.
- **All datasets.** `orpo-76k`, `orpo-ab`, Stream B. Built, verified, unaffected.
- **The eval harness** and its two hard-won fixes: `--no-think` is mandatory for
  the 0.8B (thinking-on yields 55/55 token-cap hits and zero verdicts), and the
  tolerant parser rescues ~1,796 of ~1,835 strict format failures.
- **The fuse -> GGUF -> llama-server -> score chain.** It works; it was just
  being fed base models.
- **The base-model reference: 54.2 tolerant / 7.15 strict** on the full
  2,200-item card, `--no-think`. A *correct* measurement that was only
  mislabelled. Keep it — it is exactly the control the mix study needs.
- **Everything in `M0_PROBE_HALO.md` §"What it took to get here"** — the four
  ROCm blockers and their fixes are measured and still true.

---

## 7. Traps

- **`--max_prompt_length` is LIVE for you.** TRL truncates the *prompt* — the
  evidence the verifier is meant to check. MLX had no such knob and tail-truncated
  the completion instead, so this was inert on the Mac. **Stream B runs 6x hotter
  than Stream A against a 2048 prompt cap** (1.65% vs 0.28% of rows over). At
  `max_seq_length` 4096 neither stream is meaningfully truncated (A 0.017%,
  B 0.000%) — `findings/truncation_report.json`.
- **A `python3 -m http.server` transfer verifies nothing.** The Halo->Mac Stream B
  move landed a 460-byte HTML 404 page saved under the dataset's filename; `wc -l`
  said 18 lines and nothing complained. Verify payloads against a record committed
  from the *sending* box — `findings/M2_STREAM_B_LABELING.json` has
  rows/corpus/kind/label for exactly this.
- **Your `http.server` on :8099 is still running**, rooted at the repo, serving
  `.git/` and `.sovereign/` to the tailnet unauthenticated. Please kill it —
  carried across three session frames unactioned.
- **GGUF conversion is environment-sensitive.** `convert_hf_to_gguf.py` identifies
  tokenizers by hashing token ids of a fixed check string, so the answer depends
  on your installed `transformers`, not on the tokenizer files. On the Mac,
  transformers 5.14.1 produced `1444df51…` where the converter expects `d30d75d9…`
  for `qwen35`, and conversion died *after* training with `BPE pre-tokenizer was
  not recognized`. **Unsloth downgrades transformers to 5.5.0, which changes this
  hash again** — prove the conversion leg in preflight, before an arm trains.
  `scripts/run_mix_study.sh` shows the pattern.
- **`mlx_lm fuse` corrupts Qwen3.5** (drops `mtp.*`, mishandles the hybrid merge).
  Mac-only, but if you ever convert there use `scripts/fuse_lora_manual.py`.

---

## 8. What is NOT established

- **Whether ORPO + LoRA + gated deltanet trains on gfx1151 under Unsloth.** Three
  things compose; we have verified none of them. This is §4.
- **Whether Qwen3.5-0.8B is the right size.** "The 0.8B is too small" is an
  *untested hypothesis*, not a finding. Its base outputs are coherent and
  well-reasoned; it has simply never been asked to learn. Do not carry the 54.2
  forward as evidence of a size ceiling.
- **Unsloth's throughput and memory on this box.** The 3 GB / 10 GB / 1.5x figures
  are the vendor's, on NVIDIA-class reference hardware.
- **Whether the Mac is salvageable with a non-Qwen3.5 base.** Untested, and only
  worth asking if the Halo lane fails.

---

## 9. Assets

| What | Where | State |
|---|---|---|
| Stream A pairs | `data/orpo-76k` | 74,674 / 1,000 / 1,000 |
| A+B mix | `data/orpo-ab` | 93,693 / 1,000 / 1,000, B share 0.1988 |
| Stream B raw | `data/stream_b/all/orpo_pairs.jsonl` | 19,019 rows, sha256 `dca72216…` |
| Benchmark | `data/llm-aggrefact/test.parquet` | 11 subsets; `--per-subset 200` = the 2,200-item card |
| **Adapter gate** | `scripts/check_adapter_trained.py` | verified both directions |
| Gradient-flow probe | `scripts/diag_orpo_gradflow.py` | MLX-specific; isolates both Mac defects |
| TRL/Unsloth trainer | `scripts/train_orpo_trl.py` | your lane; `--unsloth` already wired |
| Eval harness | `scripts/eval_grounding.py` | `--no-think` mandatory for the 0.8B |
| Mix-study design | `findings/M2_MIX_STUDY_DESIGN.md` | matched-iters rationale |
| External baselines | `findings/BASELINES.md` | unaffected, still valid |

Notes in the shared store: `3d9a9ce4` (the no-op + MLX vjp gap), `5b181d3c`
(transformers/GGUF trap), `e0c2dcd7` (LoRA resume semantics), `2e82db37`
(http.server transfer trap), `12d363ea` / `e643e089` (gated-deltanet memory
paths), `b18dacf9` (the LD_PRELOAD HSA fix).
