<!-- ledger -->

**phase-b-56 · 2026-09-29 · pb-bench-dials-wire · director** — this commit
- Needed: phase-b-55's twin collapse (agent-tools' `SamplingOverrides` re-exports the one definition) was refused by LAYER when that definition lived in sovereign-contracts: fan-in 43 → 44.
- Chose: move the one definition to oicp-types (completion.rs, beside `CompletionRequest`). sovereign-contracts re-exports it at `types::SamplingOverrides` and agent-tools at `role::SamplingOverrides`. Definitions 2 → 1, no path a caller names moves, no baseline re-pinned.
- Because:
  - Principle 8: one schema, one definition; keeping the twin was never on the table (phase-b-55).
  - Charter test, "the next developer who wants THIS program": agent-tools has no workspace dependency but workspace-hack. Accepting the fan-in growth would give it the contracts closure (reqwest, ed25519, rcgen) for a three-field struct. The fan-in gate names the fix itself: "depend on a narrower crate". oicp-types is that crate, and it is already in the bench package's leaf_budget.
  - §12 3a ladder, first match: two programs use it, so not "one program". It is sampling vocabulary of the inference wire, and `CompletionRequest` already carries the same knobs (temperature, top_p, max_tokens), so "federation wire → oicp-types" matches before "svrn serving contract".
  - Boundary gate: 20 (unchanged). LAYER green.

<!-- appendix -->

## phase-b-56 · 2026-09-29 — SamplingOverrides lives in oicp-types, re-exported by contracts and agent-tools

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md (b2937666a) offered (1) accept sovereign-contracts fan-in 43 → 44, (2) home the type in oicp-types, (3) keep the twin. Reproduced before deciding: `struct SamplingOverrides` was defined at sovereign-contracts types/turn.rs:624 and sovereign-agent-tools role/profile.rs:29 with identical fields; agent-tools/Cargo.toml's only in-repo dependency was workspace-hack; quality/baselines/fan_in.tsv pins sovereign-contracts at 43 and has no oicp-types row; ARCH_LAYERS.toml's bench package lists oicp-types in `leaf_budget`.

Option 1 is a baseline raise on a god-crate's fan-in (the charter leaves re-pins to the operator) and fails the take-alone test. Option 3 contradicts principle 8. Option 2 is a landing decision the charter delegates (§12 3a ladder), so the director made it.

Trial, applied and kept (not reverted, since it is the change):
- LAYER: `✓ every crate assigned, every edge points down or sideways, ... fan-in within caps`.
- BOUNDARY: `boundary-gate FAILED (20 violation(s))`, unchanged from 70f36a999.
- LINT (workspace, --all-targets): errors 0; arch-gate clean.
- TEST oicp-types, sovereign-agent-tools, sovereign-contracts, sovereign-tdd: exit 0 each. sovereign-daemon `sampling_pins`: pass 2, fail 0.
- concept-gate: could-not-judge (uncommitted rs files not yet indexed); the noun count falls 2 → 1 by construction.

One behaviour delta, additive: agent-tools' type gains `PartialEq`, which the contracts copy already derived. Serde form is byte-identical (same attributes on the same fields).

Falsified if: a later LAYER or BOUNDARY run names agent-tools → oicp-types, or an OICP consumer outside svrn/bench is shown to need a different sampling shape, in which case the type belongs back in contracts and agent-tools keeps a conversion, not a twin.

</details>
