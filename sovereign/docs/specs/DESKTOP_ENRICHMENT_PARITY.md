<!-- A contract per ARCH_PRINCIPLES §1.1 — every path/claim resolves against
     the code on the commit it appears in. Update in the same PR as the code. -->

# Desktop Enrichment Parity

## Context

Commonwealth AI has **four enrichment systems** (`corpus-engine/ENRICHMENT.md`):
System 1 `field_model` (`field_skeleton.json`), System 2 `atlas`
(`atoms.json`), System 3 `tiered`/RAPTOR (SQLite), System 4 code-intel
(SCIP). The **benches validate chat over the full variety** — `bench` builds
its runtime from the same `chat_cmd::bootstrap` (`build_session`) the CLI
`sovereign chat` uses, which wires every enrichment-consuming provider.

**The desktop runtime wires fewer of them**, so a fully-built corpus has
enrichment legs silently dropped in desktop inference. This was already
load-bearing: `apply_atlas_grounding` (the default-on, bench-validated
System-2 step) hard-returns when `atlas_context_provider` is `None`, and the
desktop never wired it — atlas-grounding was **entirely dead** in the desktop
until the 2026-06 fix (`state.rs` now calls `with_atlas_context_provider` +
`init_from_cache`). Goal: bring desktop chat (both **local** and **attach**
modes) to **full enrichment leverage** — surface every enrichment a corpus
has, matching (and where the bench has no surface yet, exceeding) the bench —
and **lock it with a permanent bench-vs-desktop gate** so a leg can't drop
silently again.

Aside (not a code gap): a machine's local `sep/` can be a **stale build**
(`field_skeleton.json`, no `atlas/`) if it didn't pull the latest
`svrnmesh/sep-index`. SEP is fully built canonically; run the audit against
**fresh** corpora, and the readiness gate must flag stale enrichment.

## The parity model — three layers

A corpus's enrichment reaches chat only if all three hold; the audit + gate
check each:
1. **Seam wired** — the `Runtime.with_*` provider is constructed in bootstrap.
2. **Data built** — the on-disk artifact exists (`atoms.json`,
   `field_skeleton.json`, `raptor_nodes`, `scip_graph.db`).
3. **Surfaced in chat** — retrieval/synthesis consumes it (atom-enum virtual
   chunks, landscape-digest splice, code-trace).

## Current state (audited 2026-06)

`Runtime` enrichment seams (`sovereign-core/src/runtime.rs`): `with_gliner`
(417), `with_meta_atlas` (491), `with_atlas_context_provider` (514),
`with_wikipedia_graph` (526), `with_conv_tiered_reader` (560),
`with_landscape_digests` (576), `with_mesh_knowledge` (629).

| Seam | CLI/bench `bootstrap.rs` | server | Desktop `state.rs` |
|---|---|---|---|
| atlas_context_provider | ✓ | ✓ | ✓ (added, ~1297-1324) |
| gliner | ✓ | ✓ | ✓ (~1341) |
| conv_tiered_reader | ✓ | — | ✓ (~1380) |
| landscape_digests | — | ✓ | ✓ conditional (~1397; attach→`MeshLandscapeDigestClient`) |
| mesh_knowledge | ✓ | — | ✓ (~1257) |
| **meta_atlas** | ✓ (467-481) | ✓ (640-655) | **✗ MISSING** |
| **wikipedia_graph** | ✓ (414-427) | ✓ (590-601) | **✗ MISSING** |

Surfacing gaps beyond seams:
- **field_model is ambient for only 3 hardcoded views.** `compute_digests`
  (`knowledge_view/manager.rs:488-508`) builds a FIXED budget list —
  Personal/Conversational/Institutional — and never consults the turn's
  `enabled_corpora`. Any other `field_model` corpus (sep/philosophy,
  gutenberg) is never spliced, on **either** bench or desktop. (Epistemic
  tools `ClaimSearchTool`/`EpistemicLandscapeTool` are chat-registered on
  desktop at `state.rs:1107-1110`, but tool-driven, not ambient.)
- **Attach-mode landscape digests rely on a maybe-unmounted endpoint** —
  desktop attach wires `MeshLandscapeDigestClient` → daemon
  `/v1/knowledge/landscape_digest` (`state.rs:1401-1421`). Handler exists
  (`sovereign-mesh/src/landscape_digest_http.rs:50-79`).

## Plan (phased)

### Phase 0 — Parity harness (the gate + measurement spine)
A Rust `bench parity-compare --bank <toml>` that, per `(corpus, question)`,
runs the **bench** path (`live_runner::run_live_pinned`,
`bench_cmd/live_runner.rs:58`) AND the **desktop** path
(`bench_cmd/desktop_bridge.rs::run_bridge_live`, 152-252 — already mirrors
`run_live` over the bridge), extracts each side's **enrichment-signal set**,
and **fails when desktop ⊊ bench**.
- Signals (both run with `RUST_LOG=retrieval_audit=info`): chunk metadata
  `source=atom-enum` / `atom_type=claim` / `code_intel_summary` / `atlas:`
  on `message.metadata.retrieved_chunks`; `retrieval_audit` events
  `atom_enum_survived` / `post-apply_atlas_grounding`; `runtime.code_trace`
  `traced=N` (`code_trace.rs:162`); presence of `knowledge_view_digests`.
- Reuse `run_live` + `run_bridge_live`, the chaos-monkey bank TOML format,
  `score-answer` (`chaos_monkey.rs:994`). New = `bench_cmd/parity.rs`
  (signal extractor + differ). Run against **fresh** corpora only.

### Phase 1 — Seam parity
Desktop build (`state.rs`, in the `.with_*` chain after
`with_atlas_context_provider`):
- `with_wikipedia_graph`: port `load_wikipedia_graph` from `bootstrap.rs`
  (probes `<indexes_dir>/<corpus>/wikipedia_graph.db`; honors
  `SOVEREIGN_DISABLE_WIKI_GRAPH=1`).
- `with_meta_atlas`:
  `MetaAtlasIndex::load(corpus_engine::meta_atlas::default_meta_atlas_path().as_deref())`
  (empty-on-missing no-op), mirror `bootstrap.rs:467-481`.
- Gotcha: clone `inference` BEFORE `Runtime::new` consumes it (as the atlas
  wiring does). Both attach-safe (local probe / build artifact).
- Flip `SOVEREIGN_ATOM_ENUM_OVERVIEW` default-on (gate in
  `retrieval.rs::enumerate_typed_atom_chunks` → `!= Some("0")`; registry
  `default:"on"`; regen `docs/retrieval-pipeline.md`).

### Phase 2 — Attach-mode field_model digests
Mount/confirm the daemon `/v1/knowledge/landscape_digest` handler so
attach-mode desktop gets the 3-view digests the local desktop already gets.
Reuse `KnowledgeViewManager::compute_digests`.

### Phase 3 — Full field_model leverage (ambient field_model for ANY corpus)
A pre-synthesis step (beside `splice_landscape_digests`, `turn.rs:410` /
`streaming.rs:2445`) that, for the turn's `enabled_corpora`, loads any
`field_skeleton` and appends a digest to `context.knowledge_view_digests`
(rendered by `system_message.rs:217`). Reuse `format_landscape`
(`knowledge_view/digest.rs:39`, domain-agnostic), `FieldSkeleton`,
`index.load_field_skeleton`. New = `field_skeleton_for_corpus` accessor +
the scoped injector. Added to the **shared** runtime so bench + desktop both
gain it (harness asserts both surface it).

### Phase 4 — Enrichment-freshness / readiness
Extend the readiness gate (`validate_corpus_readiness` /
`step_readiness_disclosure`) to flag **declared-vs-built** enrichment drift
(recipe declares `[enrichment] type` the disk lacks). Stale corpora are
surfaced + skipped by the parity bank.

### Phase 5 — Lock it
`bench parity-compare` in CI/nightly (GPU lane) as a gate + a seam-invariant
unit test: desktop wired-seam set ⊇ bench's.

## Critical files
- Desktop build: `sovereign-desktop/src-tauri/src/state.rs` (~1257-1421) +
  `state/builders/knowledge_view.rs`.
- Reference to mirror: `sovereign-cli-llm/src/chat_cmd/bootstrap.rs`
  (414-481, the `load_wikipedia_graph` helper), `sovereign-server/src/main.rs`
  (590-655).
- Harness: `sovereign-cli-llm/src/bench_cmd/{live_runner.rs, desktop_bridge.rs,
  chaos_monkey.rs}` + new `parity.rs`.
- field_model surface: `sovereign-tools/src/knowledge_view/{digest.rs,
  manager.rs}`, `sovereign-core/src/runtime/{turn.rs, streaming.rs,
  system_message.rs}`.
- Attach endpoint: `sovereign-mesh/src/landscape_digest_http.rs`.
- Flags/docs: `sovereign-core/src/runtime/{retrieval.rs, retrieval_pipeline.rs}`,
  `sovereign/docs/retrieval-pipeline.md`.

## Verification
1. **Freshness first**: re-sync SEP (or scope the bank to maple-house [atlas],
   a fresh field_model corpus, commonwealth-ai [code], conversations-anthropic
   [tiered]).
2. **Harness**: `cargo run --release -p sovereign-cli-llm -- bench
   parity-compare --bank parity-bank.toml` (daemon up + desktop spawned with
   `SOVEREIGN_COMMAND_BRIDGE=1`). Expect **0 desktop-deficient signals**.
3. **Spot checks**: maple-house overview → `atom_enum_overview` + atlas chunks
   on both; fresh-SEP → field signposts + atlas on both; commonwealth-ai code
   Q → `code_trace traced>0` on both; conversation Q → RAPTOR signpost on both.
4. `scripts/sovereign-test.sh --human` + the seam-invariant test.

## Sequencing
Phase 0 + 1 first (harness gives the scoreboard; seam fixes nearly free).
Phase 3 is the headline capability gain. Phases 2/4/5 harden attach-mode,
freshness, and the permanent gate.
