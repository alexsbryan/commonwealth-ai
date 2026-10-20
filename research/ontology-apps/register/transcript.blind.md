# Interview transcript: rule ledgers of five software projects

The partner was not available. Each answer gives its source: `description` (with the
words quoted from DESCRIPTION.md) or `assumption` (general knowledge of the domain:
rule ledgers kept in git, commit-message conventions).

Templates consulted: `sovereign recipe new --ontology list`, then scaffolded
`governance` and `engineering-org` into this directory (`tmpl-governance.toml`,
`tmpl-engineering.toml`) to read their shape. Neither was copied; the governance
template's "rule as a directive claim about a topic" does not fit, because here a rule
is a thing with an id whose standing changes.

## 1. Shape

**Q.** What is this material about, and what would you want to know about each kind of
thing? Is any of them a kind of another, or a part something plays? Do you already have
any of it in a table?

**A.** Three kinds of thing.
- A **rule**: known by its id, with its project, what it says, and its standing now.
  Source: description, "Every rule the messages mention, by its id, with its project and
  what it says" and "Each rule's standing now".
- A **project**: one of five, and it is the `thread` field of every message. Source:
  description, "five software projects: canon, zoracite, ersilia, eutropia and
  commonwealth-ai" and "`thread` (the project)". Because the field carries it, the type is
  read from metadata (`source.metadata = ["thread"]`), no model call.
- A **maintainer** (who wrote a message, and so who approved): the `author` field.
  Source: description, "when and by whom it was approved" and "`author`"; that the author
  of the approving message is the approver is an assumption.
- No specialization or role. No table: the material is only the messages. Source:
  description, "It comes as JSON lines, one message per line" (nothing else named).

Follow-up (format/extractor): JSON lines with `id`, `thread`, `created_at`, `author`,
`title`, `body` → `extract.type = "jsonl"`, `content_field = "body"`, `title_field =
"title"`. Source: description, "fields `id`, `thread` (the project), `created_at`,
`author`, `title` and `body` (the message)".

## 2. Assertion

**Q.** What do the sources say about those things — stating, requiring, deciding,
asking? About what? What does each statement carry? How do you tell strong evidence from
weak? Who is speaking? What must it never do?

**A.** Each message records a change to a ledger: "a rule proposed, approved, superseded
by a newer rule, retracted or reverted, adopted from another project" (description).
Those are declarations (saying it makes it so in the ledger), each about one rule. One
claim type, `ledger_entry` (force `declaration`, subject `rule`), with:
- `action`, closed: proposed / approved / superseded / retracted / reverted / adopted.
  Source: description (the list above).
- `successor` (ref rule): which rule superseded it. Source: description, "Which rule
  superseded a superseded rule".
- `reason` (text): why. Source: description, "When a retracted rule was retracted, and
  why".
- `actor` (ref maintainer): who made the change. Source: description, "by whom it was
  approved".
- `adopted_from` (ref rule): the other project's rule. Source: description, "adopted from
  another project"; that the adopted rule carries a new id in the adopting project is an
  assumption.
- `reverts` (text): the id of the message a revert undoes. Assumption (git revert
  convention, "This reverts commit <id>").
- `message_id` (text): the id of the message recording the entry, needed as the
  protocol fold's transition identity (validate run 1). Assumption that the reader can
  fill it from the document's declared facts.
- What the rule says is the rule's `statement` attribute, read from the proposing
  message (`by = "earliest"`). Assumption that the proposal carries the canonical wording.
- Evidence grades: none; every entry is the ledger's own record. Assumption.
- Speakers: the message authors, who are subject matter (maintainers) not excluded.
  `must_not`: none stated. Assumption.

## 3. Identity

**Q.** How do you know two mentions are the same thing? Is there an ID?

**A.** Rules: the id, "an id like `can-0123456789ab`" (description). Declared
`identity = ["rule_id"]` plus an `identity_criterion` saying same id = same rule,
different id = different rule even with the same words (the latter is an assumption,
covering adoption and supersession). Projects: the `thread` value (description). Maintainers:
the `author` value (assumption that it is a stable handle).

## 4. Change

**Q.** When does a later statement replace an earlier one? Does anything move through
stages — what stages, which statements move it, who may, reported vs named time, how is a
mistaken move corrected? Which fields give date and conversation?

**A.**
- Stages of a rule: proposed → in_force (approved) → superseded or retracted. Source:
  description, "in force, proposed and awaiting approval, superseded, or retracted".
- Moves: proposal → proposed; approval → in_force; adoption → in_force (assumption: an
  adopted rule is in force in the adopting project); supersession → superseded;
  retraction → retracted. Source: description for the move names; mapping is mine.
- Correction: a revert undoes an earlier entry; it names that entry (`corrects =
  "reverts"`). Undoing an approval returns to proposed; undoing a retraction or
  supersession returns to in_force; undoing a proposal leaves it retracted. Assumption.
- Who may move: anyone (no `by` path). Assumption; the description does not say who may
  approve.
- Time: the move takes effect when the message is written (`created_at` is report time;
  no `effective_time`). Assumption.
- Ledger entries do NOT supersede each other (`change.supersedes` not set): history is
  kept so "when proposed, when approved" stays answerable. Standing comes from the
  protocol fold `rule_standing` onto `rule.standing`.
- Document fields: `date = "created_at"`, `thread = "thread"`, `id = "id"`, `author =
  "author"`. Source: description (the field list).

## 5. Derivation

**Q.** What should it notice that no single document says — contradictions, patterns,
larger structures? What looks like a contradiction but isn't?

**A.** The main derivation is the standing itself (protocol fold over all entries of a
rule). Beyond that, the description asks for none. Assumption: conflicting ledger
entries about the same rule are worth surfacing (`tension.between = ["ledger_entry"]`),
and these look like conflicts but are not: a proposal then its approval; a supersession
alongside the approval of its successor; a revert undoing an earlier entry; the same rule
adopted into another project under a new id.

## 6. Questions

**Q.** Once it is read, what will you ask of it?

**A.** The description's five needs, each walked to the declaration:
1. Every rule by id, with project and what it says → `rule` (identity `rule_id`),
   `rule.project` (ref `project`), `rule.statement`.
2. Standing now → `rule.standing`, derived by fold `rule_standing` (protocol).
3. When proposed, when and by whom approved → `ledger_entry` with `action = proposed` /
   `approved`, its `document_date` stamp (from `created_at`) and `actor` (ref
   `maintainer`).
4. Which rule superseded a superseded rule → `ledger_entry.successor` on the
   `action = superseded` entry.
5. When a retracted rule was retracted, and why → `ledger_entry` with
   `action = retracted`, its `document_date`, and `reason`.
Each lands in a declared type; none points at a missing kind.

## Validation runs

### Run 1: `sovereign recipe validate recipe.toml` (exit 1)

```
Validating recipe: recipe.toml
✗ Validation failed:
  - derived fold `rule_standing`: protocol rule `proposal` reads `document_id`, which `ledger_entry` does not declare
  - derived fold `rule_standing`: protocol rule `approval` reads `document_id`, which `ledger_entry` does not declare
  - derived fold `rule_standing`: protocol rule `adoption` reads `document_id`, which `ledger_entry` does not declare
  - derived fold `rule_standing`: protocol rule `supersession` reads `document_id`, which `ledger_entry` does not declare
  - derived fold `rule_standing`: protocol rule `retraction` reads `document_id`, which `ledger_entry` does not declare
  - derived fold `rule_standing`: protocol rule `revert_approval` reads `document_id`, which `ledger_entry` does not declare
  - derived fold `rule_standing`: protocol rule `revert_retraction_or_supersession` reads `document_id`, which `ledger_entry` does not declare
  - derived fold `rule_standing`: protocol rule `revert_proposal` reads `document_id`, which `ledger_entry` does not declare
```

Fix: the protocol identity must be a declared claim field, not the `document_id` stamp.
Added `message_id` (text) to `ledger_entry` and set `protocol.identity = "message_id"`.

### Run 2: `sovereign recipe validate recipe.toml` (exit 0)

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  lifecycle: off on fold `rule_standing`; `lifecycle = true`, `replaced` or `acts_under` supply `rule_replaced` and `rule_act_under`
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over ledger_entry — cross-document declared corpora select the embedding net; the classifier judges each pair
  source: project ← document fields thread (name: value) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: project → name (external key, strict merge)
  source: maintainer ← document fields author (handle: value) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: maintainer → handle (external key, strict merge)
  identity: rule → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [project, maintainer, rule]; relations [(none)]; events [(none)]; aggregate [ledger_entry by actor, ledger_entry by successor, ledger_entry by adopted_from]
  derived: rule.standing ← fold `rule_standing` by `protocol`, after RESOLVE: the claims about it
  document fields: document_date ← `created_at` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread`, document_id ← `id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted; author ← `author`, read by the reader only, among each document's declared facts
```

### Reading the derived facets against the answers

- clock `document_date` from `created_at`: matches Change (report time).
- project and maintainer from `thread` / `author`, strict merge: matches Shape/Identity.
- rule → RESOLVE under the identity criterion (not a strict key merge, although
  `identity = ["rule_id"]` is declared): this is what the protocol fold requires (its
  target must be decided by RESOLVE), and the criterion states the same-id rule, so I
  accept it rather than change it.
- `rule.standing` ← protocol fold: matches need 2.
- aggregate `ledger_entry by successor`: answers need 4; `by actor` answers the "by whom"
  half of need 3.
- lifecycle off: intended; supersession is an explicit `action` value, so the method's
  `rule_replaced` kind would duplicate it.
- tension over `ledger_entry` by embedding net: acceptable; it is an assumption, not a
  stated need.
- No `fill:` notes or unfilled-attribute warnings were printed; validate was run without
  `--corpus` (no material available), so the metadata field names were not checked
  against real documents.
No facet contradicted an answer; no further edit.
