# Spec: Tiered retrieval port for conversation corpora

**Status:** Shipped 2026-05-23.
**Lifecycle:** Spec preserved for design-rationale forensics (why
the new `conversation_skeleton` sidecar table instead of extending
`document_assets`; why the conversation-seal trigger over an
ingest-completion hook). Live runtime surface in
[`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md) and
[`TIERED_RETRIEVAL_PHASE_B.md`](TIERED_RETRIEVAL_PHASE_B.md) port
matrix.

**Prerequisites for reading:** [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md)
(Phase A architecture this is the Phase B port of);
[`CLUSTER_SCORE_BLEND.md`](CLUSTER_SCORE_BLEND.md) for the
retrieval-side blend the briefing eventually feeds.

## Why this exists

The `conversations-anthropic` import (Anthropic chat-export recipe, single-user dataset) wedged in five days of restart cycles trying to complete the legacy atlas enrichment pipeline. T1 (chunks + 1024-dim embeddings + FTS) landed cleanly on the first run — **16,404 chunks across 576 conversations, ~109 MB on disk**. What loops forever is Phase 1b: per-chunk entity extraction calling Slow LLM with free-form JSON parsing (`corpus-engine/src/enrichment/entity_extraction.rs:505`). Empirical parse-fail rate ~30%. Each failure triggers retry. With 16k chunks the loop never converges within a daemon session, and every restart auto-resumes the same loop (`sovereign-mesh/src/auto_resume.rs:198`).

The attached-document tiered surface (Phase A, shipped 2026-05-22) replaces that monolithic enrichment with three explicit milestones: T1 cosine retrieval, T2 entity-graph PPR multi-hop, T3 RAPTOR signposts + motifs + verbatim quote spans. The user can query after T1; quality climbs as T2 and T3 land.

The TIERED_RETRIEVAL "Phase B" section names conversations as the first port target. This spec is that port.

## State on disk today (2026-05-22)

Surviving partition: `~/.svrnmesh/indexes/conversations-anthropic-partition-node-b88252e4325bc377/`

- `chunks.lance`: 16,404 rows, schema `(id, content, title, url, embedding[1024], metadata, content_hash, source_doc_id, ...)`. Embeddings populated. FTS + vector indexes built.
- `metadata` JSON per chunk carries `{conv_uuid, created_at, updated_at, msg_count, summary}`.
- `source_doc_id == conv_uuid` — one Lance row per chunk, threaded-turns chunker grouped each chunk into one conversation's UUID.
- `_corpus_meta.json`: `ingestion_in_progress` was flipped to `false` 2026-05-22 to stop auto-resume; rest unchanged.
- `_enrichment_checkpoint.json`: `phase_1_complete=true, phase_1b_complete=false`. Tiered impl ignores this — the legacy atlas pipeline is being supplanted, not resumed.

Per-conversation chunk distribution: mean 28.5, median 16, p95 100, max 510. **Most conversations are very small**; the long tail (~5%) approaches book-length.

## Architectural commitments inherited from Phase A

These remain load-bearing for portability. The conversation port must not break any of them.

1. **Builder signatures are corpus-free.** `build_raptor_atlas(inference, &[ChunkInput], &[Vec<f32>], DocumentTypeTag) -> Vec<RaptorNode>` already takes generic `ChunkInput`, not `DocumentChunk`. Same for `extract_action_atoms`, `extract_segments`, `detect_segment_boundaries`, `verify_quotes`. The conversation port reuses these unchanged.
2. **Storage tables key on string `asset_id`.** Conversations namespace themselves as `conv:<uuid>` and never collide with attached-doc `<asset_uuid>` or future `vault:<root>:<note_uuid>` or `corpus:wikipedia:<chunk_id>` keys.
3. **State machine is per-source.** Pending → Indexing → PartiallyReady → BuildingSkeleton → MultiHopReady → BuildingSkeleton → Ready → Failed. The variant set is universal; conversations get one state machine per `conv_uuid`, not one per corpus.

## What does NOT carry over

- **`document_assets` table.** Attached docs have one row per asset with `skeleton_json` blob. Conversations don't sit in `document_assets` and shouldn't — the conv's authoritative record is its Lance chunks + the `conversations-anthropic` corpus index.
- **`AssetState` on `DocumentAsset` struct.** The variant set is shared, but the carrying struct is corpus-specific. Conversations get a parallel `ConversationEnrichmentState` (or generic `CorpusEnrichmentTier` — see "Generic enrichment trait?" below).
- **`build_attached_doc_briefing`.** Hard-coupled to `document_assets` lookup. Generalisation needed (see "Briefing generalisation").

## Decision 1: per-conversation vs corpus-wide RAPTOR

This is the load-bearing architectural choice. Picking the wrong one costs a re-impl.

**Per-conversation RAPTOR (recommended).** Run `build_raptor_atlas` once per `conv_uuid`. 576 trees for the current import; each tree spans only that conversation's chunks. `asset_id = "conv:<uuid>"`.

Pros:
- **Semantic coherence.** Each tree summarises one coherent dialogue. A leaf-cluster summary stays inside the conversation's actual topic, never mixes "user asking about Rust" with "user asking about Conrad".
- **Briefing per conversation.** When the user queries against a specific conversation (most common case for "what did I talk about with Claude regarding X?"), the briefing pulls from that conversation's tree only — bounded prompt budget.
- **Incremental natural fit.** Phase B's "run on conversation seal" trigger (TIERED_RETRIEVAL.md line 151) only works for per-conv. New conversation → one new tree, no cross-conv touch.
- **Sizing.** Mean 28.5 chunks, median 16. At those sizes RAPTOR collapses to a flat 1-2 level tree (per `raptor_atlas.rs` LEAF_TARGET_CLUSTER_SIZE=20 logic, ≤40 items drops to a single root). Cheap to build. Long-tail conv at 510 chunks is half a book — full multi-level tree, still well within Phase A's measured per-doc budget.

Cons:
- **576 LLM-orchestrated builds.** See the "Performance budget" section below for naive vs optimized wall-clock estimates and the optimization order that gets us to the realistic floor.
- **No cross-conversation structural signal.** A leaf cluster in conv-A and a leaf cluster in conv-B may share topic; per-conv RAPTOR doesn't surface that.

**Corpus-wide RAPTOR (rejected).** One tree over all 16,404 chunks. Single `asset_id = "conversations-anthropic"`.

Pros:
- **Cross-conv structural map.** "Show me the cluster about my React debugging conversations across all 576 chats" works natively.
- **Single tree build.** ~820 leaves at 20 chunks each, 4-5 levels, ~1 hour wall-clock.

Cons:
- **Cluster soup.** k-means on 16k mixed-topic chunks gives clusters anchored on superficial token overlap. A leaf-cluster summary that mixes 4 different conversations is semantically useless for briefing.
- **Briefing budget blowup.** The briefing builder pulls top-K signposts. Top-K over a 16k-chunk tree means signposts from random conversations the user isn't asking about — directly counter to the briefing's vocabulary-priming purpose.
- **No incremental story.** Adding one new conversation requires rebuilding (or carefully insert-merging) the whole tree.

**Decision: per-conversation RAPTOR. Defer corpus-wide cross-conv structural signal to a separate future-work item** (call it "motif graph across conversations" — likely implemented as a corpus-level second-pass on the per-conv `asset_motifs` tables).

## Decision 2: trigger model

Two valid triggers; both can coexist on the same code path.

**Trigger A — batch on ingest completion.** When `corpus-engine/src/engine/ingest.rs` finishes T1 (post `mark_indexes_built`, around line 1506), if the recipe carries `[enrichment] type = "tiered"`, dispatch one tiered enrichment job per distinct `source_doc_id` in `chunks.lance`. Runs sequentially through `futures::stream::iter(...).buffered(SUMMARIZE_BUFFER)` for fan-out where mesh allows.

**Trigger B — on conversation seal.** A conversation becomes "sealed" when it's been inactive past a threshold (default: 24 hours). Sealed conversations are eligible for tiered enrichment; one tree per seal event. Lives outside the corpus ingest path — runs as a background sweeper job that wakes on a cron-ish schedule (~hourly) and processes any newly-sealed conversations.

**Decision: implement both, gate behind the same recipe key.** Trigger A satisfies the current import use case (one-shot batch enrichment for the entire claude.ai export). Trigger B satisfies the ongoing case (the user's daily Claude conversations accumulating into `conversations-anthropic` over time, with tiered enrichment landing per-conv as each one quiets down). Implementation order: A first (covers the immediate "finish the import" pain), B in a follow-up.

## Decision 3: storage shape

Phase A's `raptor_nodes` and `asset_motifs` tables `REFERENCES document_assets(id) ON DELETE CASCADE`. That FK is the friction.

**Option A — drop the FK on `raptor_nodes`/`asset_motifs`, allow polymorphic `asset_id`.** One migration; both attached-doc and conv writes go to the same tables. Memory cost: future cleanup of orphaned conv rows when a conversation is deleted needs app-level cascade since SQLite FK is gone.

**Option B — new sidecar tables `conv_raptor_nodes`, `conv_motifs` with `conv_uuid TEXT PRIMARY KEY`.** No migration risk to attached-doc data; conversation queries hit a separate physical table. Cost: two CRUD code paths the briefing layer has to merge.

**Decision: Option B initially, with Option A as a 2027-Q1 consolidation once vault + corpus ports prove the pattern.** Spec line 160 (TIERED_RETRIEVAL.md) says "use string IDs"; we honour the spirit but keep attached-doc data isolated until we have three corpora successfully sharing the schema. The two-table cost is small — the briefing builder already abstracts over its data source via the per-section emptiness check (TIERED_RETRIEVAL.md retrieval contract rule 2).

Schema for the sidecar:

```sql
CREATE TABLE conv_raptor_nodes (
    node_id                 TEXT    PRIMARY KEY,
    conv_uuid               TEXT    NOT NULL,         -- no FK; conv lives in Lance
    corpus_id               TEXT    NOT NULL,         -- partitions multiple conv corpora
    level                   INTEGER NOT NULL,
    summary                 TEXT    NOT NULL,
    summary_embedding       BLOB    NOT NULL,         -- raw little-endian f32 bytes
    centroid_embedding      BLOB    NOT NULL,
    children_node_ids       TEXT    NOT NULL,         -- JSON array
    direct_member_chunk_ids TEXT,                     -- JSON array of Lance row IDs; NULL above level 0
    evidence_chunk_ids      TEXT    NOT NULL,
    quote_spans             TEXT    NOT NULL,
    primary_entities        TEXT    NOT NULL,
    cluster_coherence       REAL    NOT NULL,
    created_at              INTEGER NOT NULL
);
CREATE INDEX idx_conv_raptor_nodes_conv_level
    ON conv_raptor_nodes(corpus_id, conv_uuid, level);

CREATE TABLE conv_motifs (
    corpus_id            TEXT    NOT NULL,
    conv_uuid            TEXT    NOT NULL,
    term                 TEXT    NOT NULL,
    tf_idf_score         REAL    NOT NULL,
    occurrence_chunk_ids TEXT    NOT NULL,
    is_distinctive       INTEGER NOT NULL,
    PRIMARY KEY (corpus_id, conv_uuid, term)
);
CREATE INDEX idx_conv_motifs_distinctive
    ON conv_motifs(corpus_id, conv_uuid, is_distinctive DESC, tf_idf_score DESC);

CREATE TABLE conv_skeletons (
    corpus_id       TEXT    NOT NULL,
    conv_uuid       TEXT    NOT NULL,
    state           TEXT    NOT NULL,    -- 'Pending', 'PartiallyReady', 'MultiHopReady', 'Ready', 'Failed'
    skeleton_json   TEXT,                -- T2 partial skeleton: main_entities, entity_index, actions, structural_moments
    overview        TEXT,                -- T3 overview
    segments_json   TEXT,                -- T3 TextTiling segments
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (corpus_id, conv_uuid)
);
```

Lives at `~/.svrnmesh/conversations.db` (new SQLite handle) or merged into the existing flat-file daemon DB (`~/.svrnmesh/active_notes_db` family — per memory `invariant_lint_db_path_canonical`, flat-file stores live at `sovereign_root`). Decision: merge into `~/.svrnmesh/features.db` since that's where flat-file daemon state already lives. Migration runs at daemon startup.

## Decision 4: recipe surface

Add a new value to `[enrichment].type`:

```toml
[enrichment]
enabled = true
type = "tiered"           # new — was "atlas" / "personal" / etc
domain = "conversational" # informs DocumentTypeTag passed to build_raptor_atlas
```

At `corpus-engine/src/engine/ingest.rs:1510`, branch on `enrichment_config.enrichment_type`:

- `"atlas"` → existing FieldModelEngine path (unchanged; old corpora still work)
- `"tiered"` → new `TieredEnrichmentRunner::run(corpus_id, chunks, embeddings, store)` path

The runner per-iterates `source_doc_id` over the corpus's Lance chunks, batches each conv's chunks + embeddings, and calls into the corpus-free builders. Writes go to the new SQLite sidecar tables.

Update the conv recipe at `sovereign-recipes/conversations-anthropic/recipe.toml` to set `type = "tiered"`. The duplicate at `corpus-engine/recipes/conversations-anthropic/recipe.toml` likely needs the same edit — check during impl.

## Decision 5: briefing generalisation

`build_attached_doc_briefing` (`sovereign-core/src/runtime.rs:13046`) is hardcoded to look up `document_assets` by conversation. The conv corpus has no `document_asset` row; the briefing needs a polymorphic dispatch.

Two options:

**Option X — trait + two impls.** Define `trait CorpusBriefing { async fn build(&self, ...) -> (String, Vec<String>); }`. One impl for attached docs (existing code, refactored), one for `conv:<uuid>` keyed at the conv level. Runtime picks impl based on what the query's retrieval surface returns.

**Option Y — single function with corpus_id branch.** `build_briefing(conversation_id) -> (String, Vec<String>)` switches on the corpus the retrieved chunks came from. Less abstraction, more conditional logic.

**Decision: Option X.** Phase B already names two more porting targets (vault, SEP); trait is paying for itself by the second impl, definitely by the third. Place trait in `sovereign-core/src/briefing.rs` (new file).

When the retrieval surface returns chunks from multiple corpora (rare — typically the user is querying against one corpus or one attached doc), each impl's briefing block is rendered separately and concatenated in priority order: attached doc first, then `conv:<uuid>` of whichever conversation the chunks predominantly came from.

## Performance budget

The naive port of Phase A's attached-doc pipeline to 576 conversations gives a wall-clock estimate that's painful but functional. With targeted optimizations the floor drops by 5-10x. Move 3 should land the v0 with at least optimizations 1-3 below; defer 4-5 to v1 if time-boxed.

### Naive baseline (Phase A approach, no conv-specific optimization)

Direct port of attached-doc tiered enrichment:

- Every leaf cluster gets one Slow-slot summarization LLM call (~3-5s each on Qwen3.5-35B-MTP)
- Every conv gets one Slow-slot overview generation call
- Every conv gets one Slow-slot motif classification call
- T2 entity extraction uses Slow per-batch

Sum across the chunk distribution: roughly **1500-2500 Slow-slot calls**. Single-daemon, single-Slow-slot wall-clock: **2-4 hours**. With Phase A's `buffered(6)` and a 2-peer owner-mesh: **30-60 minutes**.

### Realistic floor (with conv-specific optimizations)

With optimizations 1-4 below applied:

| Bucket | Convs | Calls each | Approach | Time |
|---|---:|---:|---|---:|
| ≤8 chunks | ~170 | 0 | skip RAPTOR — synthetic single node from conv title | 0s |
| 9-30 chunks | ~250 | 1 (batched ×8) | Fast slot, lark-grammar multi-conv summary | ~96s |
| 30-100 chunks | ~120 | 1-3 leaf | Fast slot per leaf | ~360s |
| 100-300 chunks | ~36 | 5-15 leaf + 1 root | Fast slot for leaves, Slow for root | ~720s |
| 300+ chunks (long tail) | ~4 | 20-30 | Slow slot (full Phase A treatment) | ~400s |
| Motif classification | 576 | 1 (batched ×8) | Fast slot | ~144s |
| T2 entity extraction | 576 | 1 | Fast slot, lark schema | ~864s |
| Overview generation | 0 | — | reuse conv.title from claude.ai export | 0s |

**Single-slot Fast-mostly wall-clock: ~43 minutes. With 2-peer owner-mesh: ~10-15 minutes.**

Bottleneck after optimizations 1-4: T2 entity extraction at ~864s (one Fast call per conv). Optimization 5 (cross-conv batched T2) takes another bite out of that but is meaningfully more complex (cross-conv prompt design, careful lark grammar, per-conv attribution risk) — defer to v1 unless v0 ships and the bench shows T2 is still load-bearing.

### Optimization order

Each row lists effort (LOC + risk) so Move 3 can budget which ones land in v0 vs v1.

| # | Optimization | Mechanism | Saves | Effort | When |
|---|---|---|---:|---|---|
| 1 | **Fast slot for short-conv summaries** | Route any conv with ≤30 chunks through `Speed::Fast` (9B) instead of `Speed::Slow` (35B). Chat content is already compressed — 9B handles it without quality regression. Spec leaves a `latency_class` knob on `build_raptor_atlas` (`DocumentTypeTag::Conversation` defaults Fast; long-tail >300 chunks override to Slow per-call). | ~2.7x per call | ~30 LOC, low risk (just a slot routing change) | v0 — must-land |
| 2 | **Skip RAPTOR for tiny convs (<8 chunks)** | Persist a single synthetic `RaptorNode` with `summary = conv.title`, `direct_member_chunk_ids = [all chunk ids]`, `summary_embedding = mean(chunk_embeddings)`. No LLM call. Briefing still gets a one-line signpost for these convs from the title. | ~170 calls outright | ~50 LOC, low risk (one new code path in `TieredEnrichmentRunner`) | v0 — must-land |
| 3 | **Reuse conv.title as overview** | claude.ai export ships `conv.title` per conversation in the source JSON; the `threaded_turns` chunker preserves it in `metadata.title`. Use it directly instead of generating an overview. | ~576 calls | ~10 LOC, zero risk | v0 — must-land |
| 4 | **Batched multi-conv summary prompt** | For the 9-30 chunk bucket: bundle 8 convs into one prompt, lark grammar enforces 8 separate `<summary>` blocks per the conv's index. One LLM call summarizes 8 convs at ~8x throughput vs sequential. Catch: per-conv attribution requires careful grammar; quality dips if convs in the batch are too topically mixed (mitigated by sorting batch members by embedding centroid distance — co-cluster similar convs together in the same prompt). | ~8x throughput on small-conv bucket | ~150 LOC + grammar design, medium risk (new prompt-engineering surface) | v0 — should-land if time allows; otherwise v1 |
| 5 | **Cross-conv batched T2 entity extraction** | Same shape as #4 but applied to the T2 entity-extraction pass. 8 small convs share one Fast-slot lark-grammar call that emits 8 separate entity lists. The bottleneck post-1-4. | ~8x throughput on T2 | ~200 LOC + grammar + per-conv attribution carefully tested | v1 — defer unless bench shows T2 wall-clock is user-facing pain |

### Owner-mesh fan-out policy

The conv recipe carries `mesh_sharing = false` to prevent third-party leakage. But "third party" ≠ "any peer". The user's own toolbx fedora node is also `node-*` in the mesh and is owner-trusted.

The right policy for Move 3 to honour: **enrichment LLM calls for `mesh_sharing = false` corpora may fan out to peers iff the peer is owner-trusted**. The `node` registry already distinguishes peer trust levels (per [[reference_tailscale_peer_debug]] the user has multiple owner-controlled devices on the Tailscale mesh).

Move 3 add: a `node.trust = "owner" | "community"` per-peer config (defaulting to `community` for safety), and the load balancer's `mesh_sharing=false` filter becomes "filter community peers" rather than "filter all peers". Without this, the owner-mesh fan-out numbers above are theoretical — the load balancer will refuse all peer dispatch.

This is a real Move 3 design ask, not just an annotation. If trust levels can't land in scope, the realistic mesh floor reverts to "single-daemon Fast-mostly, ~43 minutes" and that's still acceptable for an overnight-batch import — just slower than the 10-15 min the policy unlocks.

## Builder reuse map

| Builder | File:line | Conv port treatment |
|---|---|---|
| `build_raptor_atlas` | `sovereign-tools/src/raptor_atlas.rs:80` | reuse unchanged — pass `DocumentTypeTag::Conversation` (new variant) |
| `extract_action_atoms` | `sovereign-tools/src/document_asset.rs:2659` | reuse unchanged — operates on `&[TextChunk]` |
| `extract_segments` | `sovereign-tools/src/document_asset.rs:1913` | reuse but evaluate: TextTiling boundary detection on a 16-chunk conv may overfit. May want to skip segment extraction for short convs (n < 20). |
| `detect_segment_boundaries` | `sovereign-tools/src/document_asset.rs:2479` | reuse unchanged |
| `extract_motif_candidates` + `classify_motifs` | `sovereign-tools/src/document_asset.rs` (search) | reuse unchanged — pure-Rust TF-IDF + one LLM classification call per conv |
| `verify_quotes` | `sovereign-core/src/quote_verification.rs:70` | reuse unchanged at briefing-output time |
| `entity_graph.rs` PPR | `sovereign-tools/src/entity_graph.rs` | reuse for T2; per-conv graph (small, fast) |
| `build_attached_doc_briefing` | `sovereign-core/src/runtime.rs:13046` | refactor into trait + impl per Decision 5 |

The one place a new `DocumentTypeTag::Conversation` variant lands is wherever the existing tag is matched — likely a `match` on `doc_type` inside `raptor_atlas.rs` and the briefing builders for prompt steering. The conv-specific prompt rule: leaf-cluster summary should describe **what the conversation was about**, not **what the document says** — different framing for the same underlying mechanism.

## Bench harness

Extend `sovereign/bench/conversation/`:

1. Add a new bench `conv-anthropic-tiered.toml` that queries the conv corpus across question shapes:
   - **T1-class (recall):** "what did I ask Claude about [topic]?" — pure chunk-cosine recall
   - **T2-class (multi-hop):** "which conversations bridged [topic A] and [topic B]?" — relies on entity-graph PPR
   - **T3-class (signpost):** "summarize my conversations about [topic]" — relies on leaf-cluster summaries
   - **T3-class (verbatim):** "what exact phrasing did Claude use when explaining [concept]?" — relies on `quote_spans` + verify_quotes

2. Hand-author a 20-question seed bank against the user's actual conversations. **Don't reuse the existing conversation-bench seed bank** — it's calibrated against `conversations-personal` (Sovereign-internal chats), different corpus. Build fresh against the Anthropic export. Mark questions by tier-class so the bench rollup shows "T1 retrieval works / T2 doesn't / T3 wins on signpost but loses on verbatim".

3. Three baselines:
   - **T1-only (current state):** runs against the existing chunks.lance + dense+FTS, no enrichment
   - **T2-added:** PPR multi-hop on top of T1
   - **T3-added:** full tiered (RAPTOR signposts + motifs + verbatim)

The empirical question the bench answers: **does tiered enrichment actually improve answer quality on conversation queries, or does it cost ~1 day of impl + ~4 hrs/import for negligible bench lift?** If T2 and T3 don't beat T1 by ≥ 8-10 points on multi-hop and signpost classes respectively, the abstraction isn't earning its keep and should be reconsidered.

## Generic enrichment trait?

The TIERED_RETRIEVAL spec floats a hypothetical `CorpusEnrichmentTier` trait. **Don't introduce it yet.** Two impls of an abstraction is the cliff edge where over-design starts. Wait for the third corpus port (vault or SEP) to land before extracting the trait. The conv port should mirror the attached-doc state machine variants directly and the trait extraction is mechanical.

The thing worth committing to now: **the variant set itself.** `Pending → Indexing → PartiallyReady → BuildingSkeleton → MultiHopReady → BuildingSkeleton → Ready → Failed` is the universal vocabulary. Anything that diverges from those names is a smell.

## Implementation order

1. **Migration + sidecar tables.** Add `conv_raptor_nodes`, `conv_motifs`, `conv_skeletons` to `sovereign-store/src/migrations.rs`. Wire into the daemon startup migration runner. Smoke test: SQLite open, tables exist.
2. **Recipe surface.** Add `enrichment.type = "tiered"` branch to the recipe parser; threadable through to `ingest.rs:1510`. New `TieredEnrichmentRunner` stub that takes `(corpus_id, lance_path)` and returns `Ok(())` immediately. Smoke test: ingest with `type = "tiered"` finishes without crashing.
3. **Per-conv batch dispatch.** TieredEnrichmentRunner reads chunks from Lance, groups by `source_doc_id`, drops convs with `< 4` chunks (RAPTOR is meaningless that small), and iterates the rest through a `buffered(6)` pipeline that calls `build_raptor_atlas` per-conv. Persists `raptor_nodes` rows. Smoke test: 576 conv → 576 sets of raptor_node rows in `conv_raptor_nodes`.
4. **T2 wiring.** For each conv: extract action atoms via `extract_action_atoms`, build entity graph, persist into the `skeleton_json` field of `conv_skeletons`. Mark T2-done milestone via `state = "MultiHopReady"`.
5. **T3 wiring.** Generate overview + segments + motifs per conv. Mark `state = "Ready"`.
6. **Briefing trait.** Refactor `build_attached_doc_briefing` into trait + attached-doc impl. Add conv impl that pulls from `conv_skeletons` + `conv_raptor_nodes` + `conv_motifs` keyed on `corpus_id + conv_uuid`. Wire trait dispatch into runtime.
7. **Bench.** Author `conv-anthropic-tiered.toml` + 20-question seed. Run baseline T1-only first; mark spec verified once T2 and T3 each show measurable lift on their target question classes.

Stop after step 5 if total impl time exceeds 10 hours — the bench can land next session.

## Honest known gaps

These are real things the design as scoped doesn't yet handle. Phase B impl should call them out so a future port (vault, SEP) doesn't re-derive the surprise.

### No cross-conversation structural signal

Per-conv RAPTOR gives 576 trees. There's no tree spanning multiple convs, no built-in answer to "show me clusters that recur across my conversations". The conv-level `conv_motifs` table partially compensates (a term that's distinctive across many convs surfaces as a high-tf_idf entry per-conv), but no top-level aggregation rolls these up.

The right fix when this becomes load-bearing is a second-pass "corpus motif aggregator" that scans all `conv_motifs` rows for a `corpus_id`, ranks terms by cross-conv prevalence, and persists into a `corpus_motifs` table. Out of scope for the initial port.

### TextTiling on short conversations is noise

The TextTiling boundary detector (`detect_segment_boundaries`) is calibrated for book-length texts (~1000 chunks). On a 16-chunk conversation it has nothing to bite into. The conv port skips segment extraction for convs with `n < 20`. Won't surface as a quality issue but does mean the `skeleton.segments` field is empty for the majority of conversations.

### Quote verification's contract is weaker on chat

`verify_quotes` matches verbatim spans ≥ 40 chars against source chunks. Chat text is shorter per-message; long verbatim spans that *would* mean something in book context (a 200-char sentence) rarely occur in chat. The model will under-quote naturally, which means fewer demote-to-`[unverified]` events but also fewer high-confidence verbatim citations. Tune the threshold for conv corpora (try `MIN_QUOTE_SPAN_CHARS = 20` for conv-only builds; keep 40 for attached docs) and surface this as an `enrichment.quote_threshold` recipe key once we have two corpora that need different values.

### Privacy: the import has the most sensitive data

`conversations-anthropic` is marked `mesh_sharing = false` and `license = "private"` in the recipe. The tiered enrichment runs Slow-slot inference calls that emit chunk content as prompt context. **All inference must stay local; no remote-peer fan-out for this corpus.** The mesh load balancer must be configured to filter out enrichment requests where the source corpus has `mesh_sharing = false`. Audit this is honoured before Trigger A's `buffered(6)` ever shifts work to a peer. The current attached-doc T2/T3 code path inherits the user's general mesh-sharing posture and may not have a corpus-level override; verify and add one if missing.

### Re-running enrichment after recipe change

If the user later adjusts the conv recipe (e.g., changes `domain` from `conversational` to something tuned differently), there's no "re-enrich" path. The `conv_skeletons.state = "Ready"` rows would need to be torn down per-corpus to force re-enrichment. Add a `sovereign corpus reenrich --corpus <id>` CLI command as a Phase B sibling task — not load-bearing for the initial port but operationally required.

## What this spec deliberately does NOT do

- **No HippoRAG v2 (fact-node graph diffusion).** TIERED_RETRIEVAL.md Section "On HippoRAG 1 vs 2" is the honest framing of why. Same reasoning applies here.
- **No replacement of legacy `atlas` enrichment path.** The branch in `ingest.rs:1510` keeps both paths alive. Existing corpora using `type = "atlas"` continue to work. Migration of other corpora to tiered is a corpus-by-corpus operator decision, not a forced upgrade.
- **No retry of failed entity extraction parses.** The legacy parse-fail loop is gone because the path is gone. Tiered's lean entity extraction uses lark-grammar-enforced output (the same fix memo references via `invariant_balanced_envelope_needs_grammar_gate` and the in-flight llguidance work). Don't re-introduce free-form JSON parsing in any conv-port code.
- **No `personal` domain merge.** `conversations-personal` (Sovereign-internal chats) stays on its existing recipe + atlas path until it has a reason to migrate. Two-impl phase isolates the conv port to one recipe.

## Retrieval surface — next session's trait

The retrieval-side surface landed 2026-05-22 (steps 6-equivalent in spec impl order). The two extension points that future RAPTOR-shaped stores will need are already named in the conv-specific impl:

- **`ConvTieredReader`** (`sovereign-core/src/conv_tiered.rs`) — async trait with `list_conv_skeletons_for_corpus(corpus_id, &[conv_uuid])` and `list_conv_raptor_nodes(corpus_id, conv_uuid)`.
- **`build_conv_tiered_briefings`** (`sovereign-core/src/conv_briefing.rs`) — async function taking `Arc<dyn ConvTieredReader>` + `&[ScoredChunk]` + `display_categories`, returning a `ConvBriefingPayload` ready to inject into the prompt.

This is **Option C** from the architectural-shape menu in the spec history — a read-side composer that knows about the conv-tiered tables specifically. The shape implied by the public functions is the future generic trait:

```rust
// Sketch, NOT yet defined. Will land when the second port (vault or
// SEP) gives us two real consumers to ground the trait against,
// rather than guessing the abstraction from one impl.
#[async_trait]
pub trait TieredRetrievalSurface {
    /// Per-source briefing — e.g. for conv: one per conv_uuid.
    /// For vault: one per note. For SEP: one per article.
    async fn briefing_for_source(
        &self,
        corpus_id: &str,
        source_doc_id: &str,
    ) -> Option<TieredBriefing>;

    /// Leaf cluster a chunk belongs to (for the cluster-score blend
    /// at retrieval time). Same shape as attached-doc's existing
    /// `SOVEREIGN_DOC_CLUSTER_WEIGHT` path.
    async fn leaf_cluster_for_chunk(
        &self,
        corpus_id: &str,
        chunk_id: u64,
    ) -> Option<LeafCluster>;

    /// Given a hit set, decide which briefings to fetch. Default
    /// impl can call the conv-style top-K-by-hit-count with adaptive
    /// concentration check; overridable for stores that benefit from
    /// a different selection heuristic.
    async fn select_anchors(
        &self,
        hits: &[ScoredChunk],
    ) -> Vec<(String, String)> {
        select_anchors_default_top_k_with_concentration(hits, 0.70, 3, 8)
    }
}
```

**Why the trait extraction is deferred:** the spec history flagged the "extract trait early vs late" trade-off (TIERED_RETRIEVAL.md §"Generic enrichment trait?"). Today we have one consumer — naming the trait now would be guessing. When vault or SEP land their tiered impl, the second consumer will reveal which method signatures genuinely generalise and which were conv-specific accidents. Extract then.

**Adaptive briefing policy (landed 2026-05-22):** the conv-briefing builder uses a two-mode adaptive policy that the future trait's `select_anchors` default would mirror:

- **Deep mode** (top-3 convs, full RAPTOR signposts) — fires when top-3 ≥ 70% of total hit mass. Reader intuition: "user is asking about a focused set of conversations, render those in depth."
- **Shallow mode** (top-8 convs, overview-only) — default fallback. Reader intuition: "broad query, render breadth not depth."

The 70% threshold + (3,8) caps are empirically chosen; revisit when bench data lands.

**Cluster-score blend (deferred):** the spec's "Cluster-score blend" pattern (attached doc's `SOVEREIGN_DOC_CLUSTER_WEIGHT` from `CLUSTER_SCORE_BLEND.md`) hasn't been ported to conv corpora yet. Planned default for conv-tiered: weight=0.25 with NO env var (the spec history settled this — be bolder on new corpora without entrenched baselines). The retrieval-side hookup lands in `runtime::search_corpus_indexes` once T2 entity-graph T2-recall validation gives us a stable baseline to bench the blend against. Current state: briefing-side surfacing lands ahead of blend-side ranking.

## What still belongs to "Phase B v2"

After today's session, the spec's remaining v0 must-land items are:

- **Opt-1 Fast slot routing** for ≤30-chunk convs (provider-side; ~80 LOC InferenceProvider wrapper).
- **T2 entity-graph extraction** per conv → `conv_skeletons.skeleton_json`. Will surface inside the briefing layer as primary-entities-per-cluster (already a field in `ConvRaptorNodeRow` from RAPTOR; T2 augments with cross-cluster entity index for PPR).
- **T3 motif index** → `conv_motifs` table. Briefing layer will pick top distinctive motifs per conv to surface in Deep mode.
- **TextTiling segments** for ≥20-chunk convs → `conv_skeletons.segments_json`. Briefing surfaces these as "this conversation has N phases" markers.
- **Cluster-score blend at retrieval** — see "Retrieval surface" above.
- **`sovereign corpus reenrich --corpus <id>` CLI** for re-runs after recipe change.

## References

- Phase A architecture: `sovereign/docs/TIERED_RETRIEVAL.md`
- Retrieval-side blend the briefing eventually feeds: `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`
- RAPTOR builder (corpus-free): `sovereign/crates/sovereign-tools/src/raptor_atlas.rs:80`
- Recipe gate to extend: `corpus-engine/src/engine/ingest.rs:1510-1511`
- Briefing builder to refactor: `sovereign/crates/sovereign-core/src/runtime.rs:13046`
- Existing schema to mirror: `sovereign/crates/sovereign-store/src/migrations.rs:381` (`run_raptor_atlas_migration`)
- Threaded-turns chunker (already producing the conv-keyed Lance rows this design depends on): `corpus-engine/src/chunking/threaded_turns.rs` (per memory `project_conversation_ingest.md`)
- Memory entries to consult before implementing: `invariant_balanced_envelope_needs_grammar_gate`, `invariant_corpus_id_chunk_id_unique`, `invariant_lint_db_path_canonical`, `project_conversation_ingest`, `project_conversation_bench_v0`, `project_conversation_march_unit4`
