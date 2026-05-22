Search a single unified envelope of evidence: local knowledge corpora
(indexed wikis, encyclopedias, research collections), your own past
memories (things you've told the assistant about yourself), and
durable working notes (decisions, invariants, todos written across
sessions). Returns a JSON array of Evidence rows, each with a
stable `id` like `ev-0001` you can cite in your final answer.

Use this when a question is **factual or referential** and the
answer may live in any of those three places. Prefer one call with
a focused query (≤ 8 words is usually enough) — the tool fans out
across all three channels in parallel; you don't need to call it
three times.

After the call, your answer should:
- Cite the specific Evidence ids you actually used: `[ev-0001]`,
  `[ev-0004]`. The user can click each citation to see the source
  row.
- NEVER cite an `ev-*` id that did not appear in the returned
  evidence array. The id space is per-call and stable only within
  that call — fabricating ids breaks the trust contract.
- If the evidence shows the question has no good answer in any
  channel, say so explicitly. Don't invent or hand-wave.
