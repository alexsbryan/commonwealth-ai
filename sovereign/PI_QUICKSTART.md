# Getting started with the agent-bench (Pi + Native)

Five-minute path from a fresh checkout to a passing bench trial. Pairs with `HANDOFF.md` for the per-session work diary; this doc stays version-stable.

## What this bench measures

`sovereign-agent-bench` drives an end-to-end coding agent against a small library of held-out problems and reports per-dimension scores (correctness / approach / efficiency). It supports two agent runners:

- **`pi`** — drives the `pi-coding-agent` subprocess. Production-shaped: pi keeps its tools (`read`, `write`, `bash`), the bench observes and normalizes the telemetry into the canonical primitive set.
- **`native`** — drives the daemon's `/v1/chat/completions` directly through `commonwealth-agent-tools`. The model sees only canonical primitives (`write_file`, `build`, `smoke`, `agent_done`, `agent_plan`, `handoff_to_*`). Three-role split (Planner / Implementer / Evaluator) gives structural verify-discipline. Supports request `replay` for forensic debugging.

Same problem library, same scoring rubric, same artifacts shape. Pick the runner that matches what you want to measure.

## Prerequisites

1. **Daemon built + running**:
   ```bash
   cargo build -p sovereign-cli --release
   sovereign daemon start
   sovereign daemon status   # → "daemon running"
   ```

2. **Bench binary built**:
   ```bash
   cargo build -p sovereign-agent-bench --release
   ```

3. **Pi-coding-agent installed** (only required for `--agent pi`):
   ```bash
   npm install -g @earendil-works/pi-coding-agent
   bash scripts/setup-pi-provider.sh
   # Idempotent. Writes ~/.pi/agent/models.json with a `commonwealth`
   # provider pointing at http://localhost:9741/v1.
   ```

4. **Python tooling** (only required for Python-language problems like `3.2-lights-out-python`):
   ```bash
   python3 -m pip install --user pytest
   ```

## Daemon config that matters for the bench

`~/.sovereign/config.toml` controls the daemon. The settings the bench cares about:

```toml
[models]
primary = "/path/to/your/primary.gguf"
# Optional. Drop the `code` line on hosts where the 9B coder slot
# pushes total RSS past the macOS jetsam threshold (~36 GB on 64 GB).
# code = "/path/to/coder.gguf"
embed = "/path/to/qwen-embedding-0.6b.gguf"
context_size = 16000   # 50000 cost ~6 GB extra KV; 16K is plenty for bench turns

[daemon]
primary_idle_secs = 60
extras_idle_secs = 30
yield_to_foreground_secs = 60
force_tool_calls = false
# Engage llguidance schema-driven tool grammar — closes the
# empty-args / content-as-envelope failure class. Required for
# the native runner under tools-using requests; harmless for pi.
alternation_grammar = true
```

After editing config, restart:
```bash
sovereign daemon restart
```

## First trial — pi runner

The smallest possible smoke. Picks a single problem, runs it, drops artifacts to `/tmp/pi-smoke/`:

```bash
SOVEREIGN_DISABLE_AUTO_RESUME=1 \
SOVEREIGN_DISABLE_PEER_INFERENCE=1 \
sovereign daemon restart

./target/release/sovereign-agent-bench run \
  --agent pi \
  --problems 1.1 \
  --model commonwealth/coder \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --bench-root sovereign/bench/agent-coding \
  --report /tmp/pi-smoke.json \
  --artifacts-dir /tmp/pi-smoke
```

Expected wall-clock: ~30-90 s on a warm daemon. Look for:

```
1.1-reverse-string               3/3/3 = 9/9   exit=completed
```

If you get `0/0/0 = 0/9 exit=timeout` or `exit=crashed`, jump to **Troubleshooting** below.

## First trial — native runner

Same problem, native runner. Note `--model commonwealth/primary` (native uses the primary slot directly, not the coder slot):

```bash
./target/release/sovereign-agent-bench run \
  --agent native \
  --problems 1.1 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --bench-root sovereign/bench/agent-coding \
  --report /tmp/native-smoke.json \
  --artifacts-dir /tmp/native-smoke
```

## Listing problems

```bash
./target/release/sovereign-agent-bench list
```

Output:
```
1.1-reverse-string               CodeTest       Rust
1.2-two-sum                      CodeTest       Rust
1.3-binary-search-leftmost       CodeTest       Rust
2.1-balanced-parens              CodeTest       Rust
2.2-group-anagrams               CodeTest       Rust
3.2-lights-out                   CodeTest       Rust
3.2-lights-out-python            CodeTest       Python
```

To see one in detail:
```bash
./target/release/sovereign-agent-bench show 3.2-lights-out
```

## Tiers — Scaffolded vs FromScratch

Some problems support two tiers. **Scaffolded** (default) pre-installs a `Cargo.toml` (or `lights_out.py`) stub + a `tests/integration.rs` smoke test; the agent fills in the algorithm. **FromScratch** skips the install — the agent authors the whole project from the spec alone.

```bash
./target/release/sovereign-agent-bench run \
  --agent native \
  --tier from-scratch \
  --problems 2.1 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/r.json \
  --artifacts-dir /tmp/r
```

The two tiers measure different things. Scaffolded isolates the algorithmic dimension; FromScratch adds project-scaffolding fluency on top.

## Multi-trial runs (variance signal)

```bash
./target/release/sovereign-agent-bench run \
  --agent native \
  --problems 2.1 \
  --trials 9 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/r.json \
  --artifacts-dir /tmp/r
```

Output adds a mean ± stdev line:
```
2.1-balanced-parens   3/3/3 = 9/9
                      N=9 mean=8.44±0.50 totals=(9,8,9,9,8,9,8,9,9) exit_mix=completed×9
```

Multi-trial artifacts land under `/tmp/r/2.1-balanced-parens/trial-N/` instead of the flat layout.

## Reading the artifacts

Per-problem directory layout:

```
<artifacts-dir>/<problem_id>/
├── agent.json              high-level summary: exit, tokens, tool_calls, wall_ms
├── agent.jsonl             raw stdout lines from the agent (pi only)
├── agent.stderr.txt        full stderr (uncapped)
├── requests.jsonl          per-turn chat-completion request + response
│                           (native runner only — used by `replay`)
├── workdir/                deep copy of what the agent wrote
├── workdir-post-witness/   workdir after fixtures landed + tests ran
├── witness.json            test results: passed, failed, total, pass_fraction,
│                           stdout_tail, bucketed_score, failed_names
└── judge/
    ├── dim_b-trial-0.json  judge prompt + outcome (anchor + rationale)
    └── dim_c-trial-0.json  same for the second judged dimension
```

Common forensic moves:

```bash
# What did the agent emit?
jq '.tool_calls[] | {turn, tool, args_preview: (.args_preview|.[:120])}' \
  /tmp/r/2.1-balanced-parens/agent.json

# Did the tests run? How many passed?
jq '{passed, failed, total, pass_fraction}' \
  /tmp/r/2.1-balanced-parens/witness.json

# What did the judge see?
jq '.outcome' /tmp/r/2.1-balanced-parens/judge/dim_b-trial-0.json
```

## Replay — settling debates without rerunning the bench

The `replay` subcommand re-sends any captured chat-completion turn with overrides. Use it to isolate variables: "is this a model issue, a prompt issue, or a sampling issue?"

```bash
# List what's available — `requests.jsonl` has one record per turn.
wc -l /tmp/r/2.1-balanced-parens/requests.jsonl

# Replay turn 5 as-recorded, also showing the original response:
./target/release/sovereign-agent-bench replay \
  /tmp/r/2.1-balanced-parens \
  --turn 5 \
  --print-original

# Same turn at temperature 0 (deterministic):
./target/release/sovereign-agent-bench replay \
  /tmp/r/2.1-balanced-parens \
  --turn 5 \
  --temperature 0.0

# Same turn with NO chat history (only system + first user message):
./target/release/sovereign-agent-bench replay \
  /tmp/r/2.1-balanced-parens \
  --turn 5 \
  --strip-history

# Try a different model on the same prompt:
./target/release/sovereign-agent-bench replay \
  /tmp/r/2.1-balanced-parens \
  --turn 5 \
  --model commonwealth/coder

# Dump the final request body without sending — useful for hand-curling:
./target/release/sovereign-agent-bench replay \
  /tmp/r/2.1-balanced-parens \
  --turn 5 \
  --dump-request
```

Replay only works for the native runner (`requests.jsonl` is captured there). Pi's subprocess telemetry doesn't expose per-turn request bodies.

## Aggregating across multiple runs

```bash
./target/release/sovereign-agent-bench aggregate \
  --artifacts-root /tmp/sweep \
  --classify
```

Walks every per-problem dir under `--artifacts-root` and prints a failure-class histogram (`solved`, `partial`, `loop_trap`, `verify_stuck`, `cycle_limit`, `write_thrash`, `parse_failed_envelope`, etc.). Useful when sweeping a config matrix.

## Troubleshooting

### Daemon dies mid-run with SIGTERM (jetsam)

Symptoms: bench reports `exit=crashed`, `agent.stderr.txt` empty, daemon log has `daemon: shutdown signal received — peak RSS suggests possible jetsam/OOM trigger`.

Root cause: total RSS crossed the macOS jetsam threshold (~36 GB on 64 GB hosts). The 35B primary slot + KV cache is at the line.

Mitigations (apply progressively):

```toml
# ~/.sovereign/config.toml
[models]
primary = "/path/to/smaller-quant.gguf"  # Q4 ≈ 16 GB vs Q6 ≈ 28 GB
# Comment out the code slot to save ~7 GB:
# code = "/path/to/coder.gguf"
context_size = 16000  # KV is linear in ctx; halving from 50K → 25K saves ~3 GB
```

Disable the daemon's lint/test watchers if they're firing during your bench (they spawn cargo invocations that compete for memory):

```toml
# .sovereign/sovereign.toml at your workspace root
# Comment out [test_runner] and [lint_runner] entirely.
```

### Pi crashes with "no provider 'commonwealth'"

Run `bash scripts/setup-pi-provider.sh` (idempotent). Verify:

```bash
cat ~/.pi/agent/models.json | jq '.providers.commonwealth.baseUrl'
# → "http://localhost:9741/v1"
```

### Bench reports `exit=write_thrash` and the agent never built anything

The same-path-write detector fires when the model writes the same file 3× without a verify step in between. Common causes:
- Model emitted an absolute path that the executor rejected (look at `requests.jsonl` tool args). The cargo-shape error responses now suggest the relative form — if you see them in the model's chat history, this is operating as designed.
- Model is iterating on a malformed file (e.g. think-block prefix). Replay the offending turn with `--strip-history` to see what the model does with a clean context.

### Bench reports `exit=verify_stuck`

Same failing build/smoke stdout 3× in a row. Model is iterating but not converging. The `last_verification_output` block in the dossier carries the cargo errors verbatim — if the model isn't acting on them, replay with `--strip-history` to see whether dossier noise is the problem vs the model itself.

### Bench reports `exit=cycle_limit` 

The Implementer ↔ Evaluator loop ran 6 round-trips without an `agent_done`. The rePlan transition should have fired at cycle 3, routing the next handoff back to Planner with the full failure dossier. If Planner emitted a fresh `agent_plan` and the model still couldn't converge, that's an honest measurement of the model's algorithmic ceiling on this problem.

### Mesh routing variance (peer node serves some requests)

Symptom: daemon log shows `mesh-inference: routing complete() to peer` for bench requests. Peer node may have different config (alternation_grammar off, different model, etc.) → measurements get noisy.

Fix: pass `SOVEREIGN_DISABLE_PEER_INFERENCE=1` to the daemon. Restart with the env var in scope, e.g.:

```bash
SOVEREIGN_DISABLE_PEER_INFERENCE=1 \
SOVEREIGN_DISABLE_AUTO_RESUME=1 \
sovereign daemon restart
```

The daemon also short-circuits to local on the alternation_grammar / structured_output codepaths when this env var is set.

### Pytest "no module named pytest"

```bash
python3 -m pip install --user pytest
```

The Python-language problems use `python3 -m pytest -q tests/test_integration.py` as the verify command — pytest must be importable by the same `python3` the daemon spawns.

### Daemon log has "Verifying X..." popups + I/O freezes (macOS)

Gatekeeper / amfid verifying newly-cargo-linked binaries each rebuild. Workaround:

```bash
sudo spctl developer-mode enable-terminal
```

Or stop the daemon's lint/test watchers during heavy iteration (see "daemon dies" above).

## Common run shapes

```bash
# Full library, native runner, primary model, 1 trial each:
./target/release/sovereign-agent-bench run \
  --agent native \
  --problems 1.1,1.2,1.3,2.1,2.2,3.2,3.2-lights-out-python \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/full.json \
  --artifacts-dir /tmp/full

# Pi runner against the 9B coder slot:
./target/release/sovereign-agent-bench run \
  --agent pi \
  --problems 1.1,1.2,1.3 \
  --model commonwealth/coder \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/pi-1x.json \
  --artifacts-dir /tmp/pi-1x

# Native, multi-trial variance signal on the hard problem:
./target/release/sovereign-agent-bench run \
  --agent native \
  --problems 3.2-lights-out-python \
  --trials 5 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/r.json \
  --artifacts-dir /tmp/r

# From-scratch tier (no scaffold) — measures project-authoring:
./target/release/sovereign-agent-bench run \
  --agent native \
  --tier from-scratch \
  --problems 2.1 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/r.json \
  --artifacts-dir /tmp/r
```

## Where to go next

- `sovereign/crates/sovereign-agent-bench/HANDOFF.md` — per-session work diary, latest investigations
- `sovereign/SYSTEM_OVERVIEW.md §4.18` — architecture map for the canonical-tool layer + role split
- `~/.claude/plans/i-want-to-pickup-sorted-eagle.md` — original PR plan + methodology criteria (convergence as correctness)

If something feels wrong and the troubleshooting section doesn't cover it: capture a full trial (1 problem, 1 trial, artifacts on), then `replay` whichever turn looks off. The model-to-model layer now renders errors in cargo-shape texture — the artifacts will tell you which layer rejected and why.
