# `sovereign-agent-bench` — session handoff

Continuation doc for the agent-coding battery. Pairs with
`/Users/alexsbryan/dev/commonwealth-ai/HANDOFF.md` (the predecessor
OICP-runner diary) and the plan at
`~/.claude/plans/i-want-to-pickup-sorted-eagle.md`.

---

## 2026-05-20 → 2026-05-21 — what landed

The crate `sovereign/crates/sovereign-agent-bench/` ships as the
measurement surface for end-to-end coding agents (pi, future
opencode/codex/aider). MVS problem **3.2 Light's Out** runs
end-to-end through the full pipeline — agent → witness → judge →
report → baseline persistence.

The session was iteration-heavy: nineteen smoke runs (`h` → `s`),
each turning up one or two structural bugs in the *system around
the model* (daemon, pi config, harness plumbing). The bench did
its job — it surfaced bugs the OICP one-shot demo couldn't have
revealed.

### Smoke result at hand-off

Last run (`s`, scaffolded tier, pi=`commonwealth/coder`,
judge=`commonwealth/primary`):

```
3.2-lights-out   0/1/0 = 1/9   exit=completed  tokens(out)=820  wall=38746ms
witness: 12 tests ran, 0/12 passed (agent left todo!() in place)
judge: dim_b=1 (prose-only GF(2) recognition; no implementation)
```

Pipeline is structurally clean. The remaining gap is **agent
behaviour** — the model writes correct GF(2) reasoning into chat
instead of calling `edit`/`write` on `src/lib.rs`. That's an
agent-side problem the bench now correctly measures and exposes.

### Nine system bugs fixed this session

In order of landing:

1. **Pi `maxTokens` default too low.** Pi truncates at 60 output
   tokens unless `maxTokens` is set explicitly in
   `~/.pi/agent/models.json`. Setup script writes 16384 per slot.
2. **No artifact persistence.** `ArtifactSink` now drops a
   per-problem dir under `<bench-root>/.artifacts/<date>-<agent>-<model>/<id>/`
   carrying `agent.json`, `agent.jsonl` (raw stdout), `agent.stderr.txt`,
   `workdir/`, `workdir-post-witness/`, `judge/<dim>-trial-<n>.json`,
   `witness.json`. Forensic surface for "what actually happened."
3. **Daemon SIGTERM silent.** `wait_for_shutdown()` in
   `sovereign-cli/src/daemon_cmd.rs` now logs `pid`, `ppid`,
   `rss_mb`, `at_unix`. When SIGTERM arrives with RSS ≥ 24 GiB
   the log is `warn!` with a jetsam hint pointing at Console.app.
   Surfaced the 52 GB jetsam SIGTERM in run `o`.
4. **`SOVEREIGN_DISABLE_AUTO_RESUME` knob.** Added in
   `sovereign-mesh/src/auto_resume.rs`. When set, the daemon
   skips resume of in-progress corpus ingests at startup, freeing
   ~7 GB of fast-slot pressure during bench runs.
5. **Workdir-state prompt prefix.** Pi runner now prepends a
   factual `## Workdir state` block describing the workdir's
   contents (or `(empty)` with a hint) so the agent doesn't waste
   reads inspecting an empty directory.
6. **No-progress detector + `ExitReason::NoProgress`.** The
   PiRunner hashes the workdir on every tool-bearing turn. Eight
   consecutive tool calls without a workdir hash change → SIGTERM
   with a distinct exit reason. Cut a 15-minute infinite-`read`
   loop down to 64 s.
7. **Pre-scaffold tier.** New `Tier::Scaffolded` ProblemMeta
   variant. When set, the harness copies `problems/<id>/scaffold/`
   into the workdir before the agent runs — Cargo.toml + a
   `src/lib.rs` stub with `todo!()`. Bench measures algorithm-only
   for Level 1; Level 2 (`FromScratch`) tests project-scaffolding
   fluency separately.
8. **Slot unload between agent and judge.** Combined
   `extras_idle_secs = 30` in `~/.sovereign/config.toml` with a
   35-second pre-judge sleep in the harness, but only when
   `canonical_slot(agent_model) != canonical_slot(judge_model)`.
   Lets the fast/coder slot unload before the 29 GB primary slot
   loads, keeping peak RSS under jetsam threshold.
9. **Daemon parser orphan-bracket repair.** The Qwen3.5-9B-HighIQ
   mid-string-drift failure (run `r`) — model emits `…","path":"…"}]}`
   with an orphan `]`. New `strip_orphan_close_brackets` pre-pass
   in `sovereign-inference/src/embedded.rs` walks the body and
   drops orphan close brackets at depth 0 (string contents are
   untouched). Five new parser tests pin the behaviour.

All nine are landed in `sovereign-cli` release-build and live in
the daemon currently running.

### Three system bugs deferred

These are real product bugs surfaced by the bench. The bench works
without fixing them — the resulting score correctly reflects
"system isn't reliably bridging the model to tool actions."

**A. Grammar mask alternation grammar.** Per HANDOFF.md
§2026-05-08-later, `LlamaSampler::llguidance` installs a
JSON-Schema-derived grammar when `tool_choice = "required"` (or
`SOVEREIGN_FORCE_TOOL_CALLS=1`). The grammar is `oneOf` over the
function-call envelope shape, so a model under that grammar **must**
emit a tool call every turn. When the workdir is empty and the
model wants to say "I can't read anything, let me write first," it
can't — the only legal continuation is another tool call. Result:
infinite read loops (caught by the no-progress detector in run `n`).

The structural fix is a Lark-style alternation grammar: `oneOf
{tool_envelope, plain_text_message}`. Then the model can break out
of a useless tool loop by emitting normal text. The constraint
machinery lives in `sovereign-inference/src/json_constraint.rs`.

**B. Pi's max-iterations / done heuristic.** Pi self-terminates
after 2–3 model turns even when the work isn't finished (runs `o`,
`s`). The agent might emit a single `read` then declare done.
Either:
- pi has an internal max-iterations we haven't tuned, or
- pi treats "model didn't return a tool call this turn" as
  agent-end.

Both observable from `agent.jsonl` (`type:"agent_end"` is the last
event). Worth grepping the pi source for `maxIterations` or
similar. Could be addressable via a pi CLI flag we missed, or via
the daemon nudging the model toward continuation.

**C. Authoring tier 2 (FromScratch) of 3.2.** The scaffolded tier
isolates algorithm from scaffolding. The from-scratch tier is the
other half of the signal: can the agent produce a working
Cargo.toml + project layout + impl? Same problem statement,
different witness expectations (no `scaffold_subdir`, prompt
re-includes the "create Cargo.toml + src/lib.rs" instructions).
Until this lands, the bench measures only Level 1.

---

## Run the bench now

```bash
# One-time setup (idempotent)
bash scripts/setup-pi-provider.sh

# Daemon (note the env var)
sovereign daemon stop
SOVEREIGN_DISABLE_AUTO_RESUME=1 sovereign daemon start

# Single-problem smoke (~90 s wall)
cargo run -p sovereign-agent-bench --quiet -- run \
  --problems 3.2 \
  --model commonwealth/coder \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/agent-bench.json \
  --artifacts-dir /tmp/agent-bench-artifacts

# Inspect artifacts
ls /tmp/agent-bench-artifacts/3.2-lights-out/
cat /tmp/agent-bench-artifacts/3.2-lights-out/agent.json | python3 -m json.tool
cat /tmp/agent-bench.json | python3 -m json.tool
```

Expected with the current setup: pi makes a small number of tool
calls, mostly reads, doesn't modify `src/lib.rs`. Score ~0–2/9
depending on judge variance.

If `commonwealth/coder` is missing on `/v1/models`, the slot wiring
in `~/.sovereign/config.toml` got reset — restore the `code = "…"`
line pointing at `Qwopus3.5-9B-Coder-MTP-Q6_K.gguf`.

---

## Suggested next moves

**Small (≤ 30 min each):**

- Author tier-2 problem variant: `problems/3.2-lights-out-from-scratch/`
  with the original prompt (no scaffold), `tier = "FromScratch"`,
  same fixtures. One smoke run = comparable Level 1 vs Level 2
  signal for the same problem.
- Add tier filtering to the CLI: `sovereign agent-bench run --tier Scaffolded`.
- Surface tier in the text-rollup output of `BenchReport::text_rollup`.

**Medium (1–3 hours):**

- Pi max-iterations investigation. Read pi source at
  `~/.nvm/versions/node/v20.20.2/lib/node_modules/@earendil-works/pi-coding-agent/`.
  Find the agent-end heuristic; expose it via pi config if it
  isn't already; tune for bench runs.
- Stronger prompt nudging: instead of just "use the edit tool,"
  give a concrete first-tool-call template the model can pattern
  on. The Qwopus coder model historically follows examples well.
- Author 1 more problem (1.1 Regex Shortest Path, Rust,
  Scaffolded). With two problems live, regression detection on
  `latest.json` starts being meaningful.

**Bigger (a day or more):**

- **Grammar alternation** (deferred bug A). `sovereign-inference`
  work — extend `JsonConstraint` (or move to a Lark grammar via
  llguidance) so the model can emit either a tool envelope or
  plain text per turn. Closes the force-tool-calls loop trap
  structurally instead of via the no-progress hack.
- **Tool-result feedback in the prompt.** When pi gets back
  `read("Cargo.toml") = "no such file"`, the model treats the
  conversation history as advisory and keeps reading. The
  daemon could prepend "(prior reads on this turn returned empty
  — consider writing instead)" but that's mid-stream nagging
  and feels wrong. Better: instrument what the model sees and
  redesign the prompt for clarity.

---

## Critical file map

### Crate
- `sovereign/crates/sovereign-agent-bench/src/runner.rs` — trait, contexts, `ExitReason` (incl. `NoProgress`)
- `sovereign/crates/sovereign-agent-bench/src/runners/pi.rs` — subprocess + JSONL parser + no-progress + budget kill
- `sovereign/crates/sovereign-agent-bench/src/problem.rs` — TOML schema, closed enums (incl. `Tier`)
- `sovereign/crates/sovereign-agent-bench/src/sandbox.rs` — workdir + scaffold install + env scrub
- `sovereign/crates/sovereign-agent-bench/src/cli/run.rs` — orchestration, slot-swap sleep, resilient judge
- `sovereign/crates/sovereign-agent-bench/src/artifacts.rs` — agent.json + jsonl + judge persistence
- `sovereign/crates/sovereign-agent-bench/src/judge.rs` — HTTP judge, workspace-view assembly
- `sovereign/crates/sovereign-agent-bench/src/judge_multi.rs` — N-trial majority-vote aggregator
- `sovereign/crates/sovereign-agent-bench/tests/mvs_pipeline.rs` — synthetic problem + MockAgentRunner + StubJudge

### Data
- `sovereign/bench/agent-coding/problems/3.2-lights-out/problem.toml` — `tier = "Scaffolded"`
- `sovereign/bench/agent-coding/problems/3.2-lights-out/prompt.md` — scaffolded-tier version
- `sovereign/bench/agent-coding/problems/3.2-lights-out/scaffold/Cargo.toml`
- `sovereign/bench/agent-coding/problems/3.2-lights-out/scaffold/src/lib.rs` — `todo!()` stub
- `sovereign/bench/agent-coding/problems/3.2-lights-out/fixtures/tests/integration.rs` — 13 held-out tests

### Daemon (changes touching `sovereign-cli` + `sovereign-inference` + `sovereign-mesh`)
- `sovereign/crates/sovereign-cli/src/daemon_cmd.rs:2826-2980` — `wait_for_shutdown` glassbox + RSS hint
- `sovereign/crates/sovereign-mesh/src/auto_resume.rs:99-115` — `SOVEREIGN_DISABLE_AUTO_RESUME` knob
- `sovereign/crates/sovereign-inference/src/embedded.rs:8357-8505` — parser w/ orphan-bracket repair + 5 tests

### Operator config
- `~/.sovereign/config.toml` — `code = .../Qwopus3.5-9B-Coder-MTP-Q6_K.gguf`, `extras_idle_secs = 30`, `primary_idle_secs = 60`
- `~/.pi/agent/models.json` — `commonwealth` provider with `maxTokens: 16384` per model
- `scripts/setup-pi-provider.sh` — idempotent provider-config writer

### Plan + memory
- `~/.claude/plans/i-want-to-pickup-sorted-eagle.md` — original plan
- HANDOFF.md (top-level) — OICP predecessor diary

---

## How to read the artifacts directory

Per-run structure (under `<artifacts-dir>/<problem-id>/`):

| File | What's in it | When to read |
|---|---|---|
| `agent.json` | Tokens, wall, exit_reason, parsed tool_calls (with args), `final_assistant_text`, `raw_line_count` | First — high-level summary |
| `agent.jsonl` | Every line pi emitted on stdout, raw | When tool_calls is suspiciously low / args look empty |
| `agent.stderr.txt` | Pi's full stderr (no cap) | When exit_reason is `Crashed` |
| `workdir/` | What pi wrote, before fixtures landed | What the agent built |
| `workdir-post-witness/` | After fixtures copied + cargo ran | What the witness saw |
| `witness.json` | Verify exit, pass/fail counts, failed-test names, pass_fraction, bucketed score | When dim_a looks off |
| `judge/<dim>-trial-<n>.json` | Full judge prompt + parsed outcome (or error) | When dim_b/dim_c look off |

`raw_line_count` ≠ `tool_calls` length is the smoke signal: pi
emitted data the parser missed.

---

## Iteration log (compact)

| Run | Config delta | Tokens out | Tool calls | Workdir end | Score | Exit |
|---|---|---|---|---|---|---|
| h | first end-to-end | 166 | 0 | empty | 0/9 | completed |
| k | +pi maxTokens 16384 | 1197 | 0 | empty | 3/9 | completed (chat-only GF(2)) |
| l | +tool-explicit prompt | 114 | 0 | empty | 0/9 | completed |
| m | agent=fast (Qwen3.5-9B) | 923 | 18 (empty args) | empty | 0/9 | completed |
| n | +force_tool_calls=1 | 3418 | 48 reads | empty | 0/9 | **timeout 15min** |
| o | +no_progress detector | 212 | 3 reads | empty | 0/9 | completed |
| p | judge=fast | 424 | 8 reads | empty | 2/9 | **no_progress (64s)** |
| q | +workdir prefix | 581 | 8 reads | empty | 0/9 | no_progress |
| r | force=0 + prefix | 418 | 2 (write, bash) | **Cargo.toml landed** | 0/9 | completed; third call dropped by parser → fixed in bug 9 |
| s | scaffold tier + slot unload + parser fix | 820 | 2 (read, read) | scaffold unchanged | 1/9 | completed; **12 tests RAN**, 0 passed (todo! in place) |

The transition from `n` → `o` → `p` is the no-progress detector
catching the loop trap. The transition from `r` → `s` is the
scaffold lift — workdir is now meaningful even on a 0-write agent
because the witness still has something to test against.
