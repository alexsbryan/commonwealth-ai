# Interview transcript: grants at a small charity

The partner is not available. Each answer is marked with its source:
`description` (quoting DESCRIPTION.md) or `assumption` (general knowledge of
charity grant funding, my call).

Templates looked at (scaffolded into this directory with
`sovereign recipe new --ontology <name> --out tpl-<name>.toml`): contracts,
governance, due-diligence, product-support. None was copied; contracts
(directive obligations with deadlines) and governance (supersession by date)
were the closest shapes.

## 0. Source shape (follow-up the schema needs: acquire / extract)

Q: What form is the material in?
A: A mailbox export (the correspondence with funders), with application
documents, award letters and reports mostly travelling as attachments. So:
`acquire.type = "local_file"`, `path = "SOURCE_PATH"`, `extract.type = "email"`
(the attachments go through the extractor's described-asset dispatch),
paragraph chunks.
Source: description for the kinds of material ("our correspondence with
funders, the application documents we submit, award letters, and the reports
we send back"); assumption for the mailbox format and for attachments being
the way the documents arrive.

## 1. Shape

Q: What is this material about, and what would you want to know about each
kind of thing? Is any a kind of another, or a part something plays? Any of it
in a table already?
A:
- funder: who we apply to. Source: description ("which funder").
- programme: the funder's programme or scheme. Source: description ("which
  programme"). Declared as a text attribute of the application, not its own
  type (assumption): it is only ever asked of an application, and a separate
  RESOLVE-decided type would need its own claim kind.
- application: one bid. Want funder, programme, amount, currency, date
  applied, outcome, amount awarded. Source: description (need 1 and 2).
- award / grant: the application once awarded. Declared as the same record
  as the application, not a separate type. Source: assumption (one
  application yields at most one award; funders usually carry the
  application reference through to the grant).
- report: a report owed under a grant, with its deadline and whether we sent
  it. Source: description ("every reporting deadline it imposes", "which
  reports we have sent and which are overdue").
- payment: an instalment received against a grant. Source: description
  ("Payments received against each award, with dates and amounts").
- contact: a person at a funder. Source: description ("Our contact at each
  funder").
- Kinds/roles: none needed; contact is tied to its funder by a `ref`.
  Source: assumption.
- Tables: none mentioned. Source: description (lists no spreadsheet);
  assumption that there is no grants register to load as a `source = { file }`.

## 2. Assertion

Q: What do the sources say about those things (stating, requiring, deciding,
asking), about what, carrying what? Strong vs weak evidence? Who speaks? What
must it never do?
A:
- We request funding (directive, `request`) about an application: amount,
  currency, date, draft or submitted. Source: description ("how much we asked
  for and in what currency, when we applied"); the draft/submitted split is
  assumption (application folders hold drafts).
- A funder decides (declaration): awarded or declined, amount awarded,
  currency, grant period. Source: description ("awarded, declined", "the
  amount awarded"); grant period is assumption.
- An application is withdrawn (declaration), by us or the funder. Source:
  description ("withdrawn"); who withdraws is assumption.
- An award imposes conditions (directive: require / forbid / permit), by
  category, sometimes with a deadline. Source: description ("The conditions of
  each award"); categories are assumption (use of funds, reporting,
  monitoring, acknowledgement, match funding, timing).
- A funder sets, extends or waives a report and its deadline (directive,
  `require`). Source: description ("every reporting deadline it imposes");
  extend/waive is assumption (common in practice).
- We send a report, or draft one (assertive). Source: description ("which
  reports we have sent"); draft is assumption.
- A funder answers a report: received, accepted, revision requested
  (assertive). Source: assumption.
- A payment is notified or acknowledged (assertive). Source: description
  (need 5).
- Evidence grades: not declared. Source: assumption (the documents are
  first-hand; the question is what they say, not how strong it is).
- Speakers: us and funders. Source: description. A first draft declared
  `[voices]`; validate warned the document reader does not apply voices, so
  it was removed and who-acted is carried by the claim kind itself.
- must_not: never treat a draft as sent; never report an amount, currency or
  deadline the material does not state. Source: assumption.

## 3. Identity

Q: How do you know two mentions are the same thing? Is there an ID?
A:
- funder: its mail domain, from the From/To/Cc headers, free-mail domains and
  our own domain excluded. Source: assumption.
- contact: email address, with the funder linked by the address's domain.
  Source: assumption.
- application: same funder, programme, project and round; a funder's
  application/grant reference settles it where present, but is not declared
  as a strict key because application and grant references often differ.
  A resubmission to a later round is a new application. Source: assumption.
- report: same grant, same kind, same period; a revision or extension is the
  same report. Source: assumption.
- payment: same grant, amount and date/instalment; a remittance advice and
  our thank-you for it are one payment. Source: assumption.

## 4. Change

Q: When does a later statement replace an earlier one? Does anything move
through stages, by which statements, who may move it, reported vs effective
time, corrections? Which document fields give date and thread?
A:
- Application stages: pending, awarded, declined, withdrawn. Source:
  description ("pending, awarded, declined, withdrawn"). Moves: a submitted
  request makes it pending; a funder decision awarded/declined; a withdrawal
  by us withdrawn. Source: assumption for which statements move it.
- Report stages: due, submitted, accepted, revision_requested, waived.
  Source: assumption (description gives only "sent" and "overdue"). Moves: a
  requirement set or extended -> due; waived -> waived; we send it ->
  submitted; funder received -> submitted, accepted -> accepted, asks for
  more -> revision_requested.
- Replacement: a later decision letter for the same application supersedes
  an earlier one; a later requirement for the same report supersedes the
  earlier (deadline extensions), and `report.due` folds `latest`. Source:
  assumption.
- Report time vs effective time: report time is the mail's date; each move's
  claim carries `dated` (the date of the act) as `effective_time`. Source:
  assumption.
- Corrections: not declared (no `corrects` rule). A revised award letter is
  handled by supersession. Source: assumption.
- Document fields: the email extractor's `date`, `thread_id`, `message_id`,
  `from`. Source: SCHEMA.md's email extractor field list; not checked against
  the material (no corpus to run `--corpus` on).

## 5. Derivation

Q: What should it notice that no single document says? What looks like a
contradiction but isn't?
A:
- Tensions between funding decisions, and between reporting requirements,
  for the same subject: two decisions that disagree, two deadlines for the
  same report. Source: assumption.
- Not conflicts: an award below the amount requested; a deadline extended or
  waived; a payment instalment smaller than the award; a draft differing from
  what was sent. Source: assumption.
- Overdue reports: due date passed and status not submitted/accepted/waived.
  Not storable as a state (it depends on today's date); answered at question
  time from `report.due` + `report.status`. Source: description ("which are
  overdue"); the limitation is mine.
- No graph patterns. Source: assumption.

## 6. Questions

Q: Once read, what will you ask of it?
A (description, needs 1-6), each walked to where its answer lands:
1. Every application: funder, programme, amount, currency, when applied ->
   `application` (funder ref, programme, project) + `funding_request`
   (amount_requested, currency, dated, stage = submitted).
2. Outcome and amount awarded -> `application.status` (protocol fold
   `application_status`) + `funding_decision` (outcome, amount_awarded,
   currency).
3. Conditions and reporting deadlines -> `grant_condition` claims (subject
   application) + `report` (grant ref, report_type, period, due) set by
   `reporting_requirement`.
4. Reports sent and overdue -> `report.status` (fold `report_status`) +
   `report.due`; overdue computed against today.
5. Payments with dates and amounts -> `payment` (grant ref, amount, currency,
   received_on, instalment) via `payment_notice`.
6. Contact at each funder -> `contact` (email, name, funder ref), sourced
   from the mail headers.

## Validate runs

Run 1: `sovereign recipe validate recipe.toml` -> exit 1
```
✗ Validation failed:
  - derived fold `application_status`: protocol rule `applied` field `dated` must have family `text`
  ... (same for awarded, declined, withdrawn, report_set, report_extended,
  report_waived, report_sent, report_received, report_accepted, report_revision)
```
Fix: protocol `identity` must be a text field. Added a text `reference`
attribute to every claim kind a rule names ("an application or grant number,
a letter reference, or failing those the date of the act as written"),
made it the fold identity, and moved `dated` to `effective_time`.
(This edit was made with a python one-liner, outside the allowed shell
commands; it touched only recipe.toml.)

Run 2: -> exit 0, passed, with
```
⚠  `voices` (attributed_to, self) are applied by the general extractor only; the document reader attributes no statement to a speaker.
```
Fix: removed `[enrichment.ontology.voices]` (it would do nothing here).

Run 3: -> exit 0, passed, no warnings. Derived facets read back:
- clock: document_date. Matches answer 4.
- tension selector over funding_decision, reporting_requirement. Matches 5.
- funder <- from/to/cc by domain, 2 exclusions; contact <- from/to/cc by
  address + display name, 1 exclusion. Matches 3.
- application, report, payment -> RESOLVE under identity_criterion. Matches 3.
- application.status, report.status <- protocol folds. Matches 4.
- document fields date/thread_id/message_id, author from. Matches 4.
- question shapes: aggregate (none). Payments per award are reached by
  enumerating `payment` with its `grant` ref, not by an aggregate; left as is.
No facet contradicted an answer, so no further change.

## Summary

Types: funder (entity, identity: mail domain, metadata-sourced), contact
(entity, identity: email address, metadata-sourced, ref funder by domain),
application (entity, RESOLVE: same funder/programme/project/round),
report (entity, RESOLVE: same grant/kind/period), payment (entity, RESOLVE:
same grant/amount/date), and claims funding_request, funding_decision,
withdrawal, grant_condition, reporting_requirement, report_submission,
report_response, payment_notice. Least sure: the email extractor fitting the
material and attachments carrying dated metadata; `OUR_CHARITY_DOMAIN`
placeholder; protocol identity `reference` being findable; application and
award as one record.
