# D2 — the 4B instrument shakeout. **This is not the verdict.**

**Read this as instrument evidence and nothing else.** It is 100 pairs on a
**4B**, and the order names the 36B as the decision-grade model. Nothing below
settles H2b. What it does settle is whether the instrument works, and it
surfaced one thing about the *label supply* that changes what D3 should run.

---

## The headline

**§5 H2's value unit is dead on this calibration set, and the reason is not the
model.** All **4,207** questions in
`native_grounding_calibration.jsonl.gz` open **"Is it true that …?"** —
100.0%, measured, not sampled. They are propositions. §5 H2's unit is *"the
specific value the EVIDENCE gives"*, and a proposition has no specific value to
extract.

So the value unit's arm B refused on **100 of 100** pairs, absent and answerable
alike. The order's stop condition is 80%. The gate stopped, as written.

The same 100 pairs re-run with a **verdict unit** (YES / NO / NONE — the same
counterfactual asked in the form this data can answer) clears both stop
conditions and produces a live, non-degenerate instrument. That variant is the
recommendation for D3.

---

## 1. The two runs, side by side

BeefyMac (M2 Max, 64 GB), **Qwen3.5-4B-Q6_K**, greedy, seeds pinned, the same
deterministic 100-pair stratified slice (54 answerable / 46 absent, 97 distinct
corpora), daemon resident.

| | **value unit** (§5 H2's) | **verdict unit** |
|---|---|---|
| arm A | 48 value / 49 refusal / 3 garbage | 85 value / 15 refusal / 0 garbage |
| arm B | **0 value / 100 refusal** | 78 value / 22 refusal |
| **arm B refusal rate, ABSENT pairs** | **1.000** | **0.196** |
| arm P | 6 value / 94 refusal | **100 value** / 0 refusal |
| **P1 leak rate** (answerable) | **0/54 = 0.000** | **11/54 = 0.204** |
| within the order's predicted 10-40% band | no | **yes** |
| arm A raw values | `NONE` ×49, `Yes/YES/yes` ×36, 15 others | `YES` ×82, `NONE` ×15, `NO` ×3 |
| arm B raw values | `NONE` ×100 | `YES` ×64, `NONE` ×22, `NO` ×14 |
| determinism (arm A re-decoded) | **10/10 stable** | **10/10 stable** |
| gate outcome | `CouldNotJudge` (exit 1) | `Killed` (exit 3) |

Artifacts: `h2b_{arms,scores,verdict}.4b_shakeout*.{jsonl,json}`.

## 2. What the shakeout was asked to verify — four answers, not two

| Question the order asked | Answer |
|---|---|
| Do the arms produce typed outcomes? | **Yes.** All three types appeared in real data: 3 arm-A `garbage` rows in the value run — streams that ran to the 24-token budget without terminating — and both `value` and `refusal` throughout. The typing is structural (`finished_eog`), not a keyword list. |
| Does the leak detector fire on plausible pairs? | **Not on the value unit** (0/54, because arm P refused 94/100 — there is no value to recall). **Yes on the verdict unit**: 11/54, and the flagged pairs are ones where arm P asserted a verdict that the withheld supporting passage literally carries. |
| Do both statistics emit? | **Yes.** `evidence_dependence` on 97/100 (value) and 100/100 (verdict) — the 3 gaps are the garbage rows, which are excluded and counted rather than folded to 0. `value_margin` emitted on **100/100** in both runs. |
| Does determinism hold across 2 runs? | **Yes, 10/10 re-decodes byte-identical**, both units. Greedy + a fixed prompt is byte-stable on this host, as designed. |

**Both determinism seams were also checked, and one was broken.** Stage 2
rescoring the same arms file reproduces `h2b_scores.jsonl` byte-for-byte. Stage 3
(`--from-scores`, no model at all) did **not** at first reproduce stage 2's
verdict: the repeat-check counts lived only in the arms file, so a replay
honestly reported "determinism NOT re-checked" and its verdict differed by two
lines. That is correct §18.3 behaviour and a broken seam at the same time. The
counts now ride the scores file, and both seams are exact. A seam that cannot
reproduce the artifact it replays is not a seam.

## 3. The stop condition earned its keep — read this part

Strip the stop condition out of the value-unit run and here is what the gate
would have reported:

| | value unit, **with the stop condition suppressed** |
|---|---|
| `evidence_dependence` AUROC | 0.8796 |
| `rerank_margin` AUROC (H1's bar, same pairs) | 0.8712 |
| **combined** AUROC (w = 1.5, fitted on calibration) | **0.9164** |
| combined − margin | **+0.0452** |
| kill-bar clause A (≥ +0.02) | **true** |

That is a clean pass. **It is also worthless.** Arm B is the constant `NONE`, so
`1 − equiv(A, B)` reduces to *"did arm A assert anything?"* — a one-bit
abstention detector, cheaper than the reranker and a different hypothesis from
the one H2b is about. The stop condition is the only thing standing between that
number and a `EarnsPhase2Combined` verdict in a committed artifact.

This is the characteristic failure this workspace names: a plausible,
well-formed, exit-0 result that is wrong. It is recorded here because the gate
catching it is the single most useful thing D2 produced.

## 4. The verdict unit, on the 4B — killed, and honestly so

| | held-out, non-leaked (n = 44: 21 answerable / 23 absent) |
|---|---|
| `evidence_dependence` AUROC | **0.4731** — chance |
| `rerank_margin` AUROC | 0.8592 |
| combined AUROC (w = 3, fitted on calibration @ 0.8498) | 0.7930 |
| combined − margin | **−0.0663** |
| pearson(dependence, margin) | −0.1288 |
| **P2** mean dependence: answerable 0.3810, absent 0.4348 | **delta −0.0538**, bar 0.15 — **FAILS** |
| naive ceiling, always-answerable | 0.4773 |

**Do not read this as H2b's verdict.** Three reasons, in order of weight:

1. **It is a 4B.** The order's P1 says the leak band is "10-40% on a 36B …
   lower on the 4B", and the 4B already leaks 20.4% — a 36B's priors over
   encyclopedic philosophy are the thing the counterfactual is supposed to
   probe, and they are not represented here.
2. **n = 44 held out.** A 100-pair slice was sized to shake out the instrument,
   not to estimate an AUROC. The confidence interval on 0.4731 at n = 44 is wide
   enough to contain most of the range that matters.
3. **The 4B has a yes-bias that swamps the counterfactual.** Arm P answered
   `YES` to **82 of 100** propositions with no evidence at all, and arm A
   answered `YES` to 82 of 100 with it. A model that says yes regardless cannot
   express evidence dependence, and that is a property of this model rather than
   of the mechanism.

The **surface** flip rate before the clusterer and before the leak exclusion was
answerable 0.463 / absent 0.304, delta **+0.159** — just over P2's bar. After
excluding the 11 `parametric_known` pairs and restricting to held-out, it
inverts to −0.054. That reversal is small-n noise, and it is also exactly the
circularity guard working: the leak exclusion is defined off arm P, never off
arm A, so it is free to move the number in either direction. An exclusion rule
that could only help would not be one.

## 5. Cost, measured

| | value unit | verdict unit |
|---|---|---|
| 3 arms, wall clock, p50 | **3,196 ms/pair** | 3,084 ms/pair |
| 100 pairs, total | 353 s | 349 s |
| arm A prompt tokens, p50 | 1,616 | 1,616 |
| arm B prompt tokens, p50 | **125** | 125 |
| arm P prompt tokens, p50 | **111** | 111 |

**The order's cost claim holds and can be checked rather than estimated.** Arms
B and P together prefill ~236 tokens against arm A's 1,616 — the counterfactual
costs about **15% more prefill** than the evidence-conditioned decode alone, not
double. Decode is 1-2 tokens per arm and is free.

**Extrapolation to the 36B, stated as an estimate and not as a measurement:** at
3.1 s/pair on a 3.5 GB model, a 30 GB model at the same prefill-bound shape is
plausibly 6-10× slower, i.e. **19-31 s/pair**, i.e. **5.3-8.6 h for 1,000
pairs**. That is over the order's 2-3 h budget. D3 should measure the real rate
in its first ten minutes and size the run to the window; `--resume` and
row-at-a-time writes mean a truncated run loses only its tail.

## 6. What D3 should run, and why

1. **Primary: `--unit verdict`.** It is the only unit this label supply can
   answer, it clears both stop conditions on a 4B, and its leak rate already
   lands inside the order's predicted band.
2. **Control: `--unit value`, ~100 pairs.** Confirms the collapse reproduces at
   36B rather than being a 4B artefact. Cheap, and it is the difference between
   "the value unit is dead" and "the value unit was dead on the small model".
3. **The H2 sampling smoke** (`h2-smoke`, `h2/FINDINGS.md` §5 B) — minutes, and
   it settles whether the spec's Appendix A stays closed.

**A named deviation, carried forward.** Arm P is not in the order; it is a third
arm added because the order's arm B cannot serve as a leak detector (arm B still
instructs *"reply NONE if the EVIDENCE does not state it"*, which an empty
evidence block satisfies on every pair — see the 0/54 above, which is that
prediction confirmed). The seat provisionally endorsed it. Arm B remains the
gate's statistic, unchanged and verbatim.

---

Run provenance: 2026-08-08, BeefyMac (macOS, 64 GB, Apple M2 Max), daemon
resident throughout. Generator **Qwen3.5-4B-Q6_K** — a named substitution for
the 36B, and the whole reason this document is titled "shakeout". Reranker
Qwen3-Reranker-0.6B-Q8_0, loaded only in stage 2, never alongside the generator.
The calibration set is read-only; no bank was run and no probe generated.

Reproduce (needs a generator GGUF, ~6 min for 100 pairs):

```
svrn bench flywheel h2b-arms --model <generator.gguf> --limit 100 \
  --repeat-every 10 [--unit verdict] \
  --out-dir sovereign/bench/calibration/h2b --out-name <arms.jsonl>

svrn bench flywheel h2b-gate --arms sovereign/bench/calibration/h2b/<arms.jsonl> \
  --rerank-model <qwen3-reranker-0.6b-q8_0.gguf>
```

Reproduce the verdict from frozen scores (no model, seconds):

```
svrn bench flywheel h2b-gate \
  --from-scores sovereign/bench/calibration/h2b/h2b_scores.4b_shakeout.verdict.jsonl
```
