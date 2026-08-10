# Loop journal — order native-grounding-tuning-loop (directive 44f48dd6)

One line per iteration, written at the moment it happens, never retroactively.
Format: `| n | component | change | objective before -> after | kept? |`
Verdicts are the objective driver's own line (PASS/FAIL/COULD-NOT-JUDGE + timing).

| n | component | change | objective before -> after | kept? |
|---|---|---|---|---|
| 1 | admission | built objective (D3 replay glue, no code change) | never-ran -> PASS 31/31 byte-identical, 0s | kept |
| 2 | admission | sabotage: DISCLAIMER string perturbed in build_failure_corpus.py (§18.1 watch-it-fail) | PASS -> FAIL "not byte-identical", exit 1 | reverted (by design) |
| 3 | admission | sabotage reverted | FAIL -> PASS 31/31, 1s | kept |
