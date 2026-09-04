# Wiki seed-table migration — the cosine probe

**Question.** Wikipedia's 1.67M atoms have no embeddings. Every atom carries
`first_appearance.chunk_id`, and `chunks.lance` already holds a 1024-d vector
per chunk under the same model the query slot runs (`qwen-embedding-0.6b`,
verified in `_corpus_meta.json`). Can `atoms_ann.lance` be MIGRATED from those
vectors instead of embedded fresh — 0 embed calls against 33.1 hours?

**Bar, registered before the run** (the prebuilt-embedding probe's bar,
`project_prebuilt_snapshot_embedding_probe`): the borrowed chunk vector stands
in for the atom's own vector only if a ~200-atom sample holds cosine >= 0.92.

**Verdict: REFUSED.** 0.0% of 221 clear it. Median 0.323.

| series | min | p5 | p25 | p50 | p75 | p95 | max | mean | >=.92 |
|---|---|---|---|---|---|---|---|---|---|
| `cos_query_side` | 0.081 | 0.169 | 0.240 | **0.323** | 0.395 | 0.504 | 0.914 | 0.327 | **0.0%** |
| `cos_doc_side` | 0.093 | 0.159 | 0.257 | 0.324 | 0.400 | 0.544 | 0.907 | 0.331 | 0.0% |
| `cos_query_side_random` | 0.034 | 0.072 | 0.124 | 0.163 | 0.199 | 0.261 | 0.321 | 0.163 | 0.0% |
| `cos_doc_side_random` | 0.006 | 0.079 | 0.138 | 0.181 | 0.245 | 0.340 | 0.422 | 0.193 | 0.0% |

**Root cause, measured:** 214 of 221 sampled atoms (96.8%) have an EMPTY
`description`. The atom's whole embed text is a bare title, plus an alias list
for 90 of them. The chunk vector embeds a full article lead. The 7 atoms that
DO have a description only reach mean 0.544, so this is not an artifact of the
empty ones.

**Why the refusal is trustworthy** (ARCH §18.4 — validate the instrument
before the result):

- The random control separates. Matched pairs sit at 0.323 against an
  unrelated-chunk floor of 0.163, so the probe has resolution; there is real
  signal, an order of magnitude short of "the same vector".
- Both embedding conventions were run. Query-side applies the Qwen3-Embedding
  instruction prefix (`model_family.rs`, what `inference_to_embed_query_fn`
  and therefore `backfill_ann` use); document-side is raw. They agree to 0.004
  on the median, so a prefix mismatch is not the explanation.
- The join key is sound: 221/221 sampled atoms carry a `chunk_id` and every one
  resolved in `chunks.lance`.
- Zero external model tokens; 442 embed calls to the local daemon slot, 6.2 s.

**What the bar does not answer.** It asks "is the borrowed vector
interchangeable with the atom's own?" A seed table's job is to make the walk
find the right articles, and seeding wikipedia on the article's LEAD PASSAGE
rather than on its title is plausibly better retrieval, not worse — the title
is the impoverished text here. That is a retrieval question with a retrieval
bar (the wikipedia lane A/B), and it is why the seat re-barred rather than
abandoning the migration. Both directions of that A/B get reported (§18.6).

## Reproducing

```sh
python3 sample_atoms.py     # even sample of ~220 atoms across atoms.json
python3 seed_probe.py       # chunk join + both embeds + cosine, needs the daemon
```

`sample_atoms.py` walks the whole 756 MB `atoms.json` and takes every
`TOTAL/220`-th atom, so the sample spans the alphabet rather than the head; it
scanned 1,666,146 atoms, matching `atlas/_summary.json` exactly.
`seed_probe_results.json` is the per-atom row set behind the table.

## Known caveat, carried forward

`chunks.lance` `id` is NOT unique — 1 of the 221 sampled ids had 2 rows. Any
atom -> chunk join therefore needs a stated dedupe rule.
