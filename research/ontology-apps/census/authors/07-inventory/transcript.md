# Transcript: spare parts at three warehouses

The partner was not available. Each question from the recipe-author skill was put
and then answered from `DESCRIPTION.md`, quoted, or failing that from general
knowledge of spare-parts inventory practice, marked `assumption`.

Templates consulted: `sovereign recipe new --ontology list` named ten templates.
For their shape only, I scaffolded `materials-lab`, `product-support`,
`due-diligence`, `contracts`, `governance` and `engineering-org`. None of them is an
inventory, so the declaration below is written fresh. I deleted the scaffolds after
reading them.

## 0. Source shape (follow-up the schema needs: acquire and extract)

**Q:** What form is the material in: files, a mailbox, an API? Which extractor
reads it?

**A:** It is a mailbox of warehouse emails, with the monthly stock-take sheets as
text. The recipe uses `acquire.type = "local_file"` with `path = "SOURCE_PATH"`,
`extract.type = "email"` (one document per message, carrying `date`, `thread_id`,
`message_id` and `from`), and `chunk.type = "paragraph"`.

**Source:** description: "The material is the warehouse emails (receipts, transfers
between warehouses, issues to technicians) and the monthly stock-take sheets (as
text)." Where the sheets sit is an *assumption*. I took it that they come through the
same mailbox, either pasted into a warehouse email or attached as plain text. If they
are separate `.txt` files, the email extractor will not read them, and they need a
`plaintext` recipe of their own.

## 1. Shape

**Q:** "What is this material about, and what would you want to know about each kind
of thing? Is any of them a kind of another, or a part something plays? Do you
already have any of it in a table?"

**A:**
- `part`: part number, description, unit of measure. Source: description, "Every
  part, with its part number, description and unit of measure."
- `warehouse`: there are three, and each holds stock. A warehouse is a kind of place
  (`specializes = "place"`). Source: description, "across three warehouses" and "How
  many of each part each warehouse holds". The `place` link is an *assumption*.
- `technician`: a part a person plays (`role_of = "person"`), receiving issued parts.
  Source: description, "issues to technicians" and "Which technician holds which
  issued parts". The `role_of` is an *assumption*.
- `supplier`: a part an organisation plays (`role_of = "institution"`), sending
  receipts. Source: description, "a receipt from a supplier". The `role_of` is an
  *assumption*.
- Unit of measure is a closed set of ten (each, pair, set, kit, pack, box, roll,
  metre, litre, kilogram), so that "ea", "pcs" and "EA" all fold to `each`.
  *Assumption.*
- Tables: there is no table file to declare as a `source`, because the sheets are
  text inside the emails. *Assumption.*

## 2. Assertion

**Q:** "What do the sources say about those things: stating, requiring, deciding,
asking? About what? What does each statement carry: amounts, dates, named parties,
one of a fixed set of options? How do you tell strong evidence from weak? Who is
speaking, and are they part of the subject? What must it never do?"

**A:**
- The sources mostly *state* (assertive), and every statement is about one part:
  - `receipt`: quantity, receiving warehouse, supplier, date.
  - `transfer`: quantity, from-warehouse, to-warehouse, date.
  - `issue`: quantity, issuing warehouse, technician, date.
  - `stock_count`: counted quantity, warehouse, count date.

  Source: description, "Every movement: a receipt from a supplier, a transfer between
  warehouses, an issue to a technician, with quantity and date." and "as of the last
  stock-take".
- Each movement also carries a `reference`: the purchase order, delivery note or
  goods-received number, the transfer note, or the issue slip or job number.
  *Assumption.* It lets the two reports of one transfer be paired, and a movement
  that is restated be recognised as the same movement.
- `transfer.leg` is `dispatched` or `arrived`. *Assumption.* The sending and the
  receiving warehouse each report the transfer, stock in transit belongs to neither,
  and an arrival short of what was dispatched is a real discrepancy.
- `return` covers a technician bringing parts back, with `condition` unused or
  faulty. *Assumption.* Returns change what a technician holds (question 5), and
  faulty parts do not go back into usable stock. The description lists only three
  movements.
- `stock_count.expected` (the book figure) and `stock_count.reorder_level` are read
  when a sheet prints them. *Assumption.* The reorder level itself comes from the
  description ("Parts below their reorder level"). I put it on the count line for two
  reasons: a three-warehouse operation often sets it per warehouse, and on the line
  the count and the level sit side by side.
- The sources also *ask*: `request` (directive, `deontic = ["request"]`) is someone
  asking for parts to be issued, transferred or ordered. *Assumption.* I declared it
  so that an ask is never read as a movement.
- To tell strong evidence from weak, `stock_count` is graded `stock-take`, then
  `spot-check`, then `estimate`. *Assumption.* Movements are not graded, because the
  `reference` carries their strength.
- The speakers are storekeepers, technicians, suppliers and the manager. Only the
  technicians are subject matter, as the people who receive parts. *Assumption.* I
  first declared a `voices` block for this and removed it after run 1.
- It must never present a quantity worked out from movements as if it had been
  counted (`must_not`). *Assumption.* This is honesty in answers, because the second
  half of question 2 has to be computed.

## 3. Identity

**Q:** "How do you know two mentions are the same thing? Is there an ID?"

**A:**
- `part`: our part number, `identity = ["part_number"]`. Source: description, "with
  its part number". *Assumptions:* the comparison ignores case, spaces and dashes; a
  supplier's or manufacturer's number is not ours; and the description is a fallback,
  trusted only when nothing in it differs (a 24 V relay and a 12 V relay are two
  parts). All of this is written into `identity_criterion`.
- `warehouse`, `technician` and `supplier`: by name, which is the canonical-name
  default. *Assumption:* no employee, van or supplier codes are known.
- Movements are not records. Each report is one statement about a part, and the
  `reference` pairs the reports of one movement. *Assumption, and a design choice:*
  it keeps the per-part ledger under the one strict part-number key, rather than
  asking RESOLVE to match movements on a weak criterion.

## 4. Change

**Q:** "When does a later statement replace an earlier one, and from when? Does
anything move through stages over time: what are the stages, which statements move
it from one to the next, who may make each move, does a move take effect when it is
reported or at a time it names, and how is a mistaken move corrected or a finished
thing reopened? Which fields of each document give its date and the conversation it
belongs to?"

**A:**
- A later count replaces an earlier one for the same part *at the same warehouse*.
  Source: description, "as of the last stock-take". Supersession in this language is
  per subject, though, and the subject is the part. Declaring
  `supersedes = { stock_count = ... }` would let a count at one warehouse retire
  another warehouse's count of the same part, so I left it undeclared. The latest
  count per warehouse is read from `counted_on`. *Assumption and design choice.*
- Movements accumulate and never replace one another. *Assumption.*
- The one stage I found is a transfer going from dispatched to arrived. It is held as
  `leg`, not as a protocol fold, because a transfer is not a record that RESOLVE
  decides. *Assumption.*
- The document fields are the email extractor's `date`, `thread_id`, `message_id`
  and `from`, taken from the extractor's field reference in SCHEMA.md. I have not
  checked them with `--corpus`, because no material is available.

## 5. Derivation

**Q:** "What should it notice that no single document says: contradictions,
patterns, larger structures? What looks like a contradiction but isn't?"

**A:**
- A count that disagrees with what the movements imply. Source: description,
  question 4. The language has no summing fold, so the implied balance is assembled
  at answer time from the cited movement statements. Where a sheet prints its book
  figure, `stock_count.expected` holds it. Separately, `tension` between
  `stock_count`s with `same = ["subject", "warehouse", "counted_on"]` catches two
  counts of one part at one warehouse, for one count date, that disagree (a recount
  or a correction).
- A part below its reorder level. Source: description, question 6. This is the
  comparison of `counted` with `reorder_level` on the same line.
- What each technician holds: issues minus returns. Source: description, question 5.
- What only looks like a conflict (`not_conflicts`): counts at two different
  warehouses, and counts on different dates, where the movements in between explain
  the difference. *Assumption.*
- Interpretive configurations are turned off (`derive.configurations = false`),
  because there is nothing interpretive in a stock record. *Assumption.*

## 6. Questions

**Q:** "Once it is read, what will you ask of it? Name a few questions you would put
to it."

**A:** These are the description's six questions, each walked to where its answer
lands. Source: description (the numbered list).

1. "Every part, with its part number, description and unit of measure" lands on the
   `part` record: `part_number` is the identity key, and `description` and
   `unit_of_measure` are folded `by = "most"` over every statement that names the
   part. This is the enumerate shape.
2. "How many of each part each warehouse holds":
   - As of the last stock-take, it is `stock_count.counted`, the latest
     `counted_on` per part and `warehouse` (aggregate shape "stock_count by
     warehouse").
   - As of the latest movement, it is that count plus, since then:
     - receipts in;
     - transfers arrived in, minus transfers dispatched out;
     - issues out;
     - unused returns.

     That arithmetic is done at answer time.
3. "Every movement" lands on the `receipt`, `transfer`, `issue` and `return`
   statements, each with `quantity` and `date`, plus the `document_date` stamp.
4. "Where a stock-take count disagreed with what the movements imply" lands on
   `stock_count.expected` against `counted` where the sheet prints a book figure.
   Otherwise it is the reconciliation in question 2. Two counts that disagree for
   the same date are the declared tension.
5. "Which technician holds which issued parts" lands on `issue.technician` and
   `return.technician` (aggregate shapes "issue by technician" and "return by
   technician").
6. "Parts below their reorder level" lands on `stock_count.counted` against
   `stock_count.reorder_level`.

Every question lands on something declared. Questions 2b, 4 and 6 need arithmetic or
a comparison that the declaration language does not derive, so they are assembled
from cited statements when a question is answered.

## Validate runs

### Run 1: `sovereign recipe validate recipe.toml` (first draft, with a `voices` block)

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over stock_count — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: part → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: warehouse → canonical name (default; declare `identity = [...]` for an external key)
  identity: technician → canonical name (default; declare `identity = [...]` for an external key)
  identity: supplier → canonical name (default; declare `identity = [...]` for an external key)
  question shapes: enumerate [part, warehouse, technician, supplier]; relations [(none)]; events [(none)]; aggregate [receipt by warehouse, receipt by supplier, transfer by from_warehouse, transfer by to_warehouse, issue by warehouse, issue by technician, return by warehouse, return by technician, stock_count by warehouse, request by warehouse, request by technician]
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread_id`, document_id ← `message_id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted; author ← `from`, read by the reader only, among each document's declared facts
  ⚠  the document reader reads statement kinds, the types they are about and every type RESOLVE decides, so it makes no record of `warehouse` (entity), `technician` (entity), `supplier` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
  ⚠  `voices` (attributed_to, self) are applied by the general extractor only; the document reader attributes no statement to a speaker.
exit=0
```

Reading the facets against my answers:
- **Right:** the clock (`document_date`, the email's date), part identity (RESOLVE
  under the part-number criterion), the stamps (`date`, `thread_id`, `message_id`,
  `from`), and the aggregate shapes (by warehouse, by technician, by supplier) that
  questions 2, 3 and 5 need.
- **Wrong:** the `voices` block does nothing, because the document reader reads this
  declaration (every claim's subject is the RESOLVE-decided `part`). I removed it.
- **Wrong:** no record is made of warehouse, technician or supplier. Their names
  survive only as the ref value on each statement, so "enumerate [warehouse,
  technician, supplier]" will come back empty.

### Run 2: `sovereign recipe validate run2-probe.toml` (probe: run 1 plus an `identity_criterion` on `technician`)

```
Validating recipe: run2-probe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over stock_count — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: part → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: warehouse → canonical name (default; declare `identity = [...]` for an external key)
  identity: technician → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: supplier → canonical name (default; declare `identity = [...]` for an external key)
  question shapes: enumerate [part, warehouse, technician, supplier]; relations [(none)]; events [(none)]; aggregate [receipt by warehouse, receipt by supplier, transfer by from_warehouse, transfer by to_warehouse, issue by warehouse, issue by technician, return by warehouse, return by technician, stock_count by warehouse, request by warehouse, request by technician]
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread_id`, document_id ← `message_id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted; author ← `from`, read by the reader only, among each document's declared facts
  ⚠  the document reader reads statement kinds, the types they are about and every type RESOLVE decides, so it makes no record of `warehouse` (entity), `supplier` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
  ⚠  `voices` (attributed_to, self) are applied by the general extractor only; the document reader attributes no statement to a speaker.
exit=0
```

With the criterion, technician moves to "RESOLVE … over the claims declaring it their
subject", but no claim does. v1.md says such a type makes `enrich extract` refuse the
build, and validate did not flag it, so this route trades a visible warning for a
possible build failure. I rejected it.

The other route was to make the technician (or the warehouse) the subject of `issue`
and `return`. That would take those statements off the strict part-number key that
questions 1-4 and 6 depend on, so I rejected it as well. The three types stay ref
targets, the same pattern the shipped templates use for `customer` and `method`. I
deleted the probe file.

### Run 3: `sovereign recipe validate recipe.toml` (final: `voices` removed)

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over stock_count — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: part → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: warehouse → canonical name (default; declare `identity = [...]` for an external key)
  identity: technician → canonical name (default; declare `identity = [...]` for an external key)
  identity: supplier → canonical name (default; declare `identity = [...]` for an external key)
  question shapes: enumerate [part, warehouse, technician, supplier]; relations [(none)]; events [(none)]; aggregate [receipt by warehouse, receipt by supplier, transfer by from_warehouse, transfer by to_warehouse, issue by warehouse, issue by technician, return by warehouse, return by technician, stock_count by warehouse, request by warehouse, request by technician]
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread_id`, document_id ← `message_id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted; author ← `from`, read by the reader only, among each document's declared facts
  ⚠  the document reader reads statement kinds, the types they are about and every type RESOLVE decides, so it makes no record of `warehouse` (entity), `technician` (entity), `supplier` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
exit=0
```

The remaining warning is accepted and recorded under the least-sure assumptions
below. Every other facet matches the answers above.

## Summary

**Types declared (10):**
- **Entities:**
  - `part`: identity `part_number`, strict, decided by RESOLVE under a criterion
    (ignoring case, spaces and dashes; supplier numbers not ours; description as a
    guarded fallback). Description and unit of measure are folded `by = "most"`.
  - `warehouse` (`specializes = "place"`), `technician` (`role_of = "person"`) and
    `supplier` (`role_of = "institution"`): canonical name by default. They are ref
    targets only, so no records are made.
- **Claims, all with subject `part`:**
  - Assertive: `receipt`, `transfer`, `issue`, `return` and `stock_count`. These
    carry no identity of their own. Each is one statement about a part, and the
    movements carry a `reference`.
  - Directive: `request` (`deontic = ["request"]`), with the same subject and no
    identity of its own.
- **Tension:** between `stock_count`s with the same part, warehouse and count date.
- **Not declared:** supersession (it would cross warehouses).

**Assumptions I am least sure of:**
1. **The stock-take sheets are in the mailbox.** I assumed they arrive as email
   bodies or plain-text attachments. If they are loose `.txt` files, the `email`
   extractor never sees them, and questions 1, 2, 4 and 6 lose their main source.
2. **Warehouses, technicians and suppliers get no records.** Their names stay as
   written on each statement, so question 5 (and question 2 by warehouse) groups on
   raw names; "Dave" and "D. Smith" stay apart. If the partner named the three
   warehouses, `warehouse` should become a closed `values` attribute on every
   statement kind, which is the structural fix for that half.
3. **Balances are worked out at answer time.** "As of the latest movement" (question
   2b) and "what the movements imply" (question 4) have no summing fold in the
   language, so they rely on arithmetic over cited statements unless the sheet prints
   `expected`.
4. **The reorder level is on the stock-take line, per warehouse.** If it is set per
   part and only mentioned in emails outside the sheets, it will not be read.
5. **Some kinds are added beyond the three movements the description names.** The
   `return` and `request` kinds, the `leg` and `condition` fields, the ten-value
   unit-of-measure set and the stock-count grades all come from general practice,
   not from the description.
