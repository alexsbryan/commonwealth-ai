# ATOS — Agent Task Orchestration System

You use coding agents for non-trivial work. You've probably been
burned a few times: the agent reversed a decision it made three
turns ago; a reviewer asked "why did we pick X?" and nobody
remembered; a milestone quietly missed its tests and the session
moved on.

ATOS is scaffolding that makes those failure modes visible. It
sits inside your repo at `.sovereign/`. It records what you
agreed, detects when the spec drifts, logs the arguments against
every amendment, and hands a reviewer a one-page audit trail
they can read instead of interviewing you.

You don't configure anything. You write charter markdown. You
commit it. The agent sees the spec in every turn.

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
- You want an adversarial review when you change the charter —
  something that asks "have you considered what this breaks?"
- You're handing a project off (to yourself in six months, to a
  colleague, to a reviewer) and want the artifacts to carry the
  story.

If none of those land — you're on a throwaway, your agent is
disciplined, you don't need an audit trail — you don't need ATOS.

## Use cases — recognize yourself?

### "I want the agent to stay on spec"

Point your agent at the `commonwealth/sovereign-coder` pipeline.
Every turn's system prompt gets the project invariants (from
`CHARTER.md`) and the feature spec (from `spec.md`) prepended.
If anyone — you or the agent — edits the spec without
committing, the next turn's preamble gets a drift warning
pointing at the deviation note.

### "I want an adversarial review when I change the charter"

```
sovereign project amend
```

Opens `CHARTER.md` in `$EDITOR`. On save, ATOS diffs your edit
section-by-section. Invariants changed? You get:

> Which callers / components assume the OLD invariant? Are there
> tests that LOCK it?
>
> What's the replacement invariant, stated as strictly as the
> old one was?

Answer each, then approve. The amendment log, the Q&A, and the
new charter hash are recorded. Future sessions see WHY the
change went through despite the named risks.

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
observes the repo. `sovereign project found` runs a four-stage
conversation and produces `CHARTER.md` + `PHASES.md`. Amendments
go through `sovereign project amend` with adversarial review.
Progression is tracked via `sovereign project phase pass`.

**Feature layer** — inside a founded project (or any repo).
Write `.sovereign/features/<id>/spec.md`; commit it; close
milestones as work lands. Stop conditions run at
end-of-milestone; drift is detected on every agent turn.

The two are orthogonal — you can run the feature layer on a
repo that was never founded. Founding adds a project-wide
charter that the feature spec nests under.

## Mental model

```
charter ──▶ decisions + uncertainties + invariants
   │                       │
   ▼                       ▼
phases ◀────── every agent turn sees the spec
   │                       │
   ▼                       ▼
artifacts (phase-N.md, milestone-N.md, amendment log, notes.db)
                           │
                           ▼
           audit → reviewer reads one page
```

Three terms worth knowing:

**Charter** — the spec at whatever layer. `CHARTER.md` for the
project; `spec.md` for a feature. Committing it is approval.

**Drift** — file changed since approval. Detected by hash
comparison. Drift doesn't block — it surfaces a warning. You
either revert or re-baseline (`atos spec accept` / `project
amend`).

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

Once those three are wired, everything in the two quickstarts
below just works — the agent gets the charter preamble on every
turn with no further configuration.

## Quickstart — greenfield

```
cd my-new-repo
sovereign project init                 # observe
sovereign project found                # 4-stage conversation → CHARTER.md + PHASES.md
# ... write code in opencode ...
sovereign project phase pass 0         # run stop condition, write phase-0.md
```

From the moment `project found` approves, the agent (via the
wired-up pipeline above) sees your charter invariants + current
phase in every turn.

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
# Project layer
sovereign project init                      # observe
sovereign project found [--design <path>]   # once: CHARTER.md + PHASES.md
sovereign project phase status              # where are we?
sovereign project phase pass [N]            # verify + advance
sovereign project amend                     # adversarial charter edit
sovereign project audit                     # one-page reviewer rollup

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

| File | Written by | Tells a reviewer... |
|---|---|---|
| `CHARTER.md` | `project found` / `amend` | What you agreed; amendment log = history of reversals |
| `PHASES.md` | `project found` | How you planned to get there |
| `phase-N.md` | `project phase pass N` | Which phases actually verified green |
| `project.toml` | `init` / `found` / `amend` / `phase` | Observations + lifecycle + charter hash |
| `features/<id>/spec.md` | You | Feature contract |
| `features/<id>/milestone-N.md` | `atos end-milestone` | Which feature milestones actually passed |
| `features/<id>/red-team.md` | Red-team milestone run | Findings grouped by confidence |
| `features/<id>/epistemic-report.md` | `atos teardown` | Promoted notes + postmortem |
| `notes.db` | Agent + CLI | Decisions, invariants, attempts, uncertainties, deviations |

All live under `.sovereign/`. All get indexed into
`project_context`, so `project_context(query: "why UTC")` pulls
the relevant decision without you going to find it.

## What ATOS won't do

- **Write code for you.** That's the agent. ATOS gives the agent
  accountability structure; the code still comes from whoever
  holds the keyboard.
- **Replace testing.** Stop conditions run tests and record
  verdicts; the tests are your job.
- **Police you.** If you don't run `project found`, you never
  get a charter. If you don't commit `spec.md`, the approval
  gate never opens. ATOS surfaces when something's off — it
  doesn't block.

## Read next

- [CLI_REFERENCE.md](CLI_REFERENCE.md) — every flag for every
  subcommand
- [CODE_INTELLIGENCE.md](CODE_INTELLIGENCE.md) —
  `project_context`, symbol lookup, call graph (the tools the
  agent reaches for alongside ATOS)
- Working examples under
  [`.sovereign/atos-demo/`](../../.sovereign/atos-demo/) — the
  charters that provisioned ATOS itself
