# Brief-quality probe — `brothers_karamazov`

- queries: **25** (stratified, --sample-per-class 2)
- variants: **6**, top-K = 10
- model: **Bonsai-8B-Q1_0**
- judge calls: 150, wall-clock: 2761.3s (0.05/s)

## Headline (all queries)

| variant | n | yes | partial | no | parse_fail | yes+partial |
|---|---|---|---|---|---|---|
| flat-fp32 | 25 | 4 (16.0%) | 4 (16.0%) | 17 (68.0%) | 0 (0.0%) | **32.0%** |
| bm25-only | 25 | 7 (28.0%) | 4 (16.0%) | 13 (52.0%) | 1 (4.0%) | **44.0%** |
| atlas-tier-prune | 25 | 6 (24.0%) | 4 (16.0%) | 15 (60.0%) | 0 (0.0%) | **40.0%** |
| atlas-tier-prune-labeled | 25 | 9 (36.0%) | 4 (16.0%) | 12 (48.0%) | 0 (0.0%) | **52.0%** |
| atlas-tier-loo-hop | 25 | 6 (24.0%) | 5 (20.0%) | 14 (56.0%) | 0 (0.0%) | **44.0%** |
| atlas-tier-loo-hop-labeled | 25 | 7 (28.0%) | 4 (16.0%) | 14 (56.0%) | 0 (0.0%) | **44.0%** |

## Per-class yes+partial fraction

| variant | claim.valid (n) | claim.what (n) | entity.description (n) | entity.identity (n) | entity.role (n) | event.what (n) | event.where (n) | question.verbatim (n) | relation.labeled (n) | relation.pair (n) | state.character (n) | state.emotional (n) | tension.pair (n) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| flat-fp32 | 50% (2) | 50% (2) | 0% (2) | 100% (2) | 100% (2) | 50% (2) | 0% (2) | 0% (2) | 0% (2) | 0% (2) | 0% (2) | 50% (2) | 0% (1) |
| bm25-only | 50% (2) | 0% (2) | 50% (2) | 50% (2) | 50% (2) | 50% (2) | 50% (2) | 0% (2) | 100% (2) | 50% (2) | 50% (2) | 50% (2) | 0% (1) |
| atlas-tier-prune | 50% (2) | 50% (2) | 0% (2) | 50% (2) | 50% (2) | 50% (2) | 50% (2) | 0% (2) | 100% (2) | 50% (2) | 50% (2) | 0% (2) | 0% (1) |
| atlas-tier-prune-labeled | 50% (2) | 50% (2) | 0% (2) | 100% (2) | 50% (2) | 50% (2) | 0% (2) | 100% (2) | 100% (2) | 50% (2) | 50% (2) | 0% (2) | 100% (1) |
| atlas-tier-loo-hop | 50% (2) | 50% (2) | 0% (2) | 50% (2) | 50% (2) | 50% (2) | 100% (2) | 0% (2) | 100% (2) | 100% (2) | 0% (2) | 0% (2) | 0% (1) |
| atlas-tier-loo-hop-labeled | 50% (2) | 50% (2) | 0% (2) | 50% (2) | 50% (2) | 50% (2) | 50% (2) | 0% (2) | 100% (2) | 100% (2) | 0% (2) | 50% (2) | 0% (1) |
