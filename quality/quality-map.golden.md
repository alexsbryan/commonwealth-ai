<!-- GENERATED — do not edit by hand.
     Source: quality/instruments.toml (the declared instrument registry)
     Render: svrn quality map
     Gate:   cargo xtask instrument-gate (every censused command has a row) -->

# The quality surface — every instrument, rendered from the registry

## Layers — what each instrument is, and whether CI runs it

`enforcement` says whether a not-passed verdict may fail the run that hosts it.
`in CI` is derived from `runs_in`, not asserted.

### gate — pass/fail on the repo as it stands

| instrument | command | enforcement | fidelity | cost | in CI |
|---|---|---|---|---|---|
| `api-gate` | `cargo run -p xtask -- api-gate` | advisory | F0 | unmeasured | **no** |
| `arch-gate` | `cargo xtask arch-gate` | hard | F0 | 4s | yes |
| `bench-compile` | `cargo check --workspace --benches --features corpus-engine/treesitter,sovereign-cli/dev-tools` | hard | F0 | unmeasured | yes |
| `boundary-gate` | `cargo xtask boundary-gate` | hard | F0 | 0.10s | yes |
| `check-desktop-version` | `scripts/check-desktop-version.sh` | hard | F0 | unmeasured | yes |
| `clippy-json` | `cargo clippy --workspace --all-targets --message-format=json` | advisory | F0 | unmeasured | **no** |
| `clock-gate` | `cargo xtask clock-gate` | hard | F0 | unmeasured | yes |
| `concept-gate` | `cargo xtask concept-gate` | advisory | F0 | 5s | **no** |
| `deletion-manifest` | `python3 scripts/deletion-manifest.py --verify` | advisory | F0 | 1s | yes |
| `desktop-check` | `npm run check` | hard | F0 | 10s | yes |
| `desktop-invoke-coverage-gate` | `npm run report:coverage:gate` | tracked | F0 | unmeasured | **no** |
| `docs-gate` | `cargo xtask docs-gate` | hard | F0 | 2s | yes |
| `env-gate` | `cargo xtask env-gate` | hard | F0 | 3s | yes |
| `feature-matrix` | `cargo hack check --each-feature --no-dev-deps` | advisory | F0 | unmeasured | **no** |
| `feature-powerset` | `cargo hack check --feature-powerset --depth 2 --no-dev-deps` | advisory | F0 | unmeasured | **no** |
| `instrument-gate` | `cargo xtask instrument-gate` | hard | F0 | 0.04s | yes |
| `layer-gate` | `cargo xtask layer-gate` | hard | F0 | 0.10s | yes |
| `layout-gate` | `cargo xtask layout-gate` | hard | F0 | 3s | yes |
| `lint-gate` | `cargo run -p xtask -- lint-gate --from /tmp/clippy.json` | advisory | F0 | unmeasured | **no** |
| `lock-gate` | `cargo xtask lock-gate` | hard | F0 | 0.10s | yes |
| `rustfmt` | `cargo fmt --all --check` | hard | F0 | 6s | yes |
| `size-gate` | `cargo xtask size-gate` | advisory | F0 | 4s | yes |
| `sovereign-lint` | `./scripts/sovereign-lint.sh --human --full` | hard | F0 | 27s | **no** |
| `windows-crosscheck` | `scripts/windows-crosscheck.sh` | tracked | F0 | unmeasured | **no** |
| `wizard-verify` | `scripts/wizard-verify.sh` | hard | F5 | 15m | **no** |

### suite — a body of tests run together

| instrument | command | enforcement | fidelity | cost | in CI |
|---|---|---|---|---|---|
| `cli-journey-sandbox` | `sovereign/scripts/cli-journey-sandbox.sh` | tracked | F3 | unmeasured | **no** |
| `cli-journey-verify` | `sovereign/scripts/cli-journey-verify.sh --tier 2` | hard | F3 | unmeasured | **no** |
| `contract-nightly` | `sovereign/scripts/cli-journey-nightly.sh` | hard | F3 | unmeasured | **no** |
| `daemon-supervised` | `scripts/daemon-supervised.sh` | tracked | F4 | unmeasured | **no** |
| `desktop-demo` | `npm run demo` | tracked | F3 | unmeasured | **no** |
| `desktop-e2e-faults` | `npm run test:e2e:faults` | hard | F4 | 20m | **no** |
| `desktop-e2e-real` | `npm run test:e2e:real` | hard | F2 | 50m | **no** |
| `desktop-e2e-synthetic` | `npm run test:e2e` | hard | F1 | 7m | yes |
| `desktop-journeys` | `npm run test:journeys` | tracked | F2 | unmeasured | **no** |
| `desktop-smoke` | `scripts/desktop-smoke.sh` | hard | F5 | 240m | **no** |
| `desktop-vitest` | `npm run test` | hard | F0 | 10s | yes |
| `dst-scenarios` | `cargo test -p sovereign-mesh --features dst,treesitter --test main dst_scenarios` | tracked | F1 | unmeasured | **no** |
| `hook-selftests` | `bash .claude/hooks/tests/run-all.sh` | advisory | F0 | 45s | yes |
| `mesh-soak` | `scripts/mesh-soak.sh` | tracked | F3 | unmeasured | **no** |
| `pre-push` | `./scripts/pre-push.sh` | hard | F0 | 22s | **no** |
| `shell-selftests` | `bash scripts/tests/run-all.sh` | hard | F0 | 10s | yes |
| `sovereign-test` | `./scripts/sovereign-test.sh --human` | hard | F0 | 45s | yes |
| `xtask-quality` | `cargo xtask quality` | hard | F0 | 21s | **no** |

### bench — numbers, against a baseline or a band

| instrument | command | enforcement | fidelity | cost | in CI |
|---|---|---|---|---|---|
| `build-timings` | `cargo build --workspace --timings -j 3` | tracked | F0 | unmeasured | **no** |
| `ci-bench` | `./scripts/sovereign-ci-bench.sh --quick` | hard | F3 | 40m | **no** |
| `desktop-ttfi` | `npm run test:ttfi` | tracked | F1 | 5m | **no** |
| `mtp-probe` | `scripts/mtp-probe.sh --n 5 --max-tokens 200` | tracked | F3 | 3m | **no** |
| `routing-replay` | `sovereign-cli-llm bench routing-replay --bank <bank> --bridge-url <url>` | hard | F2 | 20m | **no** |
| `throughput-probe` | `scripts/throughput_probe.py` | tracked | F3 | 7m | **no** |

### probe — observes and reports; no verdict of its own to fail on

| instrument | command | enforcement | fidelity | cost | in CI |
|---|---|---|---|---|---|
| `arch-report` | `sovereign code arch-report` | tracked | F2 | unmeasured | **no** |
| `capability-map` | `sovereign code capability-map` | tracked | F2 | unmeasured | **no** |
| `co-sweep` | `scripts/co-sweep.sh` | tracked | F0 | unmeasured | **no** |
| `daemon-soak` | `scripts/daemon-soak.sh` | tracked | F3 | unmeasured | **no** |
| `daemon-soak-report` | `scripts/daemon-soak-report.sh` | tracked | F0 | unmeasured | **no** |
| `desktop-a11y` | `npm run a11y` | tracked | F1 | unmeasured | **no** |
| `desktop-breaker` | `npm run breaker` | tracked | F3 | unmeasured | **no** |
| `desktop-chaos` | `npm run chaos` | tracked | F3 | unmeasured | **no** |
| `desktop-demo-export` | `npm run demo:export` | tracked | F3 | unmeasured | **no** |
| `desktop-invoke-coverage` | `npm run report:coverage` | tracked | F0 | unmeasured | **no** |
| `desktop-invoke-coverage-real` | `npm run report:coverage:real` | tracked | F0 | unmeasured | **no** |
| `desktop-report-breaker` | `npm run report:breaker` | tracked | F0 | unmeasured | **no** |
| `desktop-report-journeys` | `npm run report:journeys` | tracked | F0 | unmeasured | **no** |
| `desktop-report-soak` | `npm run report:soak` | tracked | F0 | unmeasured | **no** |
| `desktop-report-ttfi` | `npm run report:ttfi` | tracked | F0 | unmeasured | **no** |
| `desktop-soak` | `npm run soak` | tracked | F3 | unmeasured | **no** |
| `desktop-soak-py` | `scripts/desktop-soak.py --mode dual` | tracked | F3 | unmeasured | **no** |
| `doc-coverage` | `cargo doc --no-deps (RUSTDOCFLAGS=--show-coverage, pinned nightly)` | tracked | F0 | unmeasured | **no** |
| `drift-detect` | `sovereign drift detect` | tracked | F3 | unmeasured | **no** |
| `inner-chaos-soak` | `sovereign-cli-llm eval inner-chaos --minutes <n> --journal <path>` | tracked | F3 | unmeasured | **no** |
| `module-cycles` | `cargo modules dependencies --acyclic -p <crate>` | tracked | F0 | unmeasured | **no** |
| `pre-commit` | `./scripts/pre-commit.sh` | advisory | F0 | unmeasured | **no** |
| `rustsec-advisories` | `cargo deny check advisories` | advisory | F0 | unmeasured | **no** |
| `smoke-attach-mode` | `sovereign/scripts/smoke-attach-mode.sh` | tracked | F3 | unmeasured | **no** |
| `watchers` | `sovereign daemon start (lint/test watchers)` | tracked | F0 | unmeasured | **no** |

### control — breaks something on purpose and requires another instrument to notice — the only kind that measures what the others would CATCH

| instrument | command | enforcement | fidelity | cost | in CI |
|---|---|---|---|---|---|
| `cli-journey-selftest` | `sovereign/scripts/tests/cli-journey-selftest.sh` | hard | F1 | 3s | yes |
| `desktop-judge-calibration` | `node tests/e2e/scripts/calibrate-judge.mjs` | hard | F3 | unmeasured | **no** |
| `desktop-sabotage` | `npm run sabotage` | hard | F1 | unmeasured | yes |
| `inner-chaos-calibrate` | `sovereign-cli-llm eval inner-chaos --calibrate` | hard | F3 | 5m | **no** |
| `run-if-stale` | `scripts/run-if-stale.sh --self-test` | advisory | F0 | unmeasured | **no** |

### check — a composed lane runner with its own lane table

| instrument | command | enforcement | fidelity | cost | in CI |
|---|---|---|---|---|---|
| `cli-contract-live-verify` | `sovereign/scripts/cli-contract-live-verify.sh` | tracked | F3 | unmeasured | **no** |
| `oicp-conformance` | `scripts/oicp-conformance-lane.sh` | hard | F3 | unmeasured | **no** |
| `quality-check` | `svrn quality check` | hard | F3 | 29m | **no** |

## Fidelity — how much each green is worth

**CI stops at F1** (mocked backend — real caller, fabricated answers). Everything above that line is verified only when a human remembers to run it.

| | meaning | instruments |
|---|---|---|
| F0 | unit — no process boundary, no backend | `api-gate`, `arch-gate`, `bench-compile`, `boundary-gate`, `build-timings`, `check-desktop-version`, `clippy-json`, `clock-gate`, `co-sweep`, `concept-gate`, `daemon-soak-report`, `deletion-manifest`, `desktop-check`, `desktop-invoke-coverage`, `desktop-invoke-coverage-gate`, `desktop-invoke-coverage-real`, `desktop-report-breaker`, `desktop-report-journeys`, `desktop-report-soak`, `desktop-report-ttfi`, `desktop-vitest`, `doc-coverage`, `docs-gate`, `env-gate`, `feature-matrix`, `feature-powerset`, `hook-selftests`, `instrument-gate`, `layer-gate`, `layout-gate`, `lint-gate`, `lock-gate`, `module-cycles`, `pre-commit`, `pre-push`, `run-if-stale`, `rustfmt`, `rustsec-advisories`, `shell-selftests`, `size-gate`, `sovereign-lint`, `sovereign-test`, `watchers`, `windows-crosscheck`, `xtask-quality` |
| F1 | mocked backend — real caller, fabricated answers | `cli-journey-selftest`, `desktop-a11y`, `desktop-e2e-synthetic`, `desktop-sabotage`, `desktop-ttfi`, `dst-scenarios` |
| F2 | real binary against a fixture daemon | `arch-report`, `capability-map`, `desktop-e2e-real`, `desktop-journeys`, `routing-replay` |
| F3 | real daemon, real models | `ci-bench`, `cli-contract-live-verify`, `cli-journey-sandbox`, `cli-journey-verify`, `contract-nightly`, `daemon-soak`, `desktop-breaker`, `desktop-chaos`, `desktop-demo`, `desktop-demo-export`, `desktop-judge-calibration`, `desktop-soak`, `desktop-soak-py`, `drift-detect`, `inner-chaos-calibrate`, `inner-chaos-soak`, `mesh-soak`, `mtp-probe`, `oicp-conformance`, `quality-check`, `smoke-attach-mode`, `throughput-probe` |
| F4 | a supervised child process | `daemon-supervised`, `desktop-e2e-faults` |
| F5 | the packaged boot chain a shipped install takes | `desktop-smoke`, `wizard-verify` |

## Load-bearing — what silently weakens a verdict

A flag here is one whose absence does not fail anything; it just makes the green mean less. A precondition is a closed-set fact that must hold before the instrument can judge at all — an unmet one is could-not-judge, never a pass.

| instrument | preconditions | load-bearing | why |
|---|---|---|---|
| `api-gate` | `binary:cargo-public-api` | `the pinned nightly` | quality/nightly-pin.txt is the ONLY nightly use in the repo; a different nightly changes the rendered public API and the diff stops meaning anything |
| `ci-bench` | `port-listening:9741`<br>`slot-decodes:primary`<br>`slot-decodes:embed` | `the HARD/SOFT/TRACKED lane split` | a HARD lane breaks the build and a SOFT synth lane never does. Read lane KIND before reading a number: a TRACKED lane carries an ABSOLUTE verdict that is a finding about the system, not a regression signal |
|  |  | `--update-baseline` | capture/refresh only on a healthy daemon; a baseline minted from a degraded run silently lowers every future bar |
| `cli-contract-live-verify` | `port-listening:9741` | — | — |
| `cli-journey-sandbox` | — | `an isolated HOME + a non-default daemon port` | the only way a MUTATING journey tier may run: its steps install/remove corpora and join/leave meshes, so on the operator's real root it would eat live state |
| `cli-journey-selftest` | — | `(ten negative controls)` | it drives the real journey runner against a stub CLI and a loopback stub daemon and proves it FAILS a wrong exit code, a missing substring, a non-reversing mutation, and that it aborts a sequence after a failed step |
| `cli-journey-verify` | `port-listening:9741` | `--mutating` | REFUSES unless SOVEREIGN_JOURNEY_ISOLATED=1 and the daemon URL is not the default :9741 — mutating steps install/remove corpora and join/leave meshes |
| `clippy-json` | — | `--message-format=json` | lint-gate consumes the stream. A hand-rolled string scan mis-read diagnostics whose children precede the top-level `level` field, which is every clippy lint with a help child |
| `concept-gate` | — | `(advisory only)` | it relays `svrn code converge status`, which reads a SCIP graph that exists only on an indexed machine. On a clean checkout its only answers are COULD-NOT-JUDGE and NEVER-RAN, which is why it is in no CI job |
| `contract-nightly` | `port-listening:9741` | — | — |
| `daemon-soak` | `host-quiet:4` | — | — |
| `desktop-chaos` | `port-listening:9741`<br>`slot-decodes:primary` | — | — |
| `desktop-check` | — | `--fail-on-warnings` | without it svelte-check exits 0 on warnings and the gate passes while the app has accessibility and unused-export problems. `check:loose` is the no-gate variant — do not wire it into CI |
| `desktop-demo` | `port-listening:9741`<br>`slot-decodes:primary` | `(a failed beat exports no clip)` | the product reel is an acceptance suite: `demo:export` is what turns green beats into artifacts, so a broken beat cannot ship as a video |
| `desktop-e2e-faults` | `port-listening:9751`<br>`binary:sovereign-desktop` | `-c playwright.faults.config.ts` | a separate config because the fault specs kill processes and own ports — they cannot share a run with anything |
|  |  | `SOVEREIGN_CLI_PATH` | the supervisor prefers it when set, so this suite exercises a DIFFERENT branch of resolve_daemon_child() than a packaged install takes |
| `desktop-e2e-real` | `port-listening:9745`<br>`binary:sovereign-desktop` | `-c playwright.real.config.ts` | bare `playwright test` silently runs the SYNTHETIC suite instead |
|  |  | `SOVEREIGN_REAL_CHAT_MODEL / SOVEREIGN_REAL_EMBED_MODEL` | setup FAILS if these do not resolve — the GGUFs are not in the repo, which is the whole reason no CI job runs this |
|  |  | `SOVEREIGN_REAL_ALLOW_ATTACH=1` | attaches to an existing daemon on :9741 instead of starting a hermetic one. NON-HERMETIC: knowledge and inference state become whatever the box has |
| `desktop-e2e-synthetic` | — | `playwright.config.ts (the default)` | spec selection is DIRECTORY-based: this config's testDir is tests/e2e/specs against a Tauri backend mocked by fixtures/tauri-shim.js. It cannot assert that any answer is correct — every answer is a string the test injected |
| `desktop-invoke-coverage` | — | `SOVEREIGN_INVOKE_COVERAGE=<path>` | off entirely when unset; a run with it unset produces no ledger and this reader then exits 3 as could-not-judge rather than reporting 0% |
| `desktop-invoke-coverage-gate` | — | `--min-percent 35` | the ratchet floor. RAISE it as coverage lands; setting it aspirationally high and muting the failure is how a floor stops being read |
| `desktop-judge-calibration` | `port-listening:9741`<br>`slot-decodes:primary` | `sensitivity floor 0.85 / specificity floor 0.8` | no rubric or judge change may score runs without passing it (ARCH §18.6) |
| `desktop-sabotage` | — | `--allow-dirty` | escape hatch only. The default refusal exists because the script rewrites TRACKED files and a SIGKILL mid-run would lose uncommitted work git cannot return |
|  |  | `(runs after the suite)` | a red suite makes every verdict here meaningless, and the script refuses to start on a red baseline |
| `desktop-smoke` | `host-quiet:4`<br>`binary:sovereign-desktop`<br>`binary:timeout` | `SMOKE_P<n>_SECS` | per-phase soft budgets. A phase that runs out of budget SKIPs, and a SKIP verified nothing — read the scoreboard for SKIP rows, not just the final verdict |
|  |  | `the timeout resolver` | run_capped shelled out to GNU `timeout` until 2026-07-28, which darwin does not ship, so every phase exited 127 and the whole gate was structurally incapable of passing on half the platforms it ships to. The start banner now names which of timeout/gtimeout/bash-fallback it resolved |
| `desktop-soak` | `port-listening:9741`<br>`slot-decodes:primary` | — | — |
| `desktop-soak-py` | `port-listening:9741`<br>`slot-decodes:primary` | — | — |
| `drift-detect` | `port-listening:9741` | — | — |
| `env-gate` | — | `--update-doc` | docs/ENV_FLAGS.md is GENERATED from the registry and freshness-checked by this gate; editing the doc by hand reddens it |
| `feature-matrix` | `binary:cargo-hack` | — | — |
| `feature-powerset` | `binary:cargo-hack` | — | — |
| `hook-selftests` | — | `(non-blocking)` | the suite sits at 16 known failures; it is non-blocking in CI for the reason it was warn_gate in the hook. Promote it the day the backlog reaches zero |
| `inner-chaos-calibrate` | `port-listening:9741`<br>`slot-decodes:primary` | `sensitivity floor 0.85 / specificity floor 0.8` | no rubric or judge change may score runs without passing it — the judge-calibration gate (ARCH §18.6) |
| `inner-chaos-soak` | `port-listening:9741`<br>`slot-decodes:primary` | — | — |
| `mesh-soak` | `binary:podman`<br>`host-quiet:4` | `the podman backend` | real partitions and cgroup OOM. The nightly workflow deliberately runs the LOCAL-subprocess backend instead, so this script's fault classes are covered by nothing scheduled |
| `module-cycles` | `binary:cargo-modules` | — | — |
| `mtp-probe` | `port-listening:9741`<br>`slot-decodes:primary`<br>`host-quiet:4` | — | — |
| `oicp-conformance` | `port-listening:9741` | — | — |
| `pre-commit` | — | `CO_PRECOMMIT_STAGED / CO_PRECOMMIT_SELF` | the only way to exercise the warn path by hand without staging a peer's file — §18.1 says watch it fire |
| `quality-check` | `port-listening:9741`<br>`slot-decodes:primary` | `--mint` | without it a first run against a NEW stack fingerprint writes no baseline at all — deliberately, so a drifting stack cannot silently re-mint its own bar |
|  |  | `--budget-secs` | a lane with under 60s of runway is could-not-judge, not a pass. `--budget-secs 0` is the cheap structural proof the journey drives |
| `routing-replay` | `port-listening:9745` | — | — |
| `run-if-stale` | — | `--self-test` | watches the scheduler both FIRE and SKIP. An exclusion only ever watched go quiet is indistinguishable from a walk that stopped working (§18.1) |
|  |  | `RUN_IF_STALE_HOURS (20, not 24)` | a 24h window plus a human who logs in at roughly the same time each morning skips every other day |
| `size-gate` | — | `--tighten` | banks a real cut and never raises — always safe |
|  |  | `--accept <key>` | raises ONE ceiling. `--update-baseline` on a working tree absorbs every other crate's growth along with yours, which is the trap AGENTS.md names for this ratchet |
| `smoke-attach-mode` | `port-listening:9741` | — | — |
| `sovereign-lint` | `container:sovereign-vulkan` | `--full` | without it the run scopes to the crates owning uncommitted changes plus direct dependents; the banner names the scope, so a scoped clean run cannot be read as a repo-wide guarantee |
|  |  | `--all-targets (always on since 2026-08-07)` | compiles #[cfg(test)] code, so a test module that does not build fails here rather than minutes later in the test run |
| `sovereign-test` | `container:sovereign-vulkan` | `--allow-empty` | a run matching zero tests exits 4, not 0. Pass it only when you genuinely expect an empty scope — a filtered run that matched nothing verified nothing |
|  |  | `--filter <whole test name>` | the filter sets the BUILD scope too: the script git-greps the literal through *.rs, so a broad substring silently degrades to a full-workspace build (measured 280s vs 37.5s) |
| `throughput-probe` | `port-listening:9741`<br>`slot-decodes:primary`<br>`host-quiet:4` | — | — |
| `watchers` | — | `[watchers] enabled = false` | DISABLED in .sovereign/sovereign.toml since 2026-05-31 — the parallel cargo fan OOM'd the daemon under a resident big model. A declared opt-out is a healthy absence, which is why posture and doctor both report it Passed; the gate is the two toolbox scripts |
| `windows-crosscheck` | `binary:cargo-xwin` | — | — |
| `wizard-verify` | `binary:sovereign-desktop`<br>`port-listening:9741` | `SOVEREIGN_CLI_PATH UNSET` | every other supervised lane in the repo SETS it, so resolve_daemon_child() takes the env branch. This is the only coverage of the `current_exe() --daemon-child` branch a packaged install actually takes |
|  |  | `a private netns (Linux) / checked-free ports (macOS)` | there is no netns equivalent on darwin, so it REFUSES to start unless :9741 and :9745 are free — which desktop-smoke.sh Phase 6 arranges |
| `xtask-quality` | — | `check-mode only` | baseline mutations stay explicit per-gate, so a habit-run can never silently move a ratchet |
|  |  | `four verdicts, not two` | a gate that could not reach its evidence did not pass, and one that never ran did not pass either — the summary keeps PASS / FAIL / COULD-NOT-JUDGE / NEVER-RAN apart |

## What runs where

| venue | instruments |
|---|---|
| `by-hand` | `arch-report`, `capability-map`, `ci-bench`, `cli-contract-live-verify`, `cli-journey-sandbox`, `cli-journey-verify`, `desktop-a11y`, `desktop-breaker`, `desktop-chaos`, `desktop-demo`, `desktop-demo-export`, `desktop-e2e-faults`, `desktop-e2e-real`, `desktop-invoke-coverage`, `desktop-invoke-coverage-real`, `desktop-journeys`, `desktop-judge-calibration`, `desktop-report-breaker`, `desktop-report-journeys`, `desktop-report-soak`, `desktop-report-ttfi`, `desktop-smoke`, `desktop-soak`, `desktop-soak-py`, `desktop-ttfi`, `drift-detect`, `quality-check`, `run-if-stale`, `sovereign-lint`, `sovereign-test`, `windows-crosscheck`, `wizard-verify`, `xtask-quality` |
| `check` | `throughput-probe` |
| `ci:cli-release` | `check-desktop-version` |
| `ci:desktop` | `desktop-check`, `desktop-e2e-synthetic`, `desktop-sabotage`, `desktop-vitest` |
| `ci:desktop-release` | `check-desktop-version` |
| `ci:fmt` | `rustfmt` |
| `ci:gates` | `arch-gate`, `boundary-gate`, `clock-gate`, `deletion-manifest`, `docs-gate`, `env-gate`, `instrument-gate`, `layer-gate`, `layout-gate`, `lock-gate`, `size-gate` |
| `ci:suites` | `hook-selftests`, `shell-selftests` |
| `ci:test` | `bench-compile`, `cli-journey-selftest`, `sovereign-test` |
| `nightly` | `contract-nightly` |
| `precommit` | `pre-commit` |
| `prepush` | `arch-gate`, `boundary-gate`, `clock-gate`, `concept-gate`, `deletion-manifest`, `docs-gate`, `env-gate`, `instrument-gate`, `layer-gate`, `layout-gate`, `lock-gate`, `pre-push`, `rustfmt`, `size-gate`, `sovereign-lint` |
| `run-if-stale` | `co-sweep`, `contract-nightly`, `oicp-conformance` |
| `smoke:0` | `desktop-check`, `desktop-e2e-synthetic`, `desktop-vitest`, `sovereign-lint`, `sovereign-test` |
| `smoke:1` | `desktop-ttfi`, `mtp-probe`, `smoke-attach-mode`, `throughput-probe` |
| `smoke:2` | `inner-chaos-calibrate`, `quality-check` |
| `smoke:3` | `routing-replay` |
| `smoke:4` | `desktop-e2e-faults`, `desktop-e2e-real` |
| `smoke:5` | `inner-chaos-soak` |
| `smoke:6` | `wizard-verify` |
| `weekly:advisories` | `rustsec-advisories` |
| `weekly:api-surface` | `api-gate` |
| `weekly:cycles` | `module-cycles` |
| `weekly:doc-coverage` | `doc-coverage` |
| `weekly:features` | `feature-matrix`, `feature-powerset` |
| `weekly:timings` | `build-timings` |

### What CI does not run (60 of 82)

- `api-gate` — .github/workflows/weekly.yml (header) · runs in: weekly:api-surface
- `arch-report` — sovereign/crates/sovereign-cli/src/posture_cmd.rs (arch_row) · runs in: by-hand
- `build-timings` — .github/workflows/weekly.yml (header) · runs in: weekly:timings
- `capability-map` — sovereign/crates/sovereign-cli/src/posture_cmd.rs (capability_row) · runs in: by-hand
- `ci-bench` — sovereign/bench/README.md · runs in: by-hand
- `cli-contract-live-verify` — sovereign/docs/TESTING_SURFACE.md · runs in: by-hand
- `cli-journey-sandbox` — sovereign/docs/TESTING_SURFACE.md · runs in: by-hand
- `cli-journey-verify` — sovereign/docs/cli-contract.toml (journeys) · runs in: by-hand
- `clippy-json` — corpus-engine/xtask/src/lint_gate.rs · runs in: **nothing**
- `co-sweep` — scripts/run-if-stale.sh (header) · runs in: run-if-stale
- `concept-gate` — quality/NOUN_CONVERGENCE.md · runs in: prepush
- `contract-nightly` — sovereign/docs/cli-contract.toml (journeys) · runs in: run-if-stale, nightly
- `daemon-soak` — sovereign/docs/specs/DAEMON_RESILIENCE.md · runs in: **nothing**
- `daemon-soak-report` — sovereign/docs/specs/DAEMON_RESILIENCE.md · runs in: **nothing**
- `daemon-supervised` — sovereign/docs/specs/DAEMON_RESILIENCE.md · runs in: **nothing**
- `desktop-a11y` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-layers · runs in: by-hand
- `desktop-breaker` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-layers · runs in: by-hand
- `desktop-chaos` — sovereign/crates/sovereign-desktop/tests/e2e/CHAOS_QA_METHODOLOGY.md · runs in: by-hand
- `desktop-demo` — sovereign/crates/sovereign-desktop/tests/e2e/demo/DEMO_BEATS.md · runs in: by-hand
- `desktop-demo-export` — sovereign/crates/sovereign-desktop/tests/e2e/demo/DEMO_BEATS.md · runs in: by-hand
- `desktop-e2e-faults` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-layers · runs in: smoke:4, by-hand
- `desktop-e2e-real` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-layers · runs in: smoke:4, by-hand
- `desktop-invoke-coverage` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#how-we-know-the-tests-themselves-are-worth-anything · runs in: by-hand
- `desktop-invoke-coverage-gate` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#how-we-know-the-tests-themselves-are-worth-anything · runs in: **nothing**
- `desktop-invoke-coverage-real` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#how-we-know-the-tests-themselves-are-worth-anything · runs in: by-hand
- `desktop-journeys` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: by-hand
- `desktop-judge-calibration` — sovereign/crates/sovereign-desktop/tests/e2e/CHAOS_QA_METHODOLOGY.md · runs in: by-hand
- `desktop-report-breaker` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: by-hand
- `desktop-report-journeys` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: by-hand
- `desktop-report-soak` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: by-hand
- `desktop-report-ttfi` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: by-hand
- `desktop-smoke` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: by-hand
- `desktop-soak` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-layers · runs in: by-hand
- `desktop-soak-py` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: by-hand
- `desktop-ttfi` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: smoke:1, by-hand
- `doc-coverage` — .github/workflows/weekly.yml (header) · runs in: weekly:doc-coverage
- `drift-detect` — sovereign/crates/sovereign-cli/src/posture_cmd.rs (drift_row) · runs in: by-hand
- `dst-scenarios` — .github/workflows/ci.yml (the commented-out `dst` job) · runs in: **nothing**
- `feature-matrix` — .github/workflows/weekly.yml (header) · runs in: weekly:features
- `feature-powerset` — .github/workflows/weekly.yml (header) · runs in: weekly:features
- `inner-chaos-calibrate` — sovereign/crates/sovereign-desktop/tests/e2e/CHAOS_QA_METHODOLOGY.md · runs in: smoke:2
- `inner-chaos-soak` — sovereign/bench/chaos_monkey/README.md · runs in: smoke:5
- `lint-gate` — scripts/pre-push.sh (header — why it is not a push gate) · runs in: **nothing**
- `mesh-soak` — commonwealth/docs/MESH_QA.md · runs in: **nothing**
- `module-cycles` — .github/workflows/weekly.yml (header) · runs in: weekly:cycles
- `mtp-probe` — sovereign/bench/README.md · runs in: smoke:1
- `oicp-conformance` — sovereign/crates/sovereign-cli/src/posture_cmd.rs (oicp_conformance_row) · runs in: run-if-stale
- `pre-commit` — scripts/pre-commit.sh (header) · runs in: precommit
- `pre-push` — scripts/pre-push.sh (header — the one-minute budget) · runs in: prepush
- `quality-check` — sovereign/docs/CLI_REFERENCE.md#svrn-quality · runs in: smoke:2, by-hand
- `routing-replay` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: smoke:3
- `run-if-stale` — scripts/run-if-stale.sh (header) · runs in: by-hand
- `rustsec-advisories` — .github/workflows/weekly.yml (header) · runs in: weekly:advisories
- `smoke-attach-mode` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#the-big-harnesses · runs in: smoke:1
- `sovereign-lint` — AGENTS.md §Compilation and test feedback · runs in: prepush, smoke:0, by-hand
- `throughput-probe` — sovereign/bench/README.md · runs in: check, smoke:1
- `watchers` — AGENTS.md §Compilation and test feedback · runs in: **nothing**
- `windows-crosscheck` — .github/workflows/desktop-release.yml · runs in: by-hand
- `wizard-verify` — sovereign/docs/specs/DAEMON_RESILIENCE.md · runs in: smoke:6, by-hand
- `xtask-quality` — corpus-engine/xtask/src/quality_cmd.rs · runs in: by-hand

### What nothing runs (9)

An instrument on no map runs nowhere. This is the population `QUALITY_SURFACE.md`'s postmortem is about, and it is a list now rather than a paragraph somebody has to remember to update.

- `clippy-json` — corpus-engine/xtask/src/lint_gate.rs · runs in: **nothing**
- `daemon-soak` — sovereign/docs/specs/DAEMON_RESILIENCE.md · runs in: **nothing**
- `daemon-soak-report` — sovereign/docs/specs/DAEMON_RESILIENCE.md · runs in: **nothing**
- `daemon-supervised` — sovereign/docs/specs/DAEMON_RESILIENCE.md · runs in: **nothing**
- `desktop-invoke-coverage-gate` — sovereign/crates/sovereign-desktop/QUALITY_SURFACE.md#how-we-know-the-tests-themselves-are-worth-anything · runs in: **nothing**
- `dst-scenarios` — .github/workflows/ci.yml (the commented-out `dst` job) · runs in: **nothing**
- `lint-gate` — scripts/pre-push.sh (header — why it is not a push gate) · runs in: **nothing**
- `mesh-soak` — commonwealth/docs/MESH_QA.md · runs in: **nothing**
- `watchers` — AGENTS.md §Compilation and test feedback · runs in: **nothing**

---

**82 instruments, 8 with a negative control, 49 unmeasured cost, 26 by-hand only.** (9 run nowhere at all.)

