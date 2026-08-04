# Retrieval system audit — 2026-08-04

**Status: IN PROGRESS.** A `scripts/sovereign-ci-bench.sh --no-synth` run is
still executing; this file is updated as lanes land. The closing section
("Where to dive") is deliberately empty until the run finishes — the whole
point is to let the full lane set rank the defects rather than ranking them
from the first thing found.

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
| D7 | >50% of RAPTOR summary claims unsupported by their own source text | **yes** | **confirmed** (absolute rate, full sampling); its *regression* is weak | below |

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

### D7 — over half of RAPTOR summary claims are unsupported by their own source text

**The most serious finding in this audit, and the gate fired for the wrong
reason.** `faithfulness-gate:chaos-secret-agent` → `FAIL(1reg)`:

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

**Confidence:** the absolute rate is solid (full sampling, 0 sentinel-filtered,
all 19 nodes). The regression is not. **Unverified interaction to check:**
whether the faithfulness judge shares the `score.rs` failure-collapse of D5; if
it does, the rate is biased upward by an unknown amount.

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

### D5 — the judge collapses its own failures into the score

`sovereign/crates/sovereign-cli-llm/src/eval_cmd/score.rs:341-362`: both a
parse failure and an inference failure push the fact onto `missing` with
`present: false`. A could-not-judge is recorded as a judged-absent — loud in
stderr, silent in the metric, and biased downward only. This is the
`ARCH_PRINCIPLES` smell *"an `Err` collapsed into a success-shaped value."*

Observed magnitude: **1 parse failure in 259 judged facts, 0 inference
failures.** It did not distort this day's numbers. The policy is still wrong.

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

## Where to dive

_To be completed when the run finishes. Deliberately empty: the ranking should
come from the full lane set, not from whichever defect was found first._
