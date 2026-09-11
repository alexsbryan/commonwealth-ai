#!/usr/bin/env python3
"""PreToolUse advisory: how much room this file has left, said before the code is written.

THE MOMENT. An Edit or Write against a `.rs` file that is near its arch-gate
ceiling. That gate is blocking and it speaks at PUSH time, which is the one
moment when compliance is most expensive: the lines are already written, so
satisfying it means extracting them from a finished diff — and landing a
refactor inside a feature commit is itself the ARCH §2 smell. The only cheap
moves left are raising the pin or skipping the gate, so that is what happens.

The same lines put in a NEW file cost nothing. Not less — nothing; it is a
different string in the Write call. The whole difference between "new file"
and "extract the file" is whether the constraint was known before line one.
This hook moves that knowledge to before line one. It is the entire fix.

ONCE PER FILE PER SESSION. Twelve edits to `frontdoor.rs` produce one line,
not twelve. An advisory that fires on every edit to any of the 183 pinned
files becomes wallpaper by lunchtime, which is the same disease one layer
down — `AGENTS.md` says as much about gates that teach people to reach for
`--no-verify`. The ledger is the one `commit-smells.py` already uses.

SLACK IS THE WHOLE TEST. `ceiling - current_lines`, where the ceiling is the
frozen baseline plus arch-gate's GROWTH_SLACK for a pinned file and
LINE_LIMIT for any other. One formula, no branches, and it is more honest
than asking "is this file pinned": a pinned file that has since been cut well
under its pin genuinely has room and stays silent, while an UNPINNED file at
1,180 lines — about to become a NEW oversized file, the harshest arch-gate
failure because it has no baseline entry at all — is caught.

NEVER BLOCKS. Always exit 0; a missing baseline, a new file or no envelope
means silence. Harness-neutral: JSON envelope on stdin, like every script in
this directory.

Counting matches `arch_gate.rs::collect_oversized` exactly (`text.lines()`,
raw, no comment stripping). A hook whose numbers disagree with the gate it
speaks for is worse than no hook.

Opt-out: SOVEREIGN_NO_SIZE_WARN=1. SOVEREIGN_SIZE_WARN_DEBUG=1 prints the
slack computation to stderr even when the verdict is silence.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

# Both from corpus-engine/xtask/src/arch_gate.rs — the gate this hook speaks
# for. If they drift there, tests/size-warn.sh fails on the parity case.
LINE_LIMIT = 1200
GROWTH_SLACK = 50

SESSIONS_ROOT = Path(os.environ.get("SOVEREIGN_SESSIONS_DIR")
                     or Path.home() / ".svrnmesh" / "sessions")
LEDGER_NAME = "size-warn-shown.json"
BASELINE = "quality/baselines/oversized.txt"
SOURCE_TREE = "quality/source-tree.toml"
# `excluded_dirs = [ "..." , ... ]` out of source-tree.toml. Regex rather than
# tomllib so the hook does not carry a 3.11 floor for one flat string list.
EXCLUDED_RX = re.compile(r"excluded_dirs\s*=\s*\[(.*?)\]", re.S)


def envelope() -> dict:
    raw = os.environ.get("SOVEREIGN_HOOK_INPUT")
    try:
        return json.loads(raw) if raw else json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return {}


def advise(msg: str) -> int:
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "additionalContext": msg}}))
    return 0


def find_repo(env: dict) -> Path | None:
    start = os.environ.get("CLAUDE_PROJECT_DIR") or env.get("cwd") or os.getcwd()
    r = subprocess.run(["git", "-C", start, "rev-parse", "--show-toplevel"],
                       capture_output=True, text=True)
    return Path(r.stdout.strip()) if r.returncode == 0 and r.stdout.strip() else None


def ledger_load(session_id: str) -> set[str]:
    try:
        return set(json.loads((SESSIONS_ROOT / session_id / LEDGER_NAME)
                              .read_text(encoding="utf-8")))
    except (OSError, ValueError):
        return set()


def ledger_save(session_id: str, keys: set[str]) -> None:
    try:
        d = SESSIONS_ROOT / session_id
        d.mkdir(parents=True, exist_ok=True)
        (d / LEDGER_NAME).write_text(json.dumps(sorted(keys)), encoding="utf-8")
    except OSError:
        pass


def pinned(repo: Path) -> dict[str, int]:
    """The frozen oversized baseline: repo-relative path -> pinned line count."""
    out: dict[str, int] = {}
    try:
        text = (repo / BASELINE).read_text(encoding="utf-8")
    except OSError:
        return out
    for line in text.splitlines():
        if line.startswith("#") or not line.strip():
            continue
        n, _, path = line.partition("\t")
        if path and n.strip().isdigit():
            out[path.strip()] = int(n.strip())
    return out


def excluded(repo: Path, rel: Path) -> bool:
    """source-tree.toml is the one decider for what the gate walks."""
    try:
        m = EXCLUDED_RX.search((repo / SOURCE_TREE).read_text(encoding="utf-8"))
    except OSError:
        return False
    names = set(re.findall(r'"([^"]+)"', m.group(1))) if m else set()
    return any(part in names for part in rel.parts)


def main() -> int:
    if os.environ.get("SOVEREIGN_NO_SIZE_WARN") == "1":
        return 0
    env = envelope()
    if env.get("tool_name") not in ("Edit", "Write"):
        return 0
    raw_path = (env.get("tool_input") or {}).get("file_path")
    if not raw_path or not str(raw_path).endswith(".rs"):
        return 0
    repo = find_repo(env)
    if repo is None:
        return 0
    # Both sides resolved: `git rev-parse` hands back a symlink-free root, and
    # on macOS an editor path under /tmp would otherwise never be relative to
    # its own /private/tmp root.
    try:
        rel = Path(raw_path).resolve().relative_to(repo.resolve())
    except (ValueError, OSError):
        return 0
    if excluded(repo, rel):
        return 0

    target = repo / rel
    try:
        current = len(target.read_text(encoding="utf-8", errors="replace").splitlines())
    except OSError:
        return 0  # new file — the outcome this hook exists to produce

    cap = pinned(repo).get(str(rel))
    ceiling = cap + GROWTH_SLACK if cap is not None else LINE_LIMIT
    slack = ceiling - current
    # ARCH §9.1. Silence is this hook's common case and its designed default,
    # which makes "decided there was room" and "crashed on the envelope"
    # indistinguishable from outside — and the falsifier for the whole idea is
    # whether the signal reaches the decision at all. One opt-in line makes the
    # decision legible without adding noise to the default path.
    if os.environ.get("SOVEREIGN_SIZE_WARN_DEBUG") == "1":
        print(f"size-warn: {rel} {current} lines, ceiling {ceiling} "
              f"({'pinned ' + str(cap) if cap is not None else 'unpinned'}), "
              f"slack {slack} -> {'warn' if slack <= GROWTH_SLACK else 'silent'}",
              file=sys.stderr)
    if slack > GROWTH_SLACK:
        return 0

    session_id = env.get("session_id") or ""
    if session_id:
        shown = ledger_load(session_id)
        if str(rel) in shown:
            return 0
        ledger_save(session_id, shown | {str(rel)})

    where = "its frozen arch-gate baseline + 50 slack" if cap is not None \
        else f"ARCH §3.1's {LINE_LIMIT}-line limit — this file is NOT yet baselined, " \
             "so crossing it fails as a NEW oversized file"
    over = " ALREADY OVER — any edit that does not shrink it fails the gate." if slack < 0 else ""
    return advise(
        f"## {rel.name} has {slack} lines of room (hook: size-warn)\n"
        f"`{rel}` is {current:,} lines against a ceiling of {ceiling:,} — {where}.{over}\n"
        f"Adding more than {max(slack, 0)} lines here fails `cargo xtask arch-gate`, "
        f"which blocks the push.\n"
        f"Put new code in a new file under `{rel.parent}/` instead. That costs nothing now "
        f"and costs an extraction later — it is the same code either way."
    )


if __name__ == "__main__":
    sys.exit(main())
