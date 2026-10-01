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
| F4 | (added by the seat 2026-10-01, phase-b-96, from the release-note census) `svrn atos`, `svrn design` print bare usage and exit 1; `project design\|plan` and `drift accept` print misleading errors; `amend design` and `audit <feature-id>` silently run other verbs. | Every retired spelling announces its retirement through the one `announce_retired`; the substitutions refuse by name. A test drives each through the built dispatcher. Row pb-distribution-f4-retired-verbs. |
| F5 | .github/workflows/cli-release.yml still builds and packages the three old binaries; F1's census reads the other two lists only. | The census reads the workflow's lists too, watched red today. Row pb-distribution-f5-release-ci. |
| F6 | the guest door's TCP listener serves `door_router` without `api_keys::seal` (guest_door.rs:429); its ALPN twin is sealed (daemon.rs:1650). | A keyed daemon's door refuses an out-of-scope key on every listener; an e2e exercises it. Row pb-distribution-f6-guest-door-seal. |
| F7 | the on-prem kit renamed main's firm-rag-daemon.service and does not retire it, so an upgrade runs two daemon units. | install.sh retires every main-era unit name; a test names a leftover. Row pb-distribution-f7-onprem-upgrade-unit. |
| F8 | (operator, phase-b-98, lands before the merge) a node upgraded across the flip sits off its mesh, and its donor stops, with no message until someone runs `svrn mesh up`. | Boot output and `svrn mesh status` name `svrn mesh up` on a main-era data dir, and not after the handover; one decider, the handover's own marker. Row pb-distribution-f8-upgrade-off-mesh-named. |
| F9 | (operator, phase-b-98) collaborate ingest on the stock binary has never run end to end on cut. | An e2e completes it through ingest's port with a real cw-rails. Row pb-distribution-f9-stock-collaborate-e2e. |
| F10 | (operator, phase-b-98) first-boot and handover moves keep no copy (media viewer id) or overwrite theirs on a second run (`config.toml.bak`), and none has a written rollback. | Every move keeps its first original; a RUNBOOK rollback section is run once in a sandbox. Row pb-distribution-f10-migration-backups. |
| F11 | (operator, phase-b-99) the de-embed forked the daemon's panic hook, memory soft limit and staleness warning into two crates; only log_rotation was finished. | One implementation each in host-kit; clone-gate's count drops and is banked. Row pb-distribution-f11-daemon-twins. |
| F12 | (operator, phase-b-99) a copy closes a forbidden edge and no gate sees it. | `xtask clone-gate` fails when duplicated production lines rise; blocking in `xtask quality` and the queue's BOUNDARY check; watched red on a planted copy, green on a move. Row pb-distribution-f12-clone-gate. |

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
| P6 upgrade (added 2026-10-01, phase-b-98) | the cut daemon booted on a sandbox data dir seeded as main left it, before and after `svrn mesh up`'s handover (F8's e2e, re-run at C) | names `svrn mesh up` before, not after; the handover keeps its originals (F10). The live node crossed the flip on 09-30, so it cannot read this path. |
| P8 on-prem | sovereign/deploy/onprem: package.sh, install.sh into a sandbox prefix on sandbox ports, then acceptance.sh (pb-distribution-onprem-kit's instrument) | every acceptance check passes against the daemon's port. The nginx leg passes or is named owed. `strings` on the onprem binary finds no withheld surface |

Not re-run, with the reason:
- **Tensor split.** pb-serve-distributes-bar passed at ab64e8dac (decode 103.5%, first token 100%), and the
  serve-mesh lift re-proved the split over cw-rails' rpc_tensor bridge after the flip (1c120f23d's
  body, PASSED twice). The review's "still unmeasured" predates both.
- **SEP install from scratch.** It takes hours. retrieval-prod covers the installed corpus.

## Not blocking: filed to phase-c

These come from the review and are true of split deployments rather than the stock path. Until phase-b-97
(2026-10-01) four of them had no phase-c row despite this list; they are now pc-rpc-probe-identity and
pc-split-deploy-honesty (phase-b-101 merged the other three):
- admin reload reporting success without checking serve's resident models;
- svrn's self-report staying stale after serve restarts;
- `/v1/mesh/status` not telling "serve down" from "serve slow";
- the RPC direct-IP probe accepting a connect with no identity check (bc323187d);
- a remote `[node] entry` serve's model-file refusal;
- `svrn mesh fetch-model`'s dead peer discovery (pc-fetch-model-peer-discovery).

The review's cold-cache 30 s list timeout went with the daemon's model-files forwarder, which the flip
deleted (1c120f23d). Serve's own route is the one to check, in phase-c.

## Merge preparation: the operator's, after the gate

- Re-register this repo with the deployed node's code watcher: `svrn project register` in the repo. The seat
  unregistered it on 2026-10-01 at ~19:55Z (`svrn project unregister commonwealth-ai`): with three loops
  committing, its SCIP rebuilds ran nearly back to back at 13-16 GB each (watch-commonwealth-ai-scip.log),
  and host memory fell to 6-7 GB available. The index on disk keeps answering, unrefreshed, until then.

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
| P8 check 4, pre-gate (35B) | 2cd4828d1 | 3 | FAIL by the bar, closed by operator ruling phase-b-95. acceptance 32/33; check 4 verdict=unverified; re-asks cannot_know_from_here 1, unverified 1. Prose honest 3/3. Primary Qwen3.6-35B-A3B-MTP-UD-Q6_K (the kit's Q4_K_M is not on this host, named). Filed: pc-partial-decline-verdict, pc-sealed-posture-web-offer (now a bullet of pc-onprem-followups, phase-b-101). | pb-par-onprem target/ralph/phase-b/onprem-proof/prove35.log |
| P8 nginx leg, pre-gate | 30aa81286 | 1 | PASSED, with check 4 under phase-b-95. The leg's first run found the kit's nginx config never loaded (`proxy_http_version` duplicate, firm-rag.conf:131, since 2026-08-03); fixed in 30aa81286 with a structural xtask scan watched red. Then nginx 1.30.5 (toolbox) `-t` successful on the kit's installed config, changed only in cert (self-signed), ports (19080/19443) and log paths for a rootless sandbox; acceptance through BASE_URL=https://127.0.0.1:19443 (ACCEPTANCE_INSECURE=1): 58 checks, 57 pass, 25 of them at the front door, 1 FAIL (check 4), 0 UNSURE. | pb-par-onprem target/ralph/phase-b/onprem-proof/{nginxleg.out,nginxleg-acceptance.log,nginx-leg/} |
| owed before the gate: DONE (phase-b-94) | | | Per-stage ceilings and primary/short throughput bars for Qwen3.6-35B-A3B-MTP-UD-Q6_K declared from the committed main-era reading, stack 2ce389007280 (2026-09-13, ef21e3a08 is an ancestor of main 18f783f44), by each file's existing rule. A median of an even n reporting its larger value is an instrument finding for phase-c. | sovereign/bench/quality-check/baselines/*/2ce389007280 |
| T0 lint, whole workspace (toolbox) | 3b8515d5f | 1 | PASSED: exit 0, 0 errors, 28 s. Load 0.96 / 5.03 / 5.49. | target/ralph/phase-b/ship/lint.log |
| T0 tests, whole workspace (toolbox) | 3b8515d5f | 1 | PASSED: exit 0, 13,688 pass, 0 fail, 582 s. Beside 13,850 at 62fa4378c: 162 fewer, the deletions Tier 1 accounts for. Doctests skipped (the script's default). Load 2.99 / 5.16 / 5.52. | target/ralph/phase-b/ship/test.log |
| T0 `cargo xtask quality` | 3b8515d5f | 1 | PASSED: 12 enforcing gates green, boundary-gate 0 violations, clone-gate 13,826 at its baseline. concept-gate (advisory) could-not-judge: the graph was indexed at 9033e0f7 and the repo is unregistered from the code watcher until merge preparation. Load 1.99 / 6.80 / 8.31. | target/ralph/phase-b/ship/xtask-quality.log |
| T0 `./scripts/pre-push.sh`, hooks on (`core.hooksPath=.githooks`) | 3b8515d5f | 1 | PASSED: exit 0, 16 s. Three advisory lanes red: size-gate (75 keys grew against the main-era baseline, re-derived at origin/main in merge preparation), deletion-manifest (p0-root-junk grew), hakari-verify. Load 1.77 / 6.51 / 8.19. | target/ralph/phase-b/ship/prepush.log, target/quality-check/20261001-141727 |
| T0 desktop `npm run check && npm run test` | 3b8515d5f | 1 | PASSED: exit 0, 53 files, 469 tests. Node 20.20.2 is present here. Load 1.66 / 6.04 / 7.98. | target/ralph/phase-b/ship/desktop.log |
| T0 macOS whole suite; Windows crosscheck | | 0 | NEVER-RAN: owed to the macOS peer. found_and_join mdns, code_mcp_e2e reindexer and admin_join_serves_venues_e2e 403 stay could-not-attribute until it runs. | |
| T1 test inventory | 18f783f44 → 3b8515d5f | 1 | PASSED at commit level. This parser counts attribute-marked fns (`#[test]`, `#[*::test]`, rstest, test_case): 14,021 → 13,948, 733 names gone, 660 new. Every gone name is attributed by `git log -S` to one of 44 commits, and none is unattributed. The seven largest (2ea67a59f 263, 1c307943c 109, 1c120f23d 100, 5cb09f22b 65, b48cbc767 51, 822681564 28, a546a456b 15) were read. ATOS (FIVE_PROGRAMS:1391) and sovereign-server/studio/mobile (§2b, §5; O2 keeps mobile retired) are features the operator ruled out. 1c307943c, 822681564, 3ec99625f and cd0ee7834 moved tests, with their paths in the body. 1c120f23d and b48cbc767 carry a ledger. a546a456b's 15 have successors (7dadd8980, ralph/next/phase-b/o3-tests.md), and containment_guard_e2e is back in sovereign-stock and sovereign-serve. For the 37 groups of 8 or fewer, the check was a keyword screen of each body (test, ledger, delete, move): every body has a hit. That screen is not a per-name read. | target/ralph/phase-b/ship/inventory.{txt,json,py} |
| T1 verbs | 3b8515d5f | 1 | PASSED. `contract census`: 0 run steps assert nothing (148 run, 125 assert output). The built dispatcher was driven with each retired spelling: `atos`, `design`, `project design`, `project plan`, `drift accept`, `amend design` and `audit <id>` each print `has been retired` with the replacement and exit 2. `mobile` names the deleted host and exits 1. None prints bare usage. The 11 names removed from quality/env-flags.toml are all named in the release note draft. | target/ralph/phase-b/ship/{contract-census,retired-verbs}.log, env-removed.txt |
| T2 P1 corpora | deployed node (sovereign-stock from ~7574869e5, not yet restarted on C) | 1 | PASSED, with a named substitution. `svrn corpus list` prints only the built-in catalog, on main as on cut (inventory.rs:18 at both 18f783f44 and C), so it cannot read the installed set. `svrn corpus status` was read instead: 2,173 corpora (372 ready, 1,794 building, 7 unsearchable) against 2,185 entries under ~/.svrnmesh/indexes. The 12 not listed are 11 database files and node_id, plus wikipedia-partition-node-44ae76142b0c3c72, which is a partition of the ready `wikipedia`. | target/ralph/phase-b/ship/corpus-status-before.{log,ids}, disk.ids |
| T2 P1 notes | live ~/.svrnmesh, read-only | 1 | PASSED: all 9,987 rows of notes.db.pre-pb-notes-memory are present, 6,427 in notes.db and 3,560 in sovereign.db memory_notes, none in both and none lost. This matches 3210f56d9's documented split exactly. Rows added since: notes.db 614, memory_notes 6. | target/ralph/phase-b/ship/notes-count.log, notes_count.py |
| T2 P6 upgrade | 3b8515d5f | 1 | PASSED in the whole-workspace run at C: solo_rails_e2e::a_main_era_data_dir_boots_naming_svrn_mesh_up, identity_handover::tests::the_upgrade_notice_names_mesh_up_until_the_handover_runs, and F10's four migration_backup_tests (the rollback script included) all pass with no failure. | target/ralph/phase-b/ship/junit-C.xml |
| T2 P8 on-prem | 3b8515d5f | 2 | PASSED, with check 4 under phase-b-95. package.sh --profile dev, then install.sh --no-systemd into a sandbox on :19741, with the pre-gate fixtures and the 35B primary (Qwen3.6-35B-A3B-MTP-UD-Q6_K). Run 1 (load 2.13): acceptance 32 of 33, and check 4 read verdict=unverified where cannot_know_from_here was expected, as at 2cd4828d1. Run 2, the nginx leg (nginx 1.30.5, `-t` ok, 8 conf lines changed for the sandbox, BASE_URL https://127.0.0.1:19443, load 0.87): 58 of 58 pass, 25 at the front door, and check 4 passes. So check 4 varies run to run (n=2: 1 fail, 1 pass). `strings -a` on the installed onprem binary finds 0 of NOT_COMPOSED's two literals (stock 1 and 1), and the control literal 1. package.sh's own gate agrees. | target/ralph/phase-b/ship/p8/{package,install,acceptance,nginxleg-acceptance}.log, nginxleg.out, strings.log |
| T2 P3 idle | 3b8515d5f | 1 each | SCREEN on DEBUG builds; the release reading is OWED. Both bars are release-profile, and this row builds no release. cw-rails, debug, default, settle 60 s, window 300 s: 0.520% and 0.587%, under the 2% bar (8dc4ff1f6's debug reading beside was 0.483%). sovereign-stock, debug, three reads. First: the instrument's absent-embed config panics a debug build, because a `debug_assert!` sits in vendored llama-cpp-4 model.rs:1880; release builds compile it out, so this is not a C regression. Second, with the real Qwen3-Embedding-0.6B as embed (a named substitution) and a 60 s settle: 33.2%. That window held about 1,600 boot-time embed calls, from 21:37 to 21:40. Third, settle 240 s: 1.74%, under 2.004%. Hot threads: measurements-re 172 and rails-kv-dial 126 ticks, against an unreachable rails. Finding for the operator, not a bar: the release instrument's absent embed means it never sees this boot embedding. | target/ralph/phase-b/ship/idle/{readings-r1,readings.jsonl,readings-settle240}/ |
| T2 P2 quality screen, P1 answer, P5 mesh, P5 restart, P3 latency, first attempt | | 0 | NEVER-RAN in that session: its `daemon stop && daemon start` was refused by the permission layer. The director restarted the node on C at 14:55 (phase-b-107); the rows below are read on it. | ralph/decisions/phase-b-107.md |
| T2 P3 idle, RELEASE (the bars' instruments, phase-b-107) | 3b8515d5f (tree at f47ab8602 differs only under ralph/) | 3 each | PASSED. Built release into target/ralph/idle-target (CARGO_PROFILE_RELEASE_STRIP=none; stock 7m48s, cw-rails 1m25s). cw-rails as 8dc4ff1f6 (fresh CW_RAILS_DIR, no peers, settle 60 s, window 300 s): 0.163%, 0.160%, 0.173%, mean 0.165% against the 2% bar (minted 0.141%). Stock by e098d2112's unchanged driver scripts/idle-stock-bars.sh (absent embed, installed under ~/.cache/pb-idle-stock): 0.787%, 0.783%, 0.793%, mean 0.788% against 2.004% (minted 1.059%). Beside, gating nothing: serve first token 20.3, 20.2, 20.0 ms (minted 20.0). The two read concurrently on separate roots and ports. Load at the starts 5.49 / 0.79 / 0.90 (the first right after the stock build). | target/ralph/phase-b/ship/idle-release/{run.sh,stock-C*.log,cwrails/}, target/ralph/phase-b/idle/stock/readings.jsonl (stock-rC1..3) |
| T2 P1 answer | deployed node, sovereign-stock pid 2214681 at C | 1 | PASSED. `svrn chat ask --corpus qc-arch-tour-2ce38900` (the Architecture Tour, installed): DeepQuery, 20 citations, provenance names only that corpus, epistemic verdict mixed (6 holdings). Load ~1.3. | target/ralph/phase-b/ship/p1-answer.{json,err} |
| T2 P2 quality screen | deployed node at C; stack fab853c8f1a9 | 1 per lane | NOT PASSED: 5 of 8 NEUTRAL, 3 alarm. NEUTRAL against the flip's screen: retrieval-prod 10/10 (=), enrichment-f1 24/29 (=), chaos-monkey 7/7 and answered 4/4 (=), synth 3/3 (=, second run; the first, 20261001-153012, read "daemon unreachable" in a cw-rails stall), routing cells_v1 27/27 (=). Alarms, each escalated to ABAB n=3 against B per the method, NOT RUN (NEEDS_HUMAN): (1) chat-ask 7 of 19 failed: the turn searched 0 corpora because the lane's new corpus id has no `corpus_state` row, and rows are reconciled only at daemon boot (sovereign-daemon daemon_cmd/corpus_registry.rs; the same boot-only reconcile is at 18f783f44 in sovereign-cli-daemon daemon_cmd/mod.rs:909). The id is new because 2c56d4b22 edited chat-ask.toml and throughput.toml, which moved the stack fingerprint from 2ce389007280 to fab853c8f1a9 and left the declared baselines incomparable. Read against the old id, the same question passes (P1 answer). (2) routing's paraphrase half errored: "daemon unreachable" during a cw-rails stall. (3) knowledge-gym 05_noresults_honesty 0/3 against 1/3 at the flip; `max_lookup_calls` read 2 against a max of 1 in all three replays. Throughput is under P3 latency. Loads 0.5-3.9, no cargo running. | target/quality-check/20261001-{150053,150327,150845,151301,151700,152700,152731,153012,153048,153328}, target/ralph/phase-b/ship/p2/{load.log,repro-ask.json} |
| T2 P3 latency | deployed node at C | 1 | ALARM, escalation owed. Every probe is inside the 09-13 baseline's bands (2ce389007280; decode within 10%, prefill and TTFT within 25%): primary/short decode 46.4 tok/s (baseline 46.4), TTFT 336 ms; fast/short 46.2 (46.2), TTFT 339 (403); primary/long 51.2 (53.4), TTFT 10,154 (10,212); fast/long 55.9 (55.9), TTFT 3,733 (3,867). The lane itself is could-not-judge on the moved fingerprint. The e2e turn read 44,616 and 31,235 ms against the baseline's 13,397 (tolerance 30%) and the flip's 35,112 and 12,261. That turn is an unscoped KnowledgeQuery over 376 local corpora, the same plan at the flip (375). Per the row, the release first-token re-read against 74fad65d4's bars and ABAB against B are owed. | target/quality-check/20261001-150845 |
| T2 P5 mesh | deployed node at C; cw-rails pid 1090860 | 1 | NOT PASSED. With rails answering (16:08), one `/v1/knowledge/search` planned 2 peer offerings, and Alexs-MacBook-Pro-2 served 5 hits through cw-rails' iroh bridge, with no error. Meshsonics, 3 of 7 online. But 3 of the 6 fan-out plans since the restart read `peer_roster=[]`: during a cw-rails stall, `RailsRoster::members` (sovereign-serve rails_mesh.rs) turns an unreadable roster into no members, logged at debug, so the turn searches local corpora and names no absence. cw-rails stalls because its kv pump loops on `activity-private`: seal (~2,830 rows above the floor), an ~82 s snapshot that re-appends them, and seal again; 14 cycles between 15:00 and 15:25, begun about 07:00 today. Rails' HTTP is unanswered through each snapshot, and the daemon's `/v1/models` then takes 3.0 s and `/status` 6.0 s (5 ms and 20 ms otherwise). `svrn mesh status` at 15:25 answered "cw-rails ... not reachable". | target/ralph/phase-b/ship/{p5-knowledge.json,p5-mesh-status.json,p5-mesh-status.log}, p2/{rails-coupling.log,rails-stall-window.log}, ~/.commonwealth-rails/rails.log |
| T2 P5 restart | sovereign-stock pid 2214681 at C | 1 | PASSED on the director's restart (phase-b-107, 14:55, `daemon stop` then `daemon start`, two calls): the node answers (`/health` ok, turns served above), and it is on the mesh (`/v1/mesh/status`: Meshsonics, running, peer served hits). cw-rails pid 1090860 was untouched by it (started 2026-09-30 20:42). Not repeated here: the row forbids a second restart while the node lives. | ralph/decisions/phase-b-107.md, target/ralph/phase-b/ship/p5-mesh-status.json |
