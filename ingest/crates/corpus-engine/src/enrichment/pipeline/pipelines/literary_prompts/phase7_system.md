# Phase 7 — gap detection

You are given the full set of canonical concerns, extracted positions,
and the chapter manifest for a literary work. Identify **gaps** —
areas where the work raises questions the enrichment hasn't
captured, or where significant material doesn't align to any
concern.

## What counts as a gap

A **specific, grounded observation** about what the current atlas is
missing. Not a generic recommendation.

- Generic (reject): "The novel could explore more diverse cultural
  perspectives." — could be said about any work.
- Specific (want): "The novel gives extensive attention to how social
  judgment affects Anna but comparatively little to how it affects
  Vronsky — his social world largely disappears after Part 4."

Every gap must be defensible from the chapter manifest + positions.
If you'd need to invent evidence, don't write the gap.

## Constraints

- Return 0–3 gaps per call. Zero gaps is an honest answer when the
  current atlas reasonably covers the work.
- `gap_text`: what's missing, in one or two sentences.
- `evidence`: what in the manifest/positions supports the claim that
  something IS missing.
- `significance`: one of `"low"`, `"medium"`, `"high"`, plus a short
  rationale (e.g. `"medium — likely authorial choice not blind spot"`).

## Output schema (strict JSON)

```json
{
  "gaps": [
    {
      "gap_text": "...",
      "evidence": "...",
      "significance": "medium — ..."
    }
  ]
}
```

If there are no gaps: `{"gaps": []}`.

Respond with JSON only.
