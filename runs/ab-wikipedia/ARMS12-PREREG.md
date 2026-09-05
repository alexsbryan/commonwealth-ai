# ei-7c A/B arms 1 and 2 — pre-registration (written BEFORE the runs)

Registered 2026-09-04, before `arms12.sh` was executed. Lane: `sovereign/bench/wikipedia`
via `svrn eval run --prod-pipeline --isolate`. Retrieval is a HARD lane with an
EXACT band (RUNBOOK §6): arm 0 showed run-to-run spread of **exactly 0.0000** on
both metrics across two runs in both modes, so **any delta here is real**.

## The arms

| arm | store | `atoms_ann.lance` | what it isolates |
|---|---|---|---|
| 0 (done) | installed `wikipedia` atlas, v1 | absent | today. Walk REFUSED by name (v1 has no `atom_id`/`chunk_id`). |
| 1 | rebuilt v2 store | **absent** | the v2 store alone — walk resolves, but can it seed? |
| 2 | rebuilt v2 store | **present** (borrowed) | the borrowed seed table's contribution. |

Arms 1 and 2 read `wikipedia-ei7c`, whose `chunks.lance` is a symlink to the
installed wikipedia's, so the retrieval substrate is byte-identical to arm 0's.
The two atlas dirs are symlink farms over the same rebuilt store; they differ by
one file.

Arm 0's committed rows, both directions reported (§18.6):

| mode | fact_recall | source_recall |
|---|---|---|
| retrieval (default — atlas NOT in the loop, negative control) | 0.7071 | 0.5458 |
| `--prod-pipeline --isolate` | 0.6946 | 0.6167 |

## The prediction, and why it is a prediction and not a hope

**Arm 1 will equal arm 0's prod row (0.6946 / 0.6167).** Not "probably" — by
construction, read out of `ground::ground`: a walk has exactly two seed sources,
the ANN table (§1a) and name-matching over an atom BAG (§1b). A wiki-class atlas
has no bag — `AtlasContextManager::load_corpus` returns before building one
because `AtlasGraph::load_from_disk` refuses a store with no `atoms.lance` — so
with no table, arm 1 resolves the store, enters the walk branch, seeds on
nothing, and injects nothing.

If arm 1 differs from arm 0's prod row at all, one of those two readings is
wrong and the finding is the mechanism, not the number.

**This makes the seed table load-bearing rather than an optimisation**, and it
is the reason the cutover cannot ship without it: moving wikipedia to the v2
store WITHOUT a table would resolve a walkable atlas that grounds nothing.

## Bars

- **Arm 2 vs arm 0-prod is the decision.** Above → the table ships; the
  substitution is named in the commit ("seeded on the lead passage, not the
  title") and nothing is tuned. Below → the table is REFUSED and the curve is
  emitted. Equal (0.0000 on both, given the measured zero spread) → the walk is
  reaching the store and finding nothing useful in it, which is a `ground`
  finding, not a seed-table one.
- **Arm 2 vs arm 1 is the table's own contribution**, and it is the only pair in
  which the table is the single varying input.
- No tuning loop. EI3/EI4/EI5/EI6 are structural bars (campaign policy); a miss
  here is a defect, not a knob.

## Scale, stated so the numbers are not misread

The v2 store holds **51,781 in-scope articles**, not the 1.67M atoms
`atlas/atoms.json` carries — the difference is dangling link targets, which have
no chunk and were never citable. So the seed table's 51,781 rows are 100% of
what the store can seed, not 3% of what atoms.json listed.

## Instrument validation, before the result (§18.4)

Every run's stderr must carry `walk provider: opened … backend=wiki-class …
seed_table=true|false` at the value its arm claims. The store PATH is not
sufficient proof: a store that fails to open reports nothing and the caller
falls silently to bag-of-atoms, which is exactly how three identical arms would
look like a null result. A run whose `seed_table=` disagrees with its arm is
discarded, not reported.

Two runs per arm, as arm 0 had, so the zero spread is re-measured rather than
assumed.
