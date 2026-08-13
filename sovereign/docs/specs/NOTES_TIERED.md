# Spec: Tiered retrieval over NoteStore

**Status:** In flight (design only).
**Targets:** Reducing false-negatives in NoteStore retrieval beyond
what the 2026-05-25 SQL filter pushdown already fixed
(`corpus-engine-notes::read_notes_scoped`). Plan reduces semantic
recall failures — synonyms, stem variants, paraphrased queries —
that BM25 / FTS5 cannot catch.

**Lifecycle:** Each tier ships independently. T1 alone is expected
to absorb 60-70% of remaining false negatives; T2 and T3 only land
if T1 measurements justify them. Runtime surface eventually lands
in `docs/inference.md` (or a new `docs/notes-retrieval.md`); this
spec retires when the highest-tier shipped row is `Shipped`.

**Prerequisites for reading:**
- [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md) — Phase A
  architecture this is the Phase B port of, applied to a non-
  document corpus.
- [`PROGRESSIVE_ENRICHMENT.md`](PROGRESSIVE_ENRICHMENT.md) —
  RAPTOR + GLiNER layered shape; prescriptive for any tiered port.
- [`../../ARCH_PRINCIPLES.md`](../../ARCH_PRINCIPLES.md) §5.4 —
  pipeline stages parameterize on data, not source identity.

---

## Problem (measured 2026-05-25)

`sovereign tools call notes --query=…` is FTS5-only. The audit
ran 8 realistic queries against 38 notes written in one session
and surfaced:

| Query | Hit | Note exists? |
|---|---|---|
| `"EOS bypass"` | pointer note only | yes — invariant note with "EOS/EOG" + "force_continue" missed |
| `"why is dedup off for wikipedia"` | 0 | yes — but content uses "wiki" not "wikipedia" |
| `"wiki dedup regression"` | 0 | yes — content says "wiki regresses" |
| `"empty bytes token EOS bypass"` | 0 | yes — content has "empty-bytes" with hyphen |
| `"regress"` | 0 | "regresses" → 1 hit (no stemming) |
| `"tokenize"` | 0 | "tokenizer" → 2 hits |
| `"speculative decoding"` | 2 / 5 SD notes | partial (verbatim phrase only matched some) |
| `"reranker"` | reasonable | works for exact-token recall |

Underlying causes:

1. **FTS5 default tokenizer** has no stemming and no synonym
   expansion. "wiki" and "wikipedia" are distinct lexemes;
   "regress" doesn't match "regresses".
2. **No semantic recall.** Concepts the note covers but doesn't
   name verbatim (e.g. an EOS-bypass invariant that uses
   "force_continue" + "empty-bytes" not "bypass") are unreachable
   by token-level FTS.
3. **No related-note traversal.** Asking about
   `UrlAllowlistConstraint` doesn't surface the
   `build_self_manifest` fast-slot mesh-alias note that the
   same session of work produced, even though the work is
   thematically connected.

Tiered retrieval (Phase A + B) was designed for exactly these
shapes against documents. NoteStore is a near-perfect fit: small
records, structured already, queried over years, "find related
decisions when I touch this file" is the canonical workload.

---

## Architecture — three tiers, additive

Mirror the [`TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md) tier
contract. Builders stay corpus-free per
[`ARCH_PRINCIPLES.md §5.4`](../../ARCH_PRINCIPLES.md).

| Tier | Available when | Retrieval mode | Backing data |
|---|---|---|---|
| **T1 — embeddings** | One Embed call per note write done | FTS5 BM25 + cosine top-K hybrid blend over note content | New `note_embeddings` sidecar (one row per note, `Vec<f32>`) |
| **T2 — entity-graph** | GLiNER over content + per-note action atoms done | T1 + PPR over note co-occurrence graph (entities + symbols + files) | New `note_entities` table; reuse `chunk_entities` shape |
| **T3 — RAPTOR over notes** | K-means cluster note embeddings → cluster summaries | T2 + RAPTOR signpost briefing ("decisions about reranking" / "URL constraint invariants" clusters) | Reuse `raptor_nodes` with a `source_namespace = "notes"` row |

State machine reuses `AssetState` semantics — `Pending →
Indexing → PartiallyReady → BuildingSkeleton → MultiHopReady →
BuildingSkeleton → Ready`. Per-tier emptiness checks gate the
briefing additively; no caller branching on tier state.

---

## Per-tier acceptance

### T1 — embeddings + cosine recall

**Persistence:**

- New table `note_embeddings(note_id TEXT PRIMARY KEY REFERENCES
  notes(id) ON DELETE CASCADE, embedding BLOB NOT NULL, model_id
  TEXT NOT NULL, created_at INTEGER NOT NULL)`.
- Schema migration adds the table + populates lazily on first
  write/read; bulk backfill is the operator's call (`sovereign
  notes reindex` subcommand TBD).

**Embedding injection:**

- NoteStore gains `with_embed_fn(EmbedFn)` builder method (clean
  no-cycle: NoteStore stays dep-free; caller injects). Per
  `ARCH_PRINCIPLES §5.4` the builder signature stays primitive —
  takes a closure, not a corpus handle.
- On write_note, if embed_fn is set, compute embedding +
  persist synchronously; otherwise skip (T1 disabled for this
  store). Failures soft-fail with `warn!` per ARCH §9 — note still
  persists, embedding-less.

**One model space per store** (added 2026-08-13, order
`mesh-scale-t1-notes`):

- `model_id` is not decoration. Cosine between two model spaces is not
  a weak signal, it is not a signal, so the cosine read admits only
  rows stamped with **this node's** embed model id
  (`local_embed_model_id()` — the one accessor for "which space is
  local", read by the write path, the backfill, the remote ingest, and
  the cosine read alike).
- Consequently the gossip wire carries no vectors. A remote note's
  content is re-embedded here, at ingest, through this node's own
  `embed_fn`; a sender's vector is discarded even when the sender
  labels it with our model id, because `model_id` is a field the
  sender supplies.
- If the local embed hook is unavailable, the remote note is stored
  with no `note_embeddings` row — readable by keyword, absent from the
  cosine pool — until the tier backfill embeds it. Never blended
  unembedded, never dropped.
- Rows written before this change may still carry a foreign
  `model_id`; the read filter is what covers them, and it is not
  redundant with the ingest change (one covers the past, one the
  future).

**Retrieval:**

- `read_notes_scoped` gains an optional `semantic_query: Option<&str>`
  param. When set + embed_fn available, compute query embedding,
  cosine top-K over `note_embeddings` **in the local model space**,
  blend with FTS5 BM25 rank
  via min-max normalisation (mirror the cluster-score-blend
  pattern in [`CLUSTER_SCORE_BLEND.md`](CLUSTER_SCORE_BLEND.md)).
- Default `embed_weight = 0.5` — operator-tunable via env var
  `SOVEREIGN_NOTES_EMBED_WEIGHT`.
- Tracing event `notes: semantic blend applied` per ARCH §9.

**Acceptance:**

- [ ] Schema migration shipped + idempotent.
- [ ] EmbedFn injection point on NoteStore.
- [ ] Re-run the 8 audit queries against the 38 May-25 notes
      with semantic on; target ≥6/8 hit (vs 3/8 today).
- [ ] Latency: per-query semantic blend ≤ 50ms on 10k-note store
      against local Embed slot.
- [ ] Storage budget: ~3-4 KB per note (768-dim fp32 + metadata);
      10k notes = ~35 MB. Acceptable.
- [ ] Regression test in `corpus-engine-notes`:
      `semantic_query_finds_paraphrased_note` — write note saying
      "wiki regresses under dedup"; query "wikipedia regression
      from deduplication"; assert hit.

**Why this tier first:** single highest-leverage win. Solves
synonyms / stems / paraphrases in one schema migration + ~150 LOC.
The FTS5-Porter-stemmer alternative (smaller migration) would
half-fix the problem; embedding-blend covers it cleanly.

### T2 — GLiNER entities + PPR

**Persistence:**

- New table `note_entities(note_id TEXT, entity TEXT, kind TEXT,
  PRIMARY KEY (note_id, entity, kind), FOREIGN KEY (note_id)
  REFERENCES notes(id) ON DELETE CASCADE)`.
- Reuse `chunk_entities` row shape from the tiered-retrieval port
  so the PPR algorithm in `sovereign-tools::entity_graph` works
  unchanged.

**Extraction:**

- On write_note, if GLiNER + Fast-slot available, extract
  entities from content + augment author-supplied `symbols` +
  `files` with extracted ones. Author-supplied wins on overlap.

**Retrieval:**

- New `read_notes_related(symbol_or_file: &str, k: usize)` —
  seeded PPR from the symbol's notes, returns top-K by
  diffusion score.
- Composes with T1: PPR finds related notes, T1 ranks them.

**Acceptance:**

- [ ] GLiNER feature-flagged behind the existing `gliner-ner`
      cargo feature (already in tree).
- [ ] Per-write extraction adds <100ms p95.
- [ ] Audit-query rerun: `read_notes_related("UrlAllowlistConstraint",
      10)` returns the fast-slot mesh-alias note + the
      EOS-bypass invariant note + the accept-every-token
      invariant note + the constraint-exclusivity decision note
      (all four are thematically connected; none mention each
      other by symbol today).

### T3 — RAPTOR over notes

**Persistence:**

- Reuse `raptor_nodes` table with `source_namespace =
  "notes:<scope>"` (e.g. `notes:global`, `notes:feature:<id>`).
- One RAPTOR tree per scope; cross-scope queries union the
  per-scope tops.

**Build:**

- K-means cluster note embeddings into ~20-50 leaf clusters
  (depending on note count). Per leaf: 1 Slow LLM summary call
  identifying primary topic + primary entities. Recurse.
- Trigger: scheduled rebuild on a watermark (every N new notes
  written, or once per day) — NOT per-write (too expensive).

**Retrieval:**

- New `read_notes_landscape(query: &str) -> Vec<RaptorSignpost>` —
  walk the tree, return signposts most relevant to the query.
- Briefing-side: when a model asks "what decisions exist about
  topic X", the runtime can splice the relevant RAPTOR cluster
  summaries into context before retrieving individual notes.

**Acceptance:**

- [ ] Rebuild scheduler integrated with the existing watcher
      infrastructure (not new cron).
- [ ] Cluster summaries readable by `sovereign notes landscape`.
- [ ] At least one bench fixture: ask vague "what did we decide
      about retrieval" against the 38 May-25 notes; landscape
      surfaces the reranker + cluster-blend + tiered-port
      clusters distinctly.

---

## Decision: where the code lives

Two reasonable shapes:

**(a) Inject EmbedFn at NoteStore construction** — keeps
`corpus-engine-notes` dep-free. T1 ships inside this crate;
embedding is just another column. Recommended for T1.

**(b) New `notes-tiered` crate** depending on `corpus-engine-notes`
+ `corpus-engine` + `sovereign-tools::raptor_atlas` +
`sovereign-tools::entity_graph`. Heavier separation, but T2 + T3
pull in `EntityGraph`, `RaptorAtlas`, `gliner-ner` feature —
all of which would otherwise force `corpus-engine-notes` to grow
a `corpus-engine` dep. **Recommended when T2 lands.**

Sequencing: ship T1 inside `corpus-engine-notes` (path (a)).
When T2 lands, extract the T1 code into `notes-tiered` along
with T2; keep `corpus-engine-notes` minimal.

---

## What this spec does NOT do

- **Replace SQL filter pushdown.** That fix already shipped 2026-
  05-25 in `corpus-engine-notes::read_notes_scoped`. T1 is for
  the residual semantic-recall class.
- **Replace FTS5.** Hybrid blend; FTS5 stays the keyword path.
- **Add stemming via Porter tokenizer.** Embedding cosine
  obviates the half-measure. If T1 ships and false-negatives
  persist on truly exact-keyword queries, revisit.
- **Promise cross-session entity graph.** Per-note entities yes;
  cross-session canonical-entity resolution is its own surface
  (see `sovereign-tools::atlas_*`).

---

## References

- 2026-05-25 audit transcript (this session) — 8 queries, 3/8
  hit. Motivating dataset.
- [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md) — Phase A
  architecture this is the port of.
- [`TIERED_RETRIEVAL_PHASE_B.md`](TIERED_RETRIEVAL_PHASE_B.md) —
  per-corpus port matrix; NoteStore row will land there when T1
  ships.
- [`CLUSTER_SCORE_BLEND.md`](CLUSTER_SCORE_BLEND.md) — blend
  pattern T1 should mirror (min-max normalisation across the
  candidate pool, default weight = 0.0 = byte-identical
  baseline).
- `corpus-engine-notes/src/notes.rs::read_notes_scoped` — SQL
  filter pushdown (the 2026-05-25 fix that complements this).
