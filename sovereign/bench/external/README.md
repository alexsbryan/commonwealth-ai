# `bench/external/` — the industry rulers

Every other bank in `sovereign/bench/` and `gym/` was built here, scored
here, and validated against this repo's own history. That makes them
uncontaminated and it makes them unaudited: nothing in them can tell you
whether a number means what a number means anywhere else.

These two lanes exist to answer that, and only that.

| Lane | Role it rules on | External comparison |
|---|---|---|
| `rewardbench2/` | the comaintainer seat — judgment | AI2's RewardBench 2 leaderboard |
| `swebench/` | the worker pool — agentic coding, and the harness around it | SWE-bench Verified leaderboard |

Neither is a gate. Both are rulers.

## Before any number is read: the calibration anchor

Run **`Qwen3-8B-Q4_K_M` first, alone, on both lanes.** It is the only
model in the zoo whose base has published scores on both, so the gap
between what we measure and what the leaderboard reports is our
quantization-plus-harness offset. Every other row is uninterpretable
until that offset exists — a low score could be the model or could be
our runner dropping tool calls, and without the anchor there is no way
to tell. This is §18.4: validate the instrument before the result.

**It is not currently served.** `~/.sovereign/config.toml` lists
`Qwen3.8-27B` as primary and the 4B as fast; adding Qwen3-8B as an extra
needs a daemon restart. Until then, runs on other models give a score on
a shared scale but NOT the offset, and the rank-correlation test below
cannot run at all: as of 2026-08-18 `gym/comaintainer` has scored
Darwin-36B, Qwen3.6-35B-A3B and claude-default, RewardBench 2 has scored
Qwen3.8-27B, and the intersection is empty.

## RewardBench 2

```bash
cd rewardbench2
./run.py --model <id> --limit 300     # the anchor cut
./run.py --model <id>                 # full bank (1,865 items)
```

**Cost, measured not guessed.** Qwen3.8-27B at concurrency 1 runs a
**median 103 s/item** — it reasons for hundreds of tokens before
answering. The 300-item cut is ~8.6h; the full bank would be ~53h. Two
earlier figures in this file were wrong and are corrected here: 1.5-2h
assumed a non-reasoning judge, and 54.5 s/item was measured while
`max_tokens` was still clipping replies mid-scratchpad. Prefer `--limit`
with the stratified sampler over the full bank.

**Resume.** `--resume` keeps answered rows in `<out>/rows.jsonl` and
re-asks only the rest (errored rows are always re-asked). A run this long
must survive being paused for a competing measurement.

**Concurrency is 1 by default and that is deliberate.** The daemon sheds
with `local_queue_full` at queue position 1. At concurrency 3 the excess
workers do nothing but 503 and back off — measured 2026-08-18: 15 minutes
bought 50 items, and because errors were then being counted as wrong the
reported accuracy was 28% against a 25% chance floor. At concurrency 1 the
same model, same bank scored 50% with coverage 1.0. Raise concurrency only
after confirming a deeper slot queue, and never read a score whose
`coverage` is below 1.0.

Writes `runs/<model>/rows.jsonl` + `summary.json`. Reports macro accuracy
excluding `Ties` (whose official metric differs), micro accuracy, and a
`malformed_rate` column — a reply that does not parse is counted WRONG
and surfaced, never retried into a pass. An endpoint error rate above 2%
exits 4: that is a could-not-judge, not a score.

**What it decides.** Rank-correlate the per-model scores against
`gym/comaintainer`'s holdout ranking over 5+ models. Spearman ≥0.9 means
the gym is a slower way to learn what a public bench already reports,
and should be refocused onto its citation columns — which RewardBench 2
does not measure, and where the local-to-frontier gap is 40 points
rather than 5. Below 0.7 means the gym is measuring something else, and
that something then has to be shown to predict real verdict quality.

## SWE-bench Verified

Five arms, one *read* seam: every arm is read the same way — `git add -A
&& git diff --cached` — and scoring is the official harness. Nothing here
decides resolved/unresolved.

**The arms do NOT yet share an environment, and that is the lane's
biggest open confound.** Measured 2026-08-18: `mini-swe-agent`'s swebench
mode runs the agent INSIDE the per-instance SWE-bench container, with
dependencies installed and `pytest` runnable. `native`, `bare-metal`,
`flat` and `comaintainer` run on the host against a bare `git clone` with
nothing installed — which is why `--verify-cmd` defaults to a syntax
check rather than the test suite. That makes our four arms a strictly
HARDER variant than every published SWE-bench number, and it makes
`native - mini-swe-agent` unreadable until both sides share a setting.
Closing it means running the host arms inside the instance image too;
until then, treat cross-arm deltas as provisional and never quote an
absolute against the public leaderboard.

| Arm | What it is | Driver |
|---|---|---|
| `bare-metal` | one completion, no orchestration | `agent-bench swebench --agent bare-metal` |
| `native` | our canonical tool primitives | `agent-bench swebench --agent native` |
| `mini-swe-agent` | the published bash-only control | `arms/mini_swe.sh` |
| `flat` | full agent harness, plain prompt | `arms/agentic.py --arm flat` |
| `comaintainer` | order schema + subagent delegation | `arms/agentic.py --arm comaintainer` |

The prompts live in `prompts/` and are read by BOTH the Rust and Python
arms. A template forked between two languages would be a silent confound
in every delta this bench reports, so there is one copy (§10.6).

```bash
cd swebench
./prepare.py --n 100 --clone                    # sample + bare repo cache (slow, once)

./arms/mini_swe.sh --model Qwen3.8-27B-UD-Q6_K_XL          # control (needs instance images)
cargo run -p sovereign-agent-bench -- swebench --agent native --model Qwen3.8-27B-UD-Q6_K_XL
./arms/agentic.py --arm flat         --engine claude --model sonnet
./arms/agentic.py --arm comaintainer --engine claude --model sonnet

./collect.py --arm native                       # -> predictions/native.jsonl
./evaluate.sh native --workers 4                # official harness, Docker
```

### Reading the deltas

- `native − mini-swe-agent` — our tool contract, against an external
  baseline on an external task.
- `mini-swe-agent − bare-metal` — what generic scaffolding buys.
- `comaintainer − flat` — the seat protocol, engine and tools held
  fixed. Vary the engine (`--engine pi --model <local>`) to hold it
  fixed against the local stack rather than a frontier one.

Only one thing may differ between two arms being compared. That is why
the constraints block is shared verbatim and why `flat` exists at all.

### Power, stated before the run

At a ~20% resolve rate the 95% Wilson interval on n=100 is roughly ±8
points, so a 100-instance cut can only detect a harness effect of ~15
points or more. **A null result at n=100 is a could-not-judge, not
evidence the harness is worthless.** Detecting a 10-point effect needs
n≈250-300. Run the 100-cut as a screen; fund the full 500 only if the
screen comes back null and the question still matters.

### Container engine, and arm64

`evaluate.sh` prefers **podman** (operator preference, 2026-08-18). The
SWE-bench harness talks the Docker API through docker-py, so podman is
used via its API socket: the machine is started if stopped and
`DOCKER_HOST` is pointed at it. Verified on this host — docker-py
connects and reports `linux/arm64/fedora-44`, API 1.44. `--engine docker`
forces the other path.

The prebuilt `swebench/*` images are x86_64. `evaluate.sh` detects Apple
silicon and builds locally, which is slow and still leaves a minority of
instances whose environments do not build. Prefer running the grading leg
on the x86 Fedora peer. Patch *generation* is unaffected and can run
anywhere — that is the point of the predictions.jsonl seam.

### Daemon prerequisites (learned the hard way)

**Context size.** `~/.sovereign/config.toml` sets `context_size` for the
primary slot. It was `16000` when this lane was built, against a primary
whose GGUF reports `context_length = 262144` — a real SWE-bench prompt
did not fit and the run crashed before the planner's first call. Check
this value before blaming an arm.

**Workdir scale.** The native runner is built for an agent-coding
scaffold: 1-3 files whose whole contents are rendered into the prompt, so
the Planner and Implementer hold no read primitive and the Implementer is
forced to write first. Every one of those assumptions is wrong for a
repository, and each broke in turn (2026-08-18: a destructive write, then
NoProgress on an empty patch, then StickyRetry on inspect_workdir).
`cli/swebench.rs` therefore declares `.with_workdir_scale(Repository)`,
which is the ONE place that answer lives — preamble size, listing depth,
the `InspectWorkdir` grant, and the forced-first-tool all derive from it.
Measured on a pylint
checkout, the defaults cost 27,630 tokens to deliver what the agent's own
`ls`/`grep` fetch for ~174. See note `76003d82`; the bound is pinned by
`workdir_listing_is_bounded_on_a_repo_scale_tree`.

**Verification.** A bare checkout has no installed environment, so the
repo's pytest fails identically forever and the agent spends its whole
budget losing that loop. The default `--verify-cmd` is a syntax check on
changed files. Full-suite verification belongs to the grader's
per-instance image, not to generation.

### Contamination, acknowledged

SWE-bench Verified is the most contaminated bench we run: the fixing PRs
and their diffs are in every model's pretraining data. It is here for
placement on a shared scale, and because contamination is roughly
constant across arms on identical instances, so it largely cancels in
the deltas above. Do not quote the absolute number as a capability
claim, and do not use it to rank the zoo against itself — `rewardbench2`
and the in-house banks are cleaner for that.
