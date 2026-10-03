# Phase 6 — pairwise Tension classifier (atlas pipeline)

You are reviewing a candidate pair of atoms from a corpus's atlas
and deciding whether the pair is in genuine **structural tension**.

A prior deterministic pass enumerated this candidate because the two
atoms share a participant (the same entity is named in both). Your
job is to classify whether the pair is **actually in tension** or
merely co-occurs around the same entity.

## What counts as Tension

Tension means the two atoms cannot both be fully true / fully obtain
*at the same time* without something giving — a value, a commitment,
a possibility, a priority. The atoms hold incompatible expectations
about the same participant, and the corpus is *navigating* that
incompatibility (not simply juxtaposing two facts).

Three flavours of genuine Tension:

- **Stated-vs-enacted.** A claim asserts X; a state shows the entity
  doing not-X. (A character vows revenge but cannot bring themselves
  to act.)
- **Two-pulls-on-the-same-axis.** Two states or two claims hold
  incompatible commitments about the same entity. (One state has the
  entity loyal to family; another has them planning to leave.)
- **Frame-collision.** A claim about the entity in one register
  conflicts with a state in another register. (Public claim of
  competence vs. private state of self-doubt.)

## What does NOT count as Tension

- **Mere co-occurrence.** Two atoms about the same entity that hold
  compatible or unrelated content. ("X is a priest" + "X visits a
  parishioner" — same entity, no incompatibility.)
- **Redundancy.** Two atoms that say the same thing differently. ("X
  is anxious" + "X feels uneasy" — aligned, not in tension.)
- **Temporal succession.** A state that simply *follows* a claim. ("X
  decides to leave" + "X leaves the next morning" — sequenced, not
  conflicting.)
- **Different entities collapsed.** If the candidate's "shared
  entity" is a generic role (e.g. "the family", "the audience") and
  the two atoms are about *different members* of that role, there is
  no real shared participant. Reject.

## Examples (form, not content — drawn from texts unrelated to whatever
## you are classifying)

**Yes — stated-vs-enacted:** Claim: "Macbeth resolves to murder
Duncan that night." State: "Macbeth, vacillating, frozen by
conscience." Both are about Macbeth; the resolve and the
paralysis cannot both fully obtain. `sub_question`: "Is Macbeth's
resolve to murder a settled commitment or a brittle posture?"

**Yes — two-pulls:** State: "Heathcliff, devoted to Cathy as a child."
State: "Heathcliff, vengeful toward Cathy's family across decades."
Same entity; the devotion and the long vengeance pull in opposite
directions on the same axis (his bond to Cathy). `sub_question`:
"Does Heathcliff's devotion survive his vengeance, or has the
vengeance consumed it?"

**Yes — frame-collision:** Claim (public): "Mr. Collins is honoured
to serve Lady Catherine." State (private): "Mr. Collins, smarting
under his own obsequiousness." Same entity, different registers,
incompatible. `sub_question`: "Is Mr. Collins's deference voluntary
or involuntary?"

**No — mere co-occurrence:** Claim: "Sherlock Holmes solves the case
through observation." State: "Sherlock Holmes, in his rooms at Baker
Street." Both about Holmes, but the location and the methodology
are not in conflict — the corpus isn't navigating an incompatibility
here.

**No — redundancy:** Claim: "Pip is full of shame after the dinner."
State: "Pip, ashamed before Estella." Aligned. The corpus is
emphasising one thing, not setting up a tension.

**No — temporal succession:** Claim: "Anna decides to take the night
train." State: "Anna, on the platform with her child." The state
follows the decision; there is no incompatibility being navigated.

## Output

Return exactly one JSON object with these keys, in this shape:

```json
{
  "is_tension": true,
  "sub_question": "<one-sentence question the tension turns on; only when is_tension is true>",
  "confidence": 0.85,
  "rationale": "<one short sentence naming the structural incompatibility>"
}
```

Or for a non-tension:

```json
{
  "is_tension": false,
  "rationale": "<one short sentence naming why these atoms co-occur but do not conflict>"
}
```

## Hard constraints

- `is_tension` is required and must be boolean (not string, not null).
- `confidence` is a number in `[0.0, 1.0]`. Default to `0.7` if you
  are reasonably confident; reserve `0.9+` for clear-cut cases.
- `sub_question` is required when `is_tension` is true; omit when
  false.
- `rationale` is required (one sentence, no more).
- Return JSON only — no `<think>` block, no prose before or after,
  no markdown code fences. Begin output with `{` now.

## Calibration

The deterministic pass enumerates every claim/state pair that shares
an entity. **Most candidates are not tensions.** A precision-leaning
classifier that passes 1-3 of every 10 candidates is doing better
than one that passes 7. When in doubt, lean toward `is_tension:
false` and write a one-sentence rationale of why the candidate
co-occurs without conflict.
