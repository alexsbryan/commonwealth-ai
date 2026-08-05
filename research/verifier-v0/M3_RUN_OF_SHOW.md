# M3 run of show — the v0 verifier training run

**Status: DRAFT, not started. 2026-08-05.**
Spec: `sovereign/docs/specs/VERIFIER_V0.md` §7 (M3), §1 (targets), §5 (the card).
Hardware evidence: notes `8aad1dbb` (PRO 5000 chosen), `f71dc9a5` (A6000 rejected),
`20167c19` (the memory guard), `6e3f7486` (a Vast machine that cannot be rented).

---

## 0. Bottom line

**One 4B ORPO LoRA run on a rented RTX PRO 5000: 5,856 steps, 62.6 hours,
$41.85.** The money is not the constraint and never was. The constraint is that
we have never trained this model for more than **25 steps**, and every quality
number we hold comes from a **0.8B checkpoint at 5.1% of one epoch**.

So this run buys two things, and the second is the one that matters:

1. A 4B checkpoint to put on the eval card.
2. **The learning curve.** Eleven intermediate checkpoints, scored in parallel
   on a machine that is otherwise idle, so that the next decision — stop at one
   epoch, push past two, escalate to 9B, or go to RLVR — is made on a curve
   instead of on one endpoint.

The second costs nothing extra in GPU dollars or wall-clock. That is the whole
design of this document.

---

## 1. The decided configuration

Everything here is measured, not chosen by preference. Each row cites where.

| | value | why |
|---|---|---|
| GPU | **RTX PRO 5000 Blackwell**, ~$0.67/h, Vast | 38.51 s/it median vs A100 38.06 at 71% of the price; A6000 is 2.1× slower for 60% of the price (note `f71dc9a5`) |
| Model | `Qwen/Qwen3.5-4B` | §2; the size HalluGuard used, so §1's 75.7 is a reproduction bar not a guess |
| Data | `data/orpo-ab` — **93,693 pairs** | Stream A 74,674 + Stream B at share .199 |
| Objective | ORPO LoRA r=32 α=64, β=0.1, lr 1e-4, seq 4096 | `launch_arm.sh:163-177`, unchanged from every prior arm |
| Batch | micro 1 × accum 32 = **32 effective** | the only shape measured at 4B; changing it invalidates every timing here |
| Length bucketing | on | takes consecutive-pair length agreement 15.2% → 92.2% |
| Grad checkpointing | on | free — 231.5 s off vs 231.8 on at 3× memory saving |
| Allocator | `PYTORCH_ALLOC_CONF=expandable_segments:True` | **required**, not optional: without it the 4B OOMs at step 16 on fragmentation (note `8aad1dbb`). Now default for CUDA at `launch_arm.sh:163` |
| Memory guard | demand-based, ceiling = 92% of device less co-tenant | note `20167c19`; without it the run aborts at step 4 on its own cache |
| Steps | 2,928/epoch, **5,856 for 2 epochs** | 93,693 ÷ 32 |
| Checkpoints | `SAVE_EVERY=500` → 11 + final | the ladder; see §4 |
| Seed | 17 | matches every prior arm; keeps losses comparable step-for-step |

**Both fixes are uncommitted.** They reach the pod through `pod.sh sync`, which
rsyncs `scripts/`. If someone commits or reverts in between, re-verify before
arming a 63-hour run.

---

## 2. Cost and schedule

| phase | wall-clock | cost |
|---|---|---|
| rent + sync + provision | ~15 min | $0.17 |
| tokenization (single-threaded, unavoidable today) | ~15–20 min | $0.22 |
| **training, 5,856 steps @ 38.51 s/it** | **62.6 h** | **$41.85** |
| fetch adapters + teardown | ~10 min | $0.11 |
| **total** | **~63.5 h (2.6 days)** | **~$42.4** |

Eval runs on the M2 Max **in parallel** and costs $0.

Against the alternatives for the same run: A100 $58–81, A6000 $53.45 over 5.5
days. Against the spec's own §10 estimate ("~16–24 h ≈ $50–75 on H100-class"),
we are cheaper in dollars and slower in calendar.

---

## 3. Phase 0 — the gates before we spend 63 hours

Each of these has a failing input we can name. Do not skip one because it looks
like a formality; every one of them is here because something like it already
cost a session.

**G1 — the resume path has never been exercised, and this run is 2.6 days long.**
This is the single largest risk in the plan. The longest run we have ever
completed at 4B is **25 steps / 16 minutes**. A Vast instance can vanish; a 63-hour
run that cannot resume is $42 and 2.6 days of nothing.
*Gate:* on a fresh pod, `ITERS=60 SAVE_EVERY=20 STOP_AT=25` → kill the process →
`RESUME=1` → confirm it restarts from step 20 with optimizer and scheduler state,
not from zero, and that `train.log` **appends** rather than truncating
(`StepTimer.__init__` opens `"a"` when `--resume`). Cost ~$0.40, ~35 min.
*Refuses if:* `--resume` finds no checkpoint — that refusal is deliberate
(`train_orpo_trl.py`, `--resume` help text) and is itself part of the test.

**G2 — CLOSED 2026-08-05. It was broken in three independent places** (note
`6d18a622`), every one of which would have surfaced only *after* the 63-hour run.

1. **`fuse_lora_manual.py` could not read a PEFT adapter at all** — it was
   written for mlx-lm-lora. Different filename, different config keys, different
   tensor names, and **transposed A/B storage**. Fixed: `load_adapter()`
   auto-detects and normalises; `resolve_weight_key()` maps module → snapshot key
   or dies naming what it tried. The shape assertion is load-bearing — at r=32
   both transpositions are well-formed, so getting it wrong yields a *fluent,
   silently wrong* model.
2. **Three key namespaces for one checkpoint.** Snapshot has
   `model.language_model.layers.N…`, PEFT recorded `model.layers.N…`, mlx
   recorded `language_model.model.layers.N…`. Result on the real adapter:
   **248/248 modules fused, tensor parity exact (738 → 738, none missing, none
   extra)** — the invariant the manual fuser exists to protect.
3. **`/usr/bin/llama-server` cannot load `qwen35`** (b6153, Jan 2026:
   `unknown model architecture: 'qwen35'`). The converter writes qwen35 happily,
   so conversion succeeds and only *serving* fails — the worst possible ordering.
   Fixed by building llama.cpp b10236 with Vulkan in the `sovereign-vulkan`
   toolbox. **Use `~/dev/llama.cpp/build/bin/llama-server`, never the system one.**

**Withdrawn:** the transformers-4.x conversion constraint at
`score_checkpoint.sh:20-30` does **not** apply here — conversion under 5.14.1
produced a working 441-tensor 4.6 GB q8_0. Do not build a 4.x env on that advice.

*Proof:* fuse 12 s → convert ~30 s → serve → `eval_grounding.py --grammar
--logprobs 10 --no-think`, **49 items scored, 0 parse failures**. The path is the
claim; the BAcc on 49 items is not.

*Residue:* `score_checkpoint.sh` itself is still zsh/Mac/0.8B. The pieces work at
4B on Linux; the wrapper has not been rewritten. Also 6 errors at `-c 8192` —
use `-c 32768` as the Mac path did and confirm `errors: 0` before a real card.

**G3 — contamination.** §3's rule: the calibration and gate banks are never
trained on. `findings/contamination_report*.json` and
`findings/streamA_contaminated_rows.json` exist from the M0/M2 passes.
*Gate:* confirm the pass covers `data/orpo-ab`, not just `orpo-76k`. Stream B was
generated after some of those reports.

**G4 — the eval bar is stated in one column and only one.** Our card will use
`--logprobs` + `--decision-threshold`, which has **no parse failures**, so our
number is comparable to the **excl-pf** column, where HalluGuard-4B scores
**76.76** in our harness (`findings/BASELINES.md:211`) — not the 70.77 strict
column. Write the column into the card template before the run, so the
comparison cannot drift afterwards.

**G5 — baseline the pod.** `cloud/pod.sh up --gpu RTX_PRO_5000` then
`provision` → preflight must return **FIT** with `gpu.vram_floor` passing at
47.27 GB. Add `--skip-machines 51579` on A6000 rentals; not needed for PRO 5000
so far.

---

## 4. Phase 1 — the run, and the ladder that rides along

```bash
cloud/pod.sh up --gpu RTX_PRO_5000 --label m3-4b-ab
cloud/pod.sh sync <id>          # DATA_DIR=data/orpo-ab
cloud/pod.sh provision <id>     # must print FIT
# then, on the pod, via pod.sh probe's shape but with the M3 knobs:
ARM=AB ITERS=5856 MICRO=1 ACCUM=32 SAVE_EVERY=500 \
  MODEL=$TRAIN_ENV/models/Qwen3.5-4B \
  OUT=$TRAIN_ENV/runs/m3-4b-ab bash scripts/launch_arm.sh
```

`cmd_probe` already takes both as environment overrides — `${ITERS:-25}`
(`pod.sh:299`) and `${DATA_DIR:-orpo-76k}` (`pod.sh:363`) — so
`ITERS=5856 SAVE_EVERY=500 DATA_DIR=data/orpo-ab cloud/pod.sh probe <id>` is a
legitimate path and keeps the preflight-before-spending step. Driving
`launch_arm.sh` directly over ssh also works and is what the expandable-segments
run did; it skips the payload preflight, so prefer `cmd_probe`.

**The ladder.** Checkpoints at 500, 1000, … 5500, plus the final. Score these
six on the sampled card (~2,186 items, 11 subsets, ~6 h each on the M2 Max):

| rung | step | ≈ epoch | why this one |
|---|---|---|---|
| 1 | 500 | 0.17 | is it learning at all — compare against the 0.8B's 118-step 68.65 |
| 2 | 1,000 | 0.34 | first real slope estimate |
| 3 | 2,000 | 0.68 | pre-epoch-boundary |
| 4 | **2,928** | **1.00** | **the decision point — see §5** |
| 5 | 4,500 | 1.54 | is the second epoch buying anything |
| 6 | 5,856 | 2.00 | the card |

Six rungs × 6 h = 36 h of M2 Max time against a 62.6 h run. **It fits in
parallel and costs nothing.** Only the final checkpoint gets the full 29,320-row
card (~3.3 days) plus FaithBench-750 and the RAGTruth subset.

**Fetching the rungs:** `cmd_fetch` **excludes `hf/`**, which is exactly where
checkpoints land (`<out>/hf/checkpoint-<step>`). Fetch the adapter files
explicitly — LoRA r=32 adapters are ~100 MB, the full checkpoints are GBs and we
do not want them.

**Two scoring rules, both non-negotiable, both bought with a prior mistake**
(note `f6e44267`):

1. **ONE GLOBAL THRESHOLD, never per-subset.** Fitting 11 thresholds on the same
   data they are scored on was worth **4.3 points of pure illusion** at step 75
   (74.99 tuned vs 70.64 global). Only the global number is shippable, and any
   historical figure produced by per-subset tuning is inflated by roughly that
   much.
2. **Score every rung on IDENTICAL items and compare PAIRED.** The unpaired noise
   floor on 550-item draws is sd 0.017 (2σ ≈ 0.034), which would have called the
   entire 43-step study noise. Paired on the same items the floor is ±0.014. Use
   paired bootstrap CIs, not raw deltas — choosing the ruler before knowing the
   design nearly threw a real result away.

**Every rung reports three numbers, not one:**

- **macro BAcc** across 11 subsets — the leaderboard metric.
- **AUC** — the discrimination metric. This is what M3 has to move; the
  threshold lever is already spent (+3.63, `findings/THRESHOLD_CALIBRATION.md`).
- **TNR at fixed false-alarm budgets (10% and 5%)** — **the product metric.** At
  68.65 BAcc the 0.8B catches **41.3%** of hallucinations at a 10% budget and
  29.6% at 5%. A checkpoint can hit 75.7 BAcc and still be unshippable as a gate.
  Operator directive 2026-08-04: gate on this first.

---

## 5. Phase 2 — stopping rules, decided in advance

Written now so they cannot be rationalized later.

**At rung 4 (step 2,928, one epoch) — the decision point.**

- **BAcc ≥ 74 and still climbing** → run the second epoch as planned.
- **BAcc ≥ 74 and flat between rungs 3 and 4** → **stop.** Bank ~31 h and $21,
  and spend it on RLVR (§10 lever 1, "the single most likely multi-point jump")
  rather than on a second epoch that is not moving.
- **BAcc < 70** → **stop and diagnose.** A 4B at a full epoch scoring below the
  0.8B's threshold-calibrated 68.65 means something is wrong with the recipe, not
  with the budget, and a second epoch will not fix it. §10's sequencing note
  agrees: "If M3 undershoots, fix the base recipe first."
- **AUC flat while BAcc rises** → the gain is threshold movement, not
  discrimination. Report it as such and do not claim progress toward a shippable
  gate.

**At rung 6 (step 5,856) — the card.**

- **≥ 75.7 macro BAcc (excl-pf column) and ≥ 84.0 RAGTruth** → §7's M3 gate is
  met. Proceed to M3.5 (RLVR) per §10's sequencing rule.
- **FaithBench below the small-classifier floor (52.6 HHEM)** → the card says so
  in the headline, not a footnote. §1 is explicit: matching on LLM-AggreFact
  while cratering on FaithBench is *not* best in class.
- **Any rung trades hallucination-catch for specificity** → red line from §1.
  Discard the round.

---

## 6. What we do not know, stated as unknowns

Honesty here is worth more than a confident plan.

- **We have one learning curve and it is inconclusive** (note `f6e44267`). Arm AB
  at steps 75/100/118, paired on 547 identical items: AUC 0.7621 → 0.7745 →
  **0.7694**. *Not monotone* — step 118 came in below 100. Paired bootstrap
  118-vs-75: +0.0073 AUC, 95% CI [−0.0068, +0.0215], P(no improvement) 0.151.
  Max-BAcc rose monotonically (+2.01, borderline). Since AUC is flat while
  max-BAcc rises, what moved is the curve *near the usable operating point*, not
  overall separation. **What it excludes:** any improvement faster than about
  ±0.014 AUC per 43 steps. It does not establish a plateau. This is why the
  ladder spans 5,856 steps rather than repeating a 43-step window.
- **We cannot attribute the 7-point gap.** 68.65 → 75.7 spans three uncompounded
  levers: model size (0.8B→4B), training (5%→200% of an epoch), and data
  (A→A+B). No measurement isolates any of them.
- **Stream B is a genuinely open question, and the two benches disagree.**
  On LLM-AggreFact, B bought **zero discrimination** — proven by exact nesting
  over 2,186 items (`A AND B` reproduces arm A, `A OR B` reproduces arm AB), so
  its +4.91 BAcc is a threshold move that recalibrating arm A matches for free
  (note `7e3758ee`). On FaithBench it **reverses**: arm AB beats arm A by
  +0.0289 AUC, paired bootstrap over the same 750 items, 95% CI
  [−0.0022, +0.0625], P(B no better) 0.037 — borderline (note `51b68b1f`). The
  mix study measured B on the bench where it could not show value. Carry B into
  M3; do not fund a second arm for it.
- **FaithBench is near chance for everyone, including us.** Arm A AUC 0.5693,
  arm AB 0.5982, chance 0.5 — while the same arm A scores 0.7847 on
  LLM-AggreFact under an identical protocol. We *dominate* HalluGuard there
  (+9.3 to +14.6 tnr at matched tpr), but that is a low bar cleared, not a
  product. **The scientific question M3 actually answers: does 4× the parameters
  move a 0.57 AUC?** If the 4B also lands near 0.57, the answer is *data*, not
  scale — and §1 says a model that matches on AggreFact while cratering on
  FaithBench is not best in class.
- **Two epochs is inherited, not derived.** It comes from §10's budget table,
  which took it from the recipe being reproduced. Rung 4 is where we find out.

---

## 7. Known gaps to close before or during

| gap | impact | cost to fix |
|---|---|---|
| `--resume` never exercised (G1) | a lost 63-hour run | ~$0.40, 35 min |
| `score_checkpoint.sh` is 0.8B/Mac-only (G2) | an unscoreable ladder | unknown until tried — do it first |
| tokenization not cached | 15–20 min per pod | one rsync of the HF arrow cache; irrelevant for M3, material for probes |
| `cmd_fetch` excludes `hf/` (`pod.sh:401`) | the ladder's checkpoints stay on a destroyed pod | fetch adapter files explicitly, before teardown |
| VRAM floor still 46 | excludes cards that would work | the A6000 run proved 44.43 GB is sufficient — floor can drop to ~44, though no cheaper card has yet beaten the PRO 5000 on $/epoch |

---

## 8. Sequence

1. **G2** (4B scoring path) — highest risk of a silent blocker, cheapest to
   discover. Uses an adapter we already have.
2. **G3, G4** — no GPU needed.
3. **G1** (resume rehearsal) — one short pod, ~$0.40.
4. **G5** → arm the run.
5. Ladder rungs scored as they arrive; **decision at rung 4**.
6. Full card on the final checkpoint; then M4 (GGUF, opt-in judge slot) or M3.5
   (RLVR), per §5's stopping rules.

Total GPU spend to a decision at rung 4: **~$22**. To a full card: **~$43**.
