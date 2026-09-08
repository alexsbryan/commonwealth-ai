# runs/ei5c-gates — ei-5c-seed-race, the mechanical gates as one unit

Branch `ei-5c`, worktree `/home/alexbryan/dev/ei5c-wt`. No GPU, no model, no
daemon call: this is cargo and two shell wrappers. Safe to interleave with a
lane on the box only in the sense that it takes the build lock — it does not.
Runs under the seat's cgroup wrapper like everything else.

| leg | what | why it is in this unit |
|---|---|---|
| `build` | `cargo build --bins --features sovereign-cli/dev-tools,corpus-engine/treesitter` | The dependency. `concept-gate` relays through the `sovereign-cli-dev` sibling and reports COULD-NOT-JUDGE (exit 3) against a stale one — which is what it did in this worktree. Also warms legs 3 and 4. |
| `concept-gate` | `cargo xtask concept-gate` | Judged only after leg 1. Exit 3 is could-not-judge, not a finding. |
| `lint` | `scripts/sovereign-lint.sh --human --full` | The compile gate, whole workspace, `--all-targets`. |
| `test` | `scripts/sovereign-test.sh --human --package …` | corpus-engine, corpus-engine-vocab, sovereign-core, corpus-mcp, sovereign-cli-llm — the crates this order changed and the ones carrying its guards. |

Forecast: 20-35 min cold, ~8 warm. Memory: the build is the peak; 36G cap.

Outputs under `runs/ei5c-gates/out/`: `<leg>.txt` + `<leg>.rc` per leg,
`box-before.txt` / `box-after.txt`, `preflight.txt`, `log.txt`, and a terminal
`DONE` marker written even on SIGTERM — a killed run is a verdict, not a
silence.

Read the rc files, not the tails: `sovereign-test.sh` exits 4 on a zero-test
run and 5 on an unattributable one, and both look green in a summary line.
