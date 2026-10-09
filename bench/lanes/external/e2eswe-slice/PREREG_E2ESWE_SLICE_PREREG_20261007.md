# E2E-SWE hard-slice battery — pre-registration

Bars are fixed when this file is committed, before the first battery result
lands; changes after that append under a dated heading.

## Question

What does this machine's stack — the stock daemon's OpenAI conversation path
(arm B) serving a local Qwen3.8-27B — produce on the hardest Python sample of
E2E-SWE under the benchmark's own standardized agent loop, and where do the
builds die? This is a diagnostic instrument for the stack, not a leaderboard
entry: n=1 per task, local model, 80-minute caps.

## Fixed setup

- Loop: mini-swe-agent 2.4.6 via its DefaultAgent/LitellmModel/LocalEnvironment
  protocol, config = upstream `agents/e2e_swe_mini_sweagent.yaml` verbatim
  (step_limit 1000, one bash call per turn, `COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT`
  sentinel), hash-pinned by the lane's `manifest.json`.
- Serving: arm B (stock daemon, release build) behind `arms/tap.py`; ctx 131072;
  model `openai/Qwen3.8-27B-UD-Q6_K_XL`. A second arm (Qwen3.6-35B-A3B) may be
  appended after this battery's readout, as a dated appendix — never mixed into
  these numbers.
- Thinking: off, via `chat_template_kwargs: {"enable_thinking": false}` — the
  daemon-side counterpart of the upstream A0 arm. Per-turn `max_tokens` 32768.
- Offline: every sandboxed command runs under bwrap `--unshare-net`; the gate
  is watched failing (curl must fail inside) before any run.
- Verify: upstream's contract — fresh copy of the workdir, `bash ./setup.sh`,
  then pytest with the task's own target and per-test timeout. One substitution,
  named: `--junitxml` instead of upstream's `--ctrf` (pytest-ctrf is not on
  PyPI; 404 at /simple/pytest-ctrf/). The reward definition is upstream's own:
  reward 1 iff the pytest invocation exits 0.
- Tasks: ldaptor, cement, j1939, hojichar — pinned by `manifest.json` at
  upstream commit `b11ac067`.

## Arms

- B1: Qwen3.8-27B-UD-Q6_K_XL on arm B (the serving path Phase 1 certified).
- (later, appended only) B2: Qwen3.6-35B-A3B, throughput pick.

## Schedule

One run per task, serial, single-tenant GPU. ~80 min cap per task plus verify;
the battery is a 4–6 h instrument.

## Measures

- reward per task (upstream's binary contract) and the graded per-test fraction
  (junit counters) — the fraction is the primary read for a local model.
- turns used, wall time, per-turn latency growth across the episode (this is
  the prefix-reuse gap the serving layer owes in Phase 3, measured in the wild).
- exit_status distribution (submitted / step_limit / wall_cap / error).
- famous-vs-obscure contrast across the four tasks (2 mid-fame, 2 obscure) —
  a crude but real memorization probe for a model trained on the same corpus.

## Verdict rule — four verdicts, not two

- passed: reward 1.
- failed: reward 0 with a produced verify result.
- could-not-judge: verify incomplete (no junit report, or sandbox failure) —
  excluded from every rate and reported as an instrument defect to fix.
- never-ran: no verify attempted — excluded from every rate.
A wall-capped run that still produced an installable tree is scored like any
other run; its exit_status says wall_cap. Nothing collapses into a zero.

## Harness gates — the battery counts only if all hold

- g1: ≥95% of agent turns carry a well-formed tool call (tap-judged, not
  agent-reported).
- g2: zero daemon 5xx during the battery.
- g3: `enable_thinking=false` observed on ≥99% of requests (probe against the
  arm log; the tap log is the per-request record).
- g4: the offline gate was watched failing before the first run.
- g5: ≥3 of 4 tasks exceed 20 turns before ending.

A gate failure is a harness/serving defect: fix it, and the re-run lands as a
dated appendix below. The original numbers stay on the record.

## Decision rule (fixed before data)

- Turns < 20 with error dominance → serving/harness defect: stop, fix, re-run.
  Not a model finding.
- Turns > 50 with test fraction ≈ 0 → model ceiling: report per-pillar failure
  modes; do not tune prompts, sampling, or the scaffold to move them.
- No prompt/threshold/scaffold tuning is driven by battery numbers, ever. The
  battery measures; the readout proposes separate, pre-registered changes.

## Instrument checks before results

- mock-server dry run of the whole loop (no GPU) is green.
- `score_junit.py --self-test` green on all-pass / partial / zero / absent.
- bwrap offline gate watched failing.
- `probe` shows the arm's conversation-path render with thinking off.

## Cost

~6 GPU-hours on the Halo (single tenant, after the parity replay closes);
zero external model tokens. Upstream task content is fetched, not committed.

## Appendix 2026-10-09 — daemon vs bare llama-server, 35B-A3B (fixed before data)

Operator 2026-10-09: the battery's question becomes the daemon against bare
llama-server, same model, same loop. The 2026-10-07/08 rows (27B, and the
35B-A3B `battery-35Br-*` on 7f3e06a41) stay on the record and are not mixed
into these numbers.

Arms, each behind `arms/tap.py` with the same injection
(`chat_template_kwargs: {"enable_thinking": false}`), ctx 131072,
`--parallel 1`, MTP draft depth 3, model `Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf`:

- A0 (bare): `target/llama-server-vanilla`, unpatched, built from the
  vendored llama.cpp commit 035e227, `--reasoning off` (`serve-arm.sh A0`).
- B (harnessed): the stock daemon at 14b6de2d8 (Phase 2 streaming and
  reasoning_content, the context-overflow 400, the conversation pin cut 4
  tokens short), release build, `SOVEREIGN_INFERENCE_TIMEOUT_SECS=1200
  SOVEREIGN_MAX_QUEUE_WAIT_SECS=1800 SOVEREIGN_PREFIX_STATE_MAX_MB=8192`
  (`serve-arm.sh B`).

Sampling: the loop sends only `max_tokens` (32768) and the injected kwargs,
so both servers apply the GGUF's `general.sampling.*`.

Schedule: one run per task per arm, task-major, the arm that goes first
alternating by task (ldaptor A0→B, cement B→A0, j1939 A0→B, hojichar
B→A0), each arm started fresh for each run so no cache crosses runs. 80-min
wall cap per run. Worst case ~11 h.

Measures, per task and arm: reward, graded test fraction, turns, wall,
exit_status; from the tap, per-turn time to first delta against prompt
size, and the share of turns with a well-formed call.

Gates g1-g5 hold per arm. g2 reads "zero 5xx" on both; a 400 for a prompt
over the window is the loop reaching the context edge, recorded as such,
not a gate failure, and is the same answer on both servers.

What this run can claim (n=1 per task per arm):
- Harness parity holds if B passes every gate A0 passes.
- Speed: B's median per-turn wall against A0's, reported as a ratio, with
  B ≤ 1.25x A0 named in advance as the parity bar.
- Quality: per-task test-fraction differences are reported, not judged. One
  run per task cannot separate the servers from sampling noise (the 35B's
  own run-to-run spread is unmeasured); a difference is never called a
  result from this run alone.
