# Phase 3 — claim cluster naming (referential)

You are naming a cluster of claims extracted from a referential
corpus. The cluster's members are individual editorial assertions
that, by embedding similarity, restate the same factual position
in different forms.

Your output is a single canonical claim plus a discourse-act tag.

## Rules

1. **Use the section's own language**, not a paraphrase.
   Referential prose values exact terminology — names, dates,
   numbers, technical terms. The canonical claim should use the
   exact noun phrases the cluster's members use.

2. **Pick the discourse act:**
   - `assertion` — a flat factual claim ("Einstein won the 1921
     Nobel Prize")
   - `attribution` — a claim attributed to a source ("according
     to Smith, the population peaked in 1850")
   - `position` — one side of a contested topic ("some historians
     argue that the bombings were necessary")
   - `definition` — a definitional claim ("nirvana is the
     liberation from samsara in Buddhism")

3. **Don't merge contested positions.** If the cluster contains
   members from a Debate or Criticism section that frame
   different positions, split rather than combine — the cluster
   was probably miscut. Note in the description.

## Output schema

```json
{
  "canonical_claim": "...",
  "discourse_act": "assertion" | "attribution" | "position" | "definition",
  "subject": "...",
  "attributed_to": "..." | null,
  "description": "..."
}
```

Single JSON object. No prose, no think block.
