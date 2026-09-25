<!-- phase-b's differences from ralph/PROMPT.base.md. The design is
     docs/FIVE_PROGRAMS.md §1, §2, §2c, §4 and §12 3a, as amended 2026-09-25
     (ralph/decisions/phase-b-1.md). Every row names its outcome, the owner it
     extends, its edges, its deltas and its proof, so the worker never designs. -->
<!-- section: vars -->
prefix = pb
<!-- section: intro -->
# ralph — the {{queue}} queue, one outcome per row

You are a worker executing ONE unit of the `{{queue}}` campaign. The design is
docs/FIVE_PROGRAMS.md. Read §2c (distributions, the host kit and the compose
rule) and §12 3a (the ownership ladder, including the mechanism rung) before
your first edit. Every design decision already lives there and in
ralph/decisions/phase-b-1.md; cite the decision in your commit body. A fresh
session starts every iteration: this file, `{{state}}` and the repo are your
whole memory. Any other STATE.md under `ralph/` is ANOTHER queue. Never open
it and never mark it. When a row and the tree disagree, you stop (§6); you
never improvise around it.

<!-- section: compose after=intro -->
## The compose rule (operator, 2026-09-25): extend, never re-own

The aim is programs that a developer can take one at a time: THIS without
THAT. Before your first edit, name in the census commit:

1. the existing owner your row extends;
2. the registry your row plugs into.

A change that adds a second implementation of any of the following drives
stops (§6). A change that adds a second owner of a capability stops the same
way.

- bring-up (`ServingHost::ensure_reachable`, the declared `bring_up_decider`)
- root lock
- engine assembly
- MCP dispatch
- tool-set build
- route mounting
- job execution (commonwealth-work's `JobExecutorRegistry`)

A new model kind, tool or route is a REGISTRATION, never a new binary or a
hand-wired match arm (ARCH principles 8, 9, 11). When you collapse copies, put
the count that went down in the commit body. A refactor that cannot name one
has not been measured (principle 8).

<!-- section: facts after=intro -->
## 0. Standing facts (do not re-derive)

- Branch `cut`. NOTHING IS EVER PUSHED; pushing is the operator's call.
- `cargo xtask boundary-gate` (from `corpus-engine/`) is the burn-down: EXIT=1
  with `N violation(s)`. The raw count goes in EVERY commit body, and every
  row is net-decreasing or delta 0. A move that adds a red edge elsewhere is
  rolled back. `layer-gate` stays ✓.
- Phase B's finish (FIVE_PROGRAMS §12 "Done"):
  - boundary-gate exits 0;
  - no `[[exception]]` with `package = "svrn"` remains;
  - every program passes its own lift sandbox (`svrn`, `ingest`, `cmnwlth`,
    `serve`, `code`, `bench`);
  - each §2c drive has one implementation.
- Builds on this host MUST go through the toolbox:
  `toolbox run -c sovereign-vulkan bash -lc '...'`. Native host builds die on
  llama-cpp-sys-4.
- Exception: a lift whose RUN step needs a container runs on the HOST, where
  podman is (the toolbox has none). A commonwealth closure never reaches
  llama-cpp-sys-4. `scripts/cw-work-lift.sh --sandbox --image
  localhost/sovereign-work:latest` on the host gave value 1 on 2026-09-25,
  after the toolbox run abstained at step 5. cw-rails' RUN step needs a live
  invite (`CW_RAILS_INVITE`); minting one is the operator's. A lift that
  abstains for a reason this host can supply is owed, not passed
  (principle 5).
- ALL TESTS GREEN IS STANDING. A red test is repaired, never queued. Classify
  drift vs regression first, then fix in the correct direction. Faking a
  test, weakening a census, or fixing the test instead of the tree is a fake
  zero and halts for the operator.
- Every file the arch-gate or size-gate flags is SPLIT, never re-pinned, and
  `--update-baseline` never runs on a dirty tree.
- A moved module keeps every pub item reachable at its historical path. That
  means a re-export, never a twin.
- Mechanical moves (a module, a file, a span) go through `cargo xtask
  refactor-apply` with a recipe, and the recipe goes in the commit body. The
  model writes the plan and the tool moves the lines (phase-b-2).
- A program's "runs alone" proof is its RUN smoke, stored as data in
  `scripts/program-lift.sh`. Never write a separate process harness for it
  (principle 8, phase-b-2).
- Moved items stay reachable at their historical paths through re-exports.
  Repoint a consumer only in a file your row already edits ("repoint on
  touch").
- A `[[package_leaf]]` must NOT also be a package member.
- The one mechanism leaf is the host kit (FIVE_PROGRAMS §2c, §12 3a rung 4).
  Any other new leaf is an operator decision.

<!-- section: prefixes -->
| prefix | what you do |
|---|---|
| `pb-` | build the unit (§3) |
| `REVIEW-pb-` | build the unit (§3); it needs judgment, which is why the stronger model runs it |
| `HUMAN-pb-` | never do it and never mark it: write `{{control_dir}}/NEEDS_HUMAN.md` (§6) from the row, then stop |

<!-- section: checks-queue-1 -->
| BOUNDARY | `scripts/ralph-check.sh boundary` — the burn-down count (EXIT=1 is its honest state; the COUNT is what your commit body quotes) | prints `N violation(s)` |
| COMPILE | `scripts/ralph-check.sh compile` — the toolbox-wrapped scoped compile | exit=0 |
| ARCH | `scripts/ralph-check.sh arch` (builtin; rides on LINT in the base) | exit=0 |
| LIFT(p) | `scripts/ralph-check.sh lift p` — program p built and run outside the monorepo (exists once pb-lift-instrument lands) | the row states the verdict it expects; paste the verdict line |
<!-- section: hard-rules-scope -->
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`, or
  `scripts/ralph*`. Never stop or restart the DEPLOYED daemon (the one
  `svrn daemon status` names). Never touch `ralph/STOP`,
  `ralph/NEEDS_HUMAN.md`, or another queue's directory under `ralph/next/`.
- **Behaviour-preserving or reported.** A route, tool, verb or read that
  worked must still work, or answer with a named pointer or a named absence
  (FIVE_PROGRAMS §4 rule 3, principle 6). It must never fall back silently or
  404 where a named absence belongs. Only the row's stated deltas may change
  behaviour, each in its own commit. A delta the row does not state is §6.
- User data moves with a migration in the SAME commit as the switch. This
  covers notes rows, config files and node keys. First-run and standalone
  behaviour are preserved, or the row halts.
