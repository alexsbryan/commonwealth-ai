---
schema: work-order/v1
id: handed-10-verified
status: draft
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — render: a released verdict is a type only a verifier can mint
engine: ralph pool; REVIEW-mint first, then the rows it mints
budget: see the mint row's cap in ralph/next/handed/STATE.md (15, re-priced at round 3 — basis below)
---

# Order: handed-10-verified — the released verdict is its own type

## Objective

`docs/ARCHITECTURE_TOUR.md:61` says "nothing ships unverified". hd-1 makes an
unverified exit VISIBLE (`never_ran`) and not impossible. This order makes the
sentence true at compile time: a verdict that rides a released turn becomes its own
type whose constructor is private behind a receipt only a verifier can mint, so an
exit cannot hand-roll one, and `Complete` already cannot be built without one (hd-1).

**Operator decision, round 3 (2026-09-17): SPLIT THE TYPE.** The first draft of this
order closed `Judgement`'s own constructors. Measured against the tree that reaches
**159 construction sites in 30 files across 9 crates, and only 2 of them are in
sovereign-core** — the rest are `posture_cmd`, `quality_check`, `refactor_wire`,
`lane_verdict`, `commonwealth-work` and the CLIs, which judge subsystems and report
quality, not released text. Closing one constructor for both populations would have
forced a cheap escape hatch that every reporting caller could reach for, which is
the substitution principle 6 forbids. So: released-turn verdicts get their own type
with a private constructor; `Judgement` stays exactly as it is for reporting.

`TARGET_ARCHITECTURE.md` §2 specifies this receipt — "mintable only by a calibrated
judge, private constructor, capability token" — and marks it `target`. This is where
it gets built.

## Premises (verified 2026-09-17, file:line — three of them correct an earlier draft)

- **`Judgement`** — kernel-types/src/judgement.rs:320-331: private fields, private
  `fn new` :333, four public constructors `passed` :345, `failed` :351,
  `could_not_judge` :357, `never_ran` :363, plus the `as_of`/`stale_after` builders.
  Construction census (`git grep -o 'Judgement::(passed|failed|could_not_judge|never_ran)'`):
  **159 sites / 30 files / 9 crates**; sovereign-core holds **2**. This is the number
  that decided the split — re-run it before minting, and if sovereign-core's share has
  grown past a handful the split is still right but the row count moves.
- **`GateSurface` is NOT the verifier taxonomy.** It is `pub(crate)` at
  sovereign-core/src/runtime/grounding/config.rs:460 with 8 variants —
  `SimpleQuery, KnowledgeQuery, DeepQuery, ComplexTask, AttachedDoc, Governance,
  ProxyArgument, Refinement` — which are ENTRY kinds, one calibration bank each. None
  of the six verifier surfaces below is one of them. An earlier draft of this order
  claimed "`GateSurface` already exists as a closed enum of 8 variants … every exit
  names one"; that is false, and reusing it would have conflated "which bank calibrates
  this turn" with "what proves this text". Leave `GateSurface` alone.
- **`Intent` is the closed set to map from** — sovereign-contracts/src/types/routing.rs:17,
  exactly **13** variants (`SimpleQuery, DeepQuery, KnowledgeQuery, ComparisonQuery,
  MetalingualQuery, ConationQuery, CommissiveQuery, ExpressiveQuery, GenerativeQuery,
  CodeQuery, SimpleAction, ComplexTask, Continuation`). `guard_story`
  (sovereign-core/src/runtime/authority_guard.rs:345) already matches all 13 with no
  `_` arm, so an exhaustive Intent match is an established pattern in this crate and
  E0004 is a watched failure mode here, not a new one.
- **kernel-types can host the new type.** Its `[dependencies]` are serde, getrandom,
  hex (+ one optional feature) and it is the bottom of the layer map, so a type and a
  marker beside `Judgement` create no upward edge.
- **The receipt technique already exists by the time this rung runs.** hd-2 mints
  `sovereign-commission-seal` (a zero-dep crate holding an unconstructible-from-outside
  marker) AND `except_from`, the layer-gate rule that restricts who may depend on it.
  Reuse that shape rather than inventing a second one (principle 11); the mint's first
  job is to confirm hd-2 landed it.
- **`CalibrationReceipt` is the precedent and it is also the cautionary tale** —
  sovereign-cli-llm/src/gym_judge/mod.rs:62, consumed at :161. Verified 2026-09-17:
  it sits far above kernel-types, it is minted inside the crate that consumes it, and
  `pub fn untrusted()` (:82) lets ANY caller mint one — the doc calls it a dev escape
  hatch that warns on every judge call. Its trusted path,
  `CalibrationSource::PassedBank`, is `#[allow(dead_code)]` and marked "Reserved — not
  yet wired", so in this repo the receipt pattern has never once minted a trusted
  instance. Copy the SHAPE (a marker no outside caller can construct) and not the
  implementation: a receipt with a public `untrusted()` leaves this rung's PLANT
  green, which is §6, and the warn-on-use consolation is exactly the "named
  substitution" that principle 6 permits for DATA and this rung is refusing for code.
- **hd-1 leaves two render holes this rung inherits** (they were `hd-1-refined`'s and
  moved here at round 3): `MessageRefinedPayload` carries no verdict, and
  sovereign-core/src/runtime/collaboration.rs:693 persists the refined message with
  `metadata: original_metadata` — the gate meta from BEFORE the refinement — so
  history, a desktop reload and a json replay all show refined text under the previous
  turn's verdict. `collaboration.rs` is at 885/885 lines inside arch-gate's no-slack
  800-1200 band, so a row that touches it pays for its lines or stops.

## What each surface's verifier is (the design; the mint verifies each against the tree)

Six surfaces, and the mapping from `Intent` is what makes the set closed:

- **grounded answers** — the existing claims-against-evidence gate.
- **abstentions** (no documents, no chunks, no step summaries) — a no-assertion
  verifier: the released text contains no factual claim. Uses the existing claim
  extractor.
- **clarifying questions** — the same no-assertion check.
- **fixed templates** (the wellbeing crisis text) — identity against the approved
  string.
- **generative and expressive** — the deterministic vetoes that already exist: the
  numeric audit (the model never originates a number) and the invented-identifier
  refusal. No model call, so no added latency on a creative turn.
- **metalingual** — the conversation is the evidence; it seals like any other corpus.

## Steps

1. **Converge the nouns, then mint them.** `sovereign code converge noun <Name>` for
   the verdict type and the surface enum BEFORE writing either (paste both). In
   kernel-types beside `Judgement`: the released-verdict type (it wraps a `Judgement`
   and names its surface, fields private, NO public constructor) and the surface enum
   (the six above, closed). Nothing else changes in kernel-types.
2. **One total mapping, exhaustively matched.** `Intent -> surface`, all 13 arms, no
   `_` arm, in the crate that owns the turn. This is the decider: a new `Intent`
   variant cannot compile until someone names what verifies it. PLANT here is E0004.
3. **The receipt.** The verdict type's constructor takes a marker that only a verifier
   can hold, using hd-2's seal shape (zero-dep crate + an `except_from` rule naming the
   verifier crate). If hd-2's rule cannot be reused, stop and report rather than
   substituting a mintable-anywhere marker — a receipt its own consumer can mint is
   not a receipt.
4. **Every turn exit names its surface** and carries the new type where hd-1 put a
   `Judgement`: `TurnFrame::Complete`, `MessageResponseWire`, and the ~14 exits. Group
   by file, at most ~10 files a row.
5. **Mint the verifiers that do not exist** (no-assertion, template identity) and wire
   the existing deterministic vetoes as the creative surfaces' verifier. `never_ran`
   narrows here: it means a verifier that crashed or timed out, and its reason names
   the failure, not the intent.
6. **Close the render holes hd-1 left** (Premises, last bullet): `MessageRefinedPayload`
   carries the verdict, and collaboration.rs:693 persists the POST-refinement meta. The
   line budget on that file is the row's problem to price — say so in the row.
7. **PLANT, two of them, each with its code.** (a) Construct the verdict type at a turn
   exit outside a verifier -> LINT red **E0603** (private constructor) or **E0624** if
   the constructor is an inherent private method; name which before running it, because
   the audit checks the code the row names. (b) Add a 14th `Intent` variant -> LINT red
   **E0004** at step 2's mapping. Revert both, LINT green.
8. Append every row you mint to the `depends` of `REVIEW-DEMO-hd-7-bench` and of
   `REVIEW-audit-hd-2` in `ralph/next/handed/STATE.md` (or `ralph/STATE.md` once
   promoted), and add a `conflicts.txt` pair for any two rows that edit one file, in the
   same commit that mints them. Without it the pool's `first_ready_review`
   (scripts/ralph.py:256-261) runs the bench and the final audit before the rows they
   are meant to cover, and `pick_wave` puts two rows on one file in one wave.

## Cap basis (re-priced at round 3; cap 15 in STATE.md)

Under the split the work is the TURN path, not the 159-site census: 1 row for the two
nouns, 1 for the mapping, 1 for the receipt, 2-3 for the exits (~14 sites, ~10 files a
row), 2-3 for the verifiers that do not exist, 1 for the two render holes, 1 for the
plants — **9-11 rows, with 15 as the ceiling**. If the mint measures more than 15,
PROMPT §4 applies: write `ralph/NEEDS_HUMAN.md` with the count and stop. Growth past a
cap is a design finding, never a queue edit.

## Kill — read this before minting

- **More than one or two exits cannot name a defensible verifier.** STOP and keep hd-1's
  labelled form. Closing the constructor while an exit has no honest verifier forces
  someone to invent a fake one, which is worse than an honest `never_ran`.
- **The receipt can be minted by the code it is meant to constrain** (hd-2's rule cannot
  be reused, or the only available shape is a public `mint()`): stop. The type is then
  convention, not structure, and the plant would stay green.
- **A surface's verifier needs a model call on a latency-sensitive path.** The creative
  surfaces are deterministic-only by design; making them otherwise changes what a user
  waits for.
- **The split needs a second decider.** If a released verdict and a reported judgement
  turn out to need the same formula in two places — a second `verdict` accessor, a second
  freshness rule — stop: that is principle 8, and the answer is one type with two
  constructors, not two types.
