fp-25: after fp-10's answer (b), the rpc-worker half is done (6b0a8e8a0). The rest of the edge needs three placement decisions the row does not name.

# fp-25 — decision package (2026-09-24)

boundary-gate **56 violation(s)**, delta 0. `sovereign-cli-daemon → sovereign-inference` is still red.

## (a) The unit and its row

`ralph/next/five-programs/STATE.md:120`, marked `[~]`:
"DIAL the post-setup half, keep the first run standalone … When fp-10 has
landed, this row is the cli-daemon repoint plus the exec shape fp-10 chose; if
fp-10's answer is that the spawner names ONE binary, `rpc_worker_main::run`
deletes here and only the exec probe remains."

## (b) What was done, and what was measured

- **Done in 6b0a8e8a0:** `rpc_worker_main::run` no longer runs here.
  The engine is built only in `sovereign-daemon`: cli-daemon's `daemon run`
  execs it (`daemon_bin.rs`), and the desktop embeds no daemon
  (`no_daemon_role_census.rs`). So `current_exe() --rpc-worker`
  (`rpc_distribution.rs:2390`) always names that binary, and it has its own
  arm (`bin/sovereign-daemon.rs:63`). `Launch::parse` still maps a bare
  `--rpc-worker` anywhere in argv to RpcWorker (`launch.rs:184`,
  test `launch.rs:835`), so an argv that reaches cli-daemon anyway is exec'd
  unchanged to the sibling. The behaviour is the same.
- Checks: `clean` exit=0 (184G), `lint` exit=0 (scoped to cli-daemon,
  0 errors), `layer` exit=0, `boundary` 56 violations.

What is left in `sovereign-cli-daemon/src` (from grep, excluding comments):

| symbol | sites | nature |
|---|---|---|
| `hardware::detect_hardware` | setup_cmd/mod.rs:242,560; fim.rs:236 | needs llama.cpp (`hardware.rs:88`) — the first-run probe |
| `hardware::select_profile` | setup_cmd/mod.rs:246,542; fim.rs:239 | pure fn over `HardwareProfile` (`hardware.rs:53`), not in contracts |
| `llama_logs::LlamaLogs` | lib.rs:129 (`is_verbose`, feeds the daemon tracing filter); setup_cmd/mod.rs:71 (`install_global` for the probe) | env decider plus a llama callback install |
| `setup_planner::*` | args.rs:85,150; catalog.rs:9; download.rs:10; fim.rs:46; mod.rs:19,30,488 | 718 lines; reqwest+fs+futures, all of which contracts already has; no inference-internal user |
| `capacity::{check_fit_sized, min_total_vram_mb, SizedSlot}` | daemon_cmd/vram_plan.rs:22 (`svrn daemon vram-plan`) | 749 lines, pure; ALSO used inside inference (`reranker_standalone.rs:128-138`) |
| `GgufExpectation`, `validate_gguf`, `HardwareProfile`, `ProfileName` | download.rs, mod.rs | path rewrite to sovereign-contracts, closes nothing alone |
| `smoketest::SMOKETEST_FLAG` | lib.rs:292, `#[cfg(test)]` | the row says STAYS; it can stay as a dev-dependency |
| Cargo features `windows-vulkan` / `windows-cuda` | Cargo.toml:103-104 → `sovereign-inference/*` | see decision 3 |

## (c) What the operator must decide

1. **Where do `setup_planner`, `capacity` and `select_profile` go?** The row
   calls them "portable moves" but does not name a home. Two readings conflict:
   - Moving them into **sovereign-contracts** works mechanically (the deps are
     already there, and `gguf_validator` is the precedent). But §12 decision 3
     admits only wire FORMAT/VOCABULARY to contracts ("no program crate is
     promoted"), and these are deciders. The desktop's svt-7 note
     (`sovereign-desktop/src-tauri/src/setup_plan.rs:1-22`) calls model choice
     "the daemon's decision — it is the process that loads the weights".
   - Or the planning goes with the serving binary: cli-daemon execs
     `sovereign-daemon` for the plan, the same way the desktop spawns
     `svrn setup --plan --json`. That means a new launch mode and moving the
     setup planning out of `setup_cmd`, which is a phase rather than a row.
2. **What is the exec shape for the first-run hardware probe?** The row says
   "the exec shape fp-10 chose". fp-10 chose one only for the rpc-worker, not
   for a probe. The structural form is a new `Launch` variant, for example
   `--probe-hardware` printing `HardwareProfile` JSON, served by
   `bin/sovereign-daemon.rs`. Every exhaustive `Launch` match has to decide
   what it means: cli-daemon lib.rs `dispatch`, the daemon bin's let-else refusal (`bin/sovereign-daemon.rs:70`), `daemon_services.rs:646`
   and desktop `main.rs:131`. `LlamaLogs::install_global` (setup_cmd/mod.rs:71)
   moves into the probe. `LlamaLogs::is_verbose` at lib.rs:129 still feeds the
   `llama_cpp=` directive of the cli-daemon filter. The options are to move the
   pure `LlamaLogs::{resolve, from_env}` into contracts (an env decider, which
   decision 3 again questions) or drop the directive here, since nothing in the
   process would emit on `llama_cpp` any more. Dropping it changes a pinned
   filter string.
3. **Who owns the Windows GPU features?** `docs/ENV_FLAGS.md:116` and
   `sovereign/SYSTEM_OVERVIEW.md:3744` say (ENV_FLAGS cites a stale `:260`) `sovereign-cli-daemon` owns
   `windows-vulkan`/`windows-cuda` "as the process that loads the weights".
   `scripts/stage-daemon-sidecar.sh:92` builds only `-p sovereign-cli-daemon
   --bin sovereign-cli-daemon`. Dropping the dependency needs somewhere else
   for those features, for example new forwarding features on
   `sovereign-daemon`. The documented build contract changes with it. Since the
   §11 step-10 fork, the weights load in `sovereign-daemon`, and that binary is
   not built by the staging script. That looks like an existing gap, and it is
   outside this row.

The alternative to all three is to record the state honestly: an
`[[exception]] package = "svrn"` for `sovereign-cli-daemon → sovereign-inference`,
with the reason "first-run setup probes hardware and plans models in-process
before any daemon exists (TSV:61 behaviour_delta)". That is the fp-10 (b)
pattern, costs no code and makes 56 → 55.

## (d) To resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.

## (e) Director resolution (2026-09-24, decision five-programs-30)

Every fact above was reproduced at 6b0a8e8a0 (boundary-gate 56). The charter cannot settle this fork. Each closing path is one of the following: an `[[exception]]`, which is the operator's; a new shared leaf, also the operator's; contracts, which fails the fs-free test (capacity.rs:96 and setup_planner.rs:339-401 call std::fs); or a change to the Windows sidecar build contract, which is end-user-observable (stage-daemon-sidecar.sh:91 builds cli-daemon only, and sovereign-daemon has no windows-* features). fp-25 is parked on the new row HUMAN-fp25-setup-host, which carries options (a)-(c). The recommendation is (b), the exception, which is fp-10's answer applied to first-run setup. The loop continues on its other rows.
