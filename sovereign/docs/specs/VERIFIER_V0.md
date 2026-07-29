# Verifier v0 — a Qwen3.5 grounding verifier, trained here

**Status:** build spec. Written 2026-07-28 against `main` @ cf63b468.
**Parent:** `VERIFICATION_COMMONS.md` §4 ("the fifth stratum") and build-order
step 8. That doc argues *why* a trained verifier is the highest-leverage
build and sets the governance (eval-card gate, provenance rules,
gate/train split). This doc is the *how*: base model, data, training
hardware, benchmark targets, milestones.

**The one-paragraph project.** Train a small (0.8–4B) document-grounded
claim verifier on a Qwen3.5 base, using the public HalluGuard-Preferences-76k
ORPO dataset as the proven core recipe plus a synthetic stream generated
through *our* production interface (real chunks → `extract_claim_list` →
construction-labeled corruptions). Train locally on the 128GB Strix Halo
node. Certify on the external best-in-class benchmarks (LLM-AggreFact,
RAGTruth, FaithBench) *and* our internal banks, ship as a versioned GGUF
behind the §4 eval-card gate, and let it replace per-claim judge calls in
the grounding gate — turning the ~35-call fan-out into sub-second local
verification with a `violation_prob` that finally means something.

---

## 1. Success criteria

Two red lines from the commons doc apply throughout: hallucination-catch
(sensitivity) never regresses to buy specificity, and the calibration banks
are never trained on. On top of those, concrete targets:

### External (the "best in class" claim)

| Benchmark | Metric | Current holders | v0 target | Stretch |
|---|---|---|---|---|
| LLM-AggreFact (11-subset avg) | BAcc | Bespoke-MiniCheck-7B **77.4**; HalluGuard-4B 75.7; GPT-4o 75.9 | ≥ 75.7 at ≤4B (match the recipe we're reproducing) | **> 77.4** — leaderboard top at half the params |
| RAGTruth subset | BAcc | MiniCheck-7B 84.0 = HalluGuard-4B 84.0; Granite Guardian 3.3-8B 82.2 | ≥ 84.0 | > 85 |
| FaithBench (hard, example-anchored) | BAcc | small classifiers collapse here (HHEM 52.6 vs frontier-judge 68.8) | beat the small-classifier floor by a wide margin; report honestly | approach frontier-judge band |

FaithBench is in the card precisely because it is where small verifiers
fail: the design-study survey found example-anchored hard benchmarks are
decisive, and headline LLM-AggreFact numbers hide the collapse. If our
model matches MiniCheck on LLM-AggreFact but craters on FaithBench, it is
not best in class and the card must say so.

### Internal (what "good" means for *our* runtime)

- **Chaos benches:** RL-2 (hallucination released) stays 0; RL-1
  (false abstention) non-regressed vs. the current judge configuration,
  measured by `rescore` over frozen transcripts.
- **Calibration:** reliability curve on a held-out receipt set; ECE
  reported on the card; τ published as an operating point on that curve
  ("τ = 0.9 ⇒ ≤10% of released claims in this band unsupported, on
  holdout") — not a bare constant.
- **Operating-point behavior:** sensitivity/specificity at the *strict*
  thresholds the gate actually uses, not just the BAcc-optimal midpoint.
  The published caution stands: reasoning verifiers can gain headline
  accuracy while losing recall exactly where safety cares.
- **Latency:** per-claim verify (chunk + claim in, verdict out) fast
  enough that a 35-claim turn costs seconds, not the current primary-tier
  fan-out. Measured on both fleet tiers (Strix Halo APU, mac).
- **Mechanism fidelity:** the §4 audit — does it track the support
  mechanism or memorize surface patterns (corruption-site probes; the
  harness pattern exists in chaos-QA).

## 2. Base model: Qwen3.5 dense

| Candidate | Role | Why |
|---|---|---|
| **Qwen3.5-0.8B** | pipeline shakeout + latency tier | Already in the fleet as the spec-decode draft (GGUF load + tokenizer proven, n_vocab 248,320). Trains in hours on Strix Halo — every pipeline bug gets found here, not on the 4B run. May ship as the "fast lane" verifier if calibration holds. |
| **Qwen3.5-4B** | **the v0 model** | The HalluGuard result is *at* 4B on Qwen3 — same recipe on a newer base is the straightest path to matching it, and 4B is the largest size that trains in acceptable wall-clock locally (§4). Q8_0 GGUF ≈ 4.3GB — resident-friendly on every fleet node. |
| Qwen3.5-9B | stretch only | ~2.2× the 4B step cost → weeks per epoch locally. Only if 4B plateaus below target, and probably rented compute. Not in the v0 plan. |

Notes that matter:

- **Family alignment is a feature.** Qwen3.5 shares the 248,320-token
  vocabulary with Qwen3.6 (invariant note on spec-decode tokenizer match),
  our resident slow tier is Qwen3.5-35B-A3B, and our router embedder is
  Qwen3-Embedding. One tokenizer family across generate/route/verify
  simplifies everything downstream.
- **Structural independence is preserved anyway:** the verifier is a
  *separately trained* model with a different objective; the
  self-confirmation concern in the commons doc is about sharing weights
  with the answering model, not sharing a tokenizer.
- Qwen3.5 is natively multimodal (early-fusion). We train and deploy
  text-only; M0 verifies that the text-only fine-tune path and the
  llama.cpp conversion of the *fine-tuned* checkpoint both work before any
  real training spend (the 0.8B fleet GGUF proves the architecture
  converts; M0 proves our checkpoint does).

## 3. Data: three streams, one provenance rule

The provenance hierarchy from the commons doc (**Constructed > Mechanical >
StrongModelJudged**; never train on agreement alone) governs all three.

### Stream A — HalluGuard-Preferences-76k (the proven core)

76,708 ORPO tuples, Apache 2.0: prompt = instructions + document + claim;
chosen = Qwen3-235B-A22B reasoning + verdict; rejected = Qwen3-0.6B.
FineWeb-derived documents, synthetic grounded/hallucinated claims,
dual-judge consensus filtering (GPT-OSS-120B + DeepSeek-V3.1).

- Use near-verbatim for the recipe-reproduction runs (M1, first 4B run).
  The 235B-teacher work is already paid for — that is the point of using
  the dataset rather than re-fabricating 76k pairs with a weaker local
  teacher.
- Its FineWeb provenance means no construction-time overlap with the
  LLM-AggreFact test suites, but we still run the M0 contamination pass
  (below) rather than trusting that.
- Its prompt format is theirs, not ours. That is fine for the core run;
  Stream B is what teaches the deployment interface.

### Stream B — our synthetic harness (the production interface)

The commons doc's bootstrap discipline, made concrete:

- **Substrate:** retrieval chunks from the public machine-stable bank
  corpora (Secret Agent, Saltgrass) — *our* chunk shapes, evidence
  windows, OCR artifacts — run through the production `extract_claim_list`
  so claims are in the exact register the verifier will see at the gate.
- **Corruption taxonomy** (labels by construction — each corruption is
  mechanically checkable at the known corruption site, and the load-time
  fairness contract (`question.rs:191-233`) validates every generated case
  exactly as it validates a hand-written one):

  | Corruption | Real failure it mirrors |
  |---|---|
  | entity swap | value bound to the wrong entity across a chunk boundary (observed in prod) |
  | number/date perturbation | numeric_audit escapes |
  | negation / modal flip | polarity errors judges miss |
  | cross-chunk chimera | two true fragments fused into one false claim |
  | OCR/date garble | "OCR-garbled date survives the gate" (observed) |
  | distractor absorption | adjacent-doc fact absorbed as if grounded (observed) |
  | unsupported-but-plausible addition | classic RAG hallucination |

- **Supported cases are half the job.** The timidity tax is a red line:
  generate hard *grounded* claims too — paraphrases, multi-hop-within-
  window, unit conversions — so the model learns confident support, not
  just suspicion. Class balance ~50/50 like the benchmarks score.
- **Preference-pair construction, our models:** chosen = Qwen3.5-35B-A3B
  (resident slow tier) or cloud-frontier *over public corpora only*
  (fabricator, never oracle — the label is already fixed by construction
  before the teacher writes a word); rejected = Qwen3.5-0.8B. Keep pairs
  only where the chosen response's *verdict matches the constructed
  label*; a teacher that gets a constructed case wrong contributes a
  discarded pair, not a bad label.
- **Volume:** 20–40k pairs (order-10⁴ is what the small-verifier
  literature actually used; the harness can always run longer).
- **Hard-negative loop:** every M1/M3 eval error on our internal banks
  feeds the next generation batch's taxonomy weights.

### Stream C — in-situ receipts (v1, not v0)

D0 corrections and receipt-grade episodes fold into training only after
the commons capture loop (steps 1–3 of the parent doc's build order)
exists. v0's job is to be worth correcting.

### Contamination and the gate/train split

- M0 ships an n-gram/embedding dedup pass between *all* training streams
  and the LLM-AggreFact + FaithBench test sets; report the collision count
  on the eval card (target: 0 after filtering).
- The chaos/calibration banks and their receipts are **never** in
  training. Leakage is detectable because every receipt carries
  provenance. The verifier is never gated by data it saw.

## 4. Training on the Strix Halo node

**Stack:** ROCm 7.x on gfx1151 + PyTorch 2.11 ROCm wheels (the official
ROCm/PyTorch container is the known-good path; `TORCH_BLAS_PREFER_HIPBLASLT=1`),
TRL `ORPOTrainer` + PEFT LoRA. ORPO is monolithic — no reference model in
memory, unlike DPO — which is part of why the recipe suits this hardware.
FlashAttention on gfx1151 is immature; plan on SDPA and treat any FA/aotriton
win as a bonus. Community SFT/LoRA guides for this exact APU exist and are
the M0 starting point rather than first-principles bring-up.

**Memory (128GB unified):** 4B bf16 weights ≈ 8GB. LoRA (r=32, α=64, all
linear) leaves >100GB for activations — seq 4096 with real batch sizes
fits trivially. Even full-FT AdamW (~48GB states) fits; memory is simply
not the constraint on this box.

**Throughput is the constraint.** Realistic sustained training compute on
gfx1151 is ~10–25 TFLOPS. Cost per ORPO epoch ≈ 6·N·T FLOPs with
T ≈ 76k pairs × ~3k tokens (chosen+rejected sequences) ≈ 230M tokens:

| Run | Est. wall-clock / epoch | Plan |
|---|---|---|
| 0.8B, Stream A | ~0.5–1 day | M1 shakeout; multiple hyperparameter attempts are affordable |
| 4B LoRA, Stream A (+B) | ~2.5–5 days | the v0 run; 2 epochs ≈ ~1 week — schedule it, don't babysit it |
| 4B full-FT | ~4–7 days/epoch | only if LoRA measurably plateaus below the M1-projected target |
| 9B anything | weeks | out of v0; rented compute if ever |

These are honest estimates, not promises; M0 measures actual tokens/sec on
a 100-step probe and re-derives this table before the 4B run is scheduled.
The same box also does all Stream-B generation (inference-heavy — the
thing Strix Halo is genuinely good at, 35B-A3B resident): generate first,
then train, so the two workloads don't fight for the GPU.

### The second box: the 64GB M2 Max

The M2 Max does not relieve the throughput constraint — Apple GPUs run
FP16 at FP32 rate, so its ~13.6 TFLOPS peak sits *below* the Halo's
training compute, and MPS/MLX efficiency doesn't change that ordering.
Where it wins is memory bandwidth (400 vs 256 GB/s → faster inference
decode) and simply being a second machine. So it joins the plan as a
**parallel lane**, not an alternative:

- **Stack: `mlx-lm-lora`, not PyTorch MPS.** Native ORPO on Apple Silicon
  (monolithic, LoRA/full-FT/QAT), mature and purpose-built. If PyTorch
  MPS is ever used on this box instead, the standing kernel-panic
  invariant applies in full (this exact machine was panicked by an
  unguarded long-context MPS loop on 2026-07-07): MPS watermark env vars,
  `empty_cache` between items, in-process RSS guard, and never co-run
  with the resident daemon + 35B judge (~14GB idle, 33GB loaded).
- **Role 1 — the eval box.** LLM-AggreFact/RAGTruth/FaithBench sweeps
  over checkpoints via llama.cpp Metal, plus internal-bank `rescore`
  runs. Inference-heavy, bandwidth-bound: this is what the box is best
  at, and it keeps eval off the Halo while the Halo trains.
- **Role 2 — the ablation lane.** 0.8B data-mix studies (Stream A vs
  A+B, taxonomy weightings) run here in parallel with Halo work.
  Cross-framework caveat, stated plainly: MLX results inform *data*
  decisions (relative comparisons on identical data), but hyperparameters
  do not transfer 1:1 to TRL/ROCm — so the M1 recipe-fidelity run and
  the M3 run stay on the same stack (TRL on the Halo), and the M2 lane
  never gates them.
- **Laptop reality:** it is the daily dev machine. Multi-day runs don't
  belong here; checkpoint-per-N-steps and expect interruptions.

M0 probes **both** boxes (TRL/ROCm on the Halo, mlx-lm-lora on the M2)
and assigns roles from the measured table, not from this prose.

**Hyperparameters:** start from the HalluGuard paper's published settings
where available, else TRL ORPO defaults (β≈0.1, lr 1e-4 LoRA / 5e-6 full,
cosine, ~3% warmup, effective batch ~32 via grad accumulation, bf16,
seq 4096 with document truncation matched to our chunk sizes). Every run
logs to a run manifest (config + data-stream digests + seed) so the eval
card can cite exactly what produced the checkpoint — runs are actuation
evidence, glassbox applies.

## 5. The eval card (the ship gate)

A verifier version is an actuation event on the slowest stratum (§4 of the
parent doc). The card that gates shipping:

1. External table (§1) — LLM-AggreFact per-subset + avg, RAGTruth,
   FaithBench — with the contamination report attached.
2. Chaos red lines via `rescore` over frozen transcripts: RL-2 = 0,
   RL-1 non-regressed vs. the incumbent judge config.
3. Calibration curve + ECE on held-out receipts; τ published as an
   operating point with its guaranteed bound.
4. Operating-point sensitivity/recall at gate thresholds (not just BAcc).
5. Mechanism-fidelity audit (corruption-site probes).
6. Adversarial holdout from the bank-growth loop.
7. Quant check: bf16 vs Q8_0 vs Q4_K_M deltas on the internal banks
   (ship Q8_0 unless Q4_K_M is within noise; the router's f16-vs-Q8_0
   equivalence measurement is the template).
8. Latency table per fleet tier.

## 6. Output contract and integration

- **Dual-format training, prompt-selected:** (a) *justify-then-verdict*
  (the HalluGuard SRM style — glassbox justification for the ledger and
  for D0 correction UX), and (b) *verdict-first* (a few tokens — the
  latency mode for the per-claim gate path, where prefill dominates and
  the shared-document prefix is cached across a turn's claims).
- **`violation_prob` becomes real:** verdict-token logprob, post-hoc
  calibrated (temperature/isotonic on held-out receipts) — the
  training-signal rule from the parent doc, applied.
- **Ships as** a versioned GGUF through the existing `model_fetch` sha256
  path, as an **opt-in judge slot** in the grounding gate. Deterministic
  vetoes stay in front and are not trainable. A sampled second-opinion
  path on a diverse judge remains (monoculture counter).
- **Cross-version A/B** is `rescore` over frozen transcripts; the drift
  watch compares verdict distributions across versions.

## 7. Milestones

| | Deliverable | Gate to pass |
|---|---|---|
| **M0** (~week 1) | Two-box bring-up: ROCm/PyTorch/TRL on the Strix Halo + `mlx-lm-lora` on the M2 Max, 100-step ORPO probes on 0.8B with measured tok/s on both; HalluGuard-76k downloaded + schema-validated; eval harness running LLM-AggreFact/RAGTruth/FaithBench against off-the-shelf MiniCheck + our banks; contamination pass | baseline table exists; wall-clock table re-derived from measured tok/s on both boxes and roles assigned; fine-tuned-checkpoint → GGUF conversion proven on the probe checkpoint |
| **M1** | 0.8B trained on Stream A, full eval card produced (numbers will be sub-4B — that's fine) | pipeline end-to-end: train → eval → calibrate → GGUF → `rescore` A/B all work; card template exists |
| **M2** | Stream B harness: corruption taxonomy implemented over Secret Agent/Saltgrass via `extract_claim_list`; 20–40k validated pairs; 0.8B mix study (A vs A+B) | every generated case passes the fairness contract; mix study shows B is non-harmful on external + helpful on internal banks |
| **M3** | **The v0 run:** 4B LoRA on A+B; eval card | §1 targets: ≥75.7 avg / ≥84.0 RAGTruth; FaithBench reported; internal gates green |
| **M4** | Ship: Q8_0 GGUF via model_fetch, opt-in judge slot, latency measurement in the real gate path | full §5 card green; two red lines non-regressed; adoption opt-in |
| stretch | best-in-class push: hard-negative loop iterations, full-FT if warranted, >77.4 avg | leaderboard-top claim only with the FaithBench caveat honestly stated |

M0–M2 are cheap and mostly parallelizable with the commons build-order
steps 1–3. The only expensive, serial thing in the plan is the M3 training
run, and by then it runs on a measured wall-clock table, a shaken-out
pipeline, and a mix study — not hope.

## 8. Risks

- **gfx1151 training maturity** — the top project risk. Mitigation: M0 is
  a hard gate with measured throughput before anything is scheduled;
  known-good ROCm containers; 0.8B absorbs all the debugging. Escape
  hatch: a rented GPU for the single M3 run changes nothing else in the
  plan.
- **FaithBench collapse** — small classifiers historically fall apart on
  example-anchored hard cases. Mitigation: it's in the card from M1, not
  discovered at M4; the SRM (reasoning) format + hard-negative loop are
  the levers; the claim "best in class" is scoped by what the card shows.
- **Recall loss at strict operating points** — reasoning verifiers gain
  BAcc while losing recall where the gate lives. Mitigation: card item 4
  is a first-class gate, not a footnote.
- **Interface mismatch** — a model trained mostly on HalluGuard's prompt
  register underperforming on our claim style. Mitigation: Stream B
  exists precisely for this; the M2 mix study measures it before M3.
- **Teacher-verdict noise in Stream B** — mitigated by construction-first
  labeling (teacher disagreement discards the pair, never relabels it).
- **Contamination embarrassment** — mitigated by the M0 dedup pass and
  publishing the collision count on the card.

## 9. Non-goals (v0)

No RL (J1-style verifiable-reward RL is a v1+ option once receipt volume
exists — never RL from preference/agreement). No 9B/27B. No multimodal
verification. No online adaptation of any kind — a verifier version is a
discrete, attributed, reversible actuation, exactly like every other
stratum. No training on calibration banks, receipts, or anything the gate
uses to certify — provenance enforces the split.

## 10. Sources

- [HalluGuard-Preferences-76k (HF dataset)](https://huggingface.co/datasets/lrsbrgrn/HalluGuard-Preferences-76k) — 76,708 ORPO tuples, Apache 2.0, construction + filtering details
- [HalluGuard paper note (arXiv 2510.22395)](https://github.com/AkihikoWatanabe/paper_notes/issues/3065) · [emergentmind topic page](https://www.emergentmind.com/topics/halluguard-framework) — 4B SRM, ORPO, 84.0 RAGTruth / 75.7 LLM-AggreFact
- [LLM-AggreFact leaderboard](https://llm-aggrefact.github.io/) · [MiniCheck (GitHub)](https://github.com/Liyan06/MiniCheck) · [Bespoke-MiniCheck-7B](https://docs.bespokelabs.ai/models/bespoke-minicheck) — the 77.4 BAcc bar
- [Paladin-mini (arXiv 2506.20384)](https://arxiv.org/html/2506.20384v1) — grounding model emphasizing real-world/operating-point evaluation
- [Qwen3.5 family overview](https://enclaveai.app/blog/2026/03/08/qwen-3-5-complete-model-family-local-ai/) · [Qwen/Qwen3.5-27B (HF)](https://huggingface.co/Qwen/Qwen3.5-27B) — dense 0.8B–27B lineup, March 2026
- [Strix Halo LLM performance tracker](https://llm-tracker.info/AMD-Strix-Halo-(Ryzen-AI-Max+-395)-GPU-Performance) · [Strix Halo fine-tuning guide (SFT/LoRA)](https://www.promptinjection.net/p/how-to-fine-tune-llms-on-amd-strix-halo-ryzen-ai-max-395-sft-lora) · [Level1Techs benchmark thread](https://forum.level1techs.com/t/strix-halo-ryzen-ai-max-395-llm-benchmark-results/233796) — gfx1151 ROCm/PyTorch state, fine-tuning viability
- [mlx-lm-lora (GitHub)](https://github.com/Goekdeniz-Guelmez/mlx-lm-lora) · [PyPI](https://pypi.org/project/mlx-lm-lora/) — native ORPO/DPO/LoRA training on Apple Silicon (the M2 lane's stack)
- Internal: MPS long-context kernel-panic invariant `[env:macos-arm64]` (2026-07-07) — mandatory guards for any PyTorch/MPS run on the M2 box
- Internal: `VERIFICATION_COMMONS.md` (parent design study); chaos-QA calibration arc; situated-harness study; spec-decode tokenizer invariant (Qwen3.5/3.6 vocab 248,320)
