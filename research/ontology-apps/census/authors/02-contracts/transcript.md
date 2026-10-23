# Interview transcript: contracts at a services firm

The partner is not available. Each answer comes from DESCRIPTION.md (quoted) or is an
assumption drawn from general knowledge of contract management.

## Opening: the source shape

**Q.** What files do you have, and in what form?
**A.** Signed agreements and amendments as text files, plus the email in which renewals, notices
and amendments are negotiated. The recipe reads one folder with the `plaintext` extractor and
`paragraph` chunks.
**Source.** description: "the signed agreements (as text), their amendments, and the email in which
renewals, notices and amendments are negotiated". Assumption: the agreements and amendments are
plain-text files, and the email (if it is in the folder at all) is exported as text. A recipe gets
only one extractor. Choosing `plaintext` means the `email` extractor's metadata (`date`,
`thread_id`, `from`) is not available.

## 1. Shape

**Q.** What is this material about, and what do you want to know about each kind of thing? Is one
kind a type of another, or a role something plays? Do you keep any of it in a table already?
**A.** It is about contracts. Each contract has a counterparty, a type (master agreement, statement of
work, NDA, lease), an owner on our side, a title, and a reference number if it has one. A statement
of work belongs to a master agreement. Counterparties are organizations; the owner is a person. The
spreadsheet register already holds some of this.
**Source.** description: "Every contract with its counterparty, type (master agreement, statement of
work, NDA, lease), and owner on our side"; "A statement of work belongs to its master agreement";
"Today we keep a contract register in a spreadsheet." Assumptions: contracts carry a title and
sometimes a reference number. The register is not part of the material handed over, so it is not
declared as a table `source`. A table-sourced contract would also be skipped in the text, and it
could not be the subject of the claims below.
**Recipe.** entity `contract` (title, contract_ref, contract_type closed set + `other`, counterparty
→ organization, owner → person, master → contract); entity `organization` (legal_name,
registration_no, jurisdiction).

## 2. Assertion

**Q.** What do the documents say about these things: stating, requiring, deciding? What does each
statement carry? How do you tell strong evidence from weak?
**A.** A signed agreement or amendment sets terms by signing them (a declaration). The terms are:
- the term period: effective date and end date;
- renewal and notice: whether the contract renews automatically, the renewal period, the notice
  needed to stop a renewal or to terminate, and the notice deadline;
- price: value, currency, and basis.

An amendment has a number, a signing date, an effective date, and the terms it changes. A notice
terminates, declines renewal, renews, extends, or withdraws an earlier notice. A parties clause
describes each party: legal name, registration number, and its role in the agreement. Evidence
grades: signed instrument, then unsigned draft, then correspondence.
**Source.** description: "effective date, end date, and notice period for renewal or termination";
"value and currency, and how an amendment changed it"; "Which amendments apply to which contract, in
order, and what each changed"; "which renew automatically and the date by which notice must be
given". Assumptions: the evidence grades; the `basis` set (total / annual / monthly / rate card);
the currency list; the parties-clause claim (standard drafting, and the only way organizations get
records); `must_not`, which is left empty because the partner named no prohibition.
**Recipe.** claims `term_dates`, `renewal_term`, `price_term`, `amendment`, `notice` (all
`declaration`, subject `contract`), `party_description` (`assertive`, subject `organization`).

## 3. Identity

**Q.** How do you know two mentions are the same contract, or the same organization? Is there an ID?
**A.** Contract: the same signed agreement with the same counterparty, matched on title, number or
signing date. A statement of work is never its master agreement. An amendment is part of the contract
it amends, not a new contract. Organization: the registration number, or else the legal name plus
jurisdiction. A trading name is the same company; an affiliate is a different one.
**Source.** assumption. The description names no ID. Contract numbers are not reliable enough to
merge on strictly: a statement of work often quotes its master agreement's number. So they are a
descriptive fallback key, and `contract_type` is `identity_necessary`.

## 4. Change

**Q.** When does a later statement replace an earlier one, and from when? Does anything move
through stages? Which document fields give the date and the conversation?
**A.** An amendment replaces the term, renewal or price statement it changes, from the amendment's
effective date. Before that date the earlier statement still holds. Each term statement carries
`as_of` (the original's effective date, or the amendment's effective date), and supersession is
ordered on that field. A contract also moves between states (in force, terminated, not renewed,
replaced) through notices.
**Source.** description: "how an amendment changed it"; "which were in force on a given past date".
Assumptions: amendments apply prospectively from their effective date, and the stages.
**Outcome.** I drafted a protocol fold for contract status driven by `notice`. Validation refused
it because report order needs `change.document.date`, and plaintext documents carry no date
metadata. I removed the fold. Notices stay as `notice` claims with `given_on` and `effective_on`.
There is no `change.document`, because the plaintext extractor carries no metadata.
**Recipe.** `change.supersedes = { term_dates = "as_of", renewal_term = "as_of", price_term = "as_of" }`.

## 5. Derivation

**Q.** What should it notice that no single document says? What looks like a contradiction but
isn't?
**A.** It should flag discrepancies: two statements of the same term for the same contract and date
that disagree, for example a figure in an amendment that does not match the agreement. These are
not conflicts:
- an amendment against the term it replaces;
- an SOW value against its MSA rate card or cap;
- a draft or proposal against the signed term;
- words against figures for the same amount;
- a renewal period against the initial term.

**Source.** assumption. The description names no derivation.
**Recipe.** `tension.between = [term_dates, renewal_term, price_term]`, default `same` (subject +
clock), `label = "discrepancy"`, five `not_conflicts`.

## 6. Questions

**Q.** Once it is read, what will you ask of it?
**A.** These are the description's seven questions. Each maps to a declaration:
1. Contract, counterparty, type, owner → `contract.counterparty`, `contract.contract_type`,
   `contract.owner` (records folded by `agree` / `most`).
2. Effective date, end date, notice period → `term_dates.effective` / `.ends`;
   `renewal_term.renewal_notice` / `.termination_notice`.
3. Value, currency, and how an amendment changed it → `price_term.value` / `.currency` / `.basis`,
   superseded on `as_of`, with `instrument` naming the amendment; `amendment.changes = value`.
4. Amendments per contract, in order, and what each changed → `amendment` claims on the contract,
   ordered by `effective` / `signed_on`, with `amendment_no`, `changes`, `summary`.
5. In force today, or on a past date → the `term_dates` statement current at that date (supersession
   on `as_of`), then effective ≤ date ≤ ends, minus any `notice` of termination or non-renewal
   effective before that date. This is a reading over declared data; no derived status attribute
   holds it (see Change).
6. Auto-renewing contracts and the notice deadline → `renewal_term.auto_renews = automatic`,
   `renewal_term.notice_deadline` where the text states it. Otherwise it has to be computed as
   `ends` minus `renewal_notice`, and nothing declared does that arithmetic.
7. SOW belongs to its master agreement → `contract.master` (ref → contract), with
   `contract_type = statement_of_work`.

**Source.** description (the numbered list).

## Validation runs

### Run 1: failed (exit 1)
```
✗ Validation failed:
  - derived fold `contract_status`: protocol report order requires `change.document.date`; transition effective time is a separate claim field
  - derived fold `contract_status`: protocol rule `termination_notice` field `given_on` must have family `text`
  - ... (same for non_renewal_notice, renewal_notice, extension_notice, notice_withdrawn)
  - derived fold `contract_status`: protocol rule `notice_withdrawn` field `withdrawn_notice` must have family `text`
  - derived fold `contract_status`: protocol rule `contract_replaced` reads `effective_on`, which `contract_replaced` does not declare
```
Fix: the plaintext documents carry no date, so the protocol fold cannot run. I removed the fold and
the derived `contract.status` attribute.

### Run 2: passed (exit 0), with two warnings
```
  identity: organization → canonical name (default; ...)
  ⚠  the document reader ... makes no record of `organization` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
  ⚠  `voices` (self) are applied by the general extractor only; the document reader attributes no statement to a speaker.
```
Fixes:
- `counterparty` could not point at any organization record. I added the `party_description`
  claim (subject organization), plus organization attributes, identity fallback and criterion.
- `voices.self` was inert, so I removed it.
- `owner by = "latest"` would never fold without document dates, so I changed it to `by = "most"`.

### Run 3: passed (exit 0)
```
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over term_dates, renewal_term, price_term — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: organization → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: contract → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [organization, contract]; relations [(none)]; events [(none)]; aggregate [(none — a claim type with a `ref` attribute can be counted by it)]
```
Checking the derived facets against my answers:
- **Identity.** Both lines match my answers.
- **Tension selector.** Matches.
- **Question shapes.** They enumerate contracts and organizations, which matches. There are no
  relations or events, by design: SOW→MSA is a ref attribute, and notices and amendments are claims.
- **Clock.** The global clock is the default `document_date`. The material has no document dates,
  and every superseding type names its own `as_of` field in `change.supersedes`. Neither `narrative`
  nor `none` is closer, so I left the default and flagged it below.

## Summary

Types declared and their identity:
- `organization` (entity): RESOLVE under criterion. Fallback keys: registration_no, legal_name,
  jurisdiction.
- `contract` (entity): RESOLVE under criterion. Fallback keys: contract_ref, counterparty, title.
  `contract_type` must agree.
- `party_description` (claim, assertive, subject organization).
- `term_dates`, `renewal_term`, `price_term` (claims, declaration, subject contract): superseded on
  `as_of`.
- `amendment`, `notice` (claims, declaration, subject contract): not superseded; they accumulate.

Assumptions I am least sure of:
1. The `plaintext` extractor for mixed agreements and email. If the email is `.eml` or a mailbox,
   it is either unread or read without its date, thread and sender. That date is also what a
   contract-status protocol fold needs.
2. Whether an amendment's effective date (`as_of`) is reliably stated. Supersession depends on it.
3. Whether the owner on our side is named in the documents at all. It may live only in the register.
4. The notice deadline is only read where it is written; it is never computed.
5. Contract identity with no strict key. Merging depends on the model judging the criterion.
