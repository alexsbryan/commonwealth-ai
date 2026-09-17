# Build latency on the hot paths — measured, and what architecture can buy

Measured 2026-09-17 at HEAD `dcb65856d` on RuggedFox (32 cores, 125 GB, mold,
no sccache, warm `target/`), inside the `sovereign-vulkan` toolbox. Harness:
`scripts/build-probe/probe.sh` (per-edit shapes) and `probe2.sh` (settled
re-measure plus the fingerprint experiment); `critpath.py` reads cargo's
`--timings` HTML and prints the serial chain that set the wall clock. Raw
tables and the cargo fingerprint excerpt: `quality/build-probe/2026-09-17/`
(a fresh run writes to `target/build-probe/`).

The question was: take the files edited most in the last 30 days, measure what
an incremental build and a focused test cost after an edit there, and find what
architecture — not profile knobs — can take off. The answer in one line: the
cost of every edit is one serial chain of six large crates, and three build
defects sit on top of it that cost more than the chain does.

## The root problem, measured

Operator, 2026-09-17: on a campaign of work orders an inordinate share of
the time goes to checking quality — builds and tests — and slow checks get
skipped. So the number this document serves is not the per-edit loop below
but what sessions actually waited on gates. `scripts/build-probe/gate-census.py`
pairs every gate command in the repo's session transcripts with its result:

| shape | runs | hours | share | median s | p90 s | sessions |
|---|---|---|---|---|---|---|
| bare `cargo build/check/test … -p` | 1428 | 18.6 | 38% | 11 | 127 | 150 |
| `sovereign-test.sh --package/--filter` | 621 | 10.3 | 21% | 25 | 171 | 112 |
| `sovereign-test.sh` (full) | 263 | 6.3 | 13% | 2 | 271 | 94 |
| `sovereign-lint.sh --full` | 313 | 5.7 | 12% | 15 | 175 | 96 |
| `sovereign-lint.sh` (scoped) | 307 | 4.7 | 10% | 15 | 154 | 86 |
| `cargo xtask …` | 302 | 1.7 | 4% | 3 | 19 | 68 |
| `pre-push.sh` | 121 | 1.3 | 3% | 13 | 108 | 39 |

30 days to 2026-09-17: 181 sessions, 3495 runs, 48.7 hours waited (a floor:
background-launched runs are timed at zero). The medians are fine; the
hours are in the tail — 68% of the time is the 475 runs over two minutes,
82% is runs over one minute — and the tail is the same thing in every row:
a configuration cargo had not built yet. Bare `cargo … -p <crate> --features …`
is the largest bucket because every hand-picked `-p`/feature pair is a new
resolution (D1). A scoped test run sampled at 123 s was 113 s of build and
11 s of tests. That is the root the levers below serve. It is the motivation, not the
score: the operator's definition of success is the per-file table — for
each hot-spot file, seconds from a one-statement edit to green (warm,
fresh-scope, full build), before and after, and the three sums
(`quality/campaigns/build-latency.toml`, SUCCESS block;
`scripts/build-probe/score.sh --compare`). Nothing composed, nothing weighted.

## Where the edits are

1070 commits in 30 days; 596 touch Rust. By crate, in `.rs` file-edits:
sovereign-mesh 766, sovereign-core 647, corpus-engine 614, sovereign-cli-llm
420, sovereign-desktop 409, sovereign-tools 269, commonwealth-api 189,
sovereign-contracts 174, sovereign-cli-daemon 160. The single hottest files are
`sovereign-mesh/src/daemon.rs` (51 commits), `sovereign-turn-client/src/lib.rs`
(50), `sovereign-core/tests/main/f26_egress_census.rs` (37), `sovereign-mesh/src/lib.rs`
(31), `sovereign-desktop/src-tauri/src/state.rs` (29). Only 261 of the 596
commits stay inside one crate; the commonest pairs are corpus-engine +
cli-llm (53), cli-llm + core (52), desktop + mesh (50), corpus-engine + core (49).

## What an edit costs today

Edit shape: one `let` statement inserted into an existing function body (a
comment-only touch is not representative — rustc's incremental cache makes it
almost free; a scoped lint after one came back in 7 s). Steady state means
the cache had already seen this scope once. Seconds of wall clock.

| edit in | scoped lint (`sovereign-lint.sh`) | full check (`--workspace --all-targets`) | full build (`--workspace`) | focused test (`sovereign-test.sh --package --filter`) |
|---|---|---|---|---|
| corpus-engine `engine/mod.rs` | 29.6 | 30.3 | 41.6 | 11.8 |
| sovereign-contracts `setup_config.rs` | 25.2 | 26.4 | 35.0 | 4.0 |
| kernel-types `lib.rs` | 20.2 | 21.8 | 29.2 | 1.0 |
| sovereign-core `deep_research/mod.rs` | 17.5 | 18.7 | 28.1 | 15.4 |
| sovereign-tools `local_corpus/manager.rs` | 12.7 | 13.8 | 20.7 | 7.9 |
| sovereign-mesh `daemon.rs` | 9.0 | 9.8 | 16.2 | 14.6 |
| sovereign-turn-client `lib.rs` | 9.3 | 10.1 | 13.1 | 2.0 |
| sovereign-cli-llm `lib.rs` | 7.7 | 8.5 | 12.7 | 13.6 |
| sovereign-cli-daemon `daemon_cmd/mod.rs` | 4.8 | 7.4 | 11.2 | 10.0 |
| sovereign-desktop `state.rs` | fails (see D2) | 7.7 → 3.4 with D3 fixed | 11.4 → 4.6 with D3 fixed | 5.0 |
| sovereign-core `tests/main/f26_egress_census.rs` | – | – | – | 2.9 |

Full-check and full-build columns are from the settled pass (`probe2.sh`:
tree settled between probes, so each row carries only its own edit); the
scoped-lint and test columns are the second of two runs. The desktop row is
the only one D3 changes: every other edit already had sovereign-mesh in its
cone.

The first time a scope is used the same commands cost far more: focused test
200 s (mesh), 190 s (core), 138 s (corpus-engine), 228 s (cli-llm), 171 s
(tools), 74 s (cli-daemon); scoped lint 192 s (mesh, first ever), 147 s
(corpus-engine), 139 s (turn-client), 92 s (contracts), 89 s (cli-llm). That
is defect D1 below, and it is the largest number in this document.

The floor with nothing edited was 7 s for a full check and 11–13 s for a full
build; after defect D3 below it is 0.8 s and 0.6 s.

## What produces those numbers

Every probe's critical path is the same chain. Read from cargo's own timing
graph, a corpus-engine edit under `cargo build --workspace`:

    corpus-engine 7.5 > sovereign-core 7.7 > sovereign-tools 6.5 > sovereign-api 4.9
      > sovereign-mesh 10.9 > sovereign-cli-llm 7.2 > link 1.4      (wall 41.4 s, 78 s CPU)

and under `cargo check`:

    corpus-engine 5.1 > core 5.1 > tools 4.2 > api 2.9 > mesh 6.4 > cli-llm 3.9 > cli 0.9   (30.0 s)

A kernel-types edit prepends `oicp-types > contracts` to the same chain; a
contracts edit prepends `contracts`. Cold, the same chain is corpus-engine 19 >
core 16 > tools 12 > api 5 > mesh 18 > cli-llm 15 (89 s of a 158 s cold check,
on a machine that spent 687 CPU-seconds in parallel elsewhere). The machine is
never the limit; the chain is. Each link is a single crate's single-threaded
front end, and no edit upstream of a link can skip it: cargo has no
early-cutoff, so a dependent recompiles whenever its dependency's rmeta is
rewritten, whether or not the interface changed.

Sizes (code lines, `src/` only): corpus-engine 172k, sovereign-cli-llm 146k,
sovereign-core 128k (runtime/ 68k, deep_research/ 26k), sovereign-mesh 88k
(+35k in one integration-test binary of 72 modules), sovereign-tools 83k,
sovereign-contracts 34k. Transitive dependents: kernel-types 55, oicp-types 47,
sovereign-contracts 38, corpus-engine 26, sovereign-core 17, sovereign-tools 9,
sovereign-mesh 4 (all four are binaries), sovereign-cli-llm 1.

## Three build defects, before any refactor

**D1. Feature unification depends on which packages are in scope.** With `-p
sovereign-core` alone, 101 packages resolve with different features than under
the full gate; with `-p sovereign-mesh`, 63. Among them are hyper (loses
`full`), hyper-util, reqwest (loses `json`), serde_json, hashbrown, once_cell,
chrono — crates under the entire tree. Every crate above a flipped one is a
new unit, so the first scoped test in a new scope recompiled 117 crates (core
scope: tokio-util, arrow, hyper, reqwest, kernel-types, contracts …) or 26
(mesh scope: datafusion-catalog upward through lance, corpus-engine and the
whole sovereign chain). `sovereign-test.sh` documents this as "cheaper disease
than cure" and shares the target dir anyway. The cure is not isolation; it is
making resolution scope-invariant: a `workspace-hack` crate (cargo-hakari)
that depends on every third-party crate with the union of features, which
every member depends on. Then `-p X` resolves identically to `--workspace`,
the 140–230 s first runs disappear, and the two check configurations the
lint script alternates between collapse into one. Bar: focused test in a
never-before-used scope ≤ 20 s (from 138–228).

**D2. The scoped lint cannot lint the desktop.** `sovereign-lint.sh` passes
`--features corpus-engine/treesitter` unconditionally, so a scope whose
closure does not contain corpus-engine (sovereign-desktop alone, 409 edits in
30 days) fails in 32 ms with cargo's "package does not contain this feature",
which the script then reports as a Fedora-host toolchain failure. The test
script already has the scope-aware `resolve_features` in
`scripts/lib/cargo-scope.sh`; the lint script carries a second, inline copy
(two implementations of one threshold). Fix: call the shared one. Bar: a
desktop-only edit gets a green scoped lint in ≤ 5 s. **Landed 4f06edf82 and
measured:** 2.9 s green (30.6 s the first time that scope is used — D1).

**D3. sovereign-mesh's build script is permanently stale.** `build.rs` emits
`cargo:rerun-if-changed=../../.git/HEAD`, which resolves to
`sovereign/.git/HEAD` — there is no such file; the repo's `.git` is one level
higher. Cargo treats a missing rerun-if-changed path as always changed, so the
script re-runs on every invocation, its output is re-stamped, and mesh plus its
four dependent binaries are dirty every time: cargo's fingerprint log for a
no-op scoped lint says exactly `StaleItem(MissingFile "…/sovereign-mesh/../../.git/HEAD")`
and then `StaleDepFingerprint` for `sovereign-mesh`, `sovereign-cli-llm`,
`sovereign-cli-dev`, `sovereign-cli-daemon`. That is the 7 s check floor and
the 11–13 s build floor: mesh 3.2 + cli-llm 3.3 checked, mesh 6.6 + cli-llm 6.0
built, for nothing, on every run by every developer. One-line fix
(`../../../.git/HEAD`, and the same for `index`). Bar: no-op full check ≤ 2 s,
no-op full build ≤ 3 s. **Measured with the fix applied:** no-op full
check 7.4 s → 0.8 s (three runs), no-op full build 11–13 s → 0.6 s, zero dirty
units in the fingerprint log. The fix is in this commit.

## Architectural levers, ranked by what they take off the chain

Each lever names the edge, the evidence that the edge is thin, and the bar.

**A1. Split sovereign-cli-llm by verb.** It is 146k lines, one compilation
unit, the last link on every chain (6–7 s build, 4 s check, on every upstream
edit), with 59 lines of tests. Only four of its fourteen large modules
reference sovereign-mesh (awareness_cmd, mesh_cmd, corpus_cmd, chat_cmd —
20k lines); bench_cmd (32k) and enrich_cmd (26k) do not. The same unit is
why a one-line edit in the 6k-line sovereign-turn-client costs 13 s: seven
cli-llm files use `TurnClient`, so all 146k lines recompile. As per-verb crates
under the dispatcher, a mesh edit recompiles ~20k lines in parallel rather
than 146k serially, and a corpus-engine edit skips the verbs that never touch
it. Bar: mesh edit, full build ≤ 10 s (from 15.8).

**A2. Take sovereign-tools off sovereign-core.** Of tools' 63 imported core
paths, `types`, `error`, `traits`, `tool_manifest` (381 uses) are re-exports of
sovereign-contracts through core's name. The real dependence is 22 files on
`health::UserOption`, `conv_tiered` rows, `memory::EntityInventory`,
`atlas_context::AtlasGraph`, `ToolRegistry`. Move those types below core (or
into contracts) and tools compiles beside core instead of after it, removing
a 4–6 s link from every chain that starts at corpus-engine, contracts or
kernel-types, and taking tools out of the core cone entirely. Bar: core edit,
full build ≤ 22 s (from 28.1).

**A3. Take corpus-engine off sovereign-contracts.** 14 files use 11 paths:
`embed_quirks`, `rebrand::svrnmesh_root`, `recipe::url_template`,
`oicp::PoolingStrategy`, and `daemon_wire::{IngestProgress, StarterQuestion,
sec_coverage}`. The knowledge engine reporting progress in the daemon's wire
types is the line drawn wrong (principle 12). With the engine owning its own
progress type and contracts converting, a contracts or kernel-types edit no
longer rebuilds the largest crate in the workspace, and corpus-engine
compiles beside contracts. Bar: contracts edit, full build ≤ 28 s (from 36.5).

**A4. Split sovereign-mesh at the `EmbeddedDaemon` hub.** `daemon.rs` (5.7k
lines, the hottest file) imports 44 sibling modules and is imported by 29; 42
files (41.5k lines) reference `EmbeddedDaemon`/`DaemonServices` and 73 files
(46.7k) do not. The daemon half took 280 of the crate's 390 edits in 30 days.
As `sovereign-mesh` (primitives: gossip, ring_sync, iroh, persist, worker
controller) plus `sovereign-mesh-daemon` (composition root and the `*_http`
handlers), an edit in daemon.rs recompiles 41k lines, not 88k plus the 35k
test binary, and cli-llm's mesh-using verbs can mostly sit on the primitives
(worker_controller, persist, deep_link, model_fetch, worker_http are all
daemon-free). Bar: daemon.rs edit, scoped lint ≤ 5 s (from 9.0), focused test
≤ 8 s (from 14.6).

**A5. Keep kernel-types a kernel.** 55 dependents, 6.3k lines, and its
30-day edits are in `quality/instruments.rs` (1.6k lines), `quality/mod.rs`,
`conformance.rs`. The ids, hashes, `Answer`, `Judgement` are used everywhere;
`quality::instruments`/`render`/`triggers` are used by five crates
(commonwealth-work, xtask, sovereign-cli, sovereign-mesh, and oicp-types for
`Precondition`/`VerdictSource` only). Moving the instrument registry out (keeping
`Precondition`/`VerdictSource` behind) turns an instruments edit from a
55-crate rebuild (21.8 s check, 29.2 s build) into a 5-crate one. Same shape
for sovereign-contracts: `rebrand` (781 lines) reaches 21 crates,
`setup_config.rs` (3.4k lines, 19 commits) 12, `daemon_wire` 12 — but the
twelve include the chain crates, so this one shortens the cone's count, not
its length, until A3 lands. Bar: instruments edit, full check ≤ 8 s.

**A6. Per-area test binaries.** `sovereign-mesh/tests/main.rs` is 72 modules
in one binary; every focused test compiles all 35k lines and links the world
once. `--filter` already locates the file by grep, so the script can pass
`--test <bin>` when the tests are split. Bar: mesh focused test build ≤ 6 s.

**Corrected by the model (2026-09-17, after the hand estimates above were
written):** `scripts/build-probe/predict.py` computes the weighted longest
path through the dependents of each probe crate from the measured warm
durations. It agrees with A1 and A4 (−3.8 s and −3.2 s on a 29.0 s weighted
chain; −7.0 together) and disagrees with A2 and A3: cutting tools→core (and
tools→store, which also reaches core) moves the chain by 0.1 s, and taking
corpus-engine off contracts by 0.4 s, because sovereign-api and sovereign-mesh
depend on core and tools directly, so the longest path survives the cut. Those
two remain worth doing for the cone *count* (tools leaves the core cone; the
largest crate leaves the contracts cone) but not for the wall clock, and the
campaign ladder ranks them accordingly.

What is not on the list: opt-level, codegen-units, `debug = 0` (already set),
linker (mold is in place), sccache. None of them shortens a serial chain.

## The loop this became

Operator direction 2026-09-17: run this as a campaign shaped like a while
loop over an objective measure — one architecture move per step, each step
banked only if the measure falls, until plateau. The campaign is
`quality/campaigns/build-latency.toml` (ruler, bars, move set, run ledger)
with its ladder in `.sovereign/features/build-latency/campaign.md`.

The measure (`scripts/build-probe/score.sh`): expected seconds from edit to
green scoped lint plus focused test, over the eleven probe files above,
weighted by each crate's 30-day commit count frozen at 2026-09-17. A second
number is the same weighting over `cargo build --workspace`. The instrument
is validated before use: an A/A pair on the unchanged tree sets the band a
move must clear (bar `bl-noise`, kill at 10%). A third instrument,
`scripts/build-probe/scope-drift.py`, counts the units a `-p <crate>` run
compiles that a `--workspace` run never does — the structural cause of D1 —
and must read 0 before the campaign closes.

Spiked, not landed: `cargo hakari` in a throwaway worktree wires a
`workspace-hack` crate into 59 members (70 files, +275 lines) and takes
scope drift from 316 to 41. The residual 41 are workspace-member features
that vary by scope — `kernel-types/wire-fixture`, `sovereign-tools/{atos,
dev-tools,treesitter}`, `commonwealth-rail-core/test-support`,
`sovereign-cli-shared`, `sovereign-enrichment-build`,
`sovereign-cli-llm/awareness` — the same list `scripts/lib/cargo-scope.sh`
keeps by hand today. Pinning those from the crates that need them is the
second build-graph rung and retires that list.

## Order

D3 and D2 are one line each and change every run; do them first. D1 is a
generated crate and a `cargo hakari` check in the pre-push gate; it removes
the only three-minute number here. A1 and A4 are the two large splits and pay
on every edit in the two hottest crates; A2 and A3 are small edge cuts that
shorten every chain. Re-run `scripts/build-probe/probe.sh` after each and
compare against the bars above; a lever that does not move its bar is
reverted (principle 7).
