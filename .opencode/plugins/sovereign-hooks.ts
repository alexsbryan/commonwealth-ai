// sovereign-hooks — opencode adapter for the repo's harness-neutral hook scripts.
//
// Same contract as the pi adapter (.pi/extensions/sovereign-hooks/index.ts):
// the scripts under `.claude/hooks/` are named for Claude Code but are not
// coupled to it. Each reads a JSON envelope on stdin (or $SOVEREIGN_HOOK_INPUT)
// and prints the block to inject. This file builds that envelope from
// opencode's plugin events and routes the output back into context. It holds
// NO policy — every threshold, budget, formatter table and rendering decision
// stays in the shared scripts, so a fix lands once for all three harnesses
// (AGENTS.md, "Harness capability map").
//
// Event mapping (opencode -> the Claude Code event the scripts were written for):
//
//   chat.message                     -> SessionStart (first message only) + UserPromptSubmit
//   tool.execute.before              -> PreToolUse
//   tool.execute.after               -> PostToolUse
//   experimental.session.compacting  -> PreCompact
//
// TWO ASYMMETRIES, both handled here rather than in the scripts:
//
// 1. opencode has no context channel at PreToolUse — `tool.execute.before` can
//    only mutate the tool's args. So advisory output from prefer-code-intel /
//    prior-art-warn / intent-warn is BUFFERED and flushed into the next
//    chat.message. The advice lands one turn later than under Claude Code.
//    This is safe precisely because all three scripts are advisory by
//    contract ("NEVER BLOCKS. Always exit 0"); a blocking hook could not be
//    adapted this way and must not be added to the buffered set.
//
// 2. opencode names tools in lowercase ("read", "edit"); the scripts filter on
//    Claude Code's casing ("Read", "Edit"). The adapter translates. Keeping
//    the scripts' vocabulary fixed is what lets one script serve every harness.
//
// There is no opencode equivalent of Claude Code's statusLine, so
// read-budget-statusline.py has no wiring here. That is a known gap, not an
// oversight.

import type { Plugin, Hooks } from "@opencode-ai/plugin"
import { spawnSync } from "node:child_process"
import { existsSync } from "node:fs"
import { join } from "node:path"

const HOOK_TIMEOUT_MS = 15_000

/** opencode tool id -> the name the shared scripts match on. */
const TOOL_NAME: Record<string, string> = {
  read: "Read",
  grep: "Grep",
  glob: "Glob",
  bash: "Bash",
  edit: "Edit",
  write: "Write",
  patch: "Edit",
}

type Envelope = Record<string, unknown>

/**
 * Run one hook script and return whatever it printed. Never throws: a hook
 * that fails, times out, or is absent must not break the session — same
 * fail-open contract the scripts hold themselves to.
 */
function runHook(dir: string, script: string, envelope: Envelope): string {
  const path = join(dir, ".claude", "hooks", script)
  if (!existsSync(path)) return ""
  const payload = JSON.stringify(envelope)
  const cmd = script.endsWith(".py") ? "python3" : "sh"
  try {
    const result = spawnSync(cmd, [path], {
      input: payload,
      encoding: "utf8",
      timeout: HOOK_TIMEOUT_MS,
      cwd: dir,
      env: {
        ...process.env,
        SOVEREIGN_HOOK_INPUT: payload,
        SOVEREIGN_PROJECT_DIR: dir,
      },
    })
    return (result.stdout ?? "").trim()
  } catch {
    return ""
  }
}

export const plugin: Plugin = async ({ directory }) => {
  // session-boot runs once, on the first message that gives us a session id.
  let booted = false
  // Advisory output from PreToolUse hooks, flushed into the next message.
  let pending: string[] = []

  const filePathOf = (args: any): string =>
    String(args?.filePath ?? args?.file_path ?? args?.path ?? "")

  const hooks: Hooks = {
    "chat.message": async (input, output) => {
      const blocks: string[] = []
      const prompt = output.parts
        .filter((p: any) => p.type === "text")
        .map((p: any) => p.text)
        .join("\n")

      const envelope: Envelope = {
        session_id: input.sessionID ?? "",
        prompt,
        transcript_path: "",
        source: "startup",
      }

      if (!booted) {
        booted = true
        blocks.push(runHook(directory, "session-boot.sh", envelope))
      }
      blocks.push(runHook(directory, "inject-notes.py", envelope))
      blocks.push(runHook(directory, "split-enforce.py", envelope))

      // Advice buffered from the previous turn's tool calls.
      blocks.push(...pending)
      pending = []

      const text = blocks.filter((b) => b.length > 0).join("\n\n---\n\n")
      if (!text) return

      output.parts.push({
        id: `sovereign-hooks-${Date.now()}`,
        sessionID: input.sessionID,
        messageID: input.messageID ?? "",
        type: "text",
        text,
        synthetic: true,
      } as any)
    },

    "tool.execute.before": async (input, output) => {
      const tool = TOOL_NAME[input.tool.toLowerCase()]
      if (!tool) return
      const envelope: Envelope = {
        session_id: input.sessionID ?? "",
        tool_name: tool,
        tool_input: output.args ?? {},
      }
      const scripts =
        tool === "Edit" || tool === "Write"
          ? ["prior-art-warn.py", "intent-warn.py"]
          : ["prefer-code-intel.py"]
      for (const s of scripts) {
        const out = runHook(directory, s, envelope)
        if (out) pending.push(out)
      }
    },

    "tool.execute.after": async (input, _output) => {
      const tool = TOOL_NAME[input.tool.toLowerCase()]
      if (tool !== "Edit" && tool !== "Write") return
      const file = filePathOf(input.args)
      if (!file) return
      runHook(directory, "format-edited-file.sh", {
        session_id: input.sessionID ?? "",
        tool_name: tool,
        tool_input: { file_path: file },
      })
    },

    "experimental.session.compacting": async (input, output) => {
      const frame = runHook(directory, "session-frame.sh", {
        session_id: input.sessionID ?? "",
        source: "compact",
      })
      if (frame) output.context.push(frame)
    },
  }

  return hooks
}
