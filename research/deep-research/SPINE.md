# The deep-research spine — what this initiative is

Five mechanisms, one loop. Everything else is wiring.

Operator-landed 2026-08-14 (directives 6ab41e6c + c45d8625). Derives from
`sovereign/docs/specs/DEEP_RESEARCH.md` and `research/deep-research/PLAN.md`;
the falsifiable gates are the `[deep-research]` bars in
`quality/initiative-bars.toml`. Every build order names the spine mechanism it
builds and the bar it moves.

## The five mechanisms

1. **The artifact spine.** Every boundary between components is a serialized,
   versioned artifact in the run directory (FR-2). One decision that buys
   modularity, replay, fixtures, and glassbox together: components qualify
   against golden fixtures, any run resumes from any boundary, the run
   directory is the flight recorder. Exercised at T1 (R11-thin).
2. **The compass.** A gated answer emits four verdicts plus NAMED gaps; gaps —
   not vibes — drive acquisition; no-new-gaps is the terminal test. Proven by
   the T0 hand-run (`dr-compass-handrun` met) and held under the dry run.
   Gate: `dr-compass` — the round-N gap set strictly shrinks on >=10 of 12
   bank questions, weekly tier.
3. **The estate.** Everything fetched becomes a stamped, custody-classed
   corpus; enrichment runs offline at electricity prices; the corpus outlives
   the report and compounds — the economics, and the v1-no-frontier argument.
   Gates: `dr-estate-visible` (met), `dr-estate-integrity`.
4. **The trust boundary.** Custody stamped at fetch — never by a model — and
   one egress choke point: a payload leaves iff every chunk is public-web,
   otherwise a typed refusal names what was withheld. The entire answer to
   "how do frontier keys not break the stance." Gates: `dr-custody` (T1),
   `dr-egress` (T2).
5. **The flight recorder.** Rules frozen into the charter at launch, states
   enumerated, abort-from-everywhere lands on a gated report with truncation
   declared. Reproducibility is the precondition of the learning loop. Gates:
   `dr-budget-one-decider` (T2), `dr-local-loop`, `dr-verdict`.

## The use case that anchors it

The product claim this spine serves: a user asks ONE research question — the
class of the operator's gentrification report ("Urban Gentrification Metrics:
Four Decades of American City Transformation") — and gets back a fluent, fully
cited report synthesized from hundreds of sources, with every number
attributable and every absence named. Four decades, ~30 metros, six measure
families, dense numeric claims, cross-entity synthesis, a policy-level
conclusion. The report class is bank v1 (the report-class question + 16
coverage keys + an 11-body source deck, minted order `deep-research-t1c`,
frozen deck sha256 `e63a14499d849301f3f0bbd00024c178609c5899b97d5b6ec0a6ee5b1e88c5ee`).
Each mechanism, re-expressed against that class:

- **The artifact spine** makes a 300-source report replayable — the run is a
  typed state machine with a flight recorder, not a prompt.
- **The estate** makes hundreds of sources economical — it compounds; the
  corpus outlives the report, so the second report is cheaper than the first.
- **The trust boundary** is why the report's citations can be believed —
  custody stamped at fetch, never by a model.
- **The compass** is why the report says "we looked for X and found nothing"
  instead of vibes — the searched-but-absent section is report content.
- **The gym** scores against the report class, not just deal rumors — bank v0
  measures the compass on twelve seeds; bank v1 measures the loop on the
  question class the product actually serves.

## The loop that makes it learnable

6. **The gym.** Self-scoring bank + the injected failure table + replay per
   change; per-probe deltas are the readout. Test what you fly — no forks of
   any prompt, threshold, or judge. Alive since T0: bank v0 + v1 (the report-class question), the F1-F28 table,
   and the first kill — FR-6's dual-string premise measured dead (100%
   agreement on 100 labeled claims) and redesigned to a C-class containment
   witness. Gate: `dr-instrument-validated` (met).

## Everything else is wiring

R8's synthesis, R9's rendering — real, but mostly reuse; specified at the
build order that needs them, not before. The ICD schemas' field-level shapes
are T1 order material.

## The acquisition trio (promoted at T1.7, order deep-research-t1e)

R1's planner, R4's query forming, and R5's triage were "wiring, deferred" in
the T1.5 spine; the t1d battery measured what deferring them cost — the v1
flight's thematic sub-questions never carried the figure tokens (Gini 0.5469,
Case-Shiller 325.78, the 95/20 ratio), so those keys were unreachable by any
downstream fix and the K-cut admitted by insertion order at all-0.9 ties.
T1.7 promoted the trio to mechanisms:

1. **R1's planner — figure-hunting sub-questions.** The plan prompt asks the
   draft to name the specific measure each sub-question implies (an index, a
   ratio, a share, a rate, a count, a median, a price, a percentage change)
   and the entities involved (cities, years) — shape, never bank vocabulary.
   The plan artifact records the question's own figure specifiers (its digit
   runs + measure-family words) and folds them into any sub-question that
   carries none, structurally, whatever the draft returned.
2. **R4's query forming — specifier fold-in.** A gap query with no figure
   specifier gets the question's specifiers appended; the floor-capped fact
   query already carries the claim's figures and never passes through here.
3. **R5's triage — figure-bearing admission.** `triage_hits` ranks score-first,
   then figure-bearing-ness (the hit's own title/snippet carries a digit),
   then insertion order, and records the rule it ran. The K-cut cannot
   silently exclude the hits the figures live in.

Gate: `dr-local-loop` — its T1.7 transition is the re-measured battery with
the plan-presence leg, per pre-registration.md.

## What the spine defers, on purpose

Design depth on any mechanism beyond its gate. The per-component contracts,
the field-level ICD schemas, and the redesign's remaining instrument work
(trigger shape, extraction mode, the adversarial sub-bank) get designed when
the build order that needs them opens — no earlier.
