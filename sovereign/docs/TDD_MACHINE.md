# TDD Machine

A backend that drives the **red-green-refactor** discipline for any
harness that calls it. **One solver loop, two polarities, composable
task wrappers.** Validated end-to-end against Darwin-36B on
2026-05-24 — lights-out at 8.33/9 mean (σ 0.47) across N=3,
language-agnostic structural templates wired, BDD intent → working
implementation in 29 seconds.

## What it does

| Polarity | Trigger | Fitness signal |
|---|---|---|
| `MaximizePassing` | Bug fix / refactor / multi-file split | Strict increase in passing-test count |
| `GenerateOneFailing` | Write a test for a new behavior | Exactly one new failing test, no regressions |

Both polarities run the same `run_trial` loop: parallel K
candidates at varied temperatures, monotonic improvement gating,
stall detection. The model is treated as a stochastic search
process; variance is the resource, tests are the only honest judge.

## Convenience task wrappers

Most callers won't build a `Trial` by hand — they use one of the
preset task wrappers in `sovereign_tdd::tasks`:

| Task | What it does | Polarity |
|---|---|---|
| [`make_failing_tests_pass`] | Drive currently-failing tests to passing | MaximizePassing |
| [`write_failing_test`] | Generate ONE failing test for a behavior | GenerateOneFailing |
| [`split_file`] | Generate a structural `max_file_size` test, then drive it | MaximizePassing |
| [`bdd_cycle`] | Natural-language intent → synthesized test → driven implementation | Both (composed) |

Tasks are 20–50 line files in `crates/sovereign-tdd/src/tasks/`.
Adding a new task = adding one file. No new core machinery.

## Where it lives

- **`sovereign-tdd` crate** — the loop (`trial.rs`), the
  `ChatBackend` trait + `Workdir` gate, the shared primitives
  (`EditAction`, `apply_edit`, `snapshot_dir`, `run_tests`), and
  the `tasks/` directory of convenience wrappers.
- **`sovereign-server`** — HTTP at `POST /v1/solve` and
  `POST /v1/cycle/bdd`; MCP tools `tdd_solve` and `tdd_bdd_cycle`.
- **`sovereign-agent-bench`** — `search` runner is a thin adapter
  over `sovereign_tdd::run_trial`.

## Workdir safety

Every solver takes a typed `Workdir` token that's only constructible
via `Workdir::check_safe(path, force)`. Refuses three classes:

- **`SystemPath`** — `/`, `/etc`, `/usr`, `/var`, `$HOME`, etc. Never bypassable.
- **`NotAGitRepo`** — the loop needs `git restore` for rollback. Never bypassable.
- **`UncommittedChanges`** — bypassable with `force=true` when the
  operator has consciously staged unrelated work.

A miswired call can't compile against an unvetted path — ARCH §7.1.

## Language support

The structural-test templates and framework auto-detection support
five frameworks out of the box:

| Framework | Detection signal | Default test command |
|---|---|---|
| pytest | `pyproject.toml` / `pytest.ini` / `conftest.py` / `tests/test_*.py` | `pytest -q` |
| cargo | `Cargo.toml` | `cargo test --quiet` |
| vitest | `package.json` with `"vitest"` | `npx vitest run` |
| jest | `package.json` with `"jest"` | `npx jest` |
| go test | `go.mod` | `go test -json ./...` |

`tasks::split_file` emits the structural-test file in the project's
actual framework (pytest's `test_max_file_size.py`, cargo's
`tests/max_file_size.rs`, etc).

## HTTP

```bash
# Unified solver — power-user surface. Pick your own polarity.
curl -X POST http://localhost:9741/v1/solve \
  -H 'content-type: application/json' \
  -d '{
    "workdir": "/path/to/project",
    "model": "commonwealth/primary",
    "prompt": "make the failing tests pass",
    "test_command": "pytest -q",
    "polarity": { "kind": "maximize_passing" }
  }'

# BDD cycle — natural-language intent. The system synthesizes the
# test then drives the implementation green.
curl -X POST http://localhost:9741/v1/cycle/bdd \
  -d '{
    "workdir": "/path/to/project",
    "model": "commonwealth/primary",
    "intent": "the cache evicts items when size limit is reached",
    "test_file_hint": "tests/test_cache_eviction.py",
    "test_command": "pytest -q",
    "review_mode": "auto"
  }'
```

Status codes:
- `200 OK` — solver ran; payload carries the structured `TrialResult`.
- `400 Bad Request` — unknown `review_mode`, etc.
- `422 Unprocessable Entity` — workdir refused by the safety gate.

## MCP

Same two tools at the daemon's `/mcp/message` endpoint:

```json
{
  "method": "tools/call",
  "params": {
    "name": "tdd_bdd_cycle",
    "arguments": {
      "workdir": "/path/to/project",
      "model": "commonwealth/primary",
      "intent": "the cache evicts items when size limit is reached"
    }
  }
}
```

Localhost-only enforcement applies (same as the Code Intelligence
tools).

## Anti-failure mechanisms

Four mechanisms work together to make the loop robust:

1. **Anti-plateau restart slot.** When the loop stalls for ≥1
   round, candidate 0 of the next round snapshots from the pristine
   baseline (not the carried-forward winner) and is prompted to try
   a different architectural approach. Eliminated plateau-stall as
   a failure mode in the 2026-05-24 N=5 probe.

2. **Syntax validator wiring.** When the bench passes a syntax
   validator, the executor rejects malformed code at apply time
   with cargo-shape error messages instead of writing it and
   failing opaquely at test collection.

3. **Error feedback to next round.** Bucketed errors from the
   previous round (parse / apply / backend / snapshot) surface in
   the next round's prompt as `## What failed last round` with
   full `render_for_agent()` text. The model sees specifically
   what went wrong and what to avoid. Largest contributor to the
   lights-out variance collapse (σ 3.56 → 0.47).

4. **Polarity-aware acceptance.** The `is_strict_improvement`
   predicate flips with polarity: `MaximizePassing` accepts when
   `passed` strictly increases; `GenerateOneFailing` accepts only
   when exactly one new failing test appeared without regressing
   any passing test. Same loop, two contracts.

## Validation status

| Probe | Mean | σ | Best | Notes |
|---|---|---|---|---|
| Lights-out (post-collapse, all fixes) | 8.33/9 | 0.47 | 9/9 | 100% completions, N=3 |
| BDD cycle on calc.evaluate intent | — | — | 29s | synth + green end-to-end, real model |
| Multi-file split_file probe | 78 lines max | — | 97 → 78 | language-agnostic dispatch validated |

## Configuration

Override the provider URL the solver loop posts chat completions to:

```bash
SOVEREIGN_TDD_PROVIDER_URL=http://localhost:9741 sovereign serve
```

Defaults to the server's own bind address. Per-request tuning
(candidates_per_round, rounds_per_trial, temp_ladder, …) goes in
the request body's `config` field.
