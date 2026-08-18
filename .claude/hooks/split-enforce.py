#!/usr/bin/env python3
"""UserPromptSubmit hook: enforce the red-SPLIT protocol (SESSION_CONTINUITY §3a).

Why this exists
---------------
Splitting is the largest priced cost lever (H1: 45-52% of fleet spend,
cache-audit --counterfactual 2026-07-23), and every mechanism it needs is
shipped: statusline thresholds, session_state encode-time frames, boot-hook
frame injection. What was missing is enforcement — the statusline colors a
number the agent may never look at. This hook injects the protocol directly
into the turn where a split decision is actionable: the moment a new user
prompt arrives while context is past threshold.

Behaviour
---------
- ctx >= RED (500k): inject a hard directive every prompt.
  - frame fresh (<= FRAME_FRESH_S) AND provenance is self-reported or
    hand-written -> "finish step, final upsert, tell the operator to split".
  - otherwise -> "upsert your frame NOW via session_state, then split".
    (A distilled frame never authorizes a split — provenance, not score.)
- ctx >= YELLOW (250k): one gentle nudge per session (marker file dedups),
  reminding the agent to keep the frame current at the next boundary.
- below thresholds: silent, zero output.

Thresholds raised 90k/250k -> 250k/500k on 2026-08-02 (operator call).
The old lines were set against a context an order of magnitude smaller,
and the arithmetic that justified them stopped holding: splitting is only
a saving when cache-read on the carried context exceeds what the split
itself costs — the donor's frame write, plus the successor re-acquiring
by hand what the frame could not carry in 2,150 tokens. Below ~250k that
overhead dominates, so the protocol was firing on sessions it made more
expensive, not less. Frequent small splits are the failure mode now; the
lever still pays at genuinely fat contexts, which is where it now fires.

Every firing appends one JSON line to ~/.svrnmesh/sessions/split-events.jsonl
— the split-adoption signal the weekly fleet report reads (did red-crossing
sessions actually split? how long did they linger red?).

Contracts
---------
stdin: hook envelope JSON; fields used: session_id, transcript_path.
stdout: injected as context (UserPromptSubmit); empty = inject nothing.
Never exits non-zero: a broken hook must not block the user's prompt.
Thresholds overridable via SPLIT_YELLOW_TOKENS / SPLIT_RED_TOKENS (testing).
"""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path

YELLOW = int(os.environ.get("SPLIT_YELLOW_TOKENS", "250000"))
RED = int(os.environ.get("SPLIT_RED_TOKENS", "500000"))
# A frame older than this is stale for split purposes: the spec wants the
# donor's LAST state, and 15 minutes of red-zone work invalidates a frame
# faster than an hour of steady state.
FRAME_FRESH_S = int(os.environ.get("SPLIT_FRAME_FRESH_S", "900"))

SESSIONS_ROOT = Path.home() / ".sovereign" / "sessions"
EVENTS_PATH = SESSIONS_ROOT / "split-events.jsonl"


def main() -> int:
    try:
        envelope = json.load(sys.stdin)
    except json.JSONDecodeError:
        return 0
    session_id = envelope.get("session_id") or ""
    transcript_path = envelope.get("transcript_path") or ""
    # `context_tokens` lets a harness that already knows its context size hand
    # it over instead of us re-deriving it. Claude Code does not, so it stays
    # on the transcript reader below; pi supplies it from ctx.getContextUsage()
    # via .pi/extensions/sovereign-hooks. The POLICY (thresholds, frame
    # freshness, directives) has one implementation either way — only the
    # measurement is per-harness, because only the measurement differs.
    supplied = envelope.get("context_tokens")
    if not session_id:
        return 0
    if isinstance(supplied, (int, float)) and supplied > 0:
        ctx = int(supplied)
    elif transcript_path:
        ctx = last_context_size(Path(transcript_path))
    else:
        return 0
    if ctx < YELLOW:
        return 0

    age_s, provenance = frame_info(session_id)
    authorizing = provenance in ("self-reported", "hand-written")
    fresh = age_s is not None and age_s <= FRAME_FRESH_S

    if ctx >= RED:
        level, directive = "red", ("split_now" if (fresh and authorizing) else "write_frame_first")
    else:
        # Yellow fires once per session; red fires every prompt.
        marker = SESSIONS_ROOT / session_id / ".split-yellow-nudged"
        if marker.exists():
            return 0
        try:
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.touch()
        except OSError:
            pass
        level, directive = "yellow", "nudge"

    log_event(session_id, ctx, age_s, provenance, level, directive)
    print(render(ctx, age_s, provenance, level, directive))
    return 0


def last_context_size(transcript_path: Path) -> int:
    """Actual context of the most recent request (input + cache_read +
    cache_creation from the last usage-bearing record). Tail-read only —
    this runs on every prompt and must stay O(1) in transcript size.
    Same logic as read-budget-statusline.py::last_context_size."""
    try:
        size = transcript_path.stat().st_size
        with transcript_path.open("rb") as f:
            f.seek(max(0, size - 262_144))
            tail = f.read().decode("utf-8", errors="replace")
    except OSError:
        return 0
    for line in reversed(tail.splitlines()):
        if '"usage"' not in line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        usage = (rec.get("message") or {}).get("usage") or {}
        total = (
            (usage.get("input_tokens") or 0)
            + (usage.get("cache_read_input_tokens") or 0)
            + (usage.get("cache_creation_input_tokens") or 0)
        )
        if total:
            return total
    return 0


def frame_info(session_id: str) -> tuple[int | None, str]:
    """(age_seconds, provenance) of this session's frame; (None, "none")
    when absent. Provenance is read from the frontmatter because only
    self-reported/hand-written frames authorize a split (spec §3a)."""
    frame = SESSIONS_ROOT / session_id / "frame.md"
    try:
        age_s = max(0, int(time.time() - frame.stat().st_mtime))
        head = frame.read_text(encoding="utf-8", errors="replace")[:2000]
    except OSError:
        return None, "none"
    provenance = "unknown"
    for line in head.splitlines():
        if line.startswith("provenance:"):
            provenance = line.split(":", 1)[1].strip()
            break
    return age_s, provenance


def log_event(
    session_id: str, ctx: int, age_s: int | None, provenance: str, level: str, directive: str
) -> None:
    try:
        EVENTS_PATH.parent.mkdir(parents=True, exist_ok=True)
        with EVENTS_PATH.open("a", encoding="utf-8") as f:
            f.write(
                json.dumps(
                    {
                        "ts": int(time.time()),
                        "session_id": session_id,
                        "ctx_tokens": ctx,
                        "frame_age_s": age_s,
                        "frame_provenance": provenance,
                        "level": level,
                        "directive": directive,
                    }
                )
                + "\n"
            )
    except OSError:
        pass


def render(ctx: int, age_s: int | None, provenance: str, level: str, directive: str) -> str:
    ctx_k = f"{ctx // 1000}k"
    if directive == "split_now":
        frame_desc = f"fresh ({age_s // 60}m old, {provenance})"
        return (
            f"<split-protocol level=red>\n"
            f"Context is {ctx_k} (>= {RED // 1000}k red threshold) and your session frame is "
            f"{frame_desc}. Per SESSION_CONTINUITY §3a: finish the current step — do "
            f"not start new work — make a final small session_state upsert if anything "
            f"changed since the last one, then tell the operator to split now "
            f"(/clear or a new session). The boot hook hands your frame to the "
            f"successor; every further turn at this context size re-bills ~{ctx_k} "
            f"of cache-read.\n"
            f"</split-protocol>"
        )
    if directive == "write_frame_first":
        frame_desc = (
            "no frame exists"
            if age_s is None
            else f"frame is {age_s // 60}m old with provenance '{provenance}'"
        )
        return (
            f"<split-protocol level=red>\n"
            f"Context is {ctx_k} (>= {RED // 1000}k red threshold) but {frame_desc} — not fresh "
            f"enough to authorize a split (spec §3a: only a recent self-reported or "
            f"hand-written frame does; a distilled frame is rescue-only). Call the "
            f"session_state tool NOW with your current goal/state/next/decisions/"
            f"invariants, then tell the operator to split (/clear or a new session).\n"
            f"</split-protocol>"
        )
    return (
        f"<split-protocol level=yellow>\n"
        f"Context is {ctx_k} (>= {YELLOW // 1000}k). At the next natural boundary (step done, "
        f"blocker hit), upsert your session frame via session_state so a red-zone "
        f"split stays cheap. This nudge fires once per session.\n"
        f"</split-protocol>"
    )


if __name__ == "__main__":
    sys.exit(main())
