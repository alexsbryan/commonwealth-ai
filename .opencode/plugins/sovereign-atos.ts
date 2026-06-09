// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * sovereign-atos — opencode plugin that plumbs an opencode session into
 * the ATOS run ledger + digest preamble.
 *
 * Reads two environment variables set by `sovereign atos start-milestone`:
 *
 *   SOVEREIGN_FEATURE_ID    — active feature id (same one the sovereign
 *                             inject-notes.sh hook filters on).
 *   ATOS_RUN_ID             — atos_runs row id this session's tool calls
 *                             should be attributed to.
 *
 * Both are optional. Absent → plugin degrades silently to a no-op, so
 * running opencode outside of ATOS (e.g. ad-hoc chat) costs nothing.
 *
 * Three hooks:
 *
 *   experimental.session.compacting
 *      → fetches `read_note_digest(feature_id)` from the MCP server and
 *        appends it as a compaction context string. Opencode then uses
 *        that context when regenerating its post-compaction summary.
 *
 *   tool.execute.before
 *      → POSTs a `record_atos_event(phase="before", args_json=...)` row
 *        so we have the args the driver intended to send, even if the
 *        tool later errors or times out.
 *
 *   tool.execute.after
 *      → POSTs a `record_atos_event(phase="after", outcome=..., duration_ms=...)`
 *        keyed by the same call id. The sovereign atos diff view joins
 *        before/after pairs on call_id.
 *
 * Design rules:
 *   - Every MCP call is fire-and-forget from opencode's perspective —
 *     plugin failures must NEVER break the session. Wrap in try/catch
 *     and log to stderr.
 *   - 1.5s timeout on each POST. Opencode's hook runtime doesn't love
 *     long-running plugin code; a stuck MCP server shouldn't stall
 *     the user's tool execution.
 *   - First event of a run attaches sessionID; later events skip the
 *     session_id field. Server-side `set_run_session` is idempotent
 *     so a duplicate send is harmless.
 */

import type { Plugin } from "@opencode-ai/plugin"

const MCP_URL = process.env.SOVEREIGN_MCP_URL ?? "http://localhost:9741/mcp"
const FEATURE_ID = process.env.SOVEREIGN_FEATURE_ID ?? ""
const RUN_ID = process.env.ATOS_RUN_ID ?? ""
const RPC_TIMEOUT_MS = 1500

// Per-run state. Track (a) whether we've attached the sessionID yet and
// (b) start timestamps for in-flight call_ids so `after` events get a
// duration_ms — opencode doesn't give us one directly.
let sessionAttached = false
const callStartMs = new Map<string, number>()

async function mcpCall(name: string, args: Record<string, unknown>): Promise<void> {
    if (!RUN_ID) return
    const body = {
        jsonrpc: "2.0",
        id: Date.now(),
        method: "tools/call",
        params: { name, arguments: args },
    }
    const ctrl = new AbortController()
    const t = setTimeout(() => ctrl.abort(), RPC_TIMEOUT_MS)
    try {
        await fetch(MCP_URL, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
            signal: ctrl.signal,
        })
    } catch (e) {
        // Plugin must never break the session — log and move on.
        console.error(`[sovereign-atos] MCP ${name} failed:`, (e as Error).message)
    } finally {
        clearTimeout(t)
    }
}

async function fetchDigest(): Promise<string | null> {
    if (!FEATURE_ID) return null
    const body = {
        jsonrpc: "2.0",
        id: Date.now(),
        method: "tools/call",
        params: {
            name: "read_note_digest",
            arguments: { feature_id: FEATURE_ID, scope: ["global", "feature"] },
        },
    }
    const ctrl = new AbortController()
    const t = setTimeout(() => ctrl.abort(), RPC_TIMEOUT_MS * 2)
    try {
        const res = await fetch(MCP_URL, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
            signal: ctrl.signal,
        })
        if (!res.ok) return null
        const outer = await res.json() as {
            result?: { content?: Array<{ type: string, text: string }> }
        }
        const txt = outer?.result?.content?.[0]?.text
        if (!txt) return null
        // The tool returns JSON wrapped in a text field. Unwrap it so
        // we hand opencode markdown, not a JSON string.
        try {
            const parsed = JSON.parse(txt) as { digest_md?: string }
            return parsed.digest_md ?? null
        } catch {
            return txt
        }
    } catch (e) {
        console.error("[sovereign-atos] fetch digest failed:", (e as Error).message)
        return null
    } finally {
        clearTimeout(t)
    }
}

// ─── M4: branch-based feature discovery for chat.headers ────────────────────
//
// The `chat.headers` hook fires on every /v1/chat/completions request
// opencode sends. We inject `X-Feature-Id` + `X-Session-Id` so the
// Commonwealth middleware knows which feature this session is working
// on — no env var ceremony, no approval token to paste.
//
// Discovery: run `git rev-parse --show-toplevel` to find the repo
// root, then `ls .sovereign/features/` to find a single feature with
// a committed spec.md on the current branch. Ambiguous (zero or
// multiple features) → no header; legacy routing takes over.
//
// The result is cached for the plugin's lifetime so we don't shell
// out to git on every hook fire.

let cachedFeatureId: string | null | undefined = undefined

async function discoverFeatureFromGit(cwd: string): Promise<string | null> {
    try {
        const repo = await runCapture("git", ["rev-parse", "--show-toplevel"], cwd)
        if (!repo) return null
        const featuresDir = `${repo}/.sovereign/features`
        const readDir = await import("node:fs/promises")
        const entries = await readDir.readdir(featuresDir).catch(() => [] as string[])
        // Keep only entries whose spec.md exists on the current branch.
        const candidates: string[] = []
        for (const entry of entries) {
            const specPath = `${featuresDir}/${entry}/spec.md`
            const log = await runCapture(
                "git",
                ["log", "-1", "--format=%H", "--", `.sovereign/features/${entry}/spec.md`],
                repo,
            )
            if (log) {
                const onDisk = await readDir.stat(specPath).catch(() => null)
                if (onDisk?.isFile()) candidates.push(entry)
            }
        }
        if (candidates.length === 1) return candidates[0]
        if (candidates.length > 1) {
            console.error(
                `[sovereign-atos] ambiguous branch: ${candidates.length} features match; omitting X-Feature-Id`,
            )
        }
        return null
    } catch {
        return null
    }
}

async function runCapture(cmd: string, args: string[], cwd: string): Promise<string | null> {
    const { spawn } = await import("node:child_process")
    return new Promise((resolve) => {
        const proc = spawn(cmd, args, { cwd, stdio: ["ignore", "pipe", "ignore"] })
        let buf = ""
        proc.stdout.on("data", (d) => (buf += d.toString()))
        proc.on("close", (code) => resolve(code === 0 ? buf.trim() : null))
        proc.on("error", () => resolve(null))
    })
}

export const plugin: Plugin = async () => {
    // M4: the plugin is active EVEN when env vars are absent — the
    // chat.headers hook discovers the feature from git. M2-era
    // env-driven flow still works as a fallback (when git discovery
    // fails but $SOVEREIGN_FEATURE_ID is set).
    // Only the tool-event hooks are RUN_ID-gated (they need the run
    // id to record against).
    return {
        "chat.headers": async (input, output) => {
            if (cachedFeatureId === undefined) {
                // Prefer explicit env (M2-era convention).
                if (FEATURE_ID) {
                    cachedFeatureId = FEATURE_ID
                } else {
                    cachedFeatureId = await discoverFeatureFromGit(process.cwd())
                }
            }
            if (cachedFeatureId) {
                output.headers["X-Feature-Id"] = cachedFeatureId
                output.headers["X-Session-Id"] = input.sessionID
            }
        },

        "experimental.session.compacting": async (_input, output) => {
            const digest = await fetchDigest()
            if (digest) {
                // Prepend so the digest lands above opencode's default
                // compaction prompt context — the agent sees durable
                // decisions first.
                output.context.unshift(digest)
            }
        },

        "tool.execute.before": async (input, output) => {
            callStartMs.set(input.callID, Date.now())
            const args = {
                run_id: RUN_ID,
                call_id: input.callID,
                tool_name: input.tool,
                phase: "before",
                args_json: safeStringify(output.args),
                ...(sessionAttached ? {} : { session_id: input.sessionID }),
            }
            sessionAttached = true
            await mcpCall("record_atos_event", args)
        },

        "tool.execute.after": async (input, output) => {
            const started = callStartMs.get(input.callID)
            const duration = started === undefined ? undefined : Date.now() - started
            callStartMs.delete(input.callID)
            // Opencode doesn't expose a structured 'outcome' yet — we
            // infer from output.output heuristically: empty string means
            // the tool returned nothing; otherwise call it success. The
            // plugin deliberately does NOT try to parse for errors —
            // that would second-guess opencode's own signal. When the
            // opencode team lands a dedicated error-phase hook we'll
            // distinguish.
            const outcome = typeof output.output === "string" && output.output.length === 0
                ? "empty_result"
                : "success"
            await mcpCall("record_atos_event", {
                run_id: RUN_ID,
                call_id: input.callID,
                tool_name: input.tool,
                phase: "after",
                outcome,
                duration_ms: duration,
            })
        },
    }
}

function safeStringify(v: unknown): string {
    try {
        return JSON.stringify(v)
    } catch {
        return "<unserializable>"
    }
}
