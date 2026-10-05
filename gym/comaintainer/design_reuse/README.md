# Design-reuse lane — home selection before plan generation

The question this lane exists for: **given a task, can a model find the
existing owner and extend it, or does it treat the task as the first
thing ever built?** It is scored on the decision, not on the prose that
follows it. Register: a real dev's design begins with "what here can
absorb this?"; the failure being measured is the swept-then-invented
7-phase plan.

## Provenance model

Eleven cases, each with a BASE revision (the outcome's first parent, or a
verified ancestor for artifact-introduction cases, stated per case), an
OUTCOME revision, and candidate surfaces that existed at BASE. Three
sides:

- **request** — requirement and constraints. The only task material.
- **dossier** — candidate existing surfaces at BASE, each with a path and
  a verbatim excerpt. Supplied to conditions B and C, withheld from A.
- **oracle** — expected disposition, acceptable owner/seam/path
  identities, correction quote, and rejected baits. Answer-side only.

`validate.py` checks every base/outcome object resolves, every excerpt
appears verbatim in the BASE tree, the oracle is non-empty, and no
correction quote or outcome sha leaks onto the request side. Split is
recomputed, never stored: every third id within (label) stratum goes to
holdout; **holdout stays frozen during iteration**.

Two labels, deliberately different weights: `historic_operator` (a
recorded operator correction names the miss) and `proposed_referee_needed`
(strong historical correction evidence, not yet refereed). These are
reconstructed replays, not the original task transcripts; the request
text is a reconstruction and says so.

## Conditions

| | prompt | interface |
|---|---|---|
| A | task + constraints | disposition/owner/seam/delta required |
| B | + dossier | same schema |
| C | + dossier + binding protocol | all fields required; `evidence` **enumerated to the candidate ids** (+ `none-of-these`); limits and delta minimum lengths |

C's grounding is structural on purpose: the decoder can only emit
candidate ids, so "I grounded my answer" cannot be a free-text claim
(principle 10). A and B keep free-text evidence — that difference is the
experiment.

Deterministic metrics per call: parsed / malformed / could-not-judge,
disposition match, home match and path match (**over the choice fields
only** — owner, seam, delta, new_components; the evidence list records
what was *surveyed*, including surfaces considered and rejected), new
component count, lexical bait hits, evidence grounding under C. Whether a
named surface can actually serve the task is semantic and stays a
refereed question; the oracle's `why_acceptable` / rejected baits are the
reference material for that pass.

## First pass — 2026-10-05 (dev cases only, n=3)

Engine: `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`. The seat pin
(`Qwen3.8-27B-UD-Q6_K_XL`) is no longer advertised by this node; the
substitution is named in each run's `meta.json`, and these numbers are
**not comparable** to the committed comaintainer pedigree (ENGINE_OF_RECORD
Darwin-36B; the README there says the pedigree does not transfer).

| condition | parsed | disposition | home | path | new_comp | evidence |
|---|---|---|---|---|---|---|
| A | 3/3 | 0/3 | 0/3 | 0/3 | 1 | — |
| B | 3/3 | 1/3 | 1/3 | 1/3 | 0 | — |
| C | 3/3 | 0/3 | 2/3 | 1/3 | 0 | 3/3 |

What the n=3 says and does not: the dossier visibly moved retrieval
(home/path 0→1, and C 2/3), but the recurring miss is the **decision
after retrieval** — given an excerpt that already describes name-free
shape matching, the model still proposed extending the detector; given
`IndexSource` as the safe seam, it chose the concrete `CorpusEngine` and
kept the very dependency the task removes. That is the swept-then-invent
failure in miniature. n=3 cannot rank conditions; that is the next run.

Two instrument defects were found by the first pass and fixed before
re-running: (1) selection metrics counted surfaces merely mentioned in
evidence — a rejected alternative read as "found" until home/path were
narrowed to the choice fields (the run rescored with zero model calls);
(2) C's evidence grounding was a prose contract the model satisfied 0/3 —
enumerating the ids in the schema made it 3/3. Both fixes are the lane's
own thesis applied to itself.

## Pre-registered directional expectations for the next run

Registered 2026-10-05, before any holdout use:

1. `home_match(B) > home_match(A)` — retrieval is a real bottleneck.
2. `disposition_match(C) >= disposition_match(B)` — grounding does not
   degrade the choice, and `bait_hits(C) <= bait_hits(B)`.
3. `new_components(A) >= new_components(B) >= new_components(C)` — the
   invention proxy falls as grounding tightens.
4. Failures that survive C name a surface from the dossier and still
   choose the wrong disposition or the wrong owner — i.e. decision, not
   retrieval. The first pass already shows this shape at n=3; the
   registered run needs n>=8 dev cases per condition.

A kill: if B and C are indistinguishable from A on home and disposition,
the dossier/protocol buys nothing and the lane stops before any training
talk. Cost is 3 calls per case (one per condition) at temp 0.

## Run

```sh
python3 gym/comaintainer/design_reuse/validate.py
python3 gym/comaintainer/design_reuse/test_design_reuse.py
python3 gym/comaintainer/design_reuse/replay.py --dry-run
python3 gym/comaintainer/design_reuse/replay.py --pin <advertised-model-id> --limit 8
python3 gym/comaintainer/design_reuse/replay.py --rescore runs/<stamp>
```

Runs persist full prompts, raw completions and served model under
`runs/<stamp>/` (gitignored); `--rescore` reproduces every metric with
zero model calls. Headline numbers land here. `--case <id>` may name a
holdout case but iteration must not; `--include-holdout` runs everything
deliberately.
