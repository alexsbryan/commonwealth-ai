# Retrieval system audit — 2026-08-04

**Status: COMPLETE.** `scripts/sovereign-ci-bench.sh --no-synth` finished
2026-08-05T02:17:25Z, exit 1, `VERDICT: FAIL`, 16,718s against a 14,400s
budget.

**Coverage warning — read this before reading anything else.** Of 14 HARD
lanes: **3 passed, 5 failed, and 6 never ran.** The six that never ran
(`governance-gate`, `mechanism-gate`, `multiturn-gate`, `search-gym-gate`,
`knowledge-gym-gate`, `agent-coding-gate`) were starved of budget by one long
advisory lane — see I1. **They are never-ran, not passed.** This audit says
nothing about tool-use judiciousness, agentic coding, multi-turn degradation,
reasoning fidelity, or governance. Treating their absence as health is the
exact failure this document exists to prevent.

## What produced this

Three independent measurement passes on 2026-08-04, all against the daemon's
live indexes with `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL` as chat and
`qwen-embedding-0.6b` as embed:

| pass | what it ran | why it is here |
|---|---|---|
| synth baseline-check | `bench all --synth`, 6 of 14 banks before being stopped | the SOFT/non-gating axis; surfaced D1 and D2 by accident |
| paired prod-pipeline | `eval run --prod-pipeline` ×2 on the same 12-question bank | deliberate determinism control |
| ci-bench `--no-synth` | `scripts/sovereign-ci-bench.sh`, HARD lanes | the gating axis; surfaced D3 and D4 |

Artifacts: `target/bench-baseline-check/*.json`, `target/ci-bench/*.json`, and
two paired runs under the session scratchpad.

**Read every entry with its confidence.** Several are single runs against
baselines 18–79 days old that record no model id. That is a lead, not a
verdict, and the table says which is which.

## Defect ledger

| id | defect | user-visible? | confidence | evidence |
|---|---|---|---|---|
| D1 | Off-topic corpora admitted into the evidence pool in bulk | **yes** | **confirmed** (6/6 questions, two independent runs) | below |
| D2 | Which off-topic corpora get admitted varies run to run (`--synth`) | indirectly | **confirmed** (paired replication) | below |
| D3 | Expected source articles lost on wikipedia | **yes** | **suspected** — one run vs a 19d baseline | below |
| D4 | Chunk selection unstable under `--isolate` while sources are stable | no (bench-facing) | **suspected, likely noise** — bidirectional | below |
| D5 | Judge collapses its own failures into the score | no (bench-facing) | **confirmed** by code read + 1 occurrence | below |
| D6 | Intent routing sends commitments and feelings to `knowledge_query` | **yes** | **strong** — 32 misroutes with consistent shape, correlated with embed coverage | below |
| D7a | ~~>50% of summary claims unsupported~~ **WITHDRAWN** — 9.3% on real data; 54.7% was chaos-corpus-only | no | **refuted** by n=3,855 | below |
| D7b | RAPTOR unsupported rate roughly doubles per tree level (0.087 → 0.170) | **yes** | **confirmed** — reproduced in two corpora, n=3,577 vs 271 | below |

### D1 — off-topic corpora admitted in bulk

The strongest finding, and the only one that is unambiguously a product defect
rather than a measurement defect.

For `summary_proof_theory` (a question about **proof theory**), two runs:

```
run A  sep(13) sep-atheism-agnosticism(11) sep-essential-accidental(4)
       sep-evil-kinds-origins(1) wikipedia(2)                    = 31 chunks
run B  sep(16) sep-chinese-logic-language(16) wikipedia(5)       = 37 chunks
```

Run B drew **16 of 37 chunks — 43% of the evidence pool — from "Chinese logic
and language."** Run A drew 11 from "atheism and agnosticism." Every off-topic
item is a `"<corpus> — key point"` atlas atom, not an article chunk. The same
shape appears on all 6 questions in the bank (3 unique-to-A, 1 unique-to-B).

This is the same class as the previously-diagnosed slow-abstention fan-out
(per-atom rescue fanning over corpora while ignoring scope). **Not root-caused
in code.** It reproduces on SEP, a corpus the CI bench gates.

**`--isolate` fully suppresses it.** Measured on the isolated
`retrieval-prod:wikipedia` lane: **0 off-target chunks out of 904**, across 36
questions. So the script's claim at `sovereign-ci-bench.sh:318` holds, and D1
is specifically a defect of the **unisolated cross-corpus admission path**.

That localisation matters in two directions. It narrows where to look in code.
It also means **isolation is not a fix for users** — `--isolate` is a bench-only
flag, so the production path is the affected one. It further implies D3b
(below) is a *symptom* of D1 rather than a separate fault.

### D2 — admission varies run to run under `--synth`

Caught by accidental replication: `bench all --synth --filter sep/summarize`
prefix-matches, so `sep/summarize_obscure` ran twice ~40 min apart on the same
binary (built 18:41:03Z), against the same baseline, with no re-ingest between.

- answer text differed **6/6** questions; retrieved set differed **6/6**
- `judge_fact_score` moved on 4/6: 10/10→9/10, 10/10→9/10, 9/10→7/10, 9/10→10/10
- answer-equiv 0.97 → 0.92; keyword-match 0.75 → 0.73
- `source_score` 1.000 in **both** — so this is not a recall failure

**Consequence for anyone reading a synth number:** a single-run `--synth` delta
is not a measurement. The arrows from this day's baseline-check run
(sep/questions +0.07, summarize_obscure +0.20, summarize +0.31, obsidian +0.06)
support *"no regression"* and nothing stronger.

### D3 — expected source articles lost on wikipedia

Running the isolated and unisolated lanes over the same corpus **split this
into two defects with different causes.** That discrimination is the most
useful thing the ci-bench run produced.

| bank | unisolated `retrieval:wikipedia` | isolated `retrieval-prod:wikipedia` |
|---|---|---|
| `newsworthy_smoke` | regressed | **regressed** |
| `questions` | regressed | **green** |
| `single_atomic` / `single_roman` | green | green |
| `summarize` | improved | improved |

#### D3a — recent-events articles lost, in BOTH configs

Appears with isolation on, so cross-corpus contamination does not explain it.
This is an independent defect.

| question | lane | source score | article lost |
|---|---|---|---|
| `newsworthy-iran-war-ceasefire` | unisolated | 1.00 → 0.67 | *2026 Iran war* |
| `newsworthy-israel-lebanon-ceasefire` | isolated | 1.00 → 0.50 | *2026 Lebanon war* |
| `newsworthy-iran-war-ceasefire` | isolated | 0.67 → 0.33 | *2026 Iran war ceasefire* |

Every article lost is a **recent-events** article, and `fact_score` stayed at
1.00 → 1.00 on both isolated questions. The facts are still being found; the
articles that should carry them are not. Corroborated across two lanes with
different pipeline configurations, which is why this is the strongest of the
suspected findings.

#### D3b — core articles lost, ONLY without isolation

| question | source score | article lost |
|---|---|---|
| `causal_great_depression_to_fascism` | 0.50 → 0.25 | *Great Depression* |
| `contested_colonialism_legacy` | 0.67 → **0.00** | *Colonialism*, *Scramble for Africa* |

A Great-Depression question no longer retrieves *Great Depression*.
`keyword-match` held flat or rose (0.89 ↑0.01) while `title-coverage` fell.
The same bank is **green under isolation**, so this is most plausibly D1
displacing the correct articles rather than a distinct retrieval fault.

**Confidence:** D3a corroborated across two lanes; D3b single-lane and
explained by D1. Neither has been repeat-run.

### D4 — chunk selection unstable under `--isolate`

HARD lane `retrieval-prod:sep` → `FAIL(2reg)`. `source_score` was 1.00 → 1.00
on **every** question — nothing lost — while `fact_score` moved in **both
directions** within the same bank:

| question | fact score |
|---|---|
| `summary_descartes_epistemology` | 0.40 → **0.70** |
| `summary_cosmological_argument` | 0.90 → **1.00** |
| `summary_conservatism` | 0.80 → **0.40** |
| `summary_mill_moral_political` | 0.80 → **0.50** |

Bidirectional swings of similar magnitude in one bank is a noise signature, not
a regression. **Scoping correction this forces:** "`--prod-pipeline` is
deterministic" is too strong. What is stable is **source recall and rank** —
verified twice (12/12 identical sets and identical `source_score` on a paired
`conversation-private` run; 1.00→1.00 across this entire sep lane). What is
**not** stable is which chunks fill the pool from those same correct articles.

### D7 — RAPTOR summary grounding degrades with each level (revised)

> **CORRECTION, entered when the `conversations-anthropic` lane finished.**
> This section first claimed "over half of RAPTOR summary claims are
> unsupported" and called it the most serious finding in the audit. **That was
> wrong.** It rested on 64 claims from `chaos-secret-agent` — a corpus built to
> be adversarial. Measured on 3,855 claims of real conversation data the rate is
> **9.3%**, six times lower. The absolute-level alarm is withdrawn. What
> survives, and is now much better evidenced, is the per-level gradient in D7b.

| corpus | nodes | claims | unsupported | rate |
|---|---|---|---|---|
| `folder-fixture-vault-8a83955f975c` (fixture) | 18 | 57 | 1 | **0.018** |
| `conversations-anthropic` (real user data) | 1,107 | 3,855 | 358 | **0.093** |
| `chaos-secret-agent` (adversarial by design) | 19 | 64 | 35 | 0.547 |

**A 30× spread across three corpora.** The absolute rate is dominated by what
is in the corpus, not by how RAPTOR summarises it — which is precisely why
generalising the chaos figure was an error.

#### D7a — absolute rate: NOT a defect on real data

9.3% unsupported on the corpus users actually query. The 54.7% figure is a
property of the chaos corpus, which exists to break the agent, and it should
never have been generalised. Zero judge failures and zero extraction failures
across all 3,855 claims (no warning emitted — see `faithfulness.rs:520-523`),
so the number is clean.

The gate's baseline of 0.4848 is likewise a *chaos-corpus* baseline. It is not
evidence that the system tolerates one unsupported claim in two on real data.

#### D7b — the per-level gradient: CONFIRMED, and this is the real finding

| RAPTOR level | conversations rate | n | chaos rate | n |
|---|---|---|---|---|
| level 0 (leaf summaries) | **0.087** | 3,577 | 0.509 | 55 |
| level 1 (summary of summaries) | **0.170** | 271 | 0.778 | 9 |
| level 2 | 0.286 | 7 | — | — |

**Each level roughly doubles the unsupported rate**, and it holds in both
corpora across a 60× difference in scale. Level 0→1 on the conversation corpus
(n=3,577 vs n=271) carries real statistical weight; level 2 (n=7) does not and
should not be quoted.

**This rests on TWO corpora, not three.** `folder-fixture-vault` has only a
level-0 tier, so it cannot test the gradient in either direction — a corpus
that cannot test a claim is not a confirmation of it.

This is the compounding-drift failure mode hierarchical summarisation would
predict: each summarisation pass is another chance to assert something the
layer below does not support, and the error does not wash out. A user asking a
broad question — the queries most likely to route to a high-level node — gets
the least-grounded summaries.

#### The regression that made the gate fire: weak, and now clearly so

`faithfulness-gate:chaos-secret-agent` → `FAIL(1reg)`:

```
metric              baseline   current       Δ      tol  dir  status
unsupported_rate      0.4848    0.5469  +0.0620   0.0300   ↓   REGRESSED
```

Absolute run: **19 nodes, 64 claims, 35 unsupported — rate 0.547.**

| RAPTOR level | unsupported | rate |
|---|---|---|
| level 0 (leaf summaries) | 28/55 | 0.509 |
| level 1 (summary of summaries) | 7/9 | **0.778** |

**Separate the two signals, because they have opposite strength:**

- **The regression is weak.** +6.2pp on n=64 is **four claims flipping**
  (31→35). The verdicts come from an LLM judge, and D5 means judge failures
  are scored as unsupported. Four claims is well inside that variance. Do not
  treat the FAIL as evidence something broke this week.
- **The absolute level is the finding, and it is not a regression at all — it
  is a standing property.** More than half of the claims in RAPTOR node
  summaries are not supported by the member chunks those summaries are built
  from. The *baseline* is 48.5%, so the gate is calibrated to accept roughly
  one unsupported claim in two as normal.

**Level 1 being worse than level 0 (0.778 vs 0.509) is the interpretable
part.** The summary-of-summaries layer is the least grounded, which is the
compounding-drift failure mode you would predict from hierarchical
summarisation. n=9 at level 1 is small and the error bars are wide, but the
direction matches the mechanism.

**Why this matters to a user:** RAPTOR summaries feed answers. If half the
claims in a summary node are unsupported by the text beneath it, answers built
on those nodes carry content the corpus does not support — while still citing
the corpus.

**Confidence:** the chaos absolute rate is solid for *that corpus* (full
sampling, 0 sentinel-filtered, all 19 nodes) but does not generalise — see the
correction above. The regression is weak.

**The D5 interaction was checked and is CLEAN — the rate is not inflated.**
`faithfulness.rs:458-464` *drops* an unjudgeable claim from the denominator and
counts it separately (`n_judge_fail`) rather than scoring it unsupported. The
counter is surfaced as a warning at `:520-523` whenever it is non-zero, and the
chaos lane emitted **no such warning** — zero judge failures, so all 64 claims
carry a real verdict. **54.7% stands unqualified.**

### D6 — intent routing misroutes commitments and feelings into knowledge lookups

HARD lane `routing` → `FAIL(4reg)`, 4 of 6 banks regressed vs 2026-07-16.
**32 misroutes**, and they are not scattered — they have a shape.

| expected intent | misroutes | lands in |
|---|---|---|
| `commissive_query` | **12** | `knowledge_query` ×6, `complex_task` ×2, `deep_query` ×2, `expressive_query` ×2 |
| `expressive_query` | 8 | `deep_query` ×8 |
| `metalingual_query` | 6 | `knowledge_query` ×6 |
| `comparison_query` | 4 | `knowledge_query` ×4 |
| `deep_query` | 2 | `knowledge_query` ×2 |

`knowledge_query` is an **attractor**: 18 of 32 misroutes land there.
`commissive_query` is the most fragile source: 12 misroutes scattering across
four different destinations.

**User-visible meaning.** "Don't let me forget", "it's on my list", "up next",
"flag that for Friday" are commitments — they get answered as knowledge
lookups. "I'm stuck", "I give up", "I'm exhausted" are expressive — they get
answered as deep research questions.

**The mechanism is visible in the per-bank numbers**, and this is the
actionable part:

| bank | accuracy | embed-layer share |
|---|---|---|
| `routing/skills_migration_smoke` | **10/10** | 100% |
| `routing/voice_routing_v1` | 21/23 | 87% |
| `routing/cells_v1` | 24/27 | 26% |
| `routing/cells_v1_paraphrases` | **18/27** | **11%** |

Accuracy tracks embed-layer coverage almost perfectly. Where the embed-exemplar
router answers, routing is right; where coverage drops and the LLM classifier
takes over, accuracy collapses. The paraphrase bank is the worst case at 67% —
canonical phrasings route at 89%, paraphrases at 67%, so this is a
**generalisation failure in the LLM fallback**, not a broken embed router.

**Confidence:** single run against a 19-day baseline, but the internal
structure (32 misroutes with a consistent shape, correlated with embed
coverage) is far stronger evidence than the regression count alone.

#### D6 worked — and the headline finding is not the one the defect predicted

Investigated with `sovereign router fit --axis intent`, an embed-only,
deterministic instrument (no LLM, no judge) that sweeps the whole
threshold space from one embedding pass. It measures what accuracy on a
saturated bench cannot: how much of a bank the embed router OWNS.

**The finding that reframes D6.** Against the 40-case calibration bank
`bench/routing/calibration/axes_v1.toml`, the shipped intent gate owned
**3 of 40 cases — 7.5% coverage, 35% accuracy, 26 missed, 0 false
positives.** The embed router was not misrouting commitments. It had
very nearly stopped deciding at all, and the LLM Pass-1 classifier
behind it is what users were actually getting on ~92% of turns. That is
why `knowledge_query` is an attractor: its prompt said *"When in doubt:
LOOKUP"* (`router.rs:587`), and 18 of the 32 misroutes landed there.

**Why the sparse classes lost.** `DEFAULT_MIN_MARGIN` is ONE global
constant arbitrating eleven classes on
`margin = top_sim - second_sim`. That is fair only between classes of
comparable density. Census on the day: knowledge 35, expressive 32, deep
31, metalingual 29, conation 26, code 18, comparison 15, generative 14,
**commissive 8, simple 6, complex_task 4**. For a query inside a sparse
class's own territory its nearest exemplar is further away than a dense
rival's, so it loses `top_sim` and therefore `margin` for a reason
unrelated to being wrong. Straight from `--explain`:
`int_commissive_lint_before_push` contains a literal "I'll" and
commissive's best exemplar still reached only **sim 0.390**, losing to
an *expressive* exemplar on the shared token "push".

**Per-class thresholds were considered and rejected.** `router_embed.rs`
proposed them, but they turn one calibrated constant into eleven, each
needing its own ≥5-case evidence, and they treat the symptom: lowering a
sparse class's threshold admits it where it is merely the least-bad
match — the exact false-positive mode `da9b5aa5` raised the margin to
stop. Frame coverage raises `top_sim` AND `margin` together, which no
threshold move can, and adds no knob.

**What shipped.** 33 exemplars across the sparse tail (commissive
8→20, expressive 32→39 for register not count, complex_task 4→12,
simple_query 6→12), each a FRAME the class did not cover rather than a
paraphrase of a row it had; the two prompt defects above; and
`MIN_EXEMPLARS_PER_INTENT` with a whole-file test, because a class goes
sparse by *not* being edited and so leaves no diff a reviewer could
catch.

**Measurement — and the instrument had to be built first.** Neither
existing artifact could score this honestly: five commissive and three
expressive items in `cells_v1_paraphrases`, and three `axes_v1` cases,
share a surface frame with exemplars the fix adds, so their deltas
measure coverage, not generalisation. So
`calibration/holdout/intent_frames_v1.toml` (24 cases) was authored
**before a single exemplar was written**, in frames then declared
off-limits to the exemplar set, and kept out of the default fit run
(the directory scan is non-recursive) so it cannot perturb the drift
baseline. `axes_v1` was frozen across the change to keep the A/B clean.

| | `axes_v1` (frozen A/B) | holdout (unseen frames) |
|---|---|---|
| ranks correct, gate ignored | 15/29 → **19/29** | 7/20 → **13/20** |
| correct fires at shipped gate | 3 → 3 | 0 → 1 |
| false positives | 0 → 0 | 1 → 1 |
| coverage / accuracy | 7.5% / 35% → unchanged | 4.2%/12.5% → 8.3%/16.7% |

Four ranking fixes on `axes_v1`, zero breaks, zero new false positives
(one break — `int_conation_halve` captured on the shared verb "cut" —
was found by the instrument and fixed by rewording the offending
exemplar, not by deleting the frame). Seven fixes on held-out frames
nobody wrote an exemplar for, which is the number that says the
density thesis generalises rather than memorises.

**State the limit plainly: at the shipped gate this is barely a
user-visible win.** `axes_v1` coverage is unchanged at 7.5%; the holdout
gained one fire. The ranking is markedly better and the gate still
rejects it, because **0.206 is now the binding constraint** — and
`router fit` re-confirms 0.206 is optimal on `axes_v1` even after the
change, so lowering it is still wrong. The exemplar work is mostly the
prerequisite that makes a future gate move safe, not the win itself.

#### End-to-end routing bench, and a prompt fix that measured flat

`bench all --routing-only --filter routing`, no `--update-baseline`:

| bank | before | after |
|---|---|---|
| `cells_v1` | 24/27 | 24/27 |
| `cells_v1_paraphrases` | 18/27 | **19/27** |
| `voice_routing_v1` | 21/23 | 21/23 |
| `skills_migration_smoke` | 10/10 | 10/10 |
| `future_timeline_v1` | — | 9/9 |

**The +1 is the exemplar change, not the prompt change.** Re-running the
paraphrase bank with `router.rs` reverted and the exemplars left in place
returns **19/27 with a byte-identical failure set and identical predicted
intents**. The prompt rewrite — killing *"When in doubt: LOOKUP"* and
replacing COMMISSION's string-literal definition — moved nothing.
`commissive_p_up_next` is the one gained item and the log shows it
decided by the embed router on a new exemplar ("That refactor is next up
for me.", margin 0.264).

**Why the prompt could not have helped, which the audit's original D6
write-up got wrong.** These items largely never reach Pass 1:

* `commissive_p_flag_for_friday` is short-circuited by the `force_action`
  pre-check at `router.rs:2168-2173` — rationale
  *"current/time-sensitive signal → external tool"*, confidence
  hardcoded 1.0. The LLM never sees the message, so no prompt wording
  can reach it.
* `commissive_p_on_my_list` and `commissive_p_mine_to_write` DO reach the
  LLM, and it answers **SIMPLE** — not LOOKUP. They land on
  `knowledge_query` / `deep_query` only via downstream promotion. So
  removing the LOOKUP tie-break was aimed at the wrong mechanism for
  these cases.

So the original finding that *"When in doubt: LOOKUP"* is the engine of
the `knowledge_query` attractor is **not supported** for the commissive
items. The prompt edits are kept because two of them assert things that
are wrong on their face — a category defined by string literals in a
prompt whose own preamble says to classify the MOVE, not the surface
form — but they are recorded here as **measured flat**, not as a win.

**And the paraphrase bank's baseline says commissive USED to work.**
`baseline_paraphrases.json` was minted **2026-05-12** (not 2026-07-16)
at 25/27, and all five commissive items pass in it with
`coarse=COMMISSION` — the LLM classified them correctly then. Something
between May and August took the LLM's COMMISSION verdict away, and it is
neither the exemplar census nor the Pass-1 wording. That is a live,
unclosed regression with a known-good reference point, and it is the
highest-value thread left on this axis.

**Left open, with evidence.**

1. The May→August loss of `coarse=COMMISSION` above.
2. The `force_action` pre-check swallowing commitments that mention a
   weekday (`router.rs:2168`) — a pre-check that fires before the
   classifier cannot be corrected by the classifier.
3. The holdout's one false positive, pre-existing and real: *"the deploy
   is tomorrow"* — a bare statement of fact — hard-commits to
   `commissive_query` at margin 0.221, clearing the gate with room. A
   user stating a fact gets it filed as their promise.

### D5 — the judge collapses its own failures into the score

`sovereign/crates/sovereign-cli-llm/src/eval_cmd/score.rs:341-362`: both a
parse failure and an inference failure push the fact onto `missing` with
`present: false`. A could-not-judge is recorded as a judged-absent — loud in
stderr, silent in the metric, and biased downward only. This is the
`ARCH_PRINCIPLES` smell *"an `Err` collapsed into a success-shaped value."*

Observed magnitude: **1 parse failure in 259 judged facts, 0 inference
failures.** It did not distort this day's numbers. The policy is still wrong.

**And the correct policy already exists in the same crate.** The faithfulness
lane faces the identical situation and handles it properly —
`bench_cmd/faithfulness.rs:458-464` drops the unjudgeable claim from the
denominator, increments `n_judge_fail`, and surfaces the count as a warning at
`:520-523`. Its comment states the reason outright:

> *Judge unavailable for every probe of this claim — a fabricated verdict would
> poison both the rate and the training feed. Drop the claim, count the
> failure.*

So this is not merely a wrong policy: it is **two implementations of one
judge-failure policy with opposite semantics**, in sibling modules of the same
crate — the `ARCH_PRINCIPLES §10.6` "one decider, one name" violation, with the
correct decider already written and commented. That makes the fix close to
free: adopt the faithfulness policy in `score.rs`, and report
could-not-judge as its own verdict rather than as absence
(`§18.3` — absence is reported, never defaulted).

## I1 — one long TRACKED lane starves every lane after it

Not a retrieval defect, but it shaped this run's coverage and will shape every
future one on a machine without `coreutils`.

`scripts/sovereign-ci-bench.sh` bounds non-HARD lanes with
`HARD_RESERVE_SECS` (1800s) so a slow advisory lane cannot consume the budget
the trailing HARD gates need. **That reserve is implemented by passing the cap
to `timeout`** (`:244-248`), and `TIMEOUT_BIN` is resolved from
`command -v timeout || command -v gtimeout` (`:170`). **On macOS neither
exists**, so `TIMEOUT_BIN` is empty, every lane runs uncapped, and the reserve
is inoperative. The banner still prints `lane cap <N>s`, which reads as though
a cap is in force.

Observed 2026-08-04: `faithfulness:conversations-anthropic` (TRACKED) is
judging **1,111 RAPTOR nodes** at ~5.15 nodes/min ≈ **3.6 h**, against ~2.0 h
of remaining budget. The budget guard is only checked *between* lanes
(`run_lane` entry), so nothing interrupts it; when it ends, `remaining()` is
negative and every subsequent lane takes the `SKIP(budget)` path — and a
**HARD** lane that skips sets `HARD_FAIL=1`.

**Second-order risk: the lane's artifact is written only at completion.** After
68 minutes, `faithfulness-conversations-anthropic.jsonl` did not exist; the
chaos lane's 4.6 MB file appeared at lane end. Progress lines are tee'd
incrementally, results are not. Interrupt the lane and the entire judging pass
is lost. This is the same failure the synth run avoided by driving one bank per
invocation.

**Fixes, cheapest first:** `brew install coreutils` (restores the reserve);
stream the faithfulness JSONL per node rather than buffering; make the banner
say "uncapped" when `TIMEOUT_BIN` is empty rather than printing a cap that is
not enforced.

**Operator decision 2026-08-04:** let the lane finish and accept the skips. The
unsupported-claim rate on the real conversation corpus is worth more than the
tail lanes, and it is the direct comparison to D7's 55% on synthetic data. The
skipped lanes (mechanism-fidelity, multiturn, search-gym, knowledge-gym,
agent-coding, governance) are therefore **NEVER-RAN, not passed** — they must
not be read as clean.

## Measurement-infrastructure gaps found alongside

Not retrieval defects, but they are why the above went unseen. Detail in notes
`d2af7720` and `aeddd363`.

- Conversation retrieval appears in **no** ci-bench lane
  (`sovereign-ci-bench.sh:159,164`), and neither conversation bank can cover it.
- **5 of 90** baselines were gitignored yet counted as "committed" — fixed in
  `posture_cmd.rs`; `svrn posture` now reports `present (gaps)`.
- **8 banks** carry a question bank but no baseline at all, so they can only
  ever report `first-run` — which also *writes* a baseline from the run under
  test.
- Every baseline in use is past the tool's own 14-day staleness threshold
  (19d–79d), and baselines record no model id.

## Lane results (ci-bench `--no-synth`, in progress)

| lane | kind | verdict | note |
|---|---|---|---|
| `enrichment:literary/bk-book-1` | HARD | PASS | baseline 58d |
| `retrieval:sep` | HARD | PASS | 78s |
| `retrieval:wikipedia` | HARD | **FAIL(2reg)** | → D3a + D3b |
| `retrieval-prod:sep` | HARD | **FAIL(2reg)** | → D4 |
| `retrieval-prod:wikipedia` | HARD | **FAIL(1reg)** | → D3a; also the D1 isolation control (0/904 off-target) |
| `routing` | HARD | **FAIL(4reg)** | → D6; 4 of 6 banks |
| synth lanes | SOFT | SKIP | `--no-synth` |
| `chaos-monkey` | TRACKED | FAIL(1) | **advisory by design** — chaos is built to break the present agent; its absolute NO-GO is a finding, not a regression |
| `chaos-gate` | HARD | **PASS** | the real chaos signal: no metric regressed past tolerance vs baseline |
| `faithfulness:chaos-secret-agent` | TRACKED | PASS | ran clean; 19 nodes, 64 claims |
| `faithfulness-gate:chaos-secret-agent` | HARD | **FAIL(1reg)** | → D7; baseline only 4d old (the freshest in the run) |

_(updated as lanes land)_

**Reading TRACKED vs HARD.** Several lanes pair an advisory TRACKED run with a
HARD `*-gate` twin that re-scores the same artifact against a committed
baseline. A TRACKED `FAIL` is an absolute verdict about the current system and
must not be counted as a regression — only its gate twin votes. Counting
`chaos-monkey`'s FAIL as a defect would be a misreading of
`sovereign-ci-bench.sh:17-30`.

## Open questions requiring a repeat run

1. **Is D3a reproducible?** The strongest suspected finding — already
   corroborated across two lanes, but never repeat-run. Re-run
   `retrieval-prod:wikipedia`; if the same recent-events articles vanish, it is
   real.
2. **Do D4's signs flip on repeat?** If `conservatism` rises and `descartes`
   falls next time, it is noise and the lane's threshold is too tight for a
   bank this small.
3. ~~**Does D1 survive `--isolate`?**~~ **ANSWERED — no.** 0 off-target chunks
   in 904 on the isolated lane. Isolation is a complete mitigation at the bench
   layer, and D1 is a defect of the unisolated (production) path.

## Final lane tally

`16718s / 14400s budget` · exit 1

| kind | passed | failed | never ran |
|---|---|---|---|
| **HARD** (votes) | 3 | 5 | **6** |
| TRACKED (advisory) | 4 | 1 | 5 |

**HARD passed:** `enrichment:literary/bk-book-1`, `retrieval:sep`, `chaos-gate`.
**HARD failed:** `retrieval:wikipedia` (2reg), `retrieval-prod:sep` (2reg),
`retrieval-prod:wikipedia` (1reg), `routing` (4reg),
`faithfulness-gate:chaos-secret-agent` (1reg).
**HARD never ran:** the six listed in the coverage warning above.

Faithfulness absolute rates, all four corpora that produced a number:

| corpus | claims | rate | tiers present |
|---|---|---|---|
| `folder-fixture-vault` | 57 | 0.018 | level 0 only |
| `obsidian-vault` | 1,464 | 0.034 | level 0 only |
| `conversations-anthropic` | 3,855 | **0.093** | levels 0, 1, 2 |
| `chaos-secret-agent` | 64 | 0.547 | levels 0, 1 |

Real corpora land between **1.8% and 9.3%**. Chaos is the outlier by design.
Only `conversations-anthropic` and `chaos-secret-agent` have more than one tier,
so **D7b rests on two corpora and only one of them carries weight.**

## Where to dive

Ranked by (user impact × evidence strength) ÷ cost to act.

**1. D1 — off-topic corpora in the production evidence pool.** The only
confirmed, user-visible, unrooted defect. A user's answer grounded ~40% by
volume in an unrelated subject is a citation-quality failure they can see. It
is already localised: `--isolate` suppresses it completely (0/904), so the bug
is in the unisolated cross-corpus admission path, and every off-topic item is a
`"— key point"` atlas atom. It also explains D3b, which makes it two findings
for one fix. Start at the atom-rescue fan-out that was diagnosed once before
for slow abstention.

**2. D6 — routing sends commitments and feelings to `knowledge_query`.**
Cheapest real fix in the ledger, and the mechanism is already identified rather
than suspected: accuracy tracks embed-layer coverage (100% → 10/10; 11% →
18/27), so the LLM fallback is what fails, and the likely remedy is exemplar
coverage rather than prompt surgery. "Don't let me forget" being answered as a
knowledge lookup is a defect any user meets in their first session.

**3. D3a — recent-events articles lost.** Corroborated across two lane configs,
every lost article a recent-events one, `fact_score` pinned at 1.00 while
`title-coverage` falls. Needs one repeat-run to move from suspected to
confirmed — do that before investing.

**4. D7b — RAPTOR grounding halves per tree level** (0.087 → 0.170, n=3,577 vs
271). Real and mechanistically sensible, but bounded: the absolute rate on real
corpora is 1.8–9.3%, so the blast radius is much smaller than the withdrawn
D7a implied. Worth fixing, not worth panicking about.

**5. D5 — judge failure policy.** Small magnitude (1 in 259) but the correct
implementation already exists 120 lines away in a sibling module. Near-free.

**6. I1 — the reserve that cannot fire.** `brew install coreutils` restores
per-lane caps; stream the faithfulness JSONL instead of buffering to remove the
all-or-nothing risk; make the banner print "uncapped" when `TIMEOUT_BIN` is
empty. Do this before the next run or it will starve its tail the same way.

**7. D4 — likely noise.** One repeat-run settles it, and if the signs flip it
also says the lane's threshold is too tight for a bank that small.

**Not on this list, and deliberately: the six never-ran HARD lanes.** Re-run
them before drawing any conclusion about tool-use, agentic coding, multi-turn
behaviour, reasoning fidelity, or governance. `--no-synth` plus a fixed I1
should fit them comfortably inside budget.
