# Interview transcript: site purchasing

The partner is not available. Each answer is given from DESCRIPTION.md where it
speaks (source `description`, quoted), else from general knowledge of purchasing
(source `assumption`).

Templates consulted (scaffolded into this directory for reference only, not the
deliverable): `tmpl-contracts.toml`, `tmpl-governance.toml`,
`tmpl-due-diligence.toml`, `tmpl-engineering-org.toml`,
`tmpl-product-support.toml`.

## Source shape (before the six questions)

**Q. What files is the material, and in what format?**
A. A mailbox: requests and approval replies are emails, POs are sent to suppliers,
confirmations and invoices arrive "as text". Extractor `email`, chunker `paragraph`.
Source: `description` ("the purchase-request emails, the approval replies, purchase
orders sent to suppliers, supplier confirmations and invoices (as text)") for the
kinds of document; `assumption` that all of them, POs and invoices included, sit in
one mailbox export (.eml / maildir) rather than as loose text files. If invoices are
loose .txt files, a second recipe with `plaintext` would be needed.

## 1. Shape

**Q. What is this material about, and what would you want to know about each kind of
thing? Is any a kind of another, or a part something plays? Any of it in a table?**
A. Purchases at a manufacturing site, each moving from request to payment. Kinds of
thing: the purchase (one record from request to PO to payment), suppliers, and the
people in the mail (colleagues who request and approve, supplier contacts).
- purchase: request number, PO number, status.
- supplier: its contacts, what we ordered from it.
- contact: email, name, which supplier (if any) they belong to.
Source: `description` ("Every purchase request: who asked, for what ...", "The
purchase order a request became", "Our suppliers, with their contacts").
`assumption`: the request and the PO it became are ONE particular (`purchase`),
because the partner's status list runs from "requested" to "paid" for one "order";
one request becomes at most one PO.
`assumption`: the existing "shared spreadsheet" is not supplied as material, so no
table `source` is declared ("Today we track it in a shared spreadsheet and miss
things" names it as the thing being replaced).
No `specializes` or `role_of`: supplier contacts are linked to their supplier by
mail domain instead (`assumption`).

## 2. Assertion

**Q. What do the sources say about those things: stating, requiring, deciding,
asking? What does each statement carry? Fixed sets of options? Strong vs weak
evidence? Who speaks? What must it never do?**
A. Six kinds of statement, all about a purchase:
- purchase_request (directive, request): requester, item, quantity (+ unit),
  estimated cost, currency, and whether it raises, amends or withdraws a request.
  Source: `description` ("who asked, for what, how many, estimated cost and
  currency"); raise/amend/withdraw is `assumption`.
- approval (declaration): approver, approved/rejected, reason. Source:
  `description` ("Who approved or rejected each request, and when"); "when" is the
  reply's date stamp. Reason is `assumption`.
- purchase_order (commissive): PO number, supplier, total, currency, requested
  delivery date, issued/amended/cancelled. Source: `description` ("its supplier and
  its total"); the rest `assumption`.
- supplier_confirmation (assertive): accepted/declined/shipped/delivered,
  confirmed total, delivery date. Source: `description` names "supplier
  confirmations"; their content is `assumption`.
- invoice (assertive): invoice number, amount, currency, invoice and due dates, the
  PO number it quotes, invoice vs credit note. Source: `description` ("The invoices
  against each purchase order and whether the invoiced amount matches"); fields
  beyond amount are `assumption`.
- payment (assertive): reference, amount, paid date, paid/failed/reversed.
  Source: `assumption`. The description lists no payment documents but its status
  list ends in "paid", so something in the mail must say an invoice was paid
  (remittance advice, accounts' confirmation).
Evidence grades: none declared. Each statement is a business document, not graded
evidence (`assumption`).
Speakers: requesters, approvers, purchasing, suppliers, accounts (`assumption`).
A `voices.attributed_to` list was drafted and then removed: validate reported that the
document reader does not use it (see run 1).
must_not: never approve, reject or release on anyone's behalf; never report an
estimated cost as an ordered or invoiced amount (`assumption`).

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**
A.
- supplier: its email domain, read from the from/to/cc headers; our own domain and
  free-mail providers are excluded. `assumption`. The description gives no
  identifier. `OUR_COMPANY_DOMAIN` is a placeholder the partner must fill.
- contact: email address from the headers, linked to its supplier by domain.
  `assumption`.
- purchase: the PO number when there is one (external key); otherwise the request
  number; otherwise RESOLVE judges under the criterion "same PO or request number, or
  a reply/order/confirmation/invoice that answers or quotes the request: same
  requester, item and quantity, same thread". `assumption`. The description says
  nothing about numbering. Whether requests carry a number at all is the largest
  unknown.

## 4. Change

**Q. When does a later statement replace an earlier one? Does anything move through
stages, which statements move it, who may move it, effective vs reported time,
corrections and reopening? Which document fields give the date and the thread?**
A.
- Stages: requested, approved, rejected, ordered, delivered, invoiced, paid,
  cancelled. Source: `description` ("Each order's status (requested, approved,
  ordered, delivered, invoiced, paid, cancelled)"). "rejected" is added from need 2
  ("approved or rejected"); `assumption` that it is a status.
- Moves (protocol fold `purchase_status` on `purchase.status`), each one claim kind plus
  an exact field value:
  request raised → requested; request amended after rejection → requested
  (reopening, `from = ["rejected"]`); request withdrawn → cancelled; approval approved
  → approved; approval rejected → rejected; PO issued → ordered; PO cancelled →
  cancelled; confirmation delivered → delivered; invoice (not credit note) →
  invoiced; payment paid → paid. Rules and fields are `assumption`.
- Who may move: not restricted (no `by` path). `assumption`.
- Report time only. Moves take effect when the email reports them; no
  `effective_time`. `assumption`.
- Transition identity: a `reference` field on every claim kind (request/PO/
  invoice/delivery-note/payment number; for approvals, the approver's name).
  `assumption`.
- Corrections: no `corrects` rule. An amended PO supersedes the earlier one
  (`change.supersedes`), as does an amended request. `assumption`.
- Document fields: the email extractor's `date`, `thread_id`, `message_id`, `from`
  (field names from SCHEMA.md's email extractor; not checked with `--corpus`
  because no material is available).

## 5. Derivation

**Q. What should it notice that no single document says? What looks like a
contradiction but isn't?**
A. An invoice whose amount does not match its purchase order: tension `mismatch`
between `purchase_order` and `invoice`, compared within the same purchase
(`same = ["subject"]`, because invoice and PO have different dates, so the default
clock criterion would never pair them). Source: `description` ("whether the invoiced
amount matches the order"). Not conflicts (`assumption`): partial invoices for
partial deliveries, separately stated freight/tax, an amended PO the invoice matches,
a credit note that restores the total.
The two-approval rule above 10,000 is a derivation, not a statement: see question 6.

## 6. Questions

**Q. Once it is read, what will you ask of it?** Source: `description` (the six
numbered needs), walked to the declaration:
1. Every request: who, what, how many, cost, currency → `purchase_request.requester /
   item / quantity / quantity_unit / estimated_cost / currency`, subject `purchase`.
2. Who approved/rejected and when → `approval.approver / decision` plus the
   `document_date` stamp. "Above 10,000 needs two approvals" is guidance only. No
   declaration counts approvals against an amount (no comparison condition and no
   count fold), so the check is left to the question, which can read
   `estimated_cost` and the approvals of the same purchase.
3. PO, supplier, total → `purchase.po_number`, `purchase_order.reference / supplier /
   order_total / currency`.
4. Invoices per PO and whether they match → `invoice` claims on the same `purchase`
   (`po_quoted`, `invoiced_amount`), tension `mismatch` between `purchase_order` and
   `invoice`.
5. Status → `purchase.status`, protocol fold `purchase_status`.
6. Suppliers with contacts, ordered this year → `supplier` (metadata-sourced by
   domain), `contact.organization` ref; "ordered this year" = `purchase_order.order_total`
   by `supplier` filtered by `document_date`. Validate lists the aggregate shape
   "purchase_order by supplier". The sum itself is not declarable (no sum fold).

## Validate runs

### Run 1: `sovereign recipe validate recipe.toml` (exit 0)

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  lifecycle: off on fold `purchase_status`; `lifecycle = true`, `replaced` or `acts_under` supply `purchase_replaced` and `purchase_act_under`
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over purchase_order, invoice — cross-document declared corpora select the embedding net; the classifier judges each pair
  source: supplier ← document fields from, to, cc (domain: domain) — one atom per identity value, model atoms with that value merge into it; 2 value(s) excluded
  identity: supplier → domain (external key, strict merge)
  source: contact ← document fields from, to, cc (email: address, name: display_name) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: contact → email (external key, strict merge)
  identity: purchase → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [supplier, contact, purchase]; relations [(none)]; events [(none)]; aggregate [purchase_request by requester, approval by approver, purchase_order by supplier, supplier_confirmation by supplier, invoice by supplier]
  derived: purchase.status ← fold `purchase_status` by `protocol`, after RESOLVE: the claims about it
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread_id`, document_id ← `message_id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted; author ← `from`, read by the reader only, among each document's declared facts
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
```

Reading the facets against the answers:
- clock document_date: matches (report time from the email date).
- tension over purchase_order and invoice: matches question 5.
- supplier and contact identities: match question 3.
- purchase → RESOLVE under the criterion: matches. The facet does not name `po_number`
  as a strict key. RESOLVE decides the record and keys on the criterion; left as is.
- aggregate purchase_order by supplier: matches need 6.
- status fold: matches question 4.
- Warning: `voices.attributed_to` does nothing under the document reader. Fix: removed
  the `[enrichment.ontology.voices]` block.

### Run 2: `sovereign recipe validate recipe.toml` (exit 0)

Same derived facets as run 1, without the voices warning:

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  lifecycle: off on fold `purchase_status`; ...
  clock: document_date — ...
  tension selector: embedding top-k (k = 10, floor = 0.5) over purchase_order, invoice — ...
  source: supplier ← document fields from, to, cc (domain: domain) — ...; 2 value(s) excluded
  identity: supplier → domain (external key, strict merge)
  source: contact ← document fields from, to, cc (email: address, name: display_name) — ...; 0 value(s) excluded
  identity: contact → email (external key, strict merge)
  identity: purchase → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [supplier, contact, purchase]; relations [(none)]; events [(none)]; aggregate [purchase_request by requester, approval by approver, purchase_order by supplier, supplier_confirmation by supplier, invoice by supplier]
  derived: purchase.status ← fold `purchase_status` by `protocol`, after RESOLVE: the claims about it
  document fields: document_date ← `date` ..., document_thread ← `thread_id`, document_id ← `message_id` — ...; author ← `from`, read by the reader only
```

No attribute was reported unfilled. `--corpus` was not run because no material is
available, so the email field names `date`, `thread_id`, `message_id` and `from` come
from SCHEMA.md and were not checked against real documents.

## Summary

Types and identity:
- `supplier` (entity): mail domain from from/to/cc, strict; excludes free-mail and
  `OUR_COMPANY_DOMAIN`.
- `contact` (entity): email address from from/to/cc, strict; `organization` → supplier
  by domain.
- `purchase` (entity): PO number, else request number, else RESOLVE under the
  identity criterion; `status` derived by protocol fold.
- Claims, all with subject `purchase`: `purchase_request` (directive/request),
  `approval` (declaration), `purchase_order` (commissive), `supplier_confirmation`,
  `invoice`, `payment` (assertive).

Least-sure assumptions:
1. A request and its PO are one particular, one-to-one (no split or consolidated POs).
2. Purchases can be linked across request, approval, PO and invoice without a shared
   number (the identity criterion carries all of it).
3. Payment statements exist in the mail; the description lists none.
4. All material is in one mailbox that the `email` extractor reads; the email field
   names are not checked against the corpus.
5. Approvals carry no number, so their transition identity is the approver's name.
6. The two-approval rule above 10,000 is not enforced by the status fold: one
   approval moves a purchase to "approved".
7. `OUR_COMPANY_DOMAIN` must be replaced, or the site itself becomes a supplier.
