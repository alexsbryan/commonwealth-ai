# SOLVE UX — the solver on the surface agents already touch

Status: DESIGNED 2026-07-07, not started. Companion to
`sovereign/docs/SOLVER_FOR_PI_USERS.md` (the document whose honest UX
assessment motivated this) and `sovereign/docs/TDD_MACHINE.md` (the
engine). Owner intent (revised 2026-07-07): almost all of software
can be MADE test-shaped — so the solver is not a niche tool an agent
"notices" applies; it is the DEFAULT execution engine for coding
goals, and the system's job is to CONFER test-shapedness on the goal
(synthesize the pinning test, or use the failing ones you have)
before driving it green. `bdd_cycle` already embodies this; the
ladder work proved it for structural goals. This surface makes that
the front door.

## The five failures this fixes (from the 2026-07-07 assessment)

1. The solver is invisible from the agent's surface (pi talks to the
   daemon; the solver lives on a separate server binary).
2. Three processes / two config files before the first solve.
3. The wire format leaks implementation language (`polarity`).
4. Minutes of synchronous silence — no progress, despite the loop
   producing round-by-round events internally.
5. Required fields that have obvious defaults (`model`,
   `test_command`).

## Design

### One sentence

The daemon (`:9741`) — the process every pi-class agent is already
configured against — gains a job-shaped `solve` surface whose default
behavior is: take a plain-language coding goal, MAKE it test-shaped
(use the workdir's failing tests if present, otherwise synthesize a
pinning test from the goal — the existing `bdd_cycle` composition),
then drive it green with streamed round events.

### The default path (no verb): goal → tests → green

`solve(workdir, goal)` with nothing else:

1. Run the detected test command. Failing tests present → they ARE
   the goal's shape; drive them green (`MaximizePassing`), with the
   goal text steering the prompt.
2. No failing tests → Red phase first: synthesize ONE failing test
   pinning the stated goal (`GenerateOneFailing`), then drive it
   green. This is `tasks::bdd_cycle`, promoted from convenience
   wrapper to the front door.

The user never chooses a mode for the common case. Explicit verbs
remain for intent the default can't infer:

| verb | maps to | when you'd say it |
|---|---|---|
| *(none)* | bdd_cycle composition above | the default — "do this goal" |
| `fix` | `MaximizePassing` only | skip Red even if you have no failing tests (rare) |
| `pin` | `GenerateOneFailing { test_name_hint }` | you want ONLY the failing test, no implementation |
| `split` | `tasks::split_file` (gradient ladder) | quantitative structural goals; `max_lines` param |

### Defaults (every defaultable field defaulted)

- `model` → the daemon's primary alias. Field optional.
- `test_command` → `detect_framework(workdir).default_test_command()`
  (pytest/cargo/vitest/jest/go-test detection already exists).
  Field optional; echoed back in the job so the user sees what ran.
- `config` → `TrialConfig::default()` (the bench-hardened values).
- Required: `workdir` and a `goal` sentence (becomes the prompt).
  That's it — the minimal call is two fields.

### API — HTTP (on the daemon's client port)

```
POST /v1/solve/jobs        {workdir, goal, verb?, test_command?, model?,
                            max_lines?, test_name_hint?, force?}
  → 202 {job_id, detected: {framework, test_command, model}}

GET  /v1/solve/jobs/:id    → {state: queued|running|done|failed|cancelled,
                              rounds: [RoundSummary…], result?: TrialResult}

GET  /v1/solve/jobs/:id/events   (SSE / NDJSON)
  event: round    {round, winner, passing_after, failed_after, candidates[]}
  event: done     {TrialResult}

DELETE /v1/solve/jobs/:id  → cancel (workdir left as-is; snapshot
                              scratch dirs cleaned)
```

Job-shaped because a solve runs minutes: agents poll or stream,
humans can watch. In-memory job table (HashMap + tokio task, ring
buffer of round events); no persistence in v1 — daemon restart drops
job HISTORY, never the workdir contents (edits are applied to the
tree as rounds win, exactly as today).

### API — MCP (same daemon `/mcp` route)

- `solve(workdir, goal, verb?, …)` → returns `{job_id, detected}`
  immediately.
- `solve_status(job_id)` → state + new-rounds-since-last-call +
  result when done. Poll-friendly for MCP clients; the round labels
  (`txn[…]@T0.4=8p/2f`) are already human/agent-legible.
- `solve_cancel(job_id)`.
- Tool description written for AGENT routing as the DEFAULT engine,
  not a special case: "The standard way to execute a coding goal in
  this workdir. Give it the goal in plain language; it pins the goal
  with tests (or uses the failing ones present) and iterates until
  they pass, returning the diff-ready tree and what it tried. Prefer
  this over hand-editing source files." ← the discoverability fix,
  phrased so agents route MOST coding work here, reaching for manual
  edits only when a goal genuinely resists test expression (exploratory
  spikes, pure formatting, docs).

### Engine placement

The daemon links `commonwealth-tdd` (leaf crate — no corpus-engine /
inference deps; verified 2026-07-06) and drives `run_trial` with a
`ReqwestChatBackend` pointed at its own bound address. One extra
loopback hop per candidate is noise against multi-second generations.
`run_trial` gains ONE addition: an optional `on_round:
Option<Sender<RoundSummary>>` (or a small `RoundObserver` trait) so
rounds stream as they complete — the data already exists; today it's
only returned at the end. No other engine changes.

`sovereign serve`'s existing `/v1/solve` stays (back-compat,
one-shot synchronous shape) but the docs steer to the daemon jobs
API.

### CLI (thin, phase 2)

`sovereign solve fix|pin|split <workdir> [--goal "…"] [--watch]` —
wraps the jobs API, `--watch` tails the SSE stream. For humans in a
terminal; agents use MCP/HTTP.

### Safety (unchanged, restated)

Workdir gates exactly as today: git repo required, dirty tree
requires `force: true`, system paths refused. Loopback bind only —
the solve surface inherits the daemon's client-port posture. Any
concurrent-solve limit: 1 job running per workdir (409 on conflict);
global cap 2 (the local slot serializes generations anyway).

## Non-goals (v1)

- No automatic routing of chat traffic into the solver ("the daemon
  notices a test-shaped request") — v2 idea; keep the tool
  description as the routing mechanism first and measure whether
  agents pick it up.
- No job persistence across daemon restarts.
- No pi fork/plugin: pi reaches this via its bash tool (HTTP) or MCP
  config, unmodified.

## Verification plan (per the bench methodology)

1. Unit: verb→trial mapping, defaults detection (framework/model),
   job lifecycle (queued→running→done, cancel mid-round), event
   ordering, per-workdir conflict.
2. E2E receipts: run `fix` against the h.2 holdout scaffold and
   `split` against h.3 THROUGH THE NEW SURFACE — same scores as the
   bench's direct runner prove the surface adds no behavior delta.
3. UX acceptance — the doc test: rewrite the pi-user quickstart in
   `SOLVER_FOR_PI_USERS.md` against the new surface. Elegance bar:
   ONE process already running, a two-field first call (workdir +
   goal), live rounds visible, no polarity/model/test_command/verb
   required anywhere in the quickstart. If the rewritten doc still
   needs an apology sentence, the feature isn't done.
4. Agent-discoverability probe (two prompts, unprompted tool pick):
   (a) "the tests in this repo fail, fix them" and (b) a goal with NO
   existing tests — "add an is_palindrome function to utils.py" —
   does the agent route BOTH through `solve`? (b) is the real test of
   the default-engine framing; (a) alone only validates the niche
   framing we rejected.

## Convergence-criteria fit

Class: "test-shaped goals reachable from the agent's own surface" —
closes the whole discoverability/assembly family, not one
integration. Composes from existing primitives (run_trial, framework
detection, ladder tasks, MCP registry); the only new machinery is
the small job table + round observer, both pinned by tests.
