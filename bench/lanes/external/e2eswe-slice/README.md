# E2E-SWE hard-slice battery (external lane)

A 4–6 hour ruler, not a gate: the four hardest Python tasks from
[E2E-SWE](https://github.com/facebookresearch/E2E-SWE) (facebookresearch,
186 build-a-whole-repo-from-a-spec tasks, CC BY-NC 4.0), run under the
benchmark's own standardized agent loop and served by our daemon. Bars are in
`PREREG_E2ESWE_SLICE_PREREG_20261007.md`, registered before any data.

Like `../swebench/`, this lane is deliberately **not** wired into `svrn bench
gate` — it is run by hand, and its value is the standardized harness (upstream's
mini-swe-agent scaffold, config verbatim), not a CI number.

## Why a custom driver instead of `mini-extra swebench`

`mini-extra swebench` (what `../swebench/arms/mini_swe.sh` uses) runs the
SWE-bench shape: fix an issue in an EXISTING repo, dockerized per instance.
E2E-SWE is the other shape — build a repo from an empty /app against a spec —
which is why upstream's own adapter (`agents/msw_harbor.py`) drives
mini-swe-agent's Python API (`DefaultAgent` + `LitellmModel` + a custom
environment) instead of the swebench runner. This lane mirrors that adapter
against a local `bwrap` environment: workdir bound at /app, no network, venv
provided. The agent config itself is upstream's, byte-checked by
`manifest.json`; only the environment is ours.

## Verification and the one substitution

Upstream verify: fresh container from the task image → `bash ./setup.sh` →
`pytest --ctrf /logs/verifier/ctrf.json <target> -v --timeout=N -rA`; reward 1
iff pytest exits 0. Ours: fresh COPY of the workdir → same setup.sh → same
pytest invocation with `--junitxml` in place of `--ctrf` (pytest-ctrf is not
on PyPI; the graded per-test counters and the reward definition are unchanged).
For cement and hojichar the sandbox starts a pinned redis (the tasks expect one
live on localhost:6379); `setup` builds it into `target/e2eswe-slice/bin/`.

Task content is fetched at setup and hash-checked against the committed
`manifest.json` (pinned upstream commit `b11ac067`) — nothing is committed to
this repo. `tasks/<id>.toml` carries the committed FACTS only: declared deps,
test target, timeouts, hidden-test counts, spec sizes, the redis flag.

## Run it

```bash
cd bench/lanes/external/e2eswe-slice
./fetch.sh                                          # materialize + verify tasks
uv run run_battery.py setup                         # redis + venvs + offline gate
# serve the battery arm (after the GPU is free):
#   sh ../../../agent-coding/arms/serve-arm.sh B 18092 131072
#   python3 ../../../agent-coding/arms/tap.py --listen 127.0.0.1:18193 \
#     --upstream http://127.0.0.1:18092 --log target-e2eswe.tap.jsonl --arm B
uv run run_battery.py probe --url http://127.0.0.1:18193/v1 \
  --arm-log ../../../../target/agent-coding-arms/B-18092/server.log
OPENAI_API_BASE=http://127.0.0.1:18193/v1 OPENAI_API_KEY=dummy \
  uv run run_battery.py run --task ldaptor --model Qwen3.8-27B-UD-Q6_K_XL
uv run run_battery.py score --run-id <id> --run-rc <rc>
```

Each `run` writes `runs/<id>/{meta.json,trajectory.json,turns.jsonl}`; `score`
adds `junit.xml`, `setup.log`, `verify.log` and appends one row to
`target/e2eswe-slice/results.jsonl` (schema `e2eswe-slice-row/v1`; wall-cap
runs come back as exit code 124 — score with `--run-rc 124`).

## Files

- `fetch.sh` — pinned tarball → `target/e2eswe-slice/{upstream,tasks}`, manifest-checked.
- `manifest.json` — SHA-256 of every committed-relevant upstream file + agent yaml.
- `tasks/<id>.toml` — committed facts per task.
- `run_battery.py` — setup / probe / run / score (PEP 723, mini-swe-agent 2.4.6).
- `score_junit.py` — pure junit → counters (`--self-test` covers all-pass/partial/zero/absent).
- `mock_server.py` — scripted stub server for the no-GPU dry run.
- `PREREG_E2ESWE_SLICE_PREREG_20261007.md` — bars, gates, decision rule.
