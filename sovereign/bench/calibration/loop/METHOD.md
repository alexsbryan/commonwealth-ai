# The tuning loop — method (one page)

Order `native-grounding-tuning-loop` (directive 44f48dd6). The parity plan
(`sovereign/docs/specs/NATIVE_GROUNDING_PARITY_PLAN.md`) supplies the work
queue (§3 conversion ledger) and the outer bars (§4). This directory
supplies the method. Everything here is glue over committed apparatus —
nothing new was built beyond the drivers in this directory.

## The three loops

**Inner loop (seconds-to-minutes, unlimited).** One command per component
replays that component's offline objective against the case ledger:

```
./objective.sh admission    # ~1s   D3 apparatus replay: ledger regenerates byte-identical
./objective.sh routing      # ~4s   router-fit embed replay, 3 A3 probes + 63-case guard
./objective.sh claims       # ~2s   chaos-rescore replay, A5 caveat probe + negative control
./objective.sh retrieval [bench-substring]   # ~25s/bench  pool recomposition vs baseline ratios
```

Protocol: **one change -> replay -> keep or revert**, one line in
`JOURNAL.md` per iteration (component, change, objective before/after,
kept?), written at the moment, never retroactively. A change with zero
measured effect is reverted — dead weight is a cost. Arms (env-var
probes) prove mechanisms but are not keeps; a keep is committed code.

Verdicts are four-valued (ARCH §18.1): PASS(0) / FAIL(1) /
COULD-NOT-JUDGE(2); never-ran is visible as absence from the journal.
Each objective was watched failing before its green was trusted
(admission: sabotage; routing/retrieval: honest before-state; claims:
standing negative control baked into every run).

**Middle loop (~20-45 min, run channel only).** A dev-bank A/B
(`../ab/run_ab.sh`) runs ONLY when a component objective improves —
cross-component non-regression against the plan's §4 bars. Staged as
`runs/<name>/run.sh` + manifest; the seat launches it. Never
self-detached.

**Outer gate (seat, at landing).** HARD lanes + the FROZEN holdout.
Never iterated against here. The case ledger is the training set; the
holdout is the overfit guard.

## Guard discipline per component

- routing: exemplars are tuned ONLY against the 3 probes; the 63-case
  calibration guard (axes_v1 + holdout bank) is read as a gate — no case
  that passed at baseline may be lost (`routing_guard_baseline.json`).
- retrieval: targets are read from `../step3/failure_corpus.jsonl` (one
  decider — no second copy of a threshold). Trimmed banks are generated
  at run time from the source banks, question blocks verbatim.
- claims: the committed transcript replayed unmodified must still FAIL
  (negative control); only then does the variant's PASS mean anything.
- admission: byte-identity of the regenerated ledger guards the training
  set itself while other components tune.

## Timings (measured 2026-08-10, BeefyMac)

admission 0-1s · routing 4-5s · claims 2s (offline) / ~45s (one live
probe) · retrieval 23-28s per bench scope, ~100s all four. Every
component clears the order's ~5-minute bar.
