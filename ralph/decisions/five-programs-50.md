<!-- ledger -->

**five-programs-50 · 2026-09-24 · fp-86 · director** — this commit
- Needed: fp-86 flipped ten of its eleven files (2a9fb23c3, TEST(sovereign-daemon) 1277 pass / 0 fail). The eleventh, `internal_gate_e2e.rs`, still has two hits at :416 and :436. Both are the `Arc<MeshStore>` argument to `sovereign_grants::ShardManager::new` (shard_manager.rs:73), which the test drives as a real shard-pull client. No daemon seed reaches those lines, so the row's bar of 0 over eleven files could not be met before fp-94.
- Chose: the package's option (a). The site moves into fp-94's file list, next to grants' three test files that build a MeshStore for the same constructor. fp-86 is rescoped to the ten files and marked `[x] 2a9fb23c3`. fp-87's dependencies stay as they are, because fp-87 → fp-88 → fp-94 already orders its crate-wide daemon bar after the seam. Boundary gate 55 (EXIT=1). No code changes.
- Because: the hit goes away with the signature change, and nothing on the daemon side can remove it. Option (b), making fp-86 depend on fp-94, would park a finished row behind an open operator question (HUMAN-fp94-grants-seam) for no benefit. The only in-row workaround is laundering the type (a grants re-export, or an inferred constructor), which the rule forbids.

<!-- appendix -->

## five-programs-50 · 2026-09-24 — internal_gate_e2e.rs's ShardManager store argument moves to fp-94; fp-86 closes on its ten files

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp86-20260924.md (git-excluded), written by fp-86's worker.

- `git grep -nw 'commonwealth_state\|MeshStore\|MeshReplicatedKv' -- <the 11 files>` at 2a9fb23c3 returns only internal_gate_e2e.rs:416 (`use commonwealth_state::MeshStore;`) and :436 (`Arc::new(MeshStore::in_memory().unwrap())` passed to `ShardManager::new`). The other ten files return 0.
- shard_manager.rs:73: `pub fn new(engine: Arc<CorpusEngine>, shard_dir: PathBuf, mesh_store: Arc<MeshStore>)`. fp-94 (STATE.md) owns that signature and already lists grants' three test files that construct it. It is blocked on HUMAN-fp94-grants-seam.
- fp-87 depends on fp-88, and fp-88 depends on fp-94. The crate-wide bar therefore runs after the site is gone, with no edit needed.
- `cargo xtask boundary-gate` (corpus-engine/, toolbox): 55 violations, EXIT=1, unchanged by this rescope.

Falsified if the store argument in internal_gate_e2e.rs can be removed without changing `ShardManager::new`'s signature and without laundering the type, for example by a test-support constructor grants already exposes that takes no store. In that case the site belonged in fp-86.

</details>
