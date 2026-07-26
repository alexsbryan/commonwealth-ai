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
# THE PAYLOAD BUDGET IS LOAD-BEARING (MEMORY_MODEL §5 E5, measured 2026-07-26).
# Claude Code spills any hook output over ~10KB to a file and shows the agent
# only a 2KB preview. Sessions then open with `Read <tool-results/hook-*.txt>`
# to get the rest — so an over-budget brief converts itself from budgeted
# context into an UNBUDGETED raw file read, landing in exactly the ramp bucket
# this hook exists to shrink (observed: 40ab6490, 86060bbd, both spilled at
# 11.4KB). Every tier below is capped, and overflow degrades to a pointer the
# agent can dereference on demand (P1) rather than a silent truncation.
#
# DEPENDABILITY CONTRACT (same discipline as inject-notes.sh): every failure
# mode degrades to a distinct, honest one-line status — never a silent skip,
# never a lie. Opt out with SOVEREIGN_NO_BOOT_BRIEF=1.
#
# stdin is the SessionStart envelope (JSON with `session_id`). We capture it
# before handing the heredoc to python, and record what we injected to
# ~/.sovereign/sessions/<session_id>/boot.json — the provenance that lets
# `cache-audit --ramp --classify` tell "re-read what the frame already had"
# from "genuine new-task acquisition". Without it, no honest classifier exists.

[ -n "$SOVEREIGN_NO_BOOT_BRIEF" ] && exit 0

export SOVEREIGN_HOOK_INPUT="$(cat)"
export SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}"
export SOVEREIGN_NO_STALE_WARN=1

exec python3 - <<'PY' 2>/dev/null
import glob
import json
import os
import re
import subprocess
import time
import urllib.request

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
BASE = f"http://localhost:{PORT}"

# Harness spill threshold measured at ~10KB (smallest observed spill 9.8KB
# across 80 transcripts). Stay well under it: the cost of being 2KB short is
# one dereference; the cost of being 1 byte over is the whole payload turning
# into a raw file read.
TOTAL_BUDGET_CHARS = int(os.environ.get("SOVEREIGN_BOOT_BUDGET_CHARS", "8000"))
FRAME_BUDGET_CHARS = int(os.environ.get("SOVEREIGN_BOOT_FRAME_CHARS", "4500"))
BRIEF_MIN_CHARS = 800
FRAME_MAX_AGE_DAYS = 14

SESSIONS_ROOT = os.path.expanduser("~/.sovereign/sessions")

try:
    envelope = json.loads(os.environ.get("SOVEREIGN_HOOK_INPUT") or "{}")
except Exception:
    envelope = {}
session_id = (envelope.get("session_id") or "").strip()

# What we injected, for boot.json. Every field is observable fact, not intent.
prov = {
    "ts": int(time.time()),
    "session_id": session_id,
    # startup | resume | clear | compact. On resume/compact the session's OWN
    # frame is the correct handoff and newest-mtime picks it naturally; on
    # startup it cannot exist yet. `frame_is_own` disambiguates the two cases
    # so the mis-injection rate isn't polluted by legitimate self-resumes.
    "source": envelope.get("source") or "",
    "budget_chars": TOTAL_BUDGET_CHARS,
    "frame_is_own": False,
    "frame_session": None,
    "frame_age_s": None,
    "frame_provenance": None,
    "frame_chars_full": 0,
    "frame_chars_injected": 0,
    "frame_truncated": False,
    "frame_candidates": 0,
    "frame_selection": "newest_mtime",
    "brief_chars": 0,
    "brief_truncated": False,
    "payload_chars": 0,
}

out = []


def emit(text):
    out.append(text)


emit("## Sovereign session boot (injected by session-boot.sh)\n")

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
        emit(f"_brain: daemon up · {len(tools)} MCP tools live "
             f"(symbols/callers/facts/code_search/notes are cheaper and exact — "
             f"prefer them over raw Read/grep)_\n")
    except Exception as e:
        emit(f"_brain: daemon up but MCP tools/list failed ({type(e).__name__}) — "
             f"CLI fallback: `sovereign tools call <id>`_\n")
except Exception:
    emit(f"_brain: daemon not reachable on :{PORT} — code intel is dark; "
         f"start it: `sovereign daemon start`; `sovereign doctor` diagnoses_\n")

# ── Tier 2: latest session frame (the handoff) ──────────────────────────
#
# Selection is still newest-mtime — a KNOWN gap (MEMORY_MODEL §5 E5 R1): under
# concurrent workstreams the newest frame is often another thread's, and the
# successor pays ramp tokens hunting for the right one. `frame_selection` and
# `frame_candidates` are recorded so the mis-injection rate is measurable
# BEFORE we redesign selection.
frames = glob.glob(os.path.join(SESSIONS_ROOT, "*", "frame.md"))
fresh = [(os.path.getmtime(p), p) for p in frames
         if time.time() - os.path.getmtime(p) < FRAME_MAX_AGE_DAYS * 86400]
prov["frame_candidates"] = len(fresh)
if fresh:
    mtime, path = max(fresh)
    age_h = (time.time() - mtime) / 3600
    prov["frame_session"] = os.path.basename(os.path.dirname(path))
    prov["frame_is_own"] = prov["frame_session"] == session_id
    prov["frame_age_s"] = int(time.time() - mtime)
    try:
        with open(path) as f:
            frame = f.read()
        prov["frame_chars_full"] = len(frame)
        m = re.search(r"^provenance:\s*(\S+)", frame, re.M)
        prov["frame_provenance"] = m.group(1) if m else "unknown"
        if len(frame) > FRAME_BUDGET_CHARS:
            frame = (frame[:FRAME_BUDGET_CHARS].rstrip()
                     + f"\n\n_[frame truncated at {FRAME_BUDGET_CHARS} chars — "
                       f"read the rest on demand: `Read {path}`]_")
            prov["frame_truncated"] = True
        prov["frame_chars_injected"] = len(frame)
        emit(f"### Latest session frame ({age_h:.0f}h old — cross-check `## Next` "
             f"against recent commits before acting)\n")
        emit(frame)
        emit("")
        if prov["frame_candidates"] > 1 and not prov["frame_is_own"]:
            emit(f"_{prov['frame_candidates']} frames are live; this is the most "
                 f"recently written, NOT necessarily your predecessor's. If it "
                 f"describes work you are not continuing, ignore it — "
                 f"`ls -t ~/.sovereign/sessions/*/frame.md` lists the others._\n")
    except Exception as e:
        emit(f"_frame at {path} unreadable ({type(e).__name__})_\n")
# No fresh frame is normal (first boot, or >14d idle) — say nothing.

# ── Tier 1: working-set brief ───────────────────────────────────────────
spent = sum(len(p) + 1 for p in out)
brief_budget = max(BRIEF_MIN_CHARS, TOTAL_BUDGET_CHARS - spent)
try:
    proc = subprocess.run(
        ["sovereign", "code", "brief", "--strategy", "recent", "--hours", "48",
         "--budget", "1200"],
        capture_output=True, text=True, timeout=15,
    )
    if proc.returncode == 0 and proc.stdout.strip():
        brief = proc.stdout.strip()
        if len(brief) > brief_budget:
            brief = (brief[:brief_budget].rstrip()
                     + "\n\n_[brief truncated to stay under the hook payload "
                       "budget — full: `sovereign code brief --hours 48`]_")
            prov["brief_truncated"] = True
        prov["brief_chars"] = len(brief)
        emit(brief)
    else:
        err = (proc.stderr or proc.stdout).strip().splitlines()
        emit(f"_working-set brief unavailable (sovereign code brief exit "
             f"{proc.returncode}: {err[-1][:120] if err else 'no output'})_")
except FileNotFoundError:
    emit("_working-set brief unavailable (`sovereign` not on PATH — "
         "ln -sf $(realpath sovereign/target/debug/sovereign-cli) ~/.local/bin/sovereign)_")
except subprocess.TimeoutExpired:
    emit("_working-set brief unavailable (sovereign code brief timed out at 15s)_")

payload = "\n".join(out)
prov["payload_chars"] = len(payload)
print(payload)

# ── Provenance sidecar (fail-silent: never break the boot) ──────────────
if session_id:
    try:
        d = os.path.join(SESSIONS_ROOT, session_id)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "boot.json"), "w", encoding="utf-8") as f:
            json.dump(prov, f)
    except OSError:
        pass
PY
