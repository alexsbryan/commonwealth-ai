# Phase 8 — Literary Configuration Detection

You are reading a structural synopsis of an authored literary work
— its resolved entities, relations, character trajectories, top
claims, open questions, and key events. You are NOT reading the
work's raw text. Your task is to identify how the **arrangement of
the parts** produces an interpretive whole.

A **Configuration** is a pattern across many atoms — not a single
claim the text makes, but a structural shape the work enacts
through which atoms it places where. Examples across works:

- *Brothers Karamazov*: "Three sons as embodiments of faith,
  reason, and sensuality" — Alyosha, Ivan, Dmitri each carry one
  face of a divided modern soul.
- *Pride and Prejudice*: "Parallel courtships mirror the theme of
  first impressions" — each marriage thread revises the reader's
  earliest judgments.
- *Tractatus*: "Ladder structure" — each proposition depends on
  the last, then the whole edifice is kicked away.

## The Ricoeur Constraint

Configurations are **interpretations**, not facts. The same
atoms can support different configurational readings depending on
what you weight. You MUST articulate, for each configuration you
report, at least one plausible alternative reading a good critic
would offer. The `interpretive_note` field is where this lives —
it is not optional.

"Alternative reading" does not mean "this is wrong." It means
"here is another valid framing; I chose the one above because of
X." A configuration without an `interpretive_note` is rejected.

## Output

Return **0 to 3** configurations. Fewer is better than forcing a
pattern. If the atlas doesn't support a load-bearing
configuration, return an empty list — do not fabricate.

```json
{
  "configurations": [
    {
      "label": "<short headline — 3–7 words>",
      "description": "<1–3 sentence statement of the configurational pattern>",
      "constituent_atoms": ["<atom_id>", "<atom_id>", ...],
      "interpretive_note": "<acknowledgment of alternative readings and why this one>",
      "confidence": 0.0–1.0,
      "evidence_chunk_ids": ["<section_id>", ...]
    }
  ]
}
```

Field notes:

- `constituent_atoms` must reference atom ids that appear in the
  summary (entity-XXXX, relation-XXXX, event-XXXX, claim-XXXX,
  question-XXXX, state-XXXX). 3–10 ids per configuration is
  typical; fewer risks thinness, more risks unfocus.
- `confidence` is your self-report of how robustly the atlas
  supports this reading. 0.9+ means "this is structurally
  unavoidable"; 0.5–0.7 is "a compelling but not unique reading";
  below 0.5 means you probably shouldn't be emitting it.
- `evidence_chunk_ids` are the section ids (e.g. `sec_0001`) where
  the configuration's strongest grounding lies — two or three is
  enough.

## Anti-patterns

- **Thematic labels masquerading as configurations.** "Redemption"
  is a theme; "three brothers with one rebirth each" is a
  configuration.
- **Single-atom observations.** A configuration spans multiple
  atoms arranged in relationship; it is not a claim about one.
- **Confident readings without alternatives.** A configuration
  that admits no alternative reading is almost always an
  oversimplification. The `interpretive_note` keeps you honest.
- **Pattern without pattern.** If the only thing the configuration
  asserts is that several atoms exist together, it's not a
  configuration — it's a list.

Return exactly one JSON object. No prose before or after. No code
fence markers.
