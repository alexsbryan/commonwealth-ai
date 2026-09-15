# ralph — a commit-driven campaign loop

`scripts/ralph.py` runs a coding-agent campaign until the repo says it is done,
and it cannot resolve to quietly stuck: every terminal state is DONE, an
operator stop, or an escalation. One Python state machine — a typed queue
parser, a session layer, and three explicit FSMs — replacing the shell drivers
(`ralph-{loop,supervise,pool,watch}.sh`) that had grown into string matching.

## Run it

```sh
nohup python3 scripts/ralph.py supervise --workdir . --label ring1 \
  -- python3 scripts/ralph.py run --workdir . --label ring1 \
  --prompt ralph/PROMPT.md --state ralph/STATE.md --notify \
  >> ralph/log.txt 2>&1 &
tail -f ralph/log.txt
```

One process, one append-only log; the agent's output streams into it live. Stop
with `touch ralph/STOP` — an EMPTY file is the operator's; a halt writes its
reason into it. For a detached Mac job, add `--install-launchd` to `supervise`
and `watch` and run the printed `launchctl bootstrap`.

## The model

- **The queue is `ralph/STATE.md`** — typed and validated at load (duplicate
  ids and unknown dependencies are errors, not silent misses). The unit is the
  first `[~]` row, else the first dependency-ready `[ ]` row.
- **Progress is a commit** in the serial driver (the stall bound halts and
  escalates) and **a unit completed** in the supervisor (a resolution that
  changes nothing escalates immediately; a resolver committing junk cannot
  reset the bound).
- **Commit as you go.** A session killed at any moment costs at most the
  in-flight step; the next session is told the tree is dirty.

## The escalation gates

- a session past `--session-timeout` is killed (its process group), and its
  uncommitted work stays in the tree;
- `--max-stall` consecutive no-commit iterations halt with a package;
- a ready `HUMAN-` row stops before any session (operator-only);
- `ralph/waiting` is bounded (`--marker-timeout`, default 7200s) — a detached
  run that never writes its marker halts with a package;
- every poll tick writes `ralph/.heartbeat`; `watch` notifies when it goes
  stale, when a package sits unresolved, when the job is down without
  DONE/operator-STOP, or when disk drops below 5 GB;
- an I/O error (a full disk) halts with a package instead of a traceback.

Markers: `ralph/DONE` (complete), `ralph/STOP` (empty = operator, non-empty =
halt), `ralph/NEEDS_HUMAN.md` (the decision package).

## Build hygiene

A campaign that builds accumulates: measured 2026-09-15, this workspace's
`target/` reached 100G — 47G incremental, 35G deps, and 503 crates holding more
than one rlib from differing feature sets — and the disk ceiling took the loop
down twice. So a unit opens with a clean canonical build
(`scripts/dev-build.sh --clean`, ~5 min here) and every later build goes
through that entry or the check scripts, never bare `cargo`: a `-p` build
resolves features differently and rebuilds the dependents twice.

## Parallel lanes

`pool` runs a ring's ready units concurrently, each in its own git worktree on
its own branch; waves of up to `--lanes`; merges are serial; a conflict aborts
and halts, never auto-resolved. REVIEW units run serially in the main tree.

```sh
nohup python3 scripts/ralph.py supervise --workdir . --label ring2 \
  -- python3 scripts/ralph.py pool --workdir . --label ring2 \
  --prompt ralph/PROMPT.md --state ralph/STATE.md --lanes 2 --notify \
  >> ralph/log.txt 2>&1 &
```

A lane session writes `ralph/lanes/<unit>.done` (committed) when the unit
passes its own tests; the pool merges a lane whose marker is present. Lanes do
not edit `STATE.md`; the pool marks a unit `[x]` after merging.

## Models and tests

`ralph/models.env` (per-host, gitignored) holds `MODEL`, `REVIEW_MODEL` and
`VARIANT`; any unit id containing `REVIEW` routes to the review model.
`python3 scripts/ralph.py models --model M --review-model R [--variant high]
--label L` writes it and restarts the loaded job. The FSMs are tested
in-process, no model calls: `python3 scripts/tests/ralph.py`.
