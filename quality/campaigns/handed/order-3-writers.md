---
schema: work-order/v1
id: handed-3-writers
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — writers: one writer at a time on an index, atlas or config
engine: ralph pool; hd- rows default worker, REVIEW-build rows the stronger model
budget: 4 rows; PLANT on every row; covered by the hd-3/hd-7 REVIEW-audit
---

# Order: handed-3-writers — the flock goes inside the write path, and the raw handle closes

## Objective

A corpus index, its atlas, and the daemon config each get a lock that is taken by the code that
writes, not handed to a caller: a per-corpus `flock(LOCK_EX|LOCK_NB)` on `<corpus>/.writer.lock`,
taken inside the mutating method, and a `<root>/config.lock` inside `SetupConfig::save_to`. This is
the `.rebuild.lock` shape that `ScipGraph::export_to_live` already uses — the one store TOPOLOGY.toml
records as MATCHES and calls "the template for the rest" (:173). Then `table()`/`connection()` stop
handing out the raw lancedb handle to other crates, so the guard cannot be walked around from
outside corpus-engine.

Cut at round 1 (2026-09-17), 25 rows to 4: the `CorpusWriter` lease VALUE and its 16 migration rows
(a pub `acquire(dir)` proves nothing a flock inside the writer does not, and threading it through
~70 files puts the rung's own Kill — nested acquires that self-refuse — on the critical path); the
daemon config route, the desktop repoint and the config-writer trait move (none of them takes a
lock, so none delivers this promise; "the desktop is not a config writer" is a grant question, not
this rung's); both HUMAN rows (both are approval-time decisions with `depends []`).

## Premises (verified 2026-09-17, file:line)

- **The precedent.** `ScipGraph::try_rebuild_lock(db_dir)` — corpus-engine-scip/src/scip_graph.rs:483-503:
  `create_dir_all`, open `<db_dir>/.rebuild.lock`, `fs4::FileExt::try_lock_exclusive`, `WouldBlock`
  mapped to `Ok(None)`. Its one caller takes it at :2313 inside `export_to_live` and returns
  `REBUILD_COALESCED` on refusal. quality/TOPOLOGY.toml:173 names it the template.
- **The flock decider to reuse.** `sovereign_contracts::run_lock::RunLock` — run_lock.rs:69-77
  (`path` + `_file`, `#[cfg(unix)]`), `acquire(data_root)` :152 (hard-codes `daemon.lock`, LOCK_FILE
  :61), `acquire_at(path)` PRIVATE :161 (unix, `libc::flock(LOCK_EX|LOCK_NB)`) / :180 (non-unix,
  returns Ok and locks nothing), `RunLockError::{Held{path},Unopenable{path,source}}` :83-98,
  `is_enforced()` :192 (`cfg!(unix)`). `RunLock` is deliberately NOT `Clone` (:66-68). A second
  acquire in the SAME process on one path is refused — flock lives on the open file description
  (run_lock.rs test ~:217). corpus-engine already depends on sovereign-contracts
  (corpus-engine/Cargo.toml:49). corpus-engine does NOT depend on `fs4`, and `fs4` is not in the
  root `[workspace.dependencies]` (`grep -n fs4 Cargo.toml` empty; only corpus-engine-scip/Cargo.toml:54).
- **The index mutation sites** (`grep -nE '\.(add|delete|create_index|create_empty_table|add_columns|optimize)\('
  corpus-engine/src/index/*.rs`), each mapped to its enclosing fn:
  - write.rs:178 `insert_batch` (decl :83) · :210 `delete_chunks_by_source_doc` (:206) ·
    :231 `delete_chunks_by_ids` (:225) · :551 `dedupe_by_content_hash` (:464).
  - create.rs:120 `build_vector_index_with_progress` (private, :58) · :191 `create_with_sharing` (:172) ·
    :798 `build_title_scalar_index` (:796) · :951,:974 `build_indexes` (:820).
  - maintain.rs:287 `prune` (:273) · :332,:359 `optimize` (:325).
  - raptor.rs:306,:334,:350 `build_raptor_index` (:260).
  - mod.rs:749 `migrate_schema` (private, :705) — called from `CorpusIndex::open` at :805.
- **`_corpus_meta.json` has ONE funnel.** `fn write_meta(index_dir, meta)` — index/mod.rs:770, private.
  35 call sites go through it: create.rs (:267,:457,:475,:500,:528,:545,:554,:686,:699,:706,:735,:742,:758,:778),
  mod.rs (:660,:666,:807,:996,:1007,:1039,:1054,:1066,:1092,:1105,:1125,:1159,:1175,:1194,:1216,:1235,:1258
  plus four `#[cfg(test)]`), write.rs (:75,:187,:387). So every `set_*` / `write_scope` /
  `backfill_*` / `set_total_shards` / `set_grantable` / `set_personal_scope` / `set_stream_axes`
  writer named in sufficiency finding 8 is covered by guarding this one function — no per-setter row.
- **Two JSON sidecar writers on `CorpusIndex`** not through `write_meta`: `write_field_checkpoint`
  index/enrichment.rs:520 and `write_field_skeleton` :553 (both `std::fs::write(self.path().join(..))`).
- **index/enrichment.rs is NOT a writer.** Its seven `.execute()` sites (:26,:77,:125,:178,:232,:306,:415)
  are all query executions inside READ methods (`sample_embeddings`, `sample_chunks_with_embeddings`,
  `stream_embedding_column`, `all_chunks`, `all_chunks_with_raw_metadata`, `all_chunks_full`,
  `chunks_by_ids`). `bulk_update_i32_column` :485 and `bulk_update_str_column` :498 are `Ok(())` stubs
  with a TODO — they write nothing.
- **The raw handle.** `pub fn connection(&self) -> &lancedb::Connection` index/mod.rs:975 and
  `pub fn table(&self) -> &lancedb::Table` :980, under the comment "Access for sharding module".
  Every caller outside `corpus-engine/src/index/` (`git grep -n '\.table()\|\.connection()' -- '*.rs'`,
  minus `StateStore::connection()` in sovereign-store/-cli-daemon/-mesh tests and
  `job_registry::table()` in sovereign-daemon, which are different types):
  - corpus-engine/src/sharding.rs — READS :483, :552, :805, :1002, :1604; WRITES `.table().add(..)`
    at :564, :938, :1179, :1722 (four, not one).
  - corpus-engine/src/alignment_projector.rs:104 — read, inside the crate, unaffected by `pub(crate)`.
  - corpus-engine/tests/main/watcher_e2e.rs:190, :316, :443 — reads; a `tests/` target is a SEPARATE
    crate, so `pub(crate)` breaks it.
  - sovereign-tools src/code/code_search.rs:173 (`index.table()` then `.query().nearest_to(..)`) and
    src/code/mod.rs:383 (`.table().query().only_if(..)`) — both reads; both satisfied by a `query()`
    accessor.
  - corpus-engine/examples/dump_code_index.rs:34,:45,:97,:133 — reads. Cargo auto-discovers
    `examples/`, so this is compiled by LINT (`--all-targets`) and must go or be migrated.
    `corpus-engine/examples/build_fts.rs` and `build_title_btree.rs` use only pub write methods
    (`build_indexes`, `mark_ingestion_complete`, `build_title_scalar_index`) — they are unaffected
    and are NOT deleted by this order.
- **The atlas is the same store** (TOPOLOGY.toml:161-166: corpus-index holds "chunks.lance, atlas/,
  asset CAS, _corpus_meta.json, caches"). Every atlas write fn takes `atlas_dir: &Path`, and the
  atlas dir is `<index_dir>/<corpus>/atlas/` (atlas/mod.rs:147), so `atlas_dir.parent()` is the
  corpus dir the lock keys on. `git grep -nE '^\s*pub (async )?fn (write_|append_|build_and_write|build_wikipedia)'
  corpus-engine/src/enrichment/atlas/*.rs corpus-engine/src/enrichment/atlas/store/*.rs` prints
  **22** lines (not 21, and the mint's stated pattern missed `build_wikipedia_columnar_store_from_chunks`):
  writer.rs :84,:127,:318,:419,:491,:503,:517,:550,:582,:632,:653,:675; store.rs :682,:719,:734,:743;
  store/derivation.rs:67; wiki_store.rs:258,:676; ann_store.rs:276; doc_to_atoms.rs:194;
  seed_population.rs:187. Four more writers the pattern does not match (sufficiency finding 8):
  `AnnSeedTable::build` ann_store.rs:214, `doc_to_atoms::write` :176, `section_cache::store` :55,
  and `write_summary_file` summary.rs:~406 reached from the READ path `read_or_compute_summary` :355.
  `atlas_teardown` atlas/mod.rs:146 renames the whole atlas dir aside and deletes it.
- **Config.** `SetupConfig::save` sovereign-contracts/src/setup_config.rs:2067 → `save_to(path)` :2074
  (`create_dir_all` + `toml::to_string_pretty` + `std::fs::write`, no lock); `remove()` :2085 →
  `remove_at(path)` :2093 (`std::fs::remove_file`). TOPOLOGY.toml:195 already names the gap:
  "the lock is taken by each writer rather than handed to it, and `cli-setup` writes config.toml
  without taking it at all."
- **TOPOLOGY.toml invariants to update**: `every-exclusive-store-has-a-lease` :312
  (`holds=false`, "corpus-index has five writers and no lease; enrichment has three"),
  `write-is-acyclic` :317 (failing input: "Two processes writing chunks.lance concurrently …
  `svrn enrich` while the daemon is up"). `a-process-writes-only-what-it-is-granted` :322 stays
  false — nothing in this order addresses it. corpus-index store `today` :166.
- **Uncommitted now** and touched by these rows: `sovereign/SYSTEM_OVERVIEW.md` only
  (`git status --short` over the rows' paths). `corpus-engine/src/enrichment/state.rs` is dirty and
  is NOT touched by this order (the enrichment catalog is a different store).

## Steps

1. Mint the guard and take it inside every index write path — `REVIEW-build-hd-3-guard`.
   `RunLock::acquire_file(path)` is added over the private `acquire_at`; a new
   `corpus-engine/src/index/writer.rs` holds `WriterGuard`, a process-wide
   `Mutex<HashMap<PathBuf, Weak<RunLock>>>` keyed on the canonicalized corpus dir (upgrade before
   acquire, so a cached `CorpusIndex` handle and a nested re-open share one claim and never
   self-refuse), and `Error::WriterHeld { path }`. Guard taken at the top of each mutating method
   and inside `write_meta`.
2. The same guard at the atlas write fns — `hd-3-atlas-guard`.
3. Close the raw handle: `table()`/`connection()` become `pub(crate)`, a `query()` accessor is
   added, the out-of-crate readers move to it, sharding's four `.table().add` writes route through
   the guarded door, the one example that cannot compile is deleted, TOPOLOGY.toml is updated —
   `REVIEW-build-hd-3-close-raw`.
4. A flock inside `SetupConfig::save_to`/`remove_at` — `hd-3-config-lock`.

## Seams

- **Must not touch.** The 91 `CorpusIndex::open` READ sites (corpus-read is excluded — campaign.md
  Decisions); IndexMeta / IndexInfo sharing fields (hd-5); the enrichment catalog store
  (`sovereign-enrichment-catalog/src/config.rs:246`, `corpus-engine/src/enrichment/state.rs:294` —
  dirty on the tree now); `scip_graph.db` and its own flock; asset CAS (engine/mod.rs:559); raw
  `lancedb::connect` inside corpus-engine and sovereign-tools; the `cancellation-reaches-the-writer`
  invariant (TOPOLOGY.toml:327, not adopted).
- **hd-5 (custody)** edits `corpus-engine/src/engine/ingest.rs` and `harness/runner.rs`. No hd-3 row
  touches either under this design — the conflict the 25-row shape had is gone.
- **hd-2 (assemble)** edits `sovereign-contracts/src/setup_config.rs`? No — it edits `launch.rs` and
  the recipe. `hd-3-config-lock` and `REVIEW-build-hd-3-guard` both edit
  `sovereign-contracts/src/run_lock.rs`, which is why `hd-3-config-lock` depends on the guard row
  rather than running as a free lane.
- **hd-6 (principal)** touches sovereign-grants and sovereign-api. No overlap.
- `sovereign/SYSTEM_OVERVIEW.md` is edited by every rung and is dirty now: the peer's hunks must be
  committed before a REVIEW row in the main tree runs `git add` on it.
- The domains pool moves files between crates. Re-grep every path premise before a row starts.

## Done when

- **PLANT red, gate green, once per row** (the error each row names, not merely "something red"):
  `REVIEW-build-hd-3-guard` TEST(corpus-engine) red on `a_write_is_refused_while_a_foreign_holder_has_the_lock`;
  `hd-3-atlas-guard` TEST(corpus-engine) red on `an_atlas_write_is_refused_while_the_lock_is_held`;
  `REVIEW-build-hd-3-close-raw` LINT red **E0624** on `index.table()` from sovereign-tools;
  `hd-3-config-lock` TEST(sovereign-contracts) red on `a_config_save_is_refused_while_the_lock_is_held`.
- **Ambient path gone**, both greps empty:
  `git grep -nE '^\s*pub fn (table|connection)\(' -- corpus-engine/src/index/`
  `git grep -n '\.table()\|\.connection()' -- '*.rs' | grep -vE '^corpus-engine/src/(index/|sharding|alignment_projector)|sovereign-(store|daemon|cli-daemon|mesh)/'`
- LINT, TEST(corpus-engine), TEST(sovereign-contracts), TEST(sovereign-tools), TOML exit 0 at the
  last row; the audit re-runs the four PLANTs.

**The promise this rung actually makes, narrowed.** *No two in-process write paths mutate one index,
atlas or config at once, and a second PROCESS attempting a write is refused by name.* What that does
NOT cover, each named rather than implied:

- **Whole-directory replace bypasses every write path**, because it never calls one:
  `sovereign-mesh/src/canonical_pull.rs:292` (rename over `<index_dir>/<corpus>`);
  `corpus_engine::restore_snapshot_archive` (corpus-engine/src/snapshot_restore.rs:68);
  the desktop's `std::fs::remove_dir_all(&index_dir)` (sovereign-desktop/src-tauri/src/import_commands.rs:299,:695);
  `sovereign-tools/src/local_corpus/writeback.rs`; `sovereign-cli/src/project_init/mod.rs:463`
  (`remove_dir_all(&existing_index)`). `atlas_teardown` (atlas/mod.rs:146) is the one of this class
  that IS covered, because it is inside corpus-engine and its signature already names the corpus.
- **Jobs interleave.** The guard is per-write, not per-job: two processes each running an ingest on
  one corpus serialize each individual mutation but interleave between them. TOPOLOGY.md:284-287
  promises no more than that ("two processes writing one corpus index" stays runtime-refusable).
  Job-level exclusion would be a different mechanism and is not in this rung.
- **Off unix the flock enforces nothing.** `RunLock::is_enforced()` is `cfg!(unix)` (run_lock.rs:192).
  The guard logs which it got on every acquire; on Windows the desktop gets an advisory claim and
  the refusal never fires. Named, not defaulted (principle 6).
- **Raw `lancedb::connect` stays nameable** inside corpus-engine and sovereign-tools (both list
  `lancedb` in their Cargo.toml). Closing that is a clippy `disallowed_methods` rule and no gate
  the loop runs executes clippy.
- **Config keeps two writers.** After this order the CLI (`model_cmd.rs:378`, `mcp_cmd.rs:236,:283`,
  `setup_cmd/*`) and the desktop (7 sites in 3 files) both still write `config.toml` — but now both
  go through `save_to`, so both take the lock. What is not delivered is "the desktop is not a config
  writer": `SetupConfig` still derives `Serialize` with a pub `default_path()` (setup_config.rs:1991),
  so a thin surface can serialize the file by hand. TOPOLOGY.toml
  `a-process-writes-only-what-it-is-granted` :322 therefore stays `holds = false`.
- The enrichment catalog, `scip_graph.db` and the asset CAS are separate stores and keep their
  current posture.

## Kill

- The guard cannot be shared within one process without a global map that leaks (a corpus dir whose
  `Weak` never drops), or the canonical-path key is ambiguous under symlinks in a way that lets two
  spellings both claim. Stop: the lease belongs on a handle, not on a path.
- Taking the guard inside `migrate_schema` makes a READ-ONLY `CorpusIndex::open` fail while another
  process writes, and the warn-and-skip degradation in step 1 turns out not to be sound (the
  migration is not idempotent). Stop: `open` must never be refused.
- More than three production paths nest a guarded write inside another guarded write in a way the
  `Weak` map cannot merge (two different corpus dirs claimed at once — a cross-corpus merge). Stop
  and redraw the key.
