# E7 step 2b: entities that DIFFER on the cited lines (GVC, verified lines; Person and Location)

| run | feature | fires | fire on gold-same | different when fired | lookalikes flagged | true text links flagged | judged | meets rule |
|---|---|---|---|---|---|---|---|---|
| c2-lines-gvc-resolve-alone | either | 1561 of 14651 | 0.12 | 0.903 | 21 of 338 (0.062) | 48 of 373 | yes | no |
| c2-lines-gvc-resolve-alone | Person | 966 of 14651 | 0.066 | 0.913 | 9 of 338 (0.027) | 22 of 373 | yes | no |
| c2-lines-gvc-resolve-alone | Location | 616 of 14651 | 0.059 | 0.878 | 12 of 338 (0.036) | 27 of 373 | yes | no |
| blind-r2--gvc-resolve-alone | either | 662 of 10312 | 0.109 | 0.909 | 56 of 669 (0.084) | 25 of 234 | yes | no |
| blind-r2--gvc-resolve-alone | Person | 277 of 10312 | 0.058 | 0.884 | 5 of 669 (0.007) | 9 of 234 | yes | no |
| blind-r2--gvc-resolve-alone | Location | 388 of 10312 | 0.051 | 0.928 | 51 of 669 (0.076) | 16 of 234 | yes | no |
| c2-lines--gvc | either | 405 of 1676 | 0.233 | 0.975 | 7 of 20 (0.35) | 2 of 15 | yes | no |
| c2-lines--gvc | Person | 150 of 1676 | 0.14 | 0.96 | 1 of 20 (0.05) | 0 of 15 | yes | no |
| c2-lines--gvc | Location | 302 of 1676 | 0.116 | 0.983 | 6 of 20 (0.3) | 2 of 15 | yes | no |
| blind-r2--gvc | either | 160 of 481 | 0.226 | 0.956 | 5 of 22 (0.227) | 5 of 19 | yes | no |
| blind-r2--gvc | Person | 27 of 481 | 0.0 | 1.0 | 0 of 22 (0.0) | 0 of 19 | yes | no |
| blind-r2--gvc | Location | 145 of 481 | 0.226 | 0.952 | 5 of 22 (0.227) | 5 of 19 | yes | no |

Verdict under the pre-registered rule (both RESOLVE-alone runs: fire on gold-same <= 0.1, different when fired >= 0.9, lookalikes flagged >= 0.3): NOT WORTH

## Today's EM with `entity_differs` added (largest judged gap)

| run | E0 | E0+differs | E0+differs, feature's own row aside | entity_differs est (lab, agreed) |
|---|---|---|---|---|
| c2-lines-gvc-resolve-alone | 0.408 (proposed_answer) | 0.427 (model_choice) | 0.427 (model_choice) | 0.193 (0.264, 242) |
| blind-r2--gvc-resolve-alone | 0.737 (model_choice) | 0.740 (model_choice) | 0.740 (model_choice) | 0.586 (0.333, 87) |
| c2-lines--gvc | 0.466 (model_choice) | 0.536 (model_choice) | 0.536 (model_choice) | 0.39 (0.214, 14) |
| blind-r2--gvc | 0.501 (document_date) | 0.478 (document_date) | 0.478 (document_date) | 0.804 (0.625, 8) |

## RESOLVE alone: every weighed link the feature fires on vetoed

| run | weighed links | vetoed (right / wrong / unscorable) | CoNLL before -> after | MUC | B3 | CEAF-e | LEA |
|---|---|---|---|---|---|---|---|
| c2-lines-gvc-resolve-alone | 324 | 46 (14 / 32 / 0) | 0.607 -> 0.578 | 0.702 -> 0.662 | 0.635 -> 0.618 | 0.483 -> 0.454 | 0.448 -> 0.412 |
| blind-r2--gvc-resolve-alone | 471 | 46 (33 / 13 / 0) | 0.505 -> 0.507 | 0.781 -> 0.748 | 0.479 -> 0.499 | 0.254 -> 0.273 | 0.364 -> 0.342 |
