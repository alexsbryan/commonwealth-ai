# Interview transcript: a small product team's work

Partner not available. Each answer is sourced either from `description` (quoted) or
marked `assumption` (general knowledge of how product teams work and talk).

Templates consulted (scaffolded into this directory with `sovereign recipe new --ontology ...`):
`engineering-org`, `product-support`, `governance`, `contracts`. None used verbatim; the
closest structure (a source-free entity as the subject of several claim kinds) follows
`engineering-org` / `product-support`.

## 0. Source shape

**Q: What form is the material in? Files, a folder, an export?**
A: Meeting notes and team chat. Treated as one JSON Lines export, one record per chat
message or meeting-notes entry, with `text`, `date` (ISO 8601), `thread` (channel/thread or
meeting), `author` and `id`. Extractor: `jsonl` (`content_field = "text"`), chunker
`paragraph`.
Source: description ("The material is our meeting notes and the team chat."); the
export format and field names are an **assumption**. Chosen over `markdown` because the
status protocol and supersession need a per-document date and author, which the
markdown extractor does not carry.

## 1. Shape

**Q: What is this material about, and what would you want to know about each kind of
thing? Is any of them a kind of another, or a part something plays? Do you already have
any of it in a table?**
A: It is about tasks the team agreed to do. For each task: owner, due date, status,
project/milestone. People: the seven team members (owners), and whoever asks for work.
Projects and milestones group tasks; a milestone belongs to a project and has a date.
A member is a role a person plays (`role_of = "person"`). No usable table: the tracker
is out of date.
Source: description ("I manage a seven-person team", "Every task we agreed to do, with its
owner and due date", "Which tasks belong to which project or milestone", "nobody keeps the
tracker up to date"). Milestone-belongs-to-project and milestone date: **assumption**.

→ types `member` (entity, role_of person), `project` (entity), `milestone` (entity:
project, target_date), `task` (entity: owner, due, project, milestone, status).

## 2. Assertion

**Q: What do the sources say about those things: stating, requiring, deciding, asking?
What does each statement carry? Who speaks? What must it never do?**
A: Five kinds of statement about a task:
- someone takes a task on / the team agrees someone will do it (commissive) → `commitment`
- someone asks for a task, with a reason (directive, request) → `request` (requester, reason)
- someone reports where a task stands: open, in progress, blocked, done, dropped (assertive)
  → `status_update` (status from the closed set, changed_at, note)
- a task waits on another task, or no longer does (assertive) → `dependency`
  (blocked_by → task, blocking ∈ {blocks, no_longer_blocks})
- a task is handed from one owner to another (declaration) → `reassignment`
  (from_owner, to_owner, reason)
Plus two kinds needed so projects and milestones get records: a milestone is set or moved
(declaration) → `milestone_plan`; a decision about a project as a whole (declaration) →
`project_decision` (decision ∈ start/pause/resume/reprioritise/cancel/finish).
Speakers are the team members themselves (they are also the subject matter: owners).
No `must_not` declared.
Source: the five task statement kinds come from description needs 1-5 ("agreed to do",
"status (open, in progress, blocked, done, dropped)", "Which tasks block which other
tasks", "Who asked for each task and why", "Tasks that were reassigned, and to whom").
The closed status set is quoted. `milestone_plan` and `project_decision` are an
**assumption** (forced by the schema: a RESOLVE-decided type needs a claim kind about it).
No grades: **assumption** (chat statements are not graded evidence).

## 3. Identity

**Q: How do you know two mentions are the same thing? Is there an ID?**
A: No task IDs (the tracker is not used). A task is the same task when it names the same
piece of work toward the same outcome, however worded; a follow-up or sub-task is a
different task → `identity_criterion` on `task`, decided by RESOLVE. Projects and
milestones likewise by name / evident short form → `identity_criterion`. Members are
known by the name they write under → `member` sourced from the `author` field,
`identity = ["handle"]`.
Source: **assumption** throughout (description gives no identifier; "nobody keeps the
tracker up to date" implies no tracker IDs).

## 4. Change

**Q: When does a later statement replace an earlier one, and from when? Does anything
move through stages? What are the stages, which statements move it, who may make each
move, does it take effect when reported or at a time it names, how is a mistaken move
corrected or a finished thing reopened? Which document fields give date and conversation?**
A: Status moves through open / in progress / blocked / done / dropped; a `status_update`
naming a status moves the task into it (protocol fold `task_status`, one rule per state).
It takes effect as reported (document date); a time the message names is kept as
`changed_at` (effective time). Anyone may make a move (no `by`). Reopening is just a later
report of `open` / `in_progress`; no separate correction rule. Owner, due date, project
and milestone hold their latest reading (`by = "latest"`). A later reassignment supersedes
an earlier one (`change.supersedes = { reassignment = "document_date" }`). Document fields:
`date`, `thread`, `id`, `author`.
Source: the stages are quoted from the description ("open, in progress, blocked, done,
dropped") and "when it last changed" asks for the report time. Who may move, reopening,
latest-wins for owner/due, and the field names are **assumption**.
Protocol identity is the `status` field itself (chat has no transition ids): two reports of
the same status at the same report time are one transition. **assumption**.

## 5. Derivation

**Q: What should it notice that no single document says (contradictions, patterns)? What
looks like a contradiction but isn't?**
A: Conflicting statements about a task's status or commitment at the same time (two people
reporting different states; someone committing to a task reported dropped). Not
conflicts: a task reopened after done, a due date moved by a later agreement, a task handed
to a new owner after someone took it on. Blocking chains come from `dependency` refs.
Source: **assumption** (the description asks no derivation question).
→ `tension.between = ["status_update", "commitment"]`, `not_conflicts` as above.

## 6. Questions

**Q: Once it is read, what will you ask of it?** Walked to where each answer lands:
1. "Every task we agreed to do, with its owner and due date" → `task` records (RESOLVE
   over `commitment` and the other task claims), `task.owner`, `task.due`.
2. "Each task's status ... and when it last changed" → `task.status` (fold `task_status`),
   its decision history carries report time (`document_date`) and `changed_at`.
3. "Which tasks block which other tasks" → `dependency.blocked_by` (ref task), `blocking`.
4. "Who asked for each task and why" → `request.requester`, `request.reason`.
5. "Tasks that were reassigned, and to whom" → `reassignment.from_owner` / `to_owner`.
6. "Which tasks belong to which project or milestone" → `task.project`, `task.milestone`;
   `milestone.project`.
Source: description, quoted verbatim. Every question lands on a declared type/attribute.

## Validate runs

### Run 1: `sovereign recipe validate recipe.toml` (exit 1)
```
Validating recipe: recipe.toml
✗ Validation failed:
  - claim type name `blocker` is reserved: the pipeline already emits claims of that kind (property, evidence, concession, observation, realisation, blocker, status, example, same_as). Pick another name.
```
Fix: renamed the claim kind `blocker` → `dependency`.

### Run 2: `sovereign recipe validate recipe.toml` (exit 0)
```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  lifecycle: off on fold `task_status`; `lifecycle = true`, `replaced` or `acts_under` supply `task_replaced` and `task_act_under`
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over status_update, commitment — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: member → canonical name (default; declare `identity = [...]` for an external key)
  identity: project → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: milestone → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: task → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [member, project, milestone, task]; relations [(none)]; events [(none)]; aggregate [request by requester, dependency by blocked_by, reassignment by from_owner, reassignment by to_owner]
  derived: task.status ← fold `task_status` by `protocol`, after RESOLVE: the claims about it
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread`, document_id ← `id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted; author ← `author`, read by the reader only, among each document's declared facts
  ⚠  the document reader reads statement kinds, the types they are about and every type RESOLVE decides, so it makes no record of `member` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
```
Reading the facets against the answers: two were wrong.
- `member` would get no record, so owner refs had nothing to pick. Fix: `member` sourced from
  the `author` document field, `identity = ["handle"]`.
- `project` and `milestone` were RESOLVE-decided but no claim kind had them as subject
  (per v1.md, `enrich extract` would refuse the build). Fix: added `milestone_plan`
  (subject milestone) and `project_decision` (subject project); milestone's `project`
  and `target_date` fold `by = "latest"`.

### Run 3: `sovereign recipe validate recipe.toml` (exit 0)
```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  lifecycle: off on fold `task_status`; `lifecycle = true`, `replaced` or `acts_under` supply `task_replaced` and `task_act_under`
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over status_update, commitment — cross-document declared corpora select the embedding net; the classifier judges each pair
  source: member ← document fields author (handle: value) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: member → handle (external key, strict merge)
  identity: project → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: milestone → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: task → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [member, project, milestone, task]; relations [(none)]; events [(none)]; aggregate [request by requester, dependency by blocked_by, reassignment by from_owner, reassignment by to_owner]
  derived: task.status ← fold `task_status` by `protocol`, after RESOLVE: the claims about it
  document fields: document_date ← `date` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `thread`, document_id ← `id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted; author ← `author`, read by the reader only, among each document's declared facts
```
Facets read against the answers: clock = document date (matches "as reported"); tension over
status_update + commitment (matches); member keyed by handle, task/project/milestone by
RESOLVE (matches); aggregates cover needs 3, 4, 5; task.status derived from the protocol
fold (matches need 2). Lifecycle left off: a task "replaced"/"rolled into" another is not
something the description asks for. Not checked: `--corpus` (no material available), so the
field names `date`, `thread`, `id`, `author` are unverified.

## Summary

Types:
- `member` (entity, role_of person): identity `handle`, sourced from the `author` field (strict merge).
- `project` (entity): RESOLVE under identity_criterion (same body of work by name/short form).
- `milestone` (entity: project, target_date): RESOLVE under identity_criterion (same checkpoint of the same project).
- `task` (entity: owner, due, project, milestone, status): RESOLVE under identity_criterion (same work, same outcome; follow-ups/sub-tasks differ).
- Claims (subject → identity of the subject): `commitment`, `request`, `status_update`,
  `dependency`, `reassignment` (all about `task`), `milestone_plan` (about `milestone`),
  `project_decision` (about `project`).
- Fold `task_status` (protocol, identity = `status`, effective_time = `changed_at`).

Least-sure assumptions:
1. The material is a JSONL export with `text`/`date`/`thread`/`author`/`id` fields.
2. Protocol transition identity = the `status` value itself.
3. `member` identity = chat author handle; owners named by first name in text may not link to it.
4. `project_decision` and `milestone_plan` exist mainly so RESOLVE has claims for those types.
5. Anyone may move a status; no correction rule.
