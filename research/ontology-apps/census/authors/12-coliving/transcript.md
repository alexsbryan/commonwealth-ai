# Interview transcript: the coliving house chat

The partner was not available. Each question from the recipe-author skill is answered from
`DESCRIPTION.md` where it speaks (source `description`, its words quoted) and otherwise from
general knowledge of shared houses and group-chat exports (source `assumption`).

## Turn 1: source shape (follow-up the schema needs before the ontology)

**Q. What is the material physically, and which extractor reads it?**

A. One text file: the house group chat as exported by a messaging app. The likely layout is one
line per message, `timestamp - sender: text`, with system notices ("X added Y", "<Media
omitted>") mixed in. Acquire it with `local_file` at `SOURCE_PATH`, extract with `plaintext`, and
chunk with `paragraph`. No extractor in the catalog is chat-aware, so I do not expect the
extractor to give per-message metadata (date, sender).

Source: description, "the house group chat (exported as text)". The line layout and the
extractor's lack of per-message fields are an assumption.

## Q1. Shape

**Q. What is this material about, and what would you want to know about each kind of thing? Is
any of them a kind of another, or a part something plays? Do you already have any of it in a
table?**

A. Four kinds of thing.

- **Housemates.** One of the twenty people who live in the house: a role a person plays
  (`role_of = "person"`), so a guest, the landlord or a plumber is not a housemate.
- **House rules.** Each standing arrangement the house runs by: quiet hours, labelling fridge
  food, washing up, guests.
- **Chores.** Each recurring task on the rota: bins, the kitchen deep clean, bathrooms.
- **Purchases.** Things one housemate bought for shared use. A purchase happened at a time, so
  it is declared as an event. What to know about it: what was bought, who paid, and when.

None of these is a kind of another. Nothing is already in a table: the chat is the only source.

Source: description, "a 20-person coliving house", "I am one of twenty people sharing a house
with a big shared kitchen", "propose and agree house rules, assign chores, and split shared
purchases", "Shared purchases: who paid". That nothing is already in a table, and the role
framing, are assumptions.

## Q2. Assertion

**Q. What do the sources say about those things: stating, requiring, deciding, asking? About
what? What does each statement carry: amounts, dates, named parties, one of a fixed set of
options? How do you tell strong evidence from weak? Who is speaking, and are they part of the
subject? What must it never do?**

A. Eight statement kinds.

| Statement | Force | About | What it carries |
|---|---|---|---|
| `proposal` | directive (require / forbid / permit) | a house rule | `proposer`, `wording`, `aim` (new_rule / amend / drop), `proposed_on` |
| `stance` | assertive | a house rule | `respondent`, `position` (supports / objects / wants_changes), `reason`, `said_on` |
| `decision` | declaration | a house rule | `outcome` (agreed / rejected / amended / dropped / withdrawn / deferred), `wording`, `matter` (closed set of 12), `decided_on`, `takes_effect`, `how` (poll / house_meeting / chat_consensus / no_objection / announcement) |
| `reminder` | directive (require / forbid / permit) | a house rule | `reminded_by`, `wording`, `reminded_on`. This is a rule restated as already in force: a reminder, or a complaint that it was broken. |
| `assignment` | declaration | a chore | `assignee`, `week` (a range), `basis` (rota / swap / volunteered / cover) |
| `completion` | assertive | a chore | `week`, `outcome` (done / not_done / partly_done / done_late), `done_by`, `reported_by` |
| `expense` | assertive | a purchase | `amount`, `currency` (closed ISO set plus `other`), `split` (whole_house / some_housemates / unequal), `share` |
| `debt` | assertive | the housemate who owes | `creditor`, `for_purchase`, `amount`, `currency`, `status` (owed / settled / waived) |

Strong versus weak evidence: there are no grades. Every statement is a housemate's own message,
and the force already separates what was proposed, what one person said, and what the house
decided.

Who speaks: the housemates. They are also the subject matter, since they pay, owe, object and
do chores, so no speaker role is excluded (`voices.not_entities` is not set).

What it must never do: "present a proposed or disputed rule as agreed", and "say a housemate
owes money the chat does not show them owing".

Source: description, for what the statements must carry: "when each was agreed"; "proposed but
not agreed, and who objected"; "amended … dropped"; "who has which chore in which week, and
whether it got done"; "who paid, how much and in what currency, and who owes whom". The rest is
assumption: the split into eight kinds, the forces, the closed value sets, `reminder`, `stance`
(which holds individual "+1" votes apart from house decisions, see validate run 4), and the two
`must_not` lines.

## Q3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**

A. There is no ID for anything, so each type carries an `identity_criterion` in words and
RESOLVE decides.

- **House rule.** The same standing arrangement for the same matter, under any wording or
  nickname ("the dishes rule"). Its proposal, stances, decisions, amendments, reminders and its
  dropping are all about the one rule; an amendment changes its terms but does not make a new
  rule. A separate exception, or a guests' rule beside a housemates' rule, is a different rule.
- **Chore.** The same recurring task under any name ("bins", "rubbish", "trash"). The same kind
  of task in a different area is a different chore, and one week's turn is not a separate
  chore.
- **Purchase.** One buying by one housemate on one occasion. The announcement, the receipt, the
  pay-me-back request and later reminders are all one purchase. Two shopping trips are two
  purchases, even for the same items or amount.
- **Housemate.** The same person under their posting name, first name, nickname or @-mention.
  Two housemates who share a first name are told apart by surname, initial or posting name.

Source: description, "A rule, a chore or a purchase mentioned several times in the chat is one
thing." That there is no ID, the wording of each criterion, and the housemate criterion are
assumptions.

## Q4. Change

**Q. When does a later statement replace an earlier one, and from when? Does anything move
through stages over time: what are the stages, which statements move it, who may make each move,
does a move take effect when it is reported or at a time it names, and how is a mistaken move
corrected or a finished thing reopened? Which fields of each document give its date and the
conversation it belongs to?**

A.

- **What replaces what.** A later house decision on a rule replaces the earlier one:
  `change.supersedes = { decision = "decided_on" }`.
- **Stages.** A rule moves through them: proposed, then agreed or rejected (or withdrawn or
  deferred), then amended any number of times, then dropped. The latest decision is the rule's
  standing now. "In force now" means the latest decision is `agreed` or `amended`. "Proposed but
  not agreed" means a rule with a proposal and no decision, or a latest decision of `rejected`,
  `withdrawn` or `deferred`.
- **Who may move it.** The house as a whole (a poll, a house meeting, nobody objecting). One
  housemate's vote is a `stance`, not a move.
- **When a move takes effect.** At the time it was made (`decided_on`). A later start date the
  decision names is kept in `takes_effect`, but it does not order decisions.
- **Corrections and reopening.** A mistaken report is corrected by a later decision statement,
  and a dropped rule is revived by a later `agreed`. Both work through the same supersession.
- **Chores and debts.** These are not superseded. Each assignment is a fact about its own week,
  and several debtors can owe for one purchase, so a later statement does not retire an earlier
  one.
- **Document fields.** None declared. The plain-text export is expected to give no per-message
  date or sender field, so each statement's time is read from its own line (`decided_on`,
  `said_on`, `week`, …) rather than stamped from metadata.

I did not declare a `by = "protocol"` fold for rule standing. It orders moves by
`change.document.date`, and without that field every move would stay pending.

Source: description, "How a rule changed when it was amended, and which rules were dropped",
"The house rules in force now, and when each was agreed". The stage model, supersession on
`decided_on`, the absence of document fields, and dropping the protocol fold are assumptions.
The document-field assumption is the first to check: run `svrn recipe validate recipe.toml
--corpus SOURCE_PATH`. If it lists a per-message date or sender field, declare
`change.document = { date = …, author = … }` and reconsider the protocol fold.

## Q5. Derivation

**Q. What should it notice that no single document says: contradictions, patterns, larger
structures? What looks like a contradiction but isn't?**

A. The thing to notice is two house decisions on the same matter that contradict each other. For
example, two rules in force about the fridge that cannot both be followed. This is declared as
`tension.between = ["decision"]` with `same = ["matter"]`, and `matter` is a closed set of 12
(kitchen, fridge_and_food, cleaning, bins_and_recycling, noise, guests, bathrooms, laundry,
common_spaces, shared_money, safety_and_access, other).

Things that look like conflicts but are not:

- an amendment and the version it replaced
- a one-off exception for a date or event, beside the standing rule
- a guests' rule beside a housemates' rule
- rules for different rooms, shelves or appliances
- a rule layered on another that it adds to

Two other things worth noticing are not declared, because only one tension block exists and they
are better done as arithmetic at answer time:

- a chore turn reported both done and not done
- debts that net out around a circle of housemates

Source: assumption. The description asks for no contradictions.

## Q6. Questions

**Q. Once it is read, what will you ask of it? Name a few questions you would put to it.**

A. These are the description's own six. Each one is walked to where its answer lands:

1. "The house rules in force now, and when each was agreed": `house_rule` records whose latest
   `decision.outcome` is agreed or amended. The date is that decision's `decided_on`, or the
   earliest `agreed` decision's date.
2. "Rules that were proposed but not agreed, and who objected": `proposal` with no `agreed`
   decision, or a latest decision of rejected, withdrawn or deferred. The objectors are
   `stance.respondent` where `position = objects`.
3. "How a rule changed when it was amended, and which rules were dropped": the rule's decisions
   in `decided_on` order, comparing `wording`, plus those with `outcome = dropped`.
4. "The chore rota: who has which chore in which week, and whether it got done": `assignment`
   (chore, `assignee`, `week`, `basis`) joined with `completion` (chore, `week`, `outcome`,
   `done_by`).
5. "Shared purchases: who paid, how much and in what currency, and who owes whom": who paid is
   the `purchase` record's `payer` (held when its statements agree) and `items`. How much and in
   what currency is `expense.amount`, `currency`, `split` and `share`. Who owes whom is `debt`
   (subject = debtor, `creditor`, `for_purchase`, `amount`, `currency`, `status`).
6. "A rule, a chore or a purchase mentioned several times in the chat is one thing": RESOLVE
   under each type's `identity_criterion` (Q3).

Every question lands in a declared type, attribute or statement kind. No question needed a type
named after it.

Source: description, all six quoted above.

## Validate runs

Each run: `cd /Users/alexsbryan/census-author-20261020/12-coliving && sovereign recipe validate
recipe.toml`.

### Run 1: failed

```
Validating recipe: recipe.toml
✗ Validation failed:
  - tension.same names `matter`, which is neither `subject`, `clock`, nor an attribute declared on decision (attributes: decided_on, how, outcome, takes_effect, wording)
```

`matter` had been declared on the `house_rule` entity. The comparability field has to be an
attribute of the claim in `tension.between`. **Fix:** I moved `matter` onto `decision`, the
statement that fixes what a rule governs, and removed it from `house_rule`.

### Run 2: passed, with a warning

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over decision — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: housemate → canonical name (default; declare `identity = [...]` for an external key)
  identity: house_rule → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: chore → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: purchase → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [housemate, house_rule, chore]; relations [(none)]; events [purchase]; aggregate [proposal by proposer, objection by objector, reminder by reminded_by, assignment by assignee, completion by done_by, completion by reported_by, debt by debtor, debt by creditor]
  ⚠  the document reader reads statement kinds, the types they are about and every type RESOLVE decides, so it makes no record of `housemate` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
```

Read against my answers, this was wrong. Q2 and Q5 need housemate records: who proposed, who
objected, who has a chore, who paid, who owes. As declared, every "who" field would point at
nothing.

**Fix:** I gave `housemate` an `identity_criterion` and made it the subject of a statement kind.
`debt` is now about the debtor ("Ben owes me 4.70 for the bin bags"). The creditor is a ref, and
the purchase is linked by `for_purchase`.

### Run 3: passed cleanly

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over decision — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: housemate → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: house_rule → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: chore → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: purchase → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [housemate, house_rule, chore]; relations [(none)]; events [purchase]; aggregate [proposal by proposer, objection by objector, reminder by reminded_by, assignment by assignee, completion by done_by, completion by reported_by, debt by creditor, debt by for_purchase]
```

Reading the aggregate list against Q2 showed a gap. In a twenty-person chat, most replies to a
proposal are individual "+1 / agreed!" votes. With only `objection` and `decision` declared,
those votes had nowhere to go but `decision`, which would read one person's vote as the house
deciding.

**Fix:** I replaced `objection` with `stance` (`respondent`, `position` = supports / objects /
wants_changes, `reason`, `said_on`). I also said in `decision`'s description that one
housemate's vote is a stance.

### Run 4: passed cleanly

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over decision — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: housemate → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: house_rule → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: chore → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: purchase → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [housemate, house_rule, chore]; relations [(none)]; events [purchase]; aggregate [proposal by proposer, stance by respondent, reminder by reminded_by, assignment by assignee, completion by done_by, completion by reported_by, debt by creditor, debt by for_purchase]
```

The reader points at the words of a `time` field; it does not compute one. So the `week` and
`bought_on` fallbacks were changed from "the week it was posted in" to "the date the message was
posted", which is something the line actually shows. These are description-only edits.

### Run 5 (final): passed cleanly

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over decision — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: housemate → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: house_rule → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: chore → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: purchase → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [housemate, house_rule, chore]; relations [(none)]; events [purchase]; aggregate [proposal by proposer, stance by respondent, reminder by reminded_by, assignment by assignee, completion by done_by, completion by reported_by, debt by creditor, debt by for_purchase]
```

### The final facets, read against the answers

- **Clock: `document_date` (the default).** None of the declarations relies on it. The one
  supersession names its own field (`decision` on `decided_on`), `tension.same` does not use
  `clock`, and `payer`'s `by = "agree"` needs no order. It stays the default and would hold
  nothing until `--corpus` shows a date field. I left it alone rather than declare `narrative`:
  a single chronological log would make that order correct, but the supersession map cannot
  name it.
- **Tension selector: over `decision`, with the classifier judging pairs.** This matches Q5.
- **Identity: all four types under RESOLVE with their criteria.** This matches Q3 and the
  description's "is one thing".
- **Question shapes.** These cover every walk in Q6. Who owes is the `debt` subject, so it does
  not appear as an aggregate field.

## Summary

**Types declared (12)**

- **Entities:** `housemate` (role of person), `house_rule`, `chore`.
- **Event:** `purchase`, with `items`, `payer` (agree-folded) and `bought_on`.
- **Claims about house rules:** `proposal` (directive), `stance` (assertive), `decision`
  (declaration, superseded on `decided_on`), `reminder` (directive).
- **Claims about chores:** `assignment` (declaration), `completion` (assertive).
- **Claims about money:** `expense` (assertive, about a purchase), `debt` (assertive, about the
  debtor).

**Identity:** there is no external ID anywhere. All four subject types are decided by RESOLVE
under their `identity_criterion`:

- **Rule:** same arrangement for the same matter; amendments keep it the same rule.
- **Chore:** same recurring task under any name.
- **Purchase:** one buying by one payer on one occasion.
- **Housemate:** same person under posting name, first name or nickname.

**Assumptions I am least sure of**

1. **The export's shape.** I assumed `plaintext` gives no per-message date or sender field, so
   time is read from each line and no `change.document` or protocol fold is declared. A
   chat-aware extractor or `--corpus` evidence would change this. The line timestamps' date
   order (day/month or month/day) also depends on the phone's locale.
2. **Debt's subject.** I made the debtor housemate, not the purchase, mainly so that housemate
   records exist at all. One debt line that covers several purchases then cannot point at all of
   them.
3. **The closed sets.**
   - The 12 currencies (the house's country is unknown).
   - The 12 rule matters, which feed the tension comparison.
   - The decision outcomes and `how` values.
4. **`reminder` and `stance`.** Both are my additions, made so reminders, complaints and
   individual votes have somewhere to land other than `decision`.
5. **`max_entities_per_section = 40`.** This is sized to one rota post listing twenty housemates
   plus their chores, not measured.
