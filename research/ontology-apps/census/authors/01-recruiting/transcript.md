# Recruiting inbox: authoring transcript

The partner was not available. Each answer is sourced as `description` (quoted from
DESCRIPTION.md) or `assumption` (general knowledge of recruiting at a small company,
or a choice the engine forced, said so).

Reference reads: six templates scaffolded into this directory with
`sovereign recipe new --ontology <name> --out template-<name>.toml` (product-support,
contracts, due-diligence, engineering-org, governance, patient-community). None
reads email headers or declares a protocol fold, so the recipe is written from
SCHEMA.md / v1.md rather than adapted from one.

## 1. Shape

**Q.** What is this material about, and what would you want to know about each kind
of thing? Is any of them a kind of another, or a part something plays? Do you already
have any of it in a table?

**A.**

- **The material** is a recruiter's inbox plus the interview notes pasted into it.
  Source: description, "The material is my recruiting inbox (candidates, agencies,
  hiring managers) and the interview notes interviewers paste into email after each
  interview."
- **Candidate.** A person being considered. I want their email and phone, which are
  what identify them. Source: description for the kind ("candidates"); assumption for
  the attributes.
- **Opening.** The role being hired for. I want its title, team, hiring manager and
  whether it is open, on hold, filled or closed. Source: description for the kind
  ("For each opening"); assumption for the attributes and the status values.
- **Application.** One candidate for one opening, which is the spreadsheet row. I want:
  - its candidate and opening;
  - how it came in (channel), and the referrer or agency;
  - its stage;
  - where its offer stands.

  Source: description, "one row per candidate per opening", "A candidate who applies
  to two openings is one person with two applications", and questions 1, 2 and 4.
- **Agency.** Source: description ("agencies"). I declared it as `organization`, a firm
  known by its email domain and read from the headers with no model call. The
  description is the source for "agencies"; reading them from the headers is an
  assumption. An agency's recruiters email from the agency's own domain, so the
  domain is the firm. Free-mail domains are excluded (`@bundled:mailbox_providers`).
- **People who work here** (hiring managers, interviewers, referrers). Source:
  description ("hiring managers", "interviewers"). I first declared them as
  `employee`, but validate run 1 showed that type would get no record at all. Every
  one of them sends or receives mail in this inbox, so they are now `correspondent`:
  one record per address in from/to/cc. The correspondent links to its
  `organization` by domain, which is how an agency recruiter reaches their agency.
  Source: assumption. The type is named for what the headers really hold, so it
  covers candidates and agency recruiters as well as staff.
- **Kind of / part played.** A candidate is a part a person plays
  (`role_of = "person"`). Source: assumption.
- **A table.** Yes: "Today I keep a spreadsheet by hand: one row per candidate per
  opening." (description). I did not declare it as a `source`. It is the hand-kept
  record this corpus replaces. SCHEMA.md also says "no stage reads a table source
  yet", and a type a table source holds "is not looked for in the text". Declaring it
  would make applications invisible to the reader and give nothing back. Source:
  description for the table; assumption plus the schema for the choice.

## 2. Assertion

**Q.** What do the sources say about those things: stating, requiring, deciding,
asking? About what? What does each statement carry? How do you tell strong evidence
from weak? Who is speaking, and are they part of the subject? What must it never do?

**A.** These are the statement kinds, each about one thing.

| Statement kind | Force | About | What it carries | Source |
|---|---|---|---|---|
| `submission` | declaration | application | An agency submitting a CV, an employee referring someone, a candidate applying, or our own outreach. The application's channel, referrer and agency are read from it. | description, Q2 "Who referred or sourced each candidate (an employee, an agency, or the candidate applied directly)?"; "sourced" as a fourth channel is an assumption |
| `stage_move` | declaration | application | The stage moved to (the seven stages), whether it is an ordinary move, a reopening or a correction, the stage a correction undoes, whose say-so (`decider_role`, `decided_by`), and an effective date if one is named | description, Q1 stage list and Q5 "When a candidate moved stage, and on whose say-so"; the reopen/correction fields are an assumption |
| `interview_feedback` | assertive | application | The interviewer, the interview date, the round (phone screen, technical, onsite, hiring manager, culture, reference) and the recommendation (strong hire, hire, no hire, strong no hire) | description, Q3 "Who interviewed a candidate, for which opening, on what date, and what each interviewer recommended?"; the round and recommendation scales are an assumption (the common scorecard shape) |
| `offer_terms` | commissive | application | Extended or revised, base salary, currency, start date, and the date the offer was made | description, Q4 "What we offered (base salary, currency, start date) and whether the offer was ... revised" |
| `offer_response` | declaration | application | Accepted, declined or countered, and when | description, Q4 "accepted, declined or revised"; "countered" is an assumption |
| `agency_fee` | directive (request) | application | An agency's fee notice or invoice: amount, currency, percent | description, Q6 "Which agencies are owed a fee for a hire, and how much"; that agencies invoice by email is an assumption |
| `fee_terms` | commissive | organization | The agreed rate (percent of first-year base or a flat fee), currency and guarantee period | assumption: an agency is owed a fee before it invoices, so "how much" needs the rate as well as the invoice |
| `candidate_profile` | assertive | candidate | Current title, current employer, location, salary expectation, currency, notice period | assumption, and required by the engine (see below) |
| `opening_update` | declaration | opening | Opening, describing, pausing, filling or closing a role. The opening's title, team, hiring manager and status are read from it. | assumption, and required by the engine (see below) |

The last two kinds are not on the partner's list. Validate run 1 showed that the
document reader makes no record of a type that no statement kind is about. Without
them there would be no candidate records, and question 7 would fail. Both are real
kinds of email in a recruiting inbox: an agency's or referrer's introduction of who
the candidate is, and a hiring manager opening or closing a role.

- **Strong vs weak.** `interview_feedback` grades are `first-hand` (the interviewer's
  own pasted note) then `relayed` (someone else reporting what the interviewer
  thought). Source: description for "interview notes interviewers paste into email";
  the ranking is an assumption.
- **Who speaks.** The recruiter, hiring managers, interviewers, candidates and
  agencies (description, "candidates, agencies, hiring managers"). I drafted a
  `voices` block, but validate run 1 said voices are "applied by the general
  extractor only; the document reader attributes no statement to a speaker". So I
  removed it. Who decided a move is carried as a field (`decider_role`,
  `decided_by`) instead.
- **Must never.** (1) Infer or record a candidate's age, ethnicity, religion,
  health, disability, pregnancy or family status. (2) Present one interviewer's
  recommendation as the hiring decision. Source: assumption (employment law and
  interview practice).

## 3. Identity

**Q.** How do you know two mentions are the same thing? Is there an ID?

**A.**

- **Candidate.** The same email address. Without one, the same full name together
  with the same phone, current employer or CV. A person who applies to two openings
  is one candidate. Source: description, Q7 "one person with two applications", for
  the rule; the keys are an assumption. Agencies often withhold a candidate's
  contact details to protect their fee, so email cannot be the only key. Declared as
  `identity = ["email"]` plus an `identity_criterion`, which RESOLVE decides.
- **Application.** One candidate for one opening, however many emails mention it.
  The same person for a different opening is a different application. Source:
  description, "one row per candidate per opening" and Q7. Declared as an
  `identity_criterion`, and the record holds its candidate and opening `by = "agree"`.
- **Opening.** The same title for the same team, however it is abbreviated. A second
  hire into the same role, opened separately, is a different opening. I assumed no
  requisition IDs (a 60-person company rarely runs them). Source: assumption.
- **Correspondent.** The email address (strict). **Organization:** the email domain
  (strict), with free-mail domains excluded. Source: assumption.

## 4. Change

**Q.** When does a later statement replace an earlier one, and from when? Does
anything move through stages? What are the stages, which statements move it, who may
make each move, does a move take effect when reported or at a time it names, and how
is a mistaken move corrected or a finished thing reopened? Which fields of each
document give its date and the conversation it belongs to?

**A.**

- **Supersession.** A later `offer_terms` replaces the earlier one for the same
  application, by document date. Source: description, "revised". A later `fee_terms`
  replaces the earlier one for the same agency. Source: assumption.
- **Stages.** An application moves through applied, screen, onsite, offer, hired,
  rejected, withdrew. Source: description, Q1. Declared as `application.stage`, a
  protocol fold over the statements about the application, with
  `initial = "applied"`. One rule per move:
  - **Ordinary moves.** `stage_move` to each stage. A move counts only out of an open
    stage (applied, screen, onsite, offer), so a rejected, withdrawn or hired
    application stays put unless reopened or corrected.
  - **Offer moves.** An extended `offer_terms` moves to `offer`. An accepted
    `offer_response` moves to `hired`. A declined `offer_response` moves to
    `withdrew`. That last mapping is an assumption: a declined offer is the
    candidate's exit, and the seven stages have no separate "declined".
  - **Who may.** Only the candidate's say-so moves an application to `withdrew`
    (`when` includes `decider_role = "candidate"`); rejected is our decision.
    Otherwise anyone, with the decider recorded. Source: description, Q5 "on whose
    say-so"; the withdrew/rejected split is an assumption.
  - **Reported vs effective.** A move takes effect when it is reported (the email's
    date). `effective_on` records a time the email names ("she withdrew last
    Tuesday") without reordering reports. Source: assumption.
  - **Corrected.** An explicit correction ("moved her to rejected by mistake") names
    the stage it undoes (`corrects = "undoes"`); the protocol identity is the stage
    moved to. **Reopened:** an explicit reopen moves a rejected or withdrawn
    application back to applied, screen or onsite. A revised offer after a decline
    reopens the application at `offer`. Hired can still move to withdrew, because a
    candidate can renege before starting. Source: assumption.
- **Offer status.** `application.offer_status` is a second protocol fold: extended,
  revised, accepted, declined. Source: description, Q4.
- **Opening status.** The latest reading of open / on hold / filled / closed, kept as
  `by = "latest"` rather than a protocol fold, since nobody asked for its history.
  Source: assumption.
- **Document fields.** `date` gives the date, `thread_id` the conversation,
  `message_id` the id, and `from` the author. Source: assumption from SCHEMA.md's
  list for `type = "email"`, which emits these fields. There is no material here to
  run `validate --corpus` on, so they are not confirmed against real documents.

## 5. Derivation

**Q.** What should it notice that no single document says (contradictions, patterns,
larger structures)? What looks like a contradiction but isn't?

**A.**

- **Agency fees owed.** An agency is owed a fee on every application whose stage is
  hired and whose channel is agency. The amount comes from that agency's invoice
  (`agency_fee`), or from its rate (`fee_terms`) times the offer's base salary.
  Source: description, Q6. I tried to declare this as a set feeding a derived
  attribute and the engine refused it (validate runs 3 to 6). The answer is still
  held, as `stage`, `channel` and `agency` on each application record.
- **Ownership disputes.** These are a tension between `submission` and `agency_fee`
  statements about the same application (`same = ["subject"]`, labelled
  "ownership dispute"). Examples: two agencies submitting one candidate for one
  opening, or an agency invoicing for someone who applied directly first. The
  application's channel, referrer and agency are kept `by = "earliest"`, because the
  first introduction usually decides ownership under agency terms. Source:
  assumption (common agency terms).
- **Not conflicts.**
  - The same agency following up on, or resubmitting, its own candidate.
  - A referral and the referred person's own careers-page application.
  - An agency's invoice for a candidate it submitted first.

  Source: assumption.
- **Also not conflicts.** Two interviewers disagreeing is a split panel, not a
  contradiction, so feedback is kept out of `tension`. A revised offer is
  supersession, not a conflict. Source: assumption.

## 6. Questions

**Q.** Once it is read, what will you ask of it?

**A.** The partner's seven (description), each walked to where its answer lands:

1. *"For each opening, which candidates are in play and at what stage."*
   `application` records with `opening`, `candidate` and the derived `stage`.
   "In play" means a stage of applied, screen, onsite or offer.
2. *"Who referred or sourced each candidate."* `application.channel`, `referrer` and
   `agency`, plus the `submission` statements they were read from.
3. *"Who interviewed a candidate, for which opening, on what date, and what each
   interviewer recommended."* `interview_feedback`: `interviewer`, `interview_date`,
   `recommendation`, and its subject application's candidate and opening. Validate
   derives "aggregate interview_feedback by interviewer".
4. *"What we offered ... and whether the offer was accepted, declined or revised."*
   `offer_terms` (`base_salary`, `currency`, `start_date`; the latest one is current),
   `offer_response`, and the derived `application.offer_status`.
5. *"When a candidate moved stage, and on whose say-so."* `stage_move` with
   `document_date`, `effective_on`, `decider_role` and `decided_by`. Validate derives
   "aggregate stage_move by decided_by".
6. *"Which agencies are owed a fee for a hire, and how much."* Applications with
   stage hired and channel agency, their `agency` (an organization), and
   `agency_fee.amount` or `fee_terms.fee_percent` times `offer_terms.base_salary`.
7. *"A candidate who applies to two openings is one person with two applications."*
   This is a constraint, not a question. It is held by the separate identity rules
   for `candidate` and `application`.

None of the seven points at a kind of thing the declaration left out.

## Follow-ups the schema needs

- **Extractor.** `email` (RFC 5322 / MIME). The material is an inbox, and the
  interview notes are pasted into email bodies (description). Assumption: the inbox
  is exported as a maildir or a folder of `.eml` files. The field reference describes
  the email extractor as walking "maildir layout, raw `.eml` files"; an mbox export
  would need converting first.
- **Acquire.** `local_file`, `path = "SOURCE_PATH"` (from the brief).
- **Chunk.** `paragraph`, 2000 / 200.
- **Distribution.** `scope = "local"`, `mesh_sharing = false`,
  `license = "private"`. Source: assumption: candidate personal data and salaries
  must not leave the host.

## Validate runs

1. **Draft 1.** It declared `employee` instead of `correspondent`/`organization`,
   with no `candidate_profile` or `opening_update`, and with a `voices` block.
   **Passed, with two warnings:**
   > ⚠ the document reader reads statement kinds, the types they are about and every
   > type RESOLVE decides, so it makes no record of `candidate` (entity), `employee`
   > (entity), `opening` (entity): no statement kind is about them, RESOLVE decides
   > none of them and no source supplies them.
   >
   > ⚠ `voices` (attributed_to, self) are applied by the general extractor only; the
   > document reader attributes no statement to a speaker.

   Fix: people and firms in the headers became the metadata-sourced
   `correspondent` and `organization`. `candidate` and `opening` got identity
   criteria and a statement kind each (`candidate_profile`, `opening_update`).
   `voices` was removed. I also added a set `agency_hires`
   (`where = { stage = { Is = "hired" }, channel = { Is = "agency" } }`).
2. **Failed:**
   > error: Recipe error: data did not match any variant of untagged enum Condition
   > in `sets.where.channel`

   Fix: `Condition` is untagged, so `Is` is a bare value:
   `where = { stage = "hired", channel = "agency" }`.
3. **Passed.** Every warning from run 1 was gone. One note:
   > derived: `agency_hires` is declared and no derived attribute uses it

   Fix attempt: `organization.owed_hires`, derived by a fold `by = "all"` from
   `^agency[agency_hires]`.
4. **Failed:**
   > derived: `organization.owed_hires` is derived before RESOLVE but reads `agency`,
   > which only RESOLVE's records carry

   Fix attempt: move it to the application, as `application.fee_owed_to`, derived
   from `[agency_hires].agency`.
5. **Failed:**
   > derived: fold `fee_owed_to`: `[agency_hires].agency`: `.` is not part of a path

   Fix attempt: `[agency_hires]agency`.
6. **Failed:**
   > derived: fold `fee_owed_to`: `[agency_hires]agency`: a step cannot start with `[`

   A filter only follows a step, and there is no step that filters the application
   itself. I stopped there and removed the set, the fold and `fee_owed_to`, leaving
   a comment in the recipe saying why. The answer to question 6 remains on the
   application records.
7. **Passed, no warnings.**
8. **Removed `agency` from `stage_move.decider_role`.** An agency relaying a
   withdrawal relays the candidate's say-so, and the value invited a misread that
   blocks the `to_withdrew` rule. **Passed, no warnings (final).**

## Derived facets read against the answers (final run)

- **`clock: document_date`.** Correct: emails are dated, and revised offers and fee
  terms supersede on date.
- **Tension selector over `submission` and `agency_fee`, embedding top-k.** Correct:
  these are the kinds an ownership dispute is between.
- **`organization ← from, to, cc (domain)`, strict on domain, 1 value excluded.**
  Correct. The excluded value is the bundled free-mail list.
- **`correspondent ← from, to, cc (address)`, strict on email.** Correct.
- **`candidate`, `opening` and `application` are decided by RESOLVE under their
  identity criteria.** Correct. For `candidate` the facet names only the criterion.
  The declared `identity = ["email"]` is kept, because v1.md says the criterion is
  used "whenever no identity key settles the question". The criterion text names
  email first, so the two agree.
- **Question shapes: enumerate the five entity types; aggregate `stage_move by
  decided_by` and `interview_feedback by interviewer`.** These match questions 5 and
  3. Questions 1, 2 and 6 are enumerations of `application` with its attributes.
- **`application.stage` and `offer_status` come from protocol folds after RESOLVE.**
  Correct.
- **`lifecycle: off` on both folds.** Accepted. The lifecycle kinds are offered only
  to a fold whose rules name one claim kind, and both folds name several. Moving a
  candidate from one opening to another is therefore not modelled as "replaced". The
  old application keeps its last stage unless someone states a move on it.
- **Document fields: `date`, `thread_id` and `message_id` are stamped, and `from` is
  shown to the reader.** These match the email extractor's documented fields. They
  are unconfirmed against real mail (no `--corpus` run was possible).

No facet contradicted an answer, so the final run needed no further declaration fix.

## Summary

**Types declared (14), and each one's identity**

| Type | Kind | Identity |
|---|---|---|
| `organization` | entity, read from header domains | domain, strict; free-mail excluded |
| `correspondent` | entity, read from header addresses | email address, strict; linked to its organization by domain |
| `candidate` | entity, role of a person | RESOLVE: same email, else same name plus phone, employer or CV; one person across openings |
| `opening` | entity | RESOLVE: same title for the same team, however abbreviated |
| `application` | entity | RESOLVE: one candidate for one opening. Holds candidate and opening (agree), channel, referrer and agency (earliest), and the derived `stage` and `offer_status` |
| `submission`, `stage_move`, `interview_feedback`, `offer_terms`, `offer_response`, `agency_fee` | claims | each about an application |
| `candidate_profile` | claim | about a candidate |
| `opening_update` | claim | about an opening |
| `fee_terms` | claim | about an organization |

**Assumptions I am least sure of**

1. **The extra statement kinds.** `candidate_profile` and `opening_update` were added
   because the engine needs some statement about a type to make its record. A
   candidate or opening that no such statement mentions gets no record, and
   applications pointing at it may link nothing. This is the biggest risk to
   questions 1 and 7.
2. **The offer-to-stage mapping.** Declined offer becomes `withdrew`, accepted offer
   becomes `hired`. Also, only the candidate's say-so counts for `withdrew`. If
   decider_role is not read, a withdrawal stays pending.
3. **Who sourced a candidate.** It is "earliest introduction wins" for channel,
   referrer and agency. The partner's agency terms may say otherwise, for example an
   ownership window, or "candidate already in our system".
4. **Agency refs.** Agencies are read as organizations by email domain. The agency
   reference is picked among the organizations in the email's headers, and our own
   company's domain is always among them. Nothing in the declaration excludes it,
   because the company's domain is not known here.
5. **Document fields and export format.** The field names come from the extractor
   reference, not a `--corpus` run. The inbox is assumed to be maildir or `.eml`, not
   mbox.
6. **The scales.** The round and recommendation scales on `interview_feedback`, and
   the `first-hand` over `relayed` grading, are my guesses at a common scorecard.
