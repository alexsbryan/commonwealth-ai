#!/usr/bin/env python3
"""Custom statusline: tracks how many tokens the agent has burned on `Read`
tool calls in the current session, plus the model name.

Why this exists
---------------
The /context audit on 2026-05-12 attributed 74.3k tokens to file reads —
roughly 7% of the 1M-context Opus session — with ~22k flagged as savable.
Three behavioural patterns drove that (re-Read after Edit, duplicate
(file, offset) Reads, Read-before-symbols on Rust sources). Those patterns
are documented in `.claude/CLAUDE.md` § "Read budget", but documentation
without a visible cost signal doesn't change behaviour.

This script is the cheap version of mechanism #2 from the
2026-05-12 conversation: render an ambient budget number on every
statusline refresh so the agent sees `Reads: 30k / 15k` mid-session,
not only when the operator types /context. If the visible signal alone
proves insufficient, mechanism #1 (PreToolUse hook on Read) becomes the
next step — but we want the data first.

Input contract
--------------
Claude Code pipes a JSON envelope to stdin. The only field this script
reads is `transcript_path` (absolute path to the session's JSONL
transcript). See `claude-code/docs/statusline` for the full schema.

Output contract
---------------
Single line on stdout. ANSI escapes allowed. The harness clips to the
terminal width; keep it under ~100 columns.

Failure mode
------------
Any parse error or missing field renders a minimal `[reads: ?]` rather
than dying — a broken statusline shouldn't blank the agent's UI.
"""

from __future__ import annotations
import json
import os
import sys
from pathlib import Path

# Approximate-token heuristic.
#
# Claude's tokenizer averages ~3.3 characters per token on English-language
# source code; Rust skews slightly lower (more punctuation, shorter
# identifiers). 4 chars/token is a safe upper bound. The Read tool's
# output is `cat -n`-prefixed, so each line costs `len(line) + ~6` for
# the line-number prefix. We use a simpler bound: total result-content
# length / `CHARS_PER_TOKEN`. Approximate; correlation with true token
# count is what matters for the budget signal, not exact accounting.
CHARS_PER_TOKEN = 4

# Soft budget — the cost signal flips colour at this threshold. Picked
# to surface the 22k-savable bucket from the 2026-05-12 audit without
# being so tight that every non-trivial session lights up red. Revise
# after we have 3-4 sessions of data on actual usage shape.
SOFT_BUDGET_TOKENS = 15_000


def main() -> int:
    try:
        envelope = json.load(sys.stdin)
    except json.JSONDecodeError:
        print(fmt_minimal("?"))
        return 0

    transcript_path = envelope.get("transcript_path")
    model = (envelope.get("model") or {}).get("display_name") or "?"

    if not transcript_path or not Path(transcript_path).exists():
        print(fmt_minimal(model))
        return 0

    read_count, read_token_estimate = scan_reads(Path(transcript_path))
    print(fmt(model, read_count, read_token_estimate))
    return 0


def scan_reads(transcript_path: Path) -> tuple[int, int]:
    """Walk the JSONL transcript and tally Read tool calls + their
    approximate token cost. Each Read tool call is paired with a
    tool_result block whose `content` is the rendered file excerpt;
    we use the result body length as the token-cost proxy.
    """
    read_count = 0
    read_char_total = 0
    pending_read_ids: set[str] = set()

    try:
        with transcript_path.open("r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue

                msg = rec.get("message") or {}
                # Some recorders wrap content directly; others nest under
                # `message`. Tolerate both.
                content = msg.get("content")
                if content is None:
                    content = rec.get("content")
                if not isinstance(content, list):
                    continue

                for block in content:
                    if not isinstance(block, dict):
                        continue
                    btype = block.get("type")
                    if btype == "tool_use" and block.get("name") == "Read":
                        read_count += 1
                        tool_id = block.get("id")
                        if tool_id:
                            pending_read_ids.add(tool_id)
                    elif btype == "tool_result":
                        tool_id = block.get("tool_use_id")
                        if tool_id in pending_read_ids:
                            pending_read_ids.discard(tool_id)
                            body = block.get("content")
                            read_char_total += content_length(body)
    except OSError:
        # Transcript file may not be ready on first turn — fall through
        # to zeros rather than blanking the line.
        return 0, 0

    return read_count, read_char_total // CHARS_PER_TOKEN


def content_length(body) -> int:
    """`tool_result.content` is either a plain string or a list of
    blocks; sum across the shapes the harness emits."""
    if isinstance(body, str):
        return len(body)
    if isinstance(body, list):
        n = 0
        for blk in body:
            if isinstance(blk, dict):
                text = blk.get("text")
                if isinstance(text, str):
                    n += len(text)
            elif isinstance(blk, str):
                n += len(blk)
        return n
    return 0


def fmt(model: str, count: int, tokens: int) -> str:
    """Render the statusline. ANSI colors: cyan model, dim count, then
    the budget number coloured by threshold so the agent can scan it
    at a glance."""
    bg_color = budget_color(tokens)
    reset = "\033[0m"
    cyan = "\033[36m"
    dim = "\033[2m"
    if tokens >= 1000:
        tk = f"{tokens / 1000:.1f}k"
    else:
        tk = str(tokens)
    budget = f"{SOFT_BUDGET_TOKENS // 1000}k"
    return (
        f"{cyan}{model}{reset} "
        f"{dim}·{reset} "
        f"📖 Reads: {dim}{count}{reset} "
        f"({bg_color}{tk}{reset}/{budget})"
    )


def fmt_minimal(model: str) -> str:
    return f"\033[36m{model}\033[0m · 📖 Reads: ?"


def budget_color(tokens: int) -> str:
    """Green under 50% of budget, yellow up to 100%, red beyond. The
    point is a visible cost signal, not a fail-state — even at 200%
    we still render the line."""
    if tokens < SOFT_BUDGET_TOKENS // 2:
        return "\033[32m"  # green
    if tokens < SOFT_BUDGET_TOKENS:
        return "\033[33m"  # yellow
    return "\033[31m"      # red


if __name__ == "__main__":
    sys.exit(main())
