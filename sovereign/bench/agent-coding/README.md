# Agent-coding battery — `sovereign-agent-bench`

Eight problems, three dimensions per problem (correctness / approach /
efficiency), `0..=3` per dimension, `72` total max.

Run:

```bash
# One-time: write the pi provider config pointing at the local daemon.
bash scripts/setup-pi-provider.sh

# Run one problem end-to-end.
sovereign agent-bench run --problems 3.2 --report /tmp/r.json

# Whole battery against a fresh baseline.
sovereign agent-bench run --update-baseline
```

## Layout

```
sovereign/bench/agent-coding/
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
| PR 5+ | opencode / codex / aider runners                             |

Plan: `~/.claude/plans/i-want-to-pickup-sorted-eagle.md`.
