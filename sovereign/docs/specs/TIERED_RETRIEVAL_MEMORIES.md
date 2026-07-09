# Tiered retrieval — the `memories` pool port

**Status:** In implementation (2026-07-08). T1 shipped + probe-verified
(retrieval-equivalent ranks; first-recall backfill 5212ms → 35ms/turn on
the 174-seed bench pool). T3 batch (mem_raptor_nodes + `mem_atlas` +
tier-aware recall + debounced trigger) in flight; incremental tree
(`mem_tree`) and the `--recall-stream` bench follow.

**Implementation divergence from this spec:** T3 persists to a
dedicated `mem_raptor_nodes` table (keyed by `scope =
MemoryScope::atlas_key()`, member ids as memory-id STRINGS, plus an
`embedding_model` staleness column) rather than reusing
`raptor_nodes`/`asset_motifs` with `source_id = "mem:<scope>"` — the
incremental tree needs per-node CF/drift/parent columns that don't
belong on the shared document table, and memories have no u32 chunk
ids. The `mem:<scope>` namespace survives as the scope key.
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
new memories land continuously, and compaction rewrites rows. RAPTOR is a
batch clustering; its own authors note the tree is *sensitive to inserts,
requiring near-full recomputation per change* — i.e. naive cadence is
O(N) LLM calls per new memory, which is unaffordable on a live pool. So
the port's real design work is **not the builders** (those are
corpus-agnostic and reused) — it is an **incremental re-clustering
decision layer**: given one new memory, which of {attach, re-summarize,
split, rebuild} to do, and *when*.

A literature survey (2026-07-08, four parallel research passes: classical
streaming clustering, LLM-agent memory trees, incremental-RAPTOR, and
drift-trigger policy) converged on a clear answer. It is written up here
because it is the load-bearing decision for the whole port.

### Reference architecture: an incremental tree, not periodic re-batch

Two 2024–25 systems already solve "RAPTOR, but streaming" and are the
closest prior art. **Neither re-clusters — they insert.**

- **MemTree** (Rezazadeh et al., ICLR 2025, arXiv 2410.14052) is almost
  exactly our target: a RAPTOR-shaped tree maintained by *top-down
  incremental insertion*. Each new memory descends from the root, at each
  level joining the best-matching child **only if cosine clears a
  depth-adaptive threshold** θ(d) = θ₀·e^(λd) (deeper nodes demand tighter
  matches; paper uses θ₀≈0.4, λ≈0.5). Reaching a leaf splits it into an
  internal node; matching nothing attaches a sibling. Only the
  **root-to-insertion path** is re-summarized (LLM conditioned on
  descendant count → deeper = more abstract), re-embedding just those
  nodes. Cost ≈ O(log N) per insert, ~10 s/insert vs RAPTOR's batch build,
  ~1.4× cumulative LLM cost, retrieval quality approaching offline RAPTOR.
- **adRAP** (adaptive RAPTOR, arXiv 2410.01736) is the RAPTOR-native
  variant and validates the *policy shape* directly: a new item is
  assigned to clusters with GMM posterior γ>0.1; **only nodes whose
  children changed** re-summarize, propagating to ancestors **bounded to
  ~5 levels** (not the root unconditionally); a cluster splits when it
  exceeds τ_c points (paper: τ_c≈11). Update cost is empirically
  independent of N.

**Recommendation: adopt a MemTree-style incremental tree as the memory
pool's T3 builder, distinct from the batch `raptor_atlas.rs` k-means
builder the fixed corpora use.** This is a legitimate per-corpus builder
choice under ARCH_PRINCIPLES §5.4 (stages parameterize on data): the
document/conversation/vault corpora are fixed and batch-built once; the
memory corpus is a stream and gets a streaming builder. The T1/T2 tiers
and the recall-integration seam are unchanged.

### The trigger ladder — four ops, each bounded

Per T3 node, maintain only O(1)–O(k) state, **no point retention**: a
BIRCH **Cluster Feature** CF = (N, LS, SS) (count, linear sum,
sum-of-squares → radius/diameter for free), a running centroid + summary
embedding, and a cheap drift detector over the residual signal
dist(new_leaf, node_centroid). On each insert, evaluate in order and take
the first that fires:

1. **ATTACH** (default, no LLM). Absorb into the nearest child if the
   post-merge radius ≤ threshold T (BIRCH / DenStream ε). Update CF in
   O(1). This is the common path — cheap by construction.
2. **RE-SUMMARIZE this node** (one LLM call). Fire when the summary is
   *stale but the shape is fine*: a **Page-Hinkley or ADWIN** drift alarm
   on the residual, **or** cumulative new mass since last summary
   ΔN/N ≥ 0.3–0.5 (adRAP's child-change gate). Regenerate summary +
   embedding using **MemGPT's recursive operator** — `new_summary =
   LLM(old_summary + changed_children)`, *not* a re-read of all
   descendants — and propagate up with a **depth cap (~5 levels)**. Batch
   the trigger on a **doubling schedule** (re-summarize when N since last
   summary doubles) so each leaf is re-summarized O(log N) times over its
   lifetime, not O(N).
3. **SPLIT** (local, no global re-cluster). Fire when the node is
   over-grown or bimodal: radius/diameter > T, **or** child count > τ_c,
   **or** an incremental cluster-validity index (iDB rising / covariance
   showing two modes) degrades. Split into two (2-means on the CF, or a
   moment-preserving Gaussian split), summarize each, and if fan-out now
   exceeds the branching factor B, merge the two closest *other* children
   (CluStream's "merge the two closest"). Fan-out cap keeps height
   O(log_B N), so splits are self-limiting.
4. **BULK-REBUILD a subtree** (rarest, most expensive). Fire only when
   *many local repairs have accumulated* — a subtree-level validity index
   has degraded past a sustained threshold. Do it **LSM-style**: rebuild
   the **smallest subtree covering the degraded region**, on a geometric
   size-doubling schedule, and **partially** (one slice per trigger, never
   the whole tree at once). By the Bentley–Saxe logarithmic method + LSM
   leveled/partial compaction, each leaf participates in O(log N) rebuilds
   total and no single insert pays a latency spike.

**Why the whole policy stays bounded.** The common path is O(k) with no
LLM. Each of the three expensive ops is gated either by a
bounded-false-positive drift detector (ADWIN/PH) or a hard geometric/size
cap (T, τ_c, doubling), so each leaf triggers each expensive class
O(log N) times over its lifetime → **total LLM work O(N log N)** across
the stream, spread by partial compaction so no insert spikes. (adRAP's
"independent of N" claim is empirical; the formal O(N log N) bound comes
from grafting Bentley–Saxe/LSM scheduling onto it — that graft is the
synthesis this spec proposes.)

### A cost gate *before* the tree: consolidate, don't just append

The cheapest re-clustering is the one you avoid by not inserting a
near-duplicate. Borrow the LLM-memory-system consolidation router
(**Mem0**'s ADD / UPDATE / DELETE / NOOP; **SAGE**'s vMF novelty gate):
before a new memory becomes a leaf, a cheap check against its nearest
existing memory decides ADD (novel → insert), UPDATE (refines an existing
memory → edit in place, no new leaf), or NOOP (redundant → drop). This
bounds pool growth at the source and is independent of the tree
machinery. It also dovetails with the linear rolling-summary we already
have (`MemoryKind::Raw`→`Summary`).

### Compaction interplay (unchanged, but now explicit)

When a `Raw` row is superseded by a `Summary`, its leaf must leave the T3
index (recall already filters `superseded_by IS NULL`); the containing
node's CF is decremented and, if that trips a validity threshold, a
re-summarize (op 2) fires. Supersession is thus just another stream event
into the same trigger ladder.

### Glassbox: the frontier is *when*, so instrument every trigger

The survey's honest gap: the clustering algorithms are decades-mature,
but **"when to re-summarize / rebuild" is past the published frontier** —
the on-target LLM-tree systems (MemTree, adRAP) are recent, not
peer-reviewed, and all of them *freeze the top-level partition* (none
solves true global drift; they rely on the periodic-rebuild escape hatch
in op 4). That is precisely where our transparency principle earns its
keep: **every trigger decision emits a structured trace** — which op
fired, which metric crossed which threshold (CF radius, PH statistic,
ΔN/N, validity index), the descendant count, and the resulting LLM-call
count. The recall bench (`svrn eval inner-chaos --recall`) then reads
those traces to tune the knobs against real retrieval outcomes rather
than guessing. We are not inventing the clustering; we are contributing a
measured, observable *policy* for the one part the literature leaves open.

### Starting knobs (to be tuned on the recall bench, not assumed)

θ₀=0.4, λ=0.5 (MemTree insertion threshold); T from A-BIRCH
auto-estimate or the 90th-percentile intra-cluster distance; Page-Hinkley
δ≈0.002 tuned to ~1 alarm per few-hundred stable inserts; re-summarize
doubling factor 2×; split τ_c ≈ observed mean cluster size (adRAP used
~11); ancestor-propagation depth cap 5; branching factor B from the
existing RAPTOR config.

### Build order

Still T1-first. T1 (persistent embeddings) independently kills the
O(N)-embeddings-per-turn cost and is the floor. **Open question the bench
answers empirically:** does the linear rolling-summary already subsume
enough of T3's value that T1 alone closes the oblique-callback misses?
Build T1, measure on `--recall` (does `plant_rendered_turns` rise, do the
retrieval-miss confabs convert to faithful/partial?), *then* commit to the
incremental T3 tree above. Do not build the trigger ladder before T1's
measurement says the summary-node bridge is actually needed.

**Measured answer (2026-07-08, retrieval probe):** T1 alone does NOT
close the misses (ranks byte-identical to the flat baseline, by
design), and the batch T3 summary bridge does not either — on the two
adversarial plants the best containing-node similarity lands BELOW the
plant's own leaf cosine (grief: node 0.401 vs leaf 0.420 at leaf-rank
42; daughter: node 0.297 vs leaf 0.461 at leaf-rank 7 behind six
same-theme distractors), and the ordering-preserving blend
`max(leaf, α·node + (1−α)·leaf)` cannot lift a memory whose node
underperforms its leaf. Two prompt iterations (period-led →
emotion-led Journal cue) moved node sims but not past the leaf field.
The bridge DOES leave the six already-retrieved plants at TOP-1 with
zero regression, and the tier is fully glassboxed (probe
tier-diagnostics per plant; `SOVEREIGN_MEM_TIER_ALPHA`,
`MEM_LEAF_CLUSTER_TARGET`, the Journal `doc_cue`). The residual
misses are a leaf-precision problem — the promising next levers are a
cross-encoder rerank over the recall top-K (`rerank_batch` already
exists on `InferenceProvider`) or T2 entity-anchored matching, not
more summary tuning.

---

## Per-port acceptance checklist (mirrors Phase-B)

- [x] **Persistence:** `memories.embedding + embedding_model` (T1;
      `run_memory_embedding_migration`) + dedicated `mem_raptor_nodes`
      table keyed by `scope = MemoryScope::atlas_key()` (T3;
      `run_mem_raptor_migration` — see the divergence note above).
      Backfill is lazy-on-read (migrations hold no inference handle);
      supersede cleanup rides `mem_tree::supersede_memory` + the op-4
      rebuild.
- [x] **Trigger:** the knowledge-view debouncer's `MemoryTouched`
      window (`DEBOUNCE_MAX_WRITES`/`IDLE`) drains touched ids through
      `mem_tree::insert_memory` — never the synchronous witness turn.
      Handles installed post-Arc via
      `KnowledgeViewManager::install_memory_atlas` (desktop + server).
- [x] **Recall integration:** `recall_relevant_memories_embed` is
      tier-aware: stored-T1 partition + lazy backfill, then level-0
      node blend `max(leaf, α·node + (1−α)·leaf)`. Witness path
      unchanged.
- [x] **Sequestration test:** `sqlite_mem_raptor_scope_isolation`
      (store_tests) + the wall is upstream by construction —
      `build_memory_atlas` fetches through `get_all_memories_for_scope`,
      whose SQL filter never loads other-scope rows.
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

### Academic sources (incremental re-clustering survey, 2026-07-08)

Reference architecture (incremental RAPTOR-shaped trees):
- MemTree — Rezazadeh et al., ICLR 2025. arXiv:2410.14052.
  Top-down incremental insertion, depth-adaptive threshold, path-only
  re-summarization. The closest architectural match.
- adRAP (adaptive RAPTOR) — arXiv:2410.01736. Child-change-gated
  re-summarize, bounded ancestor propagation (~5 levels), size-cap split.
- RAPTOR — Sarthi et al., ICLR 2024. arXiv:2401.18059. The batch baseline
  (and its own statement of the insert-sensitivity problem).

Trigger / drift-decision layer:
- BIRCH Cluster Feature + threshold T — Zhang, Ramakrishnan, Livny 1996.
  O(1) radius/diameter from (N, LS, SS). A-BIRCH auto-threshold: RG
  308941857.
- ADWIN — Bifet & Gavaldà 2007 (bounded-FP drift, O(log W) memory).
  Page-Hinkley / DDM / EDDM / KSWIN — riverml.xyz drift API.
- Incremental cluster-validity indices (iDB/iSIL/iXB) — Moshtaghi,
  Bezdek, Havens et al. 2019, arXiv:1801.02937. Temporal Silhouette —
  Flores et al. 2023, Springer ML.
- DenStream (Cao et al. 2006), DBSTREAM (Hahsler & Bolaños 2016),
  CluStream (Aggarwal et al. 2003) — attach-vs-spawn radius gate,
  merge-two-closest, two-phase online/offline template. Evolving-GMM
  split/merge — Covões & Hruschka, EVCO 2016.

Amortized rebuild (the escape hatch's cost bound):
- Bentley–Saxe logarithmic method, 1980 — O(log N) rebuilds per element.
- LSM leveled/partial compaction — Sarkar et al., VLDB 2021
  (pvldb vol14 p2216). Spreads rebuild cost, no latency spike.

Cost gates from LLM-memory systems:
- Mem0 (ADD/UPDATE/DELETE/NOOP consolidation router), SAGE (vMF novelty
  gate), Generative Agents (importance-sum reflection trigger), MemGPT
  (recursive summary = LLM(old + new_children)), A-MEM (memory evolution).
- CoverSumm (TMLR 2024, arXiv:2401.08047, centroid-drift-gated refresh),
  EraRAG (arXiv:2506.20963, LSH O(1) routing + split/merge on bucket
  size).

Honest gaps flagged by the survey: the on-target LLM-tree systems are
recent and not peer-reviewed; all freeze the top-level partition (true
global drift is unsolved → op-4 periodic rebuild is the escape hatch);
"when to re-summarize" is past the published frontier — the place this
port's glassbox tracing contributes.
