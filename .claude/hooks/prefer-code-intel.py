#!/usr/bin/env python3
"""PreToolUse advisory: point agents at the BEST AVAILABLE context path.

WHY. Audited sessions on this repo acquire codebase understanding almost
entirely through raw file reads (Read / `cat` / `grep`) and make ~zero
`symbols`/`callers`/`code_search`/`notes` calls. Every raw read then rides the
cache-read tail for the rest of the session (see `sovereign cache-audit`). This
hook surfaces the distilled path at the moment the raw-acquisition *pattern*
emerges.

FORGIVING BY DESIGN. The hook probes whether code intelligence is actually
reachable before recommending it. Advising `symbols(...)` while the daemon is
down is worse than saying nothing: the agent burns a turn on a tool that cannot
answer, distrusts the guidance, and falls back to raw reads anyway — now with
less confidence than before. So the advice degrades along a ladder:

  daemon up      → MCP code intel (symbols / callers / code_search / notes)
  daemon down,
  CLI present    → `sovereign tools call <id>` (same registry, no daemon)
  neither        → targeted raw reads + delegate fan-out to a subagent,
                   plus the one command that repairs the tier above

BEHAVIOUR. Never blocks. Counts raw-source-acquisition events per session and
injects a single advisory once the pattern is clear (the Nth such event), then
stays quiet so it never nags. Registered via a PreToolUse matcher on
Read / Grep / Bash in .claude/settings.json — inert until then.

Contract: reads the hook payload as JSON on stdin, emits (on exit 0) a JSON
object with hookSpecificOutput.additionalContext to add advice without blocking.
"""

import json
import os
import shutil
import socket
import sys
import time
from pathlib import Path

# Nudge once the raw-acquisition pattern is established, not on the first
# incidental read. Fires exactly once per session (at this Nth event).
NUDGE_AT = 3

# Liveness probe. Kept tight: this runs inline on a PreToolUse and must never
# be something the agent feels. Result is cached so we probe at most once per
# PROBE_TTL_SECS regardless of how many tool calls fly past.
DAEMON_HOST = "127.0.0.1"
DAEMON_PORT = 9741
PROBE_TIMEOUT_SECS = 0.25
PROBE_TTL_SECS = 30

SOURCE_EXTS = (
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java",
    ".c", ".cc", ".cpp", ".h", ".hpp", ".svelte", ".rb",
)
BASH_READ_TOKENS = ("cat ", "head ", "tail ", "sed -n", "grep ", "rg ", "less ")

# Tier 1 — code intelligence is live over MCP.
ADVICE_MCP = (
    "Context tip (advisory, not a rule): you've been acquiring source context "
    "via raw reads, and code intelligence is up on this machine. It's usually "
    "cheaper and exact: `symbols(\"Name\")` for a definition, "
    "`callers(\"fn\")` / `callees(\"fn\")` for the call graph (compiler-resolved "
    "via SCIP — catches dispatch that grep misses), `code_search(\"...\")` for "
    "concepts, `notes(query:\"...\")` for prior decisions. Whole-file Reads ride "
    "the cache-read tail for the rest of the session, so prefer a distilled "
    "query or a tight 15-25 line slice around a `symbols` hit. Raw reads are "
    "still the right call for config, docs, and files you're about to edit — "
    "use your judgement. (`sovereign cache-audit` shows your own spend.)"
)

# Tier 2 — daemon unreachable, but the CLI can serve the same registry.
ADVICE_CLI = (
    "Context tip (advisory, not a rule): you've been acquiring source context "
    "via raw reads. Heads up — the Sovereign daemon is NOT reachable on "
    ":9741, so the MCP code-intel tools (`symbols`, `callers`, `code_search`, "
    "`notes`) will fail if you call them. Do not burn turns on them. The CLI "
    "hits the same tool registry without the daemon:\n"
    "  sovereign tools call symbols --name=TypeName\n"
    "  sovereign tools call code_search --query=\"...\"\n"
    "If those also fail, the repair is:\n"
    "  cargo build -p sovereign-cli --features dev-tools -p sovereign-cli-dev "
    "-p sovereign-cli-daemon -p sovereign-cli-llm && sovereign daemon start\n"
    "Until then, targeted raw reads are the correct fallback — you are not "
    "doing it wrong. Prefer Glob+Grep to locate, then Read a tight slice."
)

# Tier 3 — no code intel at all. Say so plainly and name the next-best path.
ADVICE_DEGRADED = (
    "Context tip (advisory, not a rule): you've been acquiring source context "
    "via raw reads. Heads up — code intelligence is DARK on this machine (no "
    "daemon on :9741, no `sovereign` CLI on PATH), so `symbols` / `callers` / "
    "`code_search` / `notes` cannot answer. Do not burn turns on them, and "
    "don't treat this as a discipline failure: raw reads ARE the correct path "
    "right now. Next-best ladder:\n"
    "  1. Glob/Grep to locate, then Read a tight 15-25 line slice — not the "
    "whole file.\n"
    "  2. For anything needing a sweep across many files or naming "
    "conventions, delegate to an Explore subagent (up to 3 in parallel) so the "
    "file dumps land in ITS context, not yours.\n"
    "  3. Mention to the operator that code intel is down; the repair is "
    "`cargo build -p sovereign-cli --features dev-tools -p sovereign-cli-dev "
    "-p sovereign-cli-daemon -p sovereign-cli-llm && sovereign daemon start`."
)


def is_raw_source_acquisition(tool_name: str, tool_input: dict) -> bool:
    if tool_name == "Grep":
        return True
    if tool_name == "Read":
        fp = str(tool_input.get("file_path", ""))
        return fp.endswith(SOURCE_EXTS)
    if tool_name == "Bash":
        cmd = str(tool_input.get("command", ""))
        return any(tok in cmd for tok in BASH_READ_TOKENS)
    return False


def daemon_is_up() -> bool:
    try:
        with socket.create_connection(
            (DAEMON_HOST, DAEMON_PORT), timeout=PROBE_TIMEOUT_SECS
        ):
            return True
    except OSError:
        return False


def cli_is_present() -> bool:
    if shutil.which("sovereign") or shutil.which("sovereign-cli"):
        return True
    # The dispatcher is often only on the debug build path, not on PATH.
    project = os.environ.get("CLAUDE_PROJECT_DIR", "")
    if project and (Path(project) / "target/debug/sovereign-cli").exists():
        return True
    return False


def choose_advice(state_dir: Path) -> str:
    """Pick the advice tier, caching the probe so we don't stat the network
    on every single tool call."""
    cache = state_dir / "tier.probe"
    try:
        if time.time() - cache.stat().st_mtime < PROBE_TTL_SECS:
            tier = cache.read_text().strip()
            if tier in ("mcp", "cli", "degraded"):
                return {"mcp": ADVICE_MCP, "cli": ADVICE_CLI}.get(
                    tier, ADVICE_DEGRADED
                )
    except (OSError, ValueError):
        pass

    if daemon_is_up():
        tier, advice = "mcp", ADVICE_MCP
    elif cli_is_present():
        tier, advice = "cli", ADVICE_CLI
    else:
        tier, advice = "degraded", ADVICE_DEGRADED

    try:
        cache.write_text(tier)
    except OSError:
        pass
    return advice


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return 0  # never block on a parse problem

    tool_name = data.get("tool_name", "")
    tool_input = data.get("tool_input", {}) or {}
    session_id = str(data.get("session_id", "unknown"))

    if not is_raw_source_acquisition(tool_name, tool_input):
        return 0

    # Per-session counter of raw-acquisition events. session_id is stable for
    # the life of a session; a stray file just accumulates and is swept below.
    state_dir = Path(os.path.expanduser("~/.cache/sovereign/code-intel-nudge"))
    try:
        state_dir.mkdir(parents=True, exist_ok=True)
    except OSError:
        return 0
    counter = state_dir / f"{session_id}.count"
    fired = state_dir / f"{session_id}.fired"

    if fired.exists():
        return 0  # already advised this session

    n = 0
    try:
        n = int(counter.read_text().strip() or "0")
    except (OSError, ValueError):
        n = 0
    n += 1
    try:
        counter.write_text(str(n))
    except OSError:
        pass

    if n < NUDGE_AT:
        return 0

    try:
        fired.touch()
    except OSError:
        pass

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "additionalContext": choose_advice(state_dir),
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
