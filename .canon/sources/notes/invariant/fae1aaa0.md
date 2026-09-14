# BEFORE ABLATING A COMPONENT, NAME THE POPULATION THAT ACTUALLY RUNS IT — FROM THE STORE, NOT FROM MEMORY. A BENCH THAT NEVER EXERCISES THE…

BEFORE ABLATING A COMPONENT, NAME THE POPULATION THAT ACTUALLY RUNS IT — FROM THE STORE, NOT FROM MEMORY. A BENCH THAT NEVER EXERCISES THE COMPONENT RETURNS COULD-NOT-JUDGE, AND COULD-NOT-JUDGE COUNTED AS A "NO DIFFERENCE" VOTE INVERTS A CROSS-CORPUS SAFETY RULE INTO A DELETION ARGUMENT.

Established 2026-08-03 by an operator challenge to my own L0 gate, and it cuts both ways.

WHAT I GOT WRONG. I wrote a rule that would delete GLiNER from the vault path on the strength of ONE bench (obsidian). That is the same error the whole P2.1 session was about — a single-corpus result licensing a general conclusion — pointed the other direction. The operator's standing rule is right: SEP + wikipedia + obsidian are the representative cross-section, and nothing gets cut unless it fails to carry its weight on all of them.

WHAT THE STORE SAID WHEN I APPLIED THAT RULE. THE INGEST-SIDE GLiNER PASS DOES NOT RUN ON SEP OR WIKIPEDIA AT ALL. From `chunk_entity_progress` / `chunk_entities` on 2026-08-03 — every corpus that has ever had a pass:
  conversations-anthropic          16,404 chunks   68,464 mentions
  obsidian-vault-959ee8a8f330       3,175          10,004
  watched-9ef2f912ea2e                765           5,643
  watched-959ee8a8f330              1,303           5,462
  conversations-personal              267           1,268
  + 4 smaller watched/folder corpora
SEP and wikipedia: zero rows, no progress row. The pass is structurally scoped to conversation- and folder/vault-category corpora.

SO THE THREE-CORPUS GATE AS LITERALLY STATED WOULD HAVE PRODUCED TWO COULD-NOT-JUDGE VERDICTS AND ONE REAL ONE — and "no difference on SEP, no difference on wikipedia" read as "doesn't carry its weight there" is a §18.3 silent substitution that would have ARGUED FOR the deletion the rule exists to prevent. The rule is right; the corpora have to be ones that run the thing.

THE CORRECTED CROSS-SECTION for the ingest-side pass: obsidian vault (bench/obsidian/questions.toml, 12 Q / 12 expected_sources — personal-notes register) AND conversations-anthropic (bench/conversation/questions.toml, 12 Q / 14 expected_sources — 5x larger, dialogue register, the corpus GLiNER was built for). Deletion requires BOTH to say the sources are artifacts; one corpus showing real source weight keeps it.

SEP AND WIKIPEDIA ARE NOT DROPPED — THEY ARE ROUTED TO THE SURFACE THEY CAN JUDGE. GLiNER has a separate retrieval-side life (`sovereign_core::traits::EntityExtractor`; query_expansion.rs:348,:593; history.rs:547,:606) that applies to EVERY corpus including those two. That surface costs query time, not build time. If removing THAT is ever proposed, SEP and wikipedia are exactly the right benches.

GENERALISES AS: §18.1's four verdicts (passed / failed / could-not-judge / never-ran) apply to bench SELECTION, not only to bench RESULTS. A bench roster assembled without checking which benches exercise the subject will silently convert never-ran into passed.
