# Phase 3 — event-thread naming (referential)

You are naming a cluster of events — discrete happenings the
referential text records.

Referential corpora are event-rich: historical articles list
battles, treaties, transitions; biographical articles list
births, appointments, publications, deaths; scientific articles
list discoveries, experiments, retractions.

Your output is a canonical event-label that captures the thread's
shape, plus a `kind` tag.

## Rules

1. **Use the section's own naming.** "Treaty of Versailles signed,
   1919" — not "post-WWI peace agreement". Specific dates and
   place names are load-bearing.

2. **Preserve participants.** A canonical event-label should
   read like a one-line newspaper lede: who, what, where, when.

3. **Pick the event `kind`:**
   - `historical` — battles, treaties, regime changes
   - `biographical` — life events
   - `scientific` — discoveries, experiments, publications
   - `cultural` — premières, releases, foundings of artistic
     movements
   - `natural` — geological / biological / cosmological events

## Output schema

```json
{
  "canonical_event": "...",
  "kind": "historical" | "biographical" | "scientific" | "cultural" | "natural",
  "participants": ["...", "..."],
  "time": "..." | null,
  "description": "..."
}
```

Single JSON object. No prose, no think block.
