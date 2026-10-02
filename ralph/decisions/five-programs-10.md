<!-- ledger -->

**five-programs-10 · 2026-09-24 · fw-2 (the ports wave) + fp-13 · director** — this commit
- Needed: fw-2 halted pre-edit (ctl/NEEDS_HUMAN.md): the port shape cannot move any of its five pairs off the gate, and fp-13 (`depends [fw-2]`) repeats the same shape, so striking fw-2 alone would have served the same false premise one row later.
- Chose: STRIKE fw-2 and fp-13 (`[x]`, no code, delta 0). The four real edges (meshapp, meshapp-registry, gliner, watchers) go to fp-12's existing DIAL row with each pair's host fact written in. Watchers leaves the queue for the operator (TSV:12 `decision_needed` is live). meshapp, meshapp-registry and gliner stay on fp-12 but are marked halt-for-operator when reached. runtime-commission is dropped because it is intra-svrn since fp-4 and has no gate line. The §12 Ports class row now says the port is vacuous for daemon pairs.
- Because: the smaller step over the larger, and "a false row premise is the director's" (charter). The census holds at 62. The dial is §12 D2's own answer; where D2's named host (cw-rails) is forbidden by design, it is the operator's call, not a widening the director makes.

<!-- appendix -->

## five-programs-10 · 2026-09-24 — the ports wave is struck; its edges are dials or operator questions

<details><summary>reasoning, evidence, package</summary>

Reproduced before deciding (director, toolbox host, tree clean at 0aacc0818):

- `cargo xtask boundary-gate` from corpus-engine/ → `boundary-gate FAILED (62 violation(s))`, EXIT=1. Red: `sovereign-daemon → {sovereign-meshapp-registry, sovereign-gliner, sovereign-meshapp, corpus-engine-watchers}`. No `sovereign-daemon → sovereign-runtime-recipe` line.
- `sovereign-cli-daemon/Cargo.toml:46` depends on `sovereign-daemon`. ARCH_LAYERS places meshapp/meshapp-registry in cmnwlth, gliner in ingest, watchers in code.
- TSV rows 12/13/14/22/24: all `fix_shape = port`. Only row 12 (watchers) has a non-empty `decision_needed`: "Does the daemon own the watcher runtime (edge grandfathered), or does code own it and the daemon dial?"
- NOT in the worker's package: `[[forbid]] commonwealth-rails → sovereign-*` (quality/ARCH_LAYERS.toml:676-679) has no except list. Its reason is that any sovereign-* edge makes the out-of-monorepo lift impossible. `commonwealth-rails → corpus-engine*` is forbidden too, and `sovereign-meshapp → corpus-engine` is red. So D2's host cannot take the meshapp crates by the five-programs-8 convention, which widened a commonwealth-* except at +1 crate. That is a design-intent row, and re-homing, a different host, or grandfathering is the operator's choice.

Why the port shape is delta 0: the gate counts Cargo.toml edges out of the package closure. Trait in contracts + impl in owner + injection still needs some crate to construct the impl, and every host of the daemon is [svrn]. The best case moves the edge from sovereign-daemon to sovereign-cli-daemon. This is the third time the finding has come up (fp-32, recipe-author, now fw-2), which is why the §12 class row now carries it.

Readiness after the rewrite (scripts/ralph.py Queue): `current()` = fw-3. fp-12 stays `[ ] depends [fp-10]`.

Refused alternatives:
- Land fw-2 as a port-only hygiene wave: buildable, but PROMPT.md's net-decreasing rule counts a delta-0 row as refused, and it adds a trait per pair that no gate or behaviour asks for (a new abstraction, which the charter's size rule forbids).
- Widen cw-rails' sovereign-* forbid for sovereign-meshapp*: it contradicts the row's stated reason (liftability), and it drags corpus-engine into the lifted binary. Operator reserve.
- Hold fw-2 `[~]` with the package open: `current()` returns ACTIVE rows regardless of deps (ralph.py:463), so the queue would stall on a row with nothing buildable.

Falsified if: a crate outside [svrn] comes to host the daemon's composition (then a port could move the edge and fp-13's shape revives), or fw-4's cli-llm split lands an [ingest] bin that can serve gliner's model install (then fp-12's gliner pair is buildable without the operator).

</details>
