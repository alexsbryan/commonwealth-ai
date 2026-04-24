# Phase 8 — philosophy configuration detection

You are reading a structural synopsis of a philosophy article —
its resolved entities (philosophers, concepts, works), dialectical
relations, argumentative trajectories, top claims, open questions,
and key events. You are NOT reading the article's raw text. Your
task is to identify how the **arrangement of the parts** produces
an interpretive whole.

A **Configuration** in philosophy is a pattern across many atoms —
not a single claim the article makes, but an argumentative or
structural shape the article enacts through how it orchestrates
positions, objections, and refinements. Examples:

- *Tractatus*: "Ladder structure" — each proposition depends on
  the last, then the whole edifice is kicked away at the end.
- *SEP on Compatibilism*: "Frankfurt case as dialectical hinge" —
  compatibilism's entire post-1969 development orbits around a
  single thought experiment and the literature of reply.
- *SEP on Free Will*: "The three-position grid" — libertarianism,
  compatibilism, and hard determinism organise the field not as
  claims but as the exhaustive space of stances agents must
  occupy.

## The Ricoeur Constraint

Configurations are **interpretations**, not facts. The same atoms
can support different configurational readings depending on what
you weight. You MUST articulate, for each configuration you
report, at least one plausible alternative reading a good reader
would offer. The `interpretive_note` field is where this lives —
it is not optional.

"Alternative reading" does not mean "this is wrong." It means
"here is another valid framing; I chose the one above because of
X." A configuration without an `interpretive_note` is rejected.

## Philosophy-specific patterns to watch for

- **Dialectical hinge.** One concept, case, or argument around
  which the entire debate reorganises (Frankfurt cases, Gettier
  cases, twin-earth scenarios).
- **Position grid.** The article frames the space of stances as
  an exhaustive partition (libertarian / compatibilist / hard
  determinist; internalist / externalist).
- **Progressive refinement.** Successive papers narrow the
  position from naïve to sophisticated (compatibilism →
  semi-compatibilism → revisionary accounts).
- **Negative programme.** The article's structure is organised
  around what it rejects more than what it affirms.
- **Conceptual inheritance.** Later positions inherit vocabulary
  from earlier ones in a way that shapes what can be argued.

## Output

Return **0 to 3** configurations. Fewer is better than forcing a
pattern. If the atlas doesn't support a load-bearing
configuration, return an empty list — do not fabricate.

```json
{
  "configurations": [
    {
      "label": "<short headline — 3–7 words>",
      "description": "<1–3 sentence statement of the pattern>",
      "constituent_atoms": ["<atom_id>", "<atom_id>", ...],
      "interpretive_note": "<alternative readings and why this one>",
      "confidence": 0.0–1.0,
      "evidence_chunk_ids": ["<section_id>", ...]
    }
  ]
}
```

Field notes:

- `constituent_atoms` must reference atom ids that appear in the
  summary. 3–10 ids per configuration is typical.
- `confidence` — 0.9+ means "structurally unavoidable"; 0.5–0.7
  is "compelling but not unique"; below 0.5 you probably
  shouldn't emit.
- `evidence_chunk_ids` — the section ids with the strongest
  grounding.

## Anti-patterns

- **Thematic labels masquerading as configurations.** "Free will"
  is a theme; "the Frankfurt-case debate as three-round
  dialectical hinge" is a configuration.
- **Summaries of the article's conclusion.** A configuration is a
  structural pattern, not the article's bottom-line view.
- **Confident readings without alternatives.** A configuration
  that admits no alternative reading is almost always an
  oversimplification.

Return exactly one JSON object. No prose before or after. No code
fence markers.
