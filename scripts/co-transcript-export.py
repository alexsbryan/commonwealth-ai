#!/usr/bin/env python3
"""co-transcript-export.py — Claude Code session transcripts -> turn blocks.

SPIKE for order bs-1-oplog. Nothing in this repo reads Claude Code's own
`~/.claude/projects/<encoded-cwd>/*.jsonl`; the shipped `anthropic_export`
extractor reads the claude.ai WEB export, a different schema. Rather than
write a Rust extractor before we know the corpus is worth having, this
converts to the `### [ts] role` turn-block format the shipped
`threaded_turns` chunker already parses (corpus-engine/src/chunkers/
threaded_turns.rs:1-20), so the spike rides `plaintext` + `threaded_turns`
+ tiered enrichment with no engine change. Promote to a real extractor
only if the spike earns it.

EVIDENCE TRAVELS WITH THE CLAIM. Each assistant turn carries the commands
it ran and what they printed, capped. That is deliberate: a turn-pair chunk
is then self-contained for adjudication — the claim and the evidence that
bears on it land in the same retrieval unit, instead of the claim being
retrievable and its evidence three chunks away.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import sys
from pathlib import Path

TRANSCRIPTS = Path.home() / ".claude" / "projects"
# Per tool result. Enough to carry a test banner, a compile error, or a
# grep hit; not enough for one `cargo build` log to dominate a corpus.
RESULT_CAP = 1200
TOOL_CAP = 24          # tool calls rendered per assistant turn
MIN_SESSION_CHARS = 4000

def strip_reminders(t: str) -> str:
    return re.sub(r"<system-reminder>.*?</system-reminder>", "", t, flags=re.S).strip()

# Tool output is bytes from arbitrary programs: a debug binary's strings, a
# hexdump, a terminal-control sequence. The embedder rejects an interior NUL
# ("Embed tokenization failed: input contains an interior NUL at byte 1339"),
# which killed the first ingest at 4,096 chunks. Scrub at the boundary — the
# corpus is the wrong place to discover that a transcript is not text.
_CTRL = {c: None for c in range(32) if c not in (9, 10, 13)}
_CTRL[127] = None

def scrub(s: str) -> str:
    return s.translate(_CTRL)

def cap(s: str, n: int) -> str:
    s = scrub(s).strip()
    return s if len(s) <= n else s[:n] + "\n[…truncated]"

def ts(rec: dict) -> str:
    raw = rec.get("timestamp") or ""
    try:
        return _dt.datetime.fromisoformat(raw.replace("Z", "+00:00")).strftime("%Y-%m-%d %H:%M")
    except Exception:
        return "0000-00-00 00:00"

def convert(path: Path) -> str | None:
    """One transcript -> one turn-block document, or None if too thin."""
    blocks: list[str] = []
    pending: dict[str, tuple[str, str]] = {}      # tool_use_id -> (name, arg)
    evidence: list[str] = []                      # for the CURRENT assistant turn
    last_ts = "0000-00-00 00:00"

    def flush_assistant(prose: str, when: str) -> None:
        body = prose.strip()
        if evidence:
            body += "\n\n--- evidence ---\n" + "\n".join(evidence[:TOOL_CAP])
        body = scrub(body)
        if body.strip():
            blocks.append(f"### [{when}] assistant\n\n{body}\n")
        evidence.clear()

    with path.open() as fh:
        for line in fh:
            try:
                rec = json.loads(line)
            except Exception:
                continue
            kind = rec.get("type")
            content = (rec.get("message") or {}).get("content")
            if not isinstance(content, list):
                continue
            when = ts(rec)
            last_ts = when

            if kind == "user":
                is_results = any(b.get("type") == "tool_result"
                                 for b in content if isinstance(b, dict))
                if is_results:
                    # `toolUseResult` is a dict for Bash (stdout/stderr) and a
                    # bare string or list for other tools. Shape-check, never
                    # assume: a transcript is data we did not write.
                    tur = rec.get("toolUseResult")
                    out = tur.get("stdout") or "" if isinstance(tur, dict) else ""
                    err = tur.get("stderr") or "" if isinstance(tur, dict) else ""
                    if not out and isinstance(tur, str):
                        out = tur
                    for b in content:
                        if not isinstance(b, dict) or b.get("type") != "tool_result":
                            continue
                        name, arg = pending.pop(b.get("tool_use_id", ""), ("?", ""))
                        body = out
                        if not body:
                            c = b.get("content")
                            body = c if isinstance(c, str) else json.dumps(c)[:RESULT_CAP]
                        mark = "ERROR " if b.get("is_error") else ""
                        evidence.append(
                            f"{mark}$ {scrub(str(arg))}\n{cap(body, RESULT_CAP)}"
                            + (f"\nstderr: {cap(err, 300)}" if err.strip() else "")
                        )
                    continue
                text = strip_reminders("\n".join(
                    b.get("text", "") for b in content
                    if isinstance(b, dict) and b.get("type") == "text"))
                if text and not text.startswith(("<local-command", "<command-name>",
                                                 "<task-notification>")):
                    blocks.append(f"### [{when}] user\n\n{scrub(text)}\n")

            elif kind == "assistant":
                prose = "\n".join(
                    b.get("text", "") for b in content
                    if isinstance(b, dict) and b.get("type") == "text").strip()
                for b in content:
                    if isinstance(b, dict) and b.get("type") == "tool_use":
                        inp = b.get("input") or {}
                        arg = inp.get("command") or inp.get("file_path") \
                            or inp.get("pattern") or json.dumps(inp)[:200]
                        pending[b.get("id", "")] = (b.get("name", "?"), str(arg))
                if prose:
                    flush_assistant(prose, when)

    if evidence:
        flush_assistant("", last_ts)
    doc = "\n".join(blocks)
    return doc if len(doc) >= MIN_SESSION_CHARS else None

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--project", default="-Users-alexsbryan-dev-commonwealth-ai")
    ap.add_argument("--out", default=str(Path.home() / ".svrnmesh" / "agent-sessions"))
    ap.add_argument("--last", type=int, default=0, help="most recent N sessions (0 = all)")
    a = ap.parse_args()

    src = TRANSCRIPTS / a.project
    if not src.is_dir():
        print(f"no transcript dir at {src}", file=sys.stderr)
        return 2
    files = sorted(src.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
    if a.last:
        files = files[:a.last]

    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    kept = skipped = total = 0
    for f in files:
        doc = convert(f)
        if doc is None:
            skipped += 1
            continue
        (out / f"{f.stem}.txt").write_text(doc)
        kept += 1
        total += len(doc)
    print(f"wrote {kept} session documents to {out}  ({total/1e6:.1f} MB)"
          f"  skipped {skipped} under {MIN_SESSION_CHARS} chars")
    return 0

if __name__ == "__main__":
    sys.exit(main())
