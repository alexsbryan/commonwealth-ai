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

## 0. Build vs. adopt — why train at all

The adopt option is real and the spec takes it seriously. The candidates,
as of 2026-07-28:

| Off-the-shelf | Numbers | Why it can't simply be the answer |
|---|---|---|
| **HalluGuard-Qwen3-4B** (checkpoint + GGUF published on HF — this **supersedes** the design-study conclusion that no Qwen3-family verifier existed to adopt) | 84.0 RAGTruth / 75.7 avg | the strongest adopt candidate, and it enters M0 as a first-class baseline; license unverified (dataset is Apache 2.0; model card TBC at M0). Trained on FineWeb prose + their register, not our chunks/claims — the distribution gap is the open question our banks answer |
| Bespoke-MiniCheck-7B | 77.4 avg (leaderboard top) | **CC-BY-NC-4.0** — commercial use requires a negotiated license. Baseline-only; never shippable |
| Granite Guardian 3.3-8B | 82.2 RAGTruth | Apache 2.0, but 8B (≈2× the latency/residency budget of 4B) and a foreign tokenizer family in a fleet standardized on Qwen |
| Lynx 8B/70B | strong HaluBench | Llama family + sizes wrong for the per-claim budget |

Given that, four reasons to build, in order:

1. **The flywheel is the point, not the checkpoint.** The parent doc's
   architecture makes the verifier the one component that must keep
   learning from the product's own evidence: D0 corrections → receipts →
   training data → v1, v2, … A frozen third-party model cannot absorb any
   of that, and `violation_prob` stays an artifact of someone else's
   training run — post-hoc calibratable, never contractual. The training
   pipeline is the asset; v0 is its first turn.
2. **The deployment interface is the training distribution.** Our gate
   feeds OCR-garbled chunks, cross-chunk evidence windows, and
   `extract_claim_list` register. Every adopt candidate trained on prose.
   The FaithBench lesson — small verifiers collapse off-distribution — is
   exactly the risk, and Stream B (training on our interface) is the only
   fix. Adoption cannot close that gap by definition.
3. **The marginal cost of build over *responsible* adopt is small.**
   Adopting safely requires the eval harness, contamination checks,
   calibration on held-out receipts, quant checks, GGUF integration, and
   the `rescore` A/B — everything in this spec except the training runs.
   The runs themselves cost owned-hardware wall-clock, a free proven
   dataset, and a published recipe at exactly our target size.
4. **Sovereignty and monoculture.** The product's premise is that the
   trust function runs locally and answerably; a strategic dependency on
   third-party frozen weights for the *core* trust function is the
   dependency the rest of the stack refuses everywhere else. And the
   parent doc's monoculture counter requires the ability to mint diverse
   verifier versions — an ability, not a download.

**The discipline that keeps this honest:** adopt-vs-build is decided by
the eval card, not by this section. M0 baselines the adopt candidates on
*our* banks; if at any milestone an adoptable checkpoint beats our best
model on the card (internal banks + external benchmarks + calibration +
latency + license), it ships in the opt-in judge slot while training
continues toward surpassing it. Build is the strategy; adopt is always
the live fallback.

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

**Stack: Unsloth-patched TRL `ORPOTrainer`, vanilla TRL as fallback.**
Unsloth shipped official AMD support (~May 2026) and lists gfx1151 /
Strix Halo as fully supported with hardware-specific kernel tuning; its
Triton kernels claim ~2× step speed and large VRAM savings, and it
patches TRL's ORPOTrainer directly, so the recipe code is the same either
way. Install path per their docs: TheRock gfx1151 nightlies
(`rocm.nightlies.amd.com/v2/gfx1151/`) for torch + `rocm[libraries,devel]`
— pin exact versions in the run manifest, since consumer-AMD support is
only months old. **M0 runs the 100-step probe both ways (Unsloth vs
vanilla TRL) and the wall-clock table below assumes vanilla** — if
Unsloth's speedup holds on this silicon, the table shrinks and that
measurement is the evidence. Base environment either way: ROCm 7.x +
PyTorch ROCm wheels, `TORCH_BLAS_PREFER_HIPBLASLT=1`, PEFT LoRA. ORPO is
monolithic — no reference model in memory, unlike DPO — which is part of
why the recipe suits this hardware. Without Unsloth, FlashAttention on
gfx1151 is immature: plan on SDPA and treat any FA/aotriton win as a
bonus. Community SFT/LoRA guides for this exact APU exist and are the M0
starting point rather than first-principles bring-up.

**Memory (128GB unified):** 4B bf16 weights ≈ 8GB. LoRA (r=32, α=64, all
linear) leaves >100GB for activations — seq 4096 with real batch sizes
fits trivially. Even full-FT AdamW (~48GB states) fits; memory is simply
not the constraint on this box.

> **MEASURED 2026-08-04 — the 4B peaks at 51.88 GB**, not "trivially".
> `runs/probe-4b-m1`, micro 1 × accum 32, seq 4096, gradient checkpointing,
> length bucketing on: torch peak alloc 51.88 GB, reserved 43.94 GB, box GTT
> 47.16 GB. It fits, and the paragraph's conclusion survives — memory is not
> what stops this run. But "leaves >100GB for activations" is wrong by more
> than 6×, and it is the number a reader would size a rented GPU from. It is
> the reason every card at or below 48 GB is out.

**Throughput is the constraint.** Realistic sustained training compute on
gfx1151 is ~10–25 TFLOPS. Cost per ORPO epoch ≈ 6·N·T FLOPs with
T ≈ 76k pairs × ~3k tokens (chosen+rejected sequences) ≈ 230M tokens:

| Run | Est. wall-clock / epoch | Plan |
|---|---|---|
| 0.8B, Stream A | ~0.5–1 day | M1 shakeout; multiple hyperparameter attempts are affordable |
| 4B LoRA, Stream A (+B) | ~2.5–5 days | the v0 run; 2 epochs ≈ ~1 week — schedule it, don't babysit it |
| 4B full-FT | ~4–7 days/epoch | only if LoRA measurably plateaus below the M1-projected target |
| 9B anything | weeks | out of v0; rented compute if ever |

> **THE TABLE ABOVE IS FALSIFIED. It is off by ~2.7× and it is what anyone
> plans from.** Line `:249` demanded exactly this re-derivation before the 4B
> run was scheduled; here it is.
>
> The optimism traces to one number: this section assumed 10–25 TFLOPS
> sustained on gfx1151. Running the model backwards from the measured step
> times gives **2.7 TFLOPS** (`findings/M0_PROBE_HALO.md:146`) — a 4–9× gap.
>
> | Run | Est./epoch (above) | **Measured, Halo** | Basis |
> |---|---|---|---|
> | 0.8B, Stream A | ~0.5–1 day | **4.8 days** | 176.71 s/it, n=61, CI [172.4, 178.5] |
> | 4B LoRA, Stream A | ~2.5–5 days | **13.3 days** | 477.2 s/it median, `runs/probe-4b-m1` |
> | 4B LoRA, 2 epochs | ~1 week | **~27 days** | — |
>
> Epoch sizing: orpo-76k is 74,674 rows → **2,334 iters/epoch** at effective
> batch 32; orpo-ab is 93,693 rows → 2,928.
>
> **M3 therefore moves to rented GPU** (operator-directed 2026-08-04). Renting
> is cheaper than the local electricity this run would burn — `:462` budgeted
> $50–75 — and 15–25× faster, which inverts `:360`'s framing of cloud as an
> escape hatch. See `research/verifier-v0/cloud/README.md` for the harness and
> `cloud/pod.sh` for the lifecycle.
>
> **The paired probe landed, so here is the measured cloud row** (note
> `8aad1dbb`; same recipe, same seed, same effective batch — only the
> accelerator differs):
>
> | Accelerator | s/it median | 4B, 2 epochs (5,856 it) | cost |
> |---|---|---|---|
> | Strix Halo (gfx1151) | 477.2 | ~27 days | electricity |
> | RTX PRO 5000 Blackwell | **38.51** | **62.6 h** | **$41.85 @ $0.6681/h** |
> | A100 SXM4 | 38.06 | 61.9 h | $58–81 |
> | RTX A6000 | ~81 | 5.5 days | $53.45 |
>
> **The Halo cannot train this at all any more, and that is not a regression in
> our code.** It SEGVs at weight load, and upstream HEAD segfaults identically
> on the same box. Its role in v0 is now **scoring**, which it does fine: G2
> proved the fuse → GGUF → serve → eval path at 4B on Vulkan (note `6d18a622`),
> and `scripts/score_checkpoint.sh` runs it end-to-end. Read this section's
> title as historical — training happens on rented GPU.

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

- **Stack: `mlx-lm-lora`, not PyTorch MPS.** Unsloth does not run on
  Apple Silicon (Triton dependency; their Mac support is "in the works"),
  so the MLX lane is the native path: `mlx-lm-lora` has ORPO on Apple
  Silicon (monolithic, LoRA/full-FT/QAT), purpose-built; `mlx-tune`
  (Unsloth-compatible API on MLX) is the alternative to evaluate at M0.
  If PyTorch
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

### 6.1 Production telemetry — the `grounding` journal stream

*(Added 2026-08-07, after the slot shape was decided (note 700bbe09:
disagreement-triggered, not a swap) and the joined-evidence arc closed
(notes aacda78c, 4d13fc80; HEADROOM_STUDY.md Addenda 4–5: our side judges
JOINED evidence at tau 0.9, the incumbent keeps its per-chunk procedure).)*

**Phasing (operator call, 2026-08-07): the stream ships FIRST, alone.**
Many target users run 16 GB VRAM; a resident 4B verifier beside the
primary does not fly there, so v0 in the field is primary + fast slot
with the incumbent gate unchanged — and this stream collecting. Phase 0
records the incumbent-only gate (implemented: the
`gate_answer_with_progress` funnel in
`sovereign-core/src/runtime/grounding/mod.rs` journals every decision;
`svrn journal grounding` reads it). What phase 0 banks is exactly the
training and calibration substrate the second-judge slot needs later:
real claims and their judged evidence, re-fetchable by handle, minable
by `control_mine` into labeled rows. The disagreement-triggered slot
(phase 1) adds fields and an escalation line kind to THIS stream — the
verdict vocabulary below already speaks four verdicts so phase 1 is an
extension, not a migration. Everything below describes the full design;
phase-0 deltas are flagged inline.

**Substrate.** A new stream on the generic journal layer
(`sovereign-contracts/src/types/journal.rs`, note b146cf12): one
vocabulary file + one row in the CLI's `VIEWS` registry. No new store, no
new verb — `svrn journal grounding …` arrives with the layer's rotation,
caps, retention and the four-way single-decider off-switch for free. The
stream is local-only and never gossips.

**Two line kinds, episode-joined** (the decision→outcome shape next-edit
and `sovereign_mesh::decision_log` both use, because the gate decision
and the escalation resolution happen at different moments and an
append-only file does not rewrite history):

1. **Decision line** — one per gated claim: both judges' raw scores
   (`our_max_p`, `incumbent_max_support`), both verdicts, agreement +
   disagreement direction, the reader-visible consequence
   (released / caveated / blocked / regenerated), per-judge latency, and
   **attribution**: verifier checkpoint digest, tau, procedure
   (joined | per_chunk). Attribution is per-line, not per-file — when the
   serving path changes, unattributed numbers silently stop meaning what
   they meant (the OICP fast-slot-hijack lesson).
2. **Escalation line** — only when disagreement fired: which rung ran,
   its verdict, wall-clock cost.

**Evidence by reference, never by value.** A decision line carries
`message_id`, the judged chunk ids IN JUDGED ORDER, and content digests —
no claim text, no chunk text. Claims already persist in the conversation
store; chunks live in the corpus; mining joins the three at read time.
This buys two things at once: (a) the stream is **metadata-only
structurally** — no `serde_json::Value`, no free-form string field — so
it inherits the next-edit honesty apparatus wholesale (note 43770c85):
content-canary tests, rates over judged-only, `None`-never-0%; (b) it
fixes the defect that made journal mining one-sided (note d68af5d9, the
~200-char snippet truncation) and contributed to the 76%-vacuous-label
result (invariant dde7675c) — recording chunk *identity* at decision time
makes every future mining pass two-sided and exact. This absorbs the
`judged_chunk_ids` item from the hygiene backlog.

**Four verdicts on the wire, per ARCH §18.** The verifier's verdict enum
is `supported | unsupported | could_not_judge | never_ran`: a timeout or
unparseable output is `could_not_judge`; the slot being off, shed, or
unloaded is `never_ran`; both cases stamp the decision line
`mode: incumbent_only`. §18.3 made structural: a week of verifier outage
surfaces as a `never_ran` count in `stats`, never as a mysteriously calm
disagreement rate.

**Read surfaces.** `svrn journal grounding stats` prints:

- disagreement rate over both-judged lines only, against the
  **pre-registered expectation band of ~16–18%** of gated claims
  (control bank 16.2%, journal-strong prose 17.5%, both at the shipped
  operating points — rows `runs/headroom/{control_joined,
  jrnl_strong_joined}_scored.jsonl`), with an early-signal label under a
  minimum judged count (mirroring next-edit's under-20 rule);
- the direction split — the bank predicts incumbent-flags-ours-passes
  DOMINATES on grounded-heavy traffic (11 vs 6 on journal prose); an
  inverted ratio in production is a drift signal, not a curiosity;
- escalation outcomes; `could_not_judge` / `never_ran` counts;
- score-distribution quantiles for both judges vs the bank's — the
  tau-drift check that works BEFORE any labels exist (the tau 0.9 pick
  rests on n=78 fabs and a coarse score distribution; production
  quantiles are what re-justify or re-pick it).

One row joins `svrn posture`: checkpoint digest, tau, stream freshness,
disagreement rate vs band.

**The flywheel.** An idle-time mining pass — `control_mine.py` with the
strong-label gate (invariant dde7675c) over journal × conversation store
× corpus — turns decision lines into mechanically labeled rows
continuously. That makes the chaos-monkey causal partition computable ON
PRODUCTION: "this week the verifier flagged N claims the incumbent
passed; M were mechanically confirmed fabrications; K were false alarms
costing one escalation each." That sentence is the slot's scoreboard, the
user-side receipt (judges are proxies; this is what the gate DID to real
answers), and the instrument that re-checks tau on production
distributions instead of n=78.

**Decentralized posture — the part that is easy to design wrong.** This
runs in dozens of meshes we do not operate, administer, or observe.
Consequences, each load-bearing:

- **The consumer of this stream is the LOCAL operator, never us.** There
  is no phone-home, no central dashboard, and no "we'll watch prod and
  re-tune" loop — that loop does not exist and must not be assumed
  anywhere in the slot's design. Every question the telemetry answers
  must be answerable BY the node that recorded it, which is why the
  expectation band, the direction prior, and the minimum-judged-count
  rule ship IN THE STATS VIEW as constants, not in a runbook we hold.
- **The band is honest about its provenance.** 16–18% was pre-registered
  on OUR corpora and OUR traffic shape. A mesh gating legal filings or
  lab notebooks will sit elsewhere, and `stats` must say "expected band
  pre-registered on the dev mesh's banks" rather than implying a
  universal constant — a violated band on foreign corpora is a prompt to
  self-calibrate, not a defect report.
- **The flywheel is a local capability, and that is its strength.** The
  mining pass ships as a tool, so each mesh can grow its own labeled
  bank from its own traffic and re-pick its own tau against its own
  corpus — self-calibration without any data leaving the node. Our
  measured tau 0.9 is the shipped default, not a decree.
- **Aggregation only ever rides the bundle path**: the journal layer's
  `bundle` verb — human-initiated, consent-gated, with the
  field-collector audit and content canaries deciding what can leave.
  Nothing in the gate path depends on any bundle ever being shared.
- **Version skew is permanent.** Dozens of meshes means checkpoints,
  taus, and stream schema versions coexist for months — per-line
  attribution (above) is what keeps a mixed-version journal readable,
  and record parsing must tolerate unknown fields from newer writers
  rather than refusing the file.

**Defaults posture.** The stream ships with the slot under the existing
DEFAULTS_LEDGER.md:438-472 re-open gate (note 700bbe09 — no new default
minted); its off-switch is the journal layer's standard one.

**Open question, deliberately unresolved on paper:** whether the
reader-visible consequence can always be stamped at decision time, or
whether regeneration loops make it a second outcome line (next-edit's
pattern). Resolve against the actual gate code during the build, not
here.

## 7. Milestones

| | Deliverable | Gate to pass |
|---|---|---|
| **M0** (~week 1) | Two-box bring-up: ROCm/PyTorch on the Strix Halo with the probe run both Unsloth-patched and vanilla-TRL, + `mlx-lm-lora` on the M2 Max — 100-step ORPO probes on 0.8B with measured tok/s for all three; HalluGuard-76k downloaded + schema-validated; eval harness running LLM-AggreFact/RAGTruth/FaithBench against the §0 adopt candidates (HalluGuard-Qwen3-4B GGUF first, incl. license check; MiniCheck) + our banks; contamination pass | baseline table exists; wall-clock table re-derived from measured tok/s on both boxes and roles assigned; fine-tuned-checkpoint → GGUF conversion proven on the probe checkpoint |
| **M1** | 0.8B trained on Stream A, full eval card produced (numbers will be sub-4B — that's fine) | pipeline end-to-end: train → eval → calibrate → GGUF → `rescore` A/B all work; card template exists |
| **M2** | Stream B harness: corruption taxonomy implemented over Secret Agent/Saltgrass via `extract_claim_list`; 20–40k validated pairs; 0.8B mix study (A vs A+B) | every generated case passes the fairness contract; mix study shows B is non-harmful on external + helpful on internal banks |
| **M3** | **The v0 run:** 4B LoRA on A+B; eval card | §1 targets: ≥75.7 avg / ≥84.0 RAGTruth; FaithBench reported; internal gates green |
| **M4** | Ship: Q8_0 GGUF via model_fetch, opt-in judge slot, latency measurement in the real gate path | full §5 card green; two red lines non-regressed; adoption opt-in |
| stretch | best-in-class push — now the §10 campaign: RLVR on Stream B labels (M3.5), span axis, self-play generations, ≥80 avg | leaderboard-top claim only with the FaithBench caveat honestly stated; every RL round gated by external non-regression |

M0–M2 are cheap and mostly parallelizable with the commons build-order
steps 1–3. The only expensive, serial thing in the plan is the M3 training
run, and by then it runs on a measured wall-clock table, a shaken-out
pipeline, and a mix study — not hope.

## 8. Risks

- **gfx1151 training maturity** — the top project risk. Unsloth's
  official Strix Halo support (~May 2026) improves the odds but is itself
  young (nightly-pinned torch; recent bug-fix PRs against this exact
  target). Mitigation: M0 is a hard gate with measured throughput before
  anything is scheduled; the Unsloth-vs-vanilla A/B keeps a fallback
  stack proven at all times; 0.8B absorbs all the debugging. Escape
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

## 10. Path to the lead — the 2026 campaign (post-v0)

v0's stretch goal is >77.4 — the leaderboard top at half the params. This
section is the plan for taking the lead *considerably*: **≥80 avg BAcc on
LLM-AggreFact single-pass**, a span-detection axis nobody at this size
holds, and FaithBench reported honestly throughout. Nothing here changes
M0–M4; every lever below *consumes* the M2 harness and the M3 checkpoint.
The board has sat under 78 since mid-2025 (Bespoke-MiniCheck-7B 77.4;
Paladin-mini's 79.31 claim only averages in their own benchmark), and the
2026 methods that break the ceiling map directly onto assets this plan
already builds for other reasons.

### Amending the §9 RL non-goal, explicitly

§9 says "No RL … J1-style verifiable-reward RL is a v1+ option once
receipt volume exists — never RL from preference/agreement." That
deferral was premised on receipts being the only verifiable-reward
source. The 2026 field result (RLVR/GRPO as the standard post-training
step; RL4HS showing span-level RL beating SFT for exactly our task) plus
an observation about our own design changes the premise: **Stream B's
labels-by-construction are verifiable rewards** — the corruption site is
mechanically checkable at training time by the same code that validates
the case at generation time. No reward model, no preference signal, no
agreement proxy. The spirit of the non-goal (never RL from
preference/agreement) is preserved; the letter (wait for receipts) is
amended: the campaign runs RLVR on constructed labels at v0.5, receipts
join at v1 as additional reward sources, not the first ones.

### The four levers, in order of expected yield

1. **RLVR stage on Stream B (M3.5).** GRPO with class-aware advantage
   normalization (the RL4HS/CAPO recipe) on top of the M3 ORPO
   checkpoint. Prompt pool = Stream B cases (+ a Stream A slice for
   register balance); reward = verdict correctness against the
   constructed label + span match at the corruption site + abstain
   shaping at the strict operating points. Verifier rollouts are short
   (chunk + claim → bounded think → verdict), so group sampling is cheap
   relative to math-RL. This is the single most likely multi-point jump.
2. **Span-level supervision, free.** Every generated corruption carries
   its span offsets (a field the Stream B export includes from day one —
   trivial now, expensive to retrofit). Train verdict + span jointly;
   report RAGTruth span F1 as a second leaderboard axis (RL4HS-14B holds
   58.3; a 4B beating it on constructed-span data would be a result on
   its own). The same localization feeds the §6 gate UX.
3. **Self-play hard negatives (the flywheel, automated).** The
   hard-negative loop in §3 is the manual version of 2026's
   detector/generator self-play (arXiv 2607.07993): evolve a generator
   rewarded for corruptions the frozen detector misses, retrain, repeat.
   Our version is *sounder than the paper's*: their labels come from
   generator intent; ours stay fixed by mechanical construction while
   only difficulty evolves — the corruption checker is the referee, so
   escalation cannot rot the labels. Two to three generations, each
   gated by the mix-study rule (non-harmful external, helpful internal).
4. **Test-time scaling, reported honestly.** k-sample self-consistency
   with Qwen3.5's native think budget. The card reports single-pass and
   k=5 as separate rows; the ≥80 target is single-pass, the ensemble row
   is the production slow-judge option and never the headline claim.

The FaithBench guardrail applies with more force under RL, not less:
optimizing hard against our distribution is exactly how a small verifier
buys headline BAcc with off-distribution collapse. The M2 mix-study gate
(external non-regression) runs after *every* RL round, and any round
that trades FaithBench for LLM-AggreFact is discarded.

### Budget — owned machines, and where rented capacity buys weeks

Measured anchors so far (M0): 0.8B ORPO on the M2 Max ≈ 53 s/it at
effective batch 32 → ~35 h/epoch on Stream A; eval throughput on the M2
Max ≈ 6 h per 2,200-row per-subset card at 4-way concurrency (≈3.3 days
for the full 29,320-row leaderboard set). Everything marked *est.* below
is re-derived from a 1-day rollout-throughput probe before scheduling —
the M0 discipline applies to every stage.

| Stage | Owned hardware | Est. wall-clock (owned) | Rented (H100-class, ~$2.5–3/h) |
|---|---|---|---|
| Stream B generation + teacher labeling (40k pairs) | Halo (35B-A3B resident); rejected-side 0.8B on M2 | ~3–5 days *est.* | frontier-API chosen-side: order $50–150 |
| M3: 4B ORPO, A+B, 2 epochs | Halo (§4 table) | ~1 week | ~16–24 h ≈ $50–75 |
| M3.5: GRPO+span round (~30k prompts × k=8, ~96M rollout tokens) | Halo, rollouts via server-mode Q8 | ~2.5–6 days/round *est.* | ~0.5 day/round ≈ $30–60 |
| Self-play: generator round + detector round, ×2–3 generations | Halo | ~1–2 weeks/generation *est.* | ~1–1.5 days/gen ≈ $75–150 |
| Eval cards (per checkpoint) + full leaderboard run | M2 Max, overlapped with training | free (6 h/card; 3.3 d full set) | full set ≈ $25 |

Three ways to run it:

- **Owned-only:** ≈ 5–7 weeks calendar after M4, $0 marginal. The Halo
  serializes generation → ORPO → RL rounds; the M2 Max runs every card
  in parallel. Viable, just slow — and it monopolizes the Halo.
- **Hybrid (recommended):** keep generation + M3 owned (they're
  scheduled-not-babysat anyway), rent for the RL rounds and the full
  leaderboard runs. ≈ 3–4 weeks calendar, **≈ $300–800 total**. The §8
  escape-hatch principle already blesses this: a rented GPU for a
  bounded run changes nothing else in the plan.
- **Aggressive:** rent everything after M2 → ≈ 2 weeks, ~$1–1.5k. Only
  worth it if the board moves under us and timing starts to matter.

The 9B escalation (§2: only if 4B plateaus) roughly 2.2×'s the training
rows; even the aggressive path stays under ~$3k. Provenance discipline
is unchanged off-box: rented runs pull only public-substrate streams
(Stream A/B), never receipts or calibration banks, and every run
manifest records where it executed.

### Sequencing note

M3.5 inserts between M3 and M4 only if the M3 card lands ≥75.7 (the
recipe-reproduction bar). If M3 undershoots, fix the base recipe first —
RL on top of a broken SFT/ORPO checkpoint launders the problem, it
doesn't solve it. The campaign proper (self-play generations, leaderboard
submission) runs post-M4 as v0.5, so shipping the opt-in judge slot is
never hostage to the leaderboard push.

## 11. Sources

- [HalluGuard-Preferences-76k (HF dataset)](https://huggingface.co/datasets/lrsbrgrn/HalluGuard-Preferences-76k) — 76,708 ORPO tuples, Apache 2.0, construction + filtering details
- [HalluGuard paper note (arXiv 2510.22395)](https://github.com/AkihikoWatanabe/paper_notes/issues/3065) · [emergentmind topic page](https://www.emergentmind.com/topics/halluguard-framework) — 4B SRM, ORPO, 84.0 RAGTruth / 75.7 LLM-AggreFact
- [lrsbrgrn on HF](https://huggingface.co/lrsbrgrn) — HalluGuard-Qwen3-4B checkpoint **and** GGUF are published (the §0 adopt candidate) · [Bespoke-MiniCheck-7B](https://huggingface.co/bespokelabs/Bespoke-MiniCheck-7B) — CC-BY-NC-4.0, baseline-only
- [LLM-AggreFact leaderboard](https://llm-aggrefact.github.io/) · [MiniCheck (GitHub)](https://github.com/Liyan06/MiniCheck) · [Bespoke-MiniCheck-7B](https://docs.bespokelabs.ai/models/bespoke-minicheck) — the 77.4 BAcc bar
- [Paladin-mini (arXiv 2506.20384)](https://arxiv.org/html/2506.20384v1) — grounding model emphasizing real-world/operating-point evaluation
- [Qwen3.5 family overview](https://enclaveai.app/blog/2026/03/08/qwen-3-5-complete-model-family-local-ai/) · [Qwen/Qwen3.5-27B (HF)](https://huggingface.co/Qwen/Qwen3.5-27B) — dense 0.8B–27B lineup, March 2026
- [Strix Halo LLM performance tracker](https://llm-tracker.info/AMD-Strix-Halo-(Ryzen-AI-Max+-395)-GPU-Performance) · [Strix Halo fine-tuning guide (SFT/LoRA)](https://www.promptinjection.net/p/how-to-fine-tune-llms-on-amd-strix-halo-ryzen-ai-max-395-sft-lora) · [Level1Techs benchmark thread](https://forum.level1techs.com/t/strix-halo-ryzen-ai-max-395-llm-benchmark-results/233796) — gfx1151 ROCm/PyTorch state, fine-tuning viability
- [Unsloth: official AMD support](https://unsloth.ai/docs/blog/unleash-the-power-of-amd-official-support-for-unsloth-is-here) · [AMD technical article](https://www.amd.com/en/developer/resources/technical-articles/2026/train-and-run-models-on-amd-gpus-with-unsloth.html) · [Unsloth Studio on Strix Halo notes](https://github.com/t-sinclair2500/unsloth_studio_rocm_Halo_Strix) — gfx1151 fully supported, TheRock-nightly install path; no Apple Silicon support ([requirements](https://unsloth.ai/docs/get-started/fine-tuning-for-beginners/unsloth-requirements))
- [mlx-lm-lora (GitHub)](https://github.com/Goekdeniz-Guelmez/mlx-lm-lora) · [PyPI](https://pypi.org/project/mlx-lm-lora/) · [mlx-tune (Unsloth-compatible API on MLX)](https://github.com/ARahim3/mlx-tune) — native ORPO/DPO/LoRA training on Apple Silicon (the M2 lane's stack)
- Internal: MPS long-context kernel-panic invariant `[env:macos-arm64]` (2026-07-07) — mandatory guards for any PyTorch/MPS run on the M2 box
- Internal: `VERIFICATION_COMMONS.md` (parent design study); chaos-QA calibration arc; situated-harness study; spec-decode tokenizer invariant (Qwen3.5/3.6 vocab 248,320)
- §10 campaign: [RL4HS — Learning to Reason for Hallucination Span Detection (arXiv 2510.02173)](https://arxiv.org/abs/2510.02173) — GRPO + CAPO span-level rewards beat SFT on RAGTruth; [Hallucination Self-Play (arXiv 2607.07993)](https://arxiv.org/abs/2607.07993) — detector/generator co-evolution, small model matches larger on RAGTruth; [Budget-aware Test-time Scaling via Discriminative Verification (arXiv 2510.14913)](https://arxiv.org/pdf/2510.14913) · [Post-Training in 2026: GRPO/DAPO/RLVR survey](https://llm-stats.com/blog/research/post-training-techniques-2026) — the RLVR consensus; [Paladin-mini (arXiv 2506.20384)](https://arxiv.org/abs/2506.20384) — the 79.31 mixed-benchmark claim and why it isn't the LLM-AggreFact top
- Internal: `research/verifier-v0/findings/STREAM_B_DESIGN.md` — substrate survey; the corruption-site checkers that become §10's reward functions
