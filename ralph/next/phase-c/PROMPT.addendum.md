<!-- phase-c's differences from ralph/PROMPT.base.md. Phase C is Phase B's
     bugs and the cleanup above the operator's cut line (phase-b-97, -102); it
     runs as ralph's pool at 3 lanes once pc-pool-ready lands (phase-b-103).
     Every row names its outcome, its census pointers and its proof. -->
<!-- section: vars -->
prefix = pc
<!-- section: intro -->
# ralph — the {{queue}} queue, one outcome per row

You are a worker executing ONE unit of the `{{queue}}` campaign: a defect
Phase B found or introduced, or cleanup above the operator's cut line. Each
row's premises were written against an earlier tree; the site census (§3 step
2) is where you re-verify every one against today's. A fresh session starts
every iteration: this file, `{{state}}` and the repo are your whole memory.
Any other STATE.md under `ralph/` is ANOTHER queue. Never open it and never
mark it. When a row and the tree disagree, you stop (§6); you never improvise
around it.

<!-- section: compose after=intro -->
## Extend, never re-own (operator, 2026-09-25)

Phase B left every program takeable alone, and a fix keeps it so. Name in your
census commit the existing owner your row extends and the registry it plugs
into. A change that adds a second implementation of a docs/internal/FIVE_PROGRAMS.md
§2c drive (bring-up, root lock, engine assembly, MCP dispatch, tool-set build,
route mounting, job execution), or a second owner of a capability, stops (§6).
A model kind, tool or route is a REGISTRATION, never a new binary or a
hand-wired match arm (ARCH principles 8, 9, 11). When you collapse copies, put
the count that went down in the commit body.

<!-- section: facts after=intro -->
## 0. Standing facts (do not re-derive)

- Branch `cut`. NOTHING IS EVER PUSHED. Never amend and never rewrite
  history: other loops share this repo.
- `cargo xtask boundary-gate` exits 0 since Phase B
  and stays 0. BOUNDARY's line goes in EVERY commit body; a change that adds
  an edge is rolled back. `layer-gate` stays ✓.
- clone-gate rides on BOUNDARY (phase-b-99) and never rises. A row that
  collapses a family banks the drop with `cargo xtask clone-gate --tighten` in
  the same commit (phase-b-105). A copy made to avoid an edge is a phase-d
  question, never a fix.
- Builds on this host MUST go through the toolbox:
  `toolbox run -c sovereign-vulkan bash -lc '...'`. Native host builds die on
  llama-cpp-sys-4. Exception: a lift whose RUN step needs a container runs on
  the HOST, where podman is. cmnwlth's RUN founds its own two-node mesh and
  never touches the operator's mesh or roster. A lift that abstains for a
  reason this host can supply is owed, not passed (principle 5).
- ALL TESTS GREEN IS STANDING. A red test is repaired, never queued. Classify
  drift vs regression first, then fix in the correct direction. Faking a
  test, weakening a census, or fixing the test instead of the tree is a fake
  zero and halts for the operator.
- A file the arch-gate or size-gate flags is SPLIT, never re-pinned;
  `--update-baseline` never runs on a dirty tree.
- A moved item stays reachable at its historical path through a re-export,
  never a twin; repoint a consumer only in a file your row already edits.
  Mechanical moves go through `cargo xtask refactor-apply`, recipe in the
  commit body (phase-b-2).
- The one mechanism leaf is the host kit. A `[[package_leaf]]` is never also
  a package member; any other new leaf is an operator decision.
- A `-measure` row is a reading, not code. It runs alone; each reading names
  hash, n, host load and raw path, read in both directions. A miss reopens
  the code row and never re-tunes the bar (principle 7).

<!-- section: lanes after=build -->
## Pool lanes and decisions (phase-b-103)

In a POOL LANE (the note that opens this prompt says so):
- Edit `{{state}}` only to correct your own row's premises (§6). The pool
  marks your row after it merges your branch.
- A decision is minted as campaign `phase-c`:
  `scripts/ralph-decisions.py new phase-c --subject <unit-id> --who worker`.
  It writes `ralph/decisions/phase-c-<n>.md`; write only that file. Never run
  `ralph-decisions.py --write` and never edit ralph/DECISIONS.md: the pool
  regenerates it once after each merge (pc-pool-ready), and two lanes
  regenerating it is a merge conflict that halts the pool.
- The pool deletes your worktree, `target/` included, after the merge.
  Evidence a commit cites (a red line, a reading) is pasted, trimmed, into the
  commit body, never left only under `{{log_dir}}`.

Outside a lane (the serial loop, pc-pool-ready, a REVIEW row in the main
tree), mint the same way and run `scripts/ralph-decisions.py --write` in the
same commit.

<!-- section: prefixes -->
| prefix | what you do |
|---|---|
| `pc-` | build the unit (§3) |
| `REVIEW-audit-pc-` | full gate and principles review (§4); the serial loop inserts one every 10 units |
| `HUMAN-pc-` | never do it and never mark it: write `{{control_dir}}/NEEDS_HUMAN.md` (§6) from the row, then stop |

<!-- section: checks-queue-1 -->
| BOUNDARY | `scripts/ralph-check.sh boundary` — boundary-gate's count, then clone-gate's | prints `0 violation(s)`; your commit body quotes it and clone-gate's total |
| COMPILE | `scripts/ralph-check.sh compile` — the toolbox-wrapped scoped compile | exit=0 |
| ARCH | `scripts/ralph-check.sh arch` (builtin; rides on LINT in the base) | exit=0 |
| LIFT(p) | `scripts/ralph-check.sh lift p` — program p built and run outside the monorepo | the row states the verdict it expects; paste the verdict line |
<!-- section: hard-rules-scope -->
- **Scope guard (phase-b-29, -102).** Build only what the row states. A
  finding the row does not state goes in your last commit body as one line,
  `finding: <what, path:line> (phase-d | cleanup)`; the director files it,
  never above the cut line, and it never lands in this row's commits. If the
  census finds more than twice the row's LIFT, stop at census (§6) with the
  split.
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md` or `.claude/`. Never
  edit `scripts/ralph*` unless your row is pc-pool-ready, whose outcome is
  ralph's pool. Never stop or restart the DEPLOYED daemon (the one
  `svrn daemon status` names). Never touch `ralph/STOP`,
  `ralph/NEEDS_HUMAN.md`, or another queue's directory under `ralph/next/`.
- **Behaviour-preserving or reported.** A route, tool, verb or read that
  worked must still work, or answer with a named pointer or a named absence
  (FIVE_PROGRAMS §4 rule 3, principle 6), never a silent fallback or a bare
  404. Only the row's stated deltas change behaviour, each in its own commit.
  A delta the row does not state is §6.
- User data moves with a migration in the SAME commit as the switch (notes
  rows, config files, node keys). First-run and standalone behaviour are
  preserved, or the row halts.
