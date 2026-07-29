# Desktop quality surface — every gate, and which flags are load-bearing

The desktop's verification is not one suite. It is nine, in four processes,
across three Playwright configs, and several of them are silently useless if you
omit a flag or leave a port occupied. This is the map, so the next person does
not have to reconstruct it from shell history.

Companion docs: `AGENTS.md` answers *which tool to test a given kind of code
with*. This answers *what exists, how to run it, and what it actually proves*.

---

## The short version

Four commands gate a merge. Run these before you push:

```bash
npm run check     # svelte-check --fail-on-warnings   — 663 files
npm run test      # vitest run                        — 43 files / 370 tests
npx playwright test                                   # synthetic e2e — 275 tests
npm run sabotage  # negative controls                 — 19 mutants + 1 self-control
```

All four run in CI (`.github/workflows/ci.yml`, job `desktop`). Everything else
in this document runs **by hand only** — see [What CI does not
run](#what-ci-does-not-run), which is the most important section here.

---

## The layers

| Layer | Command | What it actually proves | In CI |
|---|---|---|---|
| Types | `npm run check` | Svelte + TS typecheck across 663 files | yes |
| Unit | `npm run test` | Logic in `src/**/*.{test,spec}.ts` under jsdom | yes |
| Synthetic e2e | `npx playwright test` | The real Svelte frontend against a **mocked** Tauri backend | yes |
| Negative controls | `npm run sabotage` | That the suites above can actually fail | yes |
| Real-mode e2e | `npm run test:e2e:real` | The real frontend against a **real** desktop process + daemon | **no** |
| Fault injection | `npm run test:e2e:faults` | Kill/restart/recover against a supervised desktop | **no** |
| Demo reel | `npm run demo` | 9 product beats; a failed beat exports no clip | no |
| a11y | `npm run a11y` | Accessibility report | no |
| Soak / chaos / personas | `npm run soak` · `chaos` · `personas.mjs` | Long-run stability and answer quality | no |
| **Production boot chain** | `scripts/wizard-verify.sh` | The path a **shipped install** actually takes: fresh wizard → `complete_setup` → supervised relaunch → `current_exe() --daemon-child` → Attach. Linux + macOS. | no |
| Whole-stack smoke | `scripts/desktop-smoke.sh` | All of the above, budgeted, in one run | no |

### What the three Playwright configs are for

Spec selection is **directory-based**, so a spec is in a suite by where it
lives, not by how it is named.

| Config | `testDir` | Backend |
|---|---|---|
| `playwright.config.ts` (default) | `tests/e2e/specs` | Mocked via `fixtures/tauri-shim.js` |
| `playwright.real.config.ts` | `tests/e2e/real` (ignores `**/faults/**`) | Real desktop binary via the command bridge |
| `playwright.faults.config.ts` | `tests/e2e/real/faults` | Real desktop, supervised, own lifecycle |
| `playwright.demo.config.ts` | `tests/e2e/demo` | Real, against the operator's live daemon |

`real` and `faults` are separate configs because the fault specs kill processes
and own ports — they cannot share a run with anything.

### Fidelity — how far each layer sits from what a user runs

The layer table says what each suite *proves*. This says how much that proof is
worth for the **shipped** app, which is a different question and the one that
actually governs confidence.

| | Layer | Backend | Boot mode |
|---|---|---|---|
| F0 | vitest | jsdom | none |
| F1 | synthetic e2e | mocked Tauri over `vite dev` | none |
| F2 | real-mode e2e | real desktop binary + fixture daemon | `Local{DesktopLegacy}` / forced-local |
| F3 | attach real-mode / demo | real daemon, real models | `Attach` |
| F4 | fault suite | supervised child daemon | supervisor, **`SOVEREIGN_CLI_PATH` branch** |
| F5 | `wizard-verify.sh` | real binary, private netns | supervisor, **`current_exe() --daemon-child`** |
| — | packaged `.dmg`/`.exe`/`.AppImage` on a clean machine | — | **nothing automated** |

**CI stops at F1.** Production is F5 (`supervisor_setup.rs:39-46` — supervised
is the default since 2026-07-18; `:102-109` — a *fresh* boot is
`Fresh`/`DesktopLegacy` and falls through to the wizard unsupervised, so the
supervised chain only engages from the **second** launch onward).

Two consequences worth stating plainly:

- **F1 cannot assert that any answer is correct.** Every "answer" in the
  synthetic suite is a string the test injected via `chat.api.completeMessage()`.
  It tests the UI's reaction to fabricated events — a legitimate thing to test,
  and not a thing to draw product confidence from.
- **F4 and F5 exercise different branches of the same function.**
  `resolve_daemon_child()` (`supervisor_setup.rs:64-79`) prefers
  `SOVEREIGN_CLI_PATH` when set. Every supervised lane in the repo sets it
  (`faults/spawn.ts:151`, `tests/e2e/scripts/lib/harness.mjs:257`) except
  `wizard-verify.sh`, which unsets it. So F5 is the *only* coverage of the
  branch a packaged install takes. It isolates via a private netns on Linux
  and via checked-free ports on macOS (there is no netns equivalent — it
  refuses to start unless `:9741`/`:9745` are free, which `desktop-smoke.sh`
  Phase 6 arranges for you). Verified 12/12 on Darwin, 2026-07-28.

---

## Load-bearing flags and env vars

This is the part that is impossible to reconstruct from the code.

### Flags you must not drop

| Flag | Where | Why it is load-bearing |
|---|---|---|
| `--fail-on-warnings` | `npm run check` | Without it svelte-check exits 0 on warnings and the gate passes while the app has accessibility and unused-export problems. `check:loose` is the no-gate variant — do not wire it into CI. |
| `-c playwright.real.config.ts` | real-mode | Bare `playwright test` silently runs the **synthetic** suite instead. Same for `-c playwright.faults.config.ts`. |
| `--allow-empty` | `scripts/sovereign-test.sh` | A run matching zero tests exits **4**, not 0. Pass this only when you genuinely expect an empty scope; a filtered run that matched nothing verified nothing. |
| `--allow-dirty` | `npm run sabotage` | Escape hatch only. The default refusal exists because the script rewrites tracked files and a SIGKILL mid-run would lose uncommitted work git cannot return. |

### Real-mode e2e (`tests/e2e/real/global-setup.ts`)

| Env var | Effect |
|---|---|
| `SOVEREIGN_REAL_ALLOW_ATTACH=1` | Attach to an existing daemon on `:9741` instead of starting a hermetic one. **Non-hermetic** — knowledge and inference state become whatever your box has. |
| `SOVEREIGN_REAL_XVFB=1` | Wrap the app in `xvfb-run -a`. Required on headless Linux. |
| `SOVEREIGN_REAL_KEEP_PROFILE=1` | Don't wipe the scratch profile between runs (for triage). |
| `SOVEREIGN_REAL_PROFILE_DIR` | Name the scratch profile dir under `test-artifacts/`. |
| `SOVEREIGN_REAL_CHAT_MODEL` / `SOVEREIGN_REAL_EMBED_MODEL` | Point at real GGUFs. Setup **fails** if these don't resolve — the models are not in the repo. |
| `SOVEREIGN_DEMO=1` | Skip every fixture and governance plant (demo capture only). |

The harness always sets `SOVEREIGN_COMMAND_BRIDGE=1` and
`SOVEREIGN_COMMAND_BRIDGE_LEDGER`; the app's env inherits `process.env`, so
anything you export reaches the desktop process.

### Fault suite (`tests/e2e/real/faults/spawn.ts`)

| Env var | Effect |
|---|---|
| `SOVEREIGN_USE_SUPERVISOR=1` | Run the daemon as a supervised child (set automatically for supervised spawns). |
| `SOVEREIGN_CLI_PATH` | Which `sovereign-cli` the supervisor executes. |
| `SOVEREIGN_COMMAND_BRIDGE_PORT` | Per-instance bridge port. |
| `SOVEREIGN_FORCE_LOCAL=1` | Force the local-only inference path. |

### Meta-quality instrumentation

| Env var | Effect |
|---|---|
| `SOVEREIGN_INVOKE_COVERAGE=<path>` | Records which of the 251 Tauri commands a run actually reached, newline-delimited, first sighting only. Off entirely when unset. Diff against the registry with `scripts/desktop-invoke-coverage.py`. |

Note: the desktop does **not** call `promote_legacy_env()`, so `SOVEREIGN_*` is
the correct prefix for desktop-only vars and no `SVRNMESH_*` bridging happens
in-process. The `bridged N legacy SOVEREIGN_* env var(s)` line you may see comes
from the daemon child the desktop re-execs, which inherits the environment.

---

## Ports — the invariant that silently invalidates runs

| Port | Owner |
|---|---|
| `5173` | Vite dev server |
| `9741` | The daemon |
| `9745` | Command bridge (harness ↔ desktop) |
| `9751` | The faults suite's child daemon |

**`:9741` must be FREE before a real-mode, faults, or full workspace test run.**
This is measured, not folklore: with the daemon up, three `sovereign-compute`
supervisor/child tests fail; with `:9741` free, the same tree is 8329/0. The
real-mode setup refuses to start when the port is occupied unless you pass
`SOVEREIGN_REAL_ALLOW_ATTACH=1`, which trades hermeticity for convenience.

```bash
sovereign daemon stop   # before
sovereign daemon start  # after
```

`scripts/desktop-smoke.sh` handles this handoff for you (Phase 4 frees `:9741`,
runs, then restores the resident daemon).

---

## How we know the tests themselves are worth anything

Three instruments, added because "the suite is green" and "the app works" are
different claims:

1. **Invoke coverage** — `SOVEREIGN_INVOKE_COVERAGE` + `scripts/desktop-invoke-coverage.py`.
   Measures reach across the 251-command surface (14.3% at last measurement).
   Deliberately a coverage-of-surface number rather than an assertion count:
   assertion counts inflate for free, surface reach cannot move without reaching
   a new command.
2. **Fixture liveness** — `FixtureExpectation { minChunks, why }` in
   `tests/e2e/real/global-setup.ts`. Every fixture declares the content it must
   hold, asserted at setup. `minChunks: 0` is legal but must be *stated* —
   emptiness is a claim a fixture makes on purpose, never inferred. A fixture
   that silently loses its content turns every spec above it into a test of
   nothing that still reports green.

   **The synthetic side has no equivalent gate, and it has already cost us
   once.** A `tauri-shim.js` handler is a claim about a command's *shape*, and
   nothing checks it against the TS interface. `detect_hardware` returned
   `{ram_gb, cpu_cores, gpu}` against a `HardwareInfo` of `{system_ram_gb,
   gpu_available, gpu_name, gpu_memory_gb, is_unified_memory}` — every field
   read came back `undefined`, the Settings memory-budget meter computed `NaN`
   and sat in the `ok` band no matter which models were selected, and no spec
   could have caught a regression in that guard while the stub was wrong. It
   was found by mutation testing, not by anything failing. When you add or edit
   a shim default, read the interface in `src/lib/types.ts` first.
3. **Negative controls** — `npm run sabotage` and
   `tests/e2e/specs/negative-controls.spec.ts`. Breaks invariants on purpose and
   requires the owning specs to go red. Full detail in
   [`tests/e2e/NEGATIVE_CONTROLS.md`](tests/e2e/NEGATIVE_CONTROLS.md), including
   why selection method decides what a mutation bank can find. **No coverage
   holes are open** — the three the source-first probe found on 2026-07-28 were
   closed the same day and are ordinary blocking mutants now. Read the score
   with that in mind: 16/16 and 19/19 are both 100%, and only the second one
   covers the conversation ✕, the memory-budget guard, and draft persistence.

There is also a **judge-calibration gate** for the chaos/persona rubrics
(`tests/e2e/scripts/calibrate-judge.mjs`, sensitivity floor 0.85 / specificity
floor 0.8): no rubric or judge change may score runs without passing it. See
`tests/e2e/CHAOS_QA_METHODOLOGY.md`.

---

## The big harnesses

- **`scripts/desktop-smoke.sh`** — the whole-stack run, fail-fast and budgeted.
  Phase 0 (lint + svelte-check + vitest + synthetic e2e + desktop unit tests) is
  a **hard stop**; 1 perf, 2 daemon quality, 3 desktop-layer bridge routing, 5
  safety soak share the resident daemon; **Phase 4** runs after them because it
  owns its own hermetic `:9741` for managed real-mode + faults; **Phase 6 runs
  last** — `wizard-verify.sh` in a private netns, which needs no port handoff at
  all. Budgets are tunable per phase (`SMOKE_P<n>_SECS`), and skipped phases are
  always reported — no silent gaps. Exit 1 = a gate failed, 2 = hard stop or
  setup error.
- **`scripts/desktop-soak.py`** — the canonical desktop chaos + persona soak
  (`--mode dual|chaos|persona`); builds HEAD, restarts the daemon, health-gates.
- **`npm run demo`** → `npm run demo:export` — the product reel as an acceptance
  suite; a beat that fails its assertions exports no clip
  (`tests/e2e/demo/DEMO_BEATS.md`).
- Reporting helpers: `report:soak`, `report:breaker`, `report:journeys`,
  `report:ttfi`, `report:coverage`, `report:coverage:real`.

---

## What CI does not run

Stated plainly, because the gap is the thing most likely to bite you:

- **Real-mode e2e and the fault suite run in no workflow at all.** They need
  multi-GB GGUFs that are not in the repo (`git ls-files sovereign/models` is
  empty) plus xvfb. The only automation that runs them is
  `scripts/desktop-smoke.sh` Phase 4, by hand. `ci.yml` says so in a comment.
- **Consequence:** the entire real-backend layer — the invariant pack, citation
  resolution, streaming integrity, fixture liveness, crash recovery — is
  verified only when a human remembers to run it. The negative-control spec in
  the synthetic suite exists partly to compensate: it makes CI guard the
  *potency* of assertions it never executes.
- The synthetic e2e layer itself only entered CI on 2026-07-27. Before that it
  ran nowhere, and UI regressions shipped with their only regression test
  sitting in a suite nothing executed.
- No coverage holes are open. The three that were tracked in
  `tests/e2e/sabotage-bank.mjs` closed on 2026-07-28 — but note what the probe
  that found them has *not* been pointed at: every mutant in the bank is
  `suite: "synthetic"`, so nothing has yet measured whether the real-mode and
  fault layers above would catch anything. Those are the layers CI never runs.

- **An instrument that is on no map runs nowhere.** `wizard-verify.sh` — the
  only coverage of the packaged boot chain (F5 above) — sat referenced by
  `DAEMON_RESILIENCE.md` and by nothing executable from 2026-07-18 until
  2026-07-28, despite catching a ship-blocking bug on its first run:
  `mirror_to_setup_config`'s no-op short-circuit meant fresh desktop-only
  installs never wrote `config.toml`, so supervision would never have engaged
  for them. Unit tests were green; the journey was broken. It is now
  `desktop-smoke.sh` Phase 6. **When you add a harness, add it to the layer
  table in the same commit** — that table is the only thing standing between a
  good instrument and this outcome. `daemon-soak.sh`, `daemon-supervised.sh`
  and `mesh-soak.sh` are still off it.

If you are about to cut a release, `scripts/desktop-smoke.sh` is the closest
thing to a complete answer, and it is not cheap. Budget for it — and read its
scoreboard for `SKIP` rows, not just the final verdict: a lane that could not
get its preconditions verified nothing, and only the `SKIP` detail says so.

**`desktop-smoke.sh` did not run on macOS at all until 2026-07-28.** `run_capped`
shelled out to GNU `timeout`, which darwin does not ship, so every phase exited
127 — including `stop_resident_daemon`'s own capped `sovereign daemon stop`,
whose `|| true` swallowed it, so the daemon was never asked to stop and the
port poll then blamed the daemon. One missing binary, two misleading symptoms,
and a whole-stack gate that was structurally incapable of passing on half the
platforms it ships to. It now resolves `timeout`/`gtimeout`/a bash fallback and
prints which one it is using in the start banner. Windows remains uncovered.
