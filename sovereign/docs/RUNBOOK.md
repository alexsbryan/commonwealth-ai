# RUNBOOK — operating the sovereign inference stack

> The on-call handoff artifact (2026-06-10). Written for an engineer who did
> NOT build this system. Judgment content (decision trees, noise bands, memory
> budgets) is hand-written and dated; anything with a single in-code source of
> truth is generated or referenced-by-command — the flag table lives in
> [`retrieval-pipeline.md`](./retrieval-pipeline.md) (generated from the code),
> the health-check list is `sovereign doctor --json` (self-describing), and
> common setup failures live in [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md)
> (this runbook points there rather than duplicating it).

## 1. The first five minutes

Symptom → what to run, in order. Stop at the first step that explains it.

| Symptom | Do this |
|---|---|
| Requests hang / connection refused on :9741 | `sovereign daemon status` → if down, `sovereign doctor` (check `daemon_supervised` — an unsupervised daemon stays down after a crash) → `tail -50 ~/.svrnmesh/logs/daemon.err` |
| "Daemon restarted by itself" | Expected if supervised. `grep "daemon: shutdown signal received" ~/.svrnmesh/logs/daemon.err` for the forensic line (signal source + peak RSS). `rss` near 24+ GiB → jetsam/OOM; see §4. `memory-watch: HARD limit breached` → the RSS guard restarted it deliberately. |
| `daemon start` looks stuck | Slow boot ≠ failed boot. Cold model load is 30–60s; progress lines print every 10s up to `SOVEREIGN_DAEMON_READY_TIMEOUT_SECS` (default 120). Re-check with `sovereign daemon status`. |
| Chat answers are wrong/empty but daemon is up | Enable the retrieval trace (§5) and re-ask: which pipeline step zeroed the pool? Then check corpora exist: `curl -s localhost:9741/status \| jq .knowledge` |
| A bench "regressed" | Read §6 noise bands FIRST. Check the row for `⚠ baseline Nd old` — a stale baseline measures weeks of drift, not your change. |
| A code change has no effect | You probably rebuilt the wrong binary. §8 sibling map; the dispatcher warns when a sibling is older than itself. |
| Edits to lint/test behavior invisible | The watcher daemon holds old code: `sovereign daemon restart` after rebuilding `sovereign-cli-daemon`. |

## 2. Daemon lifecycle cheatsheet

```
sovereign daemon status    # is :9741 answering, how many models
sovereign daemon start     # background via pidfile (~/.svrnmesh/daemon.pid)
sovereign daemon stop      # SIGTERM via pidfile → port-lookup → service manager
sovereign daemon restart   # stop + start
sovereign daemon reload    # hot-reload config (no availability gap)
sovereign daemon run       # foreground (dev) — Ctrl-C to stop
```

- Readiness wait: `SOVEREIGN_DAEMON_READY_TIMEOUT_SECS` (default 120s);
  progress feedback every 10s.
- Bind collision ("something is already listening on :9741") = a daemon is
  already running — `daemon start` refuses rather than racing it.
- `install-service` refuses while a manually-started daemon runs (it would
  spawn a second daemon that loses the bind and crash-loops). `sovereign
  daemon stop` first.
- Logs: `~/.svrnmesh/logs/daemon.{err,log,out}` — copy-truncate rotation at
  10 MiB, 5 backups per stream, every 30 min (`log_rotation.rs`). The doctor
  `log_dir_size` check fires only if that loop broke.

## 3. Supervision (who restarts the daemon)

- **Supervised (the converged state):** `sovereign install-service` registers
  launchd (macOS, `~/Library/LaunchAgents/com.sovereign.daemon.plist`) or
  systemd `--user` (Linux). Semantics: **crash / non-zero exit → auto-restart;
  clean exit (`daemon stop`) → stays down.** `sovereign setup` installs this
  by default; dev boxes using `daemon start` are unsupervised until converged.
- **Check:** `sovereign doctor` → `daemon_supervised` row. `doctor --fix`
  runs the install for you.
- Manual control of a supervised daemon:
  `launchctl {stop|start} com.sovereign.daemon` ·
  `systemctl --user {stop|start} sovereign.service`.
- Uninstall: `sovereign uninstall-service` (or remove the plist/unit).
- Verified 2026-06-10: `kill -9` on the supervised daemon → relaunch within
  seconds, models reloaded, `/status` uptime reset.
- **"Daemon went down during a test run":** grep `daemon: shutdown signal
  received` — if `signal="SIGTERM"` with healthy RSS, something *deliberately*
  stopped it (clean exits stay down by design). Known historical culprit: the
  `phase3_serve_lifecycle` stop test reached past its sandbox via the :9741
  port fallback and the launchctl leg (fixed 2026-06-10 with
  `SOVEREIGN_STOP_SANDBOXED=1`, an automation-only knob that confines
  `daemon stop` to the pidfile legs — operators never set it).

## 4. Memory budget + canonical model config

**Known-good on a 64 GB host:** primary `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL` +
fast `Qwopus3.5-4B-v3-MTP-Q8_0` + embed `qwen-embedding-0.6b`.
Anti-pattern: two ~30B models — jetsam kills the daemon somewhere past
~44 GB RSS with no drain and no log. Baseline RSS right after model load is
~12 GB; the fast+embed slots are eagerly pinned (~8 GB floor).

Runtime guard (`memory_watch.rs`, sampled every 60s):

| Knob | Default | Meaning |
|---|---|---|
| `SOVEREIGN_RSS_SOFT_LIMIT_MB` | 70% of RAM (Linux) / 50% (macOS); 20480 only when RAM is undetectable | `warn!` on upward crossing, re-warn ≤ every 15 min |
| `SOVEREIGN_RSS_TRIM_MB` | `min(soft limit, 20480)` | `malloc_trim(0)` — hands freed glibc arena memory back to the OS, ≤ every 10 min |
| `SOVEREIGN_RSS_HARD_LIMIT_MB` | unset (**off**) | graceful self-SIGTERM + **non-zero exit** → supervisor relaunches clean before jetsam SIGKILLs mid-write |

**This table said the soft default was `20480` until 2026-09-12 and that had
been wrong since 2026-07-19**, when the limit became RAM-derived. On a 128 GB
host it resolves to 89,653 MB, so the alarm that caught `rss_mb=54426` on
2026-07-13 (`peak_rss_mb=89369`, logged with `soft_limit_mb=20480`) has been
effectively off on large-RAM boxes ever since. That is why the growth below
went unreported three times.

**Growth you cannot see in `inference.resident` is usually the allocator, not
a leak.** glibc auto-trims only the MAIN arena, on `free()`; per-thread arenas
are never returned on their own. Measured on a 20 h daemon 2026-09-12: RSS
22,261 MB, glibc holding 20.76 GB from the OS of which **15.91 GB was free in
bins** against 4.85 GB live — ten arenas over 500 MB were >95% free, 13.58 GB
pinned by 26 MB of live data. `SOVEREIGN_RSS_TRIM_MB` now reclaims this
automatically; the one-shot manual form, which returned 14.4 GB with the
daemon intact, is:

```
# from INSIDE the toolbox — the toolbox shares the host PID namespace, but
# only there do libc symbols resolve.
gdb -p <pid> --batch -ex 'print (int) malloc_trim(0)' -ex detach
```

To see the split before trimming, dump glibc's own books — `<total type="rest">`
is the free bytes, `<system type="current">` is what it holds from the OS:

```
gdb -p <pid> --batch -ex 'print (void*) fopen("/tmp/mi.xml","w")' \
  -ex 'set $f = $' -ex 'print (int) malloc_info(0, $f)' \
  -ex 'print (int) fclose($f)' -ex detach
```

Forensics: every shutdown logs signal source + peak RSS
(`daemon: shutdown signal received`); ≥24 GiB peak is flagged as probable
jetsam. Live values: `curl -s localhost:9741/status | jq .process` —
`rss_mb`, `peak_rss_mb`, `uptime_seconds` (uptime reset = a real restart
happened). `sovereign doctor` repeats the comparison as `daemon_memory`;
`doctor --watch` makes it a 30s memory pager.

## 5. Glassbox map (diagnosing from logs)

Enable targets per-run: `RUST_LOG="warn,<target>=info" <command>` — or for
the daemon, restart with the env set.

| Tracing target | What you'll see |
|---|---|
| `retrieval.pipeline` | One line per pipeline step with `chunks_before/after/delta` — answers "which step changed the pool" |
| `retrieval_audit` | Turn-level composition: `post_merge`, `expansion_decision`, `turn_summary`, `deep_turn_summary` (per-corpus/article counts) |
| `retrieval.seal` | Corpus-seal integrity on scoped conversations (`bleed=` names offenders) |
| `[router]` lines / `router.*` | Per-message intent decision: embed sim/margin + nearest exemplar, or coarse-LLM verdict + confidence |
| `synth.refusal_retry` | Refusal detection + retry on the synthesis call |
| `llama_cpp` | Raw llama.cpp engine output (verbose; model-load + decode errors) |

Worked example — "why did this answer ignore my corpus?":
`RUST_LOG="warn,retrieval.pipeline=info,retrieval_audit=info" target/debug/sovereign-cli-llm eval run --bank <bank> --synth --isolate`
then read the step deltas: `main_retrieval_mesh` +N (retrieval found it?),
`noise_floor` −N (dropped for zero overlap?), `truncate_merged` (out-ranked
at the merge?), `expansion_decision` (which expander ran and why).

`/status` glossary: `mesh.*` (gossip view), `inference.loaded_models`
(plan vs actually-resident), `knowledge.hosted_corpora` +
`total_chunks_searchable` (live index inventory), `process.*` (§4),
`rpc_worker` (present only when a worker is genuinely accepting).

## 6. Bench gates — reading a verdict without panicking

Noise bands (codified 2026-06-10; treat sub-band motion as weather):

| Lane type | Signal | Noise band |
|---|---|---|
| Retrieval recall (HARD lanes) | deterministic source/fact recall | exact — any delta is real (embeddings + ANN are stable) |
| Synthesis answer-equiv (SOFT) | LLM-judge score | ±0.04–0.06 run-to-run; the gate already floors its threshold at 0.05 |
| Routing accuracy (HARD) | classifier decisions | embed-router verdicts deterministic; **coarse-LLM verdicts at `confidence=0.00` can flap** between runs (MoE nondeterminism at temp 0) — a flapping probe wants an embed exemplar, not a re-run |
| Chaos / mechanism-fidelity | absolute verdicts | never gate on the absolute number; only the paired `*-gate` lane (regression vs baseline) is HARD |

Baseline age: every row and the summary footer warn past
`SOVEREIGN_BASELINE_MAX_AGE_DAYS` (default 14) — `⚠ baseline 41d old
(2026-04-30)`. **Warn-only; verdicts don't change.**
(`SOVEREIGN_BASELINE_AGE_STRICT=1` makes lane gates fail on over-age, for CI.)

Re-minting a baseline (the legitimate path):
1. Adjudicate every diff first — per-question, actual numbers (the report
   JSON carries `baseline.results` vs `current.results` per bench). Decide
   per probe: desired new behavior (update the fixture's expectation with a
   dated rationale comment) vs regression (fix exemplars/code).
2. Only then: `sovereign bench all --filter <bench> [--routing-only|--synth]
   --update-baseline`, re-run to confirm green, and **commit the dated
   snapshot** so the next box agrees.
Never `--update-baseline` to silence a red you haven't explained.

Minting does NOT wait for a quiet host, and never did require one after
2026-09-08. `svrn quality check --mint` mints against whatever the machine is
and records the conditions: every `summary.json` row carries `load_start` /
`load_end` and `daemon_uptime_secs_start` / `_end`, and a lane baseline
carries `host_load_1m`. Read those before you read a wall-clock delta. The
`host-quiet:4` precondition that used to stand in front of these lanes is
deleted (ARCH §18.2) — on this host it was unmet most of the time, so it
produced abstentions rather than protection, and nobody had derived the 4.

Two readings, not one, and they answer different questions:

- **`load_start` vs `load_end`** — a row that began quiet and ended at load 30
  is a different story from one that was busy throughout.
- **`daemon_uptime_secs_start` > `daemon_uptime_secs_end`** — the daemon
  restarted inside that row. That row is not slow, it is INTERRUPTED: the
  models were evicted and re-loaded under it. Discard it; do not average it in.

And the reading a load number does NOT support: **a 503 is not a busy host.**
On 2026-09-08 the primary slot refused 82 consecutive one-token probes across
a 1-minute load range of 25.1 down to 6.2 — identical at both ends, which is
what rules load out. The body said `model_not_available` and `/status` said
`inference.resident: []`; the daemon had booted with no model plan installed.
So when a wall-clock lane is could-not-judge and its log carries 503s, read
the RESPONSE BODY before reading the load: `model_not_available` /
`model_not_ready` means the stack is not there (`slot-decodes` unmet, a true
`never-ran`), and only a body about queueing or wait is about contention.

While you are there, two `/status` fields that do not agree and neither of
which is safe to gate on, both observed 2026-09-08 on this host:

- `inference.resident` listed three models — fast, primary, embed, all
  `resident: true` — while `inference.loaded_models` was `[]` in the same
  response. One of them is wrong and the reader cannot tell which.
- `inference.resident` was `[]` while `process.rss_mb` was 34,265; the
  pre-restart process was reported holding ~42 GB wired with the same empty
  list. Residency does not describe what the daemon is holding.

The covariate this repo does read off `/status` is `process.uptime_seconds`,
which is monotone within a process and therefore says the one thing it claims.

## 7. Retrieval pipeline knobs

Generated reference: [`retrieval-pipeline.md`](./retrieval-pipeline.md) —
step sequences + the full `SOVEREIGN_*` registry with defaults and verdict
buckets (validated-ON / experimental-OFF / tunables / debug). Regenerated by
a freshness-gated test; if it disagrees with the code, the build is already
failing.

## 8. Which binary do I rebuild?

`sovereign` is a thin dispatcher that **execs sibling binaries** — rebuilding
the wrong one is a silent no-op. The dispatcher warns when a sibling binary
is older than itself (mute: `SOVEREIGN_NO_STALE_WARN=1`).

| Verbs | Owning crate (`cargo build -p …`) |
|---|---|
| `daemon`, `setup`, `doctor`, `install-service` | `sovereign-cli-daemon` (restart the daemon to load it!) |
| `bench`, `eval`, `chat`, `enrich`, `corpus`, `mesh`, `recipe`, `atlas` | `sovereign-cli-llm` |
| `tools`, `code`, `project`, `atos` | `sovereign-cli-dev` |
| `init`, `status`, `notes`, `drift`, `reflect`, … | `sovereign-cli` (in-process) |

The full verb map lives in [`CLI_REFERENCE.md`](./CLI_REFERENCE.md).
Rule of thumb: after touching daemon-side code, the change is live only
after `cargo build -p sovereign-cli-daemon && sovereign daemon restart`.
