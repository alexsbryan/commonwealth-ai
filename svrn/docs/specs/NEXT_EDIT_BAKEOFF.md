# Next-edit bakeoff — the build-vs-adopt decision, made decisively

Status: **design, pre-registration draft. Written 2026-08-05 against `main`
@ 92602386.** Companion to [`NEXT_EDIT.md`](../NEXT_EDIT.md) (the shipped
feature, both lanes) and modelled on
[`VERIFIER_V0.md`](./VERIFIER_V0.md) §0/§5 — its adopt-vs-build discipline
("the eval card decides, not this section") is the parent of everything
below.

**The one-paragraph project.** Five open-weight next-edit models now exist,
each certified on its own benchmark, and the published rulers are mutually
incommensurable — two vendors reach opposite verdicts about the same model.
No public benchmark scores *silence*, which is half of what a production
next-edit lane does. So the literature cannot answer whether to adopt or
train; only a bakeoff can. This spec builds one golden set in four strata
(harvested, session-synthesized, real-telemetry, uncontaminated), scores it
on a four-tier deterministic-first ruler with a wrong-fire operating point,
validates the instrument against a published number *before* trusting any
result, and pre-registers the decision rule that crowns a champion or
authorizes a training run.

---

## 0. Why this cannot be settled by reading

Four published rulers, no common unit:

| Source | Metric | Numbers as published |
|---|---|---|
| Sweep (blog) | whitespace-agnostic exact match, 5-benchmark avg, **their own banks** | Sweep-7B 81.28 · 3B 72.12 · 1.5B 67.82 · Qwen2.5-Coder-7B 55.62 · Mercury 54.09 · **Zeta 43.27** · **Instinct 25.30** |
| Continue (blog) | LLM-judge score (Claude), ~0–5, **their own eval** | **Instinct 3.877** · Zeta 3.735 |
| Zed (blog) | acceptance rate, **no public eval at all** | Zeta2 "30% better than Zeta1" |
| CUHK (arXiv 2508.10074) | EM / Partial / Position / judge, **210 samples**, public + Apache-2.0 | Claude 4 Opus 45.71 / 67.62 / 80.48 · Qwen2.5-Coder-32B 51.43 / 52.38 / 73.81 · GPT-4o 23.33 / 39.05 / 57.14 |

**Sweep ranks Instinct last of everything measured; Continue publishes
Instinct as the best open next-edit model in the world.** Both cannot be
right, and neither published a ruler the other could run. That contradiction
is not noise to be averaged — it is the direct evidence that adopt-vs-build
is currently undecidable from outside, and the reason this document exists.

Three further holes, each of which a production lane falls straight through:

1. **Nobody scores silence.** Every published benchmark scores positives
   only. `NEXT_EDIT.md` §1 fixes the failure cost as *a wrong edit
   proposal — precision-critical*; a benchmark with no negatives cannot see
   the failure the feature is designed around. Our own `gen` bank already
   carries 20 negatives out of 60 and is, on this axis, ahead of the field.
2. **The largest public benchmark is 210 examples.** CUHK is a
   comparability anchor, not a heroic test: 30 per language across 7
   languages, drawn from top-100-starred repos, restricted to commits with
   ≥2 chunks of ≤5 lines each, total span ≤80 lines, **additive-only**.
3. **Everyone trained on public GitHub.** Which makes the obvious golden
   set — mine popular permissive repos — a contaminated one. See §3.

## 1. The candidates

Eight arms. Every model-bearing arm is Apache-2.0 and locally servable; two
of the three wire formats they need are already implemented.

| Arm | Size / resident | Base | Wire format | Status here |
|---|---|---|---|---|
| **rule lane only** | 0 | — | n/a | the honest floor — `NEXT_EDIT.md` §2 argues the canonical case needs no model at all |
| rule lane + casing sub-lane | 0 | — | n/a | the deferred §8.4 sub-lane, stubbed for this test |
| **Mellum2-12B-A2.5B** | 10.88 GB @ q6_k | — | `region_instruct` | incumbent (`models.toml:473-484`); 29/30 useful, 0 wrong, p95 1807 ms |
| **Sweep-next-edit-1.5B** | ~1.5 GB @ Q8_0 | Qwen2.5-Coder | `sweep` | measured: 22/30 useful, 0 wrong, p95 1112 ms |
| **Sweep-next-edit-v2-7B** | ~8 GB | Qwen2.5-Coder | `sweep` (verify) | **not yet evaluated here** — their 81.28 tier |
| Sweep-next-edit-0.5B | ~0.5 GB | Qwen2.5-Coder | `sweep` | latency floor probe |
| **Zeta-2 / 2.1** | 8B | Seed-Coder-8B-Base | `zeta2` | format built (`next_edit_model.rs:515`), never run |
| **Instinct** | 7B | Qwen2.5-Coder-7B | chat (new adapter) | SeleKT 5%-param SFT; ships `instinct-data` publicly |

Two calibration arms that are not candidates but without which no result is
interpretable (§5): **Qwen2.5-Coder-1.5B-base** (untrained floor) and a
**frontier model** through the same harness (ceiling).

`sweep-next-edit-v2-7B` is the headline discovery of the survey. Sweep's
blog benchmarks a 7B at 81.28% against their 1.5B's 67.82% and only
open-sources the small one in the post — but the org publishes
`sweep-next-edit-v2-7B` (updated May 14), Apache-2.0. If that model holds
its tier on a neutral ruler it plausibly beats Mellum2-12B on quality *and*
residency, and the bakeoff is over on day one.

## 2. The golden set — four strata, four different jobs

One dataset cannot answer this. Each stratum exists to close a specific
validity threat, and the strata are scored separately and reported
separately — never pooled into one headline number.

### Stratum 1 — HARVESTED (scale; labels free by construction)

`gym/next-edit/harvest.py` generalized to arbitrary repositories. A commit
whose hunks induce a coherent repeated intent is an episode: replay the
first *k* hunks as edit history, send the mid-edit document, hold out the
remainder as ground truth. **The label is the actual subsequent hunk** — no
teacher, no judge, no preference model.

- Cost to generalize: `REPO` is hardcoded at `harvest.py:32` and read by
  `git()` at `:235`. Roughly three lines plus a repo-list driver.
- Target volume: **4,000–6,000 episodes**, 10+ languages, quota-balanced.
- Threat it does not close: a commit is a *finished* intent; a next-edit
  request is an *unfinished* one. Stratum 2 exists for that.

### Stratum 2 — SESSION-SYNTHESIZED (validity; where the strong model earns its keep)

The proxy gap between "diff" and "editing session" is the central validity
threat, and it is exactly where a strong code model belongs. Given a
harvested commit, a strong model **reconstructs a plausible editing order,
segments it into units, and chooses a stopping point** — turning a finished
diff into an interrupted session. CUHK did precisely this with GPT-4o mini
(labelling sequences where "the final edit is a logical continuation of the
preceding ones"), so the method is established, not invented here.

**The discipline, inherited verbatim from `VERIFIER_V0.md` §3: fabricator,
never oracle.** The model may order, segment, truncate, and reject an
episode as incoherent. It may **never author the answer** — the answer
remains the held-out hunk, fixed by construction before the model is
consulted. A model that mis-orders produces a discarded episode, not a bad
label. Every synthesized session is re-validated by the rule-lane replica
(`harvest.py:42-165`) and the consult-gate replica (`gen/author.py`), so a
synthesizer↔engine divergence fails loudly whichever side is wrong.

Second job for the same model: **authoring negatives at scale**. The
`gen` bank's 20 hand-written negatives are the shape to reproduce — plausible
edits that must draw silence, string-literal traps, exhausted patterns.
Target: negatives are **≥40% of the set**, because silence is ~half the
product and no competing benchmark measures it.

### Stratum 3 — REAL SESSIONS (measures how much Stratum 1 lies)

`continuedev/instinct-data` — **9,044 rows, Apache-2.0, from real IDE
telemetry**, with published test splits (221 ts / 129 py / 106 c / 114 rust
/ 124 java = 694 held out). This is the one public asset with genuine
editing-session provenance, and it is the instrument for the question
Stratum 1 cannot answer itself.

**Its use is rank correlation, not absolute score.** Compute each arm's
ranking on Stratum 1 and on Stratum 3; if the two orderings agree, commit
mining is a valid proxy and we may scale it freely. If they diverge, we have
learned the most valuable fact in the entire exercise — and it is the
FaithBench lesson from `VERIFIER_V0.md` §1 restated for code: a small model
can win the headline distribution and collapse off it.

Two caveats stated up front, not discovered later. Instinct is trained on
this data — **its number here is contaminated and is reported as such**,
excluded from the rank correlation. And only the TypeScript rows are
authentic; the other four languages were synthetically translated (via
Qwen3-Coder-30B), so they are segregated and reported separately.

### Stratum 4 — UNCONTAMINATED (the decisive slice)

Every other stratum is only as trustworthy as §3. This one is built to be
provably outside every candidate's training set:

- **(a) Our own history.** `commonwealth-ai` plus the operator's private
  repos — never public, therefore in nobody's corpus, *and* the actual
  deployment distribution. The highest-value slice in the set.
- **(b) Recency + obscurity cut.** Permissively-licensed repos from
  Codeberg / GitLab / sourcehut and low-star GitHub, restricted to commits
  **dated after 2026-07-01** — past the latest candidate release
  (Sweep-v2-7B May 14, Mellum2 June). Recency is the cheap, robust defense;
  obscurity is the backstop.

**The pre-registered champion bar is decided on Stratum 4 alone.** The other
three explain the result; this one settles it.

## 3. Contamination — the incumbents' training data is the contaminant

Sweep scraped "the most popular permissively-licensed repos on GitHub over
the past year with a commit count filter." CUHK's benchmark draws from
top-100 starred repos. Zeta2 collected from opt-in open-source repos.
**Building the golden set the obvious way scores Sweep inflated and calls it
a champion.**

Defenses, all three run:

1. Stratum 4's recency + obscurity cut (§2) — structural, not statistical.
2. An n-gram/embedding dedup pass between every stratum and every
   candidate's plausible corpus, reusing
   `research/verifier-v0/scripts/contamination_pass.py` — the verifier
   project already built and debugged this.
3. **The collision count is published on the card**, target 0 after
   filtering, exactly as `VERIFIER_V0.md` §3 requires. A bakeoff that does
   not report its contamination is a marketing document.

## 4. The ruler — deterministic first, judge last, silence scored

Four tiers, applied in order. A prediction is scored by the highest tier it
satisfies, and each tier's count is reported separately — no single blended
number.

1. **Exact** — byte-identical to the held-out hunk.
2. **Normalized** — whitespace- and line-ending-agnostic (Sweep's own
   ruler, so their published numbers stay comparable to ours).
3. **Mechanically defensible** — fails 1–2 but passes tree-sitter parse
   validity **and** `verify_pattern` (`next_edit_model.rs:934-1005`) **and**
   the already-applied / noop checks. This tier is ours alone; no competing
   benchmark has a mechanical notion of "different from ground truth but not
   wrong."
4. **Judge** — applied *only* to the residual that passes 3 and fails 2,
   and quoted only after judge↔human agreement is measured on a 100-case
   calibration slice. A judge whose agreement is unmeasured is a
   could-not-judge, not a score.

**The primary metric is not accuracy.** It is **useful-fire rate at a
bounded wrong-fire rate**, which is the operating-point discipline from
`VERIFIER_V0.md` §1/§5.4 transplanted to this seat:

- **wrong fire** — the lane emitted edits that are neither ground truth nor
  tier-3 defensible. This is the number the product is precision-critical
  about.
- **useful fire** — emitted edits at tier 1–3.
- **correct silence** — declined on a negative episode.
- **missed fire** — silent on a positive episode.

Ship floor stays as pre-registered in `NEXT_EDIT.md` §6: **wrong ≤ 5% of
fires** (GM3). The *champion* bar is **≤1%**, which 5,000 episodes can
resolve and 30 could not. Both are reported; the tighter one is new and
named as new, not silently substituted for the old.

Secondary axes, all on the card: p50/p95 wall latency per fleet tier,
resident bytes at the shipping quantization, license, and **FIM
non-regression** on `gym/fim/` (60 cases) — because one resident model
serves both seats and a next-edit win bought with a FIM loss is not a win.

## 5. Validate the instrument before the result

ARCH §18.4, and this project has already been bitten here: `NEXT_EDIT.md`
§9b records `sf06`, where a genuinely corrupted rewrite (`sock_ FD`,
`,- scratch`) was scored **correct** by the gen bank's count-ruler. That
blind spot is in the instrument we would otherwise scale by 80×. Four
checks run before any candidate number is quoted:

1. **Reproduce a published number.** Run Qwen2.5-Coder-32B through our
   harness on CUHK's public 210 and confirm ~52.38 Partial Match. A miss
   means our harness is wrong, not the model — the same move that caught
   the verifier's parse-policy artifact at M0 (`BASELINES.md`).
2. **Mutation-test the scorer.** Corrupt known-correct predictions in ways
   tier 3 must catch and ways it must tolerate; verify each verdict flips as
   designed. `sf06` is a real, already-found case and goes in as a fixture.
3. **Floor check.** Qwen2.5-Coder-1.5B-base, no next-edit training. If it
   scores near the field, the bank is too easy and the result is void.
4. **Ceiling check.** A frontier model through the identical harness. If the
   ceiling is not well clear of the field, the bank is measuring something
   other than next-edit skill.

Four verdicts throughout, never two: **passed · failed · could-not-judge ·
never-ran.**

## 6. The pre-registered decision rule

Written before the first run. A miss is a finding; the bars do not move.

**ADOPT — the bakeoff ends, we ship the winner.** Some candidate, on
**Stratum 4**, satisfies all of:

- useful-fire within **3 points** of the best arm in the field, and
- wrong-fire **≤1%** of fires, and
- correct silence **≥95%** on negatives, and
- p95 **≤1500 ms** on the slower fleet tier, and
- resident **≤4 GB** at shipping quantization, and
- **no FIM regression** on `gym/fim/`, and
- Apache-2.0 or equivalent.

Mellum2-12B is the thing being displaced: any arm clearing this bar returns
roughly **9 GB of residency on every fleet node** — on the slot that
competes with the 35B primary — and that is the user-visible win, stated in
advance.

**TRAIN — authorize the run described in §7 of the follow-on spec.** Either:

- the best adopt candidate trails the **frontier ceiling by >10 points** of
  useful-fire on Stratum 4 (the gap Sweep's own recipe — 100k commit-mined
  examples, 8×H100×4h SFT, then 2,000 RLVR steps — demonstrably closes), or
- **Spearman rank correlation between Stratum 1 and Stratum 3 < 0.7**,
  meaning the field is fragile off-distribution and adoption buys a model
  that will not hold up on our users' repos, or
- no candidate clears the wrong-fire bar, which is the axis no amount of
  prompt work fixes.

**NEITHER — cut the model lane.** No arm beats *rule lane + casing sub-lane*
by a margin justifying its residency and latency. This outcome is live, not
a formality: `NEXT_EDIT.md` §2 already argues the canonical case needs no
model, and the casing category was deferred precisely because a
deterministic sub-lane is the better home. If the models are only winning on
episodes the rule lane already owns, the honest answer is to build the
sub-lane and delete the consult.

## 7. Phase 0 first — this may be over in an afternoon

Before building any of §2, run all eight arms against the **two rulers that
already exist**: our `gym/next-edit/gen/` (60 cases) and CUHK's public 210.
That is ~2,200 predictions, well under an hour of compute, and it needs only
the daemon-free runner from §8.

Two outcomes, both valuable:

- **The field separates decisively and one arm dominates both rulers** —
  particularly if that arm is `sweep-next-edit-v2-7B`. Confirm on a small
  Stratum 4 slice and crown it. The full golden set is never built, and the
  question cost an afternoon.
- **The field does not separate, or the two rulers disagree** — which is
  the outcome the §0 contradiction predicts. Then the golden set is
  justified by evidence rather than by anticipation, and Phase 0's
  disagreement is the first result on the card.

Refusing to build 5,000 episodes before checking whether 270 settle it is
the same discipline as `VERIFIER_V0.md` M0: measure the adopt candidates on
your own harness *first*, and let the measurement authorize the spend.

## 8. What gets built

Ordered by dependency; sizes are estimates against known analogues.

| # | Item | Size | Notes |
|---|---|---|---|
| 1 | **daemon-free scorer** | ~1 day | the blocker for everything, incl. Phase 0. Lift `next_edit.rs` + `next_edit_model.rs` into a leaf crate with a `[[bin]]` — both are std-only with zero external imports — or replicate `verify_pattern` in Python. `research/verifier-v0/scripts/score_checkpoint.sh` is the recipe one domain over. |
| 2 | **multi-arm runner** | ~1 day | llama-server per candidate; `region_instruct` / `zeta2` / `sweep` formats already exist at `next_edit_model.rs:453/:515/:580`; Instinct needs a chat adapter. |
| 3 | **Phase 0 run + report** | hours | §7. May terminate the project here. |
| 4 | `harvest.py --repo` + repo-list driver | ~0.5 day | `harvest.py:32`, `:235`. |
| 5 | **session synthesizer** | ~2 days | §2 Stratum 2; strong model orders/segments/truncates, re-validated by both existing replicas. |
| 6 | negative authoring at scale | ~1 day | same harness, `gen` bank's 20 negatives as the shape. |
| 7 | contamination pass | ~0.5 day | reuse `research/verifier-v0/scripts/contamination_pass.py`. |
| 8 | instrument-validation suite | ~1 day | §5, incl. the `sf06` fixture. |
| 9 | full sweep + card | overnight | see below. |

**Cost. The heroic version is cheap here, and that is the point.** Next-edit
rollouts are ~70±20 output tokens at p95 1.1–1.8 s — an order of magnitude
shorter than a verifier rollout with thinking. 5,000 episodes × 8 arms =
40,000 predictions ≈ **4 hours wall-clock at 4-way concurrency**. A full
re-sweep after any change is an overnight run, not a week. Compare
`VERIFIER_V0.md` §10, where a single eval card cost 6 hours.

## 9. Risks and validity threats

- **Contamination inflating an incumbent** — the top threat, addressed
  structurally in §3 rather than statistically. If Stratum 4 cannot be built
  large enough, the champion bar is not met and the honest verdict is
  could-not-judge.
- **The synthesizer teaching the test its own biases** — mitigated by the
  fabricator-never-oracle rule and by re-validating every synthesized
  episode against two independent replicas. A synthesizer that drifts
  produces discarded episodes, not wrong labels.
- **Our tier-3 ruler flattering our own posture.** `verify_pattern` is our
  invention; a candidate that happens to fail it for stylistic reasons is
  penalized on a ruler no vendor trained against. Mitigation: tiers 1–2 are
  reported standalone and are the numbers used for any external comparison;
  tier 3 is reported as a separate column and never folded into a headline.
- **Instinct's chat format disadvantaging it.** Three arms have native wire
  formats here and Instinct does not. Mitigation: build its adapter from its
  model card, and report a per-arm "format fidelity" note — a model
  under-served by our prompt is a could-not-judge, not a loss.
- **Rank correlation on a 694-row Stratum 3** — thin. Mitigation: report the
  confidence interval, and treat a borderline correlation as could-not-judge
  rather than as evidence of validity.
- **`[models.edit]` is absent from the live `~/.sovereign/config.toml`**
  (the key was `[models.fim]` when this was written; the old spelling is
  still accepted as a deprecated alias), so the model lane is dark on this
  box today and `next_edit_gen_eval.py` exits at its probe. Fix before
  Phase 0. Since 2026-08-07 there is a second way out — the next-edit
  fallback serves the lane off the resident chat primary under
  `SOVEREIGN_NEXT_EDIT_FALLBACK` (`NEXT_EDIT.md` §2a) — but that is a
  different arm, not a substitute for configuring the candidate under test.

## 10. Non-goals

Not a training run — this spec authorizes one or refuses it, and the recipe
lives in a follow-on. Not a leaderboard submission. No new product surface:
every arm is measured through the shipped route's contract, because a model
that only wins outside the guards has not won anything the user will feel.

## 11. Sources

- [Sweep — Open sourcing a 1.5B Next-Edit Autocomplete model](https://blog.sweep.dev/posts/oss-next-edit) — recipe: Qwen2.5-Coder base, ~100k commit-mined entries from popular permissive repos, SFT 8×H100×4h, RL 2,000 steps with tree-sitter validity + size-regularization rewards, ~30 prompt formats searched genetically; data withheld
- [sweepai on HF](https://huggingface.co/sweepai) — `sweep-next-edit-v2-7B` (May 14), `-1.5B` (Jan 22), `-0.5B` (Jan 28), Apache-2.0 · [1.5B card](https://huggingface.co/sweepai/sweep-next-edit-1.5B)
- [Zed — We Rebuilt Zeta from the Training Data Up](https://zed.dev/blog/zeta2) — ~100k opt-in examples, SFT, DPO stated as future work, **data explicitly not released**, acceptance rate +30% vs Zeta1 · [zed-industries/zeta-2](https://huggingface.co/zed-industries/zeta-2) — Apache-2.0, Seed-Coder-8B-Base, SPM prompt with git-merge-style region markers
- [Continue — Introducing Instinct](https://blog.continue.dev/instinct) — Qwen2.5-Coder-7B, SeleKT 5%-param SFT, 5 epochs, ~4,000 real Continue-team edits + synthetic translation via Qwen3-Coder-30B; judge 3.877 vs Zeta 3.735 · [continuedev/instinct](https://huggingface.co/continuedev/instinct) Apache-2.0 · [continuedev/instinct-data](https://huggingface.co/datasets/continuedev/instinct-data) — **9,044 rows, Apache-2.0, IDE telemetry**, per-language train/test splits
- [Next Edit Prediction (arXiv 2508.10074)](https://arxiv.org/pdf/2508.10074) · [lurf21/NextEditPrediction](https://github.com/lurf21/NextEditPrediction) — Apache-2.0 data + code; 210 samples / 7 languages from top-100-starred repos; 3,211-sample CommitPackFT-derived training set; GPT-4o mini episode labeling; EM/Partial/Position/judge metrics
- [JetBrains Mellum2](https://www.marktechpost.com/2026/06/02/jetbrains-releases-mellum2-a-12b-moe-model-for-fast-specialized-tasks-in-multi-model-ai-pipelines/) — 12B MoE, Apache-2.0, the incumbent at `models.toml:451-495`
- Internal: `NEXT_EDIT.md` §1 (precision-critical failure cost), §2 (rule lane needs no model), §6 (GM1–GM5), §9b (V0 verifier, the Sweep bakeoff, `sf06`) · `VERIFIER_V0.md` §0 (adopt-vs-build discipline), §3 (fabricator-never-oracle; contamination), §5 (eval card) · ARCH §18.4 (validate the instrument)
