# THE EXISTING JOURNEY MANIFEST REACHES 9 OF 322 COMMAND-SETTLEABLE REQUIREMENTS. Measured 2026-08-31 by adjudicating all 162 steps of…

THE EXISTING JOURNEY MANIFEST REACHES 9 OF 322 COMMAND-SETTLEABLE REQUIREMENTS. Measured 2026-08-31 by adjudicating all 162 steps of sovereign/docs/cli-contract.toml against quality/requirements.toml, step -> requirement, claiming only where the step's `expect` block goes red if the clause is violated.

THE DENOMINATORS, all read from quality/requirements-enforceability.toml (the one hand-authored column, pinned by kernel-types/tests/requirements_registry.rs):
  311 cli + 11 desktop = 322 a command can settle
  260 structural, 9 model, 34 review = the other 303
  582 of 625 are model-free; that number is PINNED, not printed.

THE JOURNEY-SIDE CEILING IS NOT 162. Of 162 steps, only 82 are claimable at all: 14 journeys carry a journey-level skip_live (no lane runs them) and many remaining steps are exit-only. 80 steps are structurally incapable of proving anything.

THE ADJUDICATED RESULT: 9 requirements over 13 steps.
  UI-17  first-run[2]            status writes its answer to stdout
  UI-16  first-run[7]            distinct exit code for backend-unreachable
  OP-17  first-run[7]            health check distinguishes unreachable from healthy
  KA-15  corpus-lifecycle[0]     bundled offline catalogue works with no network
  ST-6   enrich-atlas[1]         readiness from a positive completion signal, in-flight not mistaken
  CI-32  code-intel-lifecycle[2] every status response carries the liveness object
  CI-1   code-intel-answer[0..4] the five graph queries each answer (one clause per step)
  EV-29  contract-audit[0]       census splits steps-a-lane-runs from steps-nothing-runs
  CI-34  contract-audit[4]       posture aggregates every quality subsystem into one table

WHY SO FEW, AND IT IS NOT THAT THE JOURNEYS ARE BAD. Two causes, both structural:
(1) The journeys were written to prove THE CLI'S OWN PROMISES; the spec's clauses are a different question and nobody aimed a step at one.
(2) MOST REQUIREMENTS ARE CONJUNCTIONS and a single expect falsifies one conjunct. CI-24 "a decision record store MUST exist, queryable, REPLICATED ACROSS PEERS, holding decisions/invariants/todos/failed-attempts" is read back correctly by agent-notes[1] and a non-replicated store still passes. FE-119, RT-43, CI-25, CI-30, EV-27, OP-22 all failed for exactly this reason. resource-commons is the strongest sequence in the manifest — held / expired / free, with TTL lapse proven and expired explicitly not collapsed to free — and NO requirement's clause fits it.

THE RULE ADOPTED, and it is the load-bearing decision: claim when the step's assertion reddens on the requirement's PRIMARY obligation, and leave the residue unproven rather than unclaimed. Requiring total falsification maps almost nothing; accepting "touches the area" is the 35-overclaim failure of the unit-test attempt (note c76a70a7). CI-34 is the worked example: if `posture` stops aggregating, the drift row vanishes and the step reddens; if it keeps aggregating but drops the per-row refresh commands, the step stays green and that half is unproven.

WHAT THIS MEANS FOR THE PLAN. Annotating what already exists is nearly worthless — 9 of 322, under 3%. The value is in journeys written AT a requirement clause, which is what REQUIREMENTS.md PART V section 16 already specifies as A-1..A-19. Those 19 are parsed into the registry as [[scenarios]] with cites=[...] (78 citations) by kernel_types::conformance::AcceptanceScenario, and as of this note they are INERT: conformance_cmd reads [[claim]] and the journey manifest, never `scenarios`.
