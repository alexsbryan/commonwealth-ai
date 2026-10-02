# NEEDS_HUMAN — fp-12 (the remaining daemon embeds: grants, code-next-edit, tdd, pods, meshapp, meshapp-registry, gliner)

## (a) Unit

`- [ ] fp-12 — depends [fp-10] — DIAL grants/queue + meshapp + registry + code-next-edit + tdd + pods + gliner (§12 decision 2, …) … check: scoped lint 0 per pair; gate delta recorded per commit (7 gate lines …; ceiling −4 without an operator answer — the last three need one)`

fp-10 is `[x]`, so the row is dep-ready. The row already says three pairs halt
for the operator: meshapp, meshapp-registry and gliner. It expects the other
four to close without an answer (ceiling −4). The premise check fails for all
four too, so this session made no edit and left STATE.md untouched. Every pair
runs into the wall fp-10 and fp-11 hit: nothing exists for the daemon to dial.

## (b) What I ran, at ee38dd56e

`scripts/ralph-check.sh boundary` printed `boundary-gate FAILED (56 violation(s))`.
All seven edges are still red (boundary.log:74,77,78,80,81,84,86).

`git grep -c <crate>:: -- sovereign/crates/sovereign-daemon/src` gives:

- `sovereign_grants`: 57 refs across 22 files, plus 19 in tests/.
- `code_next_edit`: 20 refs in routes_edit_predictions{,/outcome,/wire}.rs, plus 5 re-exports at lib.rs:212-216.
- `sovereign_tdd`: 4 refs, all in solve_http.rs:37-41.
- `sovereign_pods`: 10 refs, all in worker.rs.

Per pair, the four the row counted as buildable:

1. **sovereign-grants (cmnwlth package, ARCH_LAYERS.toml:1306).** D2's host is
   cw-rails, but `[[forbid]] commonwealth-rails → sovereign-*`
   (ARCH_LAYERS.toml:676-679) has no except, so cw-rails can never serve a
   grants verb. No other cmnwlth process serves the corpus-queue routes. TSV:7's
   `decision_needed` cell is still open: "Does the shard/grant/queue decider move
   to its owning process and the daemon dial it, or is it ported into a leaf?"
   This is the same wall the row already names for meshapp. It is not the
   five-programs-8 except-widening, because the target is a sovereign-* crate,
   not a commonwealth-* one.
2. **code-next-edit ([code] package, ARCH_LAYERS.toml:1340).** TSV:19 names "the
   code program serving the edit-prediction route". No such server exists. That
   is fp-11's finding (ctl/NEEDS_HUMAN.resolved-fp11-20260924.md), which is
   parked on HUMAN-fp11-code-mcp-host. There is a second coupling. The route's
   model lane (`code_next_edit::next_edit_model`, routes_edit_predictions.rs:7-10)
   consults the resident FIM slot, which is the inference that fp-10's answer (b)
   keeps in the daemon. A code-program server would have to dial the daemon back
   for that lane.
3. **sovereign-tdd ([bench] package, ARCH_LAYERS.toml:1377).** TSV:28 names "the
   bench program serving solve". The bench package's own charter
   (ARCH_LAYERS.toml, `[[package]] name = "bench"`) says "Serves no wire and
   dials a URL", and its leaf_budget exists so bench links nothing it measures.
   A bench server for solve reverses that charter. The route also feeds the MCP
   `solve`/`solve_status`/`solve_cancel` tools on every agent's surface
   (solve_tools.rs), so a dial that reports absence would change the default MCP
   surface. PROMPT §7 sends that to §6.
4. **sovereign-pods (cmnwlth package, ARCH_LAYERS.toml:1303).** TSV:23 says
   "exec: a pods worker binary owning worker mode", with decision_needed = none.
   It can be built, but not as a behaviour-preserving single row:
   - Worker mode runs inside the daemon's process setup today
     (bin/sovereign-daemon.rs:78-102: rebrand migration, the resident panic
     hook DAEMON_RESILIENCE P0.4, daemon tracing filter, 8 MiB runtime). A
     sovereign-pods `[[bin]]` cannot name `sovereign_core::rebrand` or the
     daemon's `install_panic_hook` without a new cmnwlth→svrn red edge. So the
     worker process either loses its panic hook, which is a behaviour change,
     or twins it, which violates §10.6.
   - `worker_subprocess_runner.rs:471-474` spawns the child as
     `current_exe() daemon run --config …`. Under a pods binary, current_exe is
     the pod worker itself, which does not serve `daemon run`. The runner would
     need an explicit sibling path, and that means a second sibling-resolution
     decider next to cli-daemon's `daemon_bin`.
   - The pod image builds only `--bin sovereign-cli` (Containerfile:191; the
     cuda image adds cli-daemon and cli-llm at :339). The new binary has to be
     added to both images and to entrypoint.sh:114. That is a pod contract I
     cannot exercise on this host (a Vast pod).
   - `sovereign-cli-llm → sovereign-pods` is also red (boundary.log:57), so the
     pods crate's own package placement is still open. The owner-side controller
     sits in a svrn crate.

The three pairs the row already marks as halting, with nothing new to add:
meshapp and meshapp-registry (cw-rails is forbidden sovereign-* and
corpus-engine*), and gliner (no ingest process serves; fw-4's `[ingest]` bin
has not landed; `git grep -ln 'name = "ingest"' -- '*.toml'` finds only the
ARCH_LAYERS `[[package]]` and a workflow example, no `[[bin]]`).

## (c) What the operator must decide

1. **The fp-10 pattern, applied pair by pair.** Should each of the seven edges
   get an `[[exception]] package = "svrn"` row (sovereign-daemon → X), each
   with its reason, until its owning program's server is minted as its own
   reviewed program? The reasons would be: grants — the daemon is the only
   process serving the corpus-queue routes; code-next-edit — the model lane uses
   the daemon's resident FIM slot and no code server exists (tie to
   HUMAN-fp11); tdd — bench serves no wire by charter, so solve stays with its
   MCP host; pods — worker mode shares the daemon's process setup; meshapp /
   registry / gliner — as the row already states. This closes 56 → 49 with no
   code and no behaviour change. The director recommends this for grants,
   code-next-edit and tdd, where every other arm reverses a charter line
   (cw-rails forbid, bench "serves no wire", fp-11's missing server).
2. **pods only: the exec split or the exception.** Take the exec split if you
   want TSV:23 honoured literally. Its sub-decisions: (a) the pod worker drops
   the resident panic hook, or the hook moves to a leaf both binaries can name
   (sovereign-contracts is fs-free, so that is a new home); (b) the subprocess
   runner is given the sibling daemon path, and whether cli-daemon's
   `daemon_bin` resolver moves to a leaf so there is one decider; (c) both
   Containerfiles and entrypoint.sh:114 ship and exec the new binary. That is
   4 rows, more than one. Otherwise take the exception as in 1.
3. **meshapp / meshapp-registry:** rename or re-home the crates under
   commonwealth-*, name another cmnwlth host, or grandfather them (the row's
   own question, unchanged).
4. **gliner:** which process serves ingest's model install (the row's own
   question, unchanged).

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md (add a HUMAN-fp12
row carrying the answer, or rewrite fp-12 to it), then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
