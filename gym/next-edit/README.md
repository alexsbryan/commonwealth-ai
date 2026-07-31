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
  not touch `concatenate`), the support-2-needs-≥4-chars and
  support-3-allows-≥2 rows of the firing table, deletion and
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

Over-offer (queue sites the commit author did *not* edit) is
**reported, not gated** — the queue deliberately offers every
remaining guarded site and the user tabs past unwanted ones.

## Run

```
python3 gym/next-edit/harvest.py          # (re)build cases.jsonl — stable across runs
python3 scripts/next_edit_eval.py         # run vs live daemon :9741, print table + gates
```

No model required — the rule lane is pure string work; the bank runs
against any daemon build with the route, whatever `[models]` says.
Exit code is the gate verdict (CI-able). `--json out.json` dumps raw
per-case results for triage.
