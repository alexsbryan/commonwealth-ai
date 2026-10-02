<!-- ledger -->

**five-programs-41 · 2026-09-24 · fp-77 · director** — this commit
- Needed: fp-77's code landed (76742239f) but LAYER exited 1: fan-in of `commonwealth-state` grew 4 → 5, and the worker may not raise a ratchet the row does not name.
- Chose: accept the growth explicitly, `quality/baselines/fan_in.tsv` `commonwealth-state` 4 → 5, one line; mark fp-77 `[x]`.
- Because: the new dependent is the row's own step (1) edge (five-programs-40), taken with the closure the operator's pure-Rust answer demands (+1 first-party, 0 third-party, no sqlite). No narrower crate can carry the store, and fp-87 removes two dependents, leaving 3. Precedent: fe309bfb5 (contracts 40 → 41, accepted explicitly for a row-named edge).

<!-- appendix -->

## five-programs-41 · 2026-09-24 — commonwealth-state fan-in 4 → 5, accepted for fp-77's named edge

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp77b-20260924.md. The director reproduced it at 06c889d34.

- `scripts/ralph-check.sh layer` before the edit: exit=1, "layer-gate FAILED (0 layer violations, 1 fan-in)".
- `cargo tree -i commonwealth-state -e normal,build --depth 1 --workspace`: commonwealth-rails, sovereign-cli-llm, sovereign-daemon, sovereign-grants, sovereign-mesh — five, the new one is cw-rails. fp-87 closes sovereign-daemon → commonwealth-state and sovereign-cli-llm → commonwealth-state, so the cap can be tightened to 3 there (`layer-gate --tighten`).
- No narrower crate: `commonwealth-rail → commonwealth-state` is forbidden by name and `commonwealth-work` does not depend on the store, so neither of cw-rails' existing deps can re-export it.
- After the edit: layer-gate "fan-in within caps", exit 0.

The charter reserves `[[exception]]` rows and package_leaf widening beyond what a queue row names; this is neither, but it is also not named outright, hence REVIEW-AFTER: the fan-in cap is a ratchet the charter does not list. The edge itself was decided in five-programs-40; the cap follows it mechanically.

Falsified if a narrower home for `MeshStore` + `rail_kv` exists that cw-rails could depend on without commonwealth-state (then the edge, not the cap, was wrong), or if fp-87 lands without the fan-in falling back to 3.

</details>
