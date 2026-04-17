# Knowledge Bases

Sovereign indexes curated reference sources locally. Every query searches these knowledge bases before generating a response — the model answers from verified sources rather than hallucination. Web search supplements for current events and gaps.

← [back to README](../README.md)

## Available corpora

| Corpus | Description | Size | License |
|---|---|---|---|
| **Wikipedia** | 6.8M English articles | 55 GB indexed | CC BY-SA 4.0 |
| **Stanford Encyclopedia of Philosophy** | Peer-reviewed philosophy articles (via HuggingFace dataset) | 0.5 GB indexed | CC BY-NC-ND 4.0 |
| **OpenAlex** | 250M+ scholarly abstracts with citations | 45 GB indexed | CC0 |
| **Stack Exchange** | Expert Q&A across 170+ communities (score ≥ 3) | 40 GB indexed | CC BY-SA 4.0 |
| **Project Gutenberg** | 70,000+ public domain books | 25 GB indexed | Public Domain |
| **CRS Reports** | US Congressional policy analysis | 4 GB indexed | Public Domain |

## Tiers

The Sovereign desktop app offers four curated tiers at setup time. Pick by storage budget and research focus:

- **Essential** (55 GB) — Wikipedia only. Broad general knowledge.
- **Research** (105 GB) — Wikipedia + SEP + OpenAlex + CRS. Academic and policy research.
- **Technical** (95 GB) — Wikipedia + Stack Exchange. Programming and engineering.
- **Full** (170 GB) — All corpora.

From the CLI, install corpora individually:

```sh
sovereign corpus list              # installed + available
sovereign corpus install wikipedia
sovereign corpus install sep
sovereign corpus status            # shard download / index progress
```

## How it works

Knowledge bases are defined in `data/corpora.toml`. The corpus manager downloads source files (Parquet, XML, JSONL), parses them with streaming parsers (never loading full corpora into memory), and indexes chunks via SQLite FTS5 full-text search.

Every query — regardless of how the router classifies it — searches the local knowledge base. Results are injected as context before the model generates a response. Provenance metadata records which corpora were consulted and how many chunks matched.

## Coverage-aware search pipeline

The unified `search` tool replaces separate knowledge and web search tools:

1. **Local search** — FTS5 text search across all indexed corpora.
2. **Coverage assessment** — Heuristic + optional LLM evaluation of result quality.
3. **Web fallback** — If local results are insufficient and web search is configured.
4. **Synthesis** — Cited answer with source attribution and provenance.

Budget tracking gates web search usage. The system is designed to work fully offline with local knowledge bases as the primary source.

## Corpus integrity

Corpus definitions carry optional `signature` and `signed_by` fields. The system distinguishes three trust levels — community-reviewed, author-signed, and unsigned — and records the level in every query's provenance metadata. Corpus definitions include a `mesh_sharing` flag for license-aware index transfer control; for example, SEP's CC-BY-NC-ND license sets `mesh_sharing = false`, so its index is never shared with mesh peers.
