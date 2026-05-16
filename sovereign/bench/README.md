# `sovereign/bench/` — single rollup surface for every enrichment bench

One command:

```bash
sovereign bench all                       # discover + score every bench, diff vs baseline
sovereign bench all --filter obsidian     # only matching <group>/<bench-id> paths
sovereign bench all --update-baseline     # write current run as new baseline
sovereign bench all --rebuild --filter <id>   # re-extract atlas (enrichment lane only) then score
sovereign bench all --report /tmp/r.json  # persist combined results bundle
```

**Prompt-tuning loop (no CLI rebuild required):**

```bash
export SOVEREIGN_PROMPT_DIR=~/prompt-overlays
mkdir -p $SOVEREIGN_PROMPT_DIR/<pipeline-id>
cp corpus-engine/src/enrichment/pipeline/pipelines/<pipeline-id>_prompts/<file>.md \
   $SOVEREIGN_PROMPT_DIR/<pipeline-id>/
# edit the overlay copy
sovereign enrich build <corpus>   # next run picks up the overlay
```

Without the env var (or with the file absent at the overlay path), pipelines use the compile-time-baked prompt. Edit the overlay, re-run; compare baselines via `sovereign bench all`.

**Spinning up a new bench:**

```bash
sovereign bench scaffold <corpus-id> --output sovereign/bench/<group>/<id>.toml
# review + prune + add forbidden_* blocks + tighten name_contains_any
sovereign bench all --filter <group>/<id> --update-baseline
```

`scaffold` reads the corpus's `atoms.json` and samples 10 entries per typed axis + base atom kind. The draft encodes what the extractor PRODUCED — review every entry, tighten the needles, add anti-tests. Verified: a fresh scaffold against obsidian-vault scores 100% F1 across all 5 typed axes against the same atlas (drawn from atoms; matches atoms).

## Three scoring surfaces

| Surface | Scores | Runs via | Today's coverage |
|---|---|---|---|
| **Enrichment-eval** | atom F1 per axis against a hand-authored golden | in-process `score_corpus` | obsidian, literary (bk-book-1, dubliners-3), philosophy |
| **Retrieval (bare)** | per-question source_recall / fact_recall | subprocess `eval run` (no `--synth`) | obsidian, sep, wikipedia |
| **Retrieval + synth (full chat pipeline)** | same scoring, but answers come from `runtime.handle_message_stream` | subprocess `eval run --synth` (via `bench all --synth`) | same banks; opt-in cost ~5-30s/q |

Two complementary lever sets. Atom F1 measures projection correctness; retrieval+judge measures whether the resulting atlas serves user value. Per the lever framing: **no single aggregate F1 across corpora or across surfaces** — per-corpus + per-axis + per-category only.

## Three views of the same retrieval event

A single retrieval (`bench all --synth`) produces THREE complementary scoring views — surfaced side-by-side in the rendered scoreboards. They measure different things, and conflating them produces misleading regression signals.

| View | Question it answers | Headline when |
|---|---|---|
| **answer-equiv** (LLM judge) | "Did the answer convey the expected fact?" | always when `--synth`. Strongest correlate of user value. Semantic equivalence credit — paraphrase is OK. |
| **title-coverage** (rigid src) | "Was the bank's declared canonical source title in the retrieved bag?" | retrieval reach diagnostic. Misleading as a quality metric when a sibling corpus carries equivalent content (e.g. SEP article ranks higher than the Wikipedia overview on a comparison question — chat correctly serves the better chunk, bench grades it a miss). |
| **keyword-match** (strict fact) | "Did the answer text contain the expected substring?" | calibration metric. Penalises paraphrase even when the fact is conveyed. Useful for prompt-iteration that's TRYING to tighten quoting behavior. |

The bench surfaces all three. **`answer-equiv` is the canonical user-value score.** `title-coverage` and `keyword-match` are diagnostic — useful for drilling, misleading as headline numbers.

This framing is a deliberate move toward the dual-stream legibility principle: the bank's `expected_sources` is a **narrative claim** ("here's where this answer should come from"), the actual retrieval is the **reality**, and the judge column says whether they were semantically equivalent. Treat divergence as legible, not as failure.

### Fourth lens — `meta_atlas_hits`

Per-question JSON carries `meta_atlas_hits: [{entity, corpus_id, articulation, stability, chunks_added}]`, one row per anchor (max 3 per matched canonical entity — one per articulation axis with a dominant anchor) the cross-corpus meta-atlas surfaced for the turn (Move 5). Read it as the answer to **"which canonical entities did the meta-atlas recognise, and which stream did each anchor serve?"**:

- `articulation = "inventory"` → the structural-map anchor. Broad overview content. Wikipedia article, vault reference card, code symbol entry.
- `articulation = "argument"` → the articulated-claim anchor. SEP claim, design-doc assertion, judicial opinion, essay reasoning.
- `articulation = "trace"` → the lived-practice anchor. Conversation history, journal entry, newsworthy event description.
- `stability = "frozen"` → snapshot release; re-ingest replaces wholesale.
- `stability = "versioned"` → active revision; expected to delta-ingest.
- `stability = "rolling"` → continuously updated within a window.
- `chunks_added = 0` → the meta-atlas surfaced an anchor but the focused per-corpus search returned nothing usable. Useful diagnostic when title-coverage stays flat despite meta-atlas hits — typically means the atlas's `first_appearance` chunk is missing from the live index (a sign the corpus was rebuilt without the atlas re-running).

The meta-atlas is built by `sovereign meta-atlas build` (per-atom rule-based classifier in `corpus-engine/src/meta_atlas/classifier.rs`) and persisted to `~/.sovereign/meta-atlas/canonical_atoms.json`. Per-corpus stability lives in each corpus's `_corpus_meta.json::stream` block — populated at ingest time, backfilled by `sovereign corpus stream-axes` for legacy corpora.

The synthesis prompt uses these tags to sub-bucket the corpus section into three named streams (`## Broad map (inventory)` / `## Articulated claims (arguments)` / `## Lived practice (traces)`) so the model can compose the streams as distinct epistemic sources. Chunks without an `articulation` tag fall through to the existing `## From knowledge base` section — no-regression on un-meta-tagged retrieval.

## Propagation: bench → chat

`bench all` (default retrieval-mode) exercises `CorpusIndex::search_with_rerank` — the same primitive `Runtime::search_corpus_indexes` calls from desktop chat. Improvements to embed quality, BM25, vector cosine, atlas-tier boost, and the cross-encoder reranker propagate to chat 1:1.

`bench all --synth` drives `runtime.handle_message_stream` — the **exact same entry point the desktop chat surface uses**. Adds coverage of the chat-only layers: intent classifier, router (KnowledgeQuery/DeepQuery/etc), sensitive-corpus oracle, kind filter, and the synthesis LLM itself. Use this for end-to-end propagation gates.

```bash
# Cheap: tune retrieval primitives. Same code chat uses for embed+search+rerank.
sovereign bench all                       # ~minutes
# Full-chain: confirm chat-side wins. Same code chat uses end-to-end.
sovereign bench all --synth               # ~30s/question, opt-in for nightly / release
```

Baselines are stored separately per mode (`baselines/<bench>/` vs `baselines/<bench>-synth/`) so the two never overwrite each other. Empirical 2026-05-15 obsidian baseline:

| Mode | fact_recall | source_recall |
|---|---|---|
| retrieval | 0.96 | 1.00 |
| synth | 0.56 | 0.75 |

The 0.96→0.56 / 1.00→0.75 gap is the lever surface for the chat-only layers (classifier routing, sensitive filtering, synthesis model selection of which retrieved chunks to actually surface).

## Filesystem convention

```
sovereign/bench/<group>/
    <bench>.toml                 # enrichment golden OR retrieval question bank
    baselines/<bench>/
        latest.json -> 2026-MM-DD.json
        2026-MM-DD.json
        ...
```

`<group>` is `obsidian`, `literary`, `philosophy`, `sep`, `wikipedia`, etc. Bench id is the TOML filename stem (`golden`, `bk-book-1`, `questions`). Each TOML carries its corpus binding: `[meta] corpus_id = "..."` (enrichment) or `[bank] corpus = "..."` (retrieval).

Question banks for sep + wikipedia are symlinks into `sovereign-recipes/<corpus>/eval/` so there's one source of truth.

## Per-corpus READMEs

- [`obsidian/`](obsidian/) — enrichment golden against the author's vault + retrieval bank.
- [`literary/`](literary/) — Brothers Karamazov Book I + Dubliners 3-story fixture.
- [`philosophy/`](philosophy/) — three small SEP slices for atlas calibration.
- [`sep/`](sep/README.md) — SEP retrieval bank + canonical multi-iteration baselines (synced 2026-05-15 from pre-monorepo).
- [`wikipedia/`](wikipedia/README.md) — Wikipedia retrieval bank + pre-/post-enrichment baselines.

## Lever map

When a regression appears, the cross-corpus matrix tells you which subsystem owns the lever:

| Lever | Owner |
|---|---|
| Per-axis F1 (`mechanism`, `position`, ...) | resolver projection (`resolve_type_extensions`) + typed-extension prompt |
| Base atom F1 (`person`, `concept`, ...) | Phase 1 prompt + Phase 3 facet-naming |
| `source_recall` | retrieval ranker (BM25 + vector + atlas-tier) |
| `fact_recall` | retrieval ranker AND atlas chunk content |
| `essay_readiness` | atlas navigation (claim / tension / configuration atoms) + judge prompt |

If `mechanism` drops on every enrichment corpus → projection or typed-extension prompt change. If `mechanism` drops only on obsidian → corpus-specific prompt tuning. If `source_recall` drops across retrieval banks → ranker or index regression. Same axis × different corpora reveals scope.

## See also

- [`BENCH_LOOP.md`](BENCH_LOOP.md) — process theory: the prompt-iteration loop, what generalises vs what coaches, how to add a new typed axis.
- [`HISTORY.md`](HISTORY.md) — campaign-level findings from prior bench work (pre-2026-05-10 archive purge).
- [`sep_atlas/README.md`](sep_atlas/README.md) — driver for parallel SEP enrichment across mesh peers.
