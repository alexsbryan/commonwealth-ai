# Index-aligned next-edit bank — PRE-REGISTRATION

**Written 2026-08-28 BEFORE the miner ran or any case was scored.** Nothing
below was chosen after seeing a number. If a bar here is missed, that is a
finding to triage, not a bar to move (`ARCH_PRINCIPLES.md` §18.1, §18.6).

## Why a third bank exists

Two banks already measure this feature and neither can answer the question:

- `gym/next-edit/` (120 cases) replays **historical** file states. The SCIP
  index describes HEAD, so only 27.1% of proposed sites align to it — measured
  2026-08-28, note `b11d3418`. It cannot carry an index-dependent measurement.
- `gym/next-edit/golden/` (1,098 cases) is mined from **41 external repos**
  that are not indexed at all. That is the same blocker from the other side,
  and it is why note `439368da` §5 filed a could-not-judge rather than a number.

This bank is built so the index is valid **by construction**.

## The construction

For every `.rs` file alive at HEAD, take **the most recent commit that touched
it**. By definition no later commit modified that file, so the file's content
at that commit is byte-identical to its content at HEAD, and the SCIP index
therefore describes exactly the state the episode is mined from.

The episode itself is `harvest.py`'s, reused unchanged (`build_positive`,
`hunks_of`, `expand_rule`, `predict`): replay the first k single-line hunks of
a repeated-edit group as edit history, send the mid-edit document, hold out the
remaining commit-edited sites as ground truth. Ground truth is the author's
own commit and is **independent of SCIP**.

All hunks are single-line, so the mid-edit document is line-for-line aligned
with HEAD. A candidate site at line L maps to HEAD line L with no heuristic.

## What is measured, and the bars

Ground truth is the author's commit; the ruler is the one the golden set
already uses (`score_golden.py::site_precision`), so the numbers are
comparable to the published ones.

| # | Question | Bar, set now |
|---|---|---|
| A1 | Does the bank replicate the golden set's verdict on an independent, realistic sample? | useful-fire and hunk-precision each within **±8 pts** of the golden set's **Rust/Go subset** — useful-fire 31.4%, hunk-precision 47.0%, oracle on, measured 2026-08-28. Outside that, the two banks disagree and the disagreement is the finding. |

> **Amended before mining, with no data in hand.** A1 first named the golden
> main bank as its reference (35.0% / 38.6%). That bank spans 18 languages and
> this one is Rust-only, and the syntax oracle is switched ON for Rust and OFF
> for most of the others — so the whole-bank figure is the wrong comparison and
> would have made a matching result look like a miss. The Rust/Go subset is the
> apples-to-apples reference. Recorded rather than silently swapped.
| A2 | Does the feature pull its weight? | Reported, not gated. useful-fire, wrong-fire, hunk-precision, missed-fire, each with a Wilson 95% CI. **A CI wider than ±5 pts on hunk-precision means the bank is too small to carry a verdict and I say so instead of ranking it.** |
| A3 | How much of the junk is invisible to SCIP? | Reported, not gated. Share of **junk** sites falling in a comment or string — i.e. carrying no `refs` occurrence at HEAD. |

**Target n:** ≥150 positives and ≥75 negatives. Below that, A2's CI bar decides
whether a verdict is published at all.

## What this bank CANNOT answer, stated before running

**It cannot score a SCIP site filter against the author's edits.** The index
describes the file *after* the rename landed, so a true site holds the new name
and a junk site holds the old one; "resolves to the same symbol as the
exemplar" then succeeds exactly when the text is already the ground truth. Such
a number would be near-100% and would measure nothing. Answering that honestly
needs an index of the **pre-edit** state, which this construction does not
provide and which is not worth manufacturing — a failed rust-analyzer export
wipes the graph.

A3 is the one SCIP question that survives, because "is there any code
occurrence here at all" does not depend on the rename.

## Run

```sh
python3 gym/next-edit/aligned/harvest_aligned.py     # (re)build cases.jsonl
python3 gym/next-edit/aligned/score_aligned.py       # score vs a live daemon
```

---

# RESULT (2026-08-28) — A1 FAILED, and that is the finding

Run: 182 cases (82 positive, 100 negative) against the production daemon,
rule lane isolated, 0 request errors, scored with the golden ruler.

| metric | aligned bank | golden Rust/Go (A1 reference) | verdict |
|---|---|---|---|
| useful-fire | **59.8%** (CI 48.9–69.7) | 31.4% | +28.4 pts — **outside the ±8 bar** |
| hunk-precision | **65.6%** (CI 60.3–70.5) | 47.0% | +18.6 pts — **outside the ±8 bar** |
| wrong-fire | 5.8% (3/52) | 12.6% (whole golden bank) | — |
| negatives | 100/100 silent, **0 wrong fires** | 365/387 | — |

**Do not read those as good news about the feature.** A1 was written to catch
exactly this, and it caught it. The bank is optimistic for two structural
reasons, both in the construction rather than in the engine.

## Cause 1 — it can only mine the lane's home turf

An episode here requires **three or more single-line hunks inducing the same
literal rule**. That is the rule lane's designed shape, so the bank cannot
contain the shapes it fails at. Measured on the golden set the same day:

| golden stratum | n | useful-fire |
|---|---|---|
| the 3 fanout shapes (literal / rename_casing / type_fanout) | 270 | **72.2%** — what this bank mines |
| every other shape | 441 | **12.2%** — what it cannot mine |
| whole bank | 711 | 35.0% |

`import_addition` scores 0.0% over 90 cases and `signature_fanout` 3.3% over
90; neither leaves a repeated-identical-rule group behind, so neither can ever
appear here. Those unmineable shapes are **62% of real editing episodes**.
59.8% sits inside the fanout band, exactly where composition predicts.

This is the criticism `golden/README.md` already made of the `gen` bank —
"a mirror of the gate, not a sample of the world" — reproduced by a different
construction. Sampling by *what the engine matches on* cannot measure what the
engine misses. Only sampling by *shape* can, which is why the golden set is
stratified that way and why it stays the honest read.

## Cause 2 — its negatives are the easy ones

Negatives come from `build_neg_dissimilar` and `build_neg_exhausted`. On the
golden set those same shapes score **0 wrong fires** too — every one of that
bank's 22 wrong fires is `neg_literal_trap`, the adversarial shape where the
pattern's last occurrence survives inside a comment or a string. This bank has
no literal traps, so **0/100 is what the golden set predicts for these shapes
and says nothing about safety.**

## A3 — inconclusive, and why

Of 111 junk line-groups, **73 (65.8%) could not be mapped to HEAD**: the
mining commit also changed *other* lines, which the mid-edit document leaves at
their pre-commit state, so those lines differ from HEAD and no lookup is valid.
Of the 38 that did map, 8 (21%) sit where SCIP records no occurrence. Coverage
is too low to carry a verdict and the mapped subset is not a random one, so no
number is published from A3.

## What this bank is good for

Not a headline. It is a **fanout-shape regression bank that the SCIP index
validly describes** — the only bank here with that property. Two honest uses:

- catching regressions in the fanout shapes specifically, at n=82 with a ±5 pt
  precision CI;
- any future index-dependent experiment, which neither other bank can host.

**Do not quote its useful-fire as the feature's.** The golden set's 35.0% is
the realistic figure; this bank's 59.8% is the same number restricted to the
three shapes the engine was built for.
