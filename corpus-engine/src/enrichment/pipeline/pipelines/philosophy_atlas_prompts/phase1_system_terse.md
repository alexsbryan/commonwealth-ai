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

Omit any top-level key you cannot populate. Never emit empty
strings, `null`, `"..."`, or `"TODO"` placeholders.
