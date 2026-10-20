# Interview transcript: residential lettings (08-leases)

The partner is not available. I answer each question from DESCRIPTION.md where it speaks
(source `description`, quoted) and otherwise from general knowledge of small-landlord
residential letting (source `assumption`).

Templates consulted: `sovereign recipe new --ontology list`, then `contracts` and
`product-support` scaffolded to `tpl-contracts.toml` / `tpl-product-support.toml` in this
directory, for structure only. Neither was copied: both use claim subjects without an
`identity_criterion`, while this recipe is written for the document-reader plan.

## Turn 1: source shape (before the ontology)

**Q. What is the material and in what form?**
A. Tenancy agreements as text, the landlord's email with tenants and contractors, and
contractor invoices.
Source: description, "the tenancy agreements (as text), my email with tenants and contractors, and contractor invoices."

**Q. Which extractor?**
A. `email`. A recipe takes one extractor, and the mailbox carries the dated, threaded record
(rent changes, notices, maintenance), with `date`, `thread_id`, `message_id` and `from` metadata.
I assume the agreements and invoices sit in the mailbox as attachments (invoices emailed by
contractors, agreements sent to tenants for signature). The email extractor routes those
through the described-asset sub-extractors (plaintext, docx, pdf).
Source: assumption. The description does not say where the agreements and invoices are stored.

## 1. Shape

**Q. What is this material about, and what do you want to know about each kind of thing? Is any one a kind of another, or a role something plays? Do you already have any of it in a table?**
A.
- Flats (40 of them): building, flat number, whether let or empty. Source: description, "I manage 40 flats", "Which flats are empty and since when".
- Tenants: people. `tenant` is a role of `person`. Source: description, "which tenants", "A tenant who moves to another of my flats is the same person".
- Tenancies: which flat, which tenants, start, end. Source: description, "Every tenancy: which flat, which tenants, start and end date".
- Contractors: who fixed things. Source: description, "who fixed it", "contractor invoices".
- Maintenance jobs: one fault in one flat, through to repair. Source: description, "Every maintenance request: which flat, what was wrong, ...". I name the type for the thing (a job, i.e. a fault and the work on it), not for the question. Source: assumption.
- Tables: none mentioned, so no `source`. Source: description is silent, so assumption.
- Contractor trade list (plumber, electrician, gas engineer ...): assumption, general domain knowledge.

## 2. Assertion

**Q. What do the sources say about these things (stating, requiring, deciding, asking)? About what? What does each statement carry? How do you tell strong evidence from weak? Who speaks? What must it never do?**
A.
- The agreement *declares* terms of a tenancy: monthly rent, currency, deposit. This is `agreement_terms` (declaration, subject tenancy). Source: description, "monthly rent and currency, deposit held".
- A rent change *declares* a new rent from a date. This is `rent_change` (declaration, subject tenancy; new_rent, previous_rent, currency, effective_from). Source: description, "Rent changes over time, and when each took effect."
- A renewal *commits* both sides to a further term. This is `renewal` (commissive, subject tenancy). Source: description, "Renewals ... with their dates". Commissive force and the fixed_term/periodic/extension values are assumption (UK-style letting practice).
- A notice to quit *declares* that a tenancy ends. This is `notice_to_quit` (declaration, subject tenancy; served_by, served_on, quit_date, grounds, notice_status). Source: description, "notices to quit, with their dates". served_by/grounds/status: assumption.
- A move-in, move-out or empty flat *asserts* a change in occupancy. This is `occupancy_change` (assertive, subject flat; change, effective_from). Source: description, "Which flats are empty and since when". Modelling vacancy as a reported claim is an assumption; see the derivation section.
- A tenant *requests* a repair. This is `maintenance_request` (directive, deontic request, subject maintenance_job; reported_on, reported_by, urgency). Source: description, "what was wrong, when it was reported". urgency: assumption.
- Progress on a repair is *asserted*. This is `repair_update` (assertive; contractor, on, work_done). Source: description, "who fixed it, its status".
- An invoice *asserts* a charge. This is `invoice` (assertive; contractor, invoice_number, invoice_date, amount, currency). Source: description, "contractor invoices", "its ... cost".
- Evidence grades: none declared. The agreement and the invoice are each the document of record for their own claims, so I saw no useful ordering. Source: assumption.
- Speakers: landlord ("me"), tenants, contractors. I first declared `voices` and then removed it, because validate run 1 said the document reader does not apply it (see below).
- must_not: no legal advice on notices, evictions or deposit disputes; never take a figure quoted from an earlier email as a new statement. Source: assumption.
- Currency values GBP/EUR/USD/other: the description wants currency recorded but names none. "Flats", "notices to quit" and "deposit held" read as UK usage, so GBP comes first. Source: assumption.

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**
A.
- Tenant: same person = same full name plus same email or phone. Moving flats makes a new tenancy, never a new tenant. Source: description, "A tenant who moves to another of my flats is the same person with a new tenancy." The name+email/phone rule is assumption.
- Tenancy: same flat, same tenants, unbroken period. A renewal or rent change continues it. A different flat is a new tenancy. Source: description (the move rule); the renewal-continues rule is assumption.
- Flat: same building + flat number. Source: assumption. No external ID is mentioned.
- Maintenance job: same fault, same flat, not yet fixed. A recurrence after a completed repair is a new job. Source: assumption.
- Contractor: same business or personal name, or same email. A firm and its engineer count as one. Source: assumption.
- No external identifiers, so no `identity` keys, only `identity_criterion` + `identity_fallback`. Source: assumption.

## 4. Change

**Q. When does a later statement replace an earlier one, and from when? Does anything move through stages? Which document fields give the date and thread?**
A.
- Rent: a later rent_change replaces an earlier one from its `effective_from`, not from the email's date. `change.supersedes.rent_change = "effective_from"`. Source: description, "when each took effect".
- Agreement terms: a later agreement (e.g. a renewal agreement) replaces earlier terms by document date. Source: assumption.
- Occupancy: superseded on `effective_from`. `flat.occupancy` is held `by = "latest"`. Source: assumption.
- Maintenance status moves through stages reported, acknowledged, contractor_booked, in_progress, fixed / not_fixed / cancelled. Held as `maintenance_job.status` `by = "latest"` (latest report wins). I chose this over a `by = "protocol"` fold: the description puts no restriction on who may move a job, and the corpus offers no stable transition id (the protocol fold's required `identity`), so a protocol fold would mostly stay pending. Source: description, "its status". Stages and the fold choice are assumption.
- Tenancy end: `by = "latest"`, so renewals and notices can move it. Source: assumption.
- Document fields (email extractor): date = `date`, thread = `thread_id`, id = `message_id`, author = `from`. Source: the extractor's documented metadata (SCHEMA.md). Not checked against real mail: `--corpus` was not run because there is no material here.

## 5. Derivation

**Q. What should it notice that no single document says? What looks like a contradiction but is not?**
A.
- Discrepancies between agreement terms, rent changes and invoices (rent stated differently for the same tenancy and period; a job invoiced twice). `tension.between = ["agreement_terms", "rent_change", "invoice"]`, label "discrepancy". Source: assumption.
- Not conflicts: a rent change after the start date; a renewal at a new rent; two invoices for a call-out and a return visit. Source: assumption.
- Vacancy ("empty since when") would ideally be derived as end of last tenancy with no later start. I found no declared way to compute that across tenancy records (`sets` conditions are underdocumented), so vacancy is read from occupancy statements plus the tenancy end dates. Source: assumption, recorded as a limit.

## 6. Questions

**Q. Once it is read, what will you ask of it?** Each question is walked to where its answer lands.
1. "Every tenancy: which flat, which tenants, start and end date, monthly rent and currency, deposit held." Lands in `tenancy` (flat, tenants, start, end) + `agreement_terms` (monthly_rent, currency, deposit). Source: description.
2. "Rent changes over time, and when each took effect." Lands in `rent_change` (new_rent, previous_rent, currency, effective_from), superseded on effective_from. Source: description.
3. "Renewals and notices to quit, with their dates." Lands in `renewal` (agreed_on, new_term) and `notice_to_quit` (served_on, quit_date). Source: description.
4. "Every maintenance request: which flat, what was wrong, when reported, who fixed it, its status and cost." Lands in `maintenance_job` (flat, problem, fixed_by, status) + `maintenance_request` (reported_on, reported_by) + `invoice` (amount, currency). Source: description.
5. "Which flats are empty and since when." Lands in `flat.occupancy` + `occupancy_change.effective_from`, with `tenancy.end` as backup. Source: description.
6. "A tenant who moves to another of my flats is the same person with a new tenancy." Lands in the `tenant` and `tenancy` identity criteria. Source: description.
Extra (assumption): "How much did I spend on repairs at flat X this year?" Lands in `invoice.amount` via `maintenance_job.flat`. "Which contractor do I use most?" The validator lists "invoice by contractor" as an aggregate shape.

## Validate runs

### Run 1
Command: `sovereign recipe validate recipe.toml` (exit 0)
```
✓ Validation passed
Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; ...)
  tension selector: embedding top-k (k = 10, floor = 0.5) over agreement_terms, rent_change, invoice — ...
  identity: flat → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: tenant → RESOLVE under its identity_criterion, ...
  identity: contractor → name + email (descriptive keys, judged merge)
  identity: tenancy → RESOLVE under its identity_criterion, ...
  identity: maintenance_job → RESOLVE under its identity_criterion, ...
  question shapes: enumerate [flat, tenant, contractor, tenancy, maintenance_job]; relations [(none)]; events [(none)]; aggregate [maintenance_request by reported_by, repair_update by contractor, invoice by contractor]
  document fields: document_date ← `date` ..., document_thread ← `thread_id`, document_id ← `message_id` ...; author ← `from`, read by the reader only
  ⚠  the document reader ... makes no record of `contractor` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
  ⚠  `voices` (attributed_to, self) are applied by the general extractor only; the document reader attributes no statement to a speaker.
```
Reading the facets against my answers:
- Contractor would get no records, so "who fixed it" (need 4) would have nothing to point at. Wrong against my identity answer. Fix: give `contractor` an `identity_criterion` so RESOLVE decides it, like `tenant`.
- `voices` is inert under the document-reader plan. Fix: remove it. `reported_by` (ref tenant) and the invoice/update `contractor` refs carry who said what instead.

### Run 2
Command: `sovereign recipe validate recipe.toml` (exit 0)
```
✓ Validation passed
  clock: document_date — supersession folds on document dates
  tension selector: embedding top-k (k = 10, floor = 0.5) over agreement_terms, rent_change, invoice
  identity: flat / tenant / contractor / tenancy / maintenance_job → RESOLVE under its identity_criterion
  question shapes: enumerate [flat, tenant, contractor, tenancy, maintenance_job]; aggregate [maintenance_request by reported_by, repair_update by contractor, invoice by contractor]
  document fields: document_date ← `date`, document_thread ← `thread_id`, document_id ← `message_id`; author ← `from`
```
No warnings. Clock: document_date is right for report order. Rent and occupancy supersede on their own `effective_from` (accepted, not echoed). Every type now has an identity criterion matching my identity answers. No further declaration changes.

## Summary

Types declared and their identity:
- `flat` (entity): RESOLVE, same building + flat number (fallback building, flat_number).
- `tenant` (entity, role of person): RESOLVE, same full name + same email or phone; moving flats keeps the person.
- `contractor` (entity): RESOLVE, same business/personal name or same email.
- `tenancy` (entity): RESOLVE, same flat + same tenants + unbroken period; a renewal continues it, a new flat is a new tenancy.
- `maintenance_job` (entity): RESOLVE, same unfixed fault in the same flat.
- Claims: `agreement_terms`, `rent_change`, `notice_to_quit` (declaration); `renewal` (commissive); `occupancy_change`, `repair_update`, `invoice` (assertive); `maintenance_request` (directive/request).

Least-sure assumptions:
1. That agreements and invoices sit in the mailbox as attachments, so that one `email` extractor reads all three kinds of material. If they are loose files, they need `described_asset` (or a second corpus).
2. That `tenant` and `contractor`, which are subjects of no claim kind, still get RESOLVE records through the `ref` attributes that point at them. Validate stopped warning once both had a criterion, but this is not tested against material.
3. That vacancy can be read from occupancy statements. Small landlords rarely write "flat 7 is empty"; the honest derivation (end of last tenancy, no later start) is not declared.
4. Currency set and UK-style renewal/notice vocabulary.
5. Status held `by = "latest"` by report date, not by protocol.
