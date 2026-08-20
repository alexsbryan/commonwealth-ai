// sovereign-hooks — pi adapter for the repo's harness-neutral hook scripts.
//
// The scripts under `.claude/hooks/` are named for Claude Code but are not
// coupled to it: each reads a four-field JSON envelope (`session_id`, `source`,
// `prompt`, `transcript_path`) on stdin and prints the block to inject. This
// extension is the pi-side adapter that builds that envelope and routes the
// output into context. It contains NO policy — every threshold, budget and
// rendering decision stays in the shared scripts, so a fix lands once for both
// harnesses (AGENTS.md, "Harness capability map").
//
// Mapping:
//   Claude Code SessionStart      -> pi `session_start`
//   Claude Code UserPromptSubmit  -> pi `before_agent_start`
//
// pi has no `transcript_path` in Claude's JSONL format, so `split-enforce.py`
// is handed `context_tokens` from `ctx.getContextUsage()` instead — the policy
// is shared, only the measurement is per-harness.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";

const HOOK_TIMEOUT_MS = 15_000;

/** pi's session_start reasons -> the `source` values the scripts already know. */
const SOURCE_BY_REASON: Record<string, string> = {
  startup: "startup",
  resume: "resume",
  reload: "startup",
  new: "clear",
  fork: "fork",
};

type Envelope = {
  session_id: string;
  source?: string;
  prompt?: string;
  transcript_path?: string;
  context_tokens?: number;
};

/**
 * Run one hook script with the envelope on stdin. Returns its stdout, or "" for
 * any failure at all — a hook must never block or crash a prompt, which is the
 * same contract Claude Code enforces and the reason every script here already
 * fails silently when the daemon is down.
 */
function runHook(cwd: string, script: string, envelope: Envelope, seat: boolean): string {
  const path = join(cwd, ".claude", "hooks", script);
  if (!existsSync(path)) return "";
  const interpreter = script.endsWith(".py") ? "python3" : "sh";
  try {
    const result = spawnSync(interpreter, [path], {
      cwd,
      input: JSON.stringify(envelope),
      encoding: "utf8",
      timeout: HOOK_TIMEOUT_MS,
      env: {
        ...process.env,
        SOVEREIGN_PROJECT_DIR: cwd,
        ...(seat ? { SOVEREIGN_SEAT: "1" } : {}),
      },
    });
    return (result.stdout ?? "").trim();
  } catch {
    return "";
  }
}

export default function (pi: ExtensionAPI) {
  // Boot output is produced at session_start but injected on the first prompt:
  // pi has no context to inject into until an agent turn begins.
  let pendingBoot = "";
  // Sticky, because only the FIRST prompt carries `/skill:comaintainer`. The
  // shared script's other seat signals read a Claude-format transcript that
  // does not exist here, so the adapter owns this bit and passes SOVEREIGN_SEAT=1
  // on every later turn — the script's cheapest and first-checked signal.
  let isSeat = false;

  pi.on("session_start", (event: any, ctx: any) => {
    const sessionId = ctx.sessionManager?.getSessionFile?.() ?? "";
    pendingBoot = runHook(
      ctx.cwd,
      "session-boot.sh",
      {
        session_id: String(sessionId).split("/").pop()?.replace(/\.jsonl$/, "") ?? "",
        source: SOURCE_BY_REASON[event?.reason] ?? "startup",
      },
      isSeat,
    );
  });

  pi.on("before_agent_start", (event: any, ctx: any) => {
    const sessionFile = ctx.sessionManager?.getSessionFile?.() ?? "";
    const sessionId = String(sessionFile).split("/").pop()?.replace(/\.jsonl$/, "") ?? "";
    const prompt = event?.prompt ?? "";

    if (prompt.trimStart().startsWith("/skill:comaintainer")) isSeat = true;

    const envelope: Envelope = {
      session_id: sessionId,
      prompt,
      transcript_path: sessionFile,
      context_tokens: ctx.getContextUsage?.()?.tokens ?? 0,
    };

    const blocks = [
      pendingBoot,
      runHook(ctx.cwd, "inject-notes.py", envelope, isSeat),
      runHook(ctx.cwd, "split-enforce.py", envelope, isSeat),
    ].filter((b) => b.length > 0);

    pendingBoot = "";
    if (blocks.length === 0) return;

    return {
      message: {
        customType: "sovereign-hooks",
        content: blocks.join("\n\n---\n\n"),
        display: true,
      },
    };
  });
}
