# ATOS — Agent Task Orchestration System

You use coding agents for non-trivial work. You've probably been
burned a few times: the agent reversed a decision it made three
turns ago; a reviewer asked "why did we pick X?" and nobody
remembered; a milestone quietly missed its tests and the session
moved on.

ATOS is scaffolding that makes those failure modes visible. It
sits inside your repo: the human-facing artifacts at root
(`DESIGN.md`, `OPEN_QUESTIONS.md`, `IMPLEMENTATION_PLAN.md`) and
tool state under `.sovereign/` (`CHARTER.md`, `project.toml`,
`notes.db`, `plan.db`, session transcripts). It records what you
agreed, detects when the spec drifts, logs the arguments against
every amendment, and hands a reviewer a one-page audit trail
they can read instead of interviewing you.

You don't configure anything. You write `DESIGN.md` — or let the
agent draft it with you. The agent sees the spec in every turn.

← [back to README](../README.md)

## When this is for you

- You're starting a greenfield project and want to pin down
  *what's expensive to change* before any code is written.
- You pair with coding agents on a project that's outgrown
  "vibes" — decisions are accumulating and nobody's writing them
  down.
- You've reversed a decision and two weeks later couldn't
  remember why. Or worse: someone else reversed it without
  realizing it had been decided.
- You want an adversarial review when you change the design
  or the charter — something that asks "have you considered
  what this breaks?"
- You're handing a project off (to yourself in six months, to a
  colleague, to a reviewer) and want the artifacts to carry the
  story.

If none of those land — you're on a throwaway, your agent is
disciplined, you don't need an audit trail — you don't need ATOS.

## Use cases — recognize yourself?

### "I want the agent to work on my design with me"

```
sovereign project design
```

Starts an agent-collaborative session against the running
Commonwealth daemon. opencode is the blessed transport — the
command launches it primed with a session brief that includes
your `DESIGN.md`'s current state (anchors, structural gaps, keyword
buckets) and the tools the agent can call to propose edits.
Every file write is diff-confirmed before it lands.

Don't have opencode yet? `--stopgap` gives you a provisional
in-terminal chat (clearly banner-labelled — the push is always
toward opencode). Daemon down? `--solo` drives a CLI-prompt
walk through every structural gap the parser saw and writes
them to `OPEN_QUESTIONS.md`. `--import <path>` copies an
existing design you already have into `DESIGN.md` at repo root
(diff-confirms before overwriting).

### "I want the plan derived from the design"

```
sovereign project plan
```

Reads `DESIGN.md` + `OPEN_QUESTIONS.md`; composes
`IMPLEMENTATION_PLAN.md` at repo root. Phase 0 is a
language-specific skeleton (`cargo test`, `npm run build &&
npm test`, etc.); phases 1..N come from H2 sections of `DESIGN.md`
in document order. Answered `OPEN_QUESTIONS.md` entries surface
as `Resolved (for the record)` on the matching phase; unanswered
ones block the plan unless you pass `--allow-open`, in which case
they become `Open risks` attached to the relevant phase.

Plan items are also upserted into `.sovereign/plan.db` (a
`plan_items` table with phase/state/depends_on/realizes
columns) so `sovereign read-notes` and future phase-pass flows
can query them by state.

### "I want the agent to stay on spec"

Point your agent at the `commonwealth/sovereign-coder` pipeline.
Every turn's system prompt gets the project invariants (from
`CHARTER.md`) and the feature spec (from `spec.md`) prepended.
If anyone — you or the agent — edits the spec without
committing, the next turn's preamble gets a drift warning
pointing at the deviation note.

### "I want an adversarial review when I change the charter (or the design)"

```
sovereign project amend charter   # or: sovereign project amend design
```

`amend charter` (default) opens `.sovereign/CHARTER.md` in
`$EDITOR`. On save, ATOS diffs your edit section-by-section.
Invariants changed? You get:

> Which callers / components assume the OLD invariant? Are there
> tests that LOCK it?
>
> What's the replacement invariant, stated as strictly as the
> old one was?

`amend design` opens `DESIGN.md` at repo root. It tracks edits
to three curated sections — `Anchors`, `Data & interfaces`,
`Open questions` — and asks a targeted adversarial question for
each changed section (e.g., "Which downstream assumption in
`IMPLEMENTATION_PLAN.md` changes if this anchor is reworded?").
The Q&A is appended to `DESIGN.md`'s own `## Amendment log`
(newest on top); `charter_version` is not bumped because
`DESIGN.md` is expected to iterate — git history is the
provenance.

Answer each, then the amendment log, the Q&A, and the new
hash are recorded. Future sessions see WHY the change went
through despite the named risks.

### "I want a free-form team CHARTER.md that isn't a filled-out form"

```
sovereign project charter
```

First invocation writes a minimal skeleton at
`.sovereign/CHARTER.md` (Who we are / How we decide /
Onboarding pointers / Amendment log) and opens `$EDITOR`.
Subsequent invocations just open the existing file.

`CHARTER.md` is distinct from `DESIGN.md`. `DESIGN.md` says
what you're building; `CHARTER.md` says how the team works
together on it. It is **not** auto-generated from the design —
the plan's insight was that the culture/governance doc is
human-authored prose, full stop. Indexed into `project_context`
so `project_context("how we decide")` surfaces relevant
sections during agent turns.

### "I want milestones that actually pass their tests before I declare them done"

```
sovereign atos end-milestone ingest-service
```

Runs the stop condition from your spec. On pass, writes
`milestone-1.md` with the captured output. On fail, the
milestone doesn't close — you fix and retry. Same pattern at the
project layer: `sovereign project phase pass 1` runs Phase 1's
stop condition from `PHASES.md` and only advances
`current_phase` on green.

### "I need to log decisions so I can come back later"

Every decision, uncertainty, failed attempt, or accepted
deviation gets a note in `.sovereign/notes.db`. The founding
conversation writes them automatically (Stage 1 answers, Stage 2
fault-line resolutions). Inside an agent session, the agent
writes them on real choices. Read back with:

```
sovereign read-notes --kind decision
sovereign read-notes --kind uncertainty
```

### "I'm handing this project off"

```
sovereign project audit > audit.md
```

Produces one page: founding state, phases passed (with artifact
links), notes by kind, open questions, deviations, features list,
drift status, full artifact inventory. Paste it into a PR
description or an onboarding doc. The new reader has the context
without having to ask.

### "I want a red-team review on the final milestone"

Add `**Red team:** auto` to your feature charter preamble. When
the final milestone passes, ATOS auto-spawns a red-team pass: a
second agent session with a restricted context (invariants only,
no notes, no write tools) that tries to break what you built.
Findings land in `red-team.md`.

### "I don't have docs for this API and I don't want the agent to guess"

During founding, one question asks for documentation URLs. Paste
them; ATOS fetches each and indexes them into `project_context`.
At runtime, when the agent hits a gap, the honest-uncertainty
prompt fires:

> I don't have documentation for the vendor's WebSocket reconnect
> behavior.
> Best guess: https://vendor.example/docs/ws
> Fetch it? [Y/n, or paste a different URL]

Fetches join the corpus. Declined gaps are recorded — the system
doesn't re-ask speculatively.

## The two layers

ATOS operates at two granularities. You can use one or both.

**Project layer** — before any code. `sovereign project init`
observes the repo (and offers to `git init` if you haven't
yet). `sovereign project design` collaborates with the agent
(or walks you through gaps solo) to produce `DESIGN.md` +
`OPEN_QUESTIONS.md` at repo root. `sovereign project plan`
turns those into `IMPLEMENTATION_PLAN.md`. `sovereign project
charter` writes the free-form team `CHARTER.md` in
`.sovereign/`. Founding is implicit: once those artifacts exist and are
committed, the project is founded — there's no separate
`found` step (it was retired).
Amendments go through `sovereign project amend charter` / `amend
design` with adversarial review. Progression tracks via
`sovereign project phase pass`.

**Feature layer** — inside a founded project (or any repo).
Write `.sovereign/features/<id>/spec.md`; commit it; close
milestones as work lands. Stop conditions run at
end-of-milestone; drift is detected on every agent turn.

The two are orthogonal — you can run the feature layer on a
repo that was never founded. Founding adds a project-wide
charter that the feature spec nests under.

## Mental model

```
DESIGN.md ──▶ what we're building (iterative, agent-collaborative)
   │
   ▼
OPEN_QUESTIONS.md ──▶ gaps flagged by the structural parser
   │                  (antifragile: answers append, never overwrite)
   ▼
IMPLEMENTATION_PLAN.md ──▶ phases derived from DESIGN.md sections
   │                       (plan_items table: state/phase/depends_on)
   ▼
CHARTER.md ──▶ free-form team governance (Who we are / How we decide)
   │
   ▼
PHASES.md ──▶ drives phase pass/amend
   │
   ▼
artifacts (phase-N.md, milestone-N.md, amendment log, notes.db)
   │
   ▼
audit → reviewer reads one page
```

Four terms worth knowing:

**Design** — `DESIGN.md` at repo root. What you're building.
Iterative; amendments through `project amend design` with an
inline `## Amendment log`.

**Charter** — `.sovereign/CHARTER.md`. How you work together on
it. Free-form, human-authored, versioned (`charter_version`
bumps on each `amend charter`). Not auto-generated from DESIGN.md.

**Drift** — file changed since approval. Detected by hash
comparison. Drift doesn't block — it surfaces a warning. You
either revert or re-baseline (`atos spec accept` / `project
amend charter`).

**Artifacts** — the markdown the work leaves behind. Outlives
any session. What a reviewer reads.

## First: wire up `commonwealth/sovereign-coder`

ATOS rides on top of the Commonwealth inference daemon. Both
quickstarts below assume these three pieces are in place — a
one-time setup per workstation / repo:

**1. A running Commonwealth daemon** serving the OpenAI-compatible
API at `http://localhost:9741/v1`. It registers the
`commonwealth/sovereign-coder` pipeline (approval gate,
context injector, tool injector, artifact surface) automatically.

```
commonwealth daemon start
curl -s http://localhost:9741/v1/models | jq '.data[].id'  # sanity check
```

Daemon details, alternative ports, model selection — see
[commonwealth/README.md](../../commonwealth/README.md).

**2. opencode pointed at the daemon.** In your opencode config
(usually `~/.config/opencode/config.json`), add a provider:

```json
{
  "provider": {
    "commonwealth": {
      "base_url": "http://localhost:9741/v1",
      "api_key": "not-required-for-local"
    }
  },
  "model": "commonwealth/sovereign-coder"
}
```

**3. The ATOS opencode plugin** at
`.opencode/plugins/sovereign-atos.ts`. It injects the
`X-Feature-Id` header based on the current git branch's feature
directory so the daemon knows which feature's spec to inject.

The plugin is embedded in the `sovereign-cli` binary and installed
automatically the first time you run `sovereign project init` in
a repo — no manual copy. Upgrade it after a CLI bump with:

```
sovereign atos install-plugin
```

`sovereign atos doctor` cross-checks the installed version against
the CLI binary and flags drift. Every installed copy carries a
`// sovereign-atos-version: X.Y.Z` header so old installs are
self-identifying.

Once those three are wired, everything in the quickstarts
below just works — the agent gets the charter preamble on every
turn with no further configuration.

## Quickstart — greenfield (agent-collaborative, recommended)

```
cd my-new-repo
sovereign project init                   # observe; offers `git init` if absent
sovereign project design                 # opencode launches with DESIGN.md primed
# … iterate with the agent; /done when satisfied …
sovereign project plan                   # derive IMPLEMENTATION_PLAN.md
sovereign project charter                # free-form team CHARTER.md
sovereign project found --orchestrate    # verify + flip lifecycle
# ... write code in opencode ...
sovereign project phase pass 0           # run stop condition, write phase-0.md
```

From the moment `project found` approves, the agent sees your
charter invariants + current phase in every turn. Most users
will iterate between `project design` and `project plan` for a
while before running `project found` — that's the point.

## Quickstart — greenfield (classic questionnaire)

If you'd rather answer signal-gated questions than iterate on a
DESIGN.md, the original flow still works:

```
cd my-new-repo
sovereign project init
sovereign project found                  # 4-stage signal-gated conversation
# ... write code ...
sovereign project phase pass 0
```

The Stage-1 / Stage-2 predicates no longer fire universal
questions — `fault.time-representation` now requires the design
text, observation, or prior answers to mention time, and so on
for the other fault lines.

## Quickstart — feature layer on an existing repo

```
mkdir -p .sovereign/features/ingest-service
cat > .sovereign/features/ingest-service/spec.md <<'EOF'
# ingest-service — Acquire ticks from a vendor

## Invariants
- Tick timestamps are UTC at rest.
- Schema changes require an amendment.

## Milestones

### 1. Acquirer skeleton
**Stop condition:** `cargo test acquirers::vendor`

### 2. Rate-limit handling
**Stop condition:** `cargo test rate_limit`
EOF
git add . && git commit -m "spec: ingest-service"
```

Commit = approval. With the pipeline wired up (see above), the
agent sees the spec every turn; writes are blocked until the
spec is committed.

## The commands you'll actually use

```
# Project layer — design → plan → charter → found
sovereign project init                                   # observe + git auto-confirm
sovereign project design [--import <path>] [--solo]      # agent-collaborative DESIGN.md
    [--stopgap] [--via opencode|claude-code]
sovereign project plan [--allow-open]                    # compose IMPLEMENTATION_PLAN.md
sovereign project charter [--print]                      # free-form team CHARTER.md
sovereign project found [--orchestrate] [--design <path>]# founding (orchestrator or questionnaire)
sovereign project phase status                           # where are we?
sovereign project phase pass [N]                         # verify + advance
sovereign project amend [charter|design]                 # adversarial edit
sovereign project audit                                  # one-page reviewer rollup

# Feature layer
sovereign atos provision <id> --charter <path>
sovereign atos start-milestone <id> --brief <path>
sovereign atos end-milestone <id>
sovereign atos spec diff <id>               # show drift vs approved
sovereign atos spec accept <id> --reason    # accept drift + log
sovereign atos teardown <id>                # wrap + epistemic report
sovereign atos doctor                       # health check
sovereign atos install-plugin               # (re)install the opencode plugin after a CLI upgrade
```

Full reference: [CLI_REFERENCE.md](CLI_REFERENCE.md).

## Where the signal lives

**Repo-root artifacts** (human-facing, `git diff`-able, indexed into `project_context`):

| File | Written by | Tells a reviewer... |
|---|---|---|
| `DESIGN.md` | `project design` / `amend design` | What you're building; inline `## Amendment log` records reversals |
| `OPEN_QUESTIONS.md` | `project design --solo` / agent tool / you inline | Load-bearing gaps; answers are append-only (provenance preserved) |
| `IMPLEMENTATION_PLAN.md` | `project plan` | Phase skeleton, stop conditions, open/resolved risks per phase |

**`.sovereign/` state** (tool-managed; charter + SQL indexes):

| File | Written by | Tells a reviewer... |
|---|---|---|
| `CHARTER.md` | `project charter` / `amend charter` / `project found` | Team governance: who we are, how we decide, onboarding pointers; amendment log = history of reversals |
| `PHASES.md` | `project found` | How you planned to get there |
| `phase-N.md` | `project phase pass N` | Which phases actually verified green |
| `project.toml` | `init` / `found` / `amend` / `phase` / `charter` | Observations + lifecycle + charter hash + `git_declined_at_init` |
| `plan.db` | `project plan` / future `phase pass` | `plan_items` table — queryable phase/state/depends_on |
| `notes.db` | Agent + CLI | Decisions, invariants, attempts, uncertainties, deviations |
| `.atos/design/<id>/brief.md` | `project design` | Session prompt + state for the agent-collaborative DESIGN flow |
| `features/<id>/spec.md` | You | Feature contract |
| `features/<id>/milestone-N.md` | `atos end-milestone` | Which feature milestones actually passed |
| `features/<id>/red-team.md` | Red-team milestone run | Findings grouped by confidence |
| `features/<id>/epistemic-report.md` | `atos teardown` | Promoted notes + postmortem |

All markdown files get indexed into `project_context`, so
`project_context(query: "why UTC")` pulls the relevant decision
across DESIGN.md / CHARTER.md / IMPLEMENTATION_PLAN.md /
milestone reports without you going to find it. The agent can
also call the `design_signals_extract` MCP tool mid-session to
audit `DESIGN.md` structurally (anchors, gaps, keyword buckets).

## What ATOS won't do

- **Write code for you.** That's the agent. ATOS gives the agent
  accountability structure; the code still comes from whoever
  holds the keyboard.
- **Replace testing.** Stop conditions run tests and record
  verdicts; the tests are your job.
- **Police you.** If you don't run `project design` / `plan` /
  `charter` / `found`, those artifacts don't appear. If you
  don't commit `spec.md`, the approval gate never opens. ATOS
  surfaces when something's off — it doesn't block.

## Read next

- [CLI_REFERENCE.md](CLI_REFERENCE.md) — every flag for every
  subcommand
- [CODE_INTELLIGENCE.md](CODE_INTELLIGENCE.md) —
  `project_context`, symbol lookup, call graph (the tools the
  agent reaches for alongside ATOS)
- Working examples under
  [`.sovereign/atos-demo/`](../../.sovereign/atos-demo/) — the
  charters that provisioned ATOS itself
