# PASS 1 SILENTLY DOWNGRADED ~7% OF LLM-DECIDED TURNS TO KnowledgeQuery VIA TRUNCATED JSON. Found + fixed 2026-08-05. This is the _ =>…

PASS 1 SILENTLY DOWNGRADED ~7% OF LLM-DECIDED TURNS TO KnowledgeQuery VIA TRUNCATED JSON. Found + fixed 2026-08-05. This is the `_ => KnowledgeQuery` arm in `LlmRouter::classify` doing exactly what ARCH_PRINCIPLES §18.3 warns about.

THE MEASUREMENT: 106 Pass-1 calls across the routing banks, 8 responses TRUNCATED mid-label — `{\n  "intent": "COMM` (5x, COMMISSION) and `{\n  "intent": "M` / `"METAL` / `"METALING` (3x, METALINGUAL). Every one pretty-printed. A truncated object has no closing brace, so the 2026-06-09 `extract_first_json_object` recovery cannot see it either; `serde_json` fails, `unwrap_or_default()` gives an empty intent, and the turn routes KnowledgeQuery. Well-formed, plausible, wrong, and silent.

RAISING THE OUTPUT BUDGET IS NOT THE FIX — MEASURED. 16 -> 48 tokens and the identical `{\n  "intent": "COMM` came back. The cut lands around ten tokens, UNDER even the old ceiling, so `max_tokens` was never the binding constraint; the cause is downstream in the schema-constrained generation path (the grammar permits whitespace and something terminates early). The budget is kept at 48 as insurance only — the original arithmetic assumed compact JSON and that EXPRESSIVE was the longest label, and both were wrong — but do not credit it with the fix.

THE FIX: `recover_truncated_intent` resolves a truncated label against `COARSE_INTENT_LABELS` ONLY when EXACTLY ONE label shares the prefix. `COMM` -> COMMISSION; `COM` is shared by COMPARISON and COMMISSION so it degrades exactly as before. That line is the whole point — recovering a verdict the model GAVE is legitimate, inventing one it did not is the §18.3 failure itself. Confidence is left at default: a recovered label is not a claim about certainty.

TWO STRUCTURAL BITS WORTH KEEPING:
 1. `COARSE_INTENT_LABELS` is now the ONE place the label set is written; `pass1_labels_match_the_dispatch_arms` round-trips every label through the parser so a label added to the `match` without the list can't become silently unrecoverable.
 2. The degrade path now ALSO eprintln's on the router's stderr glassbox channel. It previously had only a `tracing::warn`, which the bench harness does not enable — that is precisely why a 7% misroute rate survived weeks of green bench runs. A degrade visible only at a log level nobody turns on is not observable.

NOT THE SAME THING AS THE REMAINING BENCH FAILURES: the 8 paraphrase-bank misroutes are Pass-1 returning a COMPLETE `LOOKUP` verdict at confidence=0.00 — the flapping class RUNBOOK §6 documents, not truncation. Do not conflate them.
