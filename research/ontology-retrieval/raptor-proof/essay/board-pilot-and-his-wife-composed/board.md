# Essay board: raptor-proof-essay-pilot-and-his-wife-composed-v1

judge model (requested `commonwealth/primary`): /v1/models -> Qwen3.6-35B-A3B-MTP-UD-Q6_K; replies carried Qwen3.6-35B-A3B-MTP-UD-Q6_K
synth model(s) per run manifests: Qwen3.6-35B-A3B-MTP-UD-Q6_K
judge and synth are the SAME MODEL (Qwen3.6-35B-A3B-MTP-UD-Q6_K): self-preference applies to every arm alike, not to one

n = 12 questions on the board, 14 plot points from a 225-word reference. Route exclusions: 0. Arm-revealing tokens surviving the scrub: 0. Answers cut at 12000 chars: 0.

| arm | runs | cov (relevant) | cov (all) | contradictions | specificity | synthesis | faithfulness | rubric | chars | empty | could-not-judge | never-ran |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| bare | 3 | 0.336 | 0.290 | 97 | 2.06 | 2.72 | 1.06 | 1.94 | 4152 | 0 | 0 | 0 |
| deep | 3 | 0.312 | 0.268 | 108 | 1.75 | 2.58 | 1.00 | 1.78 | 4347 | 0 | 0 | 0 |
| full | 3 | 0.296 | 0.232 | 108 | 1.33 | 2.42 | 1.00 | 1.58 | 4247 | 0 | 0 | 0 |
| closed-book | 1 | 0.021 | 0.042 | 5 | 1.00 | 1.58 | 2.08 | 1.56 | 3994 | 0 | 0 | 0 |

| arm | questions | Summary reached | Claim | Configuration | State | Entity |
|---|---|---|---|---|---|---|
| bare | 12 | 0 | 0 | 0 | 0 | 0 |
| deep | 12 | 0 | 0 | 0 | 0 | 0 |
| full | 12 | 12 | 6 | 2 | 11 | 12 |
| closed-book | 12 | 0 | 0 | 0 | 0 | 0 |

NOT TESTABLE on this bank — the walk reached a `Summary` on under half the questions for: bare (0/12), deep (0/12). A null against these arms means the factor was not reached, which is not the same finding as the factor not helping.

bare run-to-run band (max - min of run means): coverage_all 0.042, coverage_relevant 0.085, rubric_mean 0.333

| pair | win | loss | tie | could-not-judge | n | sign p | per-question w/l/t | per-question sign p |
|---|---|---|---|---|---|---|---|---|
| full_vs_bare | 0 | 4 | 32 | 0 | 36 | 0.125 | 0/2/10 | 0.500 |
| full_vs_deep | 0 | 3 | 33 | 0 | 36 | 0.250 | 0/1/11 | 1.000 |

A pairwise win needs both presentation orders to agree. Runs at temperature 0 are near-replicates, so the per-question sign test (majority over runs) is the one whose n is honest.
