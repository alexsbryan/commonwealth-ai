# ATOS — Agent Task Orchestration System

ATOS is the "invisible orchestrator" that sits between a developer and
a coding agent. It keeps the agent on spec, remembers why decisions
were made, detects drift, and surfaces artifacts — without the
developer having to configure anything beyond writing a charter
markdown file and committing it.

← [back to README](../README.md)

## The core promise

You write a feature charter in markdown, someone else reviews it, and
from that point on the agent *can't* silently deviate from it. The
spec becomes the contract. Drift gets flagged. Decisions get logged.
Red-team passes can run automatically. You keep working the way you
already work.

If you do nothing else:

1. Write `.sovereign/features/<id>/spec.md`.
2. `git commit` it.
3. Point opencode at `commonwealth/sovereign-coder` as the model.

You now have an ATOS-orchestrated session. Everything below is
*optional* surface area for when you want to see or steer what ATOS
is doing behind the agent.

## Mental model

```
┌─────────────┐      ┌──────────────┐      ┌─────────────┐
│   charter   │ ───▶ │  milestones  │ ───▶ │   report    │
│  (spec.md)  │      │  (briefs)    │      │  artifacts  │
└─────────────┘      └──────────────┘      └─────────────┘
      │                     │                     │
      ▼                     ▼                     ▼
  approval            run + stop cmd         milestone-N.md
  (git commit)        (agent + tests)        red-team.md
                                             epistemic-report.md
```

Three things you'll encounter in daily use:

**Feature** — a unit of work anchored to
`.sovereign/features/<id>/spec.md`. The id is a filesystem-safe slug
(`zotero-acquirer`, `atos-m5-artifacts`). ATOS treats the feature as
"approved" the moment the spec has a git commit — solo developer or
team, doesn't matter. The deliberate act of committing is the
signal. Uncommitted working-tree edits don't count; that's the only
threshold.

**Milestone** — a labeled segment of the feature with a stop
condition — usually `cargo test -p ...`. When the stop condition
passes, the milestone's `milestone-N.md` report is written and indexed
into `project_context` so future sessions can retrieve it.

**Drift** — any edit to `spec.md` after approval. ATOS detects this
by hashing the file and comparing to the approved hash. Drift doesn't
block — it writes a `deviation`-kind note and surfaces a warning in
the next preamble. You either revert, or run `atos spec accept` to
re-baseline.

## Getting started

You're a solo developer who just wants to try ATOS end-to-end.

### 1. Check your environment

```
sovereign atos doctor
```

Prints a ✓/✗/⚠ report:

```
✓ repo root                  /home/yara/myrepo
✓ .sovereign directory       /home/yara/myrepo/.sovereign
✓ notes.db                   open + migrations OK
✓ features.db                0 features
✓ default pipelines          sovereign-coder resolves
⚠ opencode plugin            .opencode/plugins/sovereign-atos.ts not found
✓ commonwealth daemon        localhost:9741 responding
```

Warnings are fine. Failures (`✗`) are the only thing to act on.

### 2. Write a charter

Minimum viable charter at
`.sovereign/features/zotero-acquirer/spec.md`:

```markdown
# zotero-acquirer — Acquire from Zotero libraries

Short paragraph about the motivation.

## Invariants

- Library id must be URL-safe.
- Zotero collections deeper than 5 levels are rejected.

## Milestones

### 1. Library type

Add the ZoteroLibrary variant to LocalCorpusSourceType.

**Stop condition:** `cargo test -p corpus-engine acquirers::zotero`

### 2. RDF parser

Wire the RDF parser in.

**Stop condition:** `cargo test -p corpus-engine extractors::zotero_rdf`
```

Two things the parser is strict about:

- A `## Milestones` heading (level 2) must exist.
- Every milestone (level 3 heading) must contain a `**Stop
  condition:**` paragraph — even if the body is empty (manual review
  is a legitimate stop).

### 3. Provision + commit

```
sovereign atos provision zotero-acquirer --charter .sovereign/features/zotero-acquirer/spec.md
git add .sovereign/features/zotero-acquirer/spec.md
git commit -m "spec: zotero-acquirer"
```

That commit IS the approval. The feature is now ready for the agent.
If you're working off a branch where you'd rather not commit yet
(prototyping, still editing), use `sovereign atos feature approve
zotero-acquirer` to record a MeshStore approval against the current
working-tree spec — no commit needed.

### 4. Point your agent at it

Configure opencode to use `commonwealth/sovereign-coder` as the model.
The opencode plugin at `.opencode/plugins/sovereign-atos.ts` will
inject `X-Feature-Id` based on the current git branch's feature
directory. From here, the agent:

- Sees the spec + active notes in every system prompt.
- Can't call write tools (`str_replace_editor`, `bash`, etc.) on an
  unapproved feature.
- Gets a "Welcome back" briefing when you return after a break.
- Sees "Since last turn — milestone 2 PASSED" when you close a
  milestone between turns.

You do nothing to make any of that happen.

### 5. Close milestones as you go

```
sovereign atos start-milestone zotero-acquirer --brief brief.md
# agent drives the work to green
sovereign atos end-milestone zotero-acquirer
# ✓ stop_condition PASSED → wrote .sovereign/features/zotero-acquirer/milestone-1.md
```

The milestone report auto-indexes into `project_context`, so a future
session can run `project_context(query: "zotero BOM handling")` and
pull the relevant excerpt.

## The day-to-day loop

Most turns you won't touch the CLI. The agent calls ATOS tools
(`read_notes`, `write_note`, `project_context`) and the orchestrator
takes care of the rest.

When you DO reach for the CLI:

| Situation | Command |
|---|---|
| "Where are we on this feature?" | `atos status <id>` |
| "What did the model do in this run?" | `atos diff <id>` |
| "I edited spec.md — what changed?" | `atos spec diff <id>` |
| "The edit was intentional." | `atos spec accept <id> --reason "..."` |
| "Something feels off." | `atos doctor` |
| "Run a red-team pass now." | `atos start-milestone <id> --red-team --milestone-id <mid>` |

## Spec drift, intentional edits, and `spec accept`

Say you approved a charter, then noticed an invariant was wrong. You
edit `spec.md`. The next time the agent starts a turn, its preamble
shows:

> ⚠ **Spec drift detected since approval.** See `[note:xyz]`.
> Either write an intentional deviation note explaining the change,
> or revert spec.md to the approved version before proceeding.

Two paths:

- **Revert** — `git checkout HEAD -- .sovereign/features/<id>/spec.md`.
  Drift goes away.
- **Accept** — `sovereign atos spec accept <id> --reason "fixing
  invariant wording"`. Writes a `deviation`-kind note with the
  unified diff as justification, updates the approved hash in
  MeshStore, silences the drift warning. The original reviewer
  attribution is preserved — you're not re-approving, you're updating.

See what you'd be accepting first:

```
sovereign atos spec diff <id>
```

Prints `diff -u` between the approved content and the current file.

## Opting into auto red-team

Add a line to the charter preamble:

```markdown
# my-feature — Title

**Red team:** auto

Some prose...
```

Accepted values: `auto`, `true`, `yes`, `on` (case-insensitive).
Alternate phrasings: `**Red-team:** auto`, `**Auto red-team:** true`.

When the *final* milestone's `end-milestone` passes, ATOS
automatically spawns:

```
⚙ auto-redteam: charter opted in — spawning red-team pass for milestone 2…
```

…which runs `start-milestone --red-team --milestone-id <id>` followed
by another `end-milestone`. The red-team run writes `red-team.md`.
The red-team session uses the `commonwealth/sovereign-red-team`
pipeline, which:

- Injects *only* the `## Invariants` section of the spec (no
  implementation hints).
- Doesn't inject notes.
- Blocks write tools at the middleware layer.

Red-team sessions can only write `redteam_finding` notes. Normal
sessions can't see red-team sessions' notes, by design.

## The artifacts

Under `.sovereign/features/<id>/`:

| File | Written when | Contains |
|---|---|---|
| `spec.md` | You write it | The charter |
| `milestone-N.md` | `end-milestone` passes | Stop output, uncertainty notes, decision log |
| `red-team.md` | `end-milestone` on a `--red-team` run | Findings grouped by confidence |
| `epistemic-report.md` | `atos teardown` | Promoted notes, pending uncertainties, postmortem pointers |

All four are indexed into `project_context`. Search them like any
other doc:

```
project_context(query: "zotero collection depth limit")
```

## Mental model for the pipeline (optional)

You can ignore this section entirely. If you want to understand what's
happening between turns N and N+1:

```
opencode POST /v1/chat/completions model=commonwealth/sovereign-coder
  ▼
ApprovalGate       — blocks write-intent tools on unapproved features
SessionBriefing    — "Welcome back" block on fresh/stale sessions
ContextInjector    — prepends notes digest + spec + drift flag
ToolInjector       — merges ATOS tool defs into the model's tool list
  ▼
inference
  ▼
ArtifactSurface    — records "notes written this turn" + "milestones passed"
                     → staged on session; ContextInjector renders it on N+1
  ▼
response to client
```

You never configure any of this. The pipeline is resolved by name
from `commonwealth/crates/commonwealth-core/src/default_pipelines.toml`.

## Troubleshooting

**"no approval found for <id>"** — the feature isn't approved. Either
commit a reviewer change to `spec.md`, or run `atos feature approve
<id>` to record a MeshStore approval.

**"spec.md missing at ..."** — you deleted the spec but the feature
row is still in `features.db`. Either put the spec back or
`atos archive <id>`.

**`atos doctor` says `no fast-capable model registered`** — the Fast
slot is used for teardown suggestions. Not load-bearing for the
core loop; a warning you can ignore in solo dev.

**Drift detected but I didn't edit anything** — `git status` on the
spec file. Whitespace changes or autocrlf can also shift the hash;
normalize line endings (`.gitattributes` with `* text=auto`).

## Cheat sheet

```
atos doctor                                   # health check
atos provision <id> --charter <path>          # one-shot from charter
atos status [<id>]                            # overview
atos next [<id>]                              # what's the next milestone?
atos start-milestone <id> --brief <path>      # kick off a run
atos end-milestone <id>                       # close + write milestone-N.md
atos spec diff <id>                           # show drift
atos spec accept <id> --reason "…"            # accept drift + note it
atos feature approve <id>                     # Commonwealth-native approval
atos diff <id>                                # what the last run did
atos report <id> [--section milestone|red-team|epistemic]
atos teardown <id>                            # wrap feature + write epistemic report
atos archive <id> --reason "…"                # shelve a feature
```

## Where to look next

- [`CLI_REFERENCE.md`](CLI_REFERENCE.md) — every flag for every subcommand
- [`CODE_INTELLIGENCE.md`](CODE_INTELLIGENCE.md) — `project_context`,
  symbol lookup, call graph (the tools agents reach for alongside ATOS)
- The spec examples under
  [`.sovereign/atos-demo/`](../../.sovereign/atos-demo/) — working
  charters that provisioned the ATOS milestones themselves
