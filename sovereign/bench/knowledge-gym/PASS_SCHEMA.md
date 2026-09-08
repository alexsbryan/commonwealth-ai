# knowledge-gym `pass.toml` predicate vocabulary

The knowledge-gym replays each fixture N times against a live
daemon and grades the transcripts against the predicates in the
fixture's `pass.toml`. A fixture passes a replay when every
predicate listed in `pass.toml` evaluates true against that
replay's transcript.

This document lists every predicate the runner understands.

## `production_path` — required, and not a predicate

```toml
production_path = "executor"   # | "attached-doc" | "raw"
```

The FIRST key every `pass.toml` must carry. It is not graded; it names
the surface the replay is driven through, and a fixture that declares
none is refused at load rather than defaulted. `RUNBOOK.md` §"Which
surface a fixture measures" has the table and the reason.

Three things are refused AT LOAD, loudly, exiting non-zero — never
turned into per-replay could-not-judge verdicts: a missing
`production_path`, an unknown spelling, and `attached-doc` (which has no
driver). Same for a multi-turn fixture declaring `executor`. ARCH §18.2
as amended: an abstention nobody has watched be necessary is not rigor,
and a fact the loader already knows must not become nine unjudged
replays that read as a careful lane. Every run prints
`verdicts: passed N failed N could-not-judge N` so the abstention count
is never something a reader has to derive.

Read it before you read any number below: on `raw` the predicates grade
the daemon's native function-calling adapter, which no product turn
reaches, and every line of that fixture's report says
`not-the-product`.

## Structural predicates (tool-dispatch shape)

Every one of these reads the replay's `ToolLedger`
(`knowledge_gym_cmd/ledger.rs`) — the one record each driver projects
its own dispatch log into. They do NOT read a path's private shape, and
in particular they no longer read the OpenAI `tool_calls` field
(ARCH §10.6).

- `should_call_knowledge_lookup: bool` — true requires the model
  to invoke `knowledge_lookup` at least once during the turn.
  false requires it NOT to call it (e.g. for definitional
  questions the model should answer from pretrained knowledge).
- `expected_first_tool: String` — the first tool call's name must
  equal this. Catches "model reached for the wrong tool first."
- `max_lookup_calls: usize` — upper bound on `knowledge_lookup`
  invocations per turn. Use `1` to require a single focused
  query; higher when multi-call composition is the test shape.
- `min_tool_calls: usize` *(Tier 5 future)* — lower bound on
  total tool calls. Used for multi-call assembly fixtures where
  the answer requires combining 2-3 lookups.

## Citation predicates (evidence handles)

- `must_not_cite_evidence_id_outside_returned: bool` — when true,
  every `[ev-Tn-NNNN]` (or `[ev-NNNN]` back-compat) handle the
  model cites must appear in the tool result envelope. Catches
  fabricated citation handles. Tier 2's
  `EvidenceIdAllowlistConstraint` makes this structurally
  enforceable at sample time when the gym's request carries the
  populated `evidence_id_allowlist`.
- `max_cited_evidence_ids: usize` — upper bound on the number of
  `[ev-…]` handles in the answer. Use `0` to require zero
  citations (e.g. on `no-results` fixtures where any citation is
  fabrication).

## Answer-content predicates (semantic shape)

- `answer_acknowledges_gap: bool` — when true, the answer must
  surface honest negation (the model says "I don't know",
  "outside the corpus", "no data on …", etc.) AND mention the
  scope of the gap. Uses general English shapes (negation
  vocabulary + scope tokens like "information", "data", "record").
  No bank vocabulary — same content predicate works across
  questions.
- `must_reference_prior_turn_evidence: bool` *(Tier 5 future,
  needs multi-turn gym)* — true requires the model to cite a
  `[ev-Tn-NNNN]` handle from a turn N < current turn. Validates
  Tier 1's cross-turn result memory.
- `must_attribute_conflict: bool` *(Tier 5 future,
  judge-evaluated)* — true requires the model to acknowledge
  that two evidence rows disagree, rather than picking one
  silently. Judge-evaluated because conflict attribution is
  open-ended phrasing.
- `must_acknowledge_partial_coverage: bool` *(Tier 5 future,
  judge-evaluated)* — true requires the model to say "the
  corpus has X but not the specific Y you asked" when the
  evidence is relevant but doesn't directly answer.

## Composition predicates (Tier 3)

- `evidence_set_includes_kind: Vec<String>` *(Tier 5 future)* —
  set of `EvidenceKind` strings (e.g. `["web"]`) that must
  appear in the merged evidence. Use to verify the
  auto-escalation branch fired (web hits present) or didn't
  fire (empty list).

## Cache predicates (Tier 4)

- `expect_cache_hit: bool` *(Tier 5 future, needs multi-turn
  gym + cache wiring on the call_cached path)* — true requires
  the tool result envelope to carry `cached: true`. Validates
  cache reuse on repeat queries.

## Adding a new predicate

1. Add the field to `Pass` in
   `sovereign-cli/src/knowledge_gym_cmd/runner.rs` (Serde-skip
   unknown fields stays off, so a typo in the field name
   surfaces as a parse error).
2. Add the evaluator branch in `evaluate(&Pass, &Transcript)`.
3. Document the predicate here.
4. Use it in the fixture's `pass.toml`.

## Predicates marked "Tier 5 future"

These predicates either need the multi-turn gym infrastructure
that hasn't landed yet, or they need a runner-side change to
populate the request shape that exercises them (e.g.
`evidence_id_allowlist` accumulator on the gym side mirrors the
frontdoor accumulator on the daemon side). They're listed so
fixture authors know the vocabulary that's coming.
