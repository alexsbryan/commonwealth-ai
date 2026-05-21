# `sovereign-agent-bench` — session handoff

Continuation doc for the agent-coding battery. Pairs with
`/Users/alexsbryan/dev/commonwealth-ai/HANDOFF.md` (the predecessor
OICP-runner diary) and the plan at
`~/.claude/plans/i-want-to-pickup-sorted-eagle.md`.

---

## 2026-05-21 evening — H4 write-thrash + alias-mode unlock

**Decisive intervention.** N=5 on 2.2-group-anagrams under 35B primary
moved from baseline `8, 1, 7, 1, 2` (mean 3.8, median 2) to
`8, 6, 9, 0, 8` (mean 6.2, median 8) after F1+F2 + alias mode.

Three load-bearing changes layered:

1. **F1 — stage-discipline prompt** (`problems/2.2-group-anagrams/prompt.md`).
   PLAN → WRITE → VERIFY → FIX-ONE structure. Self-monitor write
   counter ("if N > 3 you are thrashing; summarize prior attempts
   before continuing"). Each stage exactly one concern per turn.
2. **F2 — runner-side write-thrash detector** (`runners/pi.rs`).
   `KillReason::WriteThrash` SIGTERMs at 5 consecutive writes
   without an interleaving bash. New `ExitReason::WriteThrash` +
   `FailureClass::WriteThrash` for scanner classification.
3. **Alias-mode daemon config** (`~/.sovereign/config.toml`). Removed
   `fast = ...` key. `setup_config::ModelsSection::fast_path()`
   subsumes to primary when fast is unset; `embedded.rs:5020`
   `primary_is_alias` branch constructs primary as alias of fast's
   `Arc<LlamaModel>` (one weights copy, separate KV contexts).
   Baseline daemon RSS dropped 32GB → 5.8GB, peak during single
   primary inference 46GB → 6.1GB. Jetsam SIGTERM at 44GB on 64GB
   Mac is eliminated. The 9B fast slot is no longer pinned, so
   the daemon never has both 9B and 35B resident at once.

**Residual gaps observed in the F1+F2 retest:**

- **Trial 4 zero-writes outlier** (0/9). Model ran 7 bashes + 1 read,
  never reached WRITE stage. Possibly stage instructions are too
  dense and the model spent too long in PLAN. Need to inspect the
  final assistant text to confirm.
- **`done`-loop on completion.** Every successful trial ended with
  the model emitting `done` 4-6 times consecutively, eventually
  triggering `no_progress` SIGTERM. Pi-agent-core doesn't recognise
  `done` as termination — see `invariant_pi_done_heuristic`. The
  witness still scores correctly because workdir is fixed by then,
  but exit_reason taints to `no_progress`. Cleanup: pi runner could
  intercept `done` tool name and SIGTERM with a new
  `ExitReason::ModelDone` so the scanner doesn't blame `no_progress`
  for a successful run.

**Verified diagnostics (memory):**

- `invariant_daemon_eager_fast_slot_2026_05_21.md` — RSS trajectory
  table + alias-mode fix recipe.
- `project_h4_write_thrash_2026_05_21.md` — mechanism, per-trial
  evidence, F1+F2 design.

**Bench reproduction recipe (working today):**

```
# One-time: remove `fast = ...` from ~/.sovereign/config.toml
SOVEREIGN_DISABLE_AUTO_RESUME=1 SOVEREIGN_ALTERNATION_GRAMMAR=1 \
  sovereign daemon restart

cargo run -p sovereign-agent-bench --release --quiet -- run \
  --problems 2.2 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/r.json \
  --artifacts-dir /tmp/r
```

**Scaffolding-vs-measurement tension (raised 2026-05-21 evening).**
Each fix layered on Tier 1 (PLAN/WRITE/VERIFY stages, write-thrash
detector, smoke tests in workdir, prompt.md as a file, worked-example
claims, clean stubs) increasingly carries the model. At some point we
must measure the agent's ability to PRODUCE this scaffolding rather
than just consume it. That's Tier 2 (FromScratch in `problem.rs`):
empty workdir, agent must author `Cargo.toml`, `src/lib.rs`,
function signature, and (optionally) its own tests before reaching
the algorithm. Tier-2 infrastructure is already in `problem.toml` —
we just haven't authored from-scratch variants. The proposed
empirical move: one CLI flag `--tier from-scratch` skips
`install_scaffold` and `prompt.md` copy, runs the same fixture suite
against whatever the agent produced. Direct A/B → measures the
scaffold's contribution to success rate.

**`done`-loop fix landed 2026-05-21 evening** (`runners/pi.rs`).
First `done` tool call → `KillReason::ModelDone` → SIGTERM →
`ExitReason::Completed`. Closes the trailing-done-loop tax that
previously tainted every successful trial as `no_progress`.

**Minimum-confusion fixes landed 2026-05-21 evening (Tier-1):**

- `prompt.md` copied into workdir by `run.rs` after
  `install_scaffold`. Model that takes "See prompt.md" literally
  (trial 4 of the F1+F2 retest, 7-turn search for a phantom file)
  finds the spec where it expects it.
- Misleading `// X. See prompt.md.` comment removed from all 5
  scaffold stubs. Clean function-signature + `todo!()` only.
- `scaffold/tests/integration.rs` for 2.2 carries 3 smoke tests
  (empty, single, classic). Held-out 12-fixture suite still
  overrides at grading time per `auto_test.rs:135`. Model can now
  iterate against a real `cargo test --quiet --test integration`
  command instead of "verify by reading the spec carefully."

**Daemon RSS anomaly observed 2026-05-21 evening.** Even in alias
mode (config-level fix landed), daemon RSS climbed to 45GB and
jetsam-SIGTERMed during a multi-trial run. Each individual trial
should peak at ~6GB. Either KV cache accumulates across requests
without bounds, or primary slot unload isn't fully releasing
memory. Worth a session of trace-the-allocations work.

**Next-iter (ordered, smallest-first):**

1. **Multi-trial averaging** in the bench runner: today single-shot
   variance dominates measurement. Add `--trials N` flag that runs
   the same problem N times and reports mean ± stdev. Closes the
   "is this 6/9 stable or lucky?" gap.
2. **Tier 2 (FromScratch) variants** of 1.1, 1.2, 2.1: empty
   workdir, same fixture suite. Add `--tier from-scratch` flag
   that skips scaffold + prompt.md copy. Measures agent's
   scaffolding capability separately from algorithmic capability.
3. **Daemon RSS leak investigation**. Reproduce: restart daemon
   clean, run N=5 primary inferences with 4000-token budgets, log
   RSS after each. Linear growth → leak. Stable → workload-driven.
   Mitigations downstream of that signal.
4. **Propagate Tier-1 minimum-confusion stack to 2.1, 1.3, 1.2, 1.1**:
   smoke tests, clean stubs, stage-discipline prompts. Hold prompt.md
   copy as the only no-effort layer per-problem (already in run.rs).
5. **F-OBS** (memory pinned): new `/internal/runtime/slots` endpoint
   exposing real embedded daemon inventory. The existing `/status.
   loaded_models` is still a hardcoded lie.

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
