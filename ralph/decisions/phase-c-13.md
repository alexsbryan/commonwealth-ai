<!-- ledger -->

**phase-c-13 · 2026-10-02 · pc-cli-config-load-silent-default · director** — this commit
- Needed: the lane built the cli-mesh half (16efaf015, 0f61bc951) and stopped on the other: `client_daemon_base`/`internal_daemon_base` in sovereign-contracts setup_config.rs still default on a load error, with callers in about ten crates, several of them clap `default_value_t` fns that would become run-time resolution. Split, or authorize the lift in this row.
- Chose: split. This row closes on the mesh/ring verbs (the incident's: `mesh join`, `mesh status`, rotate, ring, guest) and merges now; `pc-cli-config-load-silent-default-base` takes the two contracts accessors and their callers, onto the `SetupConfig::load_present` this row landed, depending on this row.
- Because: the charter's "splitting when proofs differ (`<id>-<suffix>` stays in scope)", and scope.txt's header puts a split of a listed row in scope. The proofs differ: this half is proven by a sandboxed `mesh status` that refuses; the other needs a refusal at each clap default site across the CLI siblings and the in-process callers (core, pipeline, eval, enrichment-catalog, corpus-index), a different test and a different blast. Neither half's outcome is narrowed: together they are the row's letter.

<!-- appendix -->

## phase-c-13 · 2026-10-02 — split the client_daemon_base half off pc-cli-config-load-silent-default

<details><summary>reasoning, evidence, package</summary>

Reproduced in the lane worktree (commonwealth-ai-lanes/pc-cli-config-load-silent-default) at
f119a4312, sovereign-vulkan toolbox, host otherwise idle (the pool was halted):

- `scripts/sovereign-test.sh --package sovereign-cli-mesh`: exit 0, 195 pass 0 fail.
- PLANT, `mesh_cmd::dial_config` back to `load_present().ok().flatten()` (mesh_cmd.rs:710):
  exit 100, 193 pass 2 fail, `present_but_unparsable_config_refuses` and
  `present_but_invalid_config_refuses_naming_the_path` (`["mesh", "status"] exited 0`).
  Reverted with `git checkout --`, tree clean.
- setup_config.rs `internal_daemon_base` and `client_daemon_base` still end in
  `.unwrap_or_else(|_| default_*_port())`. `git grep -w client_daemon_base -- '*.rs'` names 59 lines
  outside the definition (comments included) in corpus-index, xtask, sovereign-cli-{base,bench,daemon,dev,llm},
  contracts, core, enrichment-catalog, eval, pipeline; `internal_daemon_base` 9 lines in 4 crates. The
  package's "31 + 4 callers in 9 crates" undercounts slightly; the conclusion (well over the lift) holds.

The lane's decision phase-c-11 collided with the base's phase-c-11 (85fed82d1) and was renumbered
phase-c-12 before the merge, as the pool would (`ralph-decisions.py renumber`).

What would falsify this: the -base row finding that the two accessors cannot refuse without changing a
clap default's documented value (an end-user-observable delta the row does not state), which goes back
to the operator; or a mesh/ring verb found still dialling through `client_daemon_base`, which reopens
this row rather than the split.

</details>
