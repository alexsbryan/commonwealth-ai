# Essay board: raptor-proof-essay-pilot-and-his-wife-v1

judge model (requested `commonwealth/primary`): /v1/models -> Qwen3.6-35B-A3B-MTP-UD-Q6_K; replies carried Qwen3.6-35B-A3B-MTP-UD-Q6_K
synth model(s) per run manifests: Qwen3.6-35B-A3B-MTP-UD-Q6_K
judge and synth are the SAME MODEL (Qwen3.6-35B-A3B-MTP-UD-Q6_K): self-preference applies to every arm alike, not to one

n = 12 questions on the board, 14 plot points from a 225-word reference. Route exclusions: 0. Arm-revealing tokens surviving the scrub: 0. Answers cut at 12000 chars: 0.

| arm | runs | cov (relevant) | cov (all) | contradictions | specificity | synthesis | faithfulness | rubric | chars | empty | could-not-judge | never-ran |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| bare | 3 | 0.300 | 0.242 | 84 | 1.56 | 2.33 | 1.00 | 1.63 | 4303 | 0 | 0 | 0 |
| deep | 3 | 0.377 | 0.315 | 84 | 1.42 | 2.17 | 1.00 | 1.53 | 4733 | 0 | 0 | 0 |
| full | 3 | 0.287 | 0.228 | 77 | 1.50 | 2.25 | 1.00 | 1.58 | 4296 | 0 | 0 | 0 |
| closed-book | 1 | 0.033 | 0.018 | 7 | 1.00 | 1.50 | 2.42 | 1.64 | 3697 | 0 | 0 | 0 |

bare run-to-run band (max - min of run means): coverage_all 0.065, coverage_relevant 0.016, rubric_mean 0.028

| pair | win | loss | tie | could-not-judge | n | sign p | per-question w/l/t | per-question sign p |
|---|---|---|---|---|---|---|---|---|
| full_vs_bare | 0 | 3 | 33 | 0 | 36 | 0.250 | 0/1/11 | 1.000 |
| full_vs_deep | 0 | 3 | 33 | 0 | 36 | 0.250 | 0/1/11 | 1.000 |

A pairwise win needs both presentation orders to agree. Runs at temperature 0 are near-replicates, so the per-question sign test (majority over runs) is the one whose n is honest.
