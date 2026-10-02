# Plan — fix retrieval discrimination (facet #2)

_2026-06-09. The master lever. Surfacing the genuinely-supporting chunk reliably is
**one root, three payoffs**: lifts competence (grounded answers), citation_fidelity,
AND unlocks the grounding-verifier (present `violation_prob` drops below adjacent →
both chaos gates pass). Self-contained for a fresh session post-compaction._

---
## ⚑ PHASE 0 EXECUTED — premise corrected (2026-06-09)

Phase 0 did its job: it **refuted the orthogonality hypothesis before any code was
built on it.** Direct measurement against the same daemon embed endpoint the bench
uses (`target/ci-bench/cosine_probe.py`, `anisotropy_probe.py`):

- The embedding model **discriminates correctly**. For "what weapon does Winnie kill
  Verloc with" vs its true supporting passage vs an unrelated passage:
  raw cosine **support 0.622 / unrelated 0.443 → margin +0.179**.
- The **"~0.03" was never a cosine.** In hybrid (vector+FTS) mode the score field is
  LanceDB's **RRF `_relevance_score` ≈ 0.016–0.03 by construction** (`search.rs:345`
  says so explicitly). Last session read the RRF fusion score as if it were a cosine.
  The real cosine lives in the separate `vector_distance` field (`search.rs:317`).

So the corpus isn't un-embeddable — it's **anisotropic** (last-token-pooled LLM
embeddings cluster in a narrow cone): avg pairwise cosine among *random* chunks = 0.36,
corpus mean-embedding norm = 0.617 (a large common-mode component). The relevant
signal is a real but thin margin riding on a ~0.5 pedestal.

**Two measured levers** (replace the old Phase-1 branches):

| lever | support | unrelated | margin | note |
|---|---|---|---|---|
| raw (today's bench path) | 0.622 | 0.443 | +0.179 | baseline |
| **mean-centering** (subtract corpus mean) | 0.356 | **0.024** | **+0.333** | ~2× contrast; unrelated → orthogonal |
| **`Instruct:` query prefix** | 0.692 | 0.489 | +0.203 | currently DROPPED in remote path |
| **both** | 0.449 | **0.059** | **+0.391** | best |

1. **Query-prefix is silently dropped in the remote/daemon path.** `embed_query` is a
   trait default that just calls `embed()`; neither `SplitProvider` nor
   `RemoteApiProvider` overrides it, and the daemon's `/v1/embeddings` HTTP endpoint
   has no query/doc distinction. The `Instruct:` prefix is applied ONLY in the embedded
   llama-cpp path (`embed_query_sync`). Fix is isolated to the remote provider.
2. **Anisotropy / mean-centering** is the high-impact lever. `search_with_rerank`
   ALREADY over-fetches → re-ranks (`search.rs:432`), so mean-centered re-ranking slots
   in with **no re-index**. Store the corpus mean in `IndexMeta` (reuse
   `clustering.rs:265 mean_embedding`); subtract it from query + candidate embeddings
   before the `vector_distance` cosine. Blast radius = shared search path → validate
   wiki/SEP don't regress (mean-centering is a no-op on isotropic corpora, so low risk).

Original hypothesis-driven plan preserved below for the reasoning trail; Phases 2–4
(reach/truncation, recall@K metric, downstream re-validation) still stand.

---
## ⚑ EXECUTED — full findings (2026-06-09)

### Cross-corpus control: the system is healthy, chaos isn't anomalous
Read the STORED chunk embeddings (the exact vectors retrieval scores against) from
each LanceDB (`target/ci-bench/corpus_homogeneity.py`):

| corpus | rows | avg pairwise cosine | mean-norm | neighbor separation |
|---|---|---|---|---|
| chaos-secret-agent | 316 | 0.564 | 0.754 | **0.246** |
| **sep** (battle-tested) | 187,967 | **0.686** | **0.830** | 0.143 |
| wikipedia (battle-tested) | 1,507,691 | 0.387 | 0.628 | 0.217 |
| enron | 8,829 | 0.270 | 0.529 | 0.227 |

**SEP is MORE anisotropic than chaos yet retrieves great** (61%→77–94% source recall),
and chaos has the BEST neighbor-separation of the four. So "anisotropy → broken
retrieval" is false; cosine handles high-pedestal corpora fine. **Mean-centering the
shared path is NOT warranted** — dropped. (Also reinforced by `RERANK_EXPERIMENT.md`:
every retrieval intervention here is per-corpus + env-gated; "no globally-better picker
exists." A global cosine change is the opposite of that lesson.) The chaos corpus is
just a single literary doc on a flat SEP-shaped recipe (`extract=plaintext`,
`chunk=paragraph`, no enrichment) — set up to be retrieval-hard, with none of the
RAPTOR/GLiNER literary machinery.

### Query-prefix fix (shipped) — a correctness fix, NEUTRAL on quality
The `Instruct:` query prefix was dropped in the remote/daemon path (`embed_query` trait
default → `embed()`; only the embedded `embed_query_sync` applied it). Fixed in
`RemoteApiProvider`/`SplitInferenceProvider` (self-resolves via
`ModelsManifest::embed_query_instruction` with a renamed-GGUF fallback + unit test;
`SOVEREIGN_DISABLE_QUERY_PREFIX` escape hatch). Clean same-binary, same-limit A/B:

| bank | sources OFF→ON | facts OFF→ON |
|---|---|---|
| SEP (limit 30) | 54→**55**/66 (+1) | 149→149/159 (0) |
| Wikipedia (limit 30) | 38→37/58 (−1) | 109→108/130 (−1) |

Both within single-item run-noise. It's a **correctness/bench-fidelity fix** (remote now
matches embedded + the model card), **not** a retrieval-quality lever. No regression.

### Phase 2 — synthesis-window reach (the real grounding-verifier coupling)
chaos chunks ~2014 chars; KQ window = `MAX_KNOWLEDGE_CHARS` 8000 (EXPANDED 16000) →
only **~4 leaf chunks** of the 20 retrieved reach synthesis. Measured (`eval --inspect`
on chaos present-questions): retrieval finds the gold fact in **9/10**; for SIMPLE
factual questions the gold chunk ranks 1–4 → reaches synthesis (well-served). For the
**MAXIMAL exhaustive-essay** questions ("section-by-section account of the whole X"),
the gold keyword is rank 1 but the answer spans the whole novel while only ~4 leaf
chunks reach the model → the model fills from parametric memory → the grounding-verifier
correctly flags it. **That coupling is a window/scope problem, not discrimination.**

### RAPTOR retrofit (the Phase-2-motivated fix) — `sovereign enrich raptor`
A single RAPTOR summary node compresses a whole arc ("the Greenwich bombing plot") into
one chunk that fits the 8000-char window → an exhaustive essay becomes groundable. This
is why RAPTOR is "meant for literary retrieval." Retrofitted chaos-secret-agent additively
(`enrich raptor chaos-secret-agent --doc-type narrative` → 19 summary nodes, 175s;
collapsed-tree grounding is `SOVEREIGN_RAPTOR_GROUNDING` default-ON).

**Result — RAPTOR works but does NOT fix the gap; the bottleneck is the SYNTHESIS layer.**
- Grounding fires (`RUST_LOG` → `raptor-grounding: collapsed-tree summaries injected
  added=8 via_index=1`); summaries are `reserve_raptor_chunks` front-loaded + given slots
  on top of the leaf budget (`truncate(KQ_MERGED_LIMIT + raptor_n)`), so they reach the
  prompt.
- The summaries are ACCURATE whole-arc summaries (e.g. *"Vladimir coerces Verloc into
  orchestrating an anarchist bombing… his murder by Winnie… Ossipon betrays her"*).
- DESPITE accurate, front-loaded grounding, the maximal-essay answer is a **reasoning
  leak** that says verbatim: *"I need to use my general knowledge (PARAMETRIC) since this
  is a literary work and the retrieved passages only contain fragments"* — then
  hallucinates WRONG parametric facts (**axe** not knife, **son** not brother, **embassy**
  not Greenwich Observatory).

So on "exhaustive/section-by-section" asks the 35B **dismisses accurate grounding and
reaches for parametric memory**, leaking its reasoning (0 reasoning chars = no `<think>`).
The grounding-verifier's prior present-as-ungrounded flags were **correct**. The real
lever is **synthesis output-discipline + a grounded-only scoping prompt for exhaustive
asks**, NOT retrieval (facet #2 refuted) and NOT RAPTOR availability. Simple factual
present-questions are already well-served (Phase 2). RAPTOR is left ON the canonical
corpus (reversible via `SOVEREIGN_RAPTOR_GROUNDING=0` / deleting `raptor_summaries.lance`
+ `conv_raptor_nodes`) — **flag for the CI-gate baseline decision**.

---
## The symptom (ORIGINAL HYPOTHESIS — superseded by Phase 0 above)

On `chaos-secret-agent` (single doc, 316 chunks) the sealed retrieval returns
**uniform ~0.03 cosine** across the top chunks (good matches are 0.5–0.8; 0.03 is
near-orthogonal). Effects: ranking is a coin-flip among near-ties; only FTS keyword
overlap does real work; tiny MoE/Metal perturbations reorder ties run-to-run →
citation_fidelity swings 0.25↔1.00; supporting chunks for *present* facts often
never reach synthesis (so answers are parametric-correct, which the grounding-verifier
then correctly-but-unhelpfully gates). The facts DO exist in-corpus (`Winnie` 92×,
`Stevie` 173× in source) and ARE FTS-retrievable (`eval --inspect` showed facts 4/4) —
the vector half is what's broken.

## Phase 0 — Diagnose the ~0.03 (cheap, decisive; do FIRST)

Why are all cosines ~0.03? Rank the hypotheses and test directly before fixing:

1. **Query/passage prompt-template mismatch (PRIME SUSPECT).** Qwen-embedding models
   expect an instruction prefix for *queries* (`Instruct: <task>\nQuery: <q>`) while
   *passages* are embedded raw. If query and passage are embedded with mismatched
   templates (or both raw), cosines collapse toward a uniform low value — exactly this
   signature. **Check `EmbedFn` / the embed call site:** is there an asymmetric
   query-vs-passage prefix, and does it match what the model card prescribes?
2. **Missing L2 normalization** before cosine (uniform tiny scores).
3. **Weak model on literary prose** (qwen-embedding-0.6b) — only if 1 & 2 are clean.
4. **Chunk dilution** — 1024-char chunks mixing many topics flatten the embedding.

**Decisive test (one script, no daemon):** embed a query (e.g. "what weapon does Winnie
kill Verloc with") and a known-supporting chunk (the carving-knife passage), compute
raw cosine. If a *clearly-matching* pair scores ~0.03 → it's a bug (1 or 2); fix is
cheap + huge. If the matching pair scores ~0.6 but corpus-wide top-K is ~0.03 → it's
content/chunking (3/4). Repeat with the query-prefix toggled to isolate hypothesis 1.

## Phase 1 — Fix (branch on Phase-0 result)

- **Template/normalization bug (1/2):** fix the asymmetric query prefix + ensure L2
  norm. Expected: cosines become discriminating → supporting chunks rank top → the
  win cascades to all three payoffs. Re-validate that *other* corpora (wiki, SEP)
  don't regress (they may have been silently relying on FTS too).
- **Vector genuinely weak (3):** raise FTS weight in the hybrid fusion (the eval probe
  proves FTS finds the quotes). Make the vec/FTS blend adaptive — when vector scores
  are low-variance (near-uniform, as here), lean on FTS. A per-corpus or per-query
  variance gate.
- **Chunking (4):** smaller / sentence-window / overlapping chunks for dense-fact
  corpora; re-index chaos-secret-agent and re-measure.

## Phase 2 — Reach & truncation (independent of Phase 0)

The KnowledgeQuery path retrieves `KQ_PER_CORPUS_LIMIT=20`/corpus then truncates to
`MAX_KNOWLEDGE_CHARS`. A supporting chunk can be *retrieved-but-truncated* before
synthesis. Audit: does the supporting chunk survive the merge → neighbour-window →
truncate pipeline? Raise the limit / re-rank so supporting chunks aren't dropped.
(`run_live`'s `retrieved_chunk_texts` = the post-truncation set the verifier also sees,
so this directly affects the verifier too.)

## Phase 3 — Measure with a CLEAN metric (not the noisy chaos sub-metrics)

Build **retrieval recall@K**: over a bank of (question → verbatim supporting_quote)
pairs, the fraction where the supporting quote appears in the top-K sealed-retrieved
chunks. Deterministic (temp 0 retrieval), not LLM-judged — the right surface to tune
fusion/limit/chunking against. Seed from the 4 provenance + the 17 present chaos
questions (their gold facts). Tool already exists: `eval run --bank <b> --inspect
--limit K` (sealed). Target: recall@20 from today's ~partial → ≥0.9.

## Phase 4 — Re-validate the downstream payoffs + tiers

Once recall@K is high: re-run chaos (competence + citation should rise) and re-run the
grounding-verifier (present `violation_prob` should drop below adjacent → a threshold
now separates them → BOTH gates pass). Confirm on **4B + 35B** — the situated-harness
thesis predicts the retrieval fix lifts both tiers to the same (now-higher) ceiling.

## Order of operations

Phase 0 first (it's an hour and may be a one-line fix with outsized payoff). Only if
Phase 0 says "no bug, vector genuinely weak" do Phases 1-vector/2/chunking. Keep the
grounding-verifier OFF until recall@K is fixed — then re-open it (the FTS-grounded
variant becomes unnecessary if retrieval itself surfaces the support).

## Pointers

- Diagnostic harness: `eval run --inspect` (sealed); the provenance probe bank at
  `target/ci-bench/provenance_retrieval_bank.toml`.
- Embed wiring: `EmbedFn` (search `code_search "EmbedFn"`); embed call in
  `chat_cmd/bootstrap.rs`; `qwen-embedding-0.6b`.
- Retrieval path: `KQ_PER_CORPUS_LIMIT`, `MAX_KNOWLEDGE_CHARS`, neighbour-window in
  `sovereign-core/src/runtime.rs`; hybrid fusion in `corpus-engine` index/search.
- Context: `SITUATED_HARNESS_STUDY.md` (+ its grounding-verifier addendum), and the
  `chaos citation_fidelity ... measurement-bounded` sovereign note.
