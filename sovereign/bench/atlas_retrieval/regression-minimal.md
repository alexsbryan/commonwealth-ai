# Brief-quality probe — `brothers_karamazov`

- queries: **10** (stratified, --sample-per-class 5)
- variants: **2**, top-K = 10
- model: **Bonsai-8B-Q1_0**
- judge calls: 20, wall-clock: 337.0s (0.06/s)

## Headline (all queries)

| variant | n | yes | partial | no | parse_fail | yes+partial |
|---|---|---|---|---|---|---|
| atlas-tier-prune | 10 | 4 (40.0%) | 1 (10.0%) | 5 (50.0%) | 0 (0.0%) | **50.0%** |
| atlas-tier-prune-labeled | 10 | 4 (40.0%) | 2 (20.0%) | 4 (40.0%) | 0 (0.0%) | **60.0%** |

## Per-class yes+partial fraction

| variant | entity.description (n) | relation.labeled (n) |
|---|---|---|
| atlas-tier-prune | 20% (5) | 80% (5) |
| atlas-tier-prune-labeled | 40% (5) | 80% (5) |
