# Knowledge Bases

Sovereign indexes curated reference sources locally. Every query searches these knowledge bases before generating a response — the model answers from verified sources rather than hallucination. Web search supplements for current events and gaps.

← [back to README](../README.md)

## Available corpora

| Corpus | What it is | Indexed size | License |
|---|---|---|---|
| Wikipedia | Wikipedia Core — ~51K Vital Articles, expandable in place to the full 6.7M-article dump | 2.5 GB | CC BY-SA 4.0 |
| Stanford Encyclopedia of Philosophy | Peer-reviewed philosophy articles | 6 GB | CC BY-NC-ND 4.0 |
| Stack Exchange | Expert Q&A across many communities | 120 GB | CC BY-SA 4.0 |
| OpenAlex | Scholarly abstracts and metadata | 500 GB | CC0 |
| Project Gutenberg | Public-domain books | 0.3 GB | Public Domain |
| CRS Reports | US Congressional policy analysis | 5 GB | Public Domain |

Sizes are the indexed footprint and shift as corpora are added; `sovereign corpus list` shows the live catalog and what's installed.

## Tiers

The desktop app groups corpora into four tiers at setup time. Pick by storage budget and focus:

- Essential — Wikipedia Core (plus Simple English). The general-knowledge baseline, a couple of GB.
- Research — Essential plus SEP, OpenAlex, and CRS Reports. Academic and policy work, and large: OpenAlex alone is around 500 GB.
- Technical — Essential plus Stack Exchange. Programming and engineering, around 120 GB.
- Full — every corpus.

From the CLI, install corpora individually:

```sh
sovereign corpus list              # installed + available
sovereign corpus install wikipedia
sovereign corpus install sep
sovereign corpus status            # shard download / index progress
```

## How it works

Corpora are defined as recipes in `sovereign-recipes/registry.toml`. The corpus manager downloads source files (Parquet, XML, JSONL), parses them with streaming parsers (never loading full corpora into memory), and indexes chunks via SQLite FTS5 full-text search.

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
