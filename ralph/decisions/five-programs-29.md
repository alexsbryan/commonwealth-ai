<!-- ledger -->

**five-programs-29 · 2026-09-24 · fp-24 · director** — this commit
- Needed: fp-24's worker halted: the row says to repoint cli-daemon's 2 join-key refs and drop sovereign-mesh, but that edge is already closed.
- Chose: mark fp-24 `[x]` as subsumed by fp-52 (bdd22b846), with no closure counted. Nothing depends on it, so nothing is re-pointed.
- Because: reproduced at 33952f8ef. cli-daemon's Cargo.toml has `mesh-join-vocab` (line 38) and no `sovereign-mesh` dep; `git grep sovereign_mesh` in the crate finds nothing; TSV line 63 records the pair `CLOSED fp-52`; boundary-gate 58, and cli-daemon's only line is `→ sovereign-inference`.

<!-- appendix -->

## five-programs-29 · 2026-09-24 — fp-24 is subsumed by fp-52

<details><summary>reasoning, evidence, package</summary>

fp-52 was minted as "fp-24's chain". When it landed it made the leaf and did fp-24's repoint in the same move (STATE.md fp-52 row: "cli-daemon repoints its 2 deep_link refs, drops sovereign-mesh … boundary-gate 69 → 68"). Nobody closed the parent row. fp-24's check, "gate drops 1", cannot pass because the drop was already banked at 68. The closure goes to fp-52 and fp-24 gets none.

The package asked whether dependents should move to fp-52. `grep "depends \[[^]]*fp-24"` over STATE.md finds none. The other mentions are the old "fp-24 bench dial" label in the 2026-09-22 open-questions text and fp-18. That was a separate authoring-harness idea that was never queued under this id, and five-programs-14 (fp-57) superseded it. Those lines are history and are not rewritten.

Falsifier: a `sovereign-mesh` dependency or a `sovereign_mesh::` path turns up anywhere in sovereign-cli-daemon (cfg-gated or dev-deps included), or boundary-gate lists `sovereign-cli-daemon → sovereign-mesh`.

Gate at decision: boundary-gate 58 violation(s).

</details>
