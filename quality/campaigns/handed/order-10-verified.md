---
schema: work-order/v1
id: handed-10-verified
status: draft
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — render: every turn exit releases through the door that already requires a verdict
engine: ralph pool; REVIEW-mint first, then the rows it mints
budget: see the mint row's cap in ralph/next/handed/STATE.md (11 — basis below)
---

# Order: handed-10-verified — the exits adopt the type that already enforces this

## Objective

`docs/ARCHITECTURE_TOUR.md:61` says "nothing ships unverified". hd-1 makes an unverified
exit VISIBLE (`never_ran`); this rung makes it impossible.

**The type is already built and this order does not mint one.** Rewritten at round 4
after a review found it: `kernel_types::Answer` (kernel-types/src/answer.rs:351) has
private fields including `judgement: Judgement`, no `Default`, deliberately no
`Deserialize`, a private `sealed_with` (:329) as its single door, and exactly three
public doors — `Draft::release(provenance, judgements)` :310,
`Draft::release_ungated(provenance, reason)` :324, `Answer::abstained(text, provenance,
reason)` :364. Its own doc: "there is no way to make an `Answer` without saying how much
it should be trusted." The receipt is a LOCAL trait, `pub trait Seal` (:96), which is how
the whole mechanism lives inside kernel-types' `allow = []` contract with no marker crate.

Two earlier drafts got this wrong; both are recorded so the mint does not repeat them.
Draft 1 would have closed `Judgement`'s constructors — 159 sites, 30 files, 9 crates,
only 2 on the turn path. Draft 2 would have minted a second verdict type in
sovereign-contracts behind a marker crate, which could not have worked either: the
verifier and the exits are the SAME crate (sovereign-core), so an `except_from` naming it
lets `serve.rs` mint its own marker, and `except_from` is a layer-gate rule that `LINT`
(cargo check) never runs. **The work is ADOPTION, not invention** (principle 11).

## What is actually missing (measured 2026-09-17)

- **The gate's exits already comply, and a census already guards it.**
  `sovereign-core/tests/main/gate_release_census.rs` — minted for TOPOLOGY §10 phase 9
  rung 9.2, hazard 2, "an `Answer` released without a `Judgement`" — pins five doors in
  `grounding/mod.rs` (`release_held`, `release_flawed`, `abstain`, `release_unjudged`,
  `release_as`) and forbids `Draft::composed(`, `Answer::abstained(` and the four
  `Judgement::` constructors from appearing anywhere else in that file. Its header
  records the adoption history: on 2026-08-26 sixteen sites built `GateOutcome` and
  exactly ONE went through `Draft::release`. That is the instrument this rung extends. It
  does NOT mint a second census (principle 8).
- **Seven of the fourteen turn handlers never reference the gate at all** —
  `sovereign-core/src/runtime/handlers/{ask_move, code_query, commissive, conation,
  document_op, metalingual, recipe_author}.rs` have zero matches for
  `grounding|GateOutcome|gate(`. These are the exits that release text with no verdict,
  and they are this rung's population. The other seven do reference it (knowledge_query
  38, attached_doc 16, simple 14, expressive 11, complex_task 10, generative 3,
  synthesis_common 3) — re-measure before minting, because a count of 3 may be one call
  and two comments.
- **The frame does not carry the Answer.** `TurnFrame::Complete` has four production
  builds (serve.rs:306, :425, :506, :554) and after hd-1 it carries a bare `Judgement`
  beside text. Nothing requires the builder to hold an `Answer`.
- **Operator decision, round 4: the wire carries a PROJECTION of an `Answer`.** `Answer`
  stays in-process and un-deserializable, so the guarantee sits at the point of RELEASE:
  `serve.rs` must hold a real `Answer` to build a `Complete`, and the frame a client
  receives is a rendering of it. Deriving `Deserialize` was refused — it makes
  `from_value` a public constructor, exactly what
  `kernel-types/tests/ui/answer_by_deserialize.rs` exists to forbid, so the rung would
  have spent its own enforcement to save a projection. Moving `Answer` into
  sovereign-contracts was refused with it. The claim this rung can therefore make is
  "nothing can be RELEASED unverified", not "no client can fabricate a frame" — the
  honest ceiling for a wire protocol.
- **hd-1 leaves two render holes this rung inherits** (they were `hd-1-refined`'s, moved
  here at round 3): `MessageRefinedPayload` carries no verdict, and
  sovereign-core/src/runtime/collaboration.rs:693 persists the refined message with
  `metadata: original_metadata` — the pre-refinement gate meta — so history, a desktop
  reload and a json replay show refined text under the previous turn's verdict.
  `collaboration.rs` is 885/885 lines inside arch-gate's no-slack 800-1200 band, so the
  row that touches it pays for its lines or stops.
- **`Intent` is the closed set the surfaces map from** —
  sovereign-contracts/src/types/routing.rs:17, exactly 13 variants. `guard_story`
  (sovereign-core/src/runtime/authority_guard.rs:345) already matches all 13 with no `_`
  arm, so exhaustiveness is an established pattern here.

## What each surface's verifier is (the design; the mint verifies each against the tree)

- **grounded answers** — the existing claims-against-evidence gate. Already done.
- **abstentions** (no documents, no chunks, no step summaries) — a no-assertion verifier:
  the released text contains no factual claim, using the existing claim extractor. Its
  door is `Draft::composed(text, vec![]).release(provenance, &[judgement])`
  (answer.rs:310), NOT `Answer::abstained`: `abstained` (:364-372) takes a `Reason` and
  hardcodes `Judgement::failed`, so routing a verified abstention through it DISCARDS the
  verifier's verdict and stamps `Failed` whether the check passed or not — census green,
  promise false. `abstained` stays the door for the one surface that genuinely declined.
- **clarifying questions** — the same no-assertion check, through the same
  `Draft::composed(...).release(...)` door for the same reason.
- **fixed templates** (the wellbeing crisis text) — identity against the approved string.
- **generative and expressive** — the deterministic vetoes that already exist: the
  numeric audit (the model never originates a number) and the invented-identifier
  refusal. No model call, so no added latency on a creative turn.
- **metalingual** — the conversation is the evidence; it seals like any other corpus.

## Steps

1. **Map `Intent` to its surface, exhaustively.** 13 arms, no `_` arm, in the crate that
   owns the turn. This is the decider: a new `Intent` cannot compile until someone names
   what verifies it.
2. **A door per surface, beside the gate's five**, each wrapping exactly one kernel-types
   constructor and nothing else — the shape `gate_release_census.rs` already pins.
3. **Refactor `gate_release_census.rs` so it CAN be extended — instrument only, no new
   coverage in this row.** Today `grounding_source()` (census:50-53) is zero-argument and
   hardcodes one path, `DOORS` is a flat const, and `door_spans` (:63-69) PANICS when
   `src.find(door)` misses, with a message naming `grounding/mod.rs`. Point it at seven
   files that have no doors yet and it panics seven times over, so a row that extends
   coverage before step 4 adds the doors cannot land. This row parameterizes the path and
   the panic message and makes `DOORS` a per-file set; each file's entry then rides the
   step-4 row that adds that file's door, so coverage and doors land together and the
   census is never red for a file that has not been converted yet.
4. **Classify the seven, THEN route the ones that are exits** — grouped by file, at most
   ~10 files a row. The seven were found by grep-absence (`grounding|GateOutcome|gate(`
   scoring zero), which finds files that do not mention the gate, not files that release
   text. At least two are known not to be plain exits: `code_query.rs:97` DELEGATES to
   `handle_knowledge_query`, the gated path, so a door there either double-stamps or is
   dead; and `document_op.rs:286-328` releases `result_text` from a TOOL execution — text
   it did not compose, which is this order's fourth Kill. `recipe_author` and `conation`
   have no surface assigned by the design section above. So the row's first act is a
   classification, recorded in the commit: exit (gets a door), delegator (gets none, and
   say which gated path it reaches), or not-a-turn-exit (out of scope, named). Step 1's
   exhaustive map cannot be written until that classification exists.
5. **The projection EXISTS — make it required and make it the only way in.**
   `TurnFrame::Complete` already carries `epistemic_state: Option<EpistemicState>`
   (sovereign-contracts/src/types/turn.rs:126), and `EpistemicState.citations` is already
   projected from a `kernel_types::Answer` by `EpistemicState::citations_of(answer,
   headings)` (types/epistemic.rs:68, since rung `nc-20-turn-adoption`). So do NOT add a
   second projection or a third citation list — that is this order's third Kill. The gaps
   are: the field is `Option`, so a frame can omit it; and the judgement is not part of
   what the projection derives. Both are this row's work: `epistemic_state` stops being
   optional (or the row states why it cannot be and what that costs), the judgement joins
   what `citations_of`'s sibling derives from the same `&Answer`, and the projection's
   constructor takes `&Answer` — otherwise it is a pub-field DTO and the four serve.rs
   builders (:306, :425, :506, :554) can assemble it field by field with step 8(b)'s
   E0063 still passing, which would leave "ONE function" with no watcher at all.
   NOTE for the reader and the audit: `EpistemicState.verdict: TurnVerdict`
   (`Grounded | CannotKnowFromHere | GeneralKnowledge | Mixed | Unverified`,
   epistemic.rs:357, computed in sovereign-core/src/runtime/epistemic.rs) is a DIFFERENT
   axis from `Judgement`'s verdict — what basis the answer has, versus whether a check ran
   and what it said. Two facts, not two implementations of one, so they coexist; a rung
   that collapsed them would be deleting a distinction, not removing a duplicate.
6. **Mint the two verifiers that do not exist** (no-assertion, template identity) and
   wire the existing deterministic vetoes as the creative surfaces' verifier. `never_ran`
   narrows here: a verifier that crashed or timed out, its reason naming the failure
   rather than the intent.
7. **Close the two render holes hd-1 left** (above): `MessageRefinedPayload` carries the
   verdict, and collaboration.rs:693 persists the POST-refinement meta. Price the line
   budget in the row.
8. **PLANTs, each with its code.** (a) Put a `Judgement::passed(` beside a handler's
   release, outside any door -> `TEST(sovereign-core)` red on the extended
   `gate_release_census` naming that file, which is the assertion the audit checks.
   (b) Build a `Complete` from text without an `Answer` -> LINT red **E0063** (the
   projection field is required and has no `Default`). E0063 alone only proves the field
   is PRESENT, so the row also plants the field-by-field assembly it is meant to forbid:
   construct the projection at one serve.rs builder without an `&Answer` in hand -> the
   error the private constructor produces, which the row names. (c) Add a 14th `Intent` variant ->
   LINT red **E0004** at step 1's mapping. For (c): `guard_story` goes E0004 red on the
   same input TODAY, so the plant must show the error AT step 1's mapping file:line among
   the sites rustc names — "something went E0004" is watching `guard_story`, not this
   rung. The 10 trybuild fixtures in `kernel-types/tests/ui/` are the type's own plants;
   they are re-run, not rewritten.
9. Do the mint's three queue duties (PROMPT §4): the two `depends` lists, a
   `conflicts.txt` pair per shared file, and `ralph.py report` proving the queue parses.

## Cap basis (cap 11 in STATE.md; 15 at round 3, 10 at round 4, 11 after round 5)

1 row for the mapping, 1 for the doors, 1 for the census INSTRUMENT refactor (step 3 no
longer extends coverage — each file's entry rides its step-4 row), 1 for classifying the
seven and 2 for routing the ones that are exits, 1-2 for the two verifiers, 1 for the
projection, 1 for the two render holes — **9-11 rows**. The cap is the top of the
measured range, not the middle: a cap below it is a predicted stop, and this rung has
already spent three designs. Plants ride their rows' `check:` lists, as every landing row in this
campaign does. Nothing is charged for minting a type, a marker crate, an ARCH_LAYERS
entry or a census: all four exist. Past 10, PROMPT §4 applies — write
`ralph/NEEDS_HUMAN.md` with the measured count and stop.

## Kill — read this before minting

- **More than one or two surfaces cannot name a defensible verifier.** STOP and keep
  hd-1's labelled form. A door whose verifier is a rubber stamp is worse than an honest
  `never_ran`, and the census would pin the stamp in place.
- **A surface's verifier needs a model call on a latency-sensitive path.** The creative
  surfaces are deterministic-only by design; making them otherwise changes what a user
  waits for.
- **The projection becomes a second decider** — assembled at the four frame builders
  rather than derived by one function, or a `Judgement` that can differ between the
  `Answer` and the frame. Stop: that is principle 8 and it undoes the rung's own claim.
- **A handler cannot reach a door without an `Answer` it has no evidence for** (it
  releases text it did not compose, or composes after the gate ran). Stop and name it:
  that exit is a design question, not a routing one.
