# ei-7c A/B — arms 1 and 2

Run 2026-09-05 00:56–01:02 PDT at tip d1fa49b5e, in a seat lane window, capped
14 GB. Bars registered before the runs in `ARMS12-PREREG.md`.

## Instrument, checked before the numbers (§18.4)

Every run carried the provider line, and `seed_table` matches its arm:

| run | provider line |
|---|---|
| arm1 run1 | `backend="wiki-class" atoms=51781 edges=2286375 seed_table=false load_ms=5300` |
| arm1 run2 | `backend="wiki-class" atoms=51781 edges=2286375 seed_table=false load_ms=5231` |
| arm2 run1 | `backend="wiki-class" atoms=51781 edges=2286375 seed_table=true  load_ms=5198` |
| arm2 run2 | `backend="wiki-class" atoms=51781 edges=2286375 seed_table=true  load_ms=5217` |

**Zero runs discarded.** Arm 0 is the contrast that makes this line worth
requiring: it has no such line at all, because its v1 store was refused by name
and the caller fell to bag-of-atoms — which from outside is indistinguishable
from a walk that ran and found nothing.

`edges=2286375` against the 7,847,320 the rebuild wrote is not a loss: the walk
face dedupes to one edge per (source, target) and drops edges with a dangling
endpoint, because an `EdgeView` names its endpoints by atom id. The neighbor
face still serves all of them with `in_scope: false`.

## Rows

All `--prod-pipeline --isolate`, n=20, over `wikipedia-ei7c` whose
`chunks.lance` is a symlink to the installed wikipedia's.

| arm | store | seed table | fact_recall | source_recall |
|---|---|---|---|---|
| 0 run1 | installed, v1 | — (walk REFUSED) | 0.6946 | 0.6167 |
| 0 run2 | installed, v1 | — (walk REFUSED) | 0.6946 | 0.6167 |
| 1 run1 | rebuilt v2 | absent | 0.6946 | 0.6167 |
| 1 run2 | rebuilt v2 | absent | 0.6946 | 0.6167 |
| 2 run1 | rebuilt v2 | **present** | **0.6967** | **0.6458** |
| 2 run2 | rebuilt v2 | **present** | **0.6967** | **0.6458** |

Run-to-run spread: **exactly 0.0000** in both arms, 20/20 questions identical,
as arm 0 was. Retrieval is a HARD lane with an exact band, so every delta below
is real, not weather.

## The pre-registered prediction: CONFIRMED, to the last digit

Arm 1 was predicted to equal arm 0's prod row because a wiki-class atlas has no
atom bag to name-match over, so with no ANN table the walk resolves the store
and seeds on nothing. It does — 0.6946 / 0.6167, and **all 20 questions are
identical to arm 0's**, not merely the mean.

So the v2 store ALONE moves nothing. Everything arm 2 gains is the seed table's,
and the cutover cannot ship without it: it would install a walkable atlas that
grounds zero.

## Both directions (§18.6)

Arm 2 vs arm 1 differs on exactly **three of twenty** questions. Two up, one
down:

| question | fact | source | what moved |
|---|---|---|---|
| `causal_roman_empire_fall` | 0.750 → 0.750 | 0.333 → **0.667** | gained the `Migration Period` source; facts traded `476`/`invasions` for `Migration Period`/`economic` |
| `contested_quantum_determinism` | 0.500 → **0.667** | 0.250 → **0.500** | gained the `Werner Heisenberg` source and the `hidden variables`/`indeterminism` facts; lost `free will` |
| `causal_french_revolution` | 1.000 → **0.875** | 0.500 → 0.500 | **REGRESSED** — lost the `Enlightenment` fact, gained nothing |

Net: **source_recall +0.0291, fact_recall +0.0021.**

The shape is what the substitution predicts. Seeding on an article's LEAD
PASSAGE rather than its title reaches neighbouring articles a title vector
could not — `Migration Period`, `Werner Heisenberg` — and that is where the
source gain comes from. The cost is that a passage-shaped seed can crowd out a
keyword the title-shaped one happened to carry, which is the French Revolution
row. One question regressing on facts while source recall rises across the bank
is a trade, and it is reported as one.

## Verdict against the registered bars

- **Arm 2 vs arm 0-prod: ABOVE on both metrics → the table SHIPS**, with the
  substitution named ("seeded on the lead passage, not the title"). Nothing was
  tuned; no knob was touched between arms.
- **Arm 2 vs arm 1 is the table's own contribution**, and since arm 1 == arm 0
  exactly, the table accounts for the entire delta.
- **Arm 1's prediction: met.** The mechanism reading holds.

Cost of the table: 0 embed calls, 3,431 ms, 204 MB, 51,781/51,781 articles.
