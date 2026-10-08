# What a record is made of: a task model for the ontology layer (draft 3, 2026-10-07)

This replaces reasoning from the machinery forward. It starts from four user-visible records, two per
application, one ordinary and one adversarial; names the facts that establish each; tests a candidate
representation against them; and states what every boundary must carry. Every mechanism proposed
after this document must name the fact it supplies or the contract it repairs, and the observation that
would show it wrong. A proposal that can only be described as "the model understands the criterion" or "the
atlas groups the claims" is not finished.

Sources: three read-only traces run 2026-10-07 against the gold, the source documents and the system's own
artifacts. Ward losses are read from the built crm-ward atlas. uv losses are read from the running build's
Phase-1 checkpoint (199 chapters) with placement and linking simulated from the code, because the uv
atlas had not been resolved yet; those are marked "simulated". Code citations are relative to
`ingest/crates/corpus-engine/src/enrichment/`.

## The four records

**Ward, ordinary: pasadena-d5.** Enron buys Pasadena's surplus gas during a steam-plant shutdown.

- *Evidence.* Message 4. (27 Mar, from Pasadena) quotes Kim's bids: "for your KRS supply for April 1 -10,
  we are a Gas Daily minus $.50 bid". It replies "Term sheet looks fine" (a different deal, d4), then "As
  for gas pricing, it seems we'd be better off just settling per the contract", with an ambiguous referent.
  Message 3. (28 Mar, from Pasadena) says "Kim and I have arranged for Enron to purchase the non-APEA gas
  during that period."
- *Facts.*
  - An offer by Enron (quoted, so it was uttered at the quoted time, not the carrier's Date).
  - A response whose referent is unresolved.
  - A membership in another deal inside the same message.
  - A reported agreement (won), stated by the counterparty and concluded outside the mail.
  - 4. bids for KRS gas for April 1-10; 3. reports an arrangement during a shutdown from March 30 to
    approximately April 11. Their identity needs adjudication; overlap is candidate evidence.
- *Identity.* Compare the object or leg, direction and delivery period. The April-August term sheet
  includes Pasadena buying gas and selling capacity (5.); direction alone does not separate d4 and d5.
  Neither price in 4. is an agreed price. No transaction identifier is stated.
- *Lost.*
  - The "won" sentence was emitted as an *event*, so no stage was read.
  - Four nomination line items became four invented deals.
  - The quoted bids were emitted as *relations* and dropped.
  - "Term sheet looks fine" became a stage claim with no stage.
  - The scorer credited d5 with d4's acceptance. Stage hits: 0 of 3.

**Ward, adversarial: smurfit-d5, d6, d7.** Dated physical-gas deals with one counterparty and one product.
23. distinguishes April, May-June and July delivery periods and carries their identifiers.

- *Evidence.*
  - Message 23. (17 Aug, internal) is a table of "all the deals we have with Smurfit", e.g. "684928
    3/21/2001 Executed 96057923 Physical Michael Grigsby 4/1/2001 4/30/2001".
  - Message 2.: "We are all set to roll April 2001 with Enron", reported by Summit, an agent acting for
    Smurfit-Stone.
  - Message 1. has an empty body and the Subject "roll it".
  - Message 21.'s "June deal" appears only in its Subject.
- *Facts.*
  - Per table row: existence, an identifier, direction, period, trade date, state Executed.
  - A completeness assertion that licenses elimination ("all the deals").
  - Agency: 17. explicitly says Summit represents Smurfit and also has other clients; 20. confirms clients.
    An address domain cannot decide which principal a particular message concerns.
  - Lineage: "roll" proposes succession; which instance it denotes remains a separate question.
  - Trade date, delivery interval, report date and Executed status are separate facts. The table does not
    give an execution timestamp; equating Trade Date with the transition time would be an inference.
- *Identity.* Period is the only distinguishing descriptor. The recipe's four-bucket `term` cannot separate
  these deals, so its necessary-field veto never bites.
- *Lost.*
  - 23. produced no claims.
  - 1. became a commitment.
  - 2. got the wrong span and the wrong stage.
  - 21. got the right stage but no counterparty, because every address on it is internal.
  - 14 deal records name Summit as counterparty and none names Smurfit, which is split across four company
    atoms.
  - The scorer matched none of the 16 Smurfit deals.

**uv, ordinary: case-842c51e579** (thread 1373, 8 documents).

- *Evidence.*
  - The issue asks for a pip-compatible `--no-cache-dir` alias.
  - A MEMBER replies "Yes we can add a hidden alias for this".
  - Label events follow: compatibility, good first issue.
  - "cross-referenced by pull request #1380", then a PR merge and "closed by pull request #1380".
- *Facts.*
  - Existence and reporter come from structure (an issue document and its author).
  - Confirmed comes from text plus authority (a MEMBER).
  - A PR cross-reference, merge and close-by-PR are structural assertions. A cross-reference alone does
    not warrant in_progress; a close explicitly by PR supports a resolved rule on tune (20/20).
  - Two documents are members with no state.
  - Structure can account for documents independently of whether it licenses any state update.
- *Lost (simulated).*
  - The "reported" claim's anchor dropped backticks and landed nowhere.
  - A "confirmed" claim was read off the reporter's own text. The model never sees who wrote what.
  - The comment chapter yielded no case claims. Its resolution was read into `events`, which nothing
    consumes.
  - Net: 1 of 8 documents placed, with the wrong state. Conditional B³ would call it perfect.

**uv, adversarial: case-b1148435c3** (four threads, 41 documents).

- *Evidence.*
  - A maintainer files #1630 listing #1619 and #1597.
  - "Closing in favor of https://github.com/astral-sh/uv/issues/1630", with a close in the same second.
  - A contributor writes "Duplicate of #1619 , #1630 and others"; a MEMBER answers "> Duplicate of … Agreed".
  - "Yep, folding into #1630", followed by "closed as not planned".
  - Two PRs (#1650 warns, #2633 implements) resolve it.
  - One comment raises a different problem (a spin-off).
- *Facts.*
  - Three cross-thread identity assertions, each with an asserter and an authority (a maintainer acting,
    or a maintainer endorsing a contributor).
  - 29 members with no state.
  - A spin-off inside a thread.
  - Duplicate closes recorded as "not planned", which are identity redirects, not declines.
  - A current state that only a fold over the *joined* case gets right. A per-thread fold calls 1437 and
    1712 declined.
- *Lost (simulated).*
  - Identity was read as state ("Duplicate of …" became `resolved`), or spanned two documents and was
    refused, or went to `entities_developed`.
  - A spin-off cannot open its own record. The thread field links it before any reading, at precision
    .905 against a .5 bar (`atlas/resolve_records.rs:496-500`).
  - 27 of 29 no-state members have no path in.
  - The scorer excludes the spin-off document because it belongs to two gold cases.


These four records were chosen to expose mechanisms. What they show about how often each loss occurs
across the corpora is not established here; every "almost all" below is a finding about these traces.

## The facts the tasks are made of

Ten kinds of fact appear across the four records. Not every kind appears in both applications, and the
table says where one was not seen.

| # | Fact | Ward instance | uv instance |
|---|---|---|---|
| 1 | **Existence**: a particular is asserted to exist (not merely mentioned) | a deal row in 23.; an RFP | an issue; a spin-off |
| 2 | **Membership**: this document concerns that particular; many-to-many; implies no change | 5. concerns four deals | "+1", label events: 75% of tune members (trace count) |
| 3 | **Identity relation**: same as / distinct from, with asserter, authority and polarity | deal #; a successor reference whose target still needs resolution | "Duplicate of #N", "in favor of", an endorsement; directed issue redirects are separate |
| 4 | **Descriptor**: identity-bearing (period, direction, product, identifier) or not (price, amount), each with its own status (offered, agreed) and validity | period separates d5/d6/d7 | kind, area, labels |
| 5 | **Act**: who does what to the particular, for whom | bid, accept, report of agreement, instruct | report, confirm, close, merge |
| 6 | **Transition**: an act's effect on state under the declared protocol, with the time it took effect where known | trade date against 23.'s date | merge time |
| 7 | **Role and agency**: the speaker's side; acting for a principal; authority | Summit for Smurfit | MEMBER against NONE |
| 8 | **Lineage**: successor, or under a master agreement | roll; trades under an Enfolio master | not seen. A tracker issue gathering reports is identity (fact 3), not lineage |
| 9 | **Completeness assertion**: "these are all", with a scope, an asserter and a time; it licenses elimination only within its scope and as of its time | "all the deals we have with Smurfit" | not seen |
| 10 | **Not applicable**: no supported assertion of the requested type | mass mail; nomination lines without a supported transaction link; a mixed document may still apply | 152 gold no-case documents in tune threads; fork references need interpretation |

On the criterion-driven production path, statements come from claims whose subject is the record type;
explicit memberships and entity descriptors do not enter that drive. `resolution_records.rs:214-222`
retires the entity mentions, `:307-312` supplies empty statement keys, and `:395-410` writes empty record
attributes. Other atlas paths have relations, events and provenance; those are inventory to reuse, not
evidence that this composition preserves the distinctions below.

## What the encodings require

The Ward encoding found that the candidate could not preserve about 25 distinctions; the uv encoding
found 19, including distinctions supplied in explanatory prose. These counts are diagnostic notes, not
independent failures or new primitives. The proposed representation stays provisional. Four questions
organize it: what was said, what it licenses, what its references denote, and what state follows.

**Source evidence is stable; decisions and application records are revisable views of it.** A failed
assignment leaves the evidence available. A changed identity decision invalidates its dependent licences
and state views. Replaying cached assertions under a changed recipe must not require another model read.

### Preserve the arguments and their scope

An assertion needs typed local references for its arguments. A reference can denote a person, organization,
transaction, issue, case, instrument, act, or earlier assertion; the recipe declares the types. It carries
its identifier namespace, descriptive evidence, or contextual expression. An issue handle is not a case
handle; a term sheet is not automatically its transaction; a series is not automatically its first instance.
Multiple references in one passage remain separate. Unknown targets retain their candidate descriptions.

Keep, independently, with evidence per field:
- **Speaker, asserter, actor, transmitter, audience and represented principal.** A forward preserves
  the original speaker. It does not manufacture endorsement. Agency has a scope and validity; Summit's
  other clients prevent its domain from determining the principal.
- **Force, polarity scope and modality.** A performed question can report a case. "We do not support X"
  negates capability, not confirmation of a problem. "Not a duplicate of A, but a duplicate of B" is two
  scoped propositions. A reader's interpretation is identified as its interpretation.
- **Descriptor, object or leg, offered/agreed status and validity.** Two values conflict only when the
  declared criterion makes them incompatible in the same scope. Alternative bids are not a contradiction.
- **Report time, utterance time, referred-to time and effective time where stated.** Retain raw clocks,
  zone and precision. A state report at T is a constraint at T, not necessarily a transition at T.
  Unknown effective time stays unknown; report ordering is a separately labelled view.

The structural and text readers share this boundary. Each document receives an outcome: read with its
assertions, nothing applicable to the requested types, could not judge, or not read, with reasons. A
mixed document can contain applicable and inapplicable passages. A batch may share context; every output
still identifies its source document, author, role, kind and evidence. Absence of an assertion is not proof
that the document contains none.

### Separate the relationships

- **Membership** relates a document or passage to a particular, many-to-many, independently of state.
- **Case equivalence** is symmetric and transitive only for licensed equivalence evidence. **Issue
  redirect** is directed. A redirect to two issues does not establish equivalence between its targets.
- **Endorsement** targets an assertion. A maintainer's "Agreed" can adopt a contributor's proposal under
  a declared rule; it does not change the proposal's original speaker or expand its unspecified targets.
- **Act identity** is distinct from assertion identity. A comment and two timeline echoes can attest one
  closing act. Retain their citations; correlated echoes are not three independent confirmations.
- **Retraction and correction** target an assertion or scoped set. Contrary evidence from another speaker
  remains alongside it. Reopening a case changes state while keeping its previous resolution history.
- **Succession, agency, under-agreement and completeness** retain their relation arguments and scope.
  Completeness permits elimination only after its scope and applicability have been established.

These relationships can use declared propositions and qualified arguments. The examples do not yet justify
a new closed observation taxonomy, a universal rule language, or a separate store.

### Decisions carry their dependencies

A licence names its rule, input assertions, required reference assignments, authority evidence and any
measured inference precision. Precision belongs to the live decision policy and its evaluated population.
An exact source field establishes its value, not the application consequence of that value. A decision
whose prerequisites are unresolved stays pending with those prerequisites; incompatible supported
decisions produce a conflict. Retraction removes support only from dependent conclusions.

Licensing and resolution are mutually dependent: resolving Summit's role can license a report, and that
report can help resolve its transaction. The four questions are logical responsibilities, not four rigid
one-way passes. Evaluate the finite dependency set against a versioned snapshot; surface unsupported
cycles and conflicts. Initially replay the affected snapshot after a change. General incremental truth
maintenance is earned only by an observed need.

An existence assertion permits a local particular handle. It does not prove global novelty. A sufficient
identifier may settle its identity; otherwise retain a provisional unlinked particular or unresolved
reference with that status. "No candidate matched" is no assertion that the inventory contains no match.
A distinct link or descriptor mismatch excludes a candidate, not every unseen particular. The boundary
must preserve an existing candidate's evidence even if its current membership decision is later undone.

### State is a projection under a protocol

Fold licensed acts and state-report constraints under the recipe's protocol. Keep effective-time intervals
and partial orders, offer/acceptance dependencies, reopen/rescind transitions, and commercial versus
documentary states distinct where declared. If admissible orderings yield different current states, report
the alternatives and their dependencies. Every visible field cites its assertions and the rules that
licensed it; pending evidence and conflicting conclusions remain inspectable.

## Boundary contracts

| Boundary | In | Out | Must survive | Error shows as |
|---|---|---|---|---|
| Source → document | files, records | one document per source item, its id and declared fields | identity, every declared field, per-document text | documents in source ≠ documents indexed |
| Document → assertions | document plus declared context | one outcome per document | polarity, voice, asserter and raw role, time said and time referred to, provenance per field | a document with no outcome; a citation not supporting its field; a negated assertion stored as asserted |
| Assertions → licences | assertions plus declared policy | effect, rule and evidence per assertion, or the reason it licenses nothing | the rule applied; measured precision for inferences | an effect with no rule; an assertion by a role the policy excludes changing state; a retracted assertion still in effect |
| Licensing ↔ resolution | assertions, local references, candidate assignments | supported, pending or conflicting conclusions | rule and input dependencies; original evidence | an unsupported merge; a redirect treated as equivalence; incompatible values in the same declared scope |
| Particulars → state | licensed acts and state-report constraints | state with its history and ordering uncertainty | protocol, act identity and time evidence | a report turned into a fabricated transition; echoes counted independently; chronology more precise than its evidence |
| State → record | particulars, descriptors, state | the user-visible record | every field cites its assertions and rules | a field with no citation; a citation naming another particular |

## Conditional ceilings, not measured causes

A loss has one of these causes:
1. the document never reached a reader;
2. the passage was not read, or was read into a form nothing consumes;
3. it was read but its value was not;
4. its handle or reference was absent or wrong;
5. the handle was resolved to the wrong particular;
6. the state was folded wrongly;
7. the scorer could not recognise a right answer.

The four traces found causes 2-4 almost everywhere, cause 6 never exercised, and cause 7 in both
applications. That describes these traces, not the corpora.

Substituting gold for one component at a time gives conditional ceilings. Each arm swaps more than one
component relative to the all-gold arm, so no arm isolates a single cause; together they bound them.

| Arm | Assertions | Licensing and resolution | Fold | Bounds |
|---|---|---|---|---|
| A | gold | gold | gold | the instrument (1.0 everywhere) |
| F | gold | gold | production | the fold alone |
| R | gold | production | production | licensing, resolution and fold, given perfect reading |
| D | production | production | production | the product |
| C | production, aligned to gold | gold, through the alignment | gold | reading, given perfect downstream |

Arm C's candidate alignments share document, proposition family, argument roles and compatible evidence
locations (the same field or span occurrence). Do not use the value being evaluated to choose the match.
Broad overlapping spans alone do not distinguish several claims in a table row. Freeze admissible matches
before a run; ambiguous alignments have reported bounds rather than a favourable pick. Gold assignment
must never fill a missing production value, subject argument or qualifier. Handle resolution can be
substituted only where its reference occurrence aligns. Arm C establishes a reading ceiling, not causation.

Measures, all coverage-aware (an unplaced gold member earns nothing; a placed no-case document counts
against):
- assertion recall and precision by kind and polarity;
- value accuracy;
- membership coverage;
- conditional clustering (standard B³ and CEAF-e), coverage-aware recovery, unresolved references;
- state accuracy, current and historical;
- whole-build cost (calls, tokens, elapsed time).

The implemented recovery measure is `recovery_b_cubed` in the shared `sovereign-eval` scorer. It is a
nonstandard extension: full predicted cluster sizes, precision averaged over all placed documents,
full gold cluster sizes, recall averaged over all gold members; missing members and placed noise earn zero.
For gold {a,b}/{c,d} with only a and c placed separately, conditional B³ is 1.0, coverage .5 and recovery
P/R/F1 = 1/.25/.4. It is separate from standard CoNLL. Empty denominators return zero.
The support adapter excludes counted ambiguous gold documents and other-fold documents, includes known
no-case placements, refuses unknown IDs, and counts dangling or multi-record subjects without choosing
a record by vote. Its default `case_state` membership remains an explicit proxy; `--membership-kind`
can read a declared state-free membership claim when the production reader supplies one.

## What the declarations must be able to say

The engine stays generic only if the recipe can state what the applications differ on:
- field-to-assertion mappings;
- the licensing rules above;
- reference grammars ("Duplicate of #N", issue URLs, deal numbers);
- the act vocabulary;
- the protocol with its reopen and rescind transitions;
- which descriptors carry identity;
- agency rules;
- what a thread implies (membership on uv; nothing on Ward, where the thread stamp is the Message-ID).

The generality test: the same machinery expresses both recipes, with their differences supplied by
declarations.

## Hypotheses this draft makes, each with its falsifier

- **Structural reading.** Of uv's 383 state-bearing tune documents, 131 are events and 74 are issues
  (trace counts), so up to 205 could get an assertion with no model.
  - Whether those assertions license the right *state* depends on rules not yet justified. "Closed by PR"
    implying resolved is plausible; a PR cross-reference implying in progress is not established.
  - *Falsified* if, under rules justified on tune, licensed states from structure alone are right on
    fewer than 90% of the documents those rules cover.
- **Text reading.** A document-accountable reader that sees author, role, date and kind recovers more
  membership than the current chapter sketch.
  - *Falsified* if arm D's membership coverage does not exceed the current reader's on the same documents,
    or its whole-build cost per document is not lower.
- **Resolution with handles.**
  - *Falsified* if arm R on the adversarial uv case cannot join its four threads from its licensed
    case-equivalence relations, invents global novelty from candidate absence, or loses the spin-off's
    local reference. Issue redirects alone are insufficient to require a union.
- **Fold.**
  - *Falsified* if arm F is not exact on the four records.

## Implementation order and the existing seams

1. **Honest measurement — implemented in this worktree.** `bench/crates/sovereign-eval/src/`
   `entity_resolution_recovery.rs` is the one recovery formula; `bench er-score` exposes it and
   `support/score.py` supplies the evaluated universe. Controls cover partial, empty, perfect, singleton,
   noise, split/merge, order, state-free membership, dangling subjects and invalid IDs. The instrument's
   improvement is not an extraction improvement. Ward's denominator/matching repairs remain separate.
2. **One production membership slice.** `resolution_documents.rs::SourceDocument` already preserves
   metadata; `CustomOntology::declared_phase1` and the genre compose/parse hooks are the narrow-reader seam
   (`pipeline/pipelines/configurable_atlas.rs`, `genre.rs`). Add document-accountable, declared reading and
   membership independent of state through that seam. Preserve author/voice/targets and field evidence in
   cached results before projecting subjects. `resolution_records.rs:307-312` must receive actual declared
   keys; `:214-222` must not delete the evidence being resolved. Gate on recovered membership with no
   state, preservation of distinct local references in a same-thread spin-off and two-deal message,
   and unchanged undeclared-corpus behavior. Check reading under gold assignment separately from the
   fixed production identity policy. This slice's actual production output decides whether to fund the rest.
3. **Supported identity, with replay.** Keep issue redirects, case equivalence and endorsements separate;
   retain unlinked handles. Put the necessary-field compatibility check on every join path, including
   keys and fields (`resolve_records.rs:477-526`, `resolve_records/select.rs::differs`). Pin a contradicted
   key, a bridge joining incompatible records and a multi-target redirect. Reuse reconciliation history
   where it can record the decision; prove its missing support before adding a sidecar or rule engine.
4. **State projection.** Existing derived folds select entity IDs (`resolution_derived.rs`); they are
   not the requested protocol fold over qualified acts. Extend the appropriate core seam after gold
   assigned assertions pin report-as-of, duplicate close, reopen, corrected offer and ambiguous time.
   Replay those fixtures under changed assignments and recipes without another model call.

The next production read compares existing and narrow readers on the same stratified tune documents,
model and identity policy. Measure reading with arm C and the whole composition with D; keep changes to
identity policy and batching as separate experiments. Use a fresh dependency-separated final holdout
after adoption. Existing folds are development evidence.

## The first production trial (v1) and what it fixed

The paired trial `reader-comparison-20261007-v1` (prereg, driver and frozen
binaries kept under `~/.svrnmesh/bench-corpora/reader-comparison-20261007-v1`)
produced no verdict, and its failures are the most useful data so far:

- Both legacy arms completed extraction and died in `enrich build` creating
  `atlas/derived_decisions.jsonl` in a directory that did not exist yet.
- uv narrow: 7 of 10 chapters read; 3 failed because a claim's quoted evidence
  was a paraphrase (an inserted period, a literal " ... " join, a `title: "…"`
  prefix) and chapter-fatal evidence validation discarded the whole batch,
  including claims that did verify.
- Ward narrow: 0 of 48 chapters. 41 failed at parse because the decoder offered
  the UNION of every claim kind's fields and every subject type's fields, so a
  `deal_membership` could carry `stage`, a `deal` subject could carry `email`,
  and a `stage_update` could carry `due`. 7 failed as `read` with no claims.

The fixes this minted, all structural (v2 prereg:
`reader-trial-prereg-v2.toml`):

- The decoder binds each claim kind to exactly its own fields and subject
  fields (the union is gone; the required list is the kind's own attribute set),
  and each outcome status to its own branch (`read` requires ≥1 claim; an
  abstention requires none and a reason). This is the decode-time enforcement
  of what the validator previously checked after the fact.
- A claim whose citation does not verify is refused BY NAME (`refused` on the
  outcome carrier), kept inspectable, and never projected; it no longer kills
  its whole chapter. A `read` whose every claim is refused becomes
  `could_not_judge`. A claim may cite the document's own declared identity
  fields by exact match, so a membership citation of the document itself is
  legal without weakening paraphrase refusal.
- `derived_decisions.jsonl` and `resolve_decisions.jsonl` create their parent
  directory; claim stamping prefers the accountable read's own document
  identity over fuzzy anchor search among a section's several documents.

v2's two surviving Ward parse losses (a local reference reused with two
different labels) were then fixed the same way: the first occurrence stands
and the later disagreeing claim is refused by name.

**v3 (2026-10-07), all four arms, 48/48 Ward chapters read: no adoption, and
the failure is now measured.** Against the pre-registered paired bar:

- uv: membership coverage .130 -> .913, recovery F1 .048 -> .520, conditional
  B3 1.0 on 3 placed -> .700 on 21, multi-record documents 1 -> 0 — but every
  one of the 8 known no-case timeline events was placed as a member, so the
  noise control regressed and the arm bar is NOT met.
- ward: deal recall .111 -> .074 and stage .000 -> .018 (from zero), while
  false stage claims fell 18 -> 8, false commitments 33 -> 12 and unmatched
  deal atoms 36 -> 26. The primary bar is NOT met.
- The Ward binding constraint is citation exactness, not subject binding: 89
  claims were refused, 78 of them "evidence is not an exact passage" (one
  inserted word, a dropped backtick, a "... " join), taking stage claims with
  them before RESOLVE ever sees them.

So the layer's first honest reading: document-accountable membership is a real
advance on uv, false-claim suppression is real on Ward, and neither example
meets its bar. The two named next experiments are ward citation normalization
inside the exactness check (without weakening paraphrase refusal) and uv
membership precision on status-only events (a membership citation must state
the case's problem, not merely a close or label event).

## Acceptance examples to encode in the core

These expectations test the supplied assertions and declarations. Source-to-assertion quality is measured
separately; passing a gold fixture does not establish the model's reading quality.

| Source item | Must survive | Decisive negative control |
|---|---|---|
| Pasadena 3. and 4. | reported purchase, individual bids, quoted speaker/audience/time, unresolved contract, separate term-sheet response | acceptance of the term sheet cannot become acceptance of the gas bid; nomination mentions cannot silently found four transactions |
| Smurfit 17., 20. and 23. | scoped agency, other clients, Deal # distinct from Contract #, delivery interval and Executed report separate from Trade Date | Summit cannot become Smurfit; Trade Date cannot become an asserted execution timestamp |
| uv 1947794150 | request-info assertion and case membership | no state transition; no fabricated state required for membership |
| uv 1949992696 / 1950290743 | contributor proposal, MEMBER's negated identity and separate redirect to #1495 | no union with #1526; contrary evidence is not deletion of the proposal |
| uv 1953020490 / 1953022747 | quoted proposal and explicit endorsement with target | quotation alone does not grant authority; unspecified "others" are not invented targets |
| uv 1953404069 / 1953404546 | caching hypothesis, scoped withdrawal, membership of both documents | unrelated assertions and historical state remain; implicit target selection is marked inferred |
| uv 1950993105 / ev-11838444610 | directed 1619→1630 redirect, one closing act with multiple citations | no resolved/declined transition merely from the duplicate close |
| uv 1962101893 / ev-11908914925 | reopen act with authority evidence and retained resolution history | missing event role is not defaulted to MEMBER; reopen does not retract the past merge |

The large source encodings exposed gold and policy gaps, including Smurfit periods and ambiguous
Pasadena identity. Preserve those ambiguities. Do not promote their invented licensing rules into gold.

## Open for agreement

- The candidate contract's layers, once the encoding has tested them.
- Recipes declaring reference grammars, field mappings and licensing rules.
- Whether the text reader replaces the seven-facet sketch for recipes that declare types, or runs beside it.
- Gold in assertion form: who drafts it, and on how many documents.
- A fresh, dependency-separated final holdout for both applications. The current folds are development
  evidence: 15 of Ward's 69 holdout deals also sit in dev.
