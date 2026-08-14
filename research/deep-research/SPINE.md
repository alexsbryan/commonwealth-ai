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

## The loop that makes it learnable

6. **The gym.** Self-scoring bank + the injected failure table + replay per
   change; per-probe deltas are the readout. Test what you fly — no forks of
   any prompt, threshold, or judge. Alive since T0: bank v0, the F1-F28 table,
   and the first kill — FR-6's dual-string premise measured dead (100%
   agreement on 100 labeled claims) and redesigned to a C-class containment
   witness. Gate: `dr-instrument-validated` (met).

## Everything else is wiring

R1's planner, R4's query forming, R5's triage, R8's synthesis, R9's rendering
— real, but mostly reuse; specified at the build order that needs them, not
before. The ICD schemas' field-level shapes are T1 order material.

## What the spine defers, on purpose

Design depth on any mechanism beyond its gate. The per-component contracts,
the field-level ICD schemas, and the redesign's remaining instrument work
(trigger shape, extraction mode, the adversarial sub-bank) get designed when
the build order that needs them opens — no earlier.
