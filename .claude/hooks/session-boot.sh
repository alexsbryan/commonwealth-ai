#!/bin/sh
# sovereign session-boot — SessionStart hook for Claude Code.
#
# The zero-friction boot: instead of asking every agent to run a session-start
# checklist (recent_changes, project_context, notes, drift_posture, …) and
# read two architecture docs, inject one budgeted artifact at session start:
#
#   Tier 0 — brain health: is the daemon up, how many MCP tools are live.
#   Tier 2 — the session-frame INDEX: one line per live frame, so a new window
#            can see what work is in flight and dereference the frame it is
#            actually continuing (`sovereign session frames <id>`). A resumed
#            session gets its OWN frame injected whole instead — no selection
#            is involved there, so none can be wrong.
#            See docs/specs/SESSION_CONTINUITY.md and MEMORY_MODEL §5 E5.
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
    # index | own_full — which of the two Tier-2 shapes this boot injected.
    # Phase 1 recorded "newest_mtime" here; that selector is gone.
    "frame_selection": "none",
    "repo": "",
    "branch": "",
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

# ── Tier 2: the frame INDEX (the handoff pointer) ───────────────────────
#
# WHAT CHANGED AND WHY (MEMORY_MODEL §5 E5 Phase 2). This tier used to inject
# the frame with the newest mtime, whole. Under concurrent workstreams that is
# the successor's frame only by luck, and a WRONG frame costs more than none:
# session 40ab6490 was handed another thread's and burned 5,872 ramp tokens
# hunting for the right one by hand.
#
# SessionStart has no prompt, which is *why* selection fell back to recency —
# there is nothing here to select against. So it no longer tries. It injects
# the index: one line per live frame (~200 tokens for the lot, against 1–2k
# for one possibly-wrong frame), each dereferenceable with
# `sovereign session frames <id>`. Full-frame injection moves to the first
# UserPromptSubmit, where the prompt exists (inject-notes.sh).
#
# The ONE case that still injects a frame whole is a resume/compact of a
# session's OWN frame: no selection is involved, so no selection can be wrong.


def git(*args):
    try:
        p = subprocess.run(["git", *args], capture_output=True, text=True, timeout=3)
        return p.stdout.strip() if p.returncode == 0 else ""
    except Exception:
        return ""


repo = os.path.basename(git("rev-parse", "--show-toplevel"))
branch = git("rev-parse", "--abbrev-ref", "HEAD")
prov["repo"] = repo
prov["branch"] = branch

# Own frame on resume/compact — inject it whole, it is definitionally correct.
own = os.path.join(SESSIONS_ROOT, session_id, "frame.md") if session_id else ""
if own and os.path.isfile(own):
    try:
        with open(own) as f:
            frame = f.read()
        prov["frame_selection"] = "own_full"
        prov["frame_session"] = session_id
        prov["frame_is_own"] = True
        prov["frame_age_s"] = int(time.time() - os.path.getmtime(own))
        prov["frame_chars_full"] = len(frame)
        m = re.search(r"^provenance:\s*(\S+)", frame, re.M)
        prov["frame_provenance"] = m.group(1) if m else "unknown"
        if len(frame) > FRAME_BUDGET_CHARS:
            frame = (frame[:FRAME_BUDGET_CHARS].rstrip()
                     + f"\n\n_[frame truncated at {FRAME_BUDGET_CHARS} chars — "
                       f"read the rest on demand: `Read {own}`]_")
            prov["frame_truncated"] = True
        prov["frame_chars_injected"] = len(frame)
        emit("### Your own session frame (resumed — this is the state you banked)\n")
        emit(frame)
        emit("")
    except Exception as e:
        emit(f"_own frame at {own} unreadable ({type(e).__name__})_\n")
else:
    prov["frame_selection"] = "index"
    try:
        p = subprocess.run(
            ["sovereign", "session", "frames", "--json",
             "--repo", repo, "--branch", branch,
             "--limit", "8", "--max-age-days", str(FRAME_MAX_AGE_DAYS)],
            capture_output=True, text=True, timeout=10,
        )
        doc = json.loads(p.stdout) if p.returncode == 0 and p.stdout.strip() else {}
        cands = doc.get("candidates") or []
        prov["frame_candidates"] = doc.get("count", len(cands))
        if cands:
            # Rendered here rather than shelling a second time for the human
            # view; `sovereign session frames` is the authoritative renderer
            # and prints the same facts.
            lines = [f"### Live session frames ({prov['frame_candidates']}) — "
                     f"read one in full: `sovereign session frames <id>`\n"]
            for c in cands:
                sig = c.get("signals") or {}
                marks = []
                if sig.get("branch_match"):
                    marks.append("this branch")
                if sig.get("in_flight"):
                    marks.append("in-flight")
                mark = f" · {' · '.join(marks)}" if marks else ""
                age = c.get("age_s") or 0
                age_s = (f"{age // 60}m" if age < 3600
                         else f"{age // 3600}h" if age < 86400
                         else f"{age // 86400}d")
                lines.append(
                    f"- `{c.get('short_id', '')}` · {age_s}{mark} · "
                    f"{c.get('next_items', 0)} next — {c.get('goal', '')}"
                )
            lines.append("\n_None of these is injected: pick the one that "
                         "describes work you are continuing. Your predecessor's "
                         "frame is usually the top entry._\n")
            block = "\n".join(lines)
            prov["frame_chars_injected"] = len(block)
            emit(block)
        # No fresh frame is normal (first boot, or >14d idle) — say nothing.
    except FileNotFoundError:
        emit("_frame index unavailable (`sovereign` not on PATH)_\n")
    except Exception as e:
        emit(f"_frame index unavailable ({type(e).__name__})_\n")

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
