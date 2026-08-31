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

- **Prefer the MCP tools over the CLI form.** Both reach the same
  `ToolRegistry::execute()`, but the MCP path is faster and costs fewer tokens.
  `symbols({"name": "ToolRegistry"})` here; `sovereign tools call symbols
  --name=ToolRegistry` is the portable equivalent named in `AGENTS.md`.
- **Subagents are the `Agent` tool.** `Explore` for read-only fan-out searches,
  `general-purpose` for multi-step work. Cap 3 concurrent, launched in one
  message. Delegation is operator-authorized here and standing — see
  `.claude/docs/MAIN_SESSION_PROTOCOL.md` §Delegation.
- **Skills are invoked bare:** `/comaintainer`, `/fieldglass`, `/fleet-report`.
  (pi spells the same skills `/skill:comaintainer`.)
- **Hooks are wired in `.claude/settings.json`** — `SessionStart`,
  `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PreCompact`, `SessionEnd`.
  The scripts they call under `.claude/hooks/` are harness-neutral and shared
  with pi's adapter at `.pi/extensions/sovereign-hooks/`; fix a bug in the
  script, never in one harness's copy.
- **`PostToolUse` runs `format-edited-file.sh` on every `Write`/`Edit`**, so a
  `.rs` file you touch is rustfmt'd the moment you write it and formatting is
  never something you have to think about. Do not hand-format Rust, and do not
  add a fmt step to `sovereign-lint.sh`: `run_gate "rustfmt"` in
  `scripts/pre-push.sh` and CI's blocking `fmt` job remain the backstop for
  edits this hook never sees (human edits, `--no-verify`, other machines).
  The script is harness-neutral, but pi's adapter maps only `session_start`
  and `before_agent_start` today — pi sessions do NOT get auto-formatting until
  someone adds a post-tool mapping there.
- **`/context` shows what actually loaded.** If `AGENTS.md` is not listed under
  Memory files, the import above failed and you are running without the compass —
  stop and say so.
