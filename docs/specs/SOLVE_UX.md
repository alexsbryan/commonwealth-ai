# SOLVE — give the daemon a coding goal, get a green tree back

Status: built + live-verified 2026-07-07, both paths. Daemon job
host in `sovereign-cli-daemon/src/daemon_cmd/solve_http.rs` (+ MCP
tools in `solve_tools.rs`, CLI in `sovereign-cli-llm/src/solve_cmd.rs`,
composition in `sovereign-tdd/src/tasks/solve.rs`). Fix path:
failing tests → reached with a minimal diff, rounds streamed live.
Pin-then-green path: no tests → synthesized failing tests → reached
(one round per stage live). Two engine fixes made the second path
real: `GenerateOneFailing` accepts ≥1 new tests when ALL fail
(was exactly-one — live receipts showed the model idiomatically pins
with 2-4 cases), and the test parsers count collection/compile
errors as failing entries so an import-failing pin test is visible
to both stages' fitness.

## The promise

```
solve(workdir, goal)
```

Two fields. The daemon — the process your agent already talks to —
makes the goal test-shaped (uses your failing tests if you have
them; writes the one failing test that pins the goal if you don't),
then iterates until the tests pass. You watch rounds land live, and
you review the result with `git diff`.

Almost every coding goal can be made test-shaped. So this isn't a
special tool an agent reaches for on special problems — it's the
standard way to execute a coding goal. Manual editing is the
fallback, for the rare goal that resists a test.

## The contract

- **In**: `workdir` (a git repo), `goal` (plain language). Everything
  else optional: `verb` (`fix` / `pin` / `split --max-lines N`) when
  the default inference isn't what you meant, `test_command` /
  `model` / `force` when auto-detection isn't.
- **Out, immediately**: a job id + what was detected (framework, test
  command, model).
- **Out, streaming**: one event per round — what won, tests
  before/after, what each candidate tried.
- **Out, at the end**: `reached` (green — diff and commit), `improved`
  (progress, call again), `stalled` (honestly stuck, with the full
  record of what it tried), or `no_baseline` (no tests found and none
  could be written — the one true failure).
- **Never**: edits outside the workdir, edits to a dirty tree without
  `force`, anything on `/` or `$HOME`.

## The surface

On the daemon's existing port — no new processes, no extra config:

```
POST   /v1/solve/jobs            → 202 {job_id, detected}
GET    /v1/solve/jobs/:id        → state + rounds + result
GET    /v1/solve/jobs/:id/events → SSE round/done events
DELETE /v1/solve/jobs/:id        → cancel
```

MCP (same daemon): `solve`, `solve_status`, `solve_cancel`. The tool
description tells agents this is the standard engine for coding
goals — that sentence is the discoverability mechanism.

CLI: `sovereign solve <workdir> "goal" [--watch]`.

## Done means

1. The pi-user quickstart rewrites to: one process already running,
   one two-field call, live rounds, zero jargon. No apology
   sentences.
2. The holdout problems (h.2 fix, h.3 split) score the same through
   this surface as through the bench runner — the surface adds no
   behavior.
3. A fresh agent given "add an is_palindrome function to utils.py" —
   no existing tests — routes it through `solve` unprompted.

## Implementation notes (for the builder, not the user)

Daemon links `sovereign-tdd` (leaf crate); backend = its own /v1
chat endpoint. Default path = `tasks::bdd_cycle` composition (failing
tests → MaximizePassing; none → Red then green). `run_trial` gains
one optional round-observer hook — the only engine change. In-memory
job table, ring-buffered events, 1 job per workdir / 2 global.
`sovereign serve`'s `/v1/solve` stays for back-compat.
