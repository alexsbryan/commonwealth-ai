# Agent-coding battery — `sovereign-agent-bench`

Fifteen problems, three dimensions per problem (correctness / approach /
efficiency), `0..=3` per dimension, `9` per problem.

Run:

```bash
# Run one problem end-to-end against the local daemon.
sovereign agent-bench run --problems 3.2 --report /tmp/r.json

# Whole battery against a fresh baseline.
sovereign agent-bench run --update-baseline

# pi or opencode against any OpenAI-compatible server, judge off
# (dims scored only by the judge are recorded as not judged).
sovereign agent-bench run --agent opencode --model <id> \
  --agent-base-url http://127.0.0.1:18180/v1 --agent-context-window 65536 \
  --no-judge --problems 3.2-lights-out-python --report /tmp/r.json
```

pi and opencode get their provider config per run (pi through its own
`PI_CODING_AGENT_DIR`, opencode inline, with a throwaway `HOME`), so
`~/.pi` and `~/.config/opencode` are never read. `--agent-base-url`
defaults to `--judge-base-url`; runners with a built-in URL refuse it.

`arms/` holds the daemon-vs-llama-server A/B: `build-llama-server.sh`
(arm A, at the daemon's vendored llama.cpp commit), `serve-arm.sh`
(arm A, B or C on one port) and `tap.py` (the one recorder every arm's
traffic passes through).

## Layout

```
bench/lanes/agent-coding/
  README.md
  problems/<id>/
    problem.toml      meta + witness + budget + scoring
    prompt.md         task statement (handed to the agent verbatim)
    rubric.md         per-judged-dim anchor prose, 0..=3 each
    fixtures/         held-out test fixtures (copied AFTER the agent exits)
  baselines/agent-coding/
    <date>-<agent>-<model>.json
    latest.json -> <date>-…json
```

## What ships with the MVS (PR 1)

- `3.2-lights-out` (Rust) — GF(2) linear system / chase-the-lights.

## Roadmap

| PR    | Adds                                                         |
|-------|--------------------------------------------------------------|
| MVS   | crate scaffold + PiRunner + MockAgentRunner + 3.2 Light's Out|
| PR 2  | 1.1 Regex Shortest Path (Rust) + 2.1 Global Counter (Go) + `baseline compare` CLI |
| PR 3  | 1.2 Group Knapsack (Go), 1.3 Tree LIS (TS), 2.2 Mutual Friend (TS), 2.3 ZK BMI (Python), 3.1 Hex Conway (Rust) + `list / show` CLI |
| PR 4  | syn-based Rust source-content validator registered on daemon |
| PR 5+ | opencode runner (landed 2026-10-06); codex / aider          |

Plan: `~/.claude/plans/i-want-to-pickup-sorted-eagle.md`.
