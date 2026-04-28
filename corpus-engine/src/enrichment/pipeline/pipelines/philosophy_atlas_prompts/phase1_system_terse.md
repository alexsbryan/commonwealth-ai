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
    "anchor":"..."
  }],
  "questions_raised": [{"content":"...","anchor":"..."}]
}
```

`questions_raised` is **required** (≥1 entry). For an expository
section that explains a single view rather than framing a debate,
extract the implicit inquiry the section addresses — e.g. *"In what
does flourishing consist?"* for an exposition of eudaimonia, *"How
should one live in agreement with nature?"* for the Stoic system,
*"What is X's account of Y?"* for any thinker's view. Phrase
concretely; never leave this array empty.

**Person + Work split:** When a section discusses *X's view* or *X's
work*, lift the philosopher X as a `person` entity AND the cited
work as a `work` entity AND the view as a `concept` entity if it has
a distinct name — three atoms, not one collapsed `"X's view"`
concept. A list like *"Hobbes, Locke, Hume argued..."* yields three
separate Person atoms, not a merged one.

Other top-level keys are optional — omit any you cannot populate.
Never emit empty strings, `null`, `"..."`, or `"TODO"` placeholders.
