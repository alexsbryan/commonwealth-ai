<!-- ledger -->

**five-programs-57 · 2026-09-25 · fp-110 (row rescoped) · director** — this commit
- Needed: fp-110's tests named "an in-process cw-rails KV router over a real `KvHost`". That puts `commonwealth_rails::kv` in sovereign-daemon's test build, which means a dev edge `sovereign-daemon -> commonwealth-rails`. That edge is svrn → cmnwlth, and no [[exception]] covers it, so the gate would read 55. The drift the fixture was meant to catch is still real: cw-rails encodes its door bodies by hand (`to_entry`, commonwealth-rails/src/kv.rs:576), while RailsKv decodes through `ReplicatedKvEntry`'s serde.
- Chose: the package's option 1, shaped by what the tree allows. The round-trip, current-thread and p50/p99 tests run against the stand-in door the worker built. One more daemon test serves a HAND-WRITTEN literal in `to_entry`'s shape and asserts that RailsKv decodes it, and that `KvSetBody` encodes the fields cw-rails' set door parses. cw-rails' own kv/tests already pin the same literal (kv/tests.rs:108-142). A plant on each side has to turn that side red.
- Because: both edges that could carry one shared fixture are closed. `commonwealth-rails -> sovereign-*` is a [[forbid]] row (ARCH_LAYERS.toml:676, the standalone lift), and I measured 55 violations with `sovereign-contracts` added as a cw-rails dev-dep. Crate-escaping `include_str!` is a boundary-gate rule (boundary_gate.rs:223). Admitting an [[exception]] is the operator's call and cuts against -54. A wire contract across a lift boundary is pinned by two tests of one literal. A change to either encoder alone goes red in that side's own crate. Only a coordinated change to both literals passes, and that is a deliberate wire change, not drift.

<!-- appendix -->

## five-programs-57 · 2026-09-25 — fp-110 pins the KV wire with the same literal on each side, not with a cross-program fixture

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp110-20260925.md. Reproduced at 5e2f30d08, clean tree.

- sovereign-daemon is in package `svrn` and commonwealth-rails is in `cmnwlth`. The gate counts dev edges (`dep closure incl. dev+build edges`), so the row's fixture would add a violation. The worker measured 54 before; I did not re-measure the daemon → cw-rails edge, because the package's reading of packages.rs matches the code.
- The alternative I tried first was to let cw-rails' own test decode its door output through `ReplicatedKvEntry`, adding `sovereign-contracts` as a cw-rails dev-dep. `sovereign-contracts` is a global [[package_leaf]], and commonwealth-transport and commonwealth-state already name it. `cargo xtask boundary-gate` then read **55**: `commonwealth-rails → sovereign-contracts: a dev dependency forbidden by a [[forbid]] rule (this binary is BUILT AND RUN outside the monorepo by scripts/cw-rails-lift.sh …)`. I reverted it, and the tree is clean.
- A shared fixture file (the seat note's shape) needs one crate to `include_str!` across its own boundary, which boundary_gate.rs rule 3b fails. Nor does a wire-fixture home exist that both crates may read. The only fixture dir in the rail family, commonwealth-rail-core/src/fixtures, holds rail ops, not the KV door, and exposing a KV fixture from rail-core would give that crate a wire it does not own (ARCH 12).
- cw-rails' side is already pinned. kv/tests.rs:108 posts a literal set body `{app_id,key,value:b64,origin}`, and :131-142 assert `value == b64(..)` and `origin == json!(origin)` on the scan and get bodies. The stand-in door (target/ralph/five-programs/probe.rs.txt) serialises `ReplicatedKvEntry` itself, so on its own it is tautological for drift. The added daemon test is what makes the pin two-sided.

What would falsify this: a drift between `to_entry` and `ReplicatedKvEntry` that reaches a running daemon while both literal tests stay green. That can only happen if one side's test literal differs from its encoder, which is exactly what the plants in fp-110's commit body demonstrate cannot happen. It would also be falsified if the operator rules that a cross-program real-door test is required regardless, which means an [[exception]] or an ungoverned host crate (package option 3, not surveyed) and a new row.

Side finding, not acted on (outside the row): the worker reports that the scoped `sovereign-lint.sh` resolves changes under `sovereign-daemon/src/rails_client*` to the crate `commonwealth-rails`. fp-110 uses `--full` as its LINT verdict. REVIEW-AFTER: whether scoped lint's crate resolution is wrong beyond this path.

</details>
