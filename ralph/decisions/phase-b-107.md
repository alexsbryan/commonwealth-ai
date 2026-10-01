<!-- ledger -->

**phase-b-107 · 2026-10-01 · pb-distribution-ship-gate · director** — this commit
- Needed: the ship gate's Tier 2 needs the deployed node on C, and the worker's chained `sovereign daemon stop && sovereign daemon start` was refused by its permission layer; P3 idle had only a debug screen against release-profile bars.
- Chose:
  - The director ran the restart under phase-b-34's standing grant, as two separate calls. The node is now sovereign-stock pid 2214681 (target/debug, built 14:20, after C at 14:06), started by sovereign.service with the env the old pid carried (decision log decisions-EXP.jsonl, peer inference off, the same RUST_LOG); `/health` ok; cw-rails 1090860 untouched.
  - The dispatcher had been relinked without dev-tools at 14:23 (its own warning; `tools list` refused). Rebuilt with `-p sovereign-cli -p corpus-engine --features sovereign-cli/dev-tools,corpus-engine/treesitter`; `tools list` answers.
  - P3 idle is read by re-running e098d2112's and 8dc4ff1f6's instruments at C as minted, release, into their private target/ralph/idle-target. The debug screen stays beside, gating nothing.
  - The absent-embed coverage gap is a finding, not this bar: phase-c row pc-idle-embed-boot (Phase B is frozen, phase-b-106).
- Because: the grant covers the restart (phase-b-34), and a bar is read only by the instrument and profile that minted it (principle 7); re-scoping it after the debug data would be tuning it.

<!-- appendix -->

## phase-b-107 · 2026-10-01 — ship gate: restart done by the director; P3 read on its minting release instruments; embed gap to phase-c

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md, four items.

1. Restart. Verified: old pid 2957503 ran `target/debug/sovereign-stock (deleted)`, started 01:54, launched by `toolbox run ... sovereign-cli daemon run` with an experiment env. The decision log showed 5 outcomes in the last 10 min, all light session-hook traffic. `daemon stop` then `daemon start` (two calls) went through sovereign.service; the new pid's /proc environ carries the same SOVEREIGN_* and RUST_LOG values. The worker should not restart again unless the node died.
2. P3 idle. e098d2112 built `cargo build --release -p sovereign-stock` with CARGO_TARGET_DIR=target/ralph/idle-target; 8dc4ff1f6 a release cw-rails. Both binaries are still in that private target, so the rebuild at C is incremental and does not thrash the debug target. AGENTS.md permits --release where a path genuinely requires it; a release-minted bar is that path.
3. Embed finding. vendor/llama-cpp-4/src/model.rs:1880 is `debug_assert!(Path::new(path).exists(), ...)`, confirmed. The release instrument's absent embed never exercises the boot-time embed work (~1,600 calls in 3.5 min, debug). Moved to phase-c as pc-idle-embed-boot with its own pre-registered reading.
4. Release note: already owed by the row; unchanged.

Falsified if: the release idle readings at C exceed 2.004% / 2% (then NEEDS_HUMAN with the numbers, per the charter, never a re-tune); or the restarted node is found to lack a setting the old launch carried that a Tier 2 reading depends on.

</details>
