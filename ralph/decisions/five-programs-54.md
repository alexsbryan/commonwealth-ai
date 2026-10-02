<!-- ledger -->

**five-programs-54 · 2026-09-25 · local-only durability, condition 2's check, condition 1, the work-atlas dev edge · operator** — this commit
- Needed: the seat brought four open decisions with pros, cons and a recommendation for each. The first came with a new finding. five-programs-40 dropped local-only journaling because "no local-only row survives a restart today". That is false for `svrn portfolio` and `svrn newsworthy`: both open a durable SQLite `MeshStore::open` (sovereign-cli-llm portfolio_cmd/mod.rs:45-55, newsworthy_cmd.rs:73) over local-only namespaces (`portfolio-private`, `wikipedia-newsworthy:*`, commonwealth-rail-core/src/lib.rs:130-136). fp-87 moves them onto cw-rails' in-memory store, which rehydrates only journaled namespaces, so fp-87 would stop their data surviving a restart. fp-87's own restart test would catch it and halt.
- Chose: the seat's four recommendations, plus a standing direction. (1) Local-only rows become durable in cw-rails before fp-87, through REVIEW-mint-fp-local-only-durable (cap 3): journaled, never offered, rehydrated from this node's own journals, with the two `:` ids renamed inside fp-87's migration. (2) Finish condition 2 is restated: `EmbeddedDaemon` is constructed nowhere outside the sovereign-daemon crate. (3) five-programs-39's restatement of condition 1 is confirmed, and the handoff row runs the full suite and `lint --full` regardless of the loop counter. (4) The fp-84 dev edge (sovereign-mesh → sovereign-work-atlas) goes to Phase B with the work-atlas question. Standing direction: "a pure outcome of five programs with the minimal amount of lift". That means gate 0 with no standing boundary `[[exception]]` (fp-9's and fp-10's retire in Phase B, none added), reached by deleting or moving before building, reusing before minting, and the smallest host that serves each verb. Every mint states its lift before it mints.
- Because: operator's word. On (2), the literal check ("grep returns only the daemon's main") cannot be met: 373 of 377 non-comment hits are the daemon crate's own type and its uses, so the only zero is a rename, which would be a fake zero (ARCH 5). Measured 2026-09-25, construction outside the daemon crate is one site, the setup wizard (sovereign-cli-daemon/src/setup_cmd/terminal.rs:316). The desktop is already de-embedded: its remaining mentions are a stale log line (bootstrap.rs:195) and the census guard's needle string.

<!-- appendix -->

## five-programs-54 · 2026-09-25 — the seat's four recommendations; pure outcome, minimal lift

<details><summary>reasoning, evidence, package</summary>

Options as weighed in front of the operator:

- **(1) Local-only durability.** (A) Journal local-only rows in cw-rails (recommended). (B) Leave portfolio and newsworthy on their own SQLite files: cli-llm → commonwealth-state stays red and goes to Phase B, and fp-76's class stays unused. (C) Delete fp-76's class and accept the loss on restart, which is a silent user-data regression, so rejected. (A) also gives fp-76's class its users (principle 12). It makes daemon-side local-only rows survive restarts, which is an improvement and is named in the minted rows.
- **(2) Condition 2.** Restate the check, or keep the literal grep, whose only zero is a rename.
- **(3) Condition 1 (-39).** Confirmed. The cost, named: "done" for five-programs is a milestone of about 51 owned red edges, not liftability. That is Phase B's job.
- **(4) The fp-84 dev edge.** Stop counting dev edges, add an exception, or defer. Deferred, because the work atlas's fate is one product question with daemon → sovereign-work-atlas: charter §5 lists the atlas as deleted, while sessions call `work_in_flight`.

Also corrected here: Phase B inherits eight rows, not six. The app-registry mint (9f5146824) sent fp-47 to Phase B, and REVIEW-mint-fp-mesh-dial follows it through its dependencies. The handoff row and `HUMAN-phase-b` now list both.

Measurements behind the progress report given with this decision: boundary gate 103 (e8fee31a6, 2026-09-21), then 79 (09ff299b8, when this queue was minted), then 54 now. Per program: [bench] 0, [ingest] 1 (build.rs, which fp-105 closes), [cmnwlth] 3, [code] 5, [svrn] 45. The last full workspace run was at fp-68 (13,341/0, lint --full clean). The rows since then were checked by scoped lint and single-crate tests only, which is why the handoff now forces a full run.

</details>
