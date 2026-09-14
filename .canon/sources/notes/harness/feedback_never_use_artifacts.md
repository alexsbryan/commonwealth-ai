# Never publish Claude Artifacts — deliverables go in the repo as files, always

Never call the Artifact tool. No exceptions, no "but this one has an
audience", no offering it as an option. Stated emphatically 2026-08-26 when a
plan document was about to be published as an artifact.

Why: the operator works across several machines and harnesses (Claude Code,
pi, Codex) on a mesh. An artifact lives on claude.ai, outside git, invisible to
every peer session and to the repo's own history. A plan or report that isn't a
file in the repo cannot be read by the next session, diffed, or burned down.

How to apply: written deliverables — plans, pre-registrations, reports —
go in the repo as `.md`, or into the scratchpad when they are working prose.
Recall that `AGENTS.md` §"Ship code, not prose" already limits which documents
belong in the repo at all: pre-registrations and bars KEEP, narrative about work
in flight does not. Terminal prose is the default for a verdict; a file is the
default for anything durable. See [[feedback-ship-code-not-prose]].
