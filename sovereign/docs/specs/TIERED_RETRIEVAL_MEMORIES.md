# Tiered retrieval — the `memories` pool port

**Status:** Proposed (2026-07-08). Not started.
**Owner doc:** rows here promote into
[`TIERED_RETRIEVAL_PHASE_B.md`](./TIERED_RETRIEVAL_PHASE_B.md)'s port
matrix once accepted; on ship, into
[`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md).

This spec proposes porting the tiered-retrieval architecture (T1
embeddings → T2 entity graph → T3 RAPTOR) to **the memory-recall pool**
— the `memories` table the inner-work witness and general working-memory
recall from. It is the one user-facing retrieval surface that was never
ported: the Phase-B matrix has rows for documents, conversation
*transcripts*, and vaults, but **no `memories` row**.

---

## Why

The memory-recall pool is **flat for everyone** (verified 2026-07-08,
see NoteStore `reference-memory-recall-is-flat-not-raptor`):

- Witness recall is `runtime/turn.rs` step 1a →
  `memory.rs::recall_relevant_memories_embed`, which fetches all
  in-scope memories and **re-embeds every row's raw content fresh, every
  turn** (`inference.embed_batch`), cosines, decay-filters, takes top-5.
  There is **no `embedding` column** on `memories` (schema
  `sovereign-store/src/migrations.rs:110`) — no persistent index, no
  tier structure.
- The only hierarchy is a *linear* rolling summary
  (`MemoryKind::Raw`→`Summary`, `superseded_by`) — not multi-scale.
- "Conversation enrichment" (Phase-B conversations, shipped 2026-05-23)
  enriches the conversation **transcript** (`messages` + embeddings),
  retrieved by the DeepQuery/knowledge path — NOT the extracted memory
  pool the witness reads.

Two concrete costs this imposes, both measured on the inner-work recall
bench (`bench/inner_work`, `svrn eval inner-chaos --recall`):

1. **Retrieval misses on oblique callbacks are the remaining
   confabulation source.** After the synthesis-side grounding verifier
   landed (confab 56%→~12%), the residual confabulations are
   retrieval-miss driven: e.g. the callback *"that night in the spring"*
   never retrieves the grief memory because flat cosine over a single
   raw entry can't bridge vocabulary-disjoint phrasing. A RAPTOR summary
   node — *"ongoing grief for their father, hardest through spring"* —
   embeds far closer. This is the lever for the ~19–25% faithful-recall
   ceiling.
2. **O(N)-embeddings-per-turn.** Re-embedding the whole scoped pool on
   every witness turn does not scale to a real user with thousands of
   entries. A persistent T1 index (the floor of this port) removes it.

---

## What to enrich, and the sequestration guarantee

**Treat the SCOPED memory pool as a corpus.** The sequestration the
inner-work surface needs already exists at the data layer and becomes
the corpus boundary:

- `Memory.source_skill_id` + `MemoryScope::Scoped(<skill>)` is a
  bidirectional wall (`sovereign-contracts/src/traits.rs`): scoped pools
  never recall outside their scope; general recall never sees scoped
  memories. **One corpus per scope** — `Scoped("inner-work")` is its own
  corpus; `General` is another. Enrichment runs per-scope; a RAPTOR node
  only ever summarizes memories from within one scope.
- The `raptor_nodes` / `asset_motifs` tables already carry a `source_id`
  column (Phase-B conversation port added it). Namespace: `source_id =
  "mem:<scope>"` (`mem:inner-work`, `mem:general`), collision-free with
  document/conversation/vault ids.

This is why the port is **safe by construction**: the register-closure
that severed the witness from corpus/DeepQuery retrieval existed to stop
cross-corpus and encyclopedic *leakage* (a foreign corpus's chunk
surfacing in a witness reply). A RAPTOR index built **over the user's
own scoped memories** contains nothing foreign — there is no leak
surface. Combined with the already-shipped memory-grounding verifier
(`runtime/memory_grounding.rs`, which stops reciting raw content /
confabulating), the port gives better retrieval *without* reopening the
safety posture that the inner-chaos loop converged
([`../../bench/inner_work/CHAOS_HARNESS.md`](../../bench/inner_work/CHAOS_HARNESS.md)
§7).

---

## Tier plan

The builders are corpus-agnostic by signature — `(chunks: &[TextChunk],
embeddings: &[Vec<f32>], inference, store)` (ARCH_PRINCIPLES §5.4). A
`Memory` maps to a "chunk" (its `content` is the text; its `id` the
chunk id). Nothing in the builders needs rewriting (per Phase-B "What
Phase B does NOT need to redo").

| Tier | What it adds for the memory pool | Notes |
|---|---|---|
| **T1 — persistent embeddings** | An `embedding` column (or sidecar table) on `memories`, populated at write/compaction time. Recall reads the index instead of re-embedding the whole pool per turn. | The floor; independently fixes the O(N)-per-turn cost. Backfill migration for existing rows. |
| **T2 — entity graph (optional)** | Entity co-occurrence + PPR over the scoped pool (people, places, recurring themes). Multi-hop recall ("the friend I mentioned around the job decision"). | Lower priority — the recall bench's misses are vocabulary-bridge, not multi-hop, so evaluate before investing. |
| **T3 — RAPTOR over memory clusters** | K-means cluster the memory embeddings, LLM-summarize each cluster into a `raptor_nodes` row, recurse. Recall matches the query against **summary-node embeddings** as well as leaf memories; a matched summary node expands to its member memories. | The headline lever for oblique-callback retrieval. Same `raptor_atlas.rs` builder. |

**Recall integration.** `recall_relevant_memories_embed` becomes
tier-aware (mirroring `attached_document_search.rs`): cosine over leaf
memories (T1) always; when `raptor_nodes` for the scope are non-empty
(T3 done), blend in summary-node similarity so a query that matches a
cluster summary surfaces that cluster's leaves. The witness keeps
reading `context.memories` — it just receives better-chosen rows; no
witness-path change, no register-closure change.

---

## The hard part: incremental, not batch

The document/conversation/vault ports enrich a **fixed** corpus once (on
attach / seal). The memory pool is an **ever-growing, per-user stream** —
new memories land continuously, and compaction rewrites rows. RAPTOR is
a batch clustering. So the port's real design work is **incremental
re-clustering**, not the builders:

- **Trigger.** Debounced re-enrich on memory write / end-of-conversation
  extraction / compaction — reuse the KnowledgeView debouncer pattern
  (`sovereign-tools/src/knowledge_view/debouncer.rs`) rather than
  enriching synchronously on the witness turn (which must stay fast).
- **Incrementality.** New leaves attach to the nearest existing cluster;
  a full re-cluster fires only when drift crosses a threshold (cluster
  cohesion drops, or N new leaves since last rebuild). The vault port's
  `reindex_changed_sources_tiered` incremental sweeper is the closest
  precedent.
- **Compaction interplay.** When a `Raw` row is superseded by a
  `Summary`, its leaf must leave the index (recall already filters
  `superseded_by IS NULL`); the RAPTOR node's member set updates.

Open question for the design phase: does the linear rolling-summary
(`MemoryKind::Summary`) subsume enough of T3's value that only T1 is
worth building first? The recall bench answers this empirically — build
T1, measure, then decide on T3.

---

## Per-port acceptance checklist (mirrors Phase-B)

- [ ] **Persistence:** `memories.embedding` (T1) + reuse
      `raptor_nodes`/`asset_motifs` with `source_id = "mem:<scope>"`
      (T3). Migration + backfill in `sovereign-store/src/migrations.rs`.
      `ON DELETE`/supersede cleanup wired.
- [ ] **Trigger:** debounced re-enrich off the memory-write / compaction
      path — NOT on the synchronous witness turn.
- [ ] **Recall integration:** `recall_relevant_memories_embed`
      tier-aware, scope = corpus boundary. Witness path unchanged.
- [ ] **Sequestration test:** an enrichment run over `Scoped("inner-work")`
      never reads or summarizes a `General` (or other-scope) memory, and
      vice-versa. Assert at the store layer, like the existing
      `MemoryScope` wall tests.
- [ ] **Bench:** the recall harness IS the bench. Acceptance = on
      `svrn eval inner-chaos --recall`, the confab present/absent split
      shifts (retrieval-miss confabs → faithful/partial) and retrieval
      coverage (`plant_rendered_turns`) rises, with confab not
      regressing and safety at 100%. `--recall-probe` shows the
      currently-missed plants (`grief_father_march`,
      `daughter_first_steps_april`) entering top-K.
- [ ] **Safety:** re-run `svrn eval inner-chaos` (core safety loop) —
      no regression from any recall-path change. The grounding verifier
      stays in place.
- [ ] **NoteStore + doc:** promote the row into the Phase-B matrix; on
      ship, into `TIERED_RETRIEVAL.md` with a storage-shape row.

---

## References

- [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md) — architecture,
  corpus-agnostic builders, storage shape.
- [`TIERED_RETRIEVAL_PHASE_B.md`](./TIERED_RETRIEVAL_PHASE_B.md) — the
  port matrix this row belongs in.
- [`../../../corpus-engine/ENRICHMENT.md`](../../../corpus-engine/ENRICHMENT.md)
  — three enrichment systems; System 3 (tiered) is the one ported here.
- [`../../bench/inner_work/CHAOS_HARNESS.md`](../../bench/inner_work/CHAOS_HARNESS.md)
  §7 — the recall bench + the synthesis-side grounding verifier this
  port complements.
- `sovereign-core/src/runtime/memory_grounding.rs` — the shipped
  confabulation verifier (synthesis axis); this port is the retrieval
  axis.
- NoteStore: `reference-memory-recall-is-flat-not-raptor`,
  `project-inner-chaos-harness-built-2026-07-08`.
