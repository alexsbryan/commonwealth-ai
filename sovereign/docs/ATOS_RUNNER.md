# ATOS Runner — the ralph-wiggum loop

← [back to ATOS.md](ATOS.md) · [back to README](../README.md)

## What this is

ATOS already has the artifacts (`DESIGN.md`, `IMPLEMENTATION_PLAN.md`,
`CHARTER.md`, `notes.db`, `features.db`). The Runner is the thin
loop that sits on top of those artifacts and drives an agent to
completion without operator ceremony.

It is conceptually identical to Anthropic's
[ralph-wiggum](https://github.com/anthropics/claude-code/tree/main/plugins/ralph-wiggum)
plugin: keep feeding the agent until it claims the work is done.
The differences:

- The agent claims completion by **writing `DONE.md`** in the
  workdir (not by emitting an exact-string "promise"). DONE is
  durable, auditable, and amendable.
- A **reviewer pass** evaluates `DONE.md` against the project's
  `CHARTER.md` (and `DESIGN.md` / `IMPLEMENTATION_PLAN.md` when
  present). Acceptance closes the loop; rejection synthesizes a
  continuation prompt that names specific gaps and re-spawns
  opencode for another pass.

That's the whole contract. Provide a technical design; the
runner ships it; an audit log emerges as a side effect of the
existing daemon middleware.

## Why this is layered, not single-tier

The Runner ships *one* feature for *one* agent. It does not
score the work, compare agents, or grade tool usage. That belongs
to `sovereign-eval` — which already exists and consumes the same
artifacts the Runner produces (manifest, tool-event stream,
decision notes).

```
┌──────────────────────────────────────────────────────────────┐
│  Runner  (sovereign atos run)                                │
│  inputs:  DESIGN.md + CHARTER.md + IMPLEMENTATION_PLAN.md    │
│  driver:  opencode (or claude) subprocess loop               │
│  output:  shipped feature + iteration log + DONE.md history  │
└──────────────────────────────────────────────────────────────┘
                          │  artifacts
                          ▼
┌──────────────────────────────────────────────────────────────┐
│  sovereign-eval  (the benchmark rig — unchanged)             │
│  consumes:  manifest.json + atos_tool_events + DONE.md +     │
│             notes.db + git diff                              │
│  scores:    design quality · tool-call correctness vs oracle │
│             · mechanical golden suite · capable-agent judge  │
└──────────────────────────────────────────────────────────────┘
```

This split exists because they answer different questions:

- The Runner answers **"can this agent ship this feature on this
  charter?"** It produces a yes/no plus a working tree.
- `sovereign-eval` answers **"how well did it ship?"** It produces
  a per-axis score that lets us A/B candidate agents.

Conflating them was the trap in the original harness plan
(`ATOS_SELF_HOST_EXPERIMENT.md`): the loop tried to do both, so
when a run scored badly the operator couldn't tell whether the
agent, the spec, or the rubric was wrong.

## The loop

```
sovereign atos run \
  --workdir <path>                  # repo to work in (mandatory)
  [--design DESIGN.md]              # auto-discovered if present
  [--charter CHARTER.md]            # auto-discovered if present
  [--plan IMPLEMENTATION_PLAN.md]   # auto-discovered if present
  [--feature-id <id>]               # binds to FeatureStore row for audit
  [--driver opencode|claude]        # default opencode
  [--max-iters 20]                  # safety cap
  [--reviewer-model <id>]           # local Bench_Darwin model id
  [--done-marker DONE.md]
  [--dry-run]                       # prints the prompts without spawning
```

### Per iteration

1. **Compose the prompt.**
   - Iteration 1: `DESIGN.md` + `CHARTER.md` + `IMPLEMENTATION_PLAN.md`
     (all that exist) + a one-paragraph runner preamble explaining
     the DONE contract.
   - Iteration N>1: same artifacts + the previous reviewer's
     rejection memo + a tail of the iteration log so the agent
     sees what was already tried.
2. **Spawn opencode** in `--workdir` with `SOVEREIGN_FEATURE_ID`
   / `ATOS_RUN_ID` / `ATOS_DRIVER` exported. Stream stdout/stderr
   to the operator's terminal; `opencode run --input -` reads the
   prompt from stdin.
3. **On opencode exit**, look for `<workdir>/DONE.md`.
   - **Absent**: opencode ran out of turns or got stuck. The
     runner synthesizes a "no DONE found, continue from current
     state" prompt and loops.
   - **Present**: invoke the reviewer.
4. **Reviewer pass.** A capable model receives:
   - the charter (rubric)
   - the design (contract)
   - the plan (scope, if present)
   - `DONE.md` (the agent's claim)
   - `git diff <iter-start>..HEAD` in the workdir (what actually
     changed)

   The reviewer returns a strict JSON object:

   ```json
   {
     "verdict": "accept" | "reject",
     "summary": "<one paragraph>",
     "gaps": [
       { "area": "...", "what_missing": "...", "suggested_action": "..." }
     ]
   }
   ```

   - `accept` → exit the loop, dump manifest, mark feature
     completed.
   - `reject` → archive `DONE.md` to
     `~/.sovereign/runs/<run-id>/iter-<N>/DONE.rejected.md`, write
     a continuation memo enumerating the gaps, loop.
5. **Persist iteration record** to
   `~/.sovereign/runs/<run-id>/iterations.jsonl`:
   ```json
   {
     "iter": 3,
     "started_at": "2026-05-05T22:14:01Z",
     "ended_at":   "2026-05-05T23:02:18Z",
     "prompt_sha": "...",
     "opencode_exit": 0,
     "done_present": true,
     "verdict": "reject",
     "gap_count": 2,
     "wall_seconds": 2897
   }
   ```

The loop terminates when (a) reviewer accepts, (b) `--max-iters`
is hit, or (c) the operator interrupts (SIGINT writes a partial
manifest and exits non-zero).

## What the agent sees

The runner does not coach the agent on how to use Sovereign's
tools. That guidance lives in `.sovereign/SOVEREIGN.md`, which
opencode loads via the existing plugin. The runner only contributes
the *task* (design + plan) and the *contract* (the charter + the
DONE convention).

The DONE contract that ships in the iter-1 prompt:

> When you believe this feature meets `DESIGN.md` (and, if
> present, satisfies the phases in `IMPLEMENTATION_PLAN.md`),
> write a file named `DONE.md` at the repo root. Structure it
> with one section per design anchor or plan phase, naming the
> code that satisfies it (path:line). End with a "What I did NOT
> do" section listing anything you skipped or punted on. A
> reviewer will read your `DONE.md` against the charter; if you
> have skipped something the charter requires, the reviewer will
> reject and you'll see specific feedback in the next iteration.

## What survives from the existing harness plan

- **Everything in `sovereign-atos`** — charter parsing, feature
  provisioning, run lifecycle, milestone state, decision-extractor
  middleware. The runner *uses* this surface; it doesn't replace
  it.
- **The opencode plugin** at
  `sovereign/crates/sovereign-cli/assets/sovereign-atos.ts` —
  unchanged. Tool events keep flowing into `atos_tool_events`
  keyed by `ATOS_RUN_ID`.
- **`sovereign-eval`** — unchanged. The runner emits manifests,
  iteration logs, and DONE histories that eval already knows how
  to ingest. New axes (e.g. design-doc quality, DONE accuracy)
  are additive scorers in the same crate, not a redesign.
- **`scorer/golden/` + `scorer/oracle/`** in the experiment repo
  — unchanged. They are the benchmark fixtures eval grades
  against, not part of the runner's hot loop.

## What the original harness plan got wrong

`ATOS_SELF_HOST_EXPERIMENT.md` (and its successor draft, the
"jiggly-ullman" plan) treated tool-efficacy measurement as a
property of the loop. That meant any change to the rubric or the
oracle counted as a "tier-4 loop change," and an overnight that
scored badly produced a haystack of confounders.

This document supersedes those plans for the **shipping path**.
The benchmark path stays where it lives now, behind the
`sovereign-eval` boundary, and consumes the runner's outputs
without telling the runner what to optimize. Operators wanting
to A/B agents run the runner once per agent, then point
`sovereign-eval diff <run-a> <run-b>` at the resulting
`runs/<run-id>/` directories.

## Stop conditions, in order of strength

The runner accepts work when **any one** of these passes. They
are tried top-down on every iteration's DONE pass:

1. **Charter-defined hard gate** — if the charter declares a
   `stop_condition` shell command (e.g. `cargo test
   --workspace`), the runner runs it after DONE is written and
   *requires* exit-zero before the reviewer is even consulted.
   This is the cheapest, most legible signal; agents and
   operators agree on what passing means.
2. **Reviewer accept** — the LLM judge consents. Used when the
   charter has no shell gate, or as a second layer on top of
   the gate.
3. **Operator override** — `sovereign atos run --accept` exits
   the loop immediately and treats the current state as
   accepted. Recorded as `verdict: "operator_accept"` in the
   iteration log.

The reviewer's rubric is the charter — verbatim. Projects that
want strict review write strict charters; projects that want
fast iteration write loose charters. The runner has no opinion.

## Audit trail (free)

Every iteration produces:

- An `atos_runs` row (already wired in `sovereign-atos`).
- Tool events in `atos_tool_events` (already wired via the
  opencode plugin).
- Decision/invariant/attempt notes from the daemon's
  `decision_extractor` middleware (already wired).
- The iteration record in `iterations.jsonl`.
- Each rejected DONE archived under the run directory, so a
  reviewer can see the agent's *progression of claims* across
  iterations.
- A final `manifest.json` written by `sovereign-eval finalize-run`
  on accept.

Operators audit the run with:

```
sovereign-eval finalize-run <run-id> --experiment-repo <workdir>
sovereign audit <feature-id>          # rolls up notes + runs + reports
```

## Concrete dependencies and where to look

| Concern | File | Notes |
|---|---|---|
| Driver subprocess pattern | `sovereign-cli/src/atos_cmd/milestone.rs:639` | The runner reuses the `Driver::Opencode` spawn shape verbatim. |
| Feature/run lifecycle | `sovereign-atos/src/local/orchestrator.rs:236` | `begin_run` / `close_run` are reused; the runner does *not* invent new run rows. |
| Reviewer transport | `sovereign-eval/src/judge.rs` | Existing pattern (POST `/v1/chat/completions`, retry-once, parse JSON). The runner's reviewer is structurally identical. |
| Charter parse | `sovereign-atos/src/charter.rs` | Stop-condition extraction reused — see `extract_milestone_stop_condition` for the marker-in-brief technique. |
| Plugin event capture | `sovereign-cli/assets/sovereign-atos.ts` | Untouched. |
| Manifest writer | `sovereign-eval/src/manifest.rs` | Untouched. The runner just calls `sovereign-eval finalize-run` on accept. |

## What is not in scope

- **No new schema.** The runner uses existing `features`,
  `feature_milestones`, `atos_runs`, and `atos_tool_events`
  tables. Iteration log lives on disk under
  `~/.sovereign/runs/<run-id>/`.
- **No new MCP tools.** The runner is operator-facing; agents do
  not see it.
- **No background daemon hook.** The runner is a foreground
  process. Crash = orphaned run; `sovereign-eval finalize-run`
  closes the books on the next operator wake-up.
- **No automatic agent A/B.** Multi-agent comparison stays on
  the eval side: run twice, diff with `sovereign-eval diff`.

## Bring-up — minimum viable runner

The first land:

1. `sovereign atos run` subcommand that spawns opencode in a
   workdir, waits, looks for DONE.md, calls the reviewer.
2. Reviewer call against local Bench_Darwin (`/v1/chat/completions`).
3. Iteration log on disk; manifest dump on accept.
4. Smoke test on `~/dev/atos-experiment-oicp-types` using the
   already-authored `DESIGN.md` + `IMPLEMENTATION_PLAN.md`.

Anything past that — per-phase loops, alternate reviewers,
operator-override flag, automatic charter discovery — is a
follow-on once the loop's shape is honest.
