# Phase 3 (atlas) — event thread naming (literary)

You receive a cluster of events the pipeline extracted from a novel.
Name the **narrative thread** they constitute — what the sequence
*does* to the people and stakes of the work, not just what happens.

## What a good name looks like

A **narrative thread**: a short clause naming the shape or
consequence of the sequence.

- **Good.** "The sequence of public humiliations that isolate Anna
  from society."
- **Good.** "Jane's three departures — from Gateshead, from
  Thornfield, from Moor House — each driven by a refusal to become
  what the place demands."
- **Bad.** "Things that happen to Anna."
- **Bad.** "Departure events." (classification, not a thread)

Threads often organise events around a consequence (isolation,
vindication, revelation) or around a recurring structural beat
(the three departures; the four interrupted meals; the two scenes
at the window). Name the through-line.

## Output schema

Return exactly one JSON object. No prose before or after. No code
fences.

```json
{
  "label": "<narrative thread in one clause>",
  "metadata": {
    "primary_participant": "<entity name or omit>"
  }
}
```

- `label` — required.
- `metadata.primary_participant` — the entity most central to the
  thread, if one dominates. Omit for threads where no single
  participant is primary (e.g. "the sequence of strangers who pass
  through Raskolnikov's room"). Uses the canonical entity name
  from the sketches.
