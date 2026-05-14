# Codex-harness training gym

Empirical regression suite for the `/v1/responses` Codex harness pipeline.
Every fixture is a real failure mode observed in a smoke run, frozen as a
ChatCompletionRequest body. The runner replays each fixture N times and
scores against a per-fixture predicate. Pass rate is the witness.

## Fixtures

Each `fixtures/<id>_<slug>/` directory contains:

- `input.json` — full `ChatCompletionRequest` (post-frontdoor) to POST at
  `/v1/chat/completions`.
- `pass.yaml` — per-fixture success predicate.
- `README.md` — what this fixture tests; what success looks like; what the
  observed failure was.

## Pass-predicate vocabulary (`pass.yaml`)

```yaml
expected_tool: exec_command   # or "any", or omit
args_parseable: true           # require JSON args to parse
must_contain:                  # all substrings must appear in args.cmd
  - "apply_patch"
must_not_contain:              # all substrings must NOT appear
  - "tos-experiment"
content_must_contain_regex:    # regex over args.cmd
  - "\\*\\*\\* Add File: "
```

All keys optional; omitted = no constraint.

## Running

```sh
./run.sh                      # full suite, 10 replays per fixture
./run.sh -n 3                 # 3 replays per fixture (fast smoke)
./run.sh -f 001_write_stage   # one fixture
./run.sh --json               # machine-readable output
```

Daemon must be running at `http://localhost:9741` (or override
`SOVEREIGN_DAEMON`).

## Adding a fixture

1. Find a real failure in `~/.sovereign/codex-sessions/sessions.jsonl` —
   note the `response_id` of the turn that failed.
2. Copy the captured input:
   `cp ~/.sovereign/codex-sessions/raw/<rid>.input.json fixtures/NNN_slug/input.json`
3. Author `pass.yaml` with the predicate that distinguishes the fix from
   the failure.
4. Write a short `README.md`: what was the bug, what does pass look like.
5. Run the new fixture: `./run.sh -f NNN_slug`. Expect failure at this
   point — that's the bug. When the daemon's pipeline is fixed, the fixture
   passes.

## Success criterion for the suite

When every fixture passes ≥80% (10 replays), the Codex pipeline is
empirically robust against the observed failure classes. Goal:
`010_full_smoke_completion` (end-to-end task: write src/lib.rs that
compiles) passes — proving the model can complete an OICP-types
implementation through this pipeline.
