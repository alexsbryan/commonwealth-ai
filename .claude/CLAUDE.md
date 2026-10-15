@../AGENTS.md

<!--
  The compass lives in the repo-root AGENTS.md — the cross-harness standard that
  pi, Codex and the other agent CLIs read directly. Claude Code does not read
  AGENTS.md, so this file imports it. ONE source, no per-harness copy.

  Add content below ONLY when it is true of Claude Code and false of every other
  harness. Anything else belongs in AGENTS.md, where every harness sees it.
-->

## Claude Code specifics

Everything above came from `AGENTS.md` and applies in every harness. These are
this harness's own affordances.

- **Prefer the MCP tools over the CLI form.** Both run the same `Tool::execute`
  body from the same registry (there is no `ToolRegistry::execute` — see the
  note in `AGENTS.md`), and the MCP path is faster and costs fewer tokens.
  `symbols({"name": "ToolRegistry"})` here; `sovereign tools call symbols
  --name=ToolRegistry` is the portable equivalent named in `AGENTS.md`.
- **Subagents are the `Agent` tool.** `Explore` (Sonnet, read-only) for fan-out
  searches, `worker` (Opus, medium effort) for anything that edits or builds —
  not `general-purpose`, which carries ~54k of inherited context against the
  worker's ~6k. Both are defined in `.claude/agents/` and skip CLAUDE.md, so the
  brief must carry what the task needs. Up to 2 concurrent without asking, 3-4
  after asking the operator, launched in one message. Delegation is
  operator-authorized here and standing — see
  `.claude/docs/MAIN_SESSION_PROTOCOL.md` §Delegation.
- **Skills are invoked bare:** `/comaintainer`, `/fieldglass`, `/fleet-report`.
  (pi spells the same skills `/skill:comaintainer`.)
- **Hooks are wired in `.claude/settings.json`** — read it for the live set;
  one of them rustfmts every `.rs` you write. The scripts under
  `.claude/hooks/` are harness-neutral and shared with pi's adapter at
  `.pi/extensions/sovereign-hooks/`; fix a bug in the script, never in one
  harness's copy.
- **`/context` shows what actually loaded.** If `AGENTS.md` is not listed under
  Memory files, the import above failed and you are running without the compass —
  stop and say so.
