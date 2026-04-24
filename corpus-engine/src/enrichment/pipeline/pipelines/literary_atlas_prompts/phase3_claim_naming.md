# Phase 3 (atlas) — claim cluster naming (literary)

You receive a cluster of claims the pipeline extracted from a novel
— each with a `discourse_act` (enact / imply / interpret / …) and
an `epistemic_status` (confident / tentative / contested / …). Name
the **position family** the cluster articulates.

## What a good name looks like

A **position family** — the proposition the cluster defends or
enacts, phrased so the discourse act is visible. Not a topic.

- **Good.** "That active love, exercised against the world, is
  harder and more necessary than love imagined in the cell."
- **Bad.** "Love as work." (a topic, not a position)
- **Bad.** "A position about love." (vacuous)

For narrative-prose claims (`discourse_act: enact|imply`) the label
can read as a paraphrase of what the text *shows*: "that passion
outside the social order destroys the person who feels it." For
attributed claims (`discourse_act: argue|assert`), lead with the
attribution implicitly — "that compatibilism under-describes moral
blame" rather than "Zosima argues that...".

## Output schema

```json
{
  "label": "<the position in one clause>",
  "metadata": {
    "attributed_to": "<entity name or omit>",
    "dominant_act": "argue | assert | enact | imply | interpret | hypothesize | warn | commit | object"
  }
}
```

- `label` — required. Non-empty.
- `metadata.attributed_to` — the dominant attributed entity across
  the cluster if one exists, omit for text-level claim families.
- `metadata.dominant_act` — the discourse_act that best characterises
  the whole cluster. Use whichever act most of the members share; on
  ties, prefer the more structural act (`enact` > `imply` > other).
  Omit when the cluster is mixed in a way that resists a single label.
