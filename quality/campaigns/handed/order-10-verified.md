---
schema: work-order/v1
id: handed-10-verified
status: draft
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — render: nothing ships unverified
engine: ralph pool; REVIEW-mint first, then the rows it mints
budget: see the mint row's cap in ralph/next/handed/STATE.md
---

# Order: handed-10-verified — every exit names a surface, every surface has a verifier

## Objective

`docs/ARCHITECTURE_TOUR.md:61` says "nothing ships unverified". Today about fourteen
exits release text with no verifier, and hd-1 makes that visible (`never_ran`) but
not impossible. This order makes the documented sentence true and enforces it at
compile time: `Judgement`'s constructors become private behind a receipt only a
verifier mints, so an exit cannot hand-roll a verdict, and `Complete` already cannot
be built without one (hd-1).

`TARGET_ARCHITECTURE.md` §2 specifies exactly this receipt for `Judgement` — "mintable
only by a calibrated judge, private constructor, capability token" — and marks it
`target`. This is where it gets built.

## What each surface's verifier is (the design; the mint verifies each against the tree)

- grounded answers — the existing claims-against-evidence gate.
- abstentions (no documents, no chunks, no step summaries) — a no-assertion verifier:
  the released text contains no factual claim. Uses the existing claim extractor.
- clarifying questions — the same no-assertion check.
- fixed templates (the wellbeing crisis text) — identity against the approved string.
- generative and expressive — the deterministic vetoes that already exist: the numeric
  audit (the model never originates a number) and the invented-identifier refusal.
  No model call, so no added latency on creative turns.
- metalingual — the conversation is the evidence; it seals like any other corpus.

`GateSurface` already exists as a closed enum of 8 variants with one calibration bank
each (sovereign-core runtime grounding config.rs ~:460). Every exit names one.

**The persisted-verdict hole the receipt work closes.** Verified 2026-09-17:
sovereign-core/src/runtime/collaboration.rs:693 writes the refined `Message` with
`metadata: original_metadata` — the gate meta from BEFORE the refinement — so the STORED
row carries refined text under the previous turn's verdict, and history, a desktop reload
and a json replay all read it back that way. `hd-1-refined` fixes the LIVE notice only:
`collaboration.rs` is at 885/885 lines, inside arch-gate's 800-1200 approach band which
has no slack, so that row has no headroom for the store-side fix. Whatever shape the
receipt takes, the write at :693 is one of its sites.

## Steps

1. Each turn exit names its `GateSurface`. Mechanical, ~14 sites; group by file.
2. Mint the verifiers that do not exist yet (no-assertion, template identity) and wire
   the deterministic vetoes as the creative surface's verifier.
3. Close the constructors: `Judgement::{passed,failed,could_not_judge}` take a receipt
   only a verifier can mint; `never_ran` narrows to a verifier that crashed or timed
   out, and its reason must name the failure, not the intent.
4. PLANT: hand-roll `Judgement::passed` at a turn exit outside a verifier; LINT reports
   the private-constructor error. Revert, LINT green.
5. Append every row you mint to the `depends` of `REVIEW-DEMO-hd-7-bench` and of
   `REVIEW-audit-hd-2` in `ralph/next/handed/STATE.md` (or `ralph/STATE.md` once
   promoted), in the same commit that mints them. Without it the pool's
   `first_ready_review` (scripts/ralph.py:256-261) can run the bench and the final
   audit before the rows they are meant to cover.

## Kill — read this before minting

If more than one or two exits cannot name a defensible verifier, STOP and keep hd-1's
labelled form. Closing the constructor while an exit has no honest verifier forces
someone to invent a fake one, which is worse than an honest `never_ran`.

Also stop if a surface's verifier needs a model call on a latency-sensitive path: the
creative surfaces are deterministic-only by design, and making them otherwise changes
what a user waits for.
