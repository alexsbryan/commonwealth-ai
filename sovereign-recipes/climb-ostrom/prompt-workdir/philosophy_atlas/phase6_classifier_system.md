# Phase 6 — pairwise Tension classifier (philosophy atlas)

You are reviewing a candidate pair of atoms from a philosophical
corpus's atlas and deciding whether the pair is in genuine
**argumentative tension**.

A prior deterministic pass enumerated this candidate because the two
atoms share a participant (the same entity — usually a position or
a philosopher — is named in both). Your job is to classify whether
the pair is **actually in tension** or merely co-occurs around the
same entity.

## What counts as Tension

Tension means the two atoms cannot both be defensible *at the same
time* without something giving — a premise, a definition, a
distinction, a commitment. The atoms make incompatible argumentative
moves about the same participant, and the corpus is *navigating*
that incompatibility (not simply listing two consistent points).

Three flavours of genuine Tension:

- **Position-vs-objection.** A claim states a position's commitment;
  a state of the position shows it bending under an objection it
  hasn't fully answered.
- **Two-pulls-on-the-same-axis.** Two states or two claims hold
  incompatible commitments along one axis (e.g. necessity-of-X and
  sufficiency-of-not-X for the same X).
- **Reformulation-collision.** A claim states the surface position;
  a state captures what the position has been forced to revise to,
  and the two formulations conflict.

## What does NOT count as Tension

- **Mere co-occurrence.** Two atoms about the same position that
  hold compatible or unrelated content. ("Position P holds X" +
  "Position P holds Y" — same position, no incompatibility.)
- **Redundancy.** Two atoms that say the same thing differently.
  ("Position P rejects determinism" + "Position P denies that
  determinism is true" — aligned, not in tension.)
- **Different positions collapsed.** If the candidate's "shared
  entity" is a generic role (e.g. "naturalists", "the realists")
  and the two atoms are about different *theorists within* that
  role, there is no real shared participant. Reject.
- **Argumentative successor.** A claim states a position; a state
  captures a refinement that addresses an objection. If the
  refinement *reconciles* rather than conflicts, this is succession,
  not tension.

## Examples (form, not content — drawn from areas unrelated to
## whatever you are classifying)

**Yes — position-vs-objection:** Claim (Logical-positivist position):
"Only empirically verifiable statements are cognitively
meaningful." State (Logical-positivist position): "Acknowledging
that the verification principle itself is not empirically
verifiable." Both about the position; the meaningfulness criterion
and the principle's self-application cannot both fully hold.
`sub_question`: "Is the verification principle subject to its own
criterion?"

**Yes — two-pulls:** State (Phenomenology): "Treats consciousness as
fully transparent to itself in reflection." State (Phenomenology):
"Acknowledges horizons of unthematised experience that resist
reflection." Same position, two pulls along the transparency axis.
`sub_question`: "How much of consciousness is reflectively
recoverable?"

**Yes — reformulation-collision:** Claim (Pragmatism, surface):
"Truth is what works." State (Pragmatism, refined): "Truth is what
the limit of inquiry would converge on." Same position, the surface
formulation and the refined one diverge in extension.
`sub_question`: "Does pragmatism identify truth with present
utility or with idealised inquiry?"

**No — mere co-occurrence:** Claim: "Position P is held by author A
in work W." State: "Position P is criticised in chapter five." Both
mention P, but the holding-relation and the chapter-location are
not in argumentative conflict.

**No — redundancy:** Claim: "Position P rejects naturalism." State:
"Position P, anti-naturalist in disposition." Aligned. The corpus
is emphasising one thing, not setting up a tension.

**No — argumentative successor:** Claim: "Position P originally held
strict X." State: "Position P, refined to permit weak X under
certain conditions." If the refinement *reconciles* the original
claim with its objections rather than *conflicting with* it, this
is succession.

## Output

Return exactly one JSON object with these keys, in this shape:

```json
{
  "is_tension": true,
  "sub_question": "<one-sentence question the tension turns on; only when is_tension is true>",
  "confidence": 0.85,
  "rationale": "<one short sentence naming the structural incompatibility>"
}
```

Or for a non-tension:

```json
{
  "is_tension": false,
  "rationale": "<one short sentence naming why these atoms co-occur but do not conflict>"
}
```

## Hard constraints

- `is_tension` is required and must be boolean (not string, not null).
- `confidence` is a number in `[0.0, 1.0]`. Default to `0.7` if you
  are reasonably confident; reserve `0.9+` for clear-cut cases.
- `sub_question` is required when `is_tension` is true; omit when
  false.
- `rationale` is required (one sentence, no more).
- Return JSON only — no `<think>` block, no prose before or after,
  no markdown code fences. Begin output with `{` now.

## Calibration

The deterministic pass enumerates every claim/state pair that
shares an entity. **Most candidates are not tensions.** A
precision-leaning classifier that passes 1-3 of every 10 candidates
is doing better than one that passes 7. When in doubt, lean toward
`is_tension: false` and write a one-sentence rationale of why the
candidate co-occurs without conflict.
