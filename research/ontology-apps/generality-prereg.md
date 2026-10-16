# Generality bars (pre-registered 2026-10-16, before any held-out read under the majority judge)

Accepted by the operator 2026-10-16. The goal is records that feel best in class; these bars keep that honest.
They are iterative: revised only between runs, each revision added below with its date and reason.

**Instrument.** score_bank.py, judge stamp 5a45944ec884 (majority of five presentations). One measurement of an
arm is two fresh reads (loop.sh with `ASKER=daemon`, so every window is asked anew), both scored; report both
scores and their mean. The one-shot is measured the same way: two reads (runs/ceiling/saltgrass/oneshot1.json and
one more), each scored.

**Sets.** Development, diagnosed: the five worlds, ward-story, saltgrass-asked. Held out, scored and never
diagnosed (corpora.HELD_OUT): saltgrass-ledger, and every persona recipe below. A fix is found on development;
held-out corpora are re-read only after a change judged there. No new generated world (operator, 2026-10-16).

**Bar G, generalizes.** On each held-out corpus, the system's mean is at least the one-shot's mean on the same
bank minus 2 (of 42).

**Bar R, whoever writes the recipe.** For two recipes A and B on one corpus, with reads A1, A2, B1, B2:
- |mean A - mean B| is no larger than the larger of |A1 - A2| and |B1 - B2|;
- cross-recipe agreement (questions with the same verdict, averaged over the four pairs Ai, Bj) is at least
  within-recipe agreement (averaged over the pairs A1, A2 and B1, B2) minus 3.

**Recipes.** saltgrass-asked and saltgrass-ledger (blind, 2026-10-15), and persona recipes written by isolated
`claude -p` sessions under blind-author/protocol.md, one persona each, all for the same owner's description
(saltgrass/DESCRIPTION.md). A persona changes who writes the recipe, never what the owner wants from the material.

**A failed bar** is reported with its numbers; it is a finding for the development loop, never a diagnosis of the
held-out corpus.

## Revisions

**2026-10-16, before any read below; to cut spend about 70% (operator: "Can you do it with less?").**
- The judge asks the five presentations in order and stops a question once one verdict has three: the majority
  of five, unchanged, at 134-135 calls instead of 210 (checked on the saved presentations, seeds 1-5 and 6-10,
  42/42 verdicts identical).
- A fresh read at 79ab8ad4a counts as one of an arm's two reads: the reader at today's code makes identical
  records (867240dec's replay). saltgrass-asked's first read is structural-fresh-sg, saltgrass-ledger's is
  structural-fresh-ledger; each gets one new read.
- Bar G is judged on saltgrass-ledger only for now, against the two one-shot reads on file
  (runs/ceiling/saltgrass/oneshot1.json, oneshot2.json; their record types are recipe.blind.toml's).
- Each persona gets one read. Bar R's read-to-read gap is pooled: the mean of |A1 - A2| over the recipes with two
  reads (asked, ledger), and within-recipe agreement likewise pooled. A recipe pair passes when its score gap and
  its agreement are within the pooled yardstick as the bar states.
