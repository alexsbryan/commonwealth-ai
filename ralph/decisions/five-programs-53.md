<!-- ledger -->

**five-programs-53 · 2026-09-24 · HUMAN-fp94-grants-seam + the state chain's size · operator** — this commit
- Needed: fp-94 waits on HUMAN-fp94-grants-seam. sovereign-grants holds Fabric's concrete `MeshStore` and `ContributionEmitter`, and after fp-88 the daemon holds neither. The seat also asked, from the fp-80 escalation (ctl/NEEDS_HUMAN.resolved-fp80b-20260924.md), whether the state chain should finish at 21 rows.
- Chose: (c), the director's recommendation. sovereign-contracts joins the except list of grants' `[[forbid]] sovereign-grants → sovereign-*`, and only that crate. Grants takes the existing `Arc<dyn ReplicatedKv>` for its KV get/set/scan, and its one ledger write goes through a fact port in the `LedgerEmitter` shape, which the daemon implements over `ContributionLedgerPort` as `DaemonLedger` does. The contracts fan-in rise this edge causes (42 → 43) is accepted with this answer. On size: finish the chain.
- Because: operator's word. The seat first put a different option to the operator: move the ledger port traits into commonwealth-state. The operator chose it, and the seat then found it left grants' sync KV half (shard_manager.rs:185,196) with no home except the same forbid exception or a new typed port. Re-asked with that correction, the operator chose (c). The [cmnwlth] package already carries sovereign-contracts through sovereign-mesh, so the lift closure does not change.

<!-- appendix -->

## five-programs-53 · 2026-09-24 — grants takes ReplicatedKv and a fact port

<details><summary>reasoning, evidence, package</summary>

(c)'s cost as the operator saw it: one except entry; grants alone gains sovereign-contracts in its closure; one small fact trait in contracts using kernel-types `NodeId` and primitive arguments (the `LedgerEmitter` precedent, sovereign-contracts/src/venue_host.rs:19-23). No second KV shape and no ledger vocabulary moved into the leaf. The fan-in rise is pre-accepted here so that fp-94 does not halt on the layer ratchet the way fp-77 and fp-98 did. fp-94 edits quality/baselines/fan_in.tsv's one `sovereign-contracts` line and cites this decision, the precedent being b784aac79.

The seat's retracted option, recorded because the operator first answered it: move the five ledger port traits into commonwealth-state (the ledger's owner, which grants already depends on) and re-export them from sovereign-mesh. It missed grants' KV get/set of the corpus-engine handoff keys.

The chain-size answer is moot in practice. fp-80 through fp-97 ran while the question was open. What remains is fp-94, fp-88, fp-83 and fp-87.

</details>
