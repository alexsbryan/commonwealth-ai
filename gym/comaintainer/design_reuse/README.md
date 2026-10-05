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

## Registered dev run — 2026-10-05 (8 dev cases × 3 conditions)

Engine: `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`. The seat pin
(`Qwen3.8-27B-UD-Q6_K_XL`) is no longer advertised by this node; the
substitution is named in each run's `meta.json`, and these numbers are
**not comparable** to the committed comaintainer pedigree (ENGINE_OF_RECORD
Darwin-36B; the README there says the pedigree does not transfer).

24/24 parsed. Two independent full passes produced **byte-identical raw
completions on all 24 cells**, so the deltas below are exact at n=8 —
sampling variance is in the bank, not the instrument, on this engine.

| condition | parsed | disposition | home | path | declared new_comp | evidence |
|---|---|---|---|---|---|---|
| A | 8/8 | 1 | 0 | 0 | 3 | — |
| B | 8/8 | 4 | 3 | 4 | 0 | — |
| C | 8/8 | 2 | 5 | 2 | 4 | 8/8 |

Against the pre-registered expectations:

1. **MET** — `home(B) 3 > home(A) 0`; the dossier also lifts path 0→4 and
   disposition 1→4. Retrieval-by-dossier is the lever that moves the design.
2. **FAILED** — `disposition(C) 2 < 4 = disposition(B)`. Grounding is now
   structural (8/8) and C has the best home rate (5/8), but the grounded
   protocol chose a *worse* disposition than the plain dossier. The lexical
   bait detector fired **0 times anywhere** — uninformative on this run,
   never a pass.
3. **INVALID AS REGISTERED** — declared new components are not comparable
   across conditions: C's interface requires enumeration while A/B permit
   silence, so B's 0 is a floor by omission, not evidence of abstention.
   C declared new types/methods in 2 of 8 cases (desktop: an
   `EnrichmentIntegrityReport` type plus `CorpusEngine::check_integrity`;
   session-ontology: two `ClaimSketch` fields plus a `Decidability` enum).
   A v2 needs a uniform enumeration field, or referee adjudication of the
   deltas, before this axis can be compared at all.
4. **CONFIRMED** — of C's 5 home hits, 4 still chose a wrong disposition,
   and two misstate the named surface's own capability: sep claims
   `AtlasCorpusSummary` lacks per-type counts its excerpt shows, and
   core-read picks the concrete `CorpusEngine` over the seam the task
   removes. The residual failure is comprehension and decision *after*
   retrieval, not retrieval.

Reading: the dossier moves the design; the extension-map protocol as
currently worded buys grounding (8/8, and the smoke run's prose contract
satisfied 0/3) and the best surface rate, but it did not improve — and
sometimes loosened — the disposition choice (core-read B EXTEND → C USE;
sep B USE → C EXTEND). The next iteration is protocol wording and
interface, not more retrieval. History: the n=3 smoke run also exposed
two instrument defects, both fixed before this run — selection metrics
counted surfaces merely mentioned in evidence (a rejected alternative
read as "found"), and C's evidence grounding was a prose request the model
ignored; narrowing the metrics to the choice fields and enumerating the
ids in the schema are the lane's own thesis applied to itself.

## Pre-registered directional expectations for the next run

Registered 2026-10-05, before any holdout use:

1. `home_match(B) > home_match(A)` — retrieval is a real bottleneck.
2. `disposition_match(C) >= disposition_match(B)` — grounding does not
   degrade the choice, and `bait_hits(C) <= bait_hits(B)`.
3. `new_components(A) >= new_components(B) >= new_components(C)` — the
   invention proxy falls as grounding tightens.
4. Failures that survive C name a surface from the dossier and still
   choose the wrong disposition or the wrong owner — i.e. decision, not
   retrieval. The first pass already showed this shape at n=3; the
   registered run needs n>=8 dev cases per condition. **Run completed
   2026-10-05 — verdicts in the section above; expectation 4 confirmed.**

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
