# Enrichment 2026 — frontier review + best-in-class roadmap

_Status: research roadmap (intent per `ARCH_PRINCIPLES.md §1.2`, not a
§1.1 contract). Written 2026-07-29 from a full audit of the four
enrichment systems (code, docs, notes store, committed baselines) plus a
sweep of the 2025-26 literature. Companion to
[`ENRICHMENT.md`](./ENRICHMENT.md) (the canonical map of what IS);
this file is what we should build next and why. Sources at the end.
Engineering design + t-shirt sizing for every workstream:
[`ENRICHMENT_ROADMAP_SIZING.md`](./ENRICHMENT_ROADMAP_SIZING.md)._

---

## 0. TL;DR

1. **Our architecture is ahead of the field; our economics and our
   proof are behind it.** Provenance-typed edges, determinism-first
   phase layering, content-hash incremental substrate, correction
   ledgers, reversible merge oplogs — most 2026 frameworks have none of
   this. But we pay eager-LLM prices the frontier has abandoned
   (measured: ~38 min/SEP article, ~680 days extrapolated to Wikipedia),
   and the one HARD enrichment CI lane is structurally unable to catch a
   regression.
2. **Faithfulness is the sharpest open wound.** RAPTOR summaries can
   fabricate ("the Russian agent Vladimir", 0 occurrences in source),
   reach the grounding gate with provenance stripped, and the 2026-06-17
   chaos A/B proved the trade is real: contaminated corpus scored
   competence 0.71 / honesty 0.64, cleaned corpus 0.50 / 0.82. Neither
   passes both. Best-in-class means both at once — faithful enrichment,
   not no enrichment.
3. **The frontier's central lesson is cost inversion**: build cheap
   deterministic structure eagerly (noun-phrase concept graphs, SVD
   extractive trees, schema-driven encoder extraction), defer LLM
   synthesis to query time under an explicit budget, and verify anything
   abstractive before it can be cited. LazyGraphRAG indexes at vector-RAG
   cost and beats GraphRAG global search at 4% of its query cost;
   SVD-RAG matches RAPTOR retrieval within 1% at 317x less build cost.
4. **VERIFIER_V0 changes what is affordable.** A local 0.8-4B claim
   verifier at sub-second per claim makes *build-time verification of
   every summary* — the leverage the fabrication note rated "heavy" —
   routine. Enrichment also feeds the verifier's training stream
   (real chunks → claims → construction-labeled corruptions). The two
   projects are one flywheel.
5. **The prime directive: this program is funded as a subtraction.**
   Enrichment today is four systems, a name collision, ~9 artifact
   stores, five entity-extraction paths, and ~12 env knobs — a new
   engineer cannot hold it. Every phase below therefore carries a
   **Deletes** ledger, §4.0 defines the end-state system in one
   paragraph, and a tranche that raises the concept counts has failed
   its gate regardless of what it shipped. The additive-only variant of
   this roadmap is explicitly not worth funding.
6. The plan: **P0 make quality measurable (so deletion is safe) → P1
   faithful by construction → P2 extraction economics + the big
   consolidations → P3 retrieval-side exploitation (evidence-gated) →
   P4 time as a first-class dimension → P5 frontier bets.**

| Phase | One-liner | Headline gate | Headline deletion |
|---|---|---|---|
| P0 | Repair the measurement fabric | enrichment lane can fail; faithfulness measurable | dead code + the ten stale-doc items |
| P1 | No unverified synthesis in evidence | chaos: competence ≥ 0.71 AND honesty ≥ 0.82 | the unverified-abstractive artifact class |
| P2 | Cheaper structure, incremental by default, one store | wiki-class atlas in hours, not years | 3 RAPTOR stores; 2 extraction paths; System-1 engine (migration starts); the incremental flag |
| P3 | Spend the savings at query time | recall lanes move; dark knobs settled by A/B | settled knobs' env vars; per-query entity-graph rebuild |
| P4 | Bi-temporal facts for memory corpora | temporal_slice bench; contradiction → invalidation | ad-hoc "current state" judgment per consumer |
| P5 | Navigator + visual assets | traceable multi-hop; PDF recall parity | bespoke evidence-assembly paths converge on one traversal API |

---

## 1. Where we stand — the honest map

### 1.1 Genuinely strong (a redesign must not lose these)

- **Trust-discriminated structure.** Every edge carries
  `EdgeProvenance` (`LlmExtraction | Derived | ScipStructural | …`,
  `atlas/edges.rs:99`), preserved down into the CSR byte
  (`CSR_VERSION 2`). Consumers can hedge on how an edge was made. No
  mainstream framework (GraphRAG, LightRAG, HippoRAG) types this.
- **Determinism-first layering.** Clustering, resolution 3a/3b, gaps,
  grounding detection, traversal are deterministic; the LLM is confined
  to extraction, naming, judgment. Deterministic selectors feed LLM
  judges, never the reverse (`analysis/tensions.rs:71`).
- **The incremental substrate.** Content-hash atom ids, `doc_to_atoms`
  sidecar, `apply_atom_delta` (`atlas/atoms_delta.rs:75`), body-hash
  code-intel cache, per-note RAPTOR checkpoints with input-hash reset,
  blake3 content-current skip gates. The plumbing for
  frontier-grade incrementality exists — most of it is simply not wired
  to production triggers yet (§1.2, W4).
- **The DI seam + progressive availability.** One RAPTOR builder serves
  docs/conversations/vaults via `TieredEnrichmentProvider`
  (`enrichment/tiered.rs:110`); consumers degrade on *emptiness*, never
  on tier checks, so a mid-enrichment corpus is thinner, not broken.
- **Cache keys carry model identity.** `PhaseCache` stamps
  `{model, fingerprint}`; `section_cache` keys
  `(text, prompt_version, model_id)`. A model swap is a clean miss.
  (RAPTOR's checkpoint is the exception — see W1.)
- **Glass-box audit trails.** `MatchTrace` with alternatives-considered,
  grouped rejection reasons, reversible Merge/Split oplog, stable gap
  signatures requiring ≥2-corpus convergence before schema revision.
- **The opt-out lesson is institutionalized.**
  `Recipe::opts_out_of_auto_enrichment()` exists because unconditional
  enrichment contaminated retrieval-only corpora. Enrichment intent is
  recipe-owned.
- **Honest negative documentation.** `TIERED_RETRIEVAL.md:334-374`
  records *why* HippoRAG-2 was not adopted (observed failures were
  synthesis-side, not retrieval-side). §4/P3 engages that argument
  rather than overriding it.

### 1.2 Five findings that bend the roadmap

**W1 — Abstractive enrichment is unverified, and it fabricates.**
The RAPTOR summarizer prompt invites synthesis ("what shifts"), its only
content rule is a no-quote-character grammar, temp 0.2 ≠ faithfulness
(confirmed live: temp-0 still fabricates), and nothing scores a summary
against its member chunks. Provenance is dropped at every
`EvidenceContext{chunks: map(|c| c.content)}` site, so a fabricated
summary can support a fabricated answer at the grounding gate. The
checkpoint hash covers only `(chunk_id, embed_dim)` — no
`prompt_version` on `RaptorNode`, so a fixed prompt does NOT rebuild
stale trees. Silent-thinning failure modes compound it: a dropped
summary call drops the whole cluster (`raptor_atlas.rs:893-911`).
Chaos A/B (note 1ab68562): contaminated competence 0.71 / honesty 0.64;
clean 0.50 / 0.82. The same blindness applies to System-1 cluster
labels/fault lines and System-4 symbol summaries — no grounding grade
anywhere.

**W2 — Eager-LLM economics cap us at small corpora.**
Measured atlas throughput: `sep-al-farabi` 38 min (79% Phase-1); plan
~80 days for SEP end-to-end; ATLAS.md's own Wikipedia extrapolation is
~680 days. Meanwhile the deterministic `structure_first` pass lifts ~51K
articles in under a minute. Tiered is better (1.5/6/12 min per
1000-chunk doc for T1/T2/T3) but still spends an LLM call per cluster on
every tree. On owned hardware "API cost" is wall-clock and thermal
budget — the scarcest UX resource we have.

**W3 — The measurement fabric cannot catch a regression.**
The single HARD enrichment lane: (a) never rebuilds — `rebuild: false`
(`bench_cmd/all.rs:247`), so prompt/resolver regressions cannot
register; (b) diffs only `axis_scores`, which is `{}` in the only
committed default baseline (`literary/baselines/bk-book-1/latest.json`)
→ always green; (c) precision = `matched/(matched+forbidden_hit)`
(`enrich_cmd/eval.rs:977`) — structurally 1.0; over-extraction is free
(Enron shows 3,202 unmatched predicted clusters). Six goldens exist, one
has a baseline; the bk-book-1 baseline is unmoved for ~7 weeks with
concept 0/2, event 2/5. Variance tooling (`eval-median`) exists but the
gate fires on single runs at a 0.5-pt threshold. There is no edge-F1,
no tension precision (outside governance), no merge-precision metric, no
summary-faithfulness metric, and no lane that joins enrichment to
end-task QA lift.

**W4 — Incrementality is built but unwired.**
`SOVEREIGN_ATLAS_INCREMENTAL` is read and unused — production pays the
full rebuild the INCREMENTAL_ATLAS doc opens by costing at ~16,000x.
The vault path *is* delta-correct (`reenrich_changed_sources`), but
watched-folder GLiNER deltas bypass `CorpusEngine::ingest`, attached
docs lack `display.category` so Phase-B incremental never reaches them,
and the code-atlas patch path drops incoming call edges + never
recomputes salience, degrading "what calls X" monotonically between full
rebuilds — undetected, because `verify-v2` checks counts only.

**W5 — Retrieval-side exploitation lags the build-side investment,
and the docs have drifted from the code.**
Dark/unbenched: `SOVEREIGN_DOC_CLUSTER_WEIGHT` default 0.0 (whole T3
blend inert), `SOVEREIGN_CONV_PPR_WEIGHT=0.25` never swept, RAPTOR
dedupe off, bucket slot-routing aspirational. Per-query entity-graph
rebuild with no cache. Type-collapse in entity nodes ("Swift" Person vs
"SWIFT" Org). Doc drift is systemic: ENRICHMENT_V2.md materially stale
(8→11 atoms, deferred-features that shipped, shipped-features marked
next-up); TIERED_RETRIEVAL.md + ENRICHMENT.md still say the doc-path T2
is Slow-LLM-only and GLiNER is conversation-scoped — the code has a
GLiNER document fast path with LLM fallback (`document_asset.rs:1814`);
the RAPTOR quote-span docstring describes cosine selection the code does
not do (longest-sentence heuristic, `raptor_atlas.rs:972`). Plus pure
waste: the KnowledgeView debouncer runs a full five-phase v1 pass for a
view whose consumer reads v2 atoms (`debouncer.rs:271` vs
`manager.rs:455`).

---

## 2. The 2026 frontier

Eight findings, ordered by how much they should bend our roadmap.

### F1. The graph-construction cost cliff: eager LLM enrichment lost the argument
Microsoft's LazyGraphRAG replaces index-time LLM extraction with **NLP
noun-phrase concept graphs** (index cost = vector RAG, 0.1% of full
GraphRAG) and defers all LLM work to query time behind a **relevance-test
budget**: 3-5 expanded subqueries → embedding-ranked chunks → best-first
community ranking → sentence-level relevance assessor under the budget →
claim extraction over surviving chunk groups → ranked-claim synthesis.
At budget 500 it beats GraphRAG global search on quality at 4% of its
query cost (700x cheaper at parity); at budget 100 it costs the same as
an 8K-token vector-RAG query. LightRAG lands the same direction:
dual-level retrieval over a lightweight graph, ~60% less index cost,
**incremental graph patches instead of rebuilds** (EMNLP 2025). SVD-RAG
(July 2026) removes the LLM from hierarchical summarization entirely:
per-cluster SVD over sentence-embedding matrices, select sentences by
principal-component energy (τ=0.95) — MRR within 1% of RAPTOR (0.867 vs
0.875), Recall@1 slightly better (0.483 vs 0.458), 317x faster tree
build, ~85% fewer tokens, fully deterministic. (Caveat: benchmarked at
38-205 chunks; extractive summaries read less fluently and run ~1.8x
longer.)

**Where we actually landed against F1 (measured 2026-07-31).** The two
halves of LazyGraphRAG came out opposite ways. Its **index** half passed
with 57x headroom — a concept graph over 10k chunks in 5.2 s (§P2.2,
`SP5_concept_graph.md`) — and is specified but **unbuilt**. The only
**query**-half component tested, RAPTOR tree descent under an LLM
relevance budget, **lost** to one-shot cosine top-K at equal evidence
budget (§P5.1, `P51_tree_descent.md`). Note the asymmetry before
quoting the 700x figure above: it describes a community-ranking query
tier that we have not tested, and cannot test until the index half
exists.

### F2. Retrieval graphs converged on dual-node PPR + query-to-triple
HippoRAG 2 (ICML 2025) is the reference upgrade of our T2: phrase nodes
from OpenIE triples + synonym edges, **plus passage nodes** via
contains-edges (dense-sparse integration), **query-to-triple** matching
(ablations: NER-to-node −28% recall@5, query-to-node −55% on MuSiQue),
an LLM recognition-memory filter over candidate triples, then PPR with
balanced reset probabilities (passage weight 0.05). Results: recall@5
+18.3 pts over NV-Embed-v2 on 2Wiki, +7.2 on MuSiQue; answer F1 +4.9
avg; passage-node ablation −13%; gains stable under incremental corpus
growth. Microsoft's DRIFT search teaches the same lesson from the other
side: enter local search through community summaries, not raw entity
matches.

### F3. Temporal knowledge graphs are table stakes for memory corpora
Zep/Graphiti: every edge is **bi-temporal** — (t_valid, t_invalid) plus
ingestion time; a contradiction closes the old fact's validity window
instead of deleting it; episodes stay linked as provenance. This is what
makes "what did X believe in March" and retroactive correction
answerable (94.8% vs 93.4% for MemGPT on DMR). Entity-event KG work
(arXiv 2506.05939) pushes the same temporal-causal consistency for RAG
generally. Our conversation/vault corpora — the memory heart of the
product — have no validity intervals anywhere.

### F4. Faithfulness moved from vibes to claim-level machinery
The 2026 eval stack decomposes outputs into atomic claims and NLI-checks
each against evidence (FActScore → RAGTruth → RAGChecker → the 2026
agent-provenance survey). Small trained verifiers (MiniCheck-7B 77.4
LLM-AggreFact BAcc; HalluGuard-Qwen3-4B 84.0 RAGTruth) match frontier
judges at a fraction of the cost — the bet `VERIFIER_V0.md` is already
making, with FaithBench as the honest yardstick (small classifiers
collapse there). Frontier position: **anything synthesized at build time
is verified before it can ever be cited.**

### F5. Chunk-context augmentation is the cheapest large win still on the table
Contextual retrieval (prepend a 50-100-token "where this chunk sits"
preamble before embedding; layer with BM25 + rerank) reports 35-49%
retrieval-failure reduction in the original write-up, 5-15% in
independent replications. Late chunking (arXiv 2409.04701) gets a
related benefit with zero LLM calls: embed the whole document through a
long-context embedder, pool per-chunk *after* the transformer pass.
Training-free on the Qwen3-Embedding family we already ship (0.6B today;
4B/8B tiers + a matching Qwen3-Reranker exist upstream — noting the
mesh's `EmbedModelInfo` bit-compatibility contract makes an embedder
change a corpus-version event).

### F6. Structured extraction went schema-driven and CPU-sized
GLiNER2 (arXiv 2507.18546; pip + HF checkpoints, ~205M params) unifies
NER, classification, and **hierarchical structured extraction (entities
with attributes; relations)** in one encoder pass with a declarative
schema — CPU-viable, no LLM. GLiNER-Relex (May 2026) adds joint NER+RE;
an Apple-Silicon port exists (GLiNER2Swift). Our `gliner_small-v2.1`
usage is one generation and several capability classes behind; the
recipe-declared `[[enrichment.entity_types]]` investigation schema is
exactly the declarative interface GLiNER2 wants.

> **Measured 2026-08-03 — the frontier reading above held; the
> *substitution* it implied did not.** The published GLiNER2 export was
> run against our own corpora through the production extractor seam and
> **rejected as a v1 replacement**: no throughput win at our vault's
> chunk length, and worse per-mention typing (§P2.1). "One generation
> behind" remains a fair description of the literature. It is not
> evidence that swapping generations improves this system, and this
> section was read as if it were.

### F7. Reasoning-based navigation is displacing one-shot top-K for long documents
PageIndex-style retrieval — build a ToC-shaped tree, let a model
*navigate* it with bounded reasoning — hits 98.7% on FinanceBench with
full traceability. The agentic-RAG literature (arXiv 2501.09136,
2506.10408) generalizes: the retriever decides when/what/how to retrieve
iteratively, at 3-10x token cost — which local inference makes
palatable. We already own the substrate (RAPTOR trees are navigable
ToCs; the atlas CSR is BFS-traversable in `atlas-query`); what's missing
is the navigator loop in chat.

### F8. Visual-document retrieval matured into a small-model recipe
ColPali-class page-as-image retrieval (patch embeddings + MaxSim late
interaction) closes the gap OCR pipelines leave on visually rich PDFs
(financial-PDF case study: 62% dense recall vs 84% ColQwen). The
efficiency frontier reached us: ColModernVBERT at ~250M params is within
0.6 NDCG@5 of ColPali. Our content-addressed asset store (AD-1/AD-2) is
exactly the substrate this bolts onto; it is text-only today.

---

## 3. Gap analysis — us vs. the frontier

| Theme | Frontier position | Us today | Verdict |
|---|---|---|---|
| Build-time cost | Deterministic/statistical structure eagerly; LLM deferred behind budgets (F1) | Eager LLM per section/cluster; deterministic paths exist (`structure_first`, TF-IDF motifs, TextTiling) but LLM remains the default spine | **Behind — the biggest lever, and the cheapest close is already measured.** The concept-graph free tier ran 10k chunks in 5.2 s, 57x under gate, with an adopt-or-write verdict and a committed probe (§P2.2) — proven feasible, still unbuilt. The *deferral* half is the unproven one: §P5.1 failed its gate |
| Faithfulness of synthesized artifacts | Verified before citable (F4) | Unverified; provenance stripped at evidence assembly; no faithfulness metric | **Behind — the sharpest risk** |
| Incremental maintenance | Graph patches on change (F1, LightRAG; Graphiti) | Substrate built (hashes, deltas, checkpoints); production triggers unwired (W4) | **Even on design, behind on wiring** |
| Entity/relation extraction | Schema-driven 205M encoder, joint NER+RE, CPU (F6) | GLiNER v1 small, entities only, type-collapse; LLM fallbacks | **Behind — and NOT cheap to close.** The obvious close (adopt GLiNER2) was measured 2026-08-03 and rejected: no speedup at our chunk length, worse typing (§P2.1). Reopening this needs a different candidate, not a retry |
| Multi-hop graph retrieval | Dual-node PPR, query-to-triple, recognition filter (F2) | HippoRAG-1-style PPR over co-occurrence graph, w=0.25 unbenched, per-query rebuild | **Behind, but adoption must clear our own prior (TIERED_RETRIEVAL.md:334)** |
| Temporal validity | Bi-temporal edges, invalidation-not-deletion (F3) | None; State/Event/Transition atoms + timestamps exist as raw material | **Absent; high product fit** |
| Chunk-context | Contextual/late chunking standard (F5) | Chunk text embedded bare; `topic_context` only on query side | **Absent; cheap** |
| Provenance/trust typing | Rare in frameworks | `EdgeProvenance` everywhere, oplogs, MatchTrace | **Ahead** |
| Progressive availability | Rare | T1→T3 additive, emptiness-degrading consumers | **Ahead** |
| Verification machinery to pair with | External eval services | Chaos two-red-line culture, grounding gate, verifier-v0 in flight | **Ahead — the differentiator** |
| Visual assets | Late-interaction page retrieval (F8) | Asset store stores bytes; no visual index | **Absent; deferred bet** |

---

## 4. The roadmap

Phases are ordered by dependency, not calendar; P0/P1 are the
prerequisite pair, P2/P3 the payoff pair, P4/P5 the product-defining
pair. Every phase names its gate up front (per the bench culture:
gate on exit codes and committed baselines, not vibes) — and its
**Deletes** ledger, because of §4.0.

### 4.0 The end state — the system a new engineer learns (prime directive)

What a new engineer must learn today, honestly: four enrichment systems
selected per-corpus plus a name collision that needs its own warning
section ("atlas" means two unrelated things); ~9 knowledge-artifact
stores; five ways entities get extracted; two incremental mechanisms
(one unwired); ~12 env knobs, several shipped dark; and
trust-by-tribal-knowledge — knowing *which* artifacts can fabricate is
folklore, not a property of the system.

The end state, in the paragraph a new engineer should be able to hold:

> **Extractors turn sources into evidence-bearing atoms in one graph
> store. The LLM judges; it never free-writes into evidence — anything
> abstractive persists only with a verification verdict and its
> provenance attached. Renderers (briefs, digests, signposts, traces)
> read the graph. Tiers are budgets — how much of the graph a corpus
> builds — not different systems.**

Systems 1-4 become *profiles* of that one pipeline (an extractor set +
prompt assets + a budget), which is what "not a version ladder" should
have meant all along. The measurable version:

| Dimension | Today | End state |
|---|---|---|
| Enrichment systems | 4 + the "atlas"/"atlas" collision | 1 pipeline (extractors → graph → renderers); per-corpus profiles |
| Knowledge-artifact stores | **11** — measured 2026-08-01, see [`ENRICHMENT_RATCHET.md`](./ENRICHMENT_RATCHET.md) §2. (This row estimated ~9 and missed `mem_raptor_nodes`, `asset_motifs`, `conv_motifs`.) | 3 (chunks+FTS; the atom-graph family; build caches) |
| Entity-extraction paths | 5 (GLiNER v1; Slow-LLM lark fallback; Phase-1 LLM enumeration; SCIP walk; tabular) | 2 (encoder schemas; structural) — the LLM only judges |
| Trust model | per-artifact folklore | 1 rule: nothing unverified-abstractive in evidence; provenance end-to-end |
| Env knobs on these paths | **26** — 15 registered + 11 grandfathered-unregistered; measured 2026-08-01, see [`ENRICHMENT_RATCHET.md`](./ENRICHMENT_RATCHET.md) §5. (This row estimated ~12 and counted registered flags only.) | ≤ 4, each with a committed A/B behind its default |
| Incremental mechanisms | 2 + a **wired** flag — `SOVEREIGN_ATLAS_INCREMENTAL` is load-bearing at `newsworthy_host.rs:318-340`, not "read but unused" as §P2 states | 1 (content-hash deltas), the only path |
| "Explain enrichment" | a page + a warning section | the paragraph above |

**The subtraction ledger** — every major addition names what it retires:

| Addition | Retires |
|---|---|
| The verification rule (P1) | the unverified-abstractive class; silent cluster-thinning; trust folklore |
| ~~GLiNER2 schemas (P2.1)~~ **VOID 2026-08-03** — GLiNER2 rejected (§P2.1) | ~~the Slow-LLM lark path; the `gline-rs → orp → ort-rc` chain; LLM enumeration prompts~~ **None of these retire.** v1 stays, so `gline-rs → orp` stays; the lark path and the enumeration prompts lose their replacement. This row was a promised subtraction that did not happen — the ratchet has to find it elsewhere |
| Summary atoms in the one graph (P2/D3) | `raptor_nodes` + `conv_raptor_nodes` + `raptor_summaries.lance` + the ANN freshness special case + the atlas/atlas collision itself |
| Graph-rendered digests (P2-P3/D4) | `field_engine.rs` + the `Domain` registry + `field_skeleton.json` |
| Graph-resident entity PPR (P3/D5) | the per-query `conv_entity_graph` rebuild (and the planned LRU cache — an add this cancels) |
| Budget tiers (P2/P5) | "which system does my corpus use" as a concept a user-facing engineer needs |
| A/B'd defaults (P3.1/D2) | the dark-knob population, env vars deleted with their decisions |

**The complexity ratchet.** The seven "Today" numbers above are tracked
at every tranche exit exactly like the `quality/` baselines: a tranche
that leaves any of them higher than it found them has failed its gate,
whatever features it shipped.

The measurements live in
[`ENRICHMENT_RATCHET.md`](./ENRICHMENT_RATCHET.md) — one predicate, one
enumeration and one command per number, plus an append-only table of
tranche-exit values. The numbers in the table above were estimates until
2026-08-01; two of them were wrong, which is why the gate now requires a
predicate rather than a figure. **T1 exit verdict: PASS** (knobs −1,
trust model improved, nothing rose).

### P0 — Make enrichment quality measurable (the gate for everything else)

The system's own assessment doc says it: "you cannot gate a migration
you can't measure" (`ENRICHMENT_V1_TO_V2_ASSESS.md:112-120`).

1. **Repair the HARD lane.** Diff *all* populated axes (legacy named
   fields + `axis_scores`), fail on missing/empty baseline instead of
   auto-green `FirstRun`, and record `{model, prompt_version}`
   fingerprints in the baseline so a static-artifact score can never be
   mistaken for a pipeline score. Add a scheduled `--rebuild` tier
   (weekly, alongside lint-gate/api-gate cadence) since per-PR rebuild is
   too slow. Wire `eval-median` variance into the threshold: gate at
   `max(0.5pt, observed spread)`.
2. **Make over-extraction cost something.** Add unmatched-extraction
   mass as a scored quantity next to the forbidden-only FP (per-axis
   `unmatched_rate`), with a sampled adjudication loop to calibrate how
   much of it is junk.
3. **Faithfulness lane (the W1 instrument).** Score every RAPTOR node
   summary against its member chunks — judge now, verifier-v0 when it
   ships — reporting unsupported-claim rate per corpus. Extend the same
   harness to System-1 cluster labels/fault lines and System-4 symbol
   summaries (vs. symbol body). This lane is also **Stream B data
   generation** for the verifier: (chunks, summary-claim,
   support/corruption label) tuples fall out of the harness for free.
4. **Retrieval-utility A/B lane.** One command that runs a QA bank with
   enrichment surfaces toggled (`SOVEREIGN_RAPTOR_GROUNDING`,
   `--with-atlas`, PPR weight) and reports the *joined* verdict —
   institutionalizing what the chaos contamination A/B and the wikipedia
   `--with-atlas` probe (50/71 → 79/83 sources/facts) were done by hand.
5. **Close the cheap metric holes**: edge-F1 goldens (Involves/Grounds/
   Transition on the two corpora that already have atom goldens),
   merge-precision on a synthetic personal-corpus ER golden (Enron
   methodology, non-email register), baselines for the other five
   goldens. Promote the enron ER bench into the tracked set with a hard
   `bench gate` twin.

**Gate:** the enrichment lane demonstrably fails when a known regression
is injected (add a canary test that breaks a resolver constant and
asserts the lane goes red); faithfulness lane reports a number for every
enriched corpus in CI.

**Deletes:** the immediate dead-code sweep rides here — the
effectively-dead `ConvTieredProvider`, the debouncer's five-phase v1
pass for atlas-typed views, the zeroed `FieldModelStats` stub, the
never-filled conv skeleton columns, and the ten stale-doc items (§6).
More importantly, P0 is the *enabler*: every structural deletion below
is gated on a measurement this phase creates. We measure so we can
delete safely — the eval work is the demolition permit, not added
process.

### P1 — Faithful by construction (no unverified synthesis in evidence)

The four leverage points from note 1ab68562, plus structure:

1. **Extractive floor.** Adopt SVD-RAG-style extractive selection as the
   *default* summary body for RAPTOR cluster nodes (we already store
   quote spans and rank members by centroid cosine — the machinery is
   90% present). Extractive nodes cannot fabricate; they are
   deterministic and free. Measure retrieval parity on the summarize
   banks (SVD-RAG's evidence says within 1%; verify on ours).
2. **Verified abstractive lift.** Keep the abstractive summary as an
   optional layer on top, generated with the faithful prompt (leverage
   A) and **persisted only if the verifier passes its claims against
   member chunks** (leverage D — affordable once verifier-v0 ships;
   judge-scored at lower volume until then). Failed summaries fall back
   to the extractive floor, flagged for the correction ledger.
3. **Version-stamped trees.** Add `prompt_version` (and summarizer model
   id) to `RaptorNode` + the checkpoint hash (leverage C), matching the
   discipline PhaseCache/section_cache already have. A prompt fix then
   *does* rebuild stale trees — incrementally, per node, via the
   existing checkpoint machinery.
4. **Provenance-aware evidence.** Stop stripping source tags at
   `EvidenceContext` assembly (leverage B). Stratify: summary-derived
   evidence supports thematic/structural claims; factual claims must
   trace to leaf chunks (the gate already knows how to demand witnesses
   — this extends the chaos fairness contract into the evidence path).
   Kill the silent-thinning modes: a dropped cluster summary becomes an
   extractive node, never a hole.
5. Apply the same contract to System-4 symbol summaries (graded against
   the symbol body — this is CODE_INTEL_CHAT's unbuilt Inc 3) and
   System-1 fault lines.

**Gate:** chaos-secret-agent with enrichment ON: competence ≥ 0.71 AND
honesty ≥ 0.82 (beat both arms of the 2026-06-17 A/B simultaneously).
Faithfulness lane unsupported-claim rate ~0 on new trees.

**Deletes:** the unverified-abstractive artifact class itself — the
category of thing an engineer must "just know" not to trust; the three
silent cluster-thinning failure modes (`raptor_atlas.rs:893-911` —
extractive fallback replaces silent drop); the version-blind checkpoint
special case. Net-new concept count: one (the verification rule), and
it replaces folklore, which is the trade this whole roadmap wants.

### P2 — Extraction economics (structure at 10-100x lower cost, incremental by default)

1. **GLiNER2 adoption. Step (a) is MEASURED AND REJECTED (2026-08-03);
   (b)–(d) are unevaluated.** The plan was: upgrade `sovereign-gliner` to
   the GLiNER2 generation — schema-driven multi-task extraction (entities
   + types + attributes + relations) on CPU/ANE — targeting, in order:
   (a) the conversation/vault path (replacing v1, fixing type-collapse by
   extracting types jointly); (b) the document T2 (retiring the LLM
   fallback); (c) **typed-atom seeding for System 2** — GLiNER2 output
   becomes deterministic Entity/Relation candidate atoms
   (`EdgeProvenance::EncoderExtraction`, a new variant), shrinking
   Phase-1's LLM surface to judgment instead of enumeration; (d) the
   recipe `[[enrichment.entity_types]]` investigation schema compiles
   directly to a GLiNER2 schema — recipe authors get NER for free.

   **What (a) actually measured** (notes `f42cf7ec`, `dc2e4b5d`;
   `research/enrichment-spikes/findings/SP1_gliner2.md` corrections 3–4;
   harness `sovereign-gliner/examples/typing_audit.rs`, both backends
   through the production seam over all 3,175 obsidian vault chunks):

   - **No speedup on the target corpus.** 881.9 s (v1) vs 893.2 s
     (GLiNER2). The 2.52× is a property of the chunk-length distribution
     — sep chunks are p50 761 chars, vault chunks p50 1,808 — not of the
     model. Any "N× faster" figure elsewhere in this doc or in
     `ENRICHMENT_ROADMAP_SIZING.md` inherits that caveat.
   - **Type-collapse is not fixed; it is worse.** Mention-level Person
     accuracy 96.9% (v1) vs 81.8% (GLiNER2) on the vault oracle, 99.7%
     vs 67.3% on sep. `Work` becomes a catch-all for ordinary noun
     phrases: 16,053 `Work` mentions against v1's 632.
   - **What survives:** residency (~9 GB lighter, note `3f47d12e`) and
     the `LabeledEntityExtractor` seam (commit `86f83c1a`), which is
     where any future extractor lands. `SOVEREIGN_GLINER_MODEL_ID`
     selects the generation and defaults to v1.

   Scoring extractors by **entity** rather than by **mention** reported
   clean parity for a backend getting a third of its rows wrong;
   `chunk_entities` is a mention table (`bench/gliner/README.md`).
2. **Concept-graph free tier (LazyGraphRAG's index). FEASIBILITY
   MEASURED AND PASSED 2026-07-31 — NOT BUILT.** A deterministic
   noun-phrase/co-occurrence concept graph + graph-statistic communities
   as the *universal* baseline enrichment for every corpus — built at
   ingest for roughly embedding cost, feeding: community entry points
   for retrieval, seed vocabulary for Phase-1, and the lazy query tier
   (P5). `structure_first` proved the pattern for code (51K articles,
   <1 min, no LLM); this is its text-corpus sibling.

   **What SP5 measured** (note `889908e9`;
   `research/enrichment-spikes/findings/SP5_concept_graph.md`; probe
   `corpus-engine/examples/concept_graph_probe.rs`, committed — and it
   IS the first draft of the layer). Pipeline: RAKE-style noun-phrase
   candidates + capitalization runs → df-band/tf-idf top-5k vocabulary →
   df-normalized chunk co-occurrence edges → Leiden.

   - **5.2 s wall for 10,000 wikipedia chunks** (337 articles), debug
     build, single core, against a <5 min exit gate — **57x headroom**.
   - **68 communities** at resolution 2.0; **17 of the 20 largest**
     eyeball-cohere against article titles, 3 mixed.
   - **Adopt-or-write verdict is both.** Adopt `leiden-rs` 0.8.1
     (MIT/Apache-2.0, dependency-tiny core, takes a CSR edge list via
     `GraphDataBuilder`; skip its petgraph adapter, which wants ^0.8
     against our 0.6 pin). Write the noun-phrase/co-occurrence layer
     ourselves — roughly 250 lines.
   - **Two tunings were load-bearing.** The motif single-doc df band
     (0.3) admits corpus-generic vocabulary at 10k scale, so 0.05 cap +
     a calendar-term stoplist; and raw co-occurrence counts let hubs
     dominate modularity, so df normalization — which is what sharpened
     13 coarse communities into the 68 clean ones.
   - Confidence Med → High. The entity-co-occurrence-only fallback is
     **not** needed.

   **Provenance caveat before production:** `leiden-rs` is hosted on a
   gitcode.com mirror (~9.2k downloads) — vendor or pin-audit it. A
   hand-rolled Louvain (~200 lines) stays the fallback, and the probe's
   edge-list-in / partition-out seam keeps that swap local. Currently a
   `corpus-engine` **dev-dependency only**.

   **Before funding the build, answer what a USER gets.** A concept
   graph is machinery. Its three declared consumers are retrieval entry
   points (speculative), Phase-1 seed vocabulary (internal), and the P5
   lazy query tier — whose only tested component **failed its gate**
   (§P5.1). Cheap-and-proven-fast is not the same as connected. The
   defensible reason to build it now is narrower and worth stating
   plainly: it is the missing prerequisite for testing the query half in
   its real form at all.
3. **Wire the incremental machinery** (W4): flip
   `SOVEREIGN_ATLAS_INCREMENTAL` from read-but-unused to the default
   path via `apply_atom_delta`; route watched-folder deltas through the
   GLiNER hook; give attached docs the category tag Phase-B needs; fix
   the code-atlas patch gaps (recompute incoming edges + salience for
   touched atoms, or schedule a bounded repair pass) and upgrade
   `verify-v2` from count-equality to sampled edge-set equality so
   monotonic degradation is detectable.
4. **Structural contextual retrieval (F5, zero LLM).** Prepend
   deterministic context to chunk text at embed time — doc title,
   section path, owning RAPTOR-node summary line, top entities — all
   artifacts we already build. Benchmark against the notes_tiered
   failure classes. Evaluate late chunking as the follow-on (needs
   long-context embed batching; no model change).
5. **Retire measured waste**: the debouncer's v1 pass for atlas-typed
   views (W5), ~~Phase-1b's serial 4-chunk batching where GLiNER2
   covers it~~ (**void** — GLiNER2 rejected 2026-08-03; the batching
   stays until something else covers it), and the six stale-doc
   mismatches in the hygiene table (§6) — each is an hour of work and a
   trust repair.

**Gate:** end-to-end atlas build on a wiki-class corpus in hours (vs.
680-day extrapolation); vault/doc enrichment wall-clock halved at equal
or better atom-F1 (measured by the now-real P0 lane); incremental edit →
patched atlas with no full rebuild, verified by the upgraded verify-v2.

**Deletes:** this is the tranche where the store count and the system
count actually drop. **(a) and (b) are VOID as of 2026-08-03** — they
were both contingent on GLiNER2 owning extraction, and it does not.
~~(a) The Slow-LLM lark entity path and its batching machinery, once
GLiNER2 owns extraction. (b) The `gline-rs → orp → ort-rc` dependency
chain (bare `ort`).~~ The `ort` rc-pin is now *load-bearing twice* —
`gline-rs` needs it for v1 and `sovereign-gliner::gliner2` links it
directly — so that chain is more entrenched than before this phase,
not less. Any future extractor swap must re-earn both deletes. (c) The three
RAPTOR sidecar stores — `raptor_nodes`, `conv_raptor_nodes`,
`raptor_summaries.lance` + its freshness-gate special case — once
summary nodes are atoms in the one graph (sizing doc D3); this is also
what dissolves the "atlas"/"atlas" name collision for good. (d) The
`SOVEREIGN_ATLAS_INCREMENTAL` flag — incremental stops being an option
and becomes the only path. (e) System-1 retirement *starts* (sizing doc
D4): the five field phases become pipeline prompt assets over the
graph, the ambient digest becomes a renderer, and `field_engine.rs` +
the `Domain` registry go when the KnowledgeView parity gates
(P0-authored) hold. The concept-graph free tier writes into the same
atom store with statistical provenance — no new store is created by
this phase.

### P3 — Retrieval-side exploitation (spend the build savings where users feel them)

Our own prior (TIERED_RETRIEVAL.md:334-374) declined HippoRAG-2 because
observed failures were synthesis-side. P0's lanes make that claim
re-testable; P1 changes the synthesis side it indicted. So:

1. **Settle the dark knobs by measurement, not archaeology. Two of the
   four are SETTLED (2026-07-31, 2026-08-04); RAPTOR dedupe and
   chunk-neighbour expansion remain unevaluated.** The plan was: the PPR
   weight sweep (0.0/0.15/0.25/0.4 — explicitly requested, never run),
   the T3 cluster-score blend (spec'd at 0.25, shipped dark at 0.0),
   RAPTOR dedupe, chunk-neighbour expansion. Each becomes one entry in
   the P0.4 A/B lane; defaults flip only on wins.

   **What settled** (rows in `sovereign/DEFAULTS_LEDGER.md`; notes
   `6a957b47`, `f4150097`):

   - **T3 cluster-score blend — DELETED** (2026-07-31). Measured a
     **0.0000** delta across 3 sep banks × 3 weights, so the knob and
     its code went with it. This is the P3 *Deletes* contract below
     executing as written.
   - **Conversation entity PPR — DEFAULT FLIPPED TO 0.0, code kept**
     (2026-08-04). 180-question paired bank on `conversations-anthropic`,
     two-sided sign test on reciprocal rank: 49–31 alone (p=0.0567),
     64–43 under the strongest retrieval config (p=0.0527). Never
     reached p<0.05, and structurally it cannot add — it re-ranks in
     place, and `B-in-pool` (87.8%) and `source_ratio` (0.9028) were
     identical to four decimals with it on and off. A deliberate
     departure from *Deletes*, operator-directed: the measurement says
     *marginal*, not *wrong*, so a one-line default is cheaper to
     reverse than 1,325 tested lines are to rebuild. Its second-order
     value is larger than its first — turning it off is what removes the
     query-path read of `chunk_entities`, and so what makes deferred
     NER safe (`sovereign/docs/specs/PROGRESSIVE_ENRICHMENT.md`).

   **What shipped alongside them, and is the one P3 change a user can
   name: per-article dedup** (`dedup_by_source = true` on both
   conversation recipes, 2026-08-04). Measured on the same 180-question
   bank: mean RR 0.2631 → **0.3362** (+28%), both@10 26.7% → **50.6%**
   (+24pp), source_ratio 0.744 → **0.856**, at +0.7 s search. In user
   terms: fewer near-duplicate citations drawn from one conversation, so
   an answer that needs two different conversations actually gets both.
   **It is inert on an already-built index** — the flag stamps at
   ingest, so a corpus must re-ingest before a user sees any of this.
2. **Query-to-triple + passage nodes, scoped.** Prototype HippoRAG-2's
   two highest-ablation components on the conversation graph we already
   build (phrase nodes exist as entities; passage nodes = chunks; the
   "triples" **have no encoder source** — SP1 found the GLiNER2 export
   fills typed slots, not linked tuples, and P2.1 was then rejected
   outright, so relations stay LLM-judged and this component needs its
   triples from somewhere else before it can be prototyped):
   query-to-triple seeding
   replacing surface-form matching, passage-node integration replacing
   the clique fallback. Verifier-v0 doubles as the recognition-memory
   filter (it is a relevance/entailment judge). Adopt only on recall
   lane wins — the honest re-litigation our prior deserves.
3. **Persist the entity graph** (per-query rebuild + LRU today) and
   add the Qwen3-Reranker as an optional final stage on the hybrid
   scorer path, A/B'd on notes_tiered failure classes. **The reranker
   half is MEASURED AND REJECTED (2026-08-04) — on latency, after its
   quality condition passed.** It beat everything else tried (mean RR
   0.3968, both@10 75.6%, source_ratio 0.903) and is rejected anyway:
   search p50 goes 557 ms → **4,566 ms** *synchronously inside the
   turn*, so the whole cost lands on TTFT; it needs a fourth resident
   slot on a ~29 GB daemon; and it degraded ~60× (4.3 s → >280 s per
   query) under memory pressure. Dedup buys ~60% of the quality for
   ~20% of the latency, and is what shipped instead. The lesson worth
   keeping: the flip condition was written about *quality*, and quality
   was never the binding constraint. Ledger row moved DARK → REJECTED.
   The persistence half is separately **cancelled** by the *Deletes*
   contract below, since the PPR flip retires the per-query rebuild it
   was meant to optimise.
4. **Community entry points for local search** (DRIFT's lesson): route
   entity-poor queries through concept-graph communities (P2.2) before
   leaf retrieval on LanceDB corpora, which today have no tiered path
   at all.

**Gate:** notes_tiered hit@5 by failure class + conversation-bench
temporal/cross-conv archetypes move; every default flip carries its A/B
in the baseline commit.

**Deletes:** every settled knob's env var — the A/B protocol ends in
*default + deletion*, not default + one more flag (target: the ~12-knob
population drops to ≤ 4); the per-query `conv_entity_graph` rebuild,
once PPR reads entity atoms from the graph (sizing doc D5) — which also
*cancels* the separately-planned persistence/LRU addition rather than
building it.

### P4 — Time as a first-class dimension (the memory moat)

Our product thesis — a sovereign memory of your conversations, notes,
and work — is exactly the corpus class where Zep/Graphiti proved
bi-temporal structure pays.

1. **Bi-temporal envelope fields** on State/Relation/Claim atoms:
   `valid_from`, `valid_to`, `observed_at` (ingestion time already
   exists). Additive schema bump (2.3 → 2.4), `#[serde(default)]`
   per back-compat convention.
2. **Invalidation, not deletion.** Extend reconciliation — which already
   owns contradiction-shaped work (identity merges, reversible oplog) —
   with a fact-contradiction signal: a new State/Claim atom that
   contradicts a live one closes the old atom's validity window and
   writes a typed `Supersedes` edge, oplogged and reversible. The
   governance mootness machinery (superseded rules are not open
   conflicts) is the in-house precedent to generalize.
3. **Temporal retrieval surface**: `atlas-query --as-of`, and the
   conversation-bench `temporal_slice`/`trend` archetypes get a typed
   path (current-state queries filter to open validity windows; history
   queries traverse closed ones).
4. **Episode provenance stays cheap**: conversations already key chunks
   by `conv_uuid` + timestamps; the typed-extension pass gains
   validity stamping from message time.

**Gate:** temporal_slice + decision_trace archetypes in
`bench/conversation` scored with typed-path on/off; a planted
correction scenario ("X, later corrected to Y") answers Y-with-history,
never X.

**Deletes:** a concept rather than a store — "current state" stops
being every consumer's ad-hoc recency judgment and becomes one
queryable property (an open validity window), and supersession replaces
unbounded fact accumulation as the memory-growth story.

### P5 — Frontier bets (product-defining, evidence-gated)

1. **The navigator (F7). ONE HOP MEASURED AND FAILED 2026-07-31; the
   navigator as specified is UNTESTED.** A bounded agentic loop over
   structures we already ship: concept-graph communities → RAPTOR tree
   descent → atom-graph traversal → leaf chunks, with the LazyGraphRAG
   budget controlling relevance tests, every hop logged as a
   MatchTrace-style trail (glassbox by construction). This becomes the
   DeepQuery spine for big corpora and the "lazy atlas": claims
   extracted at query time are **written back** as verified atoms via
   `apply_atom_delta`, so the corpus densifies where users actually look
   — eager enrichment only where it provably pays, permanent value from
   every expensive query. The mesh compounds it: Blanket peer-assist can
   pre-warm hot communities during idle windows.

   **What P5.1 (gate G8) measured**
   (`research/enrichment-spikes/findings/P51_tree_descent.md`; harness
   `sovereign-inference/examples/p51_descent.rs` +
   `research/enrichment-spikes/scripts/p51_dump.py`, both committed; raw
   logs `runs/p51/`). LLM tree-descent vs one-shot cosine top-K at an
   equal ~3,000-token evidence budget, 14-question summarize banks,
   Qwen3.6-35B-A3B, temperature 0, on the 271-node scoped sep tree:

   | Bank | descent | one-shot top-K |
   |---|---|---|
   | `summarize` (n=8) | 19/80 = 0.238 | 16/80 = 0.200 |
   | `summarize_obscure` (n=6) | 12/60 = 0.200 | **23/60 = 0.383** |
   | overall | 31/140 = 0.221 | **39/140 = 0.279** |

   - Cost: 2–3 LLM calls and ~19.1 s/question vs 1 call and ~15.6 s.
   - **The loss is evidence assembly, not navigation.** Hop logs show the
     model picking the on-topic subtree tops; descent's leaf pools carry
     a mean **2.07** expected facts vs **3.57** for cosine top-K, and
     `facts_in_evidence` is ≤ one-shot on 11 of 14 questions. Committing
     the whole budget to two picked subtrees is worse than corpus-wide
     chunk cosine at the same budget.
   - **This IS the gate below**, run early against the middle hop alone.

   **Scope — do NOT read this as "LazyGraphRAG's query tier is
   rejected."** P5.1 descended a RAPTOR tree. LazyGraphRAG ranks
   **communities**, which is this navigator's *first* hop — and that hop
   does not exist, because §P2.2 is unbuilt. The 700x-at-parity claim in
   §F1 is a claim about a tier we have not yet been able to test. The
   real experiment is re-running G8 with concept-graph communities in
   front of the descent.

   **Two findings with value beyond the score.** (1) The
   `--group-by-article` RAPTOR output is a **forest** — 42 parentless
   tops (2 level-2 + 40 article-level-1) — invisible to production
   because both consumers fetch-all-then-filter; any future descent
   consumer must start from parentless nodes, not max-level. (2)
   `preferred_speed: Slow` requests fail outright when the 35B primary
   is not resident, and the first run's harness swallowed all 56 such
   failures via `unwrap_or_default()` — scores were vacuously zero and
   the "descents" were pure first-2 fallback. An instrument-validation
   failure of exactly the ARCH §18.4 kind: the run was green and empty.
2. **Visual assets (F8).** A ColModernVBERT-class (~250M) late-
   interaction index over asset-store page images, feature-gated like
   GLiNER, starting with the described-asset PDF verticals where OCR
   demonstrably loses (financial/scanned documents). Multi-vector
   storage fits the existing Lance sibling-table pattern
   (`raptor_summaries.lance` precedent).

**Gate (navigator):** multi-hop QA on the summarize/obscure banks vs.
one-shot top-K at equal token budget, plus trace-completeness (every
answer reconstructs its hop path). **Gate (visual):** recall parity
with the 62%→84% published gap on a scanned-PDF golden before any
default-on.

**Deletes (navigator):** the bespoke evidence-assembly paths.
`knowledge_query`, the DeepQuery branch, and `metalingual` each
hand-assemble evidence today; the navigator is fundable *because* it
ends with those converging on one budgeted traversal API — fewer query
paths, not one more. **Visual is the honest exception:** it is the one
workstream that adds a store, which is exactly why it stays a deferred,
spike-gated bet rather than part of the funded core.

---

## 5. What we deliberately do not do

- **No merge of the four systems into one.** Their coexistence is
  load-bearing (`ENRICHMENT.md`: "not a version ladder"). Convergence
  happens at the *substrate* (atoms + provenance + delta) and the
  *query* layer (navigator), not by forcing one enrichment shape.
- **No full-GraphRAG community summarization.** LazyGraphRAG's own
  authors showed the eager version is dominated; we skip straight to
  the lazy form.
- **No wholesale HippoRAG-2 adoption without clearing our own recorded
  prior** — components enter behind recall-lane evidence (P3.2).
- **No cloud calls, ever.** Every technique above runs on local
  encoders (GLiNER, ColModernVBERT), local SLMs (verifier), or the
  resident slots. That constraint is the product.
- **No un-versioned synthesis.** After P1, nothing abstractive persists
  without model id + prompt_version + a verification verdict attached.
- **No addition without a named deletion.** A workstream whose end
  state raises any §4.0 concept count does not ship, whatever it adds.
  The additive-only variant of this roadmap — all the features, none of
  the consolidation — is cheaper and is explicitly not worth funding:
  it would leave a fifth system on the pile.

## 6. Hygiene backlog surfaced by this review

Cheap, high-trust fixes; each is prose-drift or measured waste, with the
evidence ref. (These are doc-contract repairs per §1.1 — do them in the
same PRs that touch the areas.)

| Item | Evidence |
|---|---|
| ENRICHMENT_V2.md: atom/edge counts, schema version, shipped-vs-deferred tables all stale | agent audit vs `atoms.rs:987`, `edges.rs:45` |
| TIERED_RETRIEVAL.md + ENRICHMENT.md: doc-path T2 described as Slow-LLM/lark; GLiNER described conversation-only — both false since the doc fast path | `document_asset.rs:1814-1893` |
| RAPTOR quote-span docstring claims cosine selection; code is longest-sentence | `raptor_atlas.rs:11-12` vs `:972` |
| Grammar-noop comments cite a deleted file (`embedded.rs:3140`); verify against `json_grammar.rs` and fix the Person-default rationale if enforcement now binds | `resolution.rs:234`, `tension_classifier.rs:31-35` |
| Debouncer runs v1 field pass for atlas-typed views whose digest reads v2 atoms | `debouncer.rs:271` vs `manager.rs:455` |
| `FieldModelStats` is a zeros stub written to every skeleton | `field_engine.rs:775` |
| Field-engine resume silently drops clusters/fault-lines/open-questions | `field_engine.rs:219-225,321-327` |
| `ConvTieredProvider` effectively dead (FolderTieredProvider wired for both paths); stale v0-scope docstrings | `enrichment_bootstrap.rs:47-60` |
| Retrieval docs point at pre-split `runtime/retrieval.rs` line numbers | agent audit |
| Retrieval-only bench corpora contaminated on disk pre-opt-out need re-install | note 1ab68562 |

## 7. How this compounds

**With the verifier (VERIFIER_V0.md).** Enrichment is both the
verifier's *customer* (P1 build-time gating; P3 recognition filter;
P5 write-back verification) and its *supplier* (P0.3's faithfulness
harness generates construction-labeled training tuples through the
production interface — exactly the Stream B distribution argument).
Every phase makes the other project stronger; neither waits on the
other (judge-scored interim paths are specified).

**With the mesh.** Cheaper eager enrichment (P2) shrinks what Blanket
grants need to cover; the lazy tier (P5) gives idle peers well-shaped
work units (pre-warm a community, verify a tree); bi-temporal atoms (P4)
gossip cleanly because invalidation is append-shaped, matching the
oplog/gossip discipline the mesh already has.

**With the glassbox thesis.** Every proposed mechanism keeps or extends
a trace: extractive floors are quotable by construction, verifier
verdicts attach to nodes, navigator hops log as trails, invalidations
are oplogged. Best-in-class *for us* means the user can always ask "why
does the system believe this?" and get an answer with line numbers.

---

## 8. Sources

Internal: `ENRICHMENT.md`, `ENRICHMENT_V2.md`, `ATLAS.md`,
`INCREMENTAL_ATLAS.md`, `TIERED_RETRIEVAL.md`, `PROGRESSIVE_ENRICHMENT.md`,
`RAPTOR_ANN_INDEX.md`, `ATLAS_STORAGE_V2.md`, `CODE_INTEL_CHAT.md`,
`VERIFIER_V0.md`, `ENRICHMENT_V1_TO_V2_ASSESS.md`, notes store
(esp. 1ab68562 — RAPTOR contamination + chaos A/B), committed baselines
under `sovereign/bench/*/baselines/`.

External:
- LazyGraphRAG — [Microsoft Research blog](https://www.microsoft.com/en-us/research/blog/lazygraphrag-setting-a-new-standard-for-quality-and-cost/)
- HippoRAG 2 — [From RAG to Memory (arXiv 2502.14802, ICML 2025)](https://arxiv.org/abs/2502.14802)
- SVD-RAG — [arXiv 2607.10316](https://arxiv.org/html/2607.10316v1)
- Zep/Graphiti — [arXiv 2501.13956](https://arxiv.org/abs/2501.13956); [temporal KG overview](https://www.getzep.com/ai-agents/temporal-knowledge-graph/)
- Entity-event temporal KGs — [arXiv 2506.05939](https://arxiv.org/pdf/2506.05939)
- LightRAG — [GitHub (EMNLP 2025)](https://github.com/hkuds/lightrag)
- GLiNER2 — [arXiv 2507.18546](https://arxiv.org/abs/2507.18546); [GLiNER-Relex (arXiv 2605.10108)](https://arxiv.org/html/2605.10108v1); [fastino-ai/GLiNER2](https://github.com/fastino-ai/GLiNER2)
- Late chunking — [arXiv 2409.04701](https://arxiv.org/abs/2409.04701); [Jina write-up](https://jina.ai/news/late-chunking-in-long-context-embedding-models/)
- Agentic RAG surveys — [arXiv 2501.09136](https://arxiv.org/abs/2501.09136); [arXiv 2506.10408](https://arxiv.org/pdf/2506.10408)
- DRIFT search — [Microsoft Research blog](https://www.microsoft.com/en-us/research/blog/introducing-drift-search-combining-global-and-local-search-methods-to-improve-quality-and-efficiency/)
- PageIndex — [VectifyAI/PageIndex](https://github.com/VectifyAI/PageIndex)
- Visual retrieval — [ColPali/ColQwen ecosystem](https://huggingface.co/learn/cookbook/multimodal_rag_using_document_retrieval_and_vlms); [multimodal RAG 2026 survey](https://bigdataboutique.com/blog/multimodal-rag-retrieval-over-images-pdfs-and-text)
- Evidence provenance survey — [arXiv 2606.04990](https://arxiv.org/pdf/2606.04990)
- SVD/tree successors — [DTCRS (arXiv 2604.07012)](https://arxiv.org/pdf/2604.07012); [Bridge-RAG (arXiv 2603.26668)](https://arxiv.org/pdf/2603.26668)
- Embedding landscape — [open-source embedding guide 2026](https://www.bentoml.com/blog/a-guide-to-open-source-embedding-models)
