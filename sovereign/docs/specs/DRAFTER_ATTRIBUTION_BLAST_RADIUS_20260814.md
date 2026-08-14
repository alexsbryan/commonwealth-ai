# Design note: D2 input-side levers — blast radius on the gate's evidence view

Required by the order's seams ("an evidence-universe change is a judge-input
change") and by seat ruling 1 (2026-08-14). Written BEFORE landing.

## The four edits

| # | edit | surface |
|---|---|---|
| 1 | RAPTOR summaries stop rendering as `[Web:]` under "## From web search"; they render as `[Source: {title}]` under a new "## Source overviews (derived)" section whose heading carries the attribution discipline | `runtime/formatters.rs::format_scored_chunks_with_kinds` |
| 2 | `node_id` threaded from both RAPTOR candidate paths into the injected chunk's metadata (`raptor_node_id`) | `runtime/retrieval/raptor_grounding.rs` |
| 3 | The `TRUST YOUR TRAINING` bullet replaced with an ambiguous-attribution abstention rule + exact-name discipline | `runtime/prompts.rs:127` |
| 4 | DeepQuery sizing: one decider — `resolve_output_budget` (reused from the KQ path) feeds BOTH the length directive and the request ceiling; the 2048-plea/4096-ceiling contradiction dies | `runtime/retrieval/mod.rs` + `runtime/streaming.rs:2679` + `runtime/types.rs::KnowledgeContext` |

## Blast radius on `gate_evidence_with_sources` — NONE, by construction, per edit

The sealed universe is `gate_evidence_with_sources(&kc.chunks)`
(`runtime/grounding/mod.rs:310`): it reads each chunk's CONTENT, `title`,
`corpus_id`, and `metadata["source"]=="raptor"` for the Leaf/Summary split.
It never reads the formatter's rendered prompt.

- **Edit 1** changes only the drafter-prompt RENDERING (bracket label + section
  heading). Chunk set, chunk content, titles, corpus_ids, and the
  raptor-metadata marker are all untouched → the judge's evidence view is
  byte-identical. The one judge-visible second-order effect is in the ANSWER:
  the drafter will now cite summaries as `[Source: X]` instead of `[Web: X]`.
  Both shapes are already handled by every parse site (`judge.rs:992`
  citation-span strip; the §7.8 `[source:`/`[Web:` scan exemptions; the snap
  pass's `[Source:` jurisdiction) — no NEW vocabulary is introduced, which is
  the reason `[Overview:]` labels were rejected in this design.
- **Edit 2** adds a metadata key no gate site reads (`grounding/mod.rs` reads
  only `metadata["source"]`; verified by grep). Inert substrate: it is the
  node_id thread ECONOMY §7.8 names as the missing link, consumed later by the
  judge-side quote_spans carriage — which is the REPLAY SIBLING's coordinated
  work, not this order's, precisely because it changes what the per-claim
  judge may clear.
- **Edit 3** changes the synthesis system prompt only. The gate never sees the
  system prompt.
- **Edit 4** changes the directive text and `max_tokens`. Answer LENGTH
  distribution will shift (intended: D0 measured longer answers failing more
  per claim, 19.5% vs 15.4%); no gate code path reads the directive or the
  ceiling.

Conclusion: the diff is pure input-side. Per done-when #4, no frozen-adversarial
re-run is owed; this note + the diff is the required statement. If any review
finds a gate site reading a surface these edits touch, that finding voids this
note and pulls the full adversarial duties.

## What is deliberately NOT in this diff (handed off)

- Judge-side quote_spans consumption (summary-claim clearing) — replay sibling,
  via the seat. Edit 2 is its substrate.
- The S0/S1 corrupted summaries ("van Inwagen's No Forking Paths",
  "Fischer/Paul Russell reasons-responsiveness") — enrichment-time defect,
  banked by the seat as backlog.
- Judge/scan false positives (D0 class vi) — replay sibling.
- dedup_by_source finding (seat ask): the iconic query's window spans sep
  (dedup_by_source=true, `~/.sovereign/indexes/sep/_corpus_meta.json:34`),
  wikipedia (null), obsidian-vault-959ee8a8f330 (false), conversation corpora
  (no _corpus_meta.json). The proven diversifier is on for the dominant corpus
  only; enabling it for wikipedia/vault is a per-corpus recipe decision, not
  code, and D0's (ii)-stitch specimens drew their confusors mostly from the
  vault/conversation chunks — flagged to the seat, not changed here.
- Corpus hygiene: the window contains conversation-shaped/demo chunks ("Edgy
  Demos", conversation-history, conversations-anthropic) that D0 mechanism 4
  identifies as confusors for both drafter and judge re-search. Corpus
  curation is out of this order's scope; reported.
