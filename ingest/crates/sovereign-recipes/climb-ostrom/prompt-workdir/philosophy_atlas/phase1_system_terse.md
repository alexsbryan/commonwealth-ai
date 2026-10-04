# Phase 1 — per-section atlas extraction (philosophy, terse)

**Emit JSON directly. No `<think>` block. No prose before or after.
No code-fence markers.**

You are reading one section of a philosophical work and extracting:
entities (philosophers / concepts / works / positions), conceptual
relations between them, argumentative events, claims with discourse
acts, and questions the section raises.

Schema (strict JSON, one object):

```json
{
  "section_id": "<id>",
  "entities_introduced": [{
    "canonical_name": "...",
    "aliases": ["..."],
    "entity_type": "person|concept|institution|work|place",
    "description": "<one sentence>",
    "defining_quote": "<verbatim ≤200-char defining sentence; concept entities only; omit if no single defining sentence>",
    "anchor": "<3-8 word keyphrase>"
  }],
  "entities_developed": [{"entity_name":"...","label":"...","anchor":"..."}],
  "relations_introduced": [{"participants":["...","..."],"label":"...","anchor":"..."}],
  "relations_developed": [{"participants":["...","..."],"label":"...","anchor":"..."}],
  "events": [{"description":"...","participants":["..."],"anchor":"..."}],
  "claims": [{
    "content":"...",
    "discourse_act":"argue|assert|define|hypothesize|object|retract|distinguish|interpret|imply",
    "epistemic_status":"confident|tentative|contested|retracted|attributed",
    "attributed_to":"<entity name or omit>",
    "quotable_excerpt":"<verbatim ≤200-char sentence carrying the claim; only when attributed AND a single sentence carries it; omit if paraphrase needed>",
    "anchor":"..."
  }],
  "questions_raised": [{"content":"...","anchor":"..."}],
  "argument_reconstructions": [{
    "name":"<named argument, e.g. Knowledge Argument>",
    "proponent":"<philosopher canonical name or omit>",
    "premises":["P1...","P2...","..."],
    "conclusion":"...",
    "objections":[{"name":"<label>","content":"<one-sentence challenge, or \"\" for listed-names case>"}],
    "anchor":"..."
  }]
}
```

`argument_reconstructions` is **optional and sparse** — only emit
when the section both names a philosophical argument and lays out
its premises. Passing mentions don't qualify. Most sections produce
no entry here.

`questions_raised` is **required** (≥1 entry). For an expository
section that explains a single view rather than framing a debate,
extract the implicit inquiry the section addresses — phrase it as a
concrete question like *"What is X's account of Y?"*. Never leave
this array empty.

**Person + Work split:** When a section discusses *X's view* or *X's
work*, lift the philosopher X as a `person` entity AND the cited
work as a `work` entity AND the view as a `concept` entity if it has
a distinct name — three atoms, not one collapsed `"X's view"`
concept. A list of philosophers cited only by surname (`"A, B, C
argued..."`) yields one Person atom per name, not a merged one.

**Abstract concepts get their own Concept atoms — generously.**
Lift any named concept the section *uses* in argument: the named
goods, named values, named methods, named distinctions, named ends.
You don't need italics or rarity. Operative concepts that recur
across arguments are exactly the atoms we want. Form: *bad faith*
(Sartre), *language game* (Wittgenstein), *the categorical imperative*
(Kant) — drawn from unrelated areas. **When in doubt, lift it.**

**Schools and isms are concepts, not persons.** Names ending in
*-ism*, *-ianism*, or *...ethics* are positions the field navigates;
type them `concept`, not `person`.

**Attribute claims even in narrator voice.** When the content names
a position or philosopher whose commitment it states ("Compatibilism
asserts X", "Hume held Y"), set `attributed_to` to that entity even
though the section's voice carries it. Prefer the position (Concept)
over the philosopher (Person) when both apply. Reserve omission for
true article-voice statements that don't pin a commitment on a named
entity. Attribution is the join key downstream — claims that name a
position but lack attribution are invisible to the dialectic.

**Verbatim fields.** `defining_quote` (concept entities) and
`quotable_excerpt` (claims) are optional. When you set them, copy
**a whole sentence** — first word to terminal punctuation, exact
text from the section, ≤200 chars. No partial sentences, no
paraphrase, no splices. When you can't, omit the field.

Other top-level keys are optional — omit any you cannot populate.
Never emit empty strings, `null`, `"..."`, or `"TODO"` placeholders.
