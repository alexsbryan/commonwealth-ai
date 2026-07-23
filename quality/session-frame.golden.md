---
schema: session-frame/v1
session_id: e09c5e3d-e240-4e5d-95d2-6e098d09c2d2
harness: claude-code
model: claude-opus-4-8
repo: commonwealth-ai
branch: main
head_at_end: bc492eaa
started_at: 2026-07-23T05:00:17Z
ended_at: 2026-07-23T17:35:31Z
status: completed
provenance: hand-written
notes: [e4ed7df5, bdd88cae, fdb964e6, "34139298"]
---

## Goal

Make the code-intel toolchain dependable for agents *mid-work* (the
agent-efficiency initiative: heterogeneous agents must be able to count on
these tools instead of reverting to raw reads). This session: the P0 index
wipe, incremental freshness while an agent edits, and exposing the code fact
base to agents.

## State

Done, all live-verified and suite-green:

- `inject-notes.sh` rewritten: distinct honest failure modes (probe `/status`
  first; timeout ≠ outage ≠ contract change), recency fallback that names
  itself. Proven live through two daemon restarts.
- `drift_findings` registered in the daemon tool registry (was declared in
  `MCP_TOOLS_ALWAYS` but never registered — declared-vs-served lie). Preflight
  1 FAIL → 0, READY.
- SCIP P0: `export_all` fails closed — collect all exporter rows, viability
  gate, then atomic `replace_all`; never clears up front. Reindexer adds a
  pre-rename guard: a 0-symbol staging DB can never clobber a populated live
  graph. `export_changed` (per-file incremental export) extracted from the
  same loop.
- `facts` MCP tool: read-only, embed-free, freshness-stamped
  (`fresh/aging/stale` + `lags_graph`), mtime-keyed parse cache.
- Structural watcher revived: FS save → tree-sitter overlay
  (`extract_symbol_defs`) refreshes symbol defs in ms; full rust-analyzer
  export demoted to ≥5-min cooldown + git-HEAD (commit) trigger; heavy
  rebuilds `tokio::spawn`ed with an in-flight guard so the select loop stays
  responsive.
- **The deep seam (root of "always stale"):** the daemon's tool graph was a
  startup-only in-memory snapshot (`build_tool_registry` built its own),
  while the reindexer wrote a separate merged graph with zero consumers — no
  rebuild ever reached live `symbols()`. Fixed: one shared merged
  `ScipGraph` handle threaded through `mod.rs` → `build_tool_registry` +
  `start_freshness_pipeline`.
- FactStore: `facts.json` (43MB monolith, 278k records, 1,674 files) →
  `facts.db` SQLite keyed `(corpus_id, file_path)`, WAL; `replace_all` +
  per-file `replace_files`; lazy migration from legacy JSON; writer
  (`code facts`) + both readers (`facts` tool, `check-spec`) migrated; the
  overlay patches `facts.db` per save (one tree-sitter read serves symbols +
  facts). `walk()` now skips `target/`/VCS dirs (79s build vs prior hang).

## Next

1. Soak the revived watcher under real agent load — contention was the
   empirical reason the old watcher was disabled; the 5-min cooldown const in
   `reindexer.rs` is untuned.
2. Verify the `DEGRADED symbols(ToolRegistry)` stale-SCIP preflight signal
   self-heals now that spawned rebuilds import into the shared live graph.
3. Semantic plane (embeddings) still has no live-freshness path — deliberately
   deferred; design was "inference-gated semantic later."
4. Earlier backlog still open: session-scoped notes as working memory
   (`notes` needs a session_id filter); `project register`/rebuild-nudge P1s
   (the shared-graph fix removes the restart symptom, but register still
   failed when tried).

## Decisions

- Fail-closed export (collect → viability gate → atomic replace): a
  present-but-broken exporter had wiped 189,650 symbols and reported "✓ Done".
- Plane separation (note `bdd88cae`): structural tree-sitter on the hot path,
  embed/semantic work never on the watcher — embed contention is what got the
  old CodeWatcher disabled; rust-analyzer demoted to cooldown + commit-time.
- Overlay updates symbol *defs* only, not ref edges — dropping a changed
  file's edges per save is too disruptive; full rebuild corrects edges
  (accepted eventual consistency).
- Facts shape = SCIP-shaped keyed SQLite store, NOT the atlas
  content-hash/sidecar shape: atoms are a graph with edges (content-hash keeps
  edges stable) and atlas still rewrites its JSON whole; facts are flat
  per-file rows. Grounded in a survey of all four incremental subsystems —
  one recipe: keyed store, delete-by-key-then-insert at the smallest changed
  unit.
- Separate `facts.db` (not co-located in `scip_graph.db`): tree-sitter and
  rust-analyzer artifacts keep independent lifecycles. WAL is the sharing
  medium — no in-memory merged store needed for facts.

## Invariants

- SCIP rows are 0-based; the facts extractor displays 1-based (`.row + 1`).
  Overlay symbol rows must be 0-based or every overlay def is off-by-one.
- Read `cargo.exit`/the raw log, never the wrapper's exit code — the
  background-task "exit 0" is the wrapper's; misread twice this session.
- `sovereign notes` messages with backticks/apostrophes get shell-evaluated —
  write the message to a file and pass the file.
- Daemon restarts drop work-atlas claims (TTL store is in-process).
- Stop the daemon before the full workspace suite — startup rust-analyzer
  rebuilds contend with `cargo test`.
- Never `await` a heavy rebuild inline in the reindexer select loop — it
  blacks out FS-event processing for minutes; spawn with an in-flight flag.
- Exporter output, stored rows, and the stale-file set all use one
  workspace-relative path form — exact matching only, no suffix heuristics.
- `~/.sovereign/indexes` and `~/.svrnmesh/indexes` are the same inode.
- rust-analyzer ignores non-crate root-level `.rs` files — useful as an
  overlay-only live probe.

## Dead ends

- Idle-gating rebuilds via an engine slot-busy signal: `InferenceProvider` is
  too fat for a decorator and engine surgery too invasive — superseded by the
  simpler cooldown + commit-boundary design.
- Hand-rolled `utimes` FFI in a freshness test — fragile cross-platform;
  replaced by making the freshness decision a pure function of injected `now`.

## Working set

`corpus-engine-scip/src/scip_export.rs` (export_all fail-closed,
export_changed), `scip_graph.rs` (replace_all/replace_files/
replace_file_symbols + corpus-scoped variant);
`sovereign/crates/sovereign-mesh/src/reindexer.rs` (overlay wiring, spawned
rebuilds, cooldown); `corpus-engine/src/facts.rs` (extract_symbol_defs,
per-file facts, walk skip) + `facts_store.rs` (FactStore);
`sovereign-tools/src/code/facts_tool.rs`;
`sovereign-cli-daemon/src/daemon_cmd/{tool_registry,mod,bootstrap}.rs` (shared
merged handle); `.claude/hooks/inject-notes.sh`;
`quality/agent-preflight.golden.json`; SYSTEM_OVERVIEW.md +
docs/CHECK_CODE_AGAINST_SPEC.md updated.

## Verification

Final suite: **7928 pass / 0 fail, cargo exit 0** (grew 7888 → 7928 across
the session; the one mid-session red was a pre-existing stale assertion from
the prior session's `code_search` MCP restore, fixed). Live proofs: overlay
symbol visible in `symbols()` ~6s after save; facts visible ~7s; create and
delete lifecycles both proven; facts query 47ms indexed vs 43MB monolith
load. Daemon redeployed and healthy; preflight READY, 0 fail (one DEGRADED
stale-SCIP path expected to self-heal). At session end the FactStore tail was
uncommitted (commit proposal given; the hook/P0/watcher work landed
mid-session as `eff6ac12` + `bc492eaa`; the FactStore tail landed as
`71d7ac20` shortly after close).
