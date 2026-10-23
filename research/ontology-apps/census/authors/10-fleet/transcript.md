# Interview transcript: delivery company's vans

Partner not available. Each answer is sourced as `description` (quoting DESCRIPTION.md) or
`assumption` (general knowledge of running a small UK-style van fleet).

## Q0 (source shape). What is the material and in what form?

Answer: a mailbox. Drivers' daily check emails; garage job sheets and invoices as text; fuel
receipts. One recipe takes one extractor, so I take `email`: it carries `date`, `thread_id`,
`message_id` and `from`, which give claims their date and give drivers their identity. I assume
the job sheets, invoices and receipts reach the fleet manager as emails or as attachments to them
(the email extractor dispatches attachments as described assets).
Source: description ("the drivers' daily check emails (mileage, defects), garage job sheets and
invoices (as text), and fuel receipts") for the content; assumption for the mailbox form and for
job sheets/receipts arriving by email.

## Q1 Shape. What is this about, what do you want to know about each kind of thing; is any a kind or role of another; any of it in a table?

Answer:
- van: registration, model, year. Source: description ("Every van, by registration, with its model and year").
- driver: who reported a defect, who had which van. A role of a person. Source: description ("by which driver", "Which driver had which van on which day"); `role_of = "person"` is assumption.
- garage: who did the work. Source: description ("which garage").
- defect: a fault on a van, open until fixed. Declared as an entity (a condition with a van, an item and a status), because it has to carry attributes and a status a state type cannot. Source: description ("Every defect reported, on which van ... whether and when it was fixed"); modelling as an entity is assumption.
- garage_job (event): a service or repair, with van, garage, work done, cost, currency, date, job number, job type. Source: description ("Every service and repair: which van, which garage, the work done, the cost and currency, the date"); job number / job type are assumption.
- No tables: nothing indicates a fleet register file. Source: assumption (description lists only emails, job sheets/invoices, receipts).
- Fuel purchases (litres, price) are not declared as a type: none of the six needs asks about fuel. Receipts feed mileage readings only. Source: assumption.

## Q2 Assertion. What do the sources say, about what, carrying what; strong vs weak evidence; who speaks; must-nots?

Answer:
- Drivers state a van's mileage and defects on their daily check, which also says they had that van that day. Source: description ("daily check emails (mileage, defects)"); "the check says they had the van that day" is assumption.
- Garages state work done, cost, date, often mileage, and when the next service is due. Source: description for work/cost/date and for "next service is due by mileage or by date"; that garages are the ones stating the due point is assumption (service stickers, job sheets).
- Garages state a defect was fixed (or parts awaited / no fault found). Source: description ("whether and when it was fixed"); the four outcomes are assumption.
- Fuel receipts may state mileage. Source: assumption (fuel-card receipts often prompt for odometer).
- Claim kinds: `mileage_reading`, `allocation`, `defect_update`, `job_record`, `service_due`, `garage_details` (all assertive). Grades: none; no evidence ladder is described.
- Who speaks: drivers, garages, the fleet manager. A `voices.attributed_to` block was tried and removed: validate warned the document reader ignores it (see run 1).
- Must not: say a van is roadworthy / safe to drive. Source: assumption.

## Q3 Identity. How do you know two mentions are the same thing?

Answer:
- van: registration plate, ignoring spaces and case. Source: description ("by registration"); normalisation is assumption.
- driver: the address the daily check email is sent from (metadata source on `from`). Source: assumption.
- garage: VAT number, else name at the same place. Source: assumption (UK invoices carry a VAT number).
- defect: same fault on same van with no repair reported in between. Source: assumption.
- garage_job: same job/invoice number, else same van, garage and date. Source: assumption.

## Q4 Change. When does a later statement replace an earlier one; what moves through stages; which document fields give date and thread?

Answer:
- service_due: a later statement replaces the earlier one (`supersedes = { service_due = "document_date" }`). Source: description ("When each van's next service is due") implies one current due point; supersession is assumption.
- mileage_reading: never superseded; the history is the answer. Source: description ("Each van's mileage over time").
- defect moves through stages open -> awaiting_parts -> fixed (or no_fault_found), reopened by a new report. Protocol fold `defect_status` on `defect.status`, one rule per `defect_update.outcome`, transition identity `update_ref`, effective time `on`. Source: description ("whether and when it was fixed"); stages are assumption. No `by` (anyone may move it) and no `corrects` rule: assumption.
- garage_job.cost folds `latest` (invoice over job-sheet estimate). Source: assumption.
- Document fields: `date`, `thread_id`, `message_id`, author `from` (the email extractor's documented fields). Source: SCHEMA.md email extractor; not checked with `--corpus` (no material available).

## Q5 Derivation. What should it notice that no document says; what looks like a contradiction but isn't?

Answer: a later mileage reading lower than an earlier one for the same van (odometer error or wrong van). Not conflicts: readings on different days differ; a driver's and a garage's same-day readings differ by the miles to and from the garage. `tension.between = ["mileage_reading"]`, `same = ["subject"]`. Source: assumption.

## Q6 Questions. What will you ask of it? Walk each to where its answer lands.

1. Every van by registration with model and year -> `van` (registration, model, year; by agree/most). Source: description.
2. Each van's mileage over time -> `mileage_reading` (subject van; odometer, read_on, reported_by) + document_date. Source: description.
3. Every defect, which van, which driver, whether and when fixed -> `defect` (van, item, description, status via fold `defect_status`), `defect_update` (outcome, on, reported_by -> driver, garage). Source: description.
4. Every service and repair -> `garage_job` (job_type, van, garage, work_done, cost, currency, done_on), read through `job_record`; garage -> `garage`. Source: description.
5. Next service due by mileage or date -> `service_due` (due_mileage, due_date), superseded by document date. Source: description.
6. Which driver had which van on which day -> `allocation` (subject van; driver -> driver, day). Source: description.

Follow-up raised by the schema: a type RESOLVE decides needs a claim kind whose subject it is. `garage` had none, so `garage_details` (the garage's name, address, VAT number on its paperwork) was added. Source: SCHEMA.md rule; content is assumption.

## Validate runs

Run 1: `sovereign recipe validate recipe.toml` -> exit 0, "Validation passed". Derived facets:
clock document_date; tension over mileage_reading; van/garage/defect/garage_job identity by RESOLVE
under their criteria; driver from `from` (address, display_name), strict merge on email;
question shapes enumerate [van, driver, garage, defect], events [garage_job], aggregate
[allocation by driver, defect_update by reported_by, defect_update by garage]; defect.status
from fold `defect_status`; document_date <- date, document_thread <- thread_id, document_id <-
message_id, author <- from. Warning: "`voices` (attributed_to) are applied by the general
extractor only; the document reader attributes no statement to a speaker."
Read against answers: the voices block does nothing under this declaration -> removed. Garage
is RESOLVE-decided but no claim kind had it as subject -> added `garage_details`, gave garage a
`vat_number` key and `address`.

Probes (copies under scaffolds/, not the recipe): garage without identity_criterion -> exit 1,
"`name` declares `by = "most"`, but RESOLVE does not decide `garage`"; with `by` removed it
passes but warns the reader makes no record of garage. Removing `job_record` left garage_job with
no subject claim and validate did not flag it, so validate's silence on garage was not evidence
garage would be read; hence `garage_details`.

Run 2 (final): `sovereign recipe validate recipe.toml` -> exit 0, "Validation passed", same facets
as run 1 without the voices warning; garage identity still printed as RESOLVE under its criterion.
Read against answers: van identity prints as RESOLVE under the criterion rather than as the
`registration` external key; the criterion names the plate, so left as is. Driver records will
include any non-driver sender (garages, the fleet manager): no exclude list can be written
without knowing their addresses.

## Summary

Types: van (entity; identity registration + criterion), driver (entity, role of person; identity
email from `from` metadata), garage (entity; identity vat_number, fallback name, criterion),
defect (entity; criterion: same fault, same van, no repair in between; status by protocol fold),
garage_job (event; criterion: same job/invoice number or same van+garage+date), claims
job_record, garage_details, mileage_reading, allocation, defect_update, service_due.

Least sure: that job sheets/invoices/receipts arrive as email (else `email` misses them); driver
from sender address (garages and the manager become "drivers"); service_due superseding per van
regardless of kind (MOT vs service); defect status stages; `garage_details` as the claim that
keeps garage readable.
