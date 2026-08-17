# DEMO-2 — the bank-v1 arms, measured (order `deep-research-t1c`)

The T1c phase-2 demo (scene DEMO-2 in `research/deep-research/DEMO_PLAN.md`):
the measurement arm over the report-class question — bank v1's single
"four decades of American cities" seed, flown through the shipped CLI path
(`svrn deep-research "..." --backend mock --mock-deck bank/v1/deck
--run-dir ... --max-rounds 3`) against the SAME deck one-shotted through a
Rust integration test (`sovereign-core/tests/oneshot_rag.rs`, the two-arm
control — only the loop differs). Bank v0's 12 seeds were flown on the same
surfaces; the full leg verdicts are in
`research/deep-research/arms/score-report.json` (deterministic scorer
`arms/score-arms.py`, fixture-green, rules journaled in its header).

**Instrument discipline (pre-registered):** bank v0 and the v1 deck are
frozen (run, never edit — deck sha256
`e63a14499d849301f3f0bbd00024c178609c5899b97d5b6ec0a6ee5b1e88c5ee`); no
loop/gate code changed during the measurement; drafts delegated to the real
daemon on :9741 (Qwen3.6-35B-A3B-MTP-UD-Q6_K, tau 0.9); every leg reported
four-verdict (§18.2). Protocol + thresholds ratified at
`research/deep-research/adversarial/pre-registration.md`.

## The v1 flight (`runs/loop/v1/dr-1786748480`, completed 08-14)

```
round 1: search 1  fetch 4  gaps_before 0  gaps_after 1    -> draft-1 (the
         empty-estate abstention: "No evidence was retrieved this round...")
round 2: search 0  fetch 0  gaps_before 1  gaps_after 26   -> draft-2 over
         the round-1 window (governing, construction-coverage, stanford,
         terry-uga); the round-2 gap audit named 26 open questions
exit at max_rounds with gaps open -> done-partial; report = draft-2
         rendered with the verdict set; artifact strip: charter, plan,
         survey, fetch-list, evidence-window, draft, gap-list, verdict-set,
         report, manifest, budget ledger, skip ledger
```

Round 1 fetched 4 of the deck's 11 hits (the estate-search results for the
question); round 2 searched and fetched **0** — the 26 gap queries found
nothing more on the mock estate, so the loop's follow-up was genuinely
wasted-round-free on the report-class seed (the P3 mechanism, see below).

Verdict shape of the final report (`report-v1.md`, copied here): 28 claims —
**2 failed (refuted by the evidence), 26 could-not-judge (23
single-origin-support floor caps, 3 extracted-specifics-absent), 0 passed.**
The two refutations are the interesting part: the draft's "median household
incomes grew by an inflation-adjusted 8.5%, median home prices increased by
more than 56%" was flagged *refuted by the evidence* — the window's own
figures (92% income, 177% home prices, construction-coverage) contradict
it, and the gate caught the pair instead of letting it ship. Nothing passed
because every surviving claim cites exactly one chunk: the corroboration
floor caps single-origin claims at could-not-judge by design (GAP-2), which
is the loop's measured honesty shape, reported separately from coverage.

## The two-arm numbers (loop vs one-shot, same deck)

| question | loop coverage | one-shot coverage | loop density | one-shot density |
|---|---|---|---|---|
| v1 (report-class) | 3/16 | 8/16 | 1.0 | 1.0 |
| v0 seeds 01-12 (pooled) | 52/72 | 55/72 | 1.0 | 1.0 |

Density = fraction of numeric claims in the output that trace to the
deck window. Both arms trace every numeric claim (1.0 everywhere): neither
arm fabricates figures — the one-shot's ungrounded fraction is 0.0, and the
loop's is 0.0, so honesty is **not worse** (the pre-registered clause).

Coverage is scored per the pre-registered deterministic checker, including
the v1 evidence-arbiter corrected forms read from the frozen arbiter
journal (K2's required set reduces to NYC 0.5469 — the only named-source
figure; K4's deck-supported form is "Atlanta and DC rank among the high
95/20 cities"; K7's national figure corrects to 4.6; K9 cannot clear —
journaled per key in `arms/score-arms.py`). Applied: loop +1 (K7 — the
report named the corrected 4.6), one-shot +1 (K4 — the draft named Atlanta
and Washington D.C. among the high 95/20 cities).

The loop's 3/16 on v1: K7 (corrected), K8 (doubling + 20% of
lower-income neighborhoods), K14 (displacement pattern). The one-shot's
8/16 adds K1, K4, K6, K10, K15 — the one-shot reads the whole deck in one
window, the loop only ever saw the 4 chunks its round-1 search returned.

## Leg verdicts (four-verdict, bars operator-ratified at approval)

| leg | verdict | measured | bar |
|---|---|---|---|
| P4-v0 | **failed** | 52/72 | >=58/72 |
| P4-v1 (loop) | **failed** | 3/16 | >=12/16 |
| P3 (fetch shrink) | **failed** | 1/13 passed | >=10/13 |
| R-12 (gap shrink) | **failed** | 0/12 v0 seeds | >=10/12 |
| two-arm lift (pooled) | **failed** | +0.00 | >=+0.10 |
| two-arm lift (v1) | **failed** | +0.00 | >=+0.15 |
| honesty not worse | **passed** | 0.0 vs 0.0 ungrounded | loop <= one-shot |
| P5 (poisoned-drill battery) | **passed** | 6/6 | 6/6, no noise band |

Journaled mechanisms behind the failures (each measured, not assumed):

- **P3:** on the 12 v0 single-origin seeds, rounds 2+ re-fetch the same
  exemplar url (no fetch dedup) — round-2 fetched >= 20% of round-1 on
  every seed, so the shrink leg fails 12/13. The v1 flight is the single
  pass: round-2 fetched 0 < 0.8 (20% of 4) with coverage not worse
  (3 >= 2). The honest read: the pass mechanism is the empty mock estate
  returning nothing more, not loop-smartness — journaled as such.
- **R-12:** gap sets GROW on every seed (0/12, v1 journaled 1 -> 26): the
  round-N audit of a content draft adds claims, and the corroboration
  floor caps every single-origin claim, so each audit's gap set is a
  superset of the previous. The dr-compass red condition — "convergence at
  bank scale never observed" — is now a measurement.
- **P4:** the loop's coverage is capped by its own acquisition: the round-1
  search returned 4 of 11 deck hits, so facts carried only by the other 7
  (smartasset's 80/20 ratios, brookings' 95/20, pew's shares, cooper-center's
  jobs) were never in the window — evidence-absent, judged gap per the
  evidence-arbiter rule. The one-shot's full-deck window clears more keys
  on the same questions (55/72 vs 52/72 pooled) — the loop's per-round
  retrieval is narrower than the deck, and on single-origin decks the
  floor caps the rest.
- **Two-arm lift:** both arms trace every numeric claim, so the density
  lift is +0.00 on both the pooled and the v1 comparisons. The loop earns
  no density advantage on this bank: with the deck as the only evidence
  surface, both arms ground everything they state.

## P5 battery

Re-run 08-14 after the arms completed (one measurement at a time — the
daemon slot is shared). Verdict: **PASSED 6/6, no noise band** — all five
verify checks green (closed-set refusals, terminal-state equality per
pair, trace identity, fabrication absent from passed claims, injection
inertness); the verify output is recorded verbatim in `p5-verify.txt`.

## Files here

- `report-v1.md` — the v1 loop's final report (verdict-stamped claims)
- `oneshot-v1.md` — the one-shot comparator's draft over the same deck
- `p5-verify.txt` — the P5 battery verify output (after the re-run)
- full machine record: `research/deep-research/arms/score-report.json`
