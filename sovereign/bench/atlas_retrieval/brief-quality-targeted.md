# Brief-quality probe — `brothers_karamazov`

- queries: **61** (stratified, --sample-per-class 5)
- variants: **2**, top-K = 10
- model: **Bonsai-8B-Q1_0**
- judge calls: 122, wall-clock: 2156.6s (0.06/s)

## Headline (all queries)

| variant | n | yes | partial | no | parse_fail | yes+partial |
|---|---|---|---|---|---|---|
| atlas-tier-prune | 61 | 17 (27.9%) | 10 (16.4%) | 34 (55.7%) | 0 (0.0%) | **44.3%** |
| atlas-tier-prune-labeled | 61 | 23 (37.7%) | 10 (16.4%) | 28 (45.9%) | 0 (0.0%) | **54.1%** |

## Per-class yes+partial fraction

| variant | claim.valid (n) | claim.what (n) | entity.description (n) | entity.identity (n) | entity.role (n) | event.what (n) | event.where (n) | question.verbatim (n) | relation.labeled (n) | relation.pair (n) | state.character (n) | state.emotional (n) | tension.pair (n) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| atlas-tier-prune | 40% (5) | 60% (5) | 40% (5) | 80% (5) | 80% (5) | 40% (5) | 60% (5) | 0% (5) | 100% (5) | 20% (5) | 20% (5) | 0% (5) | 0% (1) |
| atlas-tier-prune-labeled | 60% (5) | 80% (5) | 0% (5) | 100% (5) | 80% (5) | 80% (5) | 40% (5) | 40% (5) | 60% (5) | 40% (5) | 40% (5) | 20% (5) | 100% (1) |
