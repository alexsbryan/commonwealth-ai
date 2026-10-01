# Phase B ship gate

Pre-registered by the seat on 2026-10-01, before any reading it names. It replaces the review draft of
2026-09-30, which read `origin/cut` at 628b01f08 on the macOS peer; where the two differ, this file says why.

Phase B's structural finish (docs/FIVE_PROGRAMS.md: boundary-gate at 0, no `package = "svrn"` exception,
six lifts, the EmbeddedDaemon census) does not say whether a user's install, chat turn, ingest or mesh query
still works the way it did on main. This gate adds that condition at a fixed price: **about two and a half
hours of runs at the final tip on RuggedFox, plus three small fix rows before it.** Anything that costs
more runs only when a cheaper reading alarms.

> cut ships when every row below has a verdict. A row passes, or an operator ruling is named for it.
> could-not-judge and never-ran are owed, not passed (principle 5).

## Method, fixed before the data

- C is the tip after pb-distribution. B is main at 18f783f44, and B is built only for an escalation.
- Screen, then confirm. Each behavioural lane runs once at C against this host's committed baselines
  (`sovereign/bench/quality-check/baselines/<lane>/<fingerprint>/latest.json`, and each bench lane's band).
  - A reading inside its RUNBOOK §6 band is reported as NEUTRAL (screen, n=1), never as a measured delta.
  - A reading outside the band in the bad direction escalates that lane alone to ABAB with n=3 against B.
    So does a could-not-judge for want of a comparable baseline. B is built in a worktree and run
    local-only on its own port, and the lanes are pointed at it with `SOVEREIGN_DAEMON_URL`. B never
    joins the mesh: the node's key now lives in cw-rails, and a second endpoint must never hold it.
- Linux on this host is the platform of record. macOS and Windows are owed rows, named below.
- Tier 2 runs on a quiet host: no cargo build or test from any session while its lanes run (Tier 0 finishes
  first, and the seat holds other loops), with the 1-minute load recorded. This was fixed after the early
  screen below. It ran beside a worker's compile at load ~5.9 against the baseline's 3.4, and on this
  unified-memory APU a compile competes with decode for memory bandwidth.
- Every reading records hash C, n, host load and the path to its raw output.

## Fixes that land before the gate

The seat checked each of these at b90845d20. Each one is a regression that Phase B's own split
introduced, so it is Phase B's to fix, not phase-c's.

| id | defect | bar |
|---|---|---|
| F1 | landing/install.sh:8 and scripts/release-cli-local.sh:10 ship three binaries (sovereign-cli, sovereign-cli-daemon, sovereign-cli-llm). The dispatcher now execs siblings that neither ships, so a release install cannot `daemon run` (sovereign-stock) or `svrn mesh up` (cw-rails). The desktop already stages sovereign-stock (d1f3e1765). | A structural test collects every binary the dispatcher and `svrn mesh up` exec. It asserts each one is in both release lists. It is watched red at today's tip, then the lists are fixed. |
| F2 | cw-rails `join` admits a member and answers 200 when `identity::save_mesh` fails (commonwealth-rails internal.rs:272; ralph/REVIEW_FINDINGS.md:3536 assigns it to pb-mesh-exit-mesh, whose row never took it). The flip made this the node's only join route, so a founder that restarts forgets the member. | The join refuses by name on a failed save. A test drives it with an unwritable data dir. |
| F3 | standalone serve mounts `/internal/rpc-warm` with no `loopback_only` guard (sovereign-serve rpc_warm.rs; only reload.rs:52 carries the guard). rpc_warm.rs:12 asserts in a comment that serve listens on loopback, but `--listen 0.0.0.0` is accepted. | The guard is on every internal route serve mounts. A test of a non-loopback caller is refused, and a census test names any unguarded internal route. |

## Tier 0: the build gate at C (about 50 min)

| check | bar |
|---|---|
| `./scripts/sovereign-lint.sh --human --full` | exit 0 |
| `./scripts/sovereign-test.sh --human`, whole workspace | exit 0; count reported beside 13,850 at 62fa4378c |
| `cd corpus-engine && cargo xtask quality` | boundary-gate 0, every blocking gate green |
| `./scripts/pre-push.sh` | exit 0, hooks on |
| desktop `npm run check && npm run test` | exit 0 here if the toolchain is present, else owed to the macOS peer |
| macOS whole suite; Windows crosscheck | owed to the macOS peer. Three failures seen at 628b01f08 stay could-not-attribute until it runs: found_and_join mdns, code_mcp_e2e reindexer, admin_join_serves_venues_e2e 403. The fourth, f26_egress_census, passes on Linux since 62fa4378c. |

## Tier 1: nothing silently dropped (about 30 min, mechanical, no build)

- **Test inventory.** Diff the test fn names between 18f783f44 and C with `git grep`, and group every name
  gone at C by the commit that removed it. At b90845d20 that is 659 names (14,026 → 13,958). Six commits
  hold 563 of them:
  - the ATOS cut, 2ea67a59f: 243;
  - the daemon test tree moved out of mesh, 1c307943c: 108;
  - the flip, 1c120f23d, whose body carries its ledger: 104;
  - sovereign-server dropped, 5cb09f22b: 65;
  - `svrn code` lifted, 822681564: 28;
  - the harness seam, a546a456b: 15.

  Bar: every group cites one account: moved (the name or a named successor exists at C), retired with a
  ledger, or removed with a feature the operator ruled out. a546a456b's 15 tests, and c23f3b4c0's deleted
  `containment_guard_e2e.rs` (the SIGABRT boot guard), need a successor or an operator ruling (O3). Any
  name with none of these is a finding.
- **Verbs.** `sovereign contract census` reports 0 run steps that assert nothing (it read 0 at 62fa4378c).
  Every verb removed on cut prints what replaced it, or that it was retired, never bare usage text and
  exit 1. That covers the ATOS family, `design`, `project design|plan`, `drift accept` and `mobile …`.
  Env vars removed from quality/env-flags.toml are named in the release note.

## Tier 2: behaviour on the deployed node at C (about 60 min)

The operator's own `~/.svrnmesh`, written by main-era builds, crossed the flip at b71fdd08b. So the live
node is also the existing-state test.

| row | reading | bar |
|---|---|---|
| P2 quality screen | `svrn quality check`, eight lanes, about 25 min | NEUTRAL on each lane, or escalated per the method. An early screen of the flip ran at 2026-09-30 21:18 PDT (target/quality-check/20260930-211826) and is the comparison point at C. |
| P1 corpora | `svrn corpus list` against the indexes on disk | the same set |
| P1 answer | one `svrn ask` against an installed corpus | at least one citation, and the answer names the corpus |
| P1 notes | notes count at C against the pre-migration backup kept by 3210f56d9 | equal, less the migration's documented drops |
| P5 mesh | cw-rails status, plus one knowledge query while a peer is online | RuggedFox 44ae7614 is on Meshsonics. The query returns the peer's hits, or names the peer absent, never an error |
| P5 restart | `sovereign daemon stop && sovereign daemon start` (CLI, toolbox) | the node answers and is still on the mesh, and cw-rails is untouched by the daemon's restart |
| P3 idle | stock idle and cw-rails idle with their existing instruments | ≤ 2.004% (e098d2112) and ≤ 2% (8dc4ff1f6) |
| P3 latency | the throughput lane inside P2 | NEUTRAL. A release first-token re-read against 74fad65d4's bars runs only if this alarms |
| P8 on-prem | sovereign/deploy/onprem: package.sh, install.sh into a sandbox prefix on sandbox ports, then acceptance.sh (pb-distribution-onprem-kit's instrument) | every acceptance check passes against the daemon's port. The nginx leg passes or is named owed. `strings` on the onprem binary finds no withheld surface |

Not re-run, with the reason:
- **Tensor split.** pb-serve-distributes-bar passed at ab64e8dac (decode 103.5%, first token 100%), and the
  serve-mesh lift re-proved the split over cw-rails' rpc_tensor bridge after the flip (1c120f23d's
  body, PASSED twice). The review's "still unmeasured" predates both.
- **SEP install from scratch.** It takes hours. retrieval-prod covers the installed corpus.

## Not blocking: filed to phase-c

These come from the review and are true of split deployments rather than the stock path, each verified
or filed as stated:
- admin reload reporting success without checking serve's resident models;
- svrn's self-report staying stale after serve restarts;
- `/v1/mesh/status` not telling "serve down" from "serve slow";
- the RPC direct-IP probe accepting a connect with no identity check (bc323187d);
- a remote `[node] entry` serve's model-file refusal;
- `svrn mesh fetch-model`'s dead peer discovery (pc-fetch-model-peer-discovery).

The review's cold-cache 30 s list timeout went with the daemon's model-files forwarder, which the flip
deleted (1c120f23d). Serve's own route is the one to check, in phase-c.

## Merge preparation: the operator's, after the gate

- Re-derive the baselines at origin/main (AGENTS.md re-pin recipe; a file over its ceiling is split).
- Correction commits for the two mis-subjected commits, e86a91f5a and f747b311c.
- Remove the ATOS dangling links.
- One release note listing every user-visible change. The seat keeps the running list in the session
  frame.

## Operator decisions, with the seat's recommendation

- **O1, merge timing: ruled by the operator 2026-10-01 (phase-b-93), as recommended.** Hold the merge until
  pb-distribution and F1 land: the interim release install cannot start the daemon.
- **O2, on-prem: ruled by the operator 2026-10-01 (phase-b-86).** On-prem works at the end of Phase B: API keys
  resolving to `Asserted`, the daemon's API, and its own hardened distribution (rows pb-distribution-onprem-*).
  Mobile stays retired.
- **O3, test deletions: ruled by the operator 2026-10-01 (phase-b-93), as recommended.** Restore
  `containment_guard_e2e` against the stock binary; the boot guard is a safety property. For a546a456b,
  name a successor for each of its 15 tests or write one. Row pb-distribution-o3-tests.
- **O4, debug first token: ruled by the operator 2026-10-01 (phase-b-93), as recommended.** Accept the
  debug-profile x1.09-1.13 first token on debug hosts. Release bars bind (phase-b-25).
- **O5, what lands on main: ruled by the operator 2026-10-01 (phase-b-93), as recommended.** As the review
  proposed: FIVE_PROGRAMS.md, the code-binding decisions and this file land; `ralph/next/*` stays off main.

## Readings

| row | hash C | n | verdict | raw |
|---|---|---|---|---|
| P2 early screen (the flip, pre-gate) | daemon built from 1c120f23d, running since 2026-09-30 20:42 PDT | 1 | 6 of 8 passed: routing, retrieval-prod, enrichment-f1, chaos-monkey (7/7: no leak, no ungrounded assertion, no refused answerable probe), knowledge-gym, synth. chat-ask could-not-judge (15/19 passed; useful median 0.992-0.998; the 4 open checks are per-stage ceilings with no table for model stem Qwen3.6-35B-A3B-MTP-UD-Q6_K in sovereign/bench/quality-check/chat-ask.toml). throughput could-not-judge (no primary/short bars for that stem in throughput.toml). Against the 09-13 baseline (same model): decode -8 to -14% at load ~5.9 with a worker compiling, confounded. e2e runs 35,112 and 12,261 ms against a 13,397 baseline; the lane reports the larger of two as the median, so the warm run is 8% faster. | target/quality-check/20260930-211826 |
| P8 check 4, pre-gate (35B) | 2cd4828d1 | 3 | FAIL by the bar, closed by operator ruling phase-b-95. acceptance 32/33; check 4 verdict=unverified; re-asks cannot_know_from_here 1, unverified 1. Prose honest 3/3. Primary Qwen3.6-35B-A3B-MTP-UD-Q6_K (the kit's Q4_K_M is not on this host, named). Filed: pc-partial-decline-verdict, pc-sealed-posture-web-offer. | pb-par-onprem target/ralph/phase-b/onprem-proof/prove35.log |
| P8 nginx leg, pre-gate | 30aa81286 | 1 | PASSED, with check 4 under phase-b-95. The leg's first run found the kit's nginx config never loaded (`proxy_http_version` duplicate, firm-rag.conf:131, since 2026-08-03); fixed in 30aa81286 with a structural xtask scan watched red. Then nginx 1.30.5 (toolbox) `-t` successful on the kit's installed config, changed only in cert (self-signed), ports (19080/19443) and log paths for a rootless sandbox; acceptance through BASE_URL=https://127.0.0.1:19443 (ACCEPTANCE_INSECURE=1): 58 checks, 57 pass, 25 of them at the front door, 1 FAIL (check 4), 0 UNSURE. | pb-par-onprem target/ralph/phase-b/onprem-proof/{nginxleg.out,nginxleg-acceptance.log,nginx-leg/} |
| owed before the gate: DONE (phase-b-94) | | | Per-stage ceilings and primary/short throughput bars for Qwen3.6-35B-A3B-MTP-UD-Q6_K declared from the committed main-era reading, stack 2ce389007280 (2026-09-13, ef21e3a08 is an ancestor of main 18f783f44), by each file's existing rule. A median of an even n reporting its larger value is an instrument finding for phase-c. | sovereign/bench/quality-check/baselines/*/2ce389007280 |
