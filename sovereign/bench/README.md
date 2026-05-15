# `sovereign/bench/` — single rollup surface for every enrichment bench

One command:

```bash
sovereign bench all                       # discover + score every bench, diff vs baseline
sovereign bench all --filter obsidian     # only matching <group>/<bench-id> paths
sovereign bench all --update-baseline     # write current run as new baseline
sovereign bench all --rebuild --filter <id>   # re-extract atlas (enrichment lane only) then score
sovereign bench all --report /tmp/r.json  # persist combined results bundle
```

**Spinning up a new bench:**

```bash
sovereign bench scaffold <corpus-id> --output sovereign/bench/<group>/<id>.toml
# review + prune + add forbidden_* blocks + tighten name_contains_any
sovereign bench all --filter <group>/<id> --update-baseline
```

`scaffold` reads the corpus's `atoms.json` and samples 10 entries per typed axis + base atom kind. The draft encodes what the extractor PRODUCED — review every entry, tighten the needles, add anti-tests. Verified: a fresh scaffold against obsidian-vault scores 100% F1 across all 5 typed axes against the same atlas (drawn from atoms; matches atoms).

## Two scoring surfaces

| Surface | Scores | Runs via | Today's coverage |
|---|---|---|---|
| **Enrichment-eval** | atom F1 per axis against a hand-authored golden | in-process `score_corpus` | obsidian, literary (bk-book-1, dubliners-3), philosophy (free-will-debate, stoicism-mini, virtue-ethics-fragments) |
| **Retrieval + LLM-judge** | per-question source_recall / fact_recall / `essay_readiness` | subprocess `sovereign eval run` (needs live daemon) | obsidian, sep, wikipedia |

Two complementary lever sets. Atom F1 measures projection correctness; retrieval+judge measures whether the resulting atlas serves user value. Per the lever framing: **no single aggregate F1 across corpora or across surfaces** — per-corpus + per-axis + per-category only.

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
