# next-edit eval bank (NEXT_EDIT.md §6)

Verifies the **rule lane** of `POST /v1/edit_predictions` against a
healthy swath of its intended universe: repeated-edit episodes mined
from this repo's real git history, plus authored edge cases probing
the firing table, guards, UTF-16 offsets, and queue mechanics.

The rule lane is deterministic, so this bank is a **contract check,
not a model score**: the gates below are pre-registered at 100%/0%
and a miss is a named bug (coalescing, induction, guards, offsets),
never "model noise". When a gate fails, triage the bug — do not move
the gate.

Two mechanisms landed *after* these fixtures were cut and decline some
of them by design: the syntax oracle (`next_edit_syntax.rs`,
2026-08-06) and the `MIN_RULE_CHARS` 4-to-5 sweep (2026-08-07). Those
cases are partitioned out rather than scored — see "the declined
population" below. The bars themselves have not moved.

## How cases are made

`harvest.py` (deterministic, no RNG) walks commits newest-first:

- **harvest-pos** — a commit+file whose single-line hunks induce the
  *same* expanded rule at ≥3 sites is a natural episode: replay the
  first k hunks as edit history (k=2, or 3 when the rule is short —
  mirroring the firing table), send the mid-edit document, hold out
  the remaining commit-edited sites as expected queue entries.
- **harvest-neg** — two *dissimilar* single-line edits from one
  commit (support 1 by construction → must stay silent), and
  exhausted episodes (the replayed edits were the last sites → must
  be silent with `no_sites`).
- **authored** — hand-built probes: canonical console.log walk,
  emoji/UTF-16 divergence, word-boundary guards (`cat`→`dog` must
  not touch `concatenate`), the rows of the firing table as it
  stood in July (a support tier the 2026-08-07 sweep retired —
  see `should_fire`; `a03` and `a15` encode it, and are annotated
  `min_rule_chars` for that reason), deletion and
  insertion rules, CRLF, tabs, no-op units amid real ones,
  multi-line units (`no_rule`), cursor wrap order, MAX_EDITS cap.

Ground truth comes from an independent Python replica of the
expansion/guard/site logic written from the spec; harvest asserts
replica-derived expectations against hand asserts where intent is
the point. A Rust↔replica divergence is a finding either way.

## Pre-registered gates (set before the first run, 2026-07-30)

| Gate | Metric | Bar |
|---|---|---|
| G1 correctness | malformed edits (bad offsets, overlap, old-span ≠ rule find, new ≠ rule replace) + authored exact-queue mismatches | **0** |
| G2 contract recall | harvest-pos: fired AND every held-out commit site in the queue | **100%** |
| G3 restraint | all negative cases silent; authored silence reasons exact | **100%** |
| G4 latency | wall p95, local daemon | **≤ 150 ms** |
| G5 declines (added 2026-08-28) | every case annotated `declined_by` still declines by that named mechanism | **100%** |

Over-offer (queue sites the commit author did *not* edit) is
**reported, not gated** — the queue deliberately offers every
remaining guarded site and the user tabs past unwanted ones.

## The declined population (why G5 exists)

G1 and G2 read the cases that are supposed to work. G5 reads the ones
a deliberate precision trade declines. Keeping them in one pool made
G2 a gate that could only ever fail, which is worse than no gate: a
reader could not tell an inherited red from a regression they had just
caused, and 25 of 120 cases sat permanently red for reasons the docs
attributed entirely to the firing policy. Only 8 of them were that.

A declined case carries `expect.declined_by`, naming the mechanism.
**The annotation is a check, not a waiver** — the harness re-derives
the mechanism on every run and G5 fails if it stops describing what
the daemon does:

| `declined_by` | n | what the harness re-derives each run |
|---|---|---|
| `syntax_oracle` | 14 | every withheld held-out site **reappears** when the request carries a path no grammar matches. That counterfactual is what separates "the oracle filtered it" from "site finding is broken". |
| `min_rule_chars` | 8 | the daemon itself reports `below_threshold`. The threshold is not re-derived on this side — `next_edit.rs` is the one decider and the harness reads its verdict. |
| `pair_fallback` | 3 | a *different*, anchored rule fired, and every held-out site is **text-equivalently** covered by one of its edits, so the routed rule makes the change the fixture wanted at a wider anchor. An unrelated edit fails. |

If an annotated case starts passing outright, G5 also goes red and
says to delete the annotation, so the set cannot rot into a green that
means nothing. All three failure modes have been watched to fail: a
stale annotation, a wrong mechanism, and a real regression tripping G2
while G5 stayed green.

Removing a case from the declined population is a measurement, not an
edit — change the mechanism, re-run, and the annotation either
verifies or the gate tells you it no longer holds.

## Run

```
python3 gym/next-edit/harvest.py          # (re)build cases.jsonl — stable across runs
python3 scripts/next_edit_eval.py         # run vs live daemon :9741, print table + gates
```

No model required — the rule lane is pure string work; the bank runs
against any daemon build with the route, whatever `[models]` says.
Exit code is the gate verdict (CI-able). `--json out.json` dumps raw
per-case results for triage.
