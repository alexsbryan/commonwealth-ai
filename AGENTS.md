# Agent instructions — commonwealth-ai

## Quick facts

- **Multi-workspace monorepo** with three Cargo workspaces: `corpus-engine`, `sovereign`, `commonwealth`
- **No single root** `Cargo.toml` — each subdirectory is its own workspace
- **Shared MCP server** at `http://localhost:9741/mcp` — use before reading files
- **Build/test scripts** in `/scripts/` run across all workspaces, not raw cargo

## Required session start

```bash
read_notes(query: "active")
project_context("<task area>")
recent_changes(hours: 24)
```

## Builds and tests

```bash
# Lint all three workspaces (runs cargo check in parallel)
./scripts/sovereign-lint.sh

# Test all three workspaces
./scripts/sovereign-test.sh
```

- `corpus-engine`: test with `--features treesitter`
- `sovereign`, `commonwealth`: test without feature flags
- Do NOT run `cargo check/test` directly — it contends with the sovereign watcher

## Architecture

```
commonwealth-ai/
├── commonwealth/      # Mesh coordination daemon (runs at localhost:9741)
├── sovereign/      # Local AI + code intelligence server
├── corpus-engine/  # Knowledge base engine
├── oicp-types/    # Shared protocol types (used by both)
├── sovereign-recipes/  # Data recipes
└── scripts/       # Build/test wrappers
```

** commonwealth ≠ sovereign**. They are peer projects, not parent/child. The Commonwealth mesh daemon serves a local API that sovereign uses for inference routing.

## Session discipline

- Use `write_note(kind: "invariant")` when you discover never-violate constraints
- Use `write_note(kind: "decision")` when you choose an approach
- Use `session_reflection` at task end

## Pre-flight before editing

- Before changing a function signature: `find_callers("fn_name")`
- Before adding a trait method: `find_callers("TraitName")`
- Before non-trivial refactor: `blast_radius("symbol", max_depth: 2)`

## Working style

How the maintainer expects agents to work here. Norms, not code rules —
but load-bearing for a good session. (The maintainer works with agents
across several machines; these keep them consistent.)

- **Prose, not formatting flash.** Default to plain, considered prose in
  replies and authored docs. Use a heading, bullet, or table only when
  the content genuinely is one — never as the default shape. Don't sell
  ("powerful", "seamless", "full stop"); say what a thing does. Full
  guide: `docs/internal/VOICE.md`.
- **Don't `git commit` without an explicit ask.** "Ship it" / "land
  this" / "commit X" mean *prepare* the change: finalize code, run
  checks, hand back the commit message as plain text to copy-paste.
  `git add` is fine; running `git commit` is the maintainer's call.
  Branch first if on `main`.
- **Debug builds for dev, not release.** `cargo build` → `target/debug/`
  for all behavioral work including CI benches (the llama.cpp kernels are
  native C++ either way). Release is ~5× slower to compile — reserve it
  for a named perf need (e.g. OCR). Run e2e via `target/debug/<sibling>`
  directly; the `sovereign` symlink may point at release.
- **Rebuild the WHOLE workspace, not one binary.** After editing a shared
  crate (esp. `sovereign-core`), run a plain full `cargo build --workspace
  --features corpus-engine/treesitter` so every binary is fresh. A scoped
  `-p sovereign-cli-daemon` leaves `target/debug/sovereign-desktop` stale —
  and the chat e2e repro (`repro-defects.mjs` / `chaos.mjs`) exercises the
  DESKTOP binary, which runs the KnowledgeQuery / grounding pipeline
  in-process (the daemon it attaches to only serves inference + fan-out).
  Rebuild just the daemon and you validate old code. Verify what actually
  runs via `readlink -f /proc/<pid>/exe` + mtime, never `strings` on a big
  debug binary (it silently misses many `&str`s). The chat pipeline logs to
  the desktop app log (`test-artifacts/repro-defects-app.log`), not
  `daemon.err`.
- **Observability before hypothesis.** When a deployed-path behavior is
  wrong and one signal can't explain it, make the real decision *visible*
  first (tracing at a captured target + `RUST_LOG`, or a trace file) and
  confirm the trace lands — a detached daemon discards `eprintln`/`dbg!`.
  Only then form a fix. No whack-a-mole.
- **Quality over the metric.** Benches approximate epistemically-grounded
  inference for end users; they are not the goal. Don't tune a
  gate/prompt/threshold to flip one bank number at the expense of the
  unmeasured whole (tone, false caveats, suppressed-correct answers).
  Prefer structural, glassbox mechanisms; surface trade-offs rather than
  silently optimizing a number.
- **Fluent CLI is a feature.** A known workflow ("kick off the SEP
  ingest") should be ~3 shell lines (daemon start · pipeline run ·
  status). If it isn't, the friction is a bug in the CLI/recipe/config —
  fix the bug, don't wrap ceremony around it.
- **No trailing `/schedule` offers.** Don't close turns proposing to
  schedule background follow-ups; the maintainer reads it as pestering.
  An ordinary "next step?" for the task at hand is fine.