# Phase 1 — procedural typed extension

You are reading one section that does procedural work — instructing
through steps, commitments, dependencies. Task lists, project
plans, meeting recaps with action items, technical specs that name
decisions and what they require. Your job is to expose the
procedural scaffolding so a downstream reader can audit "what
will be done", "who decided what", "what's stuck".

The base entities are produced by a separate prompt. Your job is
the six collections below.

## The six collections

### 1. `tasks`

Things that will be done. "Ship the new gating", "review the PR
queue".

- `content` — one sentence stating the task.
- `owner` — entity name responsible (empty when unassigned).
- `due_at` — free-form due hint ("by Thursday", "Q3", empty when
  none).
- `anchor` — 3-8 word keyphrase.

### 2. `decisions`

Choices the section commits to or records. "We will adopt approach
X", "the team chose Postgres over MongoDB".

- `content` — one sentence stating the decision.
- `alternatives` — array of alternatives considered (empty when
  none named).
- `anchor` — 3-8 word keyphrase.

### 3. `artifacts`

The produced things the procedure references. Documents, builds,
models, releases. Different from Work atoms (creative works); an
artifact is operational output.

- `name` — the artifact name.
- `description` — one sentence stating what it is (empty when
  obvious).
- `anchor` — 3-8 word keyphrase.

### 4. `dependencies`

Directional links between tasks / decisions / artifacts.
"A depends on B", "ship X before Y".

- `from` — the dependent.
- `to` — what it depends on.
- `kind` — free-form ("blocks", "precedes", "requires"; empty when
  unspecified).
- `anchor` — 3-8 word keyphrase.

### 5. `blockers`

Active obstacles — things that prevent a task / decision from
moving forward.

- `content` — one sentence stating the blocker.
- `blocks` — task / decision / artifact name being blocked (empty
  when ambient).
- `anchor` — 3-8 word keyphrase.

### 6. `status_signals`

Progress markers — "done", "in progress", "paused", "cancelled".
Lift these generously: they're the procedural counterpart of mood
shifts in reflective work.

- `state` — one of `done`, `in_progress`, `paused`, `cancelled`,
  `unknown`. Free-form to tolerate future expansion; obvious values
  are normalised downstream.
- `content` — one sentence saying what the signal is on.
- `anchor` — 3-8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object. No prose, no `<think>` block, no
code-fence markers. Empty collections may be omitted. Required
fields must be non-empty.
