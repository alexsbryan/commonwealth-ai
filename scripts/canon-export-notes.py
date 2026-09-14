#!/usr/bin/env python3
"""canon-export-notes.py — guidance notes, redacted, written out as sources `canon draft` can read.

WHY. Notes recorded to guide future behaviour move into canon as one rule plus
its history. `canon draft --from <dir>` is the import: it quotes every rule
verbatim from a source file and keeps the file span as the rule's `source`, so
`canon why <id>` points at the history. This script writes those files. It
decides nothing about which rules exist — the operator does, one at a time, in
`canon draft --resume`. The files are committed beside the canon (canon
LOAD_TEST_DESIGN entry 6) in a public repository, so they are redacted.

WHY --db HAS NO DEFAULT. Nothing in this repo can bulk-read the store: the CLI
and the daemon's `/v1/notes/query` both cap a read at 100
(sovereign-cli/src/notes_cmd.rs:200, sovereign-mesh/src/notes_http.rs:201), and
the CLI's local lookup follows `~/.svrnmesh/active_notes_db`, which on
2026-09-13 named the nested `sovereign/.sovereign/notes.db` rather than the
daemon's `~/.svrnmesh/notes.db`. A default here would be one more hand-derived
path that can export the wrong store without a word (ARCH principles 6, 8).

CLASSES — a closed set:
  invariant  store notes of kind 'invariant'
  attempt    store notes of kind 'attempt'
  memory     store notes (kind 'decision') from the 2026-07 memory migration
             whose ORIGINAL memory file is typed feedback or invariant. The
             migration dropped the type; the file in --memory-dir still carries
             it, and a note is matched to its file by content. Project and
             reference memories are state and pointers, not guidance.
  harness    feedback/invariant files in --memory-dir that were never migrated —
             written after 2026-07-18 and visible only to one harness until now.
Live is the store's own predicate, `retired_at IS NULL AND tombstone = 0`
(corpus-engine-notes/src/notes.rs:3510). Private notes are never exported.
Every note a class drops is counted in the summary, never silently absent.

SHAPE. One file per note. The heading is the note's first line (a harness file's
`description`) without markup: `canon draft` hands the nearest heading to the
model as context, and on the pilot's 20 notes that cut descriptions-extracted-as-
rules from ~18 to ~10. Migration boilerplate is dropped and `**bold**` outside
code spans is unwrapped so it cannot leak into a rule's text.

REDACTION, mechanical and counted: email addresses, private LAN and tailnet
(100.64.0.0/10) IPv4 addresses, and `/Users/<name>` or `/home/<name>` prefixes.
Loopback and 0.0.0.0 stay; they carry meaning and name nobody.

MANIFEST. `<out>/MANIFEST.jsonl`, one row per exported note: what makes a re-run
idempotent and what a later retire-with-pointer step keys on.

Exit: 0 ok, 1 error (store or memory dir unreadable, short-id collision), 2 usage.

    python3 scripts/canon-export-notes.py --db ~/.svrnmesh/notes.db \\
        --memory-dir ~/.claude/projects/-Users-<you>-dev-commonwealth-ai/memory \\
        --out .canon/sources/notes
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import re
import sqlite3
import sys
from pathlib import Path

LIVE = "retired_at IS NULL AND tombstone = 0"
MIGRATED = "session_id LIKE 'memory-migration%'"
STORE_CLASSES = {
    "invariant": "kind = 'invariant'",
    "attempt": "kind = 'attempt'",
    "memory": f"kind = 'decision' AND {MIGRATED}",
}
ALL_CLASSES = sorted([*STORE_CLASSES, "harness"])
TYPED_CLASSES = {"memory", "harness"}
GUIDANCE_TYPES = {"feedback", "invariant"}
MATCH_MIN = 40      # a shorter body matches too much to count as the same memory
MATCH_PREFIX = 120
BOILERPLATE = ("**Applies to:**", "_Migrated from")
TITLE_MAX = 140
CODE_SPAN = re.compile(r"(`[^`]*`)")
BOLD = re.compile(r"\*\*(.+?)\*\*")
FRONTMATTER = re.compile(r"\A---\n(.*?)\n---\n?(.*)\Z", re.S)

_O = r"(?:25[0-5]|2[0-4]\d|1?\d?\d)"
REDACTIONS = (
    ("email", re.compile(r"(?<![\w.%+-])(?!git@)[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}"), "<email>"),
    ("tailnet-ip", re.compile(rf"(?<![\d.])100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.{_O}\.{_O}(?!\.?\d)"), "<tailnet-ip>"),
    ("lan-ip", re.compile(rf"(?<![\d.])(?:10\.{_O}|172\.(?:1[6-9]|2\d|3[01])|192\.168)\.{_O}\.{_O}(?!\.?\d)"), "<lan-ip>"),
    ("home-path", re.compile(r"/(?:Users|home)/[A-Za-z0-9._-]+"), "~"),
)


def redact(text: str, tally: dict[str, int]) -> str:
    for name, rx, repl in REDACTIONS:
        text, n = rx.subn(repl, text)
        tally[name] = tally.get(name, 0) + n
    return text


def unbold(line: str) -> str:
    """`**x**` -> `x`, but never inside a code span: `**/*.rs` is a glob."""
    parts = CODE_SPAN.split(line)
    return "".join(p if p.startswith("`") else BOLD.sub(r"\1", p) for p in parts)


def shape(content: str, title: str | None = None) -> str:
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
    head = title if title else (body.splitlines()[0] if body else "")
    head = re.sub(r"^[#>\-*\s]+", "", unbold(head)).replace("`", "").strip()
    if len(head) > TITLE_MAX:
        head = head[:TITLE_MAX].rsplit(" ", 1)[0] + "…"
    return f"# {head}\n\n{body}\n"


def norm(text: str) -> str:
    text = re.sub(r"^\*\*Applies to:\*\*.*$", "", text, flags=re.M)
    text = re.sub(r"[*_`#>\[\]]", "", text)
    return re.sub(r"\s+", " ", text).strip().lower()


def read_memory_dir(d: Path) -> list[dict]:
    files = []
    for p in sorted(d.glob("*.md")):
        if p.name == "MEMORY.md":
            continue
        raw = p.read_text(encoding="utf-8")
        m = FRONTMATTER.match(raw)
        front, body = (m.group(1), m.group(2)) if m else ("", raw)
        typ = re.search(r"^\s*type:\s*(\w+)\s*$", front, re.M)
        desc = re.search(r"^description:\s*(.+)$", front, re.M)
        files.append({
            "stem": p.stem,
            "type": typ.group(1) if typ else None,
            "description": desc.group(1).strip().strip("\"'") if desc else None,
            "body": body.strip(),
            "key": norm(body)[:MATCH_PREFIX],
        })
    return files


def iso(created) -> str | None:
    s = str(created)
    if s.isdigit():
        return datetime.datetime.fromtimestamp(int(s), datetime.timezone.utc).isoformat()
    return s if created is not None else None


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Export guidance notes as canon draft sources.")
    p.add_argument("--db", required=True, type=Path, help="the notes store to read (no default)")
    p.add_argument("--out", required=True, type=Path, help="directory to write sources into")
    p.add_argument("--memory-dir", type=Path,
                   help="a harness memory directory: types migrated memories, supplies unmigrated ones")
    p.add_argument("--class", dest="classes", action="append", choices=ALL_CLASSES,
                   help="export only this class (repeatable); default all")
    args = p.parse_args(argv)
    classes = args.classes or ALL_CLASSES

    typed = sorted(TYPED_CLASSES & set(classes))
    if typed and args.memory_dir is None:
        print(f"canon-export-notes: {', '.join(typed)} need --memory-dir — a migrated memory's "
              "type is recorded only in its original file", file=sys.stderr)
        return 2
    if not args.db.is_file():
        print(f"canon-export-notes: no store at {args.db}", file=sys.stderr)
        return 1
    if args.memory_dir is not None and not args.memory_dir.is_dir():
        print(f"canon-export-notes: no memory directory at {args.memory_dir}", file=sys.stderr)
        return 1
    files = read_memory_dir(args.memory_dir) if typed else []
    matchable = [f for f in files if len(f["key"]) >= MATCH_MIN]

    try:
        con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
        rows, withheld = [], 0
        for cls in classes:
            if cls not in STORE_CLASSES:
                continue
            where = f"{LIVE} AND {STORE_CLASSES[cls]}"
            rows += [(cls, *r) for r in con.execute(
                f"SELECT id, created_at, content, content_hash FROM notes "
                f"WHERE {where} AND private = 0 ORDER BY id")]
            withheld += con.execute(
                f"SELECT count(*) FROM notes WHERE {where} AND private != 0").fetchone()[0]
        migrated = [norm(c) for (c,) in con.execute(
            f"SELECT content FROM notes WHERE {LIVE} AND {MIGRATED}")] if typed else []
    except sqlite3.Error as e:
        print(f"canon-export-notes: reading {args.db}: {e}", file=sys.stderr)
        return 1

    typing = {"guidance": 0, "not guidance": 0, "untyped": 0}
    if "memory" in classes:
        kept = []
        for row in rows:
            if row[0] != "memory":
                kept.append(row)
                continue
            n = norm(row[3])
            types = {f["type"] for f in matchable if f["key"] in n}
            if not types:
                typing["untyped"] += 1
            elif types <= GUIDANCE_TYPES:
                typing["guidance"] += 1
                kept.append(row)
            else:
                typing["not guidance"] += 1
        rows = kept

    harness, decided = [], {"exported": 0, "migrated": 0, "not guidance": 0, "too short": 0}
    if "harness" in classes:
        for f in files:
            if f["type"] not in GUIDANCE_TYPES:
                decided["not guidance"] += 1
            elif len(f["key"]) < MATCH_MIN:
                decided["too short"] += 1
            elif any(f["key"] in m for m in migrated):
                decided["migrated"] += 1
            else:
                decided["exported"] += 1
                harness.append(f)
        print(f"canon-export-notes: memory files in {args.memory_dir}: {decided['exported']} "
              f"feedback/invariant exported, {decided['migrated']} already migrated, "
              f"{decided['not guidance']} project/reference not exported, "
              f"{decided['too short']} too short to match", file=sys.stderr)

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

    pending = [(cls, nid, f"{cls}/{nid[:8]}.md", shape(content), iso(created), content_hash)
               for cls, nid, created, content, content_hash in rows]
    pending += [("harness", f"harness:{f['stem']}", f"harness/{f['stem']}.md",
                 shape(f["body"], f["description"]), None,
                 hashlib.sha256(f["body"].encode()).hexdigest()[:16]) for f in harness]

    tally: dict[str, int] = {}
    entries, written, unchanged = [], 0, 0
    counts = {c: 0 for c in classes}
    for cls, nid, rel, text, created, content_hash in pending:
        text = redact(text, tally)
        path = args.out / rel
        if path.exists() and path.read_text(encoding="utf-8") == text:
            unchanged += 1
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
            written += 1
        counts[cls] += 1
        entries.append({"id": nid, "class": cls, "created_at": created,
                        "content_hash": content_hash, "file": rel})

    exported = {e["id"] for e in entries}
    kept_rows = [r for r in previous.values() if r["class"] not in classes]
    gone = sorted(i for i, r in previous.items() if r["class"] in classes and i not in exported)
    args.out.mkdir(parents=True, exist_ok=True)
    manifest.write_text("".join(json.dumps(e, sort_keys=True) + "\n"
                                for e in sorted(kept_rows + entries, key=lambda r: r["id"])),
                        encoding="utf-8")

    per_class = ", ".join(f"{c} {counts[c]}" for c in classes)
    print(f"canon-export-notes: {per_class} from {args.db} — written {written}, "
          f"unchanged {unchanged}, private withheld {withheld}", file=sys.stderr)
    if "memory" in classes:
        print(f"canon-export-notes: migrated memories typed by {args.memory_dir}: "
              f"{typing['guidance']} feedback/invariant exported, {typing['not guidance']} "
              f"project/reference not exported, {typing['untyped']} untyped not exported",
              file=sys.stderr)
    print("canon-export-notes: redacted " + ", ".join(f"{k} {tally.get(k, 0)}"
                                                      for k, _, _ in REDACTIONS), file=sys.stderr)
    if gone:
        print(f"canon-export-notes: {len(gone)} note(s) left the source since the last export "
              f"(files kept, a rule may cite them): {', '.join(g[:24] for g in gone)}",
              file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
