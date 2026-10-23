# Interview transcript: employee expenses at a consultancy

The partner was not available. Each answer is sourced as `description` (quoting DESCRIPTION.md)
or `assumption` (general knowledge of consultancy expense handling).

## Source shape (before the six questions)

**Q. What files is the material, and in what format?**
A. A mailbox: the claim emails (receipts pasted as text), managers' replies and the bank's
payment notices. `acquire.type = "local_file"` at `SOURCE_PATH`, `extract.type = "email"`,
paragraph chunks.
Source: description, "the expense-claim emails with receipts (as text), the approval replies
from managers, and the payment notices from our bank". Assumption: they are exported as
.eml/maildir, which is what the email extractor reads.

## 1. Shape

**Q. What is this material about, and what would you want to know about each kind of thing?
Is any of them a kind of another, or a part something plays? Do you already have any of it in
a table?**
A. Its subject is expense claims. For each claim finance wants: who claimed, what it was for,
the amount and currency, the date incurred, the category, the client project (if any), and the
status. People appear as claimants and approving managers, both members of staff.
Source: description, need 1: "who claimed, for what, the amount and currency, the date
incurred, the category, the client project if any"; need 3: "Each claim's status".
→ `expense_claim` (entity: claimant, purpose, amount, currency, incurred, category, project,
status) and `employee` (entity).

**Follow-up. Is the client project its own thing with its own identity, or a label on the claim?**
A. In the domain it is its own thing: a client engagement with a job code. But nothing in this
material is said *about* a project. Validate showed that a project type would be decided by
RESOLVE with no claim kind about it, so it would get no records. So the project is the code or
name as written, held on the claim.
Source: assumption (the job codes), plus the validate run 2 facet.

**Follow-up. The category set?**
A. These categories are typical for a consultancy: air/rail travel, ground transport, mileage,
accommodation, meals, client entertainment, training, equipment, software, phone/internet,
office supplies, other (12, the cap).
Source: assumption. The description says only "the category".

**Follow-up. Do you hold the policy limits per category in a table?**
A. Probably, yes (finance owns the policy), but it is not part of this material. I declared it
as a table-sourced type in run 2 and removed it after validate: "declares a table source, which
no stage reads yet … so it gets no records".
Source: assumption + validate run 2.

## 2. Assertion

**Q. What do the sources say about those things: stating, requiring, deciding, asking? What
does each statement carry? Who is speaking? How do you tell strong evidence from weak? What
must it never do?**
A. There are three kinds of statement, all about one expense claim:
- The employee **asks** to be reimbursed (directive, request): a first submission, a
  re-submission, or a chase. It carries whether the receipt is included, a reference, and the
  date.
  → `expense_submission` {action: submitted/resubmitted/chased, receipt: attached/missing,
  reference, effective_on}. The claim's own fields (amount etc.) are asked of each statement
  and folded onto the record (`by = "latest"`, claimant `by = "agree"`).
- A manager **decides** (declaration): approves or rejects, with a reason.
  → `approval_decision` {decision, approver → employee, approver_role, reason (closed set incl.
  over_limit, missing_receipt), reference, effective_on}.
- The bank **states** it paid (assertive).
  → `payment_notice` {outcome: paid/returned, amount_paid, paid_currency, reference (bank
  payment ref), effective_on (payment date)}.
Strength of evidence: the bank notice is authoritative for payment, and the manager's reply for
the decision. No grades are declared, because each statement kind has a single source.
Must never: judge an employee's honesty or intent; a breach is reported as a breach of a stated
rule.
Source: description, "the expense-claim emails …, the approval replies from managers, and the
payment notices from our bank"; need 2: "Who approved or rejected each claim, and when"; need
5: "missing a receipt". The forces, the reason set, the chase action and the must_not are
assumptions.

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**
A. An employee is known by email address (an external key, read from the from/to/cc fields).
An expense claim is the same claim when it is the same employee seeking reimbursement for the
same cost: the same claim reference if one is quoted, otherwise the same purchase incurred on
the same date. A claim re-submitted after a rejection is the same claim even if the amount,
category or receipt changed. Two purchases on the same day are two claims.
Source: description, need 6: "Claims re-submitted after a rejection are the same claim."
Assumptions: email address as staff identity; that claim references are not reliably present
(so identity is an `identity_criterion` decided by RESOLVE, not an external key).

## 4. Change

**Q. When does a later statement replace an earlier one? Does anything move through stages —
what stages, which statements move it, who may make each move, does a move take effect when
reported or at a time it names, how is a mistake corrected or a finished thing reopened? Which
document fields give its date and conversation?**
A. Stages: submitted → approved | rejected → paid. A rejected claim reopens to submitted when
re-submitted. Moves:
- submission (action = submitted) → submitted, from any state;
- re-submission (action = resubmitted) → submitted, only from rejected;
- a manager's or finance's approval → approved, from submitted;
- a manager's or finance's rejection → rejected, from submitted or approved;
- bank notice paid → paid, only from approved;
- bank notice returned → back to approved, from paid (the correction for a bounced payment).
Only managers or finance may approve or reject (an approval by the claimant or anyone else moves
nothing). A move is reported at the email's date; its effective time is the date it names
(`effective_on`; for a payment, the payment date). A later submission supersedes the earlier
one for the same claim (on document date). Document fields from the email extractor: `date`,
`thread_id`, `message_id`, author `from`.
Source: description, need 3: "status (submitted, approved, rejected, paid) and the payment
date"; need 6 (resubmission). Who may move, the "from" restrictions, the returned-payment
correction and the absence of a "withdrawn" state are assumptions. The field names come from
the email extractor's documented metadata (SCHEMA.md), not from a `--corpus` run, because I
have no material.

## 5. Derivation

**Q. What should it notice that no single document says — contradictions, patterns? What
looks like a contradiction but isn't?**
A. Discrepancies between what was claimed, what was decided and what was paid for the same
claim (e.g. a paid amount that differs from the claimed amount, or a payment on a rejected
claim). Not conflicts: rejection → re-submission → approval (that is history); a payment in
another currency converted at the bank's rate; a reminder repeating a claim.
Policy breaches: "missing a receipt" is a read field (`receipt = missing`). "Over the limit for
their category" needs a comparison of amount against a per-category limit. The declaration
language has no numeric comparison against a per-category limit (sets compare by equality,
threshold patterns are one number over relation edges, and table sources are not read yet).
So it lands only where an approver says so (`reason = over_limit`).
Source: description, need 5; the rest is assumption.

## 6. Questions

**Q. Once it is read, what will you ask of it?** (check, not shape)
1. Every claim with its fields → `expense_claim` attributes. Lands.
2. Who approved/rejected and when → `approval_decision.approver`, `decision`, `effective_on`
   and the document_date stamp. Lands (validate lists "aggregate [approval_decision by
   approver]").
3. Status and payment date → `expense_claim.status` (protocol fold `claim_status`),
   `payment_notice.effective_on`. Lands.
4. Totals per employee/project/category for a period → `expense_claim.claimant`, `project`,
   `category`, `amount`, `currency`, `incurred`. The data lands; the sum is a query over
   enumerated claims and is not declared (validate's aggregate shapes list only approvals by
   approver). The project is text, not a resolved entity.
5. Policy breaches → `expense_submission.receipt = missing` lands. Over-limit lands only as a
   stated `approval_decision.reason = over_limit`; it is not computed.
6. Re-submission is the same claim → `expense_claim.identity_criterion`, the `resubmitted`
   action, the `resubmission_after_rejection` rule, and `change.supersedes`. Lands.
Source: description (the six needs).

## Validate runs

### Run 1, exit 1
```
✗ Validation failed:
  - derived fold `claim_status`: protocol rule `first_submission` reads `document_id`, which `expense_submission` does not declare
  ... (same for all 8 rules)
```
Fix: protocol `identity` must be a field declared on each rule's claim kind, so I added
`reference` to all three claim kinds and replaced `payment_ref` with it.

### Run 2, exit 0, with facets that disagreed with intent
```
✓ Validation passed
  identity: client_project → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: category_limit → category (external key, strict merge)
  derived: `submissions_without_receipt` is declared and no derived attribute uses it
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
  ⚠  ontology type `category_limit` declares a table source, which no stage reads yet, ...
```
Fixes:
- `client_project` has no claim about it, so it would get no records. Removed; `project` became text on `expense_claim`.
- The `category_limit` table is not read. Removed.
- The unused set was removed; the `receipt` field answers it directly.
- `voices` has no effect under the document reader. Removed.

### Run 3, exit 0 (final)
```
✓ Validation passed

Derived from your declarations:
  lifecycle: off on fold `claim_status`; ...
  clock: document_date — supersession folds on document dates ...
  tension selector: embedding top-k (k = 10, floor = 0.5) over expense_submission, approval_decision, payment_notice ...
  source: employee ← document fields from, to, cc (email: address, name: display_name) ...
  identity: employee → email (external key, strict merge)
  identity: expense_claim → RESOLVE under its identity_criterion, over the claims declaring it their subject; ...
  question shapes: enumerate [employee, expense_claim]; relations [(none)]; events [(none)]; aggregate [approval_decision by approver]
  derived: expense_claim.status ← fold `claim_status` by `protocol`, after RESOLVE: the claims about it
  document fields: document_date ← `date` ..., document_thread ← `thread_id`, document_id ← `message_id` ...; author ← `from` ...
```
Facets read against the answers: the clock is document date (change ✓); claim identity is
RESOLVE under the criterion (identity ✓, need 6); employee identity is email (✓); status comes
from the protocol (✓); the tension selector covers the three statement kinds (✓). None is
wrong; no further fix.

## Summary

Types:
- `employee` (entity): identity `email`, an external key read from from/to/cc metadata.
- `expense_claim` (entity): identity is RESOLVE under its `identity_criterion` (same
  employee, same cost; a re-submission is the same claim). `status` is derived by the
  protocol fold `claim_status`.
- `expense_submission` (claim, directive/request, subject expense_claim).
- `approval_decision` (claim, declaration, subject expense_claim).
- `payment_notice` (claim, assertive, subject expense_claim).
Claim kinds carry no identity of their own; their protocol transition identity is `reference`.

Assumptions I am least sure of:
1. `reference` as the protocol transition identity. If an email names no reference, the reader
   has nothing to point at, and that move stays pending.
2. Gating approvals and rejections on `approver_role` = manager/finance. If the reader can't
   tell the role from a bare "Approved", moves will not count.
3. `bank_pays` only from `approved`. A payment with no recorded approval will not move the
   status.
4. The email field names (`date`, `thread_id`, `message_id`, `from`) were not checked with
   `--corpus`.
5. The 12-value category set, and project held as text rather than a resolved entity.
6. No "withdrawn" state, because the description lists four.
