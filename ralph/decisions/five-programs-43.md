<!-- ledger -->

**five-programs-43 · 2026-09-24 · fp-80 · director** — this commit
- Needed: fp-80 halted on its own file-count bar. Flipping the three existing AppState fields (`inference_store`, `peer_preferences`, `activity_emitter`) to fp-78's async ports breaks readers in 15 files outside the row's ten, so the row as written was a 25-file commit.
- Chose: split by field, one dimension per move. fp-80 keeps only the additive part: StorePart's two new port fields over the in-process backing, the ten files' `fabric.*` readers, the recording double and the bridge. New rows: fp-89 flips `activity_emitter` with its readers (4 files), fp-90 flips `peer_preferences` (5 files). `inference_store` (15 files) takes three rows. fp-91 and fp-92 move its readers onto AppState accessors in their post-flip shape with the type unchanged. fp-93 then flips the type and fills `InferenceCache` before the router serves. fp-81 now depends on fp-93, and fp-84 to fp-86 depend on fp-93 so they do not race the test files fp-89 to fp-93 touch. The package's question 3 is answered in fp-93: async accessors read through the port (§12 D4), and the cache serves only readers that are genuinely sync. Boundary 54, unchanged, since no code moved.
- Because: row splitting and the package's own recommended split are inside the charter. Each commit stays at or under ten files and preserves behaviour (ARCH 2). The accessors extend AppState's existing ones at state.rs:1218-1250, following ARCH 8 (one accessor per path), rather than adding a new type.

<!-- appendix -->

## five-programs-43 · 2026-09-24 — fp-80's field flip splits per field; inference_store's reader shape moves before its type

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp80-20260924.md. The director reproduced it at 9ac559534.

- `git grep -n "\.\(inference_store\|peer_preferences\|activity_emitter\)\b" -- src tests` in sovereign-daemon, minus the ten files, returns 15 files. That matches the package file for file. Per field: inference_store has 14 files including daemon.rs and state.rs, peer_preferences has 3 outside the ten, and activity_emitter has 2.
- Method census on inference_store: list_models 9, get_local_embed_model 5, set_model_info 3, set_plan 1, remove_model_info 1, get_llama_address 1, plus multi-line call sites. The ports are async (`LedgerFut`, ledger_port.rs:45). `InferenceCache` (rails_client/ledger.rs) answers `Result<_, NeverFilled>` and is filled only by `refill()`, and nothing calls `refill()` today.
- §12 D4 names a read-through cache and gives no refill cadence. A cache-only reader would therefore see peer models only on refill, where today's store read is live. fp-93 avoids that by routing async readers through the port and keeping the cache for sync readers only. It stops at §6 if a sync reader whose freshness matters turns up.

Options weighed:
- (a) Accept a 25-file commit. This breaks the row's own §6 bound, and one commit would mix three fields plus a reader-shape change.
- (b) Chosen. One row per field. The one field over the cap splits along the reader-shape and backing-type seam, which is the same seam five-programs-42 used for Fabric.
- (c) Add port fields alongside the old ones under new names and migrate. Rejected: it contradicts five-programs-36 (2), which reuses the names, and it adds a rename pass.

The state mint grows by five rows (fp-89 to fp-93). Each is a piece fp-80 already carried, sized to the bar. None is new scope.

REVIEW-AFTER: the accessor shape in fp-91 (async `Result<_, LedgerAbsent>` next to sync `Result<_, NeverFilled>`) is the director's choice. Neither the TSV nor §12 names it.

Falsified if fp-91 or fp-92 finds a sync-context READER of the model list or embed model whose freshness is observable, because then the cache cadence is a live question for the operator. It is also falsified if any of fp-89 to fp-93 exceeds about ten files, or if fp-80's additive fields cannot land without touching an existing field's readers.

</details>
