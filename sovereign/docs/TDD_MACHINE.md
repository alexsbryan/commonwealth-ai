# TDD Machine

A backend that automates the **red-green-refactor** discipline for any
harness that calls it. Three solvers, one registry, two transports
(HTTP + MCP). Validated 2026-05-24 across all three phases.

## What it does

| Phase | Tool | Trigger | Fitness function |
|---|---|---|---|
| **Red** | `tdd_red` | "I have a new behavior to add" | Generated test FAILS on baseline (discriminating) |
| **Green** | `tdd_green` | "At least one test is failing" | More tests passing each round |
| **Refactor** | `tdd_refactor` | "Tests pass, code quality should improve" | Tests stay green AND structural metric improves |

Each phase runs a **parallel-candidate search** loop: K candidates at
varied temperatures, monotonic improvement gating, no defensive
parsing. The model is treated as a stochastic search process;
variance is the resource, tests are the only honest judge.

## Where it lives

- **`commonwealth-tdd` crate** — the loops themselves. Workdir gate,
  ChatBackend trait, SolverRegistry, three solvers (`RedSolver`,
  `GreenSolver`, `RefactorSolver`), shared primitives (`EditAction`,
  apply, snapshot, test_runner, source discovery, output parsers).
- **`sovereign-server`** — HTTP route `POST /v1/solve/{tdd_red |
  tdd_green | tdd_refactor}` and MCP tools `tdd_red`, `tdd_green`,
  `tdd_refactor` (both transports share the same registry).
- **`sovereign-agent-bench`** — `search` runner is now a thin adapter
  over `commonwealth_tdd::SolverRegistry`.

Design doc: `sovereign/docs/TDD_MACHINE_DESIGN.md`.

## Workdir safety

Every solver takes a typed `Workdir` token that's only constructible
via `Workdir::check_safe(path, force)`. Three classes refused:

- **`SystemPath`** — `/`, `/etc`, `/usr`, `/var`, `/bin`, `/sbin`,
  `/lib`, `/boot`, `/root`, `$HOME`. Never bypassable.
- **`NotAGitRepo`** — the loop assumes `git restore` as the safety
  net. Never bypassable.
- **`UncommittedChanges`** — bypassable with `force=true` when the
  operator has consciously staged unrelated work.

A miswired call can't compile against an unvetted path — ARCH §7.1.

## HTTP

```bash
# Red — write a failing test
curl -X POST http://localhost:9741/v1/solve/tdd_red \
  -H 'content-type: application/json' \
  -d '{
    "workdir": "/path/to/project",
    "model": "commonwealth/primary",
    "params": {
      "phase": "red",
      "behavior": "cache evicts on size limit",
      "test_command": "pytest -q"
    }
  }'

# Green — drive an implementation
curl -X POST http://localhost:9741/v1/solve/tdd_green \
  -d '{
    "workdir": "/path/to/project",
    "model": "commonwealth/primary",
    "params": {
      "phase": "green",
      "test_command": "pytest -q"
    }
  }'

# Refactor — rename a symbol while keeping tests green
curl -X POST http://localhost:9741/v1/solve/tdd_refactor \
  -d '{
    "workdir": "/path/to/project",
    "model": "commonwealth/primary",
    "params": {
      "phase": "refactor",
      "test_command": "cargo test",
      "target": {
        "kind": "rename_symbol",
        "old": "legacy_name",
        "new": "fresh_name"
      }
    }
  }'
```

Status codes:

- `200 OK` — solver ran; payload carries the structured
  `RedResult` / `GreenResult` / `RefactorResult`.
- `400 Bad Request` — unknown `solver_id`, or path/body phase
  mismatch (e.g. `/v1/solve/tdd_red` with Green params).
- `422 Unprocessable Entity` — workdir refused by the safety gate
  (response body carries `kind: "system_path" | "uncommitted_changes"
  | "not_a_git_repo"`).

## MCP

The same three solvers are registered as MCP tools at the daemon's
`/mcp/message` endpoint:

```json
{
  "method": "tools/call",
  "params": {
    "name": "tdd_red",
    "arguments": {
      "workdir": "/path/to/project",
      "model": "commonwealth/primary",
      "behavior": "cache evicts on size limit"
    }
  }
}
```

MCP localhost-only enforcement applies (same as the Code Intelligence
tools).

## v1 Refactor targets

All single-file. Multi-file (`SplitFile`, cross-file rename,
`RemoveDuplication`) is deferred to v2's multi-turn loop, per the
design's 2026-05-24 probe findings (model under-emits in single-
emission multi-file refactor — 5% per-candidate, 20% best-of-K=4).

| Target | What it does | Metric ("lower is better") |
|---|---|---|
| `extract_function { name, into_path }` | Pull `name`'s body into `into_path`; rewrite original site to call it | LOC of file containing `name` |
| `inline_function { name }` | Replace every call with the function's body; remove the definition | count of `def NAME(` / `fn NAME(` across source |
| `rename_symbol { old, new }` | Replace word-bounded `old` with `new` | count of `\bold\b` in source |
| `reorder_top_levels { path }` | Sort top-level declarations by convention | `1` if file unchanged, `0` if reordered |

## Validation status

- **Red** — 92% PASS_AS_RED across N=25 (2026-05-24 prototype).
  Unit-tested in `commonwealth-tdd/tests/red_loop.rs`: accept on
  discriminating failure, reject on tautology, reject on structural
  error, refuse empty behavior.
- **Green** — median 20/20 on 4.2-mini-evaluator (5-bug cascading)
  vs role-loop's 0-3/9. Unit-tested in
  `commonwealth-tdd/tests/green_loop.rs`: all-passed short-circuit,
  stall on no-improvement, strict-improvement promotion, no-baseline
  backend short-circuit.
- **Refactor** — unit-tested in
  `commonwealth-tdd/tests/refactor_loop.rs`: NoTestCoverage refusal,
  Improved on metric+test win, Stalled on candidates that break
  tests.

## When to use which transport

- **HTTP** — programmatic callers (Pi extension, CI hooks, scripts).
  Always available.
- **MCP** — interactive coding agents (Claude Code, Cursor, Cline).
  Localhost-only.
- **Bench** — the `search` runner uses `commonwealth-tdd` internally;
  invoke via `sovereign-agent-bench --agent search`.

## Configuration

Override the provider URL the solver loop posts chat completions to:

```bash
SOVEREIGN_TDD_PROVIDER_URL=http://localhost:9741 sovereign serve
```

Defaults to the server's own bind address. Per-request tuning
(candidates_per_round, rounds_per_trial, temp_ladder, …) goes in the
request body's `config` field.
