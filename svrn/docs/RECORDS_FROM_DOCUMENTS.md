# Typed records from your documents

Most business software is a set of records kept up to date by hand: a CRM's deals, a tracker's cases, a
register of the rules in force, a lettings book, a fleet log. The facts those records hold are usually
already written down, scattered across mail, comments, minutes and reports. This page shows you how to get
the records back out of the documents.

You describe the records you want as types in a recipe. Sovereign then:

- reads your documents with a model;
- decides which mentions are the same record;
- folds each record's fields from the statements that make them;
- gives you the records, with every value tied to the words in the document that state it.

There is no training step, no labelled examples and no code to write. The declaration is the whole program,
and the default commands run it.

## What it does, measured

These are the shipping bars (`research/ontology-apps/ship-prereg.md`), each scored against hand-labelled
answers on DeepSeek Flash, one read per corpus:

| Problem | Corpus | Result |
|---|---|---|
| Deals in a mailbox | five synthetic company mailboxes, 93 deals | 80 deals served right (one read of each whole mailbox at once by the same model: 68); 306 of 328 messages attached to the right deal |
| Cases in an issue tracker | astral-sh/uv, 554 issues, comments and events, 44 cases | 23 and 26 cases at their right current state, over two reads of identical code |
| Rules in force in a register | 42 documents of rule proposals, approvals and supersessions, 35 rules | 19 rules at the right standing, the same as one read of the whole register at once |
| Facts in a novel | a novel, 42 questions about its people and events | 34 answered right from the records |

The one-read-at-once comparison only exists where a corpus fits in one prompt. The layer reads in windows,
so it keeps going where that stops.

## A first run

You need a running daemon with a chat model ([`START_THE_DAEMON.md`](../../docs/START_THE_DAEMON.md)).
The fastest way to see the layer work is the issue-tracker fixture in this repository,
[`ingest/crates/sovereign-enrichment-build/tests/fixtures/layer/issues/`](../../ingest/crates/sovereign-enrichment-build/tests/fixtures/layer/issues/).
It holds two threads from a calendar app's tracker, one bug and one feature request, as JSON lines. Each
line carries `id`, `thread`, `kind`, `author`, `author_association`, `created_at`, `title` and `body`.

The recipe acquires the file and extracts `body` as each document's text. Then it declares three types:

```toml
[enrichment.ontology]
version = 1

[[enrichment.ontology.types]]
name = "person"
kind = "entity"
attributes = [{ name = "login", type = "text" }]
identity = ["login"]
source = { metadata = ["author"], attributes = { login = "value" } }

[[enrichment.ontology.types]]
name = "case"
kind = "entity"
description = "one underlying problem or request, however many issues report it"
identity_criterion = "Two mentions are one case when they are about the same underlying problem or request, however many issues report it and whoever writes."
identity_bar = 0.9
attributes = [
  { name = "reporter", type = "ref", of = "person", derived = "reporter_of_case" },
]

[[enrichment.ontology.types]]
name = "case_state"
kind = "claim"
description = "a statement that shows where a case stands"
force = "assertive"
subject = "case"
attributes = [
  { name = "state", type = "text", values = ["reported", "confirmed", "resolved", "declined"] },
  { name = "app_version", type = "text", description = "the app version the statement names" },
]

[enrichment.ontology.change]
document = { date = "created_at", thread = "thread", id = "id" }

[[enrichment.ontology.sets]]
id = "maintainer_post"
type = "document"
where = { author_association = ["MEMBER", "OWNER", "COLLABORATOR"] }

[[enrichment.ontology.folds]]
id = "reporter_of_case"
by = "earliest"
from = ["^subject / document [!maintainer_post] / author"]
```

Each of the three types gets its records a different way:

- A `person` comes straight from a document field. `source` names the `author` field and `identity` names
  the key, so two posts by `mira` are one person by construction, and no model is asked anything.
- A `case` has no such key, because two issues can report one problem. Its `identity_criterion` is the
  rule, in plain words. RESOLVE, the step that decides identity, asks the model whether mentions meet
  that rule, and `identity_bar` sets how sure that decision must be.
- A `case_state` is a statement about a case. `force = "assertive"` says it asserts something, and
  `subject` says what it is about. Its `state` must be one of four closed values, so the model chooses a
  value rather than inventing a label. Each statement keeps the exact words that make it.

Two more blocks finish the declaration:

- `change.document` tells the layer which fields date and thread each document. That gives it a clock,
  so `earliest` and `latest` mean something.
- The fold `reporter_of_case` defines the reporter as the author of the earliest document about the case
  that is not a maintainer's post. That is a rule over the records the recipe already has, so it needs no
  model.

Before you build anything, ask the validator what it understood:

```sh
svrn recipe validate recipe.toml
```

It exits 0 when the declaration is well formed, and it prints what the declaration derives. For this
recipe it says `identity: person → login (external key, strict merge)` and `identity: case → RESOLVE
under its identity_criterion`, which is how each type gets its records. It also prints
``derived: case.reporter ← fold `reporter_of_case` by `earliest` ``, naming the field a fold will write.
Read those lines before you spend a model call. A type RESOLVE decides needs at least one claim kind
whose `subject` it is, and the validator says so when one is missing.

Then the default commands. The same four run every recipe, and none of them names a phase or a reader:

```sh
svrn corpus install recipe.toml --wait
svrn enrich init layer-issues --from-corpus layer-issues
svrn enrich extract layer-issues --full
svrn enrich resolve layer-issues
```

`extract` reads the documents in windows and records each statement it finds with its quote. `resolve`
then does four things in order:

1. It stamps each statement with its document's date and thread.
2. It runs RESOLVE for each type that has an identity criterion.
3. It writes each fold.
4. It prints one line per part, so you can see how many records each type got and how each was decided.

Ask for the records:

```sh
svrn enrich atlas-query layer-issues --typed '{"target_type": "case"}' --json
```

Each row is one record. In this run, the bug's row reads, in part:

```json
{
  "name": "doubled calendar reminders after time change",
  "type": "case",
  "evidence_chunks": ["1"],
  "refs": [{ "attribute": "reporter", "type": "person", "name": "mira", "key": "mira" }],
  "referenced_by": [{ "attribute": "case_state.subject", "id": "claim-0001", "type": "case_state" }],
  "folds": { "reporter": { "outcome": "decided", "documents": ["comment-101-4", "issue-101"] } }
}
```

A row shows where the record came from, field by field:

- `evidence_chunks` names the indexed passage the record opens on.
- `refs` shows what the record points at.
- `referenced_by` lists the statements about it.
- `folds` shows how each derived field was decided and from which documents. A fold the documents
  contradict reads `conflict`, never a guess.

Query a statement kind and each row carries its quote and its document. Every field the reader filled
records the words that state it.

This sequence, with three questions whose answers are written down before any run, is the
`layer-default-path` journey in the CLI contract (`svrn contract map`). It runs against the live daemon,
so it is exercised rather than only described. The fixture's `expected.toml` pins every row: record ids,
values and the quote each one cites.

## Designing a declaration for your problem

Work through five decisions in order. Each one maps to keys the field reference lists in full (the
"`version = 1`" section of [`SCHEMA.md`](../../ingest/crates/sovereign-recipes/SCHEMA.md)).

**1. What is a record?** It is the thing the owner keeps a row for: a deal, a case, a tenancy, a van, a
rule. Declare it as a type with `kind = "entity"`, or `kind = "event"` when it is something that happened
(an incident, a fire, a garage job). A type's `description` is read by the model, so write it the way you
would brief a new assistant.

**2. How do you know two mentions are one record?** You have two answers. Use the cheaper one wherever it
is true.

- When a document field names the record exactly (an author's handle, an email address, a domain), use
  `source` and `identity`. The record is made from metadata, with no model call, and two mentions are one
  record exactly when their keys match.
- Otherwise, write an `identity_criterion`: the rule a careful person would apply, including the cases
  that look alike but are different. RESOLVE applies it with the model. `identity_necessary` names
  closed-value fields that must agree before two mentions can be one.

**3. What statements change it?** Declare each kind of statement as a `kind = "claim"` type with a
`force` and a `subject`. A status update, a price quote, an approval, a notice to quit and a defect
report are each a claim kind. Give each claim kind the fields it states, and close a field's `values`
whenever the owner's answer is one of a known set.

**4. How does each field get its value?** A record's field is folded from the statements about it, and
`by` says how:

| `by` | The record holds |
|---|---|
| `agree` | the value every reading gives, or a conflict |
| `most` | the value the most documents give; a tie decides nothing |
| `first` | the first input, in `from` order, that gives exactly one value |
| `earliest`, `latest` | the value of the earliest or latest document, by the declared clock |
| `all` | every value read |

A field of a RESOLVE-decided type declared without `by` folds by `agree`. For a lifecycle (asked, quoted,
confirmed, cancelled), declare a `protocol` fold. Its rules name which claim kind moves the record into
which state. A rule's `from` lists the states it may move from. Reports are replayed in date order, and
a report that no rule admits leaves the record where it was. The mailbox fixture
([`layer/mailraw/recipe.toml`](../../ingest/crates/sovereign-enrichment-build/tests/fixtures/layer/mailraw/recipe.toml))
folds an order's stage that way. `earliest`, `latest` and dated protocol moves need `change.document`.

**5. How do records point at each other?** A `type = "ref"` field names another type with `of`. A
reference a document makes lands on the record its own words name. A reference whose value exactly
equals a metadata-sourced record's key (a handle, a domain) lands on that record even when another
document is where it was projected. A reference to a type nothing else records is decided by RESOLVE by
name, so `of = "person"` works without declaring a person source. `sets` and `paths` name groups of
documents or records (a maintainer's posts, the companies that are not yours) for folds to walk, as
`reporter_of_case` does above.

When you have a draft, run `svrn recipe validate` and read every `identity:` and `derived:` line against
your intent. A field you expect to be filled that has neither a `derived:` line nor a claim field the reader
reads will stay empty. That is the commonest authoring miss.

## Asking for records

A typed query names a target type and narrows it. Pass it inline with `--typed` or from a file with
`--typed-file`:

```json
{"target_type": "case_state",
 "filters":   [{"attribute": "state", "op": "eq", "value": "confirmed"},
               {"attribute": "app_version", "op": "contains", "value": "2.3"}],
 "relations": [{"relation": "case_state.subject", "other_type": "case",
                "where": {"relations": [{"relation": "case.reporter", "other_type": "person",
                                         "other_name": "mira"}]}}]}
```

That asks: which statements confirm a case on version 2.3, about a case mira reported. The parts are:

- **Filters** compare one attribute with `eq`, `lt`, `gt` or `contains`. A time can be a date, a month or
  a year. A filter can also compare two attributes, either on the same record (`"value": {"attribute":
  "reorder_level"}`) or, inside a `where`, against the target (`"value": {"target": "order_total"}`).
- **Relations** follow a declared relation, or a ref spelled `<type>.<field>`, to another record. That
  record can be named, or described by its own `where`, two levels deep.
- **Aggregates** shape the answer:
  - `count` gives how many records match.
  - `argmax` and `argmin` give the record with the largest or smallest time or quantity, or the most or
    fewest linked records of a type.
  - `tally` counts records per value of a field.
  - `sum` totals a quantity per value of `group_by`. A ref as the group totals per linked record, for
    example per supplier.

The grammar the model is shown is
[`typed_query_grammar.md`](../../ingest/crates/understanding-atlas/src/atlas_traversal/typed_query_grammar.md).

Every name in a query is checked against your declaration before it runs, and a name the declaration does
not hold is refused rather than ignored. A record that cannot be judged (a sum over a value with no
number, a comparison with an absent field) is listed as unjudged by id, never counted as zero or as false.

`svrn enrich atlas-query <corpus> "<question>"` takes a plain question instead, and turns it into one of
these shapes. An agent connected over MCP looks up the same records by id through `atoms_lookup`.

## Patterns beyond the examples

Before any of the census primitives were built, twelve problems from outside the tuning examples were
each given to a blind author. The author saw only the owner's description, the field reference and the
validator, and wrote a recipe with no help. Judged against the owner's numbered needs, 64 of 75 now run.
Every use case's record types get records. The recipes are in
[`research/ontology-apps/census/authors/`](../../research/ontology-apps/census/authors/), and they show
the range one language covers:

| Problem | Records | Statements that change them | Needs that run |
|---|---|---|---|
| Recruiting | candidate, opening, application; organisations and correspondents from headers | stage moves, interview feedback, offers and responses | 6 of 7 |
| Contracts | contract, counterparty | term dates, renewal and price terms, amendments, notices | 5 of 7 |
| A policy register | policy, clause | rules, decisions to approve or retire, clause changes | 6 of 6 |
| Purchasing | purchase; suppliers and contacts from headers | requests, approvals, orders, invoices, payments | 3 of 6 |
| Incidents | incident (an event), service, action item | severity calls, milestones, roles, timeline, root cause | 5 of 7 |
| Tasks | task, project, milestone; members from handles | commitments, status updates, dependencies, reassignments | 6 of 6 |
| Spare parts | part, warehouse, technician, supplier | receipts, transfers, issues, returns, stock counts | 5 of 6 |
| Lettings | flat, tenant, tenancy, maintenance job | terms, rent changes, renewals, notices to quit | 6 of 6 |
| Grants | application, report, payment; funders from headers | requests, decisions, conditions, report submissions | 6 of 6 |
| A van fleet | van, garage, defect, garage job; drivers from headers | mileage readings, allocations, defect updates, service dues | 6 of 6 |
| Expenses | expense claim; employees from headers | submissions, approvals, payment notices | 5 of 6 |
| A coliving house | housemate, house rule, chore, purchase | proposals, stances, decisions, rota assignments | 5 of 6 |

Most owner needs reduce to the same few moves. Make a record per thing the owner keeps a row for. Read
each kind of statement as a claim with closed values where you can. Fold the current state by `latest`
or a protocol. List or total with a typed query. The needs that do not run are the limits below.

## Cost and repeatability

Every model answer is recorded in `~/.svrnmesh/enrichment/<corpus>/answers.jsonl`, keyed by the
question's content and stamped with the model that answered it. Run `extract` and `resolve` again with
`--asker replay` and they answer from that file without calling the daemon, and write the same records.

On DeepSeek Flash, a 554-document issue tracker took about 20 minutes and about 250k output tokens, most
of it the model's reasoning. A 42-document register took about 6 minutes and 65k.

Reads vary. RESOLVE's identity decisions and the reader's grouping are judgements, so two fresh reads of
the same corpus with the same code can disagree. On the issue tracker two reads gave 23 and 26 cases
right. Compare recipes or code changes on recorded answers, or over more than one read, before trusting
a small difference.

## What it does not do yet

These are measured gaps. Each one names the need that showed it:

- **Values computed by arithmetic.** A notice date that is an end date minus a notice period, or a fee
  that is a percentage of a salary, is held only when a document states it.
- **A set selected by its state on a date.** "Which contracts are in force today" works one named record
  at a time (`argmax` with a date bound), not as one list.
- **Counting linked records in a filter.** "Requests that lack two approvals" does not run.
- **Comparing two statements joined only through a shared subject**, such as an invoice and its purchase
  order that are both about one purchase. A filter reaches one relation deep.
- **The document's own date as a filter.** It dates statements, but a query filters only declared
  attributes, so declare a date field when you need one.
- **A reference to an event record.** A `ref` lands on entities only.
- **Netting a later statement against an earlier one.** A `settled` debt does not cancel an earlier
  `owed` one. A sum adds a corrected value alongside the value it corrects.
- **Numbers written with thousands separators.** "1,250.00" reads as two numbers, so a sum reports it as
  unjudged and a `lt`/`gt` filter misjudges it.
- **Sums and attribute comparisons from a plain question.** These forms run through `--typed` and
  `--typed-file`, not yet through a question the classifier turns into a query.
- **A withdrawal by description.** "The twelve superseded proposals" names no record, so nothing is
  withdrawn.

If your problem needs one of these, the layer will still give you the records. Do the last step outside
it, from the cited rows.

## Where to read more

- [`ENRICH_A_CORPUS.md`](./ENRICH_A_CORPUS.md) is the general enrichment journey this page builds on.
- [`SCHEMA.md`](../../ingest/crates/sovereign-recipes/SCHEMA.md) is every recipe key, generated from the
  code. Its "`version = 1`" section is the full declaration guide.
- [`ingest/crates/corpus-engine/ENRICHMENT.md`](../../ingest/crates/corpus-engine/ENRICHMENT.md) is the
  engineering material behind the layer.
