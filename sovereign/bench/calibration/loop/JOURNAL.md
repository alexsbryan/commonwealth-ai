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
| 11 | claims | built objective (chaos rescore glue, control leg + caveat-prefixed variant leg) | never-ran -> PASS: control pass=False (watched failure baked in as standing negative control), variant pass=True, honesty 1/1, 2s | kept — A5's registered probe holds |
| 12 | claims | production wiring: zero-chunk DeepQuery/CodeQuery turns commit GK_CAVEAT_PREFIX structurally (streaming.rs deep synth request; mirrors knowledge_query.rs zero-chunk fallback; conversational intents excluded) | lint green; bench-side conversion deferred to the middle-loop A/B (offline objective cannot regenerate the turn) | kept, UNVERIFIED live |
| 13 | retrieval | built objective (trimmed-bank eval-run glue, targets read from failure_corpus.jsonl) | never-ran -> FAIL 3/3 on summarize_obscure scope, values byte-match the ledger's current column (0.600/0.900/0.700) — instrument validated, 28s | kept (honest before-state) |
| 14 | retrieval | RT1: SOVEREIGN_ATOM_ENUM_TOPIC_GRIP=0.0 arm | FAIL -> FAIL, ratios identical; traced: injection bails upstream of the gate (sep atlas atom_count=0 since May, cands empty, no gate event) — knob inert here, 27s | reverted (arm) |
| 15 | retrieval | RT2: SOVEREIGN_MERGE_SELECT=0 legacy-stack arm | FAIL -> FAIL identical; arms proven NON-DISTINCT (merge_select event absent in both — pool <= budget, the step self-skips) — §18.4 instrument check, 25s | reverted (arm) |
| 16 | retrieval | decomposition (no change): old baselines' carriers of the lost facts are RAPTOR summary chunks ("The passages trace..."); today SOVEREIGN_RAPTOR_LATE=on (default) APPENDS 8 summaries after the pool tail — outside the eval's truncate(30) and plausibly outside the prompt char budget (unverified) | — | finding journaled |
| 17 | retrieval | RT3: SOVEREIGN_RAPTOR_LATE=0 arm, summarize_obscure | FAIL 3/3 -> PASS 3/3, EXACT baseline restoration (0.800/1.000/0.800), 25s | arm kept as the registered A4 probe; default NOT flipped (late-inject holds a measured QA win, sources 76->86 — landing decision escalated to seat) |
| 18 | retrieval | RT4: same arm, summarize-prod | FAIL 4/4 -> PASS 4/4 (idealism 0.700 > 0.600 target), 27s — 7/7 sep cases convert on the registered probe | same |
| 19 | retrieval | wikipedia decomposition (no change): newsworthy 2 cases = corpus content rotation (war-article family grew; the bank header declares this by-design) — candidate verdict UNCONVERTIBLE by code without coaching; questions 2 cases = single-article flood ('Africa' 10/28 pool slots, raw lane has no diversity cap) — unconverted this session | — | finding journaled |
| 20 | claims | live probe of iteration 12's wiring (one-question chaos run, rebuilt binary) | live FAIL — pass=false, answer carries no prefix: the deep spawn DECODES the request prefix but never EMITS it (KQ spawn's emission block missing on the deep path) — iteration 12 alone was a silent no-op, caught by watching it fail live | wiring bug found |
| 21 | claims | deep-spawn prefix emission mirroring the KQ spawn (streaming.rs) | live FAIL -> live PASS: css-center pass=true, honesty-when-absent 1.00, ood-caveated 1/1, 44s | kept — A5 converted LIVE, not just on the offline probe |
