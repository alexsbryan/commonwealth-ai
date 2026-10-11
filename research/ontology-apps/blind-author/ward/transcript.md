# crm-ward — recipe-author transcript (partner unavailable; answers from DESCRIPTION.md or assumption)

Templates consulted: `sovereign recipe new --ontology list`, then scaffolded `contracts`,
`product-support` and `due-diligence` for structure (nearest to deals / obligations /
counterparties). None was copied; the ontology below is written for this mailbox.

## 0. Source shape (skill: "start from the source shape")

**Q.** What are the files?
**A.** A mailbox of individual email messages → `acquire.type = "local_file"`, `path = "SOURCE_PATH"`,
`extract.type = "email"`, `chunk.type = "paragraph"` (2000 / 200).
**Source.** description: "this is my mailbox from about 2000 into 2001, the copy that was released
publicly with the rest of the Enron email". That the release is a maildir of RFC-5322 files the
`email` extractor reads is **assumption** (general knowledge of the Enron release).

## 1. Shape

**Q.** What is this material about, and what would you want to know about each kind of thing? Is any
of them a kind of another, or a part something plays? Do you already have any of it in a table?

**A.** It is a gas marketer's book of business. The central thing is the **deal** (counterparty,
direction, delivery point, volume, price, period, product, deal number). Around it: **company**
(any organization in the mail), with **customer** and **broker** as roles a company plays;
**correspondent** (a person Kim writes with, with their company and function — buyer, broker,
pricing, credit, contracts, scheduling, legal); **delivery_point** (hub / pool / citygate / meter, on
a pipeline). The pair broker → deal is a **brokered** relation. Phone calls the mail refers to are
**call** events. Where a deal stands over time is a **deal_stage** state of the deal. No table
exists; companies and correspondents are read from the From/To/Cc headers without a model call.
**Source.**
- description: "back and forth with the customers I sell natural gas to, mostly utilities,
  municipalities and industrial users, with brokers, and with people inside Enron in pricing,
  credit, contracts, scheduling and legal" → company/customer/broker/correspondent, `sector` and
  `function` value sets.
- description: "someone wants gas somewhere for some period, and we work out the terms" → deal with
  delivery point, period, volume, price.
- description: "phone calls I only half record in mail" → `call` event.
- description: "which deals I have with whom, where each one stands and how that changed over
  time" → `deal.counterparty`, `deal_stage` state.
- assumption: units (MMBtu/d volume, $/MMBtu or index ± basis price), product values (baseload,
  swing, storage, …), delivery points identified by pipeline meter, and deal numbers (Sitara /
  EnronOnline) — general knowledge of 2000-era US physical gas marketing.
- assumption: no existing table; headers are the only structured source.
- assumption: customer is a *role* of a company (`role_of`), not a kind (`specializes`), since the
  same company can be a customer on one deal and a counterparty in another capacity.

## 2. Assertion

**Q.** What do the sources say about those things — stating, requiring, deciding, asking? About
what? How do you tell strong evidence from weak? Who is speaking, and are they part of the subject?
What must it never do?

**A.** Claim types, all but one about a deal:
- **inquiry** — directive/request: someone asks for gas or a price.
- **offer** — commissive: a price or terms offered, open until accepted/withdrawn.
- **agreed_terms** — commissive: terms both sides accept, as they stand after the message (recap,
  confirm, acceptance, amendment). Grades, strongest first: signed confirmation › deal ticket/number ›
  written acceptance › recap of a call › secondhand mention.
- **status_report** — assertive: where the deal stands and who it waits on.
- **commitment** — commissive: someone undertakes to do something for someone (owed_by, owed_to,
  action, due, open/done/released).
- **requirement** — directive (require/forbid/permit), about a *company*: credit limits, guaranties,
  master agreements set by credit/contracts/legal.
Voices: `self = "Kim Ward"`; claims attributed to Kim, customer, broker, Enron colleague, newsletter.
must_not: call a term agreed when only offered; take a newsletter price as a deal term; present
personal notes / announcements as business.
**Source.**
- description: "we work out the terms over a string of messages and phone calls" → offer vs
  agreed_terms split; "what was agreed" → agreed_terms; "what I still owe somebody" → commitment;
  "where each one stands" → status_report.
- description: "I'm Kim Ward" → `voices.self`.
- description: "There is also plenty of noise, like newsletters, company announcements and personal
  notes" → must_not items 2 and 3, and `newsletter` as an attributable voice.
- assumption: the evidence grades (confirm beats recap beats hearsay) — general knowledge of how gas
  deals were papered (confirmations from the contracts group).
- assumption: requirement claims from credit/legal — description names those groups but not what
  they say; credit limits and guaranties are the usual content.

## 3. Identity

**Q.** How do you know two mentions are the same thing? Is there an ID?

**A.**
- company — email domain (external key, strict), from headers.
- customer, broker — the company's domain; fallback name.
- correspondent — email address (external key, strict), from headers; display name as `name`;
  `company` linked by domain.
- delivery_point — meter number; fallback name + pipeline.
- deal — deal number where given; otherwise RESOLVE under a criterion: same counterparty, direction,
  delivery point and overlapping period; re-pricing / re-volume / extension is the same deal; a new
  period after the old one ended is a new deal; nicknames and forwards name the deal of their thread.
**Source.**
- description: "Threads run for weeks, get forwarded, and mention the same deal under different
  words" → deal needs a judged criterion, not just a string match; forwards inherit the deal.
- assumption: deal numbers exist in some messages (Enron's Sitara/EOL tickets).
- assumption: email domain identifies a company (true for corporate customers; bad for anyone
  writing from a webmail address — the bundled mailbox-provider list skips those).

## 4. Change

**Q.** When does a later statement replace an earlier one, and from when?

**A.** Clock = document date (the email's `Date`). A later **agreed_terms**, **offer** or
**status_report** on the same deal retires the earlier one; a **requirement** supersedes on its own
`valid` period. **commitment** and **inquiry** are not superseded (several can be open on one deal at
once; a commitment carries its own open/done state). Document fields stamped: `date` →
document_date, `thread_id` → document_thread, `message_id` → document_id. A quoted earlier message
inside a forward speaks at its own date (guidance).
**Source.**
- description: "where each one stands and how that changed over time" → supersession on document
  date for terms and status.
- description: "Threads run for weeks, get forwarded" → thread stamp; quoted text keeps its date
  (assumption on how to treat it).
- assumption: the `email` extractor's metadata names are `date`, `thread_id`, `message_id`, `from`,
  `to`, `cc` (SCHEMA.md says it carries parsed headers plus a `thread_id`; exact key spellings are
  not given there).
- assumption: commitments should not supersede by deal, since supersession is keyed on subject and
  would let one promise retire another.

## 5. Derivation

**Q.** What should it notice that no single document says — contradictions, patterns, larger
structures? What looks like a contradiction but isn't?

**A.** Discrepancies between **agreed_terms** and **status_report** on the *same deal at any date*
(`same = ["subject"]`) — e.g. a confirm that disagrees with Kim's recap, or a deal reported confirmed
in one thread and dead in another. Not conflicts: an amendment; an offer vs a different agreed price;
index vs its fixed equivalent; per-day vs period volumes or MMBtu vs Dth; nickname vs deal number;
a quoted old message inside a forward. No graph patterns declared (no cycle or threshold is asked
for). Configurations derive on by default.
**Source.**
- description: "mention the same deal under different words" → nickname not_conflict.
- description: "what was agreed" → agreed_terms is the tension target.
- assumption: the remaining not_conflicts (units, index vs fixed, amendments) — general domain
  knowledge.
- assumption: comparing across dates rather than only same-date claims (see validate run 1 below).

## Follow-up the schema needed

**Q.** How many entities can one section introduce? **A.** 30 (raised from 15) — emails with long
To/Cc lists and multi-deal recaps. **Source.** assumption.

## Validation runs

### Run 1 — `sovereign recipe validate recipe.toml`
```
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over agreed_terms, status_report — cross-document declared corpora select the embedding net; the classifier judges each pair
  source: company ← document fields from, to, cc (domain: domain) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: company → domain (external key, strict merge)
  identity: customer → canonical name (default; declare `identity = [...]` for an external key)
  identity: broker → canonical name (default; declare `identity = [...]` for an external key)
  source: correspondent ← document fields from, to, cc (email: address, name: display_name) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: correspondent → email (external key, strict merge)
  identity: delivery_point → meter (external key, strict merge)
  identity: deal → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [company, customer, broker, correspondent, delivery_point, deal]; relations [brokered]; events [call]; aggregate [inquiry by requested_by, offer by offered_by, agreed_terms by delivery_point, commitment by owed_by, commitment by owed_to]
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread_id`, document_id ← `message_id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted
```
Reading the facets against the answers:
- `customer` / `broker` → canonical name: **wrong** against Identity (a customer is its company,
  known by domain). Fix: declared `domain` attribute + `identity = ["domain"]`, fallback `name`.
- Tension used the default comparability (subject + clock), so only claims at the same document
  date would be compared; a confirm that contradicts the previous day's recap would be silently
  folded as supersession. **Wrong** against Derivation. Fix: `tension.same = ["subject"]`.
- clock, sources, correspondent/company/delivery_point identity, deal RESOLVE, document stamps:
  match the answers.

### Run 2 — after the two fixes
```
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over agreed_terms, status_report — cross-document declared corpora select the embedding net; the classifier judges each pair
  source: company ← document fields from, to, cc (domain: domain) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: company → domain (external key, strict merge)
  identity: customer → domain (external key, strict merge)
  identity: broker → domain (external key, strict merge)
  source: correspondent ← document fields from, to, cc (email: address, name: display_name) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: correspondent → email (external key, strict merge)
  identity: delivery_point → meter (external key, strict merge)
  identity: deal → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [company, customer, broker, correspondent, delivery_point, deal]; relations [brokered]; events [call]; aggregate [inquiry by requested_by, offer by offered_by, agreed_terms by delivery_point, commitment by owed_by, commitment by owed_to]
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread_id`, document_id ← `message_id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted
```
All derived facets now match the answers. (The tension line does not echo `same`; the key is
accepted.)

## Summary

Types and identity:
| type | kind | identity |
|---|---|---|
| company | entity (from headers) | email domain, strict |
| customer, broker | entity, role_of company | domain, strict; fallback name |
| correspondent | entity (from headers) | email address, strict |
| delivery_point | entity | meter; fallback name + pipeline |
| deal | entity | deal number; else RESOLVE: same counterparty, direction, point, overlapping period |
| deal_stage | state of deal | — |
| brokered | relation broker → deal | — |
| call | event (marketer, counterpart) | — |
| inquiry | claim, directive/request, about deal | — |
| offer | claim, commissive, about deal | supersedes on document date |
| agreed_terms | claim, commissive, about deal, graded | supersedes on document date; tension |
| status_report | claim, assertive, about deal | supersedes on document date; tension |
| commitment | claim, commissive, about deal | fallback subject + owed_by + owed_to + action |
| requirement | claim, directive, about company | supersedes on `valid` |

Least-sure assumptions:
1. Email extractor metadata key names (`date`, `thread_id`, `message_id`, `from`, `to`, `cc`). The validator
   does not check them; if they are wrong, the sources and stamps come up empty (counted in
   `resolution_failures.json`).
2. Commitments: "what I still owe" depends on open/done being read from later mail and merged by the
   descriptive identity fallback. No supersession or fold closes a commitment, so done items could stay open.
3. Superseding `agreed_terms` by deal assumes each recap restates the full terms. A message that restates
   only the price would retire an earlier full set.
4. Company = email domain, so brokers and customers who write from webmail, or deals talked about only in
   prose, rely on model extraction alone. Newsletter senders also become companies.
5. Deal identity without a number is a judgement call, and it is the hardest part of this corpus.
