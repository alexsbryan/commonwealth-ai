# Phase 0 — section classifier (MECE axis vector)

You are reading the title, frontmatter, and opening of one section
from a heterogeneous note-taking corpus (essays, fiction, journals,
zettel cards, project notes, meeting recaps, poetry, drafts). Your
job is to classify the section across three orthogonal MECE axes
plus one optional axis, so a downstream extractor can pick the right
atom shapes for the genre.

You are **not** summarising the content. You are answering "what is
this section's language *doing*, what truth-claim is it making, and
where in time does it sit?"

## The four axes

### A. Discourse Mode — *what is the language doing?* (weighted)

Pick the **primary** mode plus up to two **secondary** modes whose
weight ≥ 0.20. Weights are how much of the section's load-bearing
work each mode does — a reader would miss this part of the content
if you ran only the other modes' extractors. Weights MUST sum to
1.0 (within ±0.01) across primary + secondaries.

| Value | Reader test |
|---|---|
| `narrative` | Sequences events through time with agents. "Then X happened, then Y." |
| `argumentative` | Supports a claim with reasons + evidence against an alternative. "I argue X because Y, contra Z." |
| `descriptive` | Lays out a thing's structure / properties / relationships. "This is what X is and how its parts connect." |
| `reflective` | First-person processing of experience or thought. "I noticed X and what I make of it is Y." |
| `procedural` | Instructs through steps / commitments / dependencies. "We will do X. Tasks: …" |
| `lyric` | Compressed expressive language: imagery / rhythm / tonal movement over information transfer. |

Most sections are a hybrid. Concrete shapes you should expect:

- A long-form policy essay that opens with a vignette: `argumentative @ 0.65, narrative @ 0.35`.
- A daily journal entry that recounts events and reflects on them: `reflective @ 0.55, narrative @ 0.45`.
- A zettel definition card: `descriptive @ 1.0`.
- A short story: `narrative @ 1.0`.
- A meeting recap that records what was decided and assigns follow-ups: `procedural @ 0.6, descriptive @ 0.4`.
- A poem: `lyric @ 1.0`.

Do not coin spurious secondaries. A section that does one thing
cleanly is `primary @ 1.0` with no secondaries.

### B. Epistemic Posture — *what truth claim?* (single value)

Pick one. This axis modulates downstream Claim atoms with
normative-marker / counterfactual flags.

| Value | Test |
|---|---|
| `factual` | Claims things are the case in the actual world. "X is happening." |
| `normative` | Claims things should be / ought to be — value judgments. "X is bad / good / problematic." |
| `fictional` | Claims a story-world; not bound to actual-world truth. |
| `hypothetical` | Explores "what if X" without committing. "Suppose X — then Y would follow." |

Hybrid factual+normative essays (most policy writing) classify as
`normative` — the claim-modulator applies per claim downstream, so
the section-level value carries the dominant posture.

### C. Temporal Frame — *where in time?* (single value)

Pick one. Modulates Event/Task atoms with `when` / `target_state`
flags.

| Value | Test |
|---|---|
| `episodic` | Specific dated/located events. The section anchors to a time. |
| `atemporal` | Claims/structures unbound from time. Definitions, principles, general theses. |
| `prospective` | Planned / expected / intended future. "We will / should / plan to X." |

### D. Audience Relation — *who is this for?* (optional)

Optional. Affects rendering tone downstream; not load-bearing for
atom shape. If you cannot tell from the opening, omit.

| Value | Test |
|---|---|
| `private_first_person` | For self — diary, scratch notes, day-end pages. |
| `specific_recipient` | For a named other — meeting recap, email-shaped note. |
| `public_impersonal` | For unknown readers — published essay, story, manuscript. |

## Calibration ladder for the discourse-mode weights

- **`primary @ 1.0`** — single-mode section. Definition cards, pure
  stories, pure poems, a one-page argument that doesn't narrate or
  describe.
- **`primary @ 0.80–0.99` + one secondary** — secondary mode shows
  up but does light work. A policy essay that opens with a one-line
  anecdote: `argumentative @ 0.85, narrative @ 0.15`.
- **`primary @ 0.55–0.70` + one secondary** — true hybrid. Both
  modes carry load-bearing structure. A journal day that recounts
  events and dwells on what they mean: `reflective @ 0.60, narrative @ 0.40`.
- **`primary ≤ 0.50`** — rare. Use only when ≥ 2 modes are
  genuinely co-equal. Cap at two secondaries.

## Output schema (strict JSON)

Return exactly one JSON object. No prose, no `<think>` block, no
code-fence markers. Keys exactly as named here.

```json
{
  "discourse_mode": {
    "primary": "argumentative",
    "primary_weight": 0.65,
    "secondaries": [
      ["narrative", 0.35]
    ]
  },
  "epistemic_posture": "normative",
  "temporal_frame": "atemporal",
  "audience_relation": "public_impersonal",
  "reasoning": "Opens with a Wheeler-family winter vignette (narrative) then turns to a sustained argument about industrial seasonality (argumentative, normative). The claim is policy-flavoured rather than story-flavoured, so weights tilt argumentative."
}
```

## Hard constraints

- Return strictly valid JSON. No prose, no code-fence markers.
- `discourse_mode.primary_weight` ∈ `(0.0, 1.0]`; each secondary
  weight ∈ `(0.0, primary_weight)`; weights sum to `1.0` ± 0.01.
- `secondaries` has 0–2 entries. Sorted by weight descending.
- `epistemic_posture` ∈ {`factual`, `normative`, `fictional`, `hypothetical`}.
- `temporal_frame` ∈ {`episodic`, `atemporal`, `prospective`}.
- `audience_relation` ∈ {`private_first_person`, `specific_recipient`, `public_impersonal`} OR omit entirely.
- `reasoning` is one short paragraph (≤ 400 chars) the operator can
  audit — cite the cues you used.
