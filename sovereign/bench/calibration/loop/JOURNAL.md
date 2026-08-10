# Loop journal — order native-grounding-tuning-loop (directive 44f48dd6)

One line per iteration, written at the moment it happens, never retroactively.
Format: `| n | component | change | objective before -> after | kept? |`
Verdicts are the objective driver's own line (PASS/FAIL/COULD-NOT-JUDGE + timing).

| n | component | change | objective before -> after | kept? |
|---|---|---|---|---|
| 1 | admission | built objective (D3 replay glue, no code change) | never-ran -> PASS 31/31 byte-identical, 0s | kept |
| 2 | admission | sabotage: DISCLAIMER string perturbed in build_failure_corpus.py (§18.1 watch-it-fail) | PASS -> FAIL "not byte-identical", exit 1 | reverted (by design) |
| 3 | admission | sabotage reverted | FAIL -> PASS 31/31, 1s | kept |
| 4 | routing | built objective (router-fit glue over the 3 A3 probes + 63-case guard baseline captured) | never-ran -> FAIL 2/3 missed (commissive cushion -0.0240 sub-sim; research_survey margin 0.0011 < 0.015; metalingual fragile +0.0002), 4s | kept (honest before-state) |
| 5 | routing | +1 commissive exemplar, "flag it for <day>" frame (visa topic) | FAIL 2 miss -> FAIL 1 miss; commissive -0.0240 -> +0.0618 fired_correct; guard clean 30/63, 5s | kept |
| 6 | routing | +1 deep exemplar, literature-survey frame (rent control) | research_survey cushion -0.0139 -> -0.0000, still missed; guard clean, 4s | kept (direction right, gate not cleared) |
| 7 | routing | +1 deep exemplar, same frame, second topic (congestion pricing) | FAIL -> PASS 3/3 fired_correct; research_survey +0.0077; guard clean, 4s | kept |
| 8 | routing | +1 deep exemplar "In the DSM's framing..." to harden metalingual cushion | PASS -> PASS, cushion unchanged +0.0002 (row never nearest) | reverted (zero measured effect = dead weight) |
| 9 | routing | swap for "In the Talmud's framing..." (closer frame: named reference + bare concept) | PASS -> PASS, cushion unchanged +0.0002 | reverted (same; metalingual accepted fragile at +0.0002, bench-side adjudication left to the A/B) |
| 10 | routing | router-embed-cache re-mint (423 exemplars; freshness gate was exit-3 stale) | PASS -> PASS 3/3, guard clean, 4s | kept |
