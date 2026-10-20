# Ship bars for the ontology layer (pre-registered 2026-10-20, before any read or authoring below)

**The claim.** Raw input plus a declared ontology plus a patterned method yields typed, cited records. Most SaaS
applications are such records (a CRM's deals, a tracker's cases, a register of rules in force). The next use case
is expressible in the same language and runs through the same default commands.

**Engine and reads.** DeepSeek Flash through the daemon at HEAD, every question asked fresh (`ASKER=daemon
RESOLVE_ASKER=daemon`). One read per corpus (operator 2026-10-20: a scrappy pass, not two reads per arm), so each
number is reported beside its known single-read noise (Ward about three deals, REVIEW_BRIEF.md). Approved spend:
about 1M answer tokens plus twelve authoring sessions.

## The bars

**S1, deals in mail.** The five worlds and ward-story, one fresh read each (arm `ship-h1`). Passes if the worlds
serve at least 68/93 and attach at least 237/328 (the one-shot under c138d09b1), and ward-story serves at least
6/14 (the one-shot's 8, minus 2).

**S2, facts in a novel.** saltgrass-ledger, one fresh read: at least 23.5/42 (the one-shot mean of 25.5, minus 2).

**S3, a tracker's cases.** uv-support's fixed tune third, read by the window reader for the first time (`ship-uv`),
scored by `converge-s2/uv_served.py` unchanged. Passes if at least 15 of 44 cases serve the right current state,
beating the passes reader's 14/44 (CLOSING_BRIEF.md). No one-shot exists for uv, so best-in-class is could-not-judge
here. This bar only asks that the shipped reader beat the one it replaced.

**S4, a register of rules in force.** A register built from this repo's own rules (canon and operator directions
with their dates), under `register/`. Its documents, gold and scorer are committed before any read. One one-shot
read and one fresh system read; passes at the one-shot minus 2 on its scorer's main count.

**S5, the n+1 census.** Twelve owner descriptions (`census/descriptions/`), written before any authoring, from
outside the tuning examples (mail CRM, issue support, novels):
- recruiting, contracts, policies, purchasing, incidents, tasks;
- spare parts, lettings, grants, vans, expenses, a coliving house.

Each gets one blind author session under `blind-author/protocol.md`, with today's docs and today's validator. Each
numbered owner need is judged one of three ways:
- **runs:** declared with primitives that the reader, RESOLVE or a derivation executes, so a typed query can answer it;
- **inert:** declared, but nothing executes it (a validator warning, a key the parser drops, a documented-only facet);
- **inexpressible:** dropped, or held as free text where the owner needs a typed value, a link or a date.

Every miss is tagged with the primitive it lacks.
- **E1:** every use case's record types are declarable with running primitives, 12/12.
- **E2:** after building the primitives that two or more use cases lack, at least 90% of all owner needs run.
- **E3, the n+1 bar.** In seed-0 order, the last four use cases add no gap tag the first eight did not already show,
  judged on the first-pass recipes before any fix. Seed-0 order: contracts, vans, grants, tasks, expenses,
  policies, purchasing, lettings, incidents, recruiting, coliving, spare parts.

A gap that appears in one use case only is a stated limit of the claim, not a blocker.

**S6, the user's path.** The CLI journeys for mail, issues and news (`svrn/docs/cli-contract.toml`) pass on the
live daemon. A test asserts each fixture's `expected.toml` (record ids, values, evidence).

## Decision

Ship when S1-S6 pass. A failed bar is reported with its numbers. It blocks the claim for its shape, unless the
operator narrows the claim to exclude that shape.

## Results (2026-10-20, one read each, DeepSeek Flash)

**S1 passes** (`ship-h1`, head 84c210f6e, `runs/loop/ship/headline.log`): the worlds serve 73/93 (bar 68) and
attach 301/328 (bar 237); ward-story serves 7/14 (bar 6).

**S2 passes**: saltgrass-ledger 30/42 (bar 23.5).

**S3 passes** (`ship-uv`, `runs/loop/ship/uv.log`): 27/44 cases at the right current state (bar 15, passes reader
14). 12 serve a wrong state, mostly reported/resolved/declined swaps; 5 match no record.

**S4: 19/35 against the one-shot's 19/35 on the amended scorer; fails on the frozen one, 15 against 19** (bar 17;
`ship-reg`, head a0585e49b). The frozen `rule_of` ranked a record's own name below how often it names an id, and
a successor's approval quotes "superseding <old id>", so the records of can-92092826490e and can-3a33d3749895
(each named by its own id, each folding to in force) were scored as their predecessors: two false splits and two
false no-records. With the name first, only those four system rows move and the one-shot does not change
(`score.amended.json` beside each frozen score). The misses left:
- 12 retracted rules, missed by both readers. One message withdraws "the twelve superseded agent:ralph
  proposals" and names no id; the ids are in two earlier messages. A claim about a described set is a stated limit.
- 3 ledger entries read with no `action` ("A -> B", "superseding them is the holder's act"), so no rule matches.
  The one-shot gets all three wrong too.
- 1 revert that names no target message: three revert rules match and the fold stays pending, though the rule's
  prior state (proposed) admits only one of them.
- 1 rule neither reader records.

## Operator rulings (2026-10-20, after the S1-S4 results)

- **S4 stands on the amended scorer: passes, 19/35 against the one-shot's 19/35.** The frozen 15 is reported beside
  it. The register claim covers rules named by id. A withdrawal by description ("the twelve superseded
  proposals") is outside the claim.
- **Frozen fixture truths change only through a blind audit.** An assertion holds what the recipe's declaration and
  the source documents alone determine. An agent that sees no run output audits every assertion in the issues,
  news and mailraw fixtures (passing and failing alike) and returns keep, amend or optional with a reason.
  Its amendments land in one commit before the next journey. What fails after that is the reader's.
- **A particular's field holds one value per stating document**, so `agree` and dated folds see every reading.
  The window answer is reshaped; it is not a prompt rule. It is in the final read.
- **One final read, approved (about 1.5-2.5M output tokens):** one read per corpus as above, after every change
  that touches reading or RESOLVE has landed and passed a one-window smoke test. Ward is reported beside its
  noise. No second read is bought.
- **The alias stamp is committed before the final read**, so the answer store it fills names the model that
  served each answer.

## S5 result, first pass (2026-10-20, `census/judgments.json`, one judge, static reading of code and validator)

75 owner needs: 53 run (71%), 13 inert, 9 inexpressible.
- **E1 fails, 8/12.** In 02, 05, 06 and 07 a type that a need requires gets no records. Each of those four shows
  `ref-target-unrecorded`: a reference to a type the reader makes no record of (e.g. `of = "person"` with no
  person type declared) is removed at `statement_refs.rs:137-142`, and validate says nothing about it.
- **E3 fails.** In seed-0 order the last four add one tag, `fold-needs-document-date` (05-incidents only). A fold's
  clock comes only from `change.document.date`, and `latest`/`earliest` answer `Unordered` even when every
  reading carries one value (`resolution_derived.rs:684`).
- **E2, before building: 53/75.** The tags two or more use cases show, with the needs each holds:
  ref-target-unrecorded 8 (4 use cases), totals 5 (4), computed-value 2, value-compare 2, subject-field-unheld 2.
  Built, they hold 19 needs: a ceiling of 72/75 against the bar of 68. Single-use-case limits: as-of-state (02),
  linked-count-filter (04), fold-needs-document-date (05).
