#!/bin/sh
# sovereign session-boot — SessionStart hook for Claude Code.
#
# The zero-friction boot: instead of asking every agent to run a session-start
# checklist (recent_changes, project_context, notes, drift_posture, …) and
# read two architecture docs, inject one budgeted artifact at session start:
#
#   Tier 0 — brain health: is the daemon up, how many MCP tools are live.
#   Tier 2 — the latest session frame (if fresh): the predecessor session's
#            goal / position / next actions, so a new window resumes work
#            instead of re-deriving it. See docs/specs/SESSION_CONTINUITY.md.
#   Tier 1 — the working-set brief (`sovereign code brief`): recent activity,
#            relevant notes, drift posture — token-budgeted.
#
# DEPENDABILITY CONTRACT (same discipline as inject-notes.sh): every failure
# mode degrades to a distinct, honest one-line status — never a silent skip,
# never a lie. Opt out with SOVEREIGN_NO_BOOT_BRIEF=1.

[ -n "$SOVEREIGN_NO_BOOT_BRIEF" ] && exit 0

export SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}"
export SOVEREIGN_NO_STALE_WARN=1

exec python3 - <<'PY' 2>/dev/null
import glob
import json
import os
import subprocess
import time
import urllib.request

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
BASE = f"http://localhost:{PORT}"
# The frame is <=2k tokens by schema; this is a hard backstop, not a budget.
FRAME_CHAR_CAP = 12_000
FRAME_MAX_AGE_DAYS = 14

print("## Sovereign session boot (injected by session-boot.sh)\n")

# ── Tier 0: brain health ────────────────────────────────────────────────
try:
    urllib.request.urlopen(f"{BASE}/status", timeout=2).read(1)
    body = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
    }).encode()
    req = urllib.request.Request(
        f"{BASE}/mcp", data=body, headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=4) as r:
            tools = json.loads(r.read().decode()).get("result", {}).get("tools", [])
        print(f"_brain: daemon up · {len(tools)} MCP tools live "
              f"(symbols/callers/facts/code_search/notes are cheaper and exact — "
              f"prefer them over raw Read/grep)_\n")
    except Exception as e:
        print(f"_brain: daemon up but MCP tools/list failed ({type(e).__name__}) — "
              f"CLI fallback: `sovereign tools call <id>`_\n")
except Exception:
    print(f"_brain: daemon not reachable on :{PORT} — code intel is dark; "
          f"start it: `sovereign daemon start`; `sovereign doctor` diagnoses_\n")

# ── Tier 2: latest session frame (the handoff) ──────────────────────────
frames = glob.glob(os.path.expanduser("~/.sovereign/sessions/*/frame.md"))
fresh = [(os.path.getmtime(p), p) for p in frames
         if time.time() - os.path.getmtime(p) < FRAME_MAX_AGE_DAYS * 86400]
if fresh:
    mtime, path = max(fresh)
    age_h = (time.time() - mtime) / 3600
    try:
        with open(path) as f:
            frame = f.read()
        if len(frame) > FRAME_CHAR_CAP:
            frame = frame[:FRAME_CHAR_CAP] + "\n[frame truncated at backstop cap]"
        print(f"### Latest session frame ({age_h:.0f}h old — cross-check `## Next` "
              f"against recent commits before acting)\n")
        print(frame)
        print()
    except Exception as e:
        print(f"_frame at {path} unreadable ({type(e).__name__})_\n")
# No fresh frame is normal (first boot, or >14d idle) — say nothing.

# ── Tier 1: working-set brief ───────────────────────────────────────────
try:
    out = subprocess.run(
        ["sovereign", "code", "brief", "--strategy", "recent", "--hours", "48",
         "--budget", "1200"],
        capture_output=True, text=True, timeout=15,
    )
    if out.returncode == 0 and out.stdout.strip():
        print(out.stdout.strip())
    else:
        err = (out.stderr or out.stdout).strip().splitlines()
        print(f"_working-set brief unavailable (sovereign code brief exit "
              f"{out.returncode}: {err[-1][:120] if err else 'no output'})_")
except FileNotFoundError:
    print("_working-set brief unavailable (`sovereign` not on PATH — "
          "ln -sf $(realpath sovereign/target/release/sovereign-cli) ~/.local/bin/sovereign)_")
except subprocess.TimeoutExpired:
    print("_working-set brief unavailable (sovereign code brief timed out at 15s)_")
PY
