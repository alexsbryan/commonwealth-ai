# `sovereign atos run` — smoke test plan

This file documents how to validate the runner against the
`atos-experiment-oicp-types` workdir without spending real LLM
cycles. It is a runbook, not a test crate — the unit tests that
ship next to `run.rs` cover the pure-function pieces.

## Prerequisites

- `sovereign-cli` builds clean (lint watcher reports `fresh_passing`).
- The `atos-experiment-oicp-types` repo at
  `~/dev/atos-experiment-oicp-types/` has `oicp-v0.3.md`,
  `ARCHITECTURE.md`, `IMPLEMENTATION_PLAN.md`, and
  `.sovereign/features/oicp-core/spec.md` present (already true
  per the project's current state).
- For the `--dry-run` smoke (steps 1–4): no daemon needed.
- For the live smoke (step 5+): `sovereign daemon status` reports a
  loaded chat slot for the reviewer model (default
  `FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L`).

## 1. Help renders without arguments

```
sovereign atos run --help
```

Expected: usage block lists `--workdir`, `--design`, `--charter`,
`--plan`, `--feature-id`, `--driver`, `--max-iters`,
`--reviewer-model`, `--done-marker`, `--dry-run`. Exit 0.

## 2. Required-flag check fails cleanly

```
sovereign atos run
```

Expected: stderr `atos run: missing --workdir <path>`, help
follows, exit 2.

## 3. Workdir validation

```
sovereign atos run --workdir /this/does/not/exist
```

Expected: stderr `atos run: --workdir not a directory: …`,
exit 2.

## 4. Dry-run on the experiment repo composes iter-1 prompt

```
sovereign atos run \
  --workdir ~/dev/atos-experiment-oicp-types \
  --design oicp-v0.3.md \
  --feature-id oicp-core \
  --max-iters 1 \
  --dry-run
```

Expected:
- `atos run: workdir = …` block names oicp-v0.3.md as the design,
  no charter, IMPLEMENTATION_PLAN.md as the plan.
- `atos run: feature=oicp-core run_id=…` line printed.
- `atos run: [dry-run] iter 1: would spawn opencode with prompt
  …/iter-001/prompt.md (N bytes)`.
- `~/.svrnmesh/runs/<run-id>/iter-001/prompt.md` exists.
- That file contains: `# ATOS run`, the design content, the plan
  content, the DONE contract block, and `Starting fresh`.
- Exit 0.

## 5. Live run (one-iteration test)

This burns reviewer model cycles. Use only after step 4 passes.

```
sovereign atos run \
  --workdir ~/dev/atos-experiment-oicp-types \
  --design oicp-v0.3.md \
  --feature-id oicp-core \
  --max-iters 1
```

Expected:
- opencode subprocess spawns inside the workdir; stdout/stderr
  inherits to the operator's terminal.
- After opencode exits, the runner looks for `DONE.md`. If absent,
  iter-001 records `verdict=no_done` in
  `~/.svrnmesh/runs/<run-id>/iterations.jsonl`. If present, the
  reviewer is called and the verdict lands in
  `iter-001/verdict.json`.
- Exit 0 if accepted, 1 otherwise.

## 6. Audit hook

After step 5, the run is visible to the existing audit machinery:

```
sovereign-eval finalize-run <run-id> \
  --experiment-repo ~/dev/atos-experiment-oicp-types
```

Expected: a `manifest.json` lands in
`~/.svrnmesh/runs/<run-id>/` summarising tool events, notes, and
the iteration record stream.
