# Phase 1 — descriptive typed extension

You are reading one section that does descriptive work — laying out a
thing's structure, properties, or relationships. Zettel cards,
glossary entries, anatomical descriptions of institutions or systems,
"what X is" pages. Your job is to expose the descriptive scaffolding
so a downstream reader can find a definition, a property, or a
pointer to source.

The base entities (Person, Place, Concept, Institution, Work) are
produced by a separate prompt. Your job is the five collections
below.

## The five collections

### 1. `definitions`

Direct definitions the section gives — "X is the practice of …",
"a Y is any Z that …".

- `term` — the term being defined.
- `content` — one sentence stating the definition.
- `anchor` — 3-8 word keyphrase.

### 2. `property_claims`

Specific properties a thing has — "the section gives X attribute Y".
Distinct from a Claim atom: a property_claim is structural ("the
PBM industry has three large players"), not assertive ("the PBM
industry is bad").

- `subject` — the thing being described.
- `property` — the property name.
- `value` — what the property is.
- `anchor` — 3-8 word keyphrase.

### 3. `relationships`

Structural relations between described things — "X is part of Y",
"X reports to Y", "X depends on Y". Same shape as the narrative
extension's relations, but framed structurally rather than
narratively.

- `participants` — 2+ entity names.
- `label` — one-clause name for the relationship.
- `anchor` — 3-8 word keyphrase.

### 4. `examples`

Concrete examples used to illustrate definitions or property claims.
Different from `evidence_invocations` on the argumentative extension:
examples illustrate, evidence supports.

- `label` — short tag for the example.
- `content` — one sentence stating what the example is.
- `illustrates` — definition / claim / concept the example illustrates.
- `anchor` — 3-8 word keyphrase.

### 5. `provenance`

Source pointers — author/title references, citations, "as
described in", URLs. Useful for downstream reading-desk style
linking.

- `label` — short label for the source.
- `context` — one sentence of context.
- `anchor` — 3-8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object with the five collections. No prose,
no `<think>` block, no code-fence markers. Empty collections may be
omitted. Required fields must be non-empty.
