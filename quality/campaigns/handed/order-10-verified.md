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
budget: see the mint row's cap in ralph/next/handed/STATE.md (10 — basis below)
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
  the released text contains no factual claim, using the existing claim extractor.
  `Answer::abstained` is already its door.
- **clarifying questions** — the same no-assertion check.
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
3. **Extend `gate_release_census.rs`** to the new doors and to the seven gate-less
   handler files: the six `MINTS` may appear only inside a door, there too. Extend the
   existing test; do not write a second census.
4. **Route the seven gate-less handlers** through a door each, grouped by file, at most
   ~10 files a row.
5. **`TurnFrame::Complete` carries the projection of an `Answer`**, built only from one:
   the four builders in serve.rs must hold an `Answer` to construct a frame, and
   `MessageResponseWire` likewise. The projection is a rendering — text, citations,
   judgement as data — derived from the `Answer` by ONE function, never assembled field
   by field at four sites.
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
   projection field is required and has no `Default`). (c) Add a 14th `Intent` variant ->
   LINT red **E0004** at step 1's mapping. For (c): `guard_story` goes E0004 red on the
   same input TODAY, so the plant must show the error AT step 1's mapping file:line among
   the sites rustc names — "something went E0004" is watching `guard_story`, not this
   rung. The 10 trybuild fixtures in `kernel-types/tests/ui/` are the type's own plants;
   they are re-run, not rewritten.
9. Do the mint's three queue duties (PROMPT §4): the two `depends` lists, a
   `conflicts.txt` pair per shared file, and `ralph.py report` proving the queue parses.

## Cap basis (cap 10 in STATE.md; 15 at round 3, re-priced at round 4 on the census find)

1 row for the mapping, 1 for the doors, 1 for the census extension, 2-3 for the seven
handlers, 1-2 for the two verifiers, 1 for the projection, 1 for the two render holes —
**8-10 rows**. Plants ride their rows' `check:` lists, as every landing row in this
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
