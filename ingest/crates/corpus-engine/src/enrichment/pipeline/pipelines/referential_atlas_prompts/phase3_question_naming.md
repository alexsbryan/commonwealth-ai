# Phase 3 — question cluster naming (referential)

You are naming a cluster of questions extracted from a referential
corpus (encyclopedia, wiki, reference work). The cluster has been
formed by embedding similarity — its members ask versions of "the
same question shape" in different surface forms.

Your output is a single canonical question that captures what the
whole cluster is asking, plus a one-line description of the kind
of answer the cluster invites.

## Rules

1. **Phrase the canonical question as a user would type it.** Not
   as a librarian would index it. "What caused the fall of the
   Roman Empire?" — not "Causes of the decline of the Western
   Roman Empire (3rd–5th c.)".

2. **Preserve the question's `kind`** if all members agree on
   `factual` / `definitional` / `causal` / `comparative` /
   `procedural`. If members disagree, pick the dominant kind and
   note the dissent in the description.

3. **Don't synthesise away differences.** If the cluster has
   members about *Roman fall* and *Byzantine fall*, the canonical
   question should name the broader pattern ("How do empires
   decline?") OR be split — your call. Note the split candidates
   in the description if you decline to combine.

4. **Surface alternate phrasings** a reader might use. Add 1–3
   `aliases` covering natural-language variants ("decline of Rome",
   "why did Rome collapse"). These are retrieval anchors.

## Output schema

```json
{
  "canonical_question": "...",
  "kind": "factual" | "definitional" | "causal" | "comparative" | "procedural",
  "description": "...",
  "aliases": ["...", "..."]
}
```

Single JSON object. No prose, no think block.
