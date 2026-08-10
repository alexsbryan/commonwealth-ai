# Atlas Enrichment — A Newcomer's Guide

A friendly walkthrough of what the atlas is, how it gets built, how it
plugs into retrieval, and how to drive it from the CLI. The companion
to [ENRICHMENT_V2.md](./ENRICHMENT_V2.md), which tracks shipped work
landing-by-landing; this doc is the *concept and operating manual*.

---

## What the atlas is

A **typed knowledge graph** computed *from your indexed corpus*, sitting
alongside the chunk store. Where the chunk store says "here are passages
ranked by relevance to your query", the atlas says "here are the
**entities** mentioned in those passages, **what they did, what they're
related to, what claims surround them, and what questions they raise**".

It's a second-class index. Retrieval starts from chunks; the atlas adds
*grounding* — entity-level summaries that travel with the chunks into the
prompt assembly path.

The vocabulary is small and stable:

| Atom type | Roughly | Example |
|---|---|---|
| **Entity** | A nameable thing | "Albert Einstein", "Berlin Conference" |
| **Event** | Something that happened | "Einstein publishes 1905 papers" |
| **State** | A condition that holds for a span | "Germany under hyperinflation, 1921–23" |
| **Relation** | A typed link between entities | "Einstein advised by Minkowski" |
| **Claim** | A proposition the corpus asserts | "Spontaneous emission is irreducibly probabilistic" |
| **Question** | An open question the corpus raises | "What was the role of patronage in early Italian science?" |
| **Configuration** | A higher-order pattern | "Static-vs-dynamic ontology grid" (philosophy domain) |

| Edge type | Connects | Means |
|---|---|---|
| **Involves** | Atom → Entity | "this event/state/claim involves this entity" |
| **Transition** | State → State | "trajectory step" |
| **Causes** | Event → Event | "directly precipitated" |
| **Grounds** | Claim → Evidence | "this claim is supported by this passage" |
| **Tensions** | Claim ↔ Claim | "these are in productive disagreement" |
| **Contrasts** | Entity ↔ Entity | "compared on shared dimension" |
| **CrossCorpus** | Atom in A → Atom in B | "same person across two corpora" |

Both shapes are stable across corpora. What *varies* is which strategy
populates them and how deep the enrichment goes.

---

## The two ingestion strategies

The atlas pipeline is open at one seam — `AtlasIngestion` — and closed
everywhere else. Two strategies ship today:

### `structure_first` — fast, deterministic

For corpora that already encode structure (Wikipedia is the canonical
example: titles, sections, wikilinks, infoboxes). The walker reads the
chunk store, emits one Entity per article, an Involves edge for every
outgoing wikilink, and a placeholder Entity for off-corpus link targets.
**No LLM calls.** The full English-language L5 Wikipedia corpus (~51K
articles, ~1.5M placeholder entities) populates in under a minute.

Each Entity gets `enrichment_depth = "structural"` — a one-sentence
description from the article lead, no extracted claims/events/states.

### `extraction_first` — slow, deep

For authored works (novels, philosophy entries, reports) where structure
must be inferred by reading. Drives chunks through Phase 1 (per-section
extraction) → Phase 1b (entity/concept coverage) → clustering → naming →
resolution → tensions → gaps → configuration. Heavy on LLM calls — a
~50-section book takes hours; full Wikipedia would take ~680 days. So
this strategy is reserved for **focused subsets**, not whole corpora.

Output entities get `enrichment_depth = "extracted"` and carry the
richer atom types (Event, State, Claim, Question, plus typed Relations).

### When you'd combine them

The pipeline supports **layered depths**: structure_first runs on the
whole corpus first (cheap baseline), then extraction_first runs on a
chosen subset (deep enrichment for the articles that matter most). The
`enrichment_depth` tag lets the brief assembler know which calibration
to use when prompting downstream — terse for structural, dense for
extracted.

This is exactly the **Tier-2 enrichment workflow**: pick the highest-
value subset of articles (e.g. those that show up as `expected_sources`
in your eval bank) and pay the LLM cost only there.

---

## The pipeline phases (extraction_first)

Each phase reads from a checkpoint and writes the next one — `--resume`
picks up wherever the last run left off, and `--retry-failed` re-runs
just the chapters that errored.

| Phase | What it does | Output |
|---|---|---|
| **0. Seed** (Stage 1a) | Read the first chapter, extract canonical entity names, cache to `seed.json`. Gives every later Phase 1 call a stable name list to align against. | `cache/seed.json` |
| **1. Per-section extraction** | LLM reads each chunk, returns six-facet sketch + anchor keyphrases + `questions_raised`. Schema-bound JSON output. | `_phase1_checkpoint.jsonl`, run-file |
| **1b. Coverage** | A second pass over the same chunk asks "what entities/concepts are present that Phase 1 might have missed?". Schema-free. | merged into Phase 1 output |
| **2. Cluster + name** | Group section sketches by facet (e.g. all claims in the corpus), then have an LLM name each cluster ("Process philosophy argues reality's primary units are dynamic organizations"). | `cache/atlas-clusters.json` |
| **3a. Entity / event resolution** | Deduplicate and merge atoms by alias / Levenshtein distance / cosine similarity, with diacritic-folding and per-token fuzzy matching. | `atlas/atoms.json` (Entity + Event) |
| **3b. State / relation / claim / question resolution** | Build trajectories, Transition / Grounds / Involves edges. Snap participant strings to entity atoms with fuzzy matching. | `atlas/atoms.json` (rest) + `atlas/edges.json` |
| **4. Tensions** | Find candidate Claim ↔ Claim disagreements (intra-cluster + entity-overlap), then optionally pass through an LLM tension classifier. | Tension edges |
| **5. Gaps** | Deterministic gap analysis: ungrounded claims, transitions without trigger events, etc. | gap report |
| **6. Configurations** (domain-dependent) | Phase 8 in the philosophy pipeline: detect higher-order patterns like "Parmenidean static bias as dialectical hinge". | Configuration atoms |
| **7. Cross-corpus** | Bridge atoms across two corpora by entity name + folded-name + cosine similarity. | `atlas/cross_corpus_edges.json` |
| **8. Schema validation** | Compute 8-dimension report (coverage, depth distribution, confidence histogram, orphan analysis, …). Two corpora's reports can be diffed for **convergent gaps** = schema-revision candidates. | `atlas/schema_validation.json` |

Not every pipeline runs every phase — `referential_atlas` (Wikipedia)
runs Phases 0–3; `literary_atlas` and `philosophy_atlas` run further.
The pipeline is selected at `init` time via the recipe.

---

## Examples — what the data actually looks like

These snippets are taken verbatim from real corpora on disk, trimmed for
display.

### Wikipedia: structure_first then extraction_first on the same entity

After `structure_first` runs over the wikipedia chunk store, every
article becomes a structural Entity:

```json
{
  "atom_type": "Entity",
  "data": {
    "id": "entity-1417",
    "canonical_name": "Albert Einstein",
    "entity_type": "article",
    "first_appearance": {
      "chunk_id": "184581",
      "passage_preview": "Albert Einstein was a German-born theoretical physicist…"
    },
    "description": "was a German-born theoretical physicist who is widely held as one of the most influential scientists.",
    "salience": 0.5,
    "enrichment_depth": "structural"
  }
}
```

Every wikilink out of the article becomes an `Involves` edge to the
linked entity, with `wikilink_structural` provenance:

```json
{
  "id": "edge-171088",
  "edge_type": "Involves",
  "source": "entity-1417",        // Albert Einstein
  "target": "entity-103731",      // Aarau (the Swiss town he studied in)
  "confidence": 1.0,
  "provenance": "wikilink_structural"
}
```

Linked entities that don't have their own article in the corpus become
**placeholder entities** — a stub that fills out the graph without
needing source content:

```json
{
  "atom_type": "Entity",
  "data": {
    "id": "entity-103731",
    "canonical_name": "Aarau",
    "entity_type": "article",
    "first_appearance": { "chunk_id": "211801" },
    "description": "",
    "salience": 0.0,
    "enrichment_depth": "structural"
  }
}
```

That's the full structural baseline — done in seconds, no LLM cost.

When `extraction_first` runs Phase 1 over a section, it returns
section-grounded `questions_raised` with passage anchors. Here's the
raw checkpoint record for Einstein's "1900–1905: First scientific
papers" section:

```json
{
  "chapter_id": "sec_00049",
  "questions": [
    "What was Einstein's first scientific paper?",
    "When did Einstein complete his doctoral dissertation?",
    "What are the four famous papers Einstein published in 1905?"
  ],
  "section_extraction": {
    "section_id": "sec_00049",
    "enrichment_depth": "extracted",
    "questions_raised": [
      {
        "content": "What was Einstein's first scientific paper?",
        "anchor": "\"Folgerungen aus den Capillaritätserscheinungen\" in Annalen der Physik"
      },
      {
        "content": "What are the four famous papers Einstein published in 1905?",
        "anchor": "\"his famous papers on the photoelectric effect, Brownian motion, special theory of relativity and equivalence of mass and energy\""
      }
    ]
  }
}
```

Aggregated across all the article's sections and folded into the same
Entity record, the description grows from a one-liner to a dense bag of
section-level questions and anchors:

```json
{
  "atom_type": "Entity",
  "data": {
    "id": "entity-1417",
    "canonical_name": "Albert Einstein",
    "description": "was a German-born theoretical physicist who is widely held as one of the most influential scientists.\nWhat was Albert Einstein's nationality and where was he born?\nWhat are the major contributions of Albert Einstein to physics?\nWhat is E = mc² and what does it represent?\nWhy did Einstein receive the Nobel Prize in Physics in 1921?\n…[~30 KB total across 50 sections]…",
    "enrichment_depth": "extracted"
  }
}
```

That description text is what `--with-atlas` embeds and matches against
the query — it's the substrate of the +29pp source-recall lift on the
wikipedia eval bank.

### Literary / philosophy: the richer atom types in action

Phases beyond 1 surface atom types that don't exist in pure
structure_first output. From the `bk-test` atlas (*Brothers
Karamazov*):

```json
// Event — something that happens at a specific point
{
  "atom_type": "Event",
  "data": {
    "id": "event-0001",
    "description": "Fyodor Pavlovitch marries Sofya Ivanovna after his previous marriage ends.",
    "participants": ["entity-0013", "entity-0001"],
    "evidence": [{ "chunk_id": "sec_0003", "passage_preview": "Very shortly after getting his four-year-old Mitya off his hands…" }],
    "section_position": { "section_id": "sec_0003" },
    "enrichment_depth": "extracted"
  }
}

// State — a condition that holds for an entity over a span
{
  "atom_type": "State",
  "data": {
    "id": "state-0001",
    "entity_id": "entity-0001",
    "label": "A meek and gentle creature subjected to tyranny",
    "section_range": { "start": "sec_0003", "end": "sec_0003" },
    "enrichment_depth": "extracted"
  }
}

// Relation — a typed link between entities
{
  "atom_type": "Relation",
  "data": {
    "id": "relation-0001",
    "label": "Benefactress and tormentor relationship dynamic",
    "participants": ["entity-0042", "entity-0001"],
    "evidence": [{ "chunk_id": "sec_0003", "passage_preview": "benefactress and tormented by this old woman" }],
    "enrichment_depth": "extracted"
  }
}

// Claim — a proposition the corpus asserts, with discourse metadata
{
  "atom_type": "Claim",
  "data": {
    "id": "claim-0001",
    "content": "The narrator asserts that Fyodor Pavlovich's attraction to Sofya was purely sensual, not romantic.",
    "discourse_act": "assert",
    "epistemic_status": "confident",
    "scope": "fictional",
    "evidence": [{ "chunk_id": "sec_0003", "passage_preview": "in a man so depraved this might mean no more than sensual attraction" }]
  }
}

// Question — an open question the text raises
{
  "atom_type": "Question",
  "data": {
    "id": "question-0001",
    "content": "What was the true nature or motivation behind Fyodor Pavlovitch's sudden change in behavior toward his children?",
    "question_type": "thematic",
    "resolution_status": { "kind": "open" }
  }
}
```

Note the recurring pattern: every atom carries `evidence`
(`chunk_id` + `passage_preview`) so a downstream consumer can always
trace any claim or relation back to the source passage that grounded
it. This is what makes the atlas *auditable* — nothing is hallucinated
from outside the corpus, and every edge in the graph has a citation
chain.

---

## End-to-end CLI workflow

The reference workflow for a Tier-2 Wikipedia enrichment looks like:

```bash
# 1. Triage — find which articles are worth deep enrichment.
sovereign enrich triage-candidates wiki-l5-struct \
    --top-k 200 --output triage.json

# 2. Scaffold a Tier-2 workspace targeting those articles.
sovereign enrich init wiki-tier2-bank \
    --pipeline referential_atlas \
    --from-corpus wikipedia \
    --include-articles bank_articles.txt   # or triage.json

# 3. Extract — the long-running step. Per-chapter checkpointing means
#    you can ctrl-c and `--resume` freely.
sovereign enrich extract wiki-tier2-bank --full --resume

# 4. Retry just the chapters that hit parse failures.
sovereign enrich extract wiki-tier2-bank --retry-failed --terse

# 5. Finalize — write the canonical run-file from the checkpoint.
sovereign enrich extract wiki-tier2-bank --finalize

# 6. Build the structural baseline atlas (whole-corpus; cheap).
sovereign enrich ingest wiki-l5-struct \
    --strategy structure_first --source-corpus wikipedia

# 7. Eval — measure retrieval quality before/after enrichment.
sovereign eval run \
    --bank sovereign/bench/wikipedia/questions.toml
sovereign eval run \
    --bank sovereign/bench/wikipedia/questions.toml \
    --with-atlas wiki-l5-tier2-full --atlas-depth extracted
```

---

## Atlas-grounded retrieval — `--with-atlas`

This is how the atlas earns its keep at query time.

When you pass `--with-atlas <atlas-corpus-id>` to `sovereign eval run`,
the runner loads the atlas's `atoms.json`, embeds every Entity's
`name + aliases + description` once at startup, and then per question:

1. Cosines the query embedding against every Entity embedding.
2. Takes top-K (default 3, tunable with `--atlas-top-k`).
3. Wraps each as a virtual `ScoredChunk` (`title = canonical_name`,
   `content = description`, `corpus_id = "atlas:<id>"`).
4. Merges into the regular hybrid-search hit set, re-sorts, truncates.

Because atlas entries are dense, hand-curated grounding (or LLM-extracted
in the Tier-2 case), they **lift both source-recall** (the title lands
in the top-K) **and fact-recall** (the description text adds to the
fact-scoring snippet bag).

Filters keep the embed pass tractable on wiki-scale atlases:

- `--atlas-min-description-chars 200` (default) — drops structural
  one-liners; keeps actually-enriched entries.
- `--atlas-depth extracted` — explicit depth allowlist.
- `--atlas-max-entries N` — hard cap; safety net for misconfigured runs.

Without filtering, embedding 50K+ Wikipedia entities at ~50ms each takes
~40 minutes. With the default filter on a Tier-2 atlas, the same step
embeds ~50 entries in ~5 seconds.

---

## Where the data lives

```
~/.svrnmesh/
├── indexes/
│   ├── wikipedia/                          # the source chunk store
│   │   ├── chunks.lance/                   # IVF-PQ + Tantivy hybrid
│   │   └── _corpus_meta.json
│   ├── wiki-l5-struct/                     # structure_first atlas (cheap baseline)
│   │   └── atlas/
│   │       ├── atoms.json                  # ~1.5M atoms (mostly placeholders)
│   │       ├── edges.json
│   │       └── schema_validation.json
│   └── wiki-l5-tier2-full/                 # extraction_first atlas (Tier-2 deep)
│       └── atlas/{atoms,edges}.json
└── enrichment/
    └── wiki-tier2-bank/                    # extraction workspace
        ├── config.json                     # chat/embed model, max_tokens, etc.
        ├── chapters.json -> ../indexes/wiki-tier2-bank/chapters.json
        ├── cache/                          # seed.json, atlas-clusters.json
        ├── exemplars/                      # few-shot exemplars for prompts
        └── runs/
            ├── _phase1_checkpoint.jsonl    # per-chapter success/failure log
            └── questions-subset-001.json   # canonical run-file
```

Three rules of thumb:

1. **Indexes contain searchable on-disk artifacts.** Lance tables, atlas
   atoms/edges, schema validation reports.
2. **Enrichment workspaces contain the in-flight machinery.** Configs,
   per-phase checkpoints, exemplars, run files. Once an extraction is
   `--finalize`'d, the result lands in the corresponding index dir.
3. **Recipes live in the repo, not the data dir.** Bank TOMLs,
   pipeline prompts, source mappings.

---

## Measuring whether enrichment helped

Three eval surfaces, each measures something different:

- **`sovereign eval run --bank …`** — runs hybrid retrieval against the
  chunk store, scores against `expected_sources` (titles) and
  `expected_facts` (substring matches). The honest e2e baseline.
  Add `--with-atlas` to see atlas lift.
- **`sovereign enrich atlas-eval <atlas> --bank …`** — pure atlas-only
  retrieval via tokenized title-overlap. Fast to iterate on atlas
  changes without going through hybrid search.
- **`sovereign enrich schema-report <corpus>`** / **`schema-review <a> <b> …`**
  — measures atlas *health* (coverage, orphans, depth distribution,
  cross-corpus convergent gaps). Not a quality metric — a structural
  audit.

A typical signal-validation cycle:

```
baseline atlas-eval     →  shows which entities the atlas has (or lacks)
baseline `eval run`     →  shows raw chunk-retrieval recall
add Tier-2 enrichment   →  expensive, targets the bank's expected_sources
re-run atlas-eval       →  confirms entity content is improving
re-run `eval run`       →  confirms it lifts e2e retrieval
                            (fact-recall lift = atlas content reaching the snippet bag)
```

This was the loop that took the wikipedia bank from 50% / 71% sources/facts
to 79% / 83% after a Tier-2 pass on 52 high-value articles.

---

## Pointers for going deeper

- **[ENRICHMENT_V2.md](./ENRICHMENT_V2.md)** — landing-by-landing status, the
  durable record of what's shipped and what's next.
- **`src/enrichment/atlas/`** — the closed surface (atoms, edges, writer,
  reader, registry, strategies, analysis).
- **`src/enrichment/pipeline/pipelines/`** — the open surface: one
  module per pipeline (`literary_atlas`, `philosophy_atlas`,
  `referential_atlas`) with its prompt assets.
- **`sovereign/crates/sovereign-cli/src/enrich_cmd/`** — every CLI
  subcommand (`init`, `extract`, `ingest`, `triage-candidates`,
  `atlas-eval`, `atlas-resolve`, `atlas-tensions`, `schema-report`,
  `schema-review`, `atlas-cross-corpus`).
- **`sovereign/crates/sovereign-cli/src/eval_cmd/`** — `eval run` and
  the `--with-atlas` wiring.

When something breaks in production, the most useful starting point is
usually `_phase1_checkpoint.jsonl` for the relevant workspace — every
chapter outcome (success or failure with reason) is one JSON line.
