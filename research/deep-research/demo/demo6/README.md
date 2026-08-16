# DEMO-6 — the estate's corpus-search surface rendering the v1 report-class question

Order `deep-research-t1g` (T1 rung 2 of the acquisition ladder). The v1
question is the report-class question the deep-research loop exists to
serve:

> "How did American cities change across four decades (1980-2024):
> gentrification, inequality, affordability, and displacement — every claim
> cited?"

The t1f-era cap: the loop's acquisition ran on the gym's ON-DISK deck
surface (`--backend mock` term-ranked retrieval) — the deck is a fixture,
not the estate. Rung 2 wires the acquisition's search leg to the ESTATE:
the compounding corpus the T1a demo proved (`svrn corpus ingest` →
`CorpusIndex::open` + `search`, the house's existing corpus retrieval —
§19: nothing new was built). This demo shows the **corpus search source** —
pre-registered BEFORE the re-measure (`adversarial/pre-registration.md`,
T1 rung-2 declaration) — rendering that question: the loop searches the
dr-demo6-v1 corpus (built ONCE from the verbatim frozen v1 deck bodies),
every search hit stamped `engine: corpus` with the estate's `personal`
custody and chunk-level `estate:` locators — and the report's every
number is either attributable to the evidence window or named absent:
the passed claim's era years are flagged untraced (the honesty leg's
measured failure, below), never silently dropped.

## What is in this directory

| File | What it is |
|---|---|
| `deck-extract/` | The FROZEN v1 deck bodies, copied verbatim (byte-identical) for the one-time corpus build — the bank is read, never edited |
| `report-v1-corpus.md` | The v1 corpus flight's report (verbatim from the battery's run dir) — verdict-stamped claims, chunk-level estate citations |
| `bars.md` | The re-measured bars — **the scorer's own numbers** (score-report-t1g.json), never hand-typed |
| `verify-demo6.sh` | The corpus-source strips — the demo is only as strong as its verification |
| `README.md` | This file |

The raw artifacts live in the battery's run dir
`research/deep-research/arms/runs/loop/v1/dr-*/` — the manifest, the plan,
the per-round fetch lists (each search hit stamped `engine: corpus` with its
LanceDB relevance score), the triage outcome (code_set_k + ε-admits), gap
lists, the evidence window, the skip ledger and the budget ledger are all
there, as recorded by the shipped CLI on the corpus source.

## What the corpus source did on this question

1. **Source dispatch (mock | corpus).** One decider, one flag: the v1
   flight ran `--search-source corpus --corpora dr-demo6-v1`; every search
   hit is stamped `engine: corpus` (glassbox — the fetch lists, triage
   outcomes and manifest name the source), and every admitted chunk's
   locator is a CHUNK-LEVEL estate locator (`estate:dr-demo6-v1:<chunk_id>`)
   carrying the estate's `personal` custody — never re-stamped public-web.
   The 12 v0 seeds ran unchanged on the mock deck surface; the battery's
   protocol (budget 12/12, max-rounds 3, model pin) is untouched.
2. **Real corpus retrieval, real scores.** The corpus is the estate's
   vector+FTS hybrid (`CorpusIndex::search`) over the daemon's embed slot
   (Qwen3-Embedding-0.6B-Q8_0, dim 1024) — the same surface `svrn corpus
   search` uses. A concept query ("What is the Gini coefficient measuring
   income inequality for major US metropolitan areas...") retrieves the
   source-report chunk whose content carries "Gini coefficients exceeding
   0.54" — the corpus search works.
3. **The measured boundary (the corpus-leg evidence).** The flight's
   round-1 triage admitted 3 chunks — thematically relevant, but none
   value-shaped — and the round-1 evidence window carried NONE of the
   bank's figures, so the report could not name them (14 of 16 keys failed
   "missing figures in answer"). The mechanism, read from the artifacts:
   LanceDB's hybrid relevance scores QUANTIZE to identical f32 buckets
   (~0.03333333507180214) for the top hit of every query; the triage's
   score-then-figure-bearing tie-break reads only the TITLE (chunk titles
   are digit-free document names — dead on the corpus surface); so the
   top-k admission degenerates to insertion order and the value-bearing
   chunks lost a tie lottery to thematically-relevant figure-free chunks.
   The budget (12/12) exhausted in round 1, so there was no round-2
   second chance. This is the boundary the landing's forks must weigh:
   the corpus leg retrieves; the R5 triage boundary cannot see past the
   quantized scores.
4. **The honest report.** One passed claim (the thematic overview);
   every other claim is [could-not-judge] — the corroboration floor
   named single-origin support on every one. The passed-position
   honesty property is VIOLATED on this flight: the passed claim
   restates the question's own era ("1980", "2024"), which the window
   does not carry verbatim, and the scorer's own tokenizer flags that
   claim untraced (density row `traces=false`, `nums_in_window=[]` —
   score-report-t1g.json). The honesty leg therefore failed on BOTH
   the letter (loop ungrounded 0.062 vs one-shot 0.019) and the
   load-bearing passed-position property. The era years are the
   question's framing restated — traced-once artifacts, not
   fabrication — but the decider has no year exemption, and neither
   does this strip: the violation is named, never exempted (verify
   strip 3 below, amendment B).

## How to verify

```bash
./verify-demo6.sh
```

The strips, in order:

1. the v1 corpus flight exists and terminated (report.md + terminal
   manifest);
2. every claim in the report is verdict-stamped — a claim with no verdict
   is a silent number;
3. every figure token in the report's passed claims appears in the run's
   accumulated evidence window (the scorer's own tokenizer, loaded from
   score-arms.py — one decider); flagged claims' absences are named by
   their stamps. On THIS flight strip 3 FAILS — the passed claim's era
   years (1980/2024, the question's own framing restated) trace to no
   chunk (see the measured boundary above). The failure is named, never
   exempted: the honesty leg failed on the passed-position property as
   well as the letter;
3b. the acquisition source on THIS flight is the corpus — every round-1
   search hit stamped `engine: corpus`; chunk-level estate locators with
   the estate's `personal` custody on the window; round-1 queries carrying
   no value-shaped digits beyond the question's own. The third half — an
   ADMITTED chunk whose content carries value-shaped figure runs in none
   of those queries — is the concept→value proof through the corpus, and
   on THIS flight it FAILS: the admitted chunks carried no value-shaped
   figures (the triage boundary's quantized-score mechanism above). The
   failure is the measurement — it is what the landing's fork evidence
   rides on; it is named, never silenced;
4. bars.md carries the scorer's per-question fractions and bar legs
   verbatim (score-report-t1g.json) — never hand-typed;
5. the two-arm lift is the same scorer's, over the same pairs.

The declaration's disjunction resolved to the second outcome: DEMO-6 is
the **corpus-leg evidence** for the landing's fork surface (the bank-key
re-cut remains the operator's call), not the strong demo — P4-v1 2/16
below the ≥12/16 bar, with the mechanism journaled above and in the t1g
execution section of `adversarial/pre-registration.md`.
