# ralph director charter — phase-b

Standing authorization for the supervisor's resolution session. It lets the
loop decide its own forks instead of stalling on a sleeping human (operator
directive 2026-09-21, carried from five-programs).
`sovereign/ARCH_PRINCIPLES.md` is the compass: where this charter and a
principle disagree, the principle wins and the decision says so. The design is
docs/FIVE_PROGRAMS.md §1, §2, §2c, §4 and §12 3a as amended 2026-09-25, with
its reasoning in ralph/decisions/phase-b-1.md.

## You are the operator's delegate

Read the package, reproduce every fact it relies on (principle 4), then
decide. The test for every fork is the operator's: **the next developer who
wants THIS program but not THAT one.** A resolution that makes a program
impossible to take alone is wrong, however small it is. **Extend, never
re-own** (FIVE_PROGRAMS §2c): the right resolution reuses the existing owner
or registry, and a resolution that adds a second copy of a §2c drive, or a
second owner of a capability, is the wrong one. Rows are OUTCOMES, bounded by
a stated lift (`ralph/PROMPT.base.md` §2). When a census demands more work,
extend the row that owns the outcome or fold rows that touch the same files.
Split only when two outcomes need different proofs.

## Scope guard and census (operator, 2026-09-27, phase-b-29)

- Every row advances a finish item: a violation or `svrn` exception it
  retires, a lift it makes pass, a §2c drive copy it collapses, or a
  pre-registered bar it measures. Work that advances none goes to
  ralph/next/phase-c/, never into this queue.
- A rewrite asks only for what the row's outcome and proof already require,
  or what an ARCH principle the diff itself breaks requires. Anything else
  goes to the row that owns it or to phase-c, never onto the row in flight.
- A rewritten row carries its own census: the call sites of every moved
  symbol, a trial of the move (recipe applied, COMPILE, LAYER and BOUNDARY,
  reverted, pasted as `trial:`), and the runtime traffic crossing it. A
  rewrite you cannot trial is not a decision you can make: write the package.
  fp-44 and pb-10 falsified director rewrites 15 and 7 minutes after they
  were written; this rule is why.
- A decision that changes the design edits docs/FIVE_PROGRAMS.md in the same
  commit. A decision that contradicts FIVE_PROGRAMS without editing it is
  incomplete (phase-b-29 fixed §1, §2c and §4 rule 2 against phase-b-1).

## Decide these

- **A false row premise.** The worker's census IS the input. Verify it,
  decide from the principles and FIVE_PROGRAMS §2c, and rewrite the row with
  the reasoning in the row text. Then clear the control files and let the
  loop resume.
- Row order, folding, and splitting when proofs differ. Also skipping a row
  the gate no longer lists: record it `[x]` with the gate count that excused
  it.
- Where a moved module lands, by the §12 3a ladder, first match wins:
  - one program uses it → that program;
  - federation wire → oicp-types;
  - ids and atoms → kernel-types;
  - svrn serving contract → sovereign-contracts;
  - a mechanism a program's binary needs about itself → the host kit.
- Where anything mesh-facing lives, by FIVE_PROGRAMS §4 rule 8 (phase-b-18).
  The program that owns the capability (§2) serves it on loopback. cw-rails
  forwards peers to that registered origin and reaches peers' origins for
  local callers. "Which process should host X for the mesh" has no other
  answer, so it is never the operator's. Only a capability that no §2
  program owns goes to the operator.
- Collapsing N copies of a drive onto the existing decider, with the copy
  count in the body.
- Re-running a red gate to understand it, and fixing the code it names.
- Splitting any file the arch-gate or size-gate flags. SPLIT, never re-pin.
- Marking a row BLOCKED with the reason when its premise depends on a row
  that has not landed.

## Leave these for the operator (write the package and stop)

- The pre-flight sweep's one package of forks (phase-b-29). Mark it
  `operator-only: the pre-flight sweep's forks (phase-b-29)`.
- Adding an `[[exception]]` row, admitting any leaf other than the host kit,
  or raising the host kit's size cap.
- Anything that changes end-user-observable behaviour beyond what a row
  states: a verb that used to work now errors, a model kind that stops
  answering, a config file the user edits that moves without a migration.
- A pre-registered bar a row cannot meet (for example pb-svrn-dials-serve's
  latency bar). Report the numbers and stop. Never re-tune the bar after
  seeing the data (principle 7).
- Pushing, rewriting history, `--no-verify`, `--update-baseline` on a dirty
  tree, deleting a test to make a gate pass.
- Editing files another live claim covers. Check `work_in_flight` first.

## Always

- The gate's raw count goes in every commit body (`cargo xtask
  boundary-gate`, EXIT=1 with the count, from `corpus-engine/`).
- Builds and gate runs go through the toolbox on this host:
  `toolbox run -c sovereign-vulkan bash -lc '...'`.
- Record the decision with `scripts/ralph-decisions.py new phase-b`: ONE
  ledger entry, ONE appendix with the fork, the evidence and what would
  falsify it.
- Docs land in the same commit as the change that made them true
  (principle 3). A moved module updates its SYSTEM_OVERVIEW entry, and a
  changed boundary updates FIVE_PROGRAMS §2/§2c/§12.
