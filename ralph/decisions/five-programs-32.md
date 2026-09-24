<!-- ledger -->

**five-programs-32 · 2026-09-24 · fp-12 · director** — this commit
- Needed: fp-12 halted because none of its seven daemon embeds has a process to dial. The row expected four of them (grants, code-next-edit, tdd, pods) to close without an operator answer.
- Chose: park fp-12 on a new operator row, HUMAN-fp12-daemon-embeds (the fp-10, fp-11 and fp-25 shape). Recommendation: `[[exception]] package = "svrn"` for grants, code-next-edit and tdd, and for pods for now. meshapp, meshapp-registry and gliner keep the questions the row already asked.
- Because: every closing arm is an exception row, a new serving program, or a reversal of a charter line: the cw-rails forbid for grants, fp-11's missing code server for code-next-edit, bench's "serves no wire" for tdd. pods is a ~4-row exec split that touches the pod image contract. Adding exceptions and minting new servers are both the operator's call under the charter. Measured at boundary-gate 56.

<!-- appendix -->

## five-programs-32 · 2026-09-24 — fp-12's four "buildable" pairs have nothing to dial either: park on HUMAN-fp12-daemon-embeds

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp12-20260924.md. Reproduced at ee38dd56e:

- `scripts/ralph-check.sh boundary` reports 56 violations. All seven edges are listed at boundary.log:74,77,78,80,81,84,86.
- `git grep -c <crate> -- sovereign/crates/sovereign-daemon/src` gives these counts: sovereign_grants 60 in 22 files, code_next_edit 20 in 4, sovereign_tdd 4 in 1 (solve_http.rs), sovereign_pods 10 in 1, sovereign_meshapp:: 35 in 2, sovereign_meshapp_registry 7 in 5, sovereign_gliner 12 in 3.
- quality/ARCH_LAYERS.toml:676-679 is `[[forbid]] commonwealth-rails → sovereign-*` with no except. D2's host therefore cannot serve grants, meshapp or the registry.
- ARCH_LAYERS `[[package]] name = "bench"` (:1344-1347) reads "Serves no wire and dials a URL". A bench solve server would reverse that.
- bin/sovereign-daemon.rs:78-102 runs the rebrand migration, the resident panic hook, the tracing filter and the 8 MiB runtime before worker mode dispatches. sovereign-pods/src/worker_subprocess_runner.rs:473 spawns `current_exe()`. sovereign/container/Containerfile:191 builds only `--bin sovereign-cli`.

The row text "ceiling −4 without an operator answer" was the false premise, and it is corrected in the fp-12 row. No gate edge moves without either an exception or new capability. The per-pair dial remains what the row prescribes once the operator names a host.

Falsifier: a cmnwlth or code process turns out to already serve the corpus-queue, edit-prediction or solve routes, or can do so without linking a forbidden crate. In that case that pair is a worker row again and this park was wrong for it. Likewise, if the pods exec split proves to be a single behaviour-preserving commit (hook and sibling resolution already in a leaf), pods should not have waited.

Gate at decision: boundary-gate 56 violation(s).

</details>
