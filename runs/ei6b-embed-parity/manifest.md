# ei6b-embed-parity — does a bare endpoint embed the way the corpus was built?

Run 2026-09-07, order ei-6b-embedder-parity. **Zero egress**: the probe that
ei-6 spent 12 minutes and 875 MB to reach is reproducible offline, because
`CorpusEngine::probe_embedding_space`
(`corpus-engine/src/engine/ingest_prebuilt.rs`) re-embeds `chunk.content`
verbatim and cosines it against the stored vector, and `embed_input == content`
at ingest (`corpus-engine/src/engine/ingest.rs:1314-1317`). So: read
`chunks.lance` with python-lance, POST to an embed endpoint, compare. Same
comparison, no download.

## What it refuted

The order's premise — that corpus-mcp sends raw text where the daemon prepends
instructions and appends `<|endoftext|>`, and that this explains `sep`'s
probe_cosine 0.6822. **The bare endpoint already reproduces the daemon at
0.9956 raw / 0.9998 prepared.** The gap is not in the caller.

## Instrument validated before use (ARCH §18.4)

Daemon `/v1/embeddings` vs its OWN corpora's stored vectors, n=3 chunks each:

| corpus | built | cosine |
|---|---|---|
| wessex-hoard | 2026-09-02 | 0.9999 |
| alignment | 2026-07-24 | 0.9999 |
| brothers-karamazov-book-1 | 2026-08-07 | 0.9995 |
| **sep** | 2026-04-09 | **0.6980** |

The daemon cannot reproduce sep either. That eliminates the caller.

## Four arms (`arms.py`), n=3 real sep chunks, vs the stored vector

A = daemon, B = bare llama-server raw, C = bare +EOS, D = bare +query instruction.

| chunk | chars | A | B | C | D | B vs A | C vs A |
|---|---|---|---|---|---|---|---|
| 4 | 628 | 0.7399 | 0.7510 | 0.7395 | 0.6669 | 0.9957 | 0.9999 |
| 6 | 777 | 0.6030 | 0.6118 | 0.6063 | 0.5976 | 0.9948 | 0.9997 |
| 8 | 1269 | 0.6536 | 0.6560 | 0.6531 | 0.6437 | 0.9962 | 0.9999 |
| **mean** | | 0.6655 | 0.6729 | 0.6663 | 0.6361 | **0.9956** | **0.9998** |

## Root cause (`pool_arms.py`, `extra_arms.py`) — pooling, at 0.9997

| subject | built | `--pooling last` | `--pooling mean` | `cls` |
|---|---|---|---|---|
| sep chunks (doc) | 2026-04-09 | 0.7069 | **0.9997** | 0.2730 |
| wikipedia chunks (doc+EOS) | 2026-04-29 | 0.7192 | **0.9573** | — |
| wessex-hoard chunks (doc) | 2026-09-02 | **0.9968** | 0.6615 | 0.1480 |
| sep-al-farabi atlas seeds (query+EOS) | 2026-09-05 | **0.9610** | 0.5131 | — |

Read it as a date column. Everything before ~July is mean-pooled; everything
since is last-pooled. The sep-al-farabi row is the control: those seeds were
written by ei-3b on the CURRENT stack, same GGUF, opposite pooling — **the
stack flipped, not the model.**

## Query side (`seed_arms.py`), wessex-hoard's atlas seed table

Seeds are embedded QUERY-side over `"<canonical_name>: <description>"`:

| arm | mean cosine vs stored seed |
|---|---|
| raw | 0.8605 |
| raw + EOS | 0.8558 |
| + query instruction | 0.9841 |
| + query instruction + EOS | **0.9888** |

+0.128 for the query instruction. This is what ei-6b item 2 buys, on a corpus
that is not stale.

## Caveats, stated

`wikipedia`'s 0.9573 and one sep-al-farabi atom's 0.8918 are TEXT
RECONSTRUCTION error, not space error — the seed text is inferred as
`"<canonical_name>: <description>"` from a 0.9888 fit on wessex-hoard, and
wikipedia's chunker may title-prepend differently than my read of `content`.
The direction is not in doubt at +0.24 / -0.45; the third decimal is.

## Reproducing

Needs `python3` with `lance`, a daemon on :9741 for arm A, and
`llama-server -m Qwen3-Embedding-0.6B-Q8_0.gguf --embeddings --pooling <p>`
inside the `sovereign-vulkan` toolbox. Each script takes the port; the vector
dumps they read are regenerated from `~/.svrnmesh/indexes/<id>/chunks.lance`
and are left out of git (140 KB of float arrays, derivable in seconds).
Raw logs: `test-artifacts/ei6b-mechanism/` (gitignored).
