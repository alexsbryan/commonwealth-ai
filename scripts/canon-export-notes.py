#!/usr/bin/env python3
"""canon-export-notes.py — guidance notes, written out as sources `canon draft` can read.

WHY. Notes recorded to guide future behaviour move into canon as one rule plus
its history. `canon draft --from <dir>` is the import: it quotes every rule
verbatim from a source file and keeps the file span as the rule's `source`, so
`canon why <id>` points at the history. This script writes those files. It
decides nothing about which rules exist — the operator does, one at a time, in
`canon draft --resume`.

WHY --db HAS NO DEFAULT. Nothing in this repo can bulk-read the store: the CLI
and the daemon's `/v1/notes/query` both cap a read at 100
(sovereign-cli/src/notes_cmd.rs:200, sovereign-mesh/src/notes_http.rs:201), and
the CLI's local lookup follows `~/.svrnmesh/active_notes_db`, which on
2026-09-13 named the nested `sovereign/.sovereign/notes.db` rather than the
daemon's `~/.svrnmesh/notes.db`. A default here would be one more hand-derived
path that can export the wrong store without a word (ARCH principles 6, 8). The
caller names the store.

WHAT COUNTS AS GUIDANCE — a closed set of classes:
  invariant  kind 'invariant'
  attempt    kind 'attempt'
  memory     kind 'decision' from the 2026-07 memory migration, carrying a
             `**Why:**` line. A heuristic: on the 2026-09-13 pilot it dropped a
             27k-character project log and also one real convention that has
             no Why line, so the summary counts what it skipped.
Live is the store's own predicate, `retired_at IS NULL AND tombstone = 0`
(corpus-engine-notes/src/notes.rs:3510). Private notes are never exported,
because the output is committed; they are counted, not silently absent.

SHAPE. One file per note, `<out>/<class>/<id8>.md`. The heading is the note's
first line without markup: `canon draft` hands the nearest heading to the model
as context, and on the pilot's 20 notes that cut descriptions-extracted-as-rules
from ~18 to ~10. Migration boilerplate (`**Applies to:**`, `_Migrated from`) is
dropped, and `**bold**` outside code spans is unwrapped so it cannot leak into a
rule's text. Everything else is verbatim.

MANIFEST. `<out>/MANIFEST.jsonl`, one row per exported note. It makes a re-run
idempotent and is what a later retire-with-pointer step keys on. A note that has
left the store since the last export is reported and its file is left in place:
a canon rule may already cite it.

Exit: 0 ok, 1 error (store unreadable, short-id collision), 2 usage.

    python3 scripts/canon-export-notes.py --db ~/.svrnmesh/notes.db --out .canon/sources/notes
    python3 scripts/canon-export-notes.py --db ~/.svrnmesh/notes.db --out .canon/sources/notes --class attempt
"""
from __future__ import annotations

import argparse
import datetime
import json
import re
import sqlite3
import sys
from pathlib import Path

LIVE = "retired_at IS NULL AND tombstone = 0"
MIGRATED = "kind = 'decision' AND session_id LIKE 'memory-migration%'"
HAS_WHY = "content LIKE '%**Why:**%'"
CLASSES = {
    "invariant": "kind = 'invariant'",
    "attempt": "kind = 'attempt'",
    "memory": f"{MIGRATED} AND {HAS_WHY}",
}
BOILERPLATE = ("**Applies to:**", "_Migrated from")
TITLE_MAX = 140
CODE_SPAN = re.compile(r"(`[^`]*`)")
BOLD = re.compile(r"\*\*(.+?)\*\*")


def unbold(line: str) -> str:
    """`**x**` -> `x`, but never inside a code span: `**/*.rs` is a glob."""
    parts = CODE_SPAN.split(line)
    return "".join(p if p.startswith("`") else BOLD.sub(r"\1", p) for p in parts)


def shape(content: str) -> str:
    out, fenced = [], False
    for line in content.strip().splitlines():
        if line.lstrip().startswith(BOILERPLATE):
            continue
        if line.lstrip().startswith("```"):
            fenced = not fenced
        out.append(line if fenced else unbold(line))
    while out and not out[0].strip():
        out.pop(0)
    body = "\n".join(out).strip()
    first = body.splitlines()[0] if body else ""
    title = re.sub(r"^[#>\-*\s]+", "", first).replace("`", "").strip()
    if len(title) > TITLE_MAX:
        title = title[:TITLE_MAX].rsplit(" ", 1)[0] + "…"
    return f"# {title}\n\n{body}\n"


def iso(created) -> str:
    s = str(created)
    if s.isdigit():
        return datetime.datetime.fromtimestamp(int(s), datetime.timezone.utc).isoformat()
    return s


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Export guidance notes as canon draft sources.")
    p.add_argument("--db", required=True, type=Path, help="the notes store to read (no default)")
    p.add_argument("--out", required=True, type=Path, help="directory to write sources into")
    p.add_argument("--class", dest="classes", action="append", choices=sorted(CLASSES),
                   help="export only this class (repeatable); default all")
    args = p.parse_args(argv)
    classes = args.classes or sorted(CLASSES)

    if not args.db.is_file():
        print(f"canon-export-notes: no store at {args.db}", file=sys.stderr)
        return 1
    try:
        con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
        rows, withheld = [], 0
        for cls in classes:
            where = f"{LIVE} AND {CLASSES[cls]}"
            rows += [(cls, *r) for r in con.execute(
                f"SELECT id, created_at, content, content_hash FROM notes "
                f"WHERE {where} AND private = 0 ORDER BY id")]
            withheld += con.execute(
                f"SELECT count(*) FROM notes WHERE {where} AND private != 0").fetchone()[0]
        no_why = con.execute(
            f"SELECT count(*) FROM notes WHERE {LIVE} AND {MIGRATED} AND NOT ({HAS_WHY})"
        ).fetchone()[0] if "memory" in classes else None
    except sqlite3.Error as e:
        print(f"canon-export-notes: reading {args.db}: {e}", file=sys.stderr)
        return 1

    by_short: dict[str, set[str]] = {}
    for _, nid, *_ in rows:
        by_short.setdefault(nid[:8], set()).add(nid)
    collisions = {k: sorted(v) for k, v in by_short.items() if len(v) > 1}
    if collisions:
        for short, ids in sorted(collisions.items()):
            print(f"canon-export-notes: short-id collision {short}: {', '.join(ids)}",
                  file=sys.stderr)
        return 1

    manifest = args.out / "MANIFEST.jsonl"
    previous: dict[str, dict] = {}
    if manifest.exists():
        for line in manifest.read_text(encoding="utf-8").splitlines():
            if line.strip():
                row = json.loads(line)
                previous[row["id"]] = row

    entries, written, unchanged, counts = [], 0, 0, {c: 0 for c in classes}
    for cls, nid, created, content, content_hash in rows:
        rel = f"{cls}/{nid[:8]}.md"
        path = args.out / rel
        text = shape(content)
        if path.exists() and path.read_text(encoding="utf-8") == text:
            unchanged += 1
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
            written += 1
        counts[cls] += 1
        entries.append({"id": nid, "class": cls, "created_at": iso(created),
                        "content_hash": content_hash, "file": rel})

    exported = {e["id"] for e in entries}
    kept = [r for r in previous.values() if r["class"] not in classes]
    gone = sorted(i for i, r in previous.items() if r["class"] in classes and i not in exported)
    args.out.mkdir(parents=True, exist_ok=True)
    manifest.write_text("".join(json.dumps(e, sort_keys=True) + "\n"
                                for e in sorted(kept + entries, key=lambda r: r["id"])),
                        encoding="utf-8")

    per_class = ", ".join(f"{c} {counts[c]}" for c in classes)
    skipped = "" if no_why is None else f", migrated memories without a Why line {no_why}"
    print(f"canon-export-notes: {per_class} from {args.db} — written {written}, "
          f"unchanged {unchanged}, private withheld {withheld}{skipped}", file=sys.stderr)
    if gone:
        print(f"canon-export-notes: {len(gone)} note(s) left the store since the last export "
              f"(files kept, a rule may cite them): {', '.join(g[:8] for g in gone)}",
              file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
