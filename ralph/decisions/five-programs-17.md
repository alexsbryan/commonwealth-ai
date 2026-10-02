<!-- ledger -->

**five-programs-17 · 2026-09-24 · REVIEW-mint-fp-core-dial (residue over the mint cap) · director** — this commit
- Needed: the core-dial mint halted (ctl/NEEDS_HUMAN.md). Its census of the daemon's commonwealth_core use asked for at least 13 rows against a cap of 8, and it named three forks: a sequencing contradiction with finish condition 2, leaf homes for the unhomed vocabulary, and whether to split the mint.
- Chose: re-sequence the ONE row, no new row. It now depends on the open D2 dial rows (fp-7, fp-8, fp-9, fp-10, fp-12, fp-42, fp-45, REVIEW-mint-fp-mesh-dial) and mints one row per CLASS (repoint / move-by-3a-rung / dial / test tree) instead of one per item. No code changed. Boundary gate unchanged at 63 (five-programs-16's count; nothing here touches an edge).
- Because: the false premise was the row's `depends [fp-0, fp-6]`. Most of the 13 are the work of dial rows already queued, so measuring now double-mints it. Fork 1 is decided by §12 D2 (the daemon dials membership), and finish condition 2 is EmbeddedDaemon construction outside the daemon main, not the daemon holding a Mesh. Fork 2 is decided by §12 3a's ladder, which the minted rows cite per name.

<!-- appendix -->

## five-programs-17 · 2026-09-24 — re-sequence the core-dial mint behind the dial rows; homes by the 3a ladder

<details><summary>reasoning, evidence, package</summary>

Reproduced by the director at 300478238. `grep -rn commonwealth_core sovereign/crates/sovereign-daemon --include=*.rs` returns 433 refs across 123 files, which matches the package. In non-test src, the `commonwealth_core::mesh` uses concentrate in daemon.rs (13), state.rs (6), mesh_admin.rs, gossip.rs, internal_principal.rs (5 each). `AppState::new(self_node_id, mesh: Mesh)` is at state.rs:738, with nine sibling constructors down to :945.

Fork 1 (sequencing). The STATE.md note defines finish condition 2 as "desktop attach surface, mesh test tree, the wizard seam". FIVE_PROGRAMS §11 step 10 says the same: construction sites outside the svrn daemon binary's main. The daemon holding cmnwlth's membership model in-process is §12 D2's "component holding another's lifecycle", and D2 already chose the dial. So arm (a) is the decided one. The dial rows fp-6..fp-12, fp-42 and the mesh-dial mint are how the queue works it down. Pulling their work into this row is what inflated the count.

Fork 2 (homes). §12 3a is operator-approved and ends with "first match wins". Names that already have a leaf home (NodeId/NodePubkey in kernel-types, OriginKind in oicp-types, `oicp` = `pub use oicp_types`) are a pure repoint. For the rest, each minted row cites the rung. A name that fails every rung is "not vocabulary: dial, port trait, or split — never a leaf". That is Clock/unix_now_* (svrn already has sovereign-time) and constant_time_eq. None of this widens a leaf's dependency budget, so none of it reserves an operator call.

Fork 3 (split). Declined. A second mint row is the scope the charter warns against. The package itself shows the DTO half would close zero edges before the dial half lands (delta 0). Deferring the whole row wastes nothing.

Falsified if, after the dial rows land, the residue still needs more than 8 class rows (then the mint halts again with a real count), or if any of the named dial rows turns out not to touch the commonwealth_core sites attributed to it (then that site's class belongs in (iii) here, and the attribution in the row text is corrected).

</details>
