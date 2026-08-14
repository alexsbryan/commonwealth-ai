# Gate redesign — decision note (order `deep-research-t1a`, FR-6 REDESIGN)

The FR-6 measurement (`notes/fr6.md`) found the dual-string premise dead
(100/100 agreement, 0 joint-miss) and the residual failure shape to be
**shared world-knowledge bias**: the judge marks a claim `supported`
because the fact is in the *model*, not in the evidence. The operator
chose REDESIGN (directive c45d8625): **single-string judge + a C-class
containment witness** — a deterministic presence check over the claim's
extracted specifics, generalized from the in-tree
`value_present_in_chunks` / `absent-name-attribution` / `absent-identifier-attribution`
vetoes. This note records the T1 design decisions the order delegates to
the worker: **trigger shape** and **extraction mode**, plus the
**pre-registration** (§18.6) that must precede the gate change shipping.

## §1 The changed gate (what ships)

The loop's audit composes, per claim:

1. **Single-string judge** — `claim_violation_joint` (unchanged, the
   one judge string — ARCH §10.6 one implementation).
2. **C-class containment witness** — NEW, additive, lives in
   `sovereign-core/src/runtime/deep_research/containment.rs` (a new
   module; **no edits to judge.rs or grounding/mod.rs for this change** —
   the native-grounding cutover's file stays untouched by the witness).
   The witness downgrades only; it never upgrades a verdict.

## §2 Trigger shape (decision)

**The witness fires on judge-`supported` claims only, and only when the
claim yields ≥1 extracted checkable specific.**

- `failed` / `could-not-judge` / `never-ran` claims are already refused —
  there is no supported verdict to witness, and no downgrade is possible.
  Firing there would burn model budget for a no-op.
- Rationale from the measurement: the residual failure shape was on the
  *supported* side (bias marks unsupported claims supported). The witness
  is aimed at exactly that residual.
- A claim with no checkable specifics (pure framing, no names/dates/
  figures/relationships) is not witnessable: it stays as the judge said.
  This is a recorded limitation, not a hole — a claim without specifics
  cannot be shown *absent* from evidence (see §4, "absent is
  witnessable").

## §3 Extraction mode (decision)

**LLM extraction, tiny budget, NONE sentinel — then a deterministic
presence check per specific.**

- Extraction: one prompt turn over the claim, asking for its checkable
  specifics (max `specifics_max` = 4, `max_tokens` = 32, temp 0.0).
  `NONE` sentinel when the claim carries no specifics (extending the
  `extract_answer_value` pattern from `grounding/value_presence.rs`).
- Trust class: **I** (instrument). An extraction error costs a witness
  *miss* (a supported claim stays supported), never a false downgrade or
  a false pass — the witness's failure mode is bounded on the honest
  side. A `NONE`-but-should-have-specifics claim is the same bounded
  miss, journaled in the witness record.
- Presence check: **reuse `value_present_in_chunks`** (one matcher,
  ARCH §10.6) per specific — verbatim ≥2-word phrase containment first,
  then all-significant-words with the verified STOP list. Evidence is
  `strip_citation_spans`-ed before the check. Scoping per the documented
  false-positive lessons: check *whole extracted specifics*, never raw
  nouns (the 0.09→0.40 FP explosion), and apply the
  `absent_name_attribution` refusal guards — artifact nouns
  (email/letter/memo/…), capitalized-bigram names, list separators,
  markdown emphasis, heading/label-shaped specifics are **unwitnessable**
  (a specific that matches a heading or label shape proves nothing about
  the evidence body).
- **Verdict effect**: all extracted specifics absent from the evidence →
  `supported` downgrades to `could-not-judge`, with the witness record
  (`ran`, `specifics`, `all_absent`) written into the claim's ICD entry.
  Could-not-judge is never defaulted — the downgrade is a recorded
  verdict change with its evidence.

## §4 What "absent" means — the witness's claim

The witness asserts: *if the claim's checkable specifics are all absent
from the evidence, the claim cannot be grounded on this evidence* — the
supported verdict is a world-knowledge artifact, not a citation.
Symmetrically, presence of ≥1 specific is *not* a pass by itself: the
witness only downgrades; the judge's supported verdict stands when any
specific is present. The witness is a veto with a narrow trigger, not a
second judge.

## §5 Pre-registration (§18.6) — BEFORE the gate change ships

Two frozen instruments, minted in `research/deep-research/adversarial/`
**before** the changed gate ships (before the loop's audit wires the
witness), both authored NWCI (no gate output consulted):

1. **The adversarial sub-bank** — world-knowledge-lean claims paired
   with evidence windows:
   - *negative half*: claims that are plausible, well-formed, and lean —
     their specifics are absent from their paired evidence (the bias
     residual shape). The changed gate must downgrade them.
   - *positive control*: claims whose specifics are verbatim present in
     their paired evidence. The changed gate must keep them supported
     (guards the witness against over-firing).
2. **The frozen longform-negative set** — long-form answers with
   `[Source: …]` citation spans, confident, well-formed, whose specifics
   are NOT in their windows: the shape that fooled the old gate (the
   fr6 residual, at length).

**The frozen-set run precedes the ship**: the frozen set runs against
the judge alone (baseline) and against judge+witness (changed), both
recorded into `adversarial/pre-registration.md` with per-claim rows.
Acceptance shape (declared here, before any run): judge-only passes most
frozen claims (the bias residual); judge+witness downgrades the
frozen negatives to could-not-judge and keeps the positive controls
supported. The adversarial read ships **beside** the gate change, in the
same commit wave — a judge change with no adversarial read beside it is
not a landed change (§18.6).

## §6 Custody-side composition (the other half of "changed gate")

The same audit path consumes the custody ledger: a claim whose
supporting evidence carries `provenance_class: unknown` must refuse
(R-3) — the gate's refusal is the containment witness's sibling veto.
Both vetoes (containment downgrade, unknown-provenance refusal) are
recorded per claim in the gap-list ICD; the report's Open questions
section carries the outcome (custody.md §4, icd-schemas.md §4/§8).

## §7 What this note does NOT decide

The generic chat gate's wiring of the witness (judge.rs/grounding
integration for non-loop paths) is deferred: T1 ships the witness in the
loop's audit path — the product path — only. The generic gate's posture
is a T2 decision with its own measurement.
