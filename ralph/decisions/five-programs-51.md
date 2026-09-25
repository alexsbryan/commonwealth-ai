<!-- ledger -->

**five-programs-51 · 2026-09-24 · fp-98 · director** — this commit
- Needed: fp-98's code landed (9e6c557c0) but LAYER exited 1: fan-in of `sovereign-contracts` grew 41 → 42, and the worker may not raise a ratchet the row does not name.
- Chose: accept the growth explicitly, `quality/baselines/fan_in.tsv` `sovereign-contracts` 41 → 42, one line; mark fp-98 `[x]`.
- Because: the new dependent is sovereign-cli-base, and the edge is the row's own (its `allow` list names sovereign-contracts, five-programs-38). The three reads it carries (rebrand, setup_config::client_daemon_base, guest_link) live in no other leaf, and cli-shared keeps its own edge for repo/models/mcp_client. Precedent: five-programs-41, fe309bfb5.

<!-- appendix -->

## five-programs-51 · 2026-09-24 — sovereign-contracts fan-in 41 → 42, accepted for fp-98's named edge

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp98-20260924.md. Reproduced at 9e6c557c0.

- `scripts/ralph-check.sh layer` (toolbox) before the edit: exit=1, "layer-gate FAILED (0 layer violations, 1 fan-in)". After: "fan-in within caps", exit 0.
- `grep -rn sovereign_contracts sovereign/crates/sovereign-cli-base/src`: dirs.rs:26,33 (rebrand), urls.rs:31 (setup_config::client_daemon_base), guest_link.rs:54 (re-export). The same grep over sovereign-cli-shared/src still lists repo.rs, models.rs, mcp_client.rs, so no dependent drops in exchange.
- `scripts/ralph-check.sh boundary`: 55, unchanged, as the row expects.

REVIEW-AFTER: the fan-in cap is a ratchet the charter does not list; the edge itself was decided by the operator in five-programs-38, and the cap follows it mechanically, as in five-programs-41.

Falsified if the three contracts items could be reached through an existing leaf cli-base is already allowed (then the edge, not the cap, was wrong), or if cli-shared's fat-half move removes its last contracts reference without the cap being tightened back.

</details>
