#!/usr/bin/env python3
"""opencode-prune.py — cap opencode's session store by age.

WHY (2026-10-07): opencode.db reached 54 GB on the macOS peer, 51 GB of it
the `event` table. opencode 1.18 logs a full snapshot of a part on every
streaming update (900k `message.part.updated` events against 381k parts),
the log began 2026-08-31, and nothing in opencode expires it — its config
has no retention setting. Most of the bytes are ralph pool-lane sessions
whose evidence is already committed.

HOW: top-level sessions untouched for --days are deleted through
`opencode session delete`, opencode's own path: it recurses into child
sessions, cascades messages and parts, and deletes the session's event log
in one transaction. A SQL DELETE on `session` would leave the event log,
which is keyed by aggregate id with no foreign key to `session`. Every
delete is verified by reading the rows back, because the CLI exits 0 on
"Session not found" too.

SQLite never shrinks a file on DELETE. In auto_vacuum=INCREMENTAL mode
`pragma incremental_vacuum` hands the freed pages back, which --apply does.
--compact moves the file into that mode once: opencode must be closed, and
it needs free disk about the size of what remains (VACUUM INTO a sibling
file, then a rename).

USAGE:
  scripts/opencode-prune.py                  # dry run: what 14 days would delete
  scripts/opencode-prune.py --apply          # delete, then reclaim if incremental
  scripts/opencode-prune.py --compact        # once, with opencode closed
"""
import argparse
import os
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

GB = 1 << 30


def human(n):
    return f"{n / GB:.1f} GB" if n >= GB else f"{n / (1 << 20):.1f} MB"


def db_path():
    out = subprocess.run(["opencode", "db", "path"], capture_output=True, text=True, check=True)
    return Path(out.stdout.strip())


def connect(db, readonly):
    uri = f"file:{db}?mode=ro" if readonly else f"file:{db}"
    con = sqlite3.connect(uri, uri=True, timeout=60)
    con.isolation_level = None
    return con


def pages(con):
    one = lambda q: con.execute(q).fetchone()[0]
    return one("pragma page_size"), one("pragma page_count"), one("pragma freelist_count")


def describe(db, con):
    size, count, free = pages(con)
    wal = Path(f"{db}-wal")
    walsz = wal.stat().st_size if wal.exists() else 0
    mode = {0: "none", 1: "full", 2: "incremental"}[con.execute("pragma auto_vacuum").fetchone()[0]]
    return (f"file {human(db.stat().st_size)}, wal {human(walsz)}, "
            f"free pages {human(free * size)}, auto_vacuum={mode}")


def candidates(con, days):
    cutoff = int((time.time() - days * 86400) * 1000)
    return con.execute(
        "select id, directory, time_updated, title from session "
        "where parent_id is null and time_updated < ? order by time_updated",
        (cutoff,)).fetchall()


def gone(con, sid):
    left = con.execute(
        "select (select count(*) from session where id = ?1 or parent_id = ?1)"
        " + (select count(*) from event_sequence where aggregate_id = ?1)", (sid,)).fetchone()[0]
    return left == 0


def holders(db):
    """Pids with the database or its WAL open. Asked of the file, not of the
    process table: the pool's lane workers keep opencode running at all hours,
    and only a handle on THIS file makes the swap unsafe."""
    files = [str(p) for p in (db, Path(f"{db}-wal"), Path(f"{db}-shm")) if p.exists()]
    out = subprocess.run(["lsof", "-t", *files], capture_output=True, text=True).stdout
    return sorted({int(p) for p in out.split()} - {os.getpid()})


def prune(db, days, apply):
    ro = connect(db, readonly=True)
    rows = candidates(ro, days)
    print(f"opencode-prune: {db}")
    print(f"  before: {describe(db, ro)}")
    print(f"  {len(rows)} top-level sessions untouched for {days}+ days"
          + ("" if apply else " (dry run; --apply deletes them)"))
    if rows:
        first, last = rows[0][2], rows[-1][2]
        fmt = lambda ms: time.strftime("%Y-%m-%d", time.localtime(ms / 1000))
        print(f"  last updated between {fmt(first)} and {fmt(last)}")
    if not apply:
        return 0
    deleted, failed = 0, []
    for sid, directory, _, title in rows:
        cwd = directory if os.path.isdir(directory) else str(Path.home())
        r = subprocess.run(["opencode", "session", "delete", sid, "--pure"], cwd=cwd,
                           capture_output=True, text=True, timeout=300)
        if gone(ro, sid):
            deleted += 1
        else:
            tail = (r.stdout + r.stderr).strip().splitlines()[-1:] or ["(no output)"]
            failed.append((sid, title, tail[0]))
    print(f"  deleted {deleted}, still present {len(failed)}")
    for sid, title, why in failed[:10]:
        print(f"    {sid} {title[:50]!r}: {why}")
    rw = connect(db, readonly=False)
    if rw.execute("pragma auto_vacuum").fetchone()[0] == 2:
        rw.execute("pragma incremental_vacuum").fetchall()
        busy = rw.execute("pragma wal_checkpoint(TRUNCATE)").fetchone()[0]
        print("  incremental_vacuum ran" + (
            "; the WAL is still held by a reader and resets at opencode's next checkpoint"
            if busy else ""))
    else:
        print("  freed pages stay inside the file until --compact (opencode closed)")
    print(f"  after: {describe(db, rw)}")
    return 1 if failed else 0


def compact(db):
    pids = holders(db)
    if pids:
        print(f"opencode-prune --compact: refused, {db.name} is open in pids {pids}; "
              "close those opencode sessions first", file=sys.stderr)
        return 2
    con = connect(db, readonly=False)
    size, count, free = pages(con)
    live = (count - free) * size
    avail = shutil.disk_usage(db.parent).free
    print(f"opencode-prune --compact: {describe(db, con)}")
    if avail < live * 1.1:
        print(f"  refused: {human(live)} to copy, {human(avail)} free on the volume",
              file=sys.stderr)
        return 2
    busy, log, done = con.execute("pragma wal_checkpoint(TRUNCATE)").fetchone()
    if busy:
        print(f"  refused: WAL checkpoint busy (log={log} checkpointed={done}); "
              "something still has the database open", file=sys.stderr)
        return 2
    tmp = db.with_name(db.name + ".compact")
    tmp.unlink(missing_ok=True)
    con.execute("pragma auto_vacuum=INCREMENTAL")
    con.execute("vacuum into ?", (str(tmp),))
    con.close()
    check = sqlite3.connect(tmp)
    ok = check.execute("pragma quick_check").fetchone()[0]
    mode = check.execute("pragma auto_vacuum").fetchone()[0]
    check.close()
    if ok != "ok" or mode != 2:
        print(f"  refused to swap: quick_check={ok!r} auto_vacuum={mode}; {tmp} kept for "
              "inspection, the original is untouched", file=sys.stderr)
        return 1
    os.replace(tmp, db)
    for side in ("-wal", "-shm"):
        Path(f"{db}{side}").unlink(missing_ok=True)
    print(f"  after: {describe(db, connect(db, readonly=True))}")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--days", type=int, default=14)
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--compact", action="store_true")
    a = ap.parse_args()
    db = db_path()
    if a.compact:
        return compact(db)
    return prune(db, a.days, a.apply)


if __name__ == "__main__":
    sys.exit(main())
