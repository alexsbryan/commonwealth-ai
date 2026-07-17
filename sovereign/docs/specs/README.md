# sovereign/docs/specs/

Design specs — proposals in motion. Distinct from
[`../`](../README.md) (what exists today) and
[`../archive/`](../archive/README.md) (frozen forensic record).

## Three kinds of spec live here

### 1. In-flight design proposals

A spec for work that's being designed or built. Status banner is
`Draft` or `In flight (design only)` or `In flight — partially
shipped`. When the work lands, the runtime surface moves to the
relevant feature doc under `../` and **this spec moves to
`../archive/`** with a companion `decision` note pointing at the
archived path.

Current in-flight:

- [`TIERED_RETRIEVAL_PHASE_B.md`](TIERED_RETRIEVAL_PHASE_B.md) —
  per-corpus port matrix for the tiered-retrieval architecture.
- [`TYPED_EXTENSION_PASS.md`](TYPED_EXTENSION_PASS.md) — design
  for typed-extension LLM pass over RAPTOR cluster summaries.
- [`PRODUCTION_SEARCH_INTEGRATION.md`](PRODUCTION_SEARCH_INTEGRATION.md)
  — phased plan; orchestrator + registry + DDG fallback shipped;
  Tavily + operator decisions remain.
- [`STREAMING_GATE_PIPELINE.md`](STREAMING_GATE_PIPELINE.md) —
  overlap the longform grounding-gate verification under the draft
  stream; the final holistic-scan barrier stays the safety floor.

### 2. Reference patterns

A spec that's been shipped, but the *shape* is prescriptive for
future work — anyone porting the pattern to a new corpus / new
backend / new component should adopt this layout. Status banner
includes `Stays in docs/specs/ as a reference pattern`.

Current reference patterns:

- [`PROGRESSIVE_ENRICHMENT.md`](PROGRESSIVE_ENRICHMENT.md) —
  RAPTOR + GLiNER layered enrichment shape; prescriptive for any
  future corpus port.

### 3. Canonical wire specs

A spec that defines a protocol or schema other systems implement
against. Status banner includes `Canonical wire spec — evolves in-
place`. Never ships-and-retires; versions in-place.

Current canonical wire specs:

- [`oicp.md`](oicp.md) — Open Inference Capabilities Protocol
  (v0.1.0-draft). CC0-licensed. The mesh's capability-negotiation
  contract.

## Recently shipped, kept for forensics

When in-flight specs ship and have load-bearing design-rationale
content (failure-mode analysis, choice-vs-alternatives, bench
plans), they may stay here with `Shipped <date>` status rather
than moving to `../archive/`. Judgement call at ship time —
preserve when rationale would be expensive to re-derive.

- [`CLUSTER_SCORE_BLEND.md`](CLUSTER_SCORE_BLEND.md) — shipped
  2026-05-22; spec carries failure-mode analysis + bench plan
  worth preserving inline.
- [`CONV_TIERED_PORT.md`](CONV_TIERED_PORT.md) — shipped
  2026-05-23; carries why-this-schema and why-this-trigger
  rationale.

## When to write a new spec here

- Design work that will take multiple sessions / multiple PRs.
- A choice with real alternatives where the rationale needs to
  outlive your memory of why you picked one.
- A pattern other contributors will need to follow.

When you ship:

1. Update the spec's status banner.
2. Write a NoteStore `decision` note (`sovereign tools call note
   --kind=decision`) summarising the shipped surface + linking the
   spec path.
3. Promote durable runtime detail to a feature doc under `../`.
4. Per lifecycle (above), move spec to `../archive/` OR leave
   here with Shipped status if rationale has forensic value.

## When NOT to write a new spec here

- Operator runbook → `../` (e.g. `TOOLBOX_SETUP.md`).
- Experiment writeup → ship the experiment, then the lessons go
  to NoteStore (`sovereign tools call note`); the writeup itself
  goes to `../archive/`.
- Per-PR design notes → put them in the PR body, not a spec.
- Single-turn implementation plan → put it in `~/.claude/plans/`,
  not here.
