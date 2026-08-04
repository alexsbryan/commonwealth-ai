# Sizing gate — where does the second conversation actually rank?

**Run 2026-08-03. Report-only, no bank authored.** This is the cheap gate the
corrected design in `README.md` calls for: it sizes both investment options
*before* anyone writes 40 questions, and it needs no authored questions to do it.

Probe: `corpus-engine/examples/bridge_rank_probe.rs` (production hybrid search
via `CorpusIndex::search`). Candidate miner + sample: SQL over `chunk_entities`,
reproducible (md5-ordered, no RNG). Raw output stays in scratchpad — it carries
entity surface forms and conversation UUIDs from a personal archive. Only
aggregates appear here.

## Bottom line

**Neither option has a large opportunity, and the measurement is too
phrasing-unstable to justify funding a bench on it as designed.** Under
realistic phrasing, the per-document PPR prior (option 2) has a ~22% operating
window and a true cross-document bridge (option 3) a ~39% one — but bucket
assignment flips for **52% of candidates** on a mere change of question wording.
That instability is larger than either effect being sized.

## What was measured

For 90 candidate pairs — entity `E` appearing in exactly two conversations `A`
and `B`, stratified 30/30/30 by mention count — under a query naming `E`, where
does conversation `B` land in the retrieval ranking?

| `rank_B` | reading |
|---|---|
| ≤ 10 | cosine/FTS already surfaces B. No headroom for any entity mechanism. |
| 11–20 | **option 2's window** — B is inside the pool `rerank_conv_chunks_via_ppr` re-sorts, so the prior *can* promote it. |
| > 20 or absent | B is outside the pool. PPR re-ranks in place and never adds, so only a real cross-document bridge could reach it. |

Pool = 20 (`KQ_MERGED_LIMIT`, `prompts.rs:362`). Two phrasings: `bare` (the
entity alone — maximum entity focus, optimistic) and `nat` ("What did we discuss
about E?" — closer to a real question). Three legs, gated independently per
`search.rs:344-351`: hybrid, vector-only, FTS-only.

## Instrument validation (ARCH §18.4)

`rank_A` is the positive control — conversation A holds *more* mentions of E, so
a query naming E that cannot retrieve A is not exercising retrieval. **Hybrid
control passes 90.0% (bare) / 85.6% (nat).** Every aggregate below is computed
over controlled rows only.

The vector leg fails its own control 31–37% of the time. Dense retrieval alone
frequently cannot retrieve even the entity's *primary* conversation — a known
weakness on proper nouns, and load-bearing for reading the decomposition below.

Query embeddings carry the production instruction prefix
(`model_family.rs:302-304`); `/v1/embeddings` does not add it
(`oicp-client/src/lib.rs:54-56`). Without it the cosines would be
self-consistent but would not be what retrieval sees.

## Headline

Hybrid leg, controlled rows:

| phrasing | n | ≤10 (no headroom) | 11–20 (**option 2**) | >20/absent (**option 3 only**) |
|---|---|---|---|---|
| `nat` (realistic) | 77 | 39.0% | **22.1%** | **39.0%** |
| `bare` (optimistic) | 81 | 63.0% | **19.8%** | **17.3%** |

**Union ceiling.** B is in the top-20 under *at least one* leg for **68.8%** of
candidates (`nat`). So ~31% is unreachable by any re-ranking-only mechanism, no
matter how good — that is option 3's irreducible share.

## The finding that should govern the decision

**Phrasing stability is 48%.** Of 75 candidates controlled under both phrasings,
only 36 land in the same bucket. Migrations are not noise around a boundary —
14 candidates go from ≤10 straight to >20/absent.

This is the design's own "variance first" rule (ARCH §18.5) firing before a
single question was authored. The README's power calculation assumed a paired
sd of ~0.3 and concluded n=20 detects a 0.20 effect. A 52% bucket-flip rate
from wording alone implies substantially more variance than that, so the
proposed 40-question bank would be underpowered for the effects measured here.

## Decomposition — FTS is doing the entity layer's job

B *contains* E's surface form by construction, so keyword search finds it
directly. Share of candidates with B inside the 20-slot pool:

| leg | `bare` | `nat` |
|---|---|---|
| FTS-only | 83.5% | 62.5% |
| hybrid | 82.3% | 58.3% |

FTS-only ranks B *better* than hybrid in ~53% of paired candidates (hybrid
better in 25–33%), though pool *membership* differs by only 1–4 points. The
practical reading: fusion is not costing much, but essentially all of B's
findability is coming from the keyword leg, not the vector leg — and neither is
coming from the entity graph.

## Where the opportunity concentrates

By entity type (`nat`/hybrid, controlled):

| label | n | ≤10 | 11–20 | >20/absent |
|---|---|---|---|---|
| Person | 21 | 38.1% | **33.3%** | 28.6% |
| Location | 22 | 40.9% | 22.7% | 36.4% |
| Organization | 28 | 39.3% | 7.1% | **53.6%** |

By mention strength: **strongly-attested entities are *worse***, not better —
43.3% of their B's fall outside the pool vs 30.0% for thin ones. The likely
mechanism is crowding: a strong entity means conversation A dominates the
ranking harder (`MAX_CHUNKS_PER_ARTICLE_AT_MERGE = 10`, so one conversation can
hold half the pool).

Any bench built after this should be Person-weighted and should not assume
strong entities are the easy cases.

## Limits — state these with any use of these numbers

- **This sizes the opportunity, not the effect.** It measures where B ranks
  *before* PPR runs. Whether the prior actually *promotes* B inside its 22%
  window is unmeasured and is a separate experiment. A mechanism with a 22%
  window can still deliver zero.
- **Raw `CorpusIndex::search`, not the full pipeline.** Production applies
  boosts, expansions, a noise floor, grounding and the per-document cap before
  truncating. "Top-20 of raw search" is an approximation of "in the PPR pool",
  not the same thing.
- n=90 candidates, 2 phrasings, one corpus (`conversations-anthropic`).
- Entities spanning exactly 2 conversations. Wider spans behave differently and
  were excluded to keep A/B unambiguous.

## Recommended next step

Neither build. The open question the shipped default actually poses is whether
PPR *earns its keep* — `SOVEREIGN_CONV_PPR_WEIGHT` is `status = "shipped"` and
running at 0.25 in production today. The cheap test is an A/B over the ~17
candidates already identified as sitting in the window, comparing B's final rank
at weight 0, 0.25 and 0.5 through the Runtime path. That requires no authored
questions, and it settles whether option 2 has any effect at all before option 3
is costed.
